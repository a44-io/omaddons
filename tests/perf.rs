//! Time budgets, measured on release builds only, one at a time:
//! `cargo test --release --test integration perf:: -- --ignored --skip bench_home --skip live_home --test-threads=1`
//! (the milestone checkpoint runs them and quotes the printed numbers; the two 500 MiB
//! tests also never overlap when run in parallel, through `HALF_GIB`). A debug build is
//! several times slower and proves nothing, so each test is ignored. Every speed budget
//! lives here. Elsewhere, time appears only as a correctness bound with a wide margin: a
//! wait that must last at least its timeout, a sanity ceiling of a second or more, or a
//! killed child that must not run its course. Never as a speed target.
//!
//! Also here: `argc bench`'s home generator (ignored, and a no-op without `BENCH_HOME`),
//! and the request-economy assertions the other suites do not make, which run in the
//! normal gate because they count `net.log` lines, never time.
//!
//! The approval checked under the commit lock (D8) logs through the test log:
//! `the_approval_check_under_the_lock_costs` reports its cost on the realistic home in
//! release, and a gate test proves that home and the cost note's numbers, never a time.
//! e2e suite: the under-lock approval check on the realistic home, reported in release and proven in the gate.

use crate::common;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::addons::toc;
use common::archives::zip_text;
use common::engine::{Scene, lock_held_ms};
use common::env::TestEnv;
use common::home::Home;
use common::http::{Asset, paths, replay, scenario};
use common::netlog::net_log;
use common::trees::CustomRoot;
use jiff::Timestamp;
use omaddons::drift::DriftMode;
use omaddons::model::{Channel, FolderName, Ref, RunId};
use omaddons::sources::wowi::index::{IndexPolicy, StoredIndex, WowiEntry, WowiIndex};
use omaddons::sources::{SourceConfigView, SourceRegistry, Wanted, fetch_once};
use omaddons::state::memo::{self, MemoEntry, ReleaseMemo};
use support::{
    addon_release, current_rss, drift_report, history_line, lua_workload, measured, median_of_21,
    mib, peak_rss, reset_peak_rss, restore_lock_hold, sampled_peak_rss,
};

/// Held by each 500 MiB test, so two never share the disk.
static HALF_GIB: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod support {
    use omaddons::discover::DiscoverySource;
    use omaddons::discover::battlenet::{BattlenetHints, discover_root};
    use omaddons::drift::{DriftMode, DriftReport, check};
    use omaddons::model::{Actor, FolderName, RunId, TargetKey, TxId, TxKind};
    use omaddons::rules::RulesTable;
    use omaddons::state::history::{self, FolderOp, HistoryEntry, HistoryOp, TxResult};

    use super::common::home::Home;
    use super::common::http::{Asset, gh_latest, gh_release_v};
    use super::common::state::{files_index, record};

    /// One measurement line on stderr, written directly so libtest does not capture it:
    /// the checkpoint quotes these numbers.
    pub fn measured(line: std::fmt::Arguments<'_>) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{line}");
    }

    /// The median of 21 runs of `f`, and every run's time, sorted.
    pub fn median_of_21(mut f: impl FnMut()) -> (std::time::Duration, Vec<std::time::Duration>) {
        let mut times: Vec<std::time::Duration> = (0..21)
            .map(|_| {
                let start = std::time::Instant::now();
                f();
                start.elapsed()
            })
            .collect();
        times.sort();
        (times[times.len() / 2], times)
    }

    /// Retail checked against its records and files index.
    pub fn drift_report(home: &Home, mode: DriftMode) -> DriftReport {
        let (targets, _) = discover_root(
            &home.root,
            &RulesTable::builtin(),
            &BattlenetHints::default(),
            "battlenet",
            &DiscoverySource::Scan,
        );
        let retail = targets
            .into_iter()
            .find(|t| t.target.flavor_dir.as_deref() == Some("_retail_"))
            .unwrap()
            .target;
        let index = files_index(home, "retail");
        check(&retail, &record(home, "retail"), Some(&index), mode)
    }

    /// An update by `run` on `key` replacing twelve folders.
    pub fn history_line(key: TargetKey, run: RunId) -> HistoryEntry {
        HistoryEntry {
            schema_version: history::CURRENT,
            tx: TxId::new(),
            kind: TxKind::Update,
            at: jiff::Timestamp::now(),
            target: Some(key),
            targets: Vec::new(),
            actor: Actor::Human,
            reason: None,
            argv: ["omaddons", "update"].map(str::to_owned).to_vec(),
            ops: (0..12)
                .map(|i| HistoryOp {
                    op: FolderOp::Replace,
                    folder: FolderName::try_from(format!("Addon{i:03}")).unwrap(),
                    addon: format!("Addon{i:03}"),
                    edit: None,
                    backup: None,
                    from: None,
                    files: None,
                })
                .collect(),
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: Some(run),
            backup: None,
            plan: None,
            doctor_run_id: None,
            repairs: Vec::new(),
        }
    }

    /// The highest `VmHWM` of `child` sampled until it exits: a high-water mark, so any
    /// sample after the peak holds it, and the result can only fall short of the peak,
    /// never exceed it.
    pub fn sampled_peak_rss(child: &mut std::process::Child) -> (u64, std::process::ExitStatus) {
        let status_file = format!("/proc/{}/status", child.id());
        let mut peak = 0u64;
        loop {
            if let Ok(status) = std::fs::read_to_string(&status_file)
                && let Some(kib) = status
                    .lines()
                    .find_map(|l| l.strip_prefix("VmHWM:"))
                    .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            {
                peak = peak.max(kib * 1024);
            }
            if let Some(exit) = child.try_wait().unwrap() {
                return (peak, exit);
            }
        }
    }

    /// This process's peak resident memory (`VmHWM`), in bytes.
    pub fn peak_rss() -> u64 {
        own_status_bytes("VmHWM:")
    }

    /// This process's resident memory now (`VmRSS`), in bytes.
    pub fn current_rss() -> u64 {
        own_status_bytes("VmRSS:")
    }

    /// Lowers this process's peak resident memory (`VmHWM`) to what it holds now and
    /// returns that, so `peak_rss()` less it is what one case added at its peak. The
    /// stand-in for a heap peak: `getrusage`'s `ru_maxrss` is the same mark and cannot be
    /// lowered, and a counting allocator would need `unsafe`, which the crate forbids.
    /// Pages count, not allocations, so memory an earlier case freed and the allocator
    /// kept goes unseen: a lower bound, which a case with a memory budget measures in a
    /// child process instead.
    pub fn reset_peak_rss() -> u64 {
        std::fs::write("/proc/self/clear_refs", "5").unwrap();
        current_rss()
    }

    /// `bytes` in MiB, for a measurement line.
    pub fn mib(bytes: u64) -> f64 {
        bytes as f64 / f64::from(1u32 << 20)
    }

    /// One `kB` field of `/proc/self/status`, in bytes.
    fn own_status_bytes(field: &str) -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let line = status.lines().find_map(|l| l.strip_prefix(field)).unwrap();
        let kib: u64 = line.trim().trim_end_matches("kB").trim().parse().unwrap();
        kib * 1024
    }

    /// `len` bytes of SavedVariables for a parser resource workload, built in place (no
    /// second copy), and the globals to build.
    pub fn lua_workload(name: &str, len: usize) -> (Vec<u8>, &'static [&'static str]) {
        let mut bytes = Vec::with_capacity(len);
        let fill = |bytes: &mut Vec<u8>, piece: &[u8], reserve: usize| {
            while bytes.len() + piece.len() + reserve <= len {
                bytes.extend_from_slice(piece);
            }
        };
        match name {
            // Tiny tokens in a table 63 deep: the node cap, then skip mode.
            "tiny_tokens" => {
                bytes.extend_from_slice(b"T = ");
                bytes.extend(std::iter::repeat_n(b'{', 63));
                fill(&mut bytes, b"1,", 63);
                bytes.extend(std::iter::repeat_n(b'}', 63));
                (bytes, &["T"])
            }
            "one_huge_key" => {
                bytes.extend_from_slice(b"T = { [\"");
                fill(&mut bytes, b"k", 9);
                bytes.extend_from_slice(b"\"] = 1 }");
                (bytes, &["T"])
            }
            "fragments" => {
                fill(&mut bytes, b"A = {[\n", 0);
                (bytes, &["A"])
            }
            "deep_skipped" => {
                let mut line = b"A = ".to_vec();
                line.extend(std::iter::repeat_n(b'{', 64));
                line.extend(std::iter::repeat_n(b'}', 64));
                line.push(b'\n');
                fill(&mut bytes, &line, 0);
                (bytes, &[])
            }
            other => unreachable!("no workload {other}"),
        }
    }

    /// Release `tag` of `gh:bench/addonNN` (`n`) as the latest: `AddonNN` and
    /// `AddonNN_Options`, `files` files each, loading on retail, classic and era.
    pub fn addon_release(scenario: &std::path::Path, n: usize, tag: &str, files: usize) {
        let name = format!("Addon{n:02}");
        let toc = format!("## Interface: 120105, 50504, 11509\n## Version: {tag}\n");
        let mut entries: Vec<(String, String)> = Vec::new();
        for folder in [name.clone(), format!("{name}_Options")] {
            entries.push((format!("{folder}/{folder}.toc"), toc.clone()));
            for i in 0..files {
                entries.push((format!("{folder}/f{i:03}.lua"), format!("-- {tag} {i}\n")));
            }
        }
        let borrowed: Vec<(&str, &str)> = entries
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let zip = format!("{name}.zip");
        let assets: [Asset<'_>; 1] = [(zip.as_str(), &[], borrowed)];
        let base = format!("https://github.com/bench/addon{n:02}");
        gh_release_v(scenario, &base, tag, &assets);
        gh_latest(scenario, &base, tag);
    }

    /// The global lock hold of a whole restore of backup `id` on `home`, after one file
    /// of `wtf` changed.
    pub fn restore_lock_hold(home: &Home, id: &str, wtf: &std::path::Path) -> u64 {
        std::fs::write(
            wtf.join("Account/FAKEACCT/FakeRealm/Char0/SavedVariables/Addon0.lua"),
            "changed",
        )
        .unwrap();
        let argv = ["backup", "restore", id, "-t", "retail", "-v", "--json"].map(str::to_owned);
        let r = home.exec(&argv);
        r.expect(0);
        let held = super::lock_held_ms(&r);
        assert!(held.is_some(), "no lock log line\n{}", r.context());
        held.unwrap_or_default()
    }
}

/// The three clients of the bench home and the economy test.
const CLIENTS: &[&str] = &["wow", "wow_classic", "wow_classic_era"];
const TARGETS: &[&str] = &["retail", "classic", "era"];

/// `argc bench`'s home, built at `$BENCH_HOME` (read by this test alone, never by
/// omaddons): retail, classic and era, each with the same 10 addons installed one
/// release behind (30 records, 5,040 managed files; each ref and archive is shared by
/// the three), a fixture scenario serving the newer releases, and `bench.env`, the
/// environment every benchmarked command runs with. The cache is then deleted, so the
/// home starts cold: no memo, no blobs.
#[test]
#[ignore = "argc bench's home (a debug build, which replays fixtures): BENCH_HOME=<dir> cargo test --test integration perf::bench_home -- --ignored"]
fn bench_home() {
    // A builder for `argc bench`, not a budget: without its directory it builds nothing.
    let Some(dir) = std::env::var_os("BENCH_HOME").map(PathBuf::from) else {
        return;
    };
    assert!(!dir.exists(), "{} exists", dir.display());
    let home = Home::at(&dir, CLIENTS);
    let refs: Vec<String> = (0..10).map(|n| format!("gh:bench/addon{n:02}")).collect();
    for n in 0..10 {
        addon_release(&home.http, n, "v1", 83);
    }
    for target in TARGETS {
        let mut args: Vec<&str> = vec!["install"];
        args.extend(refs.iter().map(String::as_str));
        args.extend(["-t", target]);
        home.run(&args).expect(0);
    }
    for n in 0..10 {
        addon_release(&home.http, n, "v2", 83);
    }
    let records: usize = TARGETS
        .iter()
        .map(|t| common::state::record(&home, t).addons.len())
        .sum();
    assert_eq!(records, 30);
    fs::remove_dir_all(home.env.cache().join("omaddons")).unwrap();
    let mut lines = home.env.env_lines();
    lines.push(format!("OMADDONS_HTTP_FIXTURES={}", home.http.display()));
    fs::write(dir.join("bench.env"), lines.join("\n") + "\n").unwrap();
}

/// The throwaway home of the one live run (H3): retail, classic and era at `$LIVE_HOME`
/// (read by this test alone), with no addon and no fixtures, and `live.env`, its
/// environment. The release binary then installs catalog addons into it from the real
/// sources; it never touches a real client.
#[test]
#[ignore = "the live run's home: LIVE_HOME=<dir> cargo test --test integration perf::live_home -- --ignored"]
fn live_home() {
    // A builder for the live run, not a budget: without its directory it builds nothing.
    let Some(dir) = std::env::var_os("LIVE_HOME").map(PathBuf::from) else {
        return;
    };
    assert!(!dir.exists(), "{} exists", dir.display());
    let home = Home::at(&dir, CLIENTS);
    let lines = home.env.env_lines();
    fs::write(dir.join("live.env"), lines.join("\n") + "\n").unwrap();
}

/// The keyless coverage study's home (omad-m2-audit-yegz.7.7, re-measured by
/// omad-author-built-loading-5po6.6) at `$COVERAGE_HOME` (read by this test alone): one
/// client per live family (retail, Mists Classic, Anniversary, Classic Era, Forever and
/// Titan) and a custom 3.3.5a client, with no addon and no fixtures, and
/// `coverage.env`, its environment. A release binary then dry-runs installs from the
/// real sources into it; it never touches a real client.
#[test]
#[ignore = "the coverage study's home: COVERAGE_HOME=<dir> cargo test --test integration perf::coverage_home -- --ignored"]
fn coverage_home() {
    // A builder for the study, not a budget: without its directory it builds nothing.
    let Some(dir) = std::env::var_os("COVERAGE_HOME").map(PathBuf::from) else {
        return;
    };
    assert!(!dir.exists(), "{} exists", dir.display());
    let products = [
        "wow",
        "wow_classic",
        "wow_anniversary",
        "wow_classic_era",
        "wowf",
        "wow_classic_titan",
    ];
    let mut home = Home::at(&dir, &products);
    let legacy = CustomRoot::at(home.env.root().join("wow-335")).version(3, 3, 5, 12_340);
    home.with_custom(legacy);
    let lines = home.env.env_lines();
    fs::write(dir.join("coverage.env"), lines.join("\n") + "\n").unwrap();
}

/// A release build registers no test fixer, whatever `OMADDONS_TEST_FIXERS` says: naming
/// one is an unknown id (exit 64), and a home with every test fixer's subject planted
/// reports no `fm-test-` finding. Not a time budget: it is here because the checkpoint
/// runs this file's ignored tests on a release build.
#[test]
#[cfg(not(debug_assertions))]
#[ignore = "release build, checkpoint"]
fn release_build_lists_no_test_fixers() {
    use common::doctor::test_fixers::{
        ENV, Kind, Side, plant_flatten, plant_quarantine, plant_write, register,
    };
    let home = Home::new(&["wow"]);
    let argv = |args: &[&str]| -> Vec<String> { args.iter().map(|a| (*a).to_owned()).collect() };
    let named = home.exec_with(
        &argv(&["doctor", "--only", "fm-test-write", "--json"]),
        &[ENV],
    );
    assert_eq!(named.code, 64, "{}", named.context());
    register(&home, "retail");
    plant_write(&home, "a.write", b"broken\n");
    plant_quarantine(&home, Side::State, "b.quarantine", Kind::File);
    plant_flatten(&home, "_retail_", "TestWrapA", "Inner");
    let diagnosed = home.exec_with(&argv(&["doctor", "--json"]), &[ENV]);
    assert!([0, 1].contains(&diagnosed.code), "{}", diagnosed.context());
    assert!(
        !diagnosed.stdout.contains("fm-test-"),
        "{}",
        diagnosed.context()
    );
}

/// The budget: 5,000 files in 20 folders, installed through the engine, checked by Stat
/// right after in under 30 ms warm, with at most 1% re-hashed. (Moved from `drift.rs`.)
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn five_thousand_files_check_in_thirty_milliseconds() {
    let home = Home::new(&["wow"]);
    let mut entries: Vec<(String, String)> = Vec::new();
    for folder in 0..20 {
        let name = format!("Big{folder:02}");
        entries.push((
            format!("{name}/{name}.toc"),
            "## Interface: 120105\n".to_owned(),
        ));
        for file in 0..249 {
            entries.push((
                format!("{name}/f{file:03}.lua"),
                format!("-- {folder} {file}\n"),
            ));
        }
    }
    let borrowed: Vec<(&str, &str)> = entries
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    // A local archive: release builds replay no HTTP fixture.
    let zip = home.env.root().join("Big.zip");
    fs::write(&zip, common::archives::zip_text(&borrowed)).unwrap();
    home.run(&["install", &zip.display().to_string(), "-t", "retail"])
        .expect(0);
    let warm = drift_report(&home, DriftMode::Stat);
    assert_eq!(warm.files_checked, 5_000);
    let started = std::time::Instant::now();
    let r = drift_report(&home, DriftMode::Stat);
    let elapsed = started.elapsed();
    measured(format_args!(
        "drift Stat over 5,000 files: {elapsed:?}, {} re-hashed",
        r.files_rehashed
    ));
    assert!(r.files_rehashed <= 50, "{} re-hashed", r.files_rehashed);
    assert!(
        elapsed < std::time::Duration::from_millis(30),
        "Stat took {elapsed:?}"
    );
}

/// Loading the stored WoWInterface index (8,218 entries, as on WoWInterface) stays under
/// 40 ms. (Moved from `wowi_index.rs`.)
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn a_full_wowi_index_loads_under_40_ms() {
    let env = TestEnv::new();
    let scenario = env.root().join("scenario");
    fs::create_dir_all(&scenario).unwrap();
    let paths = paths(&env);
    let f = |name: &str| FolderName::new(name).unwrap();
    let entries: Vec<WowiEntry> = (0..8218u64)
        .map(|uid| WowiEntry {
            uid,
            name: format!("Project {uid}"),
            author: format!("Author {}", uid % 900),
            version: format!("v{}.{}", uid % 13, uid % 7),
            date_ms: 1_700_000_000_000 + uid,
            downloads: uid * 31,
            monthly: uid % 500,
            favorites: uid % 90,
            cat: uid % 40,
            dirs: (0..(1 + uid % 74).min(if uid % 50 == 0 { 74 } else { 3 }))
                .map(|i| f(&format!("Project{uid}_Module{i}")))
                .collect(),
            compat: Vec::new(),
        })
        .collect();
    let stored = StoredIndex {
        fetched_at: Timestamp::now() - jiff::SignedDuration::from_mins(1),
        max_age_s: 3600,
        entries,
    };
    let path = paths.wowi_index();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, omaddons::state::store::encode(&stored).unwrap()).unwrap();
    let bytes = fs::metadata(&path).unwrap().len();
    let http = replay(&env, &scenario);
    let mut times: Vec<Duration> = (0..11)
        .map(|_| {
            let start = Instant::now();
            let load =
                WowiIndex::load_or_refresh(&paths, http.as_ref(), IndexPolicy::Offline).unwrap();
            assert_eq!(load.index.unwrap().entries().len(), 8218);
            start.elapsed()
        })
        .collect();
    times.sort();
    let median = times[times.len() / 2];
    measured(format_args!(
        "WoWInterface index: {bytes} bytes, median load {median:?}"
    ));
    assert!(median < Duration::from_millis(40), "{median:?} ({times:?})");
}

/// Request economy (normal gate): `update` across three clients sharing one ref and
/// archive downloads it once, and asks `releases/latest` once.
#[test]
fn update_across_clients_downloads_each_archive_once() {
    let mut home = Home::new(CLIENTS);
    let toc = "## Interface: 120105, 50504, 11509\n## Version: v1\n";
    let toc2 = "## Interface: 120105, 50504, 11509\n## Version: v2\n";
    let v1: [Asset<'_>; 1] = [("foo.zip", &[], vec![("Foo/Foo.toc", toc)])];
    let v2: [Asset<'_>; 1] = [("foo.zip", &[], vec![("Foo/Foo.toc", toc2)])];
    home.installed_behind(
        "https://github.com/o/foo",
        ("v1", &v1),
        ("v2", &v2),
        TARGETS,
    );
    let before = net_log(&home).len();
    let r = home.run(&["update"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["data"]["summary"]["changed"], 3, "{}", r.context());
    let log = net_log(&home);
    let new = &log.entries[before..];
    let count = |purpose: &str| new.iter().filter(|e| e.purpose == purpose).count();
    assert_eq!(
        (count("release-latest"), count("asset")),
        (1, 1),
        "{}",
        log.context()
    );
    common::netlog::assert_no_github_rest(new);
}

/// `capabilities --json` (M4-D4), an agent's first call, on a home with two clients and a
/// config.toml of three overrides, median of 21 runs of the binary, process spawn
/// included: under 20 ms.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn capabilities_answers_in_under_20_ms() {
    let home = Home::new(&["wow", "wow_classic"]);
    let config = home.env.config().join("omaddons");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        "git_hosts = [\"codeberg.org\"]\n\
         [limits]\ndownload_max_bytes = \"1GiB\"\n\
         [sources.wowi]\nenabled = false\n",
    )
    .unwrap();
    let first = home.run(&["capabilities", "--json"]);
    let envelope = first.expect(0);
    assert!(envelope["data"]["config_error"].is_null(), "{envelope}");
    let bytes = first.stdout.len();
    let (median, times) = median_of_21(|| {
        home.run(&["capabilities", "--json"]).expect(0);
    });
    let p95 = times[times.len() * 95 / 100];
    measured(format_args!(
        "capabilities --json: median {median:?}, p95 {p95:?}; {bytes} bytes of stdout"
    ));
    assert!(
        median < Duration::from_millis(20),
        "capabilities median {median:?} (runs {times:?})"
    );
}

/// `status --cached` (the widget's view) with 100 records, a memo of 200 entries copied
/// from recorded release metadata (DBM, BigWigs and WeakAuras), Lua errors, 200 backups
/// and the bar widget installed, median of 21 runs of the binary: under 5 ms.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn status_cached_stays_under_5_ms_with_a_realistic_memo() {
    // Realistic release sets, from the recorded catalog scenario.
    let env = TestEnv::new();
    let http = replay(&env, &scenario("catalog"));
    let wanted: Vec<Wanted> = [
        "gh:DeadlyBossMods/DeadlyBossMods",
        "gh:BigWigsMods/BigWigs",
        "gh:WeakAuras/WeakAuras2",
    ]
    .iter()
    .map(|r| Wanted::stable(r.parse().unwrap()))
    .collect();
    let sets: Vec<_> = fetch_once(
        wanted,
        &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
        http.as_ref(),
    )
    .into_values()
    .map(Result::unwrap)
    .collect();
    assert_eq!(sets.len(), 3);

    // Retail, registered by a first install, then 100 records. With `$STATUS_HOME` (read
    // by this test alone) the home is built and kept there, with `status.env`, its
    // environment, so a release `status --cached` can be profiled or timed on it.
    let kept = std::env::var_os("STATUS_HOME").map(PathBuf::from);
    let home = match &kept {
        Some(dir) => {
            assert!(!dir.exists(), "{} exists", dir.display());
            Home::at(dir, &["wow"])
        }
        None => Home::new(&["wow"]),
    };
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("seed.zip");
    fs::write(&zip, zip_text(&[("Seed/Seed.toc", &toc("120105", &[]))])).unwrap();
    let zip = zip.display().to_string();
    home.run(&[
        "install",
        "gh:perf/seed",
        "--file",
        &zip,
        "-t",
        "retail",
        "--offline",
    ])
    .expect(0);
    let key = common::state::key_of(&home, "retail");
    let mut record = common::state::record(&home, "retail");
    let seed = record.addons.values().next().unwrap().clone();
    record.addons.clear();
    let at = Timestamp::now();
    let mut entries: Vec<(String, MemoEntry)> = Vec::new();
    for i in 0..200 {
        let r: Ref = format!("gh:perf/addon{i:03}").parse().unwrap();
        let mut set = sets[i % sets.len()].clone();
        set.r#ref = r.clone();
        if i < 100 {
            let mut addon = seed.clone();
            addon.name = format!("Addon{i:03}");
            addon.r#ref = r.clone();
            addon.release_id = set.releases.last().map(|rel| rel.release_id.clone());
            addon.folders = vec![FolderName::try_from(addon.name.clone()).unwrap()];
            // On disk too: `--cached` checks each managed folder's presence.
            let folder = home.addons("_retail_").join(&addon.name);
            fs::create_dir_all(&folder).unwrap();
            fs::write(
                folder.join(format!("{}.toc", addon.name)),
                toc("120105", &[]),
            )
            .unwrap();
            record.addons.insert(
                omaddons::state::records::TargetRecord::addon_key(&addon.name),
                addon,
            );
        }
        let entry = MemoEntry::of(&r, Channel::Stable, &Ok(set), at).unwrap();
        entries.push((memo::key(&r, Channel::Stable), entry));
    }
    fs::write(
        paths(&home.env).target_record(&key),
        omaddons::state::store::encode(&record).unwrap(),
    )
    .unwrap();
    home.seed_memo(&ReleaseMemo {
        entries: entries.into_iter().collect(),
        ..ReleaseMemo::default()
    });
    let memo_bytes = fs::metadata(home.memo()).unwrap().len();
    assert!(memo_bytes > 100_000, "a realistic memo: {memo_bytes} bytes");

    // Lua errors (M3-D20): BugGrabber, with a 2 MB database in each of 16 accounts whose
    // SavedVariables hold 40 other files, read by `errors` (the errors memo) and
    // dismissed (the acknowledgement). `--cached` stats and lists them, never opens one.
    common::addons::addon_folder(
        &home.addons("_retail_"),
        "!BugGrabber",
        &[(
            "!BugGrabber.toc",
            &toc("120105", &["## SavedVariables: BugGrabberDB"]),
        )],
        &[],
    );
    let bug = common::buggrabber::flood(common::buggrabber::Writer::Modern, 16_000).render();
    assert!(bug.len() >= 2 << 20, "a 2 MB database: {} bytes", bug.len());
    for account in 0..16 {
        let sv = home
            .client("_retail_")
            .join(format!("WTF/Account/FAKEACCT{account:02}/SavedVariables"));
        fs::create_dir_all(&sv).unwrap();
        fs::write(sv.join(common::buggrabber::FILE), &bug).unwrap();
        for other in 0..40 {
            fs::write(sv.join(format!("Addon{other:03}.lua")), "Addon = {}\n").unwrap();
        }
    }
    home.run(&["errors"]).expect(0);
    home.run(&["errors", "dismiss"]).expect(0);
    let errors_memo = home.env.cache().join("omaddons/errors.json");
    let errors_memo_bytes = fs::metadata(&errors_memo).unwrap().len();
    assert!(home.state().join("errors_ack.json").is_file());

    // Backups (M3-D37): 200 manual ones, added up by `backup list` in the projection;
    // `--cached` lists them and opens no `backup.json`.
    common::backups::seed_many(&home.client("_retail_"), 200);
    home.run(&["backup", "list"]).expect(0);

    // The bar widget, installed and current (E4): `status` hashes what this binary would
    // render for it and compares, on every call. Written from E1's render, so no Omarchy
    // tool runs.
    let state = paths(&home.env);
    let rendered = omaddons::omarchy::render::render(
        std::path::Path::new(env!("CARGO_BIN_EXE_omaddons")),
        &omaddons::omarchy::render::Context::of(&state),
        &state.omarchy_plugin_id(),
    )
    .unwrap();
    for (rel, bytes) in &rendered.files {
        let path = state.omarchy_plugin_dir().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    if let Some(dir) = &kept {
        let lines = home.env.env_lines();
        fs::write(dir.join("status.env"), lines.join("\n") + "\n").unwrap();
    }

    // A cold run, then 21 warm ones.
    let start = Instant::now();
    let check = home.run(&["status", "--cached"]).expect(0);
    let cold = start.elapsed();
    let updates = &check["data"]["targets"][0]["updates"];
    assert!(updates["checked_at"].is_string(), "{updates}");
    assert_eq!(
        check["data"]["targets"][0]["drift"],
        serde_json::json!([]),
        "every managed folder is present"
    );
    let errors = &check["data"]["targets"][0]["errors"];
    assert_eq!(
        (&errors["state"], &errors["coverage"], &errors["new"]),
        (
            &serde_json::json!("ok"),
            &serde_json::json!("complete"),
            &serde_json::json!(0)
        ),
        "the memo stands and the dismissal holds: {errors}"
    );
    assert!(errors["total"].as_u64().unwrap() > 0, "{errors}");
    let backups = &check["data"]["targets"][0]["backups"];
    assert_eq!(
        (&backups["count"], &backups["stale"]),
        (&serde_json::json!(200), &serde_json::json!(false)),
        "the projection covers them: {backups}"
    );
    let widget = &check["data"]["widget"];
    assert_eq!(
        (&widget["installed"], &widget["outdated"]),
        (&serde_json::json!(true), &serde_json::json!(false)),
        "the widget is compared: {widget}"
    );
    let (median, times) = median_of_21(|| {
        home.run(&["status", "--cached"]).expect(0);
    });
    let p95 = times[times.len() * 95 / 100];
    measured(format_args!(
        "status --cached: median {median:?}, p95 {p95:?}, first {cold:?}; memo {memo_bytes} \
         bytes, 100 records; 16 accounts with a {} byte BugGrabber file each, errors memo \
         {errors_memo_bytes} bytes; 200 backups; the bar widget installed",
        bug.len()
    ));
    assert!(
        median < Duration::from_millis(5),
        "status --cached median {median:?} (memo {memo_bytes} bytes; runs {times:?})"
    );

    // A 20 MB history of three runs at a time interleaving, written around omaddons:
    // the next change rebuilds the projection, and `--cached` reads that alone.
    let history_file = paths(&home.env).history_file();
    let mut text = fs::read_to_string(&history_file).unwrap();
    let mut newest = None;
    while text.len() < 20 << 20 {
        let runs = [RunId::new(), RunId::new(), RunId::new()];
        for _ in 0..4 {
            for run in runs {
                text.push_str(&serde_json::to_string(&history_line(key, run)).unwrap());
                text.push('\n');
            }
        }
        newest = Some(runs[2]);
    }
    fs::write(&history_file, &text).unwrap();
    let start = Instant::now();
    home.run(&["config", "set", "check_interval_hours", "6"])
        .expect(0);
    let rebuilt = start.elapsed();
    let start = Instant::now();
    let first = home.run(&["status", "--cached"]).expect(0);
    let cold = start.elapsed();
    let change = &first["data"]["last_change"];
    assert_eq!(
        (&first["data"]["last_change_state"], &change["run"]),
        (
            &serde_json::json!("ok"),
            &serde_json::json!(newest.unwrap().to_string())
        ),
        "{change}"
    );
    assert_eq!(change["changes"]["replaced"], 4 * 12);
    let (median, times) = median_of_21(|| {
        home.run(&["status", "--cached"]).expect(0);
    });
    let p95 = times[times.len() * 95 / 100];
    measured(format_args!(
        "status --cached, {} MB history: median {median:?}, p95 {p95:?}, first {cold:?}; \
         the change that rebuilt the projection {rebuilt:?}",
        text.len() >> 20
    ));
    assert!(
        median < Duration::from_millis(5),
        "status --cached median {median:?} with a 20 MB history (runs {times:?})"
    );
}

/// Every commit holds the global lock under 50 ms (vision §7 M1): 20 updates of one
/// addon, then three addons installed in one transaction, removed together, and that
/// remove undone. Local archives only: release builds replay no fixture.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn commits_hold_the_lock_under_50_ms() {
    let s = Scene::new();
    for name in ["Baz", "Qux"] {
        let zip = s.dl.join(format!("{}.zip", name.to_ascii_lowercase()));
        fs::write(
            &zip,
            zip_text(&[(&format!("{name}/{name}.toc"), "## Interface: 120105\n")]),
        )
        .unwrap();
    }
    let mut held: Vec<(String, u64)> = Vec::new();
    let mut run = |what: String, mut args: Vec<String>| {
        args.push("-v".to_owned());
        let r = s.run(&args, "");
        assert_eq!(r.code, 0, "{what}\n{}", r.context());
        let ms = lock_held_ms(&r);
        assert!(ms.is_some(), "{what}: no lock log line\n{}", r.context());
        held.extend(ms.map(|ms| (what, ms)));
    };
    for i in 0..20 {
        let zip = if i % 2 == 0 { "v1" } else { "v2" };
        run(format!("update {i}"), s.install_args("gh:o/foo", zip));
    }
    let (bar, baz, qux) = (s.zip("bar"), s.zip("baz"), s.zip("qux"));
    run(
        "install of three".into(),
        s.args(&["install", &bar, &baz, &qux]),
    );
    run(
        "remove of three".into(),
        s.args(&["remove", "Bar", "Baz", "Qux"]),
    );
    run("undo of the remove".into(), s.args(&["undo"]));
    let mut ms: Vec<u64> = held.iter().map(|(_, ms)| *ms).collect();
    ms.sort_unstable();
    measured(format_args!(
        "lock_held_ms over {} commits: min {} median {} max {}; {held:?}",
        ms.len(),
        ms[0],
        ms[ms.len() / 2],
        ms[ms.len() - 1]
    ));
    assert!(
        held.iter().all(|(_, ms)| *ms < 50),
        "lock hold times {held:?}"
    );
}

/// An automatic backup costs the commit window one history append: the day's first
/// change, with `auto_backup = "daily-copy"` over 16 MiB of settings, still holds the
/// lock under 50 ms (the copy is made before it).
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn a_change_with_an_automatic_backup_holds_the_lock_under_50_ms() {
    let s = Scene::new();
    common::wtf::Wtf::at(s.home.client("_retail_"))
        .saved_variables("Foo", 16 << 20)
        .build();
    let r = s.run(&s.args(&["config", "set", "auto_backup", "daily-copy"]), "");
    assert_eq!(r.code, 0, "{}", r.context());
    let mut args = s.install_args("gh:o/foo", "v1");
    args.push("-v".to_owned());
    let r = s.run(&args, "");
    assert_eq!(r.code, 0, "{}", r.context());
    let ms = lock_held_ms(&r);
    measured(format_args!(
        "lock_held_ms with an automatic backup: {ms:?}"
    ));
    let listed = s.run(&s.args(&["backup", "list"]), "");
    assert!(
        listed.stdout.contains("\"kind\":\"auto\""),
        "the backup was made: {}",
        listed.context()
    );
    assert!(ms.is_some_and(|ms| ms < 50), "{ms:?}");
}

/// 16 MiB of SavedVariables and a 64 MiB folder still hold the lock under 50 ms: what
/// can be copied or hashed is done before the lock. A release build ignores failpoints,
/// so SavedVariables are copied the way this filesystem allows (reflinks where it can).
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn large_saved_variables_and_folders_hold_the_lock_under_50_ms() {
    let s = Scene::new();
    s.heavy_saved_variables();
    let [one, two] = s.big_folders();
    let mut held: Vec<(&str, u64)> = Vec::new();
    let mut run = |what: &'static str, mut args: Vec<String>| {
        args.push("-v".to_owned());
        let r = s.run(&args, "");
        assert_eq!(r.code, 0, "{what}\n{}", r.context());
        let ms = lock_held_ms(&r);
        assert!(ms.is_some(), "{what}: no lock log line\n{}", r.context());
        held.extend(ms.map(|ms| (what, ms)));
    };
    run(
        "install with SavedVariables",
        s.install_args("gh:o/foo", "v1"),
    );
    run(
        "update with SavedVariables",
        s.install_args("gh:o/foo", "v2"),
    );
    run(
        "install over a 64 MiB folder",
        s.args(&["install", "gh:o/big", "--file", &one, "--force"]),
    );
    run(
        "update of a 64 MiB folder",
        s.args(&["install", "gh:o/big", "--file", &two]),
    );
    run("undo of that update", s.args(&["undo"]));
    measured(format_args!("lock_held_ms with large data: {held:?}"));
    assert!(
        held.iter().all(|(_, ms)| *ms < 50),
        "lock hold times {held:?}"
    );
}

/// `status --cached` budgets 5 ms, so classes depth over 500 folders stays under it.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn classes_inventory_of_500_folders_under_5_ms() {
    use omaddons::inventory::{Depth, inventory};
    use omaddons::rules::RulesTable;

    let env = TestEnv::new();
    let target = common::targets::battlenet(&env, "wow");
    for i in 0..500 {
        fs::create_dir_all(target.addons_dir().join(format!("Addon{i:03}"))).unwrap();
    }
    let table = RulesTable::builtin();
    let check = inventory(&target, None, &table, Depth::Classes).unwrap();
    assert_eq!(check.counts.unmanaged, 500);
    let (median, times) = median_of_21(|| {
        inventory(&target, None, &table, Depth::Classes).unwrap();
    });
    measured(format_args!(
        "classes inventory of 500 folders: median {median:?}"
    ));
    assert!(
        median < Duration::from_millis(5),
        "median {median:?} (runs {times:?})"
    );
}

/// Discovery from the cache, with nothing scanned, takes under 5 ms for 10 roots.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn cached_discovery_of_10_roots_under_5_ms() {
    use omaddons::discover::cache::{DiscoveryMode, discover_cached};
    use omaddons::discover::{DiscoveryInput, ScanLimits};
    use omaddons::rules::RulesTable;
    use omaddons::state::paths::Paths;

    let env = TestEnv::new();
    for i in 0..10 {
        common::trees::CustomRoot::at(env.home().join(format!("Games/client{i}"))).build();
    }
    let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
    let input = DiscoveryInput {
        home: env.home(),
        ..DiscoveryInput::default()
    };
    let table = RulesTable::builtin();
    let run = |mode| discover_cached(&input, &table, &paths, ScanLimits::default(), mode);
    run(DiscoveryMode::Full);
    let (d, stats) = run(DiscoveryMode::NoScan);
    assert_eq!(
        (d.targets.len(), stats.reused, stats.scanned),
        (10, 10, false)
    );
    let (median, times) = median_of_21(|| {
        run(DiscoveryMode::NoScan);
    });
    measured(format_args!(
        "cached discovery of 10 roots: median {median:?}"
    ));
    assert!(
        median < Duration::from_millis(5),
        "median {median:?} (runs {times:?})"
    );
}

/// Staging an archive of 5,001 small files takes under a second.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn staging_5001_files_under_1_s() {
    use omaddons::engine::inspect::{ArchiveLimits, inspect};
    use omaddons::engine::stage::stage;
    use omaddons::fsx::{InflightLock, ScopeTable};
    use omaddons::model::TxId;
    use omaddons::rules::RulesTable;

    let env = TestEnv::new();
    let target = common::targets::battlenet(&env, "wow");
    let paths = paths(&env);
    let scopes = ScopeTable::new(&paths, std::slice::from_ref(&target.client_dir), None).unwrap();
    let files: Vec<(String, Vec<u8>)> = (0..5_000)
        .map(|i| {
            (
                format!("Big/d{}/f{i}.lua", i % 50),
                format!("return {i}\n").into_bytes(),
            )
        })
        .chain([("Big/Big.toc".to_owned(), b"## Interface: 120105\n".to_vec())])
        .collect();
    let entries: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, c)| (n.as_str(), c.as_slice()))
        .collect();
    let blob = env.root().join("big.zip");
    fs::write(&blob, common::archives::zip_of(&entries)).unwrap();
    let inspected = inspect(&blob, &ArchiveLimits::default(), &RulesTable::builtin()).unwrap();
    let inflight = InflightLock::create(&paths, &TxId::new()).unwrap();
    let started = Instant::now();
    let staged = stage(&inflight, &scopes, &target, Some(&blob), &inspected).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(staged.folders[0].index.files.len(), 5_001);
    measured(format_args!("staging 5,001 files: {elapsed:?}"));
    assert!(
        elapsed < Duration::from_secs(1),
        "staging 5,001 files took {elapsed:?}"
    );
}

/// Running-client detection on the real `/proc` takes under 2 ms
/// (omad-m1-working-slice-ejl.3.10). It only reads `/proc`.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn detect_running_on_the_real_proc_under_2_ms() {
    use omaddons::discover::DiscoverySource;
    use omaddons::discover::battlenet::{BattlenetHints, discover_root};
    use omaddons::discover::running::detect_running;
    use omaddons::model::Target;
    use omaddons::rules::RulesTable;

    let env = TestEnv::new();
    let prefix = common::trees::Prefix::new(&env, "battlenet");
    let mut root = common::trees::BattlenetRoot::at(prefix.wow_root());
    for code in ["wow", "wow_classic", "wow_classic_era", "wowf"] {
        root = root.product(code);
    }
    let (found, _) = discover_root(
        &root.build(),
        &RulesTable::builtin(),
        &BattlenetHints::default(),
        "battlenet",
        &DiscoverySource::Scan,
    );
    let targets: Vec<Target> = found.into_iter().map(|t| t.target).collect();
    let _warm = detect_running(&targets);
    let started = Instant::now();
    let result = detect_running(&targets);
    let elapsed = started.elapsed();
    assert!(
        result.clients.is_empty(),
        "the synthetic targets are not running"
    );
    assert_eq!(
        result.unavailable, None,
        "the real /proc lists this process"
    );
    measured(format_args!(
        "detect_running on the real /proc: {elapsed:?}"
    ));
    assert!(
        elapsed < Duration::from_millis(2),
        "detect_running took {elapsed:?}"
    );
}

#[test]
#[ignore = "a release-build budget on btrfs: cargo test --release --test integration perf:: -- --ignored"]
fn clone_tree_500_mb() {
    use common::wtf::Wtf;
    use omaddons::backup::tree::{CloneOpts, clone_tree};
    use omaddons::fsx::{ClonePolicy, ScopeTable, WriteSession, Writer};
    use omaddons::output::ProgressSink;
    use omaddons::state::paths::Paths;

    let _alone = HALF_GIB
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    const TOTAL: u64 = 500 * 1024 * 1024;
    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let dir = tempfile::tempdir_in(&target).unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    // ubs:ignore — comparing a filesystem magic number, not a secret
    let btrfs = rustix::fs::statfs(&root).is_ok_and(|s| i128::from(s.f_type) == 0x9123_683E);
    if !btrfs {
        common::checkpoint::skip("clone_tree_500_mb", "target/ is not on btrfs");
        return;
    }
    // Checkpoint criterion 8: 500 MB in realistic file counts (5,000) under 1 s; 20,000
    // files are measured and reported without a budget.
    for (files, budget) in [(5_000u64, Some(Duration::from_secs(1))), (20_000, None)] {
        let client = root.join(format!("client-{files}"));
        fs::create_dir_all(client.join("Interface/AddOns")).unwrap();
        let wtf = Wtf::at(&client).big(TOTAL, files).build();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
        let mut times = Vec::new();
        for run in 0..5 {
            let stage = client.join(format!("Interface/.omaddons/stage/run{run}"));
            let mut s = WriteSession::untracked(Writer::Backup, &scopes, "perf");
            let opts = CloneOpts::capture(ClonePolicy::Required);
            let started = Instant::now();
            let copy = clone_tree(
                &mut s,
                &wtf,
                &stage.join("WTF"),
                &opts,
                None,
                &ProgressSink::none(),
            )
            .unwrap();
            times.push(started.elapsed());
            assert_eq!(copy.files, files);
            assert_eq!(copy.copied_bytes, 0, "clone-only: not a byte copied");
            fs::remove_dir_all(&stage).unwrap();
        }
        times.sort();
        let median = times[times.len() / 2];
        measured(format_args!(
            "clone_tree of 500 MiB in {files} files (btrfs, clone-only, second walk included): median {median:?}, runs {times:?}"
        ));
        if let Some(budget) = budget {
            assert!(
                median < budget,
                "{files} files: median {median:?} >= {budget:?}"
            );
        }
    }
}

/// Checkpoint criterion 8 through the binary (D12), on btrfs under `target/`: `backup
/// create` of 500 MiB in 5,000 files (cloned, the consistency recheck included) under
/// 1 s, median of 5; 20,000 files and `--verify` measured and reported; and a whole
/// restore of that tree holding the global lock under 50 ms (stamp digests, so nothing
/// is hashed under the lock). One test, so the 500 MiB runs never overlap each other.
#[test]
#[ignore = "a release-build budget on btrfs: cargo test --release --test integration perf:: -- --ignored"]
fn backups_of_500_mb_meet_their_budgets() {
    use common::wtf::Wtf;

    let _alone = HALF_GIB
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    const TOTAL: u64 = 500 * 1024 * 1024;
    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let base = tempfile::tempdir_in(&target).unwrap();
    // ubs:ignore — comparing a filesystem magic number, not a secret
    let btrfs = rustix::fs::statfs(base.path()).is_ok_and(|s| i128::from(s.f_type) == 0x9123_683E);
    if !btrfs {
        common::checkpoint::skip(
            "backups_of_500_mb_meet_their_budgets",
            "target/ is not on btrfs",
        );
        return;
    }
    let made = |envelope: &serde_json::Value| {
        envelope["data"]["targets"][0]["backup"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    for (files, budget) in [(5_000u64, Some(Duration::from_secs(1))), (20_000, None)] {
        let root = base.path().join(format!("home-{files}"));
        fs::create_dir_all(&root).unwrap();
        let home = Home::at(&root, &["wow"]);
        let wtf = Wtf::at(home.client("_retail_")).big(TOTAL, files).build();
        let backups = home.client("_retail_").join("Interface/.omaddons/backups");
        let mut times = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            let created = home.run(&["backup", "create", "-t", "retail"]).expect(0);
            times.push(started.elapsed());
            let b = &created["data"]["targets"][0]["backup"];
            assert_eq!(
                (&b["reflinked"], &b["files"]),
                (&serde_json::json!("all"), &serde_json::json!(files)),
                "{created}"
            );
            fs::remove_dir_all(backups.join(made(&created))).unwrap();
        }
        times.sort();
        let median = times[times.len() / 2];
        measured(format_args!(
            "backup create of 500 MiB in {files} files (btrfs, the binary): median {median:?}, p95 {:?}, runs {times:?}",
            times[times.len() - 1]
        ));
        if let Some(budget) = budget {
            assert!(
                median < budget,
                "{files} files: median {median:?} >= {budget:?}"
            );
        }
        if files != 5_000 {
            continue;
        }
        // --verify: reported, no budget (M3-D47).
        let started = Instant::now();
        let verified = home
            .run(&["backup", "create", "-t", "retail", "--verify"])
            .expect(0);
        measured(format_args!(
            "backup create --verify of 500 MiB in {files} files: {:?}",
            started.elapsed()
        ));
        let id = made(&verified);
        // A whole restore of it on btrfs: reported. Its commit window is bound by the
        // fsyncs a durable commit makes there (about 3 ms each), as every commit's is.
        let held = restore_lock_hold(&home, &id, &wtf);
        measured(format_args!(
            "whole restore of 500 MiB in {files} files on btrfs: lock_held_ms {held}"
        ));
    }

    // The budget, on the basis of every lock budget here (a tmpfs home, where an fsync
    // costs nothing): what the commit window itself does with a 500 MiB WTF/, which it
    // never hashes (stamp digests).
    let home = Home::new(&["wow"]);
    let wtf = Wtf::at(home.client("_retail_")).big(TOTAL, 5_000).build();
    let created = home.run(&["backup", "create", "-t", "retail"]).expect(0);
    let held = restore_lock_hold(&home, &made(&created), &wtf);
    measured(format_args!(
        "whole restore of 500 MiB in 5000 files on tmpfs: lock_held_ms {held}"
    ));
    assert!(held < 50, "lock held {held} ms");
}

/// A 4 MB BugGrabber file as a modern client writes it, median of 21 parses: under
/// 20 ms (M3 checkpoint criterion 6).
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn a_4_mb_buggrabber_file_parses_under_20_ms() {
    use common::buggrabber::{self, Writer};
    let sample = buggrabber::modern_int(Writer::Modern);
    let mut db = sample.clone();
    db.entries.clear();
    while db.render().len() < 4_000_000 {
        db.entries
            .extend(sample.entries.iter().cloned().cycle().take(500));
    }
    let bytes = db.render();
    let parsed = omaddons::lua::parse_saved_variables(&bytes, &["BugGrabberDB"]);
    assert!(parsed.complete && parsed.issues.is_empty());
    let (median, times) = median_of_21(|| {
        std::hint::black_box(omaddons::lua::parse_saved_variables(
            &bytes,
            &["BugGrabberDB"],
        ));
    });
    measured(format_args!(
        "savedvariables: {} bytes, {} entries: median {median:?}, max {:?}",
        bytes.len(),
        db.entries.len(),
        times[times.len() - 1]
    ));
    assert!(
        median < Duration::from_millis(20),
        "median {median:?} ({times:?})"
    );
}

/// The parser's resource workloads (M3-D17): tiny-token tables, one huge key, a flood
/// of malformed fragments, deep skipped globals, and one file read for each of 64
/// candidates.
const LUA_WORKLOADS: &[&str] = &[
    "tiny_tokens",
    "one_huge_key",
    "fragments",
    "deep_skipped",
    "sixty_four_candidates",
];

/// Each workload in a child process, so the harness's own memory does not count: what the
/// workload adds to the child's resident memory (its peak less what the child held before
/// the workload began, which is the test binary and its list of every test, and grows
/// with the suite) at most 3 × the input plus 16 MiB, and at most 1 s per 64 MiB parsed.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn the_savedvariables_parser_stays_within_its_memory_and_time_budgets() {
    const MIB: u64 = 1 << 20;
    let exe = std::env::current_exe().unwrap();
    for workload in LUA_WORKLOADS {
        let out = std::process::Command::new(&exe) // ubs:ignore — the test binary itself
            .args([
                "perf::lua_resource_child",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("OMADDONS_LUA_WORKLOAD", workload)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{workload}: {stderr}");
        let line = stderr
            .lines()
            .find_map(|l| l.strip_prefix("lua-resource "))
            .unwrap_or_else(|| unreachable!("{workload}: no measurement in {stderr}"));
        let field = |name: &str| -> u64 {
            line.split(' ')
                .find_map(|f| f.strip_prefix(&format!("{name}=")))
                .and_then(|v| v.parse().ok())
                .unwrap()
        };
        let (input, parsed, start, peak, ms) = (
            field("input"),
            field("parsed"),
            field("start"),
            field("peak"),
            field("ms"),
        );
        // What the workload added to the child: its peak less what the child held before
        // it began (the test binary and its harness, which grow with the suite).
        let added = peak.saturating_sub(start);
        measured(format_args!(
            "savedvariables {workload}: input {:.1} MiB ({:.0} MiB parsed), peak RSS {:.1} MiB \
             ({:.1} MiB at start, {:.1} MiB added), {ms} ms",
            input as f64 / MIB as f64,
            parsed as f64 / MIB as f64,
            peak as f64 / MIB as f64,
            start as f64 / MIB as f64,
            added as f64 / MIB as f64,
        ));
        assert!(
            added <= 3 * input + 16 * MIB,
            "{workload}: {added} bytes added (peak {peak}, start {start}) for {input}"
        );
        assert!(
            ms <= 1000 * parsed.div_ceil(64 * MIB),
            "{workload}: {ms} ms for {parsed} bytes"
        );
    }
}

/// One workload of the test above, named by `OMADDONS_LUA_WORKLOAD`; it does nothing
/// when run on its own.
#[test]
#[ignore = "a child of the_savedvariables_parser_stays_within_its_memory_and_time_budgets"]
fn lua_resource_child() {
    use omaddons::lua::{parse_saved_variables, read_bounded};
    let Ok(workload) = std::env::var("OMADDONS_LUA_WORKLOAD") else {
        return;
    };
    // This process before the workload: the test binary itself and the harness's list of
    // every test in it, which grows with the suite and is not the parser's.
    let start_rss = current_rss();
    let (input, parsed, ms) = if workload == "sixty_four_candidates" {
        // One BugGrabber file of about 1 MiB, read and parsed once per candidate: what
        // one read holds is dropped before the next. The database that wrote the file is
        // the test's, so it is gone before the first read.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("!BugGrabber.lua");
        let db = common::buggrabber::flood(common::buggrabber::Writer::Modern, 9_000);
        fs::write(&path, db.render()).unwrap();
        drop(db);
        let size = fs::metadata(&path).unwrap().len();
        let start = Instant::now();
        for _ in 0..64 {
            let bytes = read_bounded(&path).unwrap();
            let parsed = parse_saved_variables(&bytes, &["BugGrabberDB"]);
            assert!(parsed.complete);
            std::hint::black_box(parsed);
        }
        (size, 64 * size, start.elapsed())
    } else {
        let len = if workload == "one_huge_key" {
            32 << 20
        } else {
            64 << 20
        };
        let (bytes, wanted) = lua_workload(&workload, len);
        let start = Instant::now();
        let parsed = parse_saved_variables(&bytes, wanted);
        let ms = start.elapsed();
        assert!(parsed.issues.len() <= omaddons::limits::LUA_MAX_ISSUES);
        std::hint::black_box(parsed);
        (bytes.len() as u64, bytes.len() as u64, ms)
    };
    measured(format_args!(
        "lua-resource input={input} parsed={parsed} start={start_rss} peak={} ms={}",
        peak_rss(),
        ms.as_millis()
    ));
}

// ---- the approval checked under the commit lock (D8): its cost on the realistic home -------

/// One `-v` cost note of a checked window: `<target>: approval checked under the lock in
/// <us> us: folders <n>, entries <n>, rehashed <n>, files <n>, bytes <n>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CostNote {
    us: u64,
    folders: u64,
    entries: u64,
    rehashed: u64,
    files: u64,
    bytes: u64,
}

/// The note in `message`, if it is one.
fn cost_note(message: &str) -> Option<CostNote> {
    let (_, rest) = message.split_once(": approval checked under the lock in ")?;
    let (us, rest) = rest.split_once(" us: ")?;
    let mut counts = std::collections::BTreeMap::new();
    for part in rest.split(", ") {
        let (name, value) = part.split_once(' ')?;
        counts.insert(name, value.parse::<u64>().ok()?);
    }
    Some(CostNote {
        us: us.parse().ok()?,
        folders: *counts.get("folders")?,
        entries: *counts.get("entries")?,
        rehashed: *counts.get("rehashed")?,
        files: *counts.get("files")?,
        bytes: *counts.get("bytes")?,
    })
}

/// One commit window, from its run's `-v` stderr: how long it held the lock, its cost
/// notes, and how many `lock released` lines it logged.
struct Window {
    held_ms: u64,
    notes: Vec<CostNote>,
    released: usize,
}

/// `args` with `-v --json`, which must succeed, as one measured window.
fn window(log: &common::tlog::TestLog, home: &Home, args: &[String]) -> Window {
    let mut argv = args.to_vec();
    argv.extend(["-v", "--json"].map(str::to_owned));
    let ran = log.run(home, &argv);
    ran.ok();
    let messages: Vec<String> = ran
        .events()
        .iter()
        .filter(|e| e["event"] == "log")
        .filter_map(|e| e["message"].as_str().map(str::to_owned))
        .collect();
    let notes: Vec<CostNote> = messages.iter().filter_map(|m| cost_note(m)).collect();
    for note in &notes {
        log.json(
            "note",
            &serde_json::json!({
                "us": note.us, "folders": note.folders, "entries": note.entries,
                "rehashed": note.rehashed, "files": note.files, "bytes": note.bytes,
            }),
        );
    }
    Window {
        held_ms: lock_held_ms(&ran.to_run()).unwrap_or_default(),
        notes,
        released: messages
            .iter()
            .filter(|m| m.starts_with("lock released: "))
            .count(),
    }
}

/// Addon `n` on `target` updated to `tag` bound to its preview: the dry run's token, then
/// the bound run as a measured window.
fn bound_window(
    log: &common::tlog::TestLog,
    realistic: &common::realistic::Realistic,
    n: usize,
    tag: &str,
    target: &str,
) -> Window {
    let args = realistic.install_args(n, tag, target);
    let mut preview = args.clone();
    preview.extend(["--dry-run", "--json"].map(str::to_owned));
    let dry = log.run(&realistic.home, &preview);
    let token = dry.ok().data()["precondition"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let mut bound = args;
    bound.extend(["--if-match".to_owned(), token]);
    window(log, &realistic.home, &bound)
}

/// What makes a bound window's numbers trustworthy: one cost note for its two folders and
/// their 170 entries (84 files and the folder itself, each), and one `lock released`
/// line.
fn assert_bound_window(log: &common::tlog::TestLog, w: &Window) {
    log.assert_eq(
        "(cost notes, lock released lines, note folders, note entries)",
        &(
            w.notes.len(),
            w.released,
            w.notes.first().map(|n| n.folders),
            w.notes.first().map(|n| n.entries),
        ),
        &(1, 1, Some(2), Some(170)),
    );
}

/// The median of `values` (the upper one of an even count); 0 when empty.
fn median(values: &[u64]) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted.get(sorted.len() / 2).copied().unwrap_or_default()
}

/// The realistic home builds offline, and a run bound to its preview logs one cost note
/// for the two folders it replaces. Shape and counts only, never a time: the report is
/// `the_approval_check_under_the_lock_costs`.
#[test]
fn realistic_home_builds_and_one_bound_update_logs_its_cost() {
    let log = common::tlog::tlog!();
    log.step("build the realistic home");
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let realistic = common::realistic::realistic_home(&dir.path().join("home"));
    log.env(&realistic.home);
    log.note("30 records and 5,040 managed files, asserted by the builder");
    log.step("one bound update on retail");
    let w = bound_window(&log, &realistic, 0, "v2", "retail");
    assert_bound_window(&log, &w);
    log.step("an unbound update logs no note");
    let w = window(
        &log,
        &realistic.home,
        &realistic.install_args(1, "v2", "retail"),
    );
    log.assert_eq(
        "(cost notes, lock released lines)",
        &(w.notes.len(), w.released),
        &(0, 1),
    );
}

/// D8's cost on the realistic home (M4-D60: "D8 reports what it adds to `lock_held_ms`
/// on the realistic home"; risk #20): three rounds in which every addon on every client is
/// updated twice, once bound to its preview and once not, so each round's bound windows
/// check all 5,040 files. Prints, per client and in total, the check's time from the
/// notes and the bound and unbound windows' lock holds. A report, not a budget: it
/// asserts only what makes the numbers trustworthy.
#[test]
#[ignore = "a release-build report: cargo test --release --test integration perf:: -- --ignored"]
fn the_approval_check_under_the_lock_costs() {
    use common::realistic::{ADDONS, REALISTIC_TARGETS, realistic_home};
    const ROUNDS: usize = 3;
    let log = common::tlog::tlog!();
    log.step("build the realistic home");
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let realistic = realistic_home(&dir.path().join("home"));
    log.env(&realistic.home);

    // Per target: each round's summed check time, and every window's numbers.
    let mut report = Vec::new();
    let (mut total_us, mut total_entries) = (0, 0);
    for target in REALISTIC_TARGETS {
        let mut round_us = Vec::new();
        let (mut check_us, mut bound_ms, mut unbound_ms) = (Vec::new(), Vec::new(), Vec::new());
        let mut counts = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for round in 0..ROUNDS {
            log.step(&format!("round {round} {target}"));
            let (bound_tag, unbound_tag) =
                (format!("v{}", 2 * round + 2), format!("v{}", 2 * round + 3));
            let mut summed = [0u64; 5];
            for n in 0..ADDONS {
                let w = bound_window(&log, &realistic, n, &bound_tag, target);
                assert_bound_window(&log, &w);
                let note = w.notes[0];
                check_us.push(note.us);
                bound_ms.push(w.held_ms);
                for (sum, value) in summed.iter_mut().zip([
                    note.us,
                    note.entries,
                    note.rehashed,
                    note.files,
                    note.bytes,
                ]) {
                    *sum += value;
                }
                let args = realistic.install_args(n, &unbound_tag, target);
                let w = window(&log, &realistic.home, &args);
                log.assert_eq(
                    "unbound: (cost notes, lock released lines)",
                    &(w.notes.len(), w.released),
                    &(0, 1),
                );
                unbound_ms.push(w.held_ms);
            }
            round_us.push(summed[0]);
            for (list, value) in counts.iter_mut().zip(&summed[1..]) {
                list.push(*value);
            }
        }
        let (bound, unbound) = (median(&bound_ms), median(&unbound_ms));
        let target_us = median(&round_us);
        let entries = median(&counts[0]);
        total_us += target_us;
        total_entries += entries;
        let line = format!(
            "approval-check {target}: {ADDONS} bound windows check_us {target_us} (median of {ROUNDS} rounds), per window {}; lock_held_ms per window bound {bound} unbound {unbound} (+{}); entries {entries}, rehashed {}, files {}, bytes {}",
            median(&check_us),
            i128::from(bound) - i128::from(unbound),
            median(&counts[1]),
            median(&counts[2]),
            median(&counts[3]),
        );
        measured(format_args!("{line}"));
        report.push(line);
    }
    log.step("report");
    let total = format!(
        "approval-check total: check_us {total_us} for {total_entries} entries over {} clients ({} managed files)",
        REALISTIC_TARGETS.len(),
        common::realistic::MANAGED_FILES
    );
    measured(format_args!("{total}"));
    report.push(total);
    log.json("report", &report);
}

/// The daily pass over every client (bmcx.3) on the realistic home: an update on retail
/// with the pass due (its stamp deleted, so it walks classic's and era's snapshots)
/// against one the same day (not due), by each run's `meta.elapsed_ms`. The pass runs
/// after the commit window, so the lock hold is not part of it. A report, not a budget.
#[test]
#[ignore = "a release-build report: cargo test --release --test integration perf:: -- --ignored"]
fn the_daily_pass_over_every_client_costs() {
    use common::realistic::{ADDONS, realistic_home};
    const ROUNDS: usize = 5;
    let log = common::tlog::tlog!();
    log.step("build the realistic home");
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let realistic = realistic_home(&dir.path().join("home"));
    log.env(&realistic.home);
    let stamp = realistic.home.state().join("retention_sweep.stamp");
    let (mut due, mut quiet) = (Vec::new(), Vec::new());
    for round in 0..ROUNDS {
        for pass_due in [true, false] {
            log.step(&format!("round {round}, pass due: {pass_due}"));
            if pass_due {
                fs::remove_file(&stamp).unwrap();
            }
            let tag = format!("v{}", 2 * round + 2 + usize::from(!pass_due));
            let mut args = realistic.install_args(round % ADDONS, &tag, "retail");
            args.push("--json".into());
            let ran = log.run(&realistic.home, &args);
            ran.ok();
            log.assert_eq("stamped", &stamp.is_file(), &true);
            let ms = ran.envelope()["meta"]["elapsed_ms"].as_u64().unwrap();
            (if pass_due { &mut due } else { &mut quiet }).push(ms);
        }
    }
    let (due, quiet) = (median(&due), median(&quiet));
    let line = format!(
        "daily-pass: an update on retail elapsed_ms {due} with the pass over classic and era, {quiet} without (+{}), median of {ROUNDS}",
        i128::from(due) - i128::from(quiet)
    );
    measured(format_args!("{line}"));
    log.json("report", &line);
}

/// F6 (M4-D52): what re-hashing a cached archive before its use costs. A printed note,
/// not a budget (the plan calls it "a perf note"): the median of 21 verifications of a
/// 5 MiB blob, each on a fresh memo so every run hashes. The checkpoint quotes it beside
/// "under 10 ms per 5 MiB".
#[test]
#[ignore = "a release-build measurement: cargo test --release --test integration perf:: -- --ignored"]
fn blob_reverification_cost() {
    use omaddons::engine::blobs::{Verification, VerifyMemo};
    use sha2::{Digest, Sha256};
    let env = TestEnv::new();
    let dir = env.cache().join("omaddons/blobs");
    fs::create_dir_all(&dir).unwrap();
    // Pseudo-random bytes from a fixed seed (xorshift), so the run is repeatable.
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let bytes: Vec<u8> = (0..5 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect();
    let name: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let path = dir.join(&name);
    fs::write(&path, &bytes).unwrap();
    let now = Timestamp::now().as_nanosecond();
    let (median, _) = median_of_21(|| {
        let verdict = VerifyMemo::new().verify(&path, &name, &mut || false, now);
        assert!(
            matches!(verdict, Verification::Matches { .. }),
            "{verdict:?}"
        );
    });
    let ms = median.as_secs_f64() * 1000.0;
    measured(format_args!(
        "blob re-verification: {ms:.2} ms per 5 MiB (median of 21)"
    ));
}

/// Questie's `QuestieDB` keeps its quest database in its TOCs, 13 to 57 MB each
/// (omad-btel). With an addon whose 20 MB TOC is 200,000 `## X-Quest-…` lines, `list -v`,
/// `errors` and `status -v` each take under 30 ms more than with the same addon's TOC
/// without them, and none peaks over 64 MiB of resident memory.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn a_large_metadata_toc_costs_under_30_ms_and_64_mib() {
    const MIB: u64 = 1 << 20;
    let homes: Vec<(&str, Home)> = [("without", 0usize), ("with", 200_000)]
        .into_iter()
        .map(|(label, quests)| {
            let home = Home::new(&["wow"]);
            let src = home.env.root().join("src/Big");
            fs::create_dir_all(&src).unwrap();
            let mut toc =
                b"## Interface: 120105\n## Title: Big\n## SavedVariables: BigDB\n".to_vec();
            let payload = "QUJD".repeat(25);
            for n in 0..quests {
                toc.extend_from_slice(format!("## X-Quest-{n}-1: {payload}\n").as_bytes());
            }
            toc.extend_from_slice(b"## Version: 1.0\nBig.lua\n");
            fs::write(src.join("Big.toc"), &toc).unwrap();
            fs::write(src.join("Big.lua"), "-- big\n").unwrap();
            let src = src.to_str().unwrap();
            home.run(&["install", src, "-t", "retail", "--json"])
                .expect(0);
            (label, home)
        })
        .collect();
    for args in [
        &["list", "-t", "retail", "-v", "--json"][..],
        &["errors", "-t", "retail", "--json"][..],
        &["status", "-v", "--json"][..],
    ] {
        let mut medians = Vec::new();
        for (label, home) in &homes {
            let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            home.run(args).expect(0);
            let (median, _) = median_of_21(|| {
                assert!(
                    home.std_command(&argv, &[])
                        .output()
                        .unwrap()
                        .status
                        .success()
                );
            });
            let mut child = home
                .std_command(&argv, &[])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let (peak, exit) = sampled_peak_rss(&mut child);
            assert!(exit.success(), "{args:?} {label}");
            measured(format_args!(
                "{} {label} the large TOC: median {median:?}, peak RSS {} KiB",
                args.join(" "),
                peak / 1024
            ));
            assert!(peak < 64 * MIB, "{args:?} {label}: peak {peak} bytes");
            medians.push(median);
        }
        let extra = medians[1].saturating_sub(medians[0]);
        assert!(
            extra < Duration::from_millis(30),
            "{args:?}: {extra:?} more with the large TOC ({medians:?})"
        );
    }
}

// ---------------------------------------------------------------------------------------
// The errors ledger (design 7.9): its core and its store, measured before the light read
// runs around every change. `argc bench` runs these (`perf::ledger_`), and the finished
// slice is compared with their medians. Each line they print starts with `ledger`.

#[cfg(test)]
mod ledger_shapes {
    use std::fs;
    use std::ops::Range;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use jiff::Timestamp;
    use jiff::tz::TimeZone;
    use omaddons::errors::buggrabber::{TimeEvidence, TimeMeaning, TzEvidence, evidence};
    use omaddons::errors::ledger::{
        ChangeMark, ClientNow, DbRead, DbSighting, EntrySighting, SightMode, TargetLedger, load,
        open, sight,
    };
    use omaddons::errors::observe::{Coverage, read_stable};
    use omaddons::errors::sanitize::identities;
    use omaddons::fsx::{FileStamp, ScopeTable};
    use omaddons::limits::LEDGER_PRECHANGE_MAX_BYTES;
    use omaddons::model::RunId;
    use omaddons::state::paths::Paths;

    use crate::common::buggrabber::{BugDb, BugEntry, NOW, TimeKind, Writer, stack_era_c};

    pub const TARGET: &str = "k";

    pub fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(NOW + seconds).unwrap()
    }

    /// A client: its accounts (one BugGrabber database each), the errors each holds now,
    /// each its own family, and the families the ledger remembers that no database holds
    /// any more.
    #[derive(Debug, Clone, Copy)]
    pub struct Shape {
        pub accounts: usize,
        pub entries: usize,
        pub absent: usize,
    }

    impl Shape {
        /// The families database `db` holds now.
        pub fn present(&self, db: usize) -> Range<usize> {
            db * self.entries..(db + 1) * self.entries
        }

        /// The families gone from every database (database 0 held them once).
        pub fn gone(&self) -> Range<usize> {
            let start = self.accounts * self.entries;
            start..start + self.absent
        }

        pub fn families(&self) -> usize {
            self.accounts * self.entries + self.absent
        }
    }

    pub fn db_key(db: usize) -> String {
        format!("d:{db:04}")
    }

    /// Database `db` in state `stamp`, holding `families`, as a proved read gives them to
    /// `sight`: keys only, nothing parsed.
    pub fn sighting(
        db: usize,
        stamp: &str,
        observed: i64,
        session: u32,
        families: impl Iterator<Item = usize>,
    ) -> DbSighting {
        let entries = families
            .map(|i| EntrySighting {
                family: format!("a:{i:016x}"),
                fingerprint: format!("e:{i:016x}"),
                session: Some(session),
                time: TimeEvidence {
                    at: Some(at(observed - 60)),
                    meaning: TimeMeaning::Last,
                    tz: TzEvidence::Explicit,
                },
                counter: 1,
            })
            .collect();
        DbSighting {
            key: db_key(db),
            provider: "!BugGrabber".to_owned(),
            stamp: stamp.to_owned(),
            written_at: Some(at(observed - 1)),
            observed_at: at(observed),
            session: Some(session),
            read: Some(DbRead {
                coverage: Coverage::Proved,
                entries,
            }),
        }
    }

    pub fn client(now: Timestamp) -> ClientNow {
        ClientNow {
            version: Some("12.1.5.70391".to_owned()),
            now,
        }
    }

    /// A ledger of `shape`, as play builds one: every database read holding its families
    /// (database 0 also those to be gone), then read again without the gone ones. Each
    /// database's newest state is `s2:<db>`, looked at at 200.
    pub fn ledger(shape: &Shape) -> TargetLedger {
        let mut ledger = TargetLedger::default();
        let first: Vec<DbSighting> = (0..shape.accounts)
            .map(|db| {
                let gone = if db == 0 { shape.gone() } else { 0..0 };
                sighting(
                    db,
                    &format!("s1:{db}"),
                    100,
                    1,
                    shape.present(db).chain(gone),
                )
            })
            .collect();
        sight(
            &mut ledger,
            &first,
            &[],
            &client(at(100)),
            true,
            SightMode::Full,
        );
        let second: Vec<DbSighting> = (0..shape.accounts)
            .map(|db| sighting(db, &format!("s2:{db}"), 200, 2, shape.present(db)))
            .collect();
        sight(
            &mut ledger,
            &second,
            &[],
            &client(at(200)),
            true,
            SightMode::Full,
        );
        ledger
    }

    /// A BugGrabber file holding errors `families`, each in a file of its own (so each is
    /// its own family), saved in session `session`. Each error carries what a modern
    /// client writes: a six-frame stack and a few locals.
    pub fn bugfile(families: impl Iterator<Item = usize>, session: u32) -> Vec<u8> {
        let mut db = BugDb::new(Writer::Modern, session);
        for i in families {
            let line = u32::try_from(i % 997).unwrap() + 1;
            let frames: Vec<String> = (0..6u32)
                .map(|f| {
                    format!(
                        "{} in function 'Handler{f}'",
                        stack_era_c("Gen", &format!("f{i}.lua"), line + f)
                    )
                })
                .collect();
            db.entries.push(
                BugEntry::new(
                    &format!(
                        "Interface/AddOns/Gen/f{i}.lua:{line}: attempt to index a nil value \
                         (field 'x')"
                    ),
                    session,
                    (TimeKind::Unix, NOW - 60),
                )
                .stack(&frames)
                .locals(
                    "self = <table> {\n  frame = <table> {}\n  count = 3\n}\nevent = \"UNIT_AURA\"",
                ),
            );
        }
        db.render()
    }

    /// What the light read's capture makes of one file the ledger has not seen in this
    /// state: a stable, bounded read, and each entry's two identities.
    pub fn capture(key: &str, path: &Path, budget: &mut u64, now: Timestamp) -> DbSighting {
        let (_, coverage, db, stamp) = read_stable(path, budget, Timestamp::now().as_nanosecond());
        let entries = db
            .entries
            .iter()
            .map(|e| {
                let (fingerprint, family) = identities(&e.message);
                EntrySighting {
                    family,
                    fingerprint,
                    session: e.session,
                    time: evidence(e.time.as_ref(), &TimeZone::UTC),
                    counter: e.counter,
                }
            })
            .collect();
        let written_at = fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| Timestamp::try_from(t).ok());
        DbSighting {
            key: key.to_owned(),
            provider: "!BugGrabber".to_owned(),
            stamp: stamp.map_or_else(String::new, |s| s.fingerprint()),
            written_at,
            observed_at: now,
            session: db.session,
            read: Some(DbRead { coverage, entries }),
        }
    }

    /// A client of `shape` on disk: one BugGrabber file per database, and the ledger as
    /// play left it, every file recorded as it is now.
    pub struct OnDisk {
        pub _dir: tempfile::TempDir,
        pub paths: Paths,
        pub scopes: ScopeTable,
        pub files: Vec<(String, PathBuf)>,
        pub shape: Shape,
    }

    impl OnDisk {
        pub fn new(shape: Shape) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let paths = Paths::with_roots(
                root.join("home"),
                root.join("config"),
                root.join("state"),
                root.join("cache"),
            );
            let scopes = ScopeTable::new(&paths, &[], None).unwrap();
            let files = (0..shape.accounts)
                .map(|db| {
                    let path = root
                        .join("wtf")
                        .join(format!("account{db}"))
                        .join("!BugGrabber.lua");
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    (db_key(db), path)
                })
                .collect();
            let disk = Self {
                _dir: dir,
                paths,
                scopes,
                files,
                shape,
            };
            // First every file with the gone families in database 0's, then without.
            disk.write(1, true);
            disk.record_all();
            disk.write(2, false);
            disk.record_all();
            disk
        }

        /// Every file saved again in session `session` (the game wrote them all).
        pub fn write(&self, session: u32, with_gone: bool) {
            for (db, (_, path)) in self.files.iter().enumerate() {
                let gone = if db == 0 && with_gone {
                    self.shape.gone()
                } else {
                    0..0
                };
                fs::write(path, bugfile(self.shape.present(db).chain(gone), session)).unwrap();
            }
            // Past the kernel's timestamp slack, so the next read is stable.
            std::thread::sleep(Duration::from_millis(30));
        }

        fn record_all(&self) {
            let now = Timestamp::now();
            let mut budget = u64::MAX;
            let dbs: Vec<DbSighting> = self
                .files
                .iter()
                .map(|(key, path)| capture(key, path, &mut budget, now))
                .collect();
            let mut session = open(&self.paths, &self.scopes, "seed").unwrap();
            session.sight(TARGET, &dbs, &[], &client(now), true, SightMode::Full);
            session.commit().unwrap();
        }

        pub fn ledger_bytes(&self) -> u64 {
            fs::metadata(self.paths.errors_ledger()).map_or(0, |m| m.len())
        }

        /// One light read around a change, both halves, as design 7.1.8 runs it, and the
        /// bytes its one session wrote. The capture, before the change: a stamp of each
        /// database, an unlocked load of the ledger, and a bounded parse of each file
        /// whose stamp is neither its newest proved read's nor its unproved one. The
        /// commit, after the change: a second stamp of each, then one session that sights
        /// what the capture saw (`Light`), adds a change mark for each database whose
        /// state is now its newest proved read, and stores the ledger. Every change that
        /// reads leaves marks, so every light read writes the ledger once.
        pub fn light_read(&self) -> u64 {
            let now = Timestamp::now();
            let run = RunId::new();
            let stamp = |path: &Path| FileStamp::read(path).unwrap().map(|s| s.fingerprint());
            let stamps: Vec<Option<String>> =
                self.files.iter().map(|(_, path)| stamp(path)).collect();
            let known = load(&self.paths).unwrap_or_default();
            let target = known.targets.get(TARGET);
            let mut budget = LEDGER_PRECHANGE_MAX_BYTES;
            let dbs: Vec<DbSighting> = self
                .files
                .iter()
                .zip(&stamps)
                .map(|((key, path), stamp)| {
                    let db = target.and_then(|t| t.databases.get(key));
                    let newest = db.and_then(|db| db.reads.last()).map(|r| &r.stamp);
                    let unproved = db.and_then(|db| db.unproved_stamp.as_ref());
                    match stamp {
                        Some(stamp) if newest == Some(stamp) || unproved == Some(stamp) => {
                            DbSighting {
                                key: key.clone(),
                                provider: "!BugGrabber".to_owned(),
                                stamp: stamp.clone(),
                                written_at: None,
                                observed_at: now,
                                session: None,
                                read: None,
                            }
                        }
                        _ => capture(key, path, &mut budget, now),
                    }
                })
                .collect();
            // The change runs here.
            let settled: Vec<bool> = self
                .files
                .iter()
                .zip(&stamps)
                .map(|((_, path), before)| stamp(path) == *before)
                .collect();
            let before = fs::metadata(self.paths.errors_ledger())
                .map(|m| m.ino())
                .ok();
            let mut session = open(&self.paths, &self.scopes, "light").unwrap();
            session.sight(TARGET, &dbs, &[], &client(now), true, SightMode::Light);
            for (s, settled) in dbs.iter().zip(settled) {
                let Some(db) = session
                    .ledger
                    .targets
                    .get(TARGET)
                    .and_then(|t| t.databases.get(&s.key))
                else {
                    continue;
                };
                let Some(newest) = db.reads.last().filter(|r| r.stamp == s.stamp) else {
                    continue;
                };
                let mark = ChangeMark {
                    run,
                    at: now,
                    generation: db.generation.clone(),
                    stamp: s.stamp.clone(),
                    session: newest.session,
                    running: Some(false),
                    settled,
                };
                assert!(
                    session.add_mark(TARGET, &s.key, mark),
                    "{} has no mark",
                    s.key
                );
            }
            session.commit().unwrap();
            let after = fs::metadata(self.paths.errors_ledger()).ok();
            match after {
                Some(m) if Some(m.ino()) != before => m.len(),
                _ => 0,
            }
        }
    }
}

/// `sight` over one 5,000-entry database at a new state and a 5,000-family ledger: under
/// 10 ms, median of 21.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn ledger_sight_over_5000_entries_and_families_under_10_ms() {
    use ledger_shapes::{Shape, at, client, ledger, sighting};
    use omaddons::errors::ledger::{SightMode, sight};
    let shape = Shape {
        accounts: 1,
        entries: 5_000,
        absent: 0,
    };
    let base = ledger(&shape);
    // A new state holding every family, and one new error.
    let next = [sighting(
        0,
        "s3:0",
        300,
        3,
        shape.present(0).chain(5_000..5_001),
    )];
    let mut copies: Vec<_> = (0..21).map(|_| base.clone()).collect();
    let start = reset_peak_rss();
    let (median, times) = median_of_21(|| {
        let mut ledger = copies.pop().unwrap();
        let sighted = sight(
            &mut ledger,
            &next,
            &[],
            &client(at(300)),
            true,
            SightMode::Full,
        );
        assert_eq!(sighted.appeared.len(), 1);
    });
    measured(format_args!(
        "ledger sight: 5000 entries, 5000 families: median {median:?}, max {:?}, 0 bytes \
         written, peak RSS added {:.1} MiB",
        times[times.len() - 1],
        mib(peak_rss().saturating_sub(start))
    ));
    assert!(
        median < Duration::from_millis(10),
        "median {median:?} ({times:?})"
    );
}

/// The light read around a change, both halves together (its capture and its commit,
/// with a change mark per database), over unchanged databases: under 2 ms with 100
/// families, under 20 ms with 5,000, median of 21.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn ledger_light_read_of_unchanged_databases_stays_within_its_budgets() {
    use ledger_shapes::{OnDisk, Shape};
    for (families, budget) in [(100, 2), (5_000, 20)] {
        let disk = OnDisk::new(Shape {
            accounts: 1,
            entries: families,
            absent: 0,
        });
        let mut written = 0;
        let start = reset_peak_rss();
        let (median, times) = median_of_21(|| written = disk.light_read());
        measured(format_args!(
            "ledger light read, unchanged, {families} families: median {median:?}, max {:?}, \
             {written} bytes written, ledger {} bytes, peak RSS added {:.1} MiB",
            times[times.len() - 1],
            disk.ledger_bytes(),
            mib(peak_rss().saturating_sub(start))
        ));
        assert!(
            median < Duration::from_millis(budget),
            "{families} families: median {median:?} ({times:?})"
        );
    }
}

/// A typical multi-account client: 8 databases of 500 errors (BugGrabber's own cap), half
/// the families gone, every error its own family (the most families that shape holds).
/// The light read, both halves, under 10 ms, unchanged, median of 21; the ledger under
/// 1 MiB. The light read after a play session, when the game wrote every file, is
/// measured too (each file parsed; bounded by the capture's own case).
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn ledger_light_read_of_a_multi_account_client_stays_within_its_budgets() {
    use ledger_shapes::{OnDisk, Shape};
    let disk = OnDisk::new(Shape {
        accounts: 8,
        entries: 500,
        absent: 4_000,
    });
    let mut written = 0;
    let start = reset_peak_rss();
    let (median, times) = median_of_21(|| written = disk.light_read());
    let ledger = disk.ledger_bytes();
    measured(format_args!(
        "ledger light read, 8 accounts of 500, 4000 gone, unchanged: median {median:?}, max \
         {:?}, {written} bytes written, ledger {ledger} bytes, peak RSS added {:.1} MiB",
        times[times.len() - 1],
        mib(peak_rss().saturating_sub(start))
    ));
    let start = reset_peak_rss();
    let mut changed: Vec<Duration> = (0..21u32)
        .map(|run| {
            disk.write(3 + run, false);
            let start = Instant::now();
            written = disk.light_read();
            start.elapsed()
        })
        .collect();
    changed.sort();
    measured(format_args!(
        "ledger light read, 8 accounts of 500, 4000 gone, every file written since: median \
         {:?}, max {:?}, {written} bytes written, peak RSS added {:.1} MiB",
        changed[10],
        changed[20],
        mib(peak_rss().saturating_sub(start))
    ));
    assert!(
        median < Duration::from_millis(10),
        "median {median:?} ({times:?})"
    );
    assert!(ledger < 1 << 20, "the ledger is {ledger} bytes");
}

/// The ledger at `LEDGER_FAMILIES_MAX`, half its families present in 8 databases and half
/// gone: a session's load, `sight` of every database at a new state, encode and write,
/// under 100 ms, median of 21, in a child process whose added peak resident memory stays
/// under 64 MiB. The bytes written are recorded.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn ledger_at_its_family_cap_stays_within_its_budgets() {
    use omaddons::limits::LEDGER_FAMILIES_MAX;
    const MIB: u64 = 1 << 20;
    let dir = tempfile::tempdir().unwrap();
    let paths = ledger_cap::paths(dir.path());
    let shape = ledger_cap::shape();
    assert_eq!(shape.families(), LEDGER_FAMILIES_MAX);
    {
        let scopes = omaddons::fsx::ScopeTable::new(&paths, &[], None).unwrap();
        let mut session = omaddons::errors::ledger::open(&paths, &scopes, "seed").unwrap();
        session.ledger.targets.insert(
            ledger_shapes::TARGET.to_owned(),
            ledger_shapes::ledger(&shape),
        );
        session.commit().unwrap();
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap()) // ubs:ignore — the test binary itself
        .args([
            "perf::ledger_cap_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("OMADDONS_LEDGER_CAP_ROOT", dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let (_, exit) = sampled_peak_rss(&mut child);
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(exit.success(), "{stderr}");
    let line = stderr
        .lines()
        .find_map(|l| l.split("ledger-cap ").nth(1))
        .unwrap_or_else(|| unreachable!("no measurement in {stderr}"));
    let field = |name: &str| -> u64 {
        line.split(' ')
            .find_map(|f| f.strip_prefix(&format!("{name}=")))
            .and_then(|v| v.parse().ok())
            .unwrap()
    };
    let (start, peak, median_us, max_us, bytes) = (
        field("start"),
        field("peak"),
        field("median_us"),
        field("max_us"),
        field("bytes"),
    );
    let added = peak.saturating_sub(start);
    measured(format_args!(
        "ledger at its cap: {} families: load, sight, encode, write: median {:.1} ms, max {:.1} \
         ms, {bytes} bytes written, peak RSS added {:.1} MiB",
        shape.families(),
        median_us as f64 / 1000.0,
        max_us as f64 / 1000.0,
        added as f64 / MIB as f64
    ));
    assert!(median_us < 100_000, "median {median_us} µs");
    assert!(
        added < 64 * MIB,
        "{added} bytes added (peak {peak}, start {start})"
    );
}

#[cfg(test)]
mod ledger_cap {
    use std::path::Path;

    use omaddons::state::paths::Paths;

    use super::ledger_shapes::Shape;

    pub fn paths(root: &Path) -> Paths {
        Paths::with_roots(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("cache"),
        )
    }

    /// `LEDGER_FAMILIES_MAX` families: 8 databases of 1,024 present, and as many gone.
    pub fn shape() -> Shape {
        Shape {
            accounts: 8,
            entries: 1_024,
            absent: 8 * 1_024,
        }
    }
}

/// The workload of the test above, on the ledger it stored; it does nothing when run on
/// its own.
#[test]
#[ignore = "a child of ledger_at_its_family_cap_stays_within_its_budgets"]
fn ledger_cap_child() {
    use ledger_shapes::{TARGET, at, client, sighting};
    use omaddons::errors::ledger::{SightMode, open};
    let Some(root) = std::env::var_os("OMADDONS_LEDGER_CAP_ROOT") else {
        return;
    };
    let paths = ledger_cap::paths(std::path::Path::new(&root));
    let scopes = omaddons::fsx::ScopeTable::new(&paths, &[], None).unwrap();
    let shape = ledger_cap::shape();
    let start_rss = current_rss();
    let mut times: Vec<Duration> = Vec::new();
    let mut bytes = 0;
    for run in 0..21u32 {
        let observed = 300 + i64::from(run) * 100;
        let dbs: Vec<_> = (0..shape.accounts)
            .map(|db| {
                sighting(
                    db,
                    &format!("s{}:{db}", run + 3),
                    observed,
                    run + 3,
                    shape.present(db),
                )
            })
            .collect();
        let started = Instant::now();
        let mut session = open(&paths, &scopes, "cap").unwrap();
        session.sight(
            TARGET,
            &dbs,
            &[],
            &client(at(observed)),
            true,
            SightMode::Full,
        );
        session.commit().unwrap();
        times.push(started.elapsed());
        bytes = std::fs::metadata(paths.errors_ledger()).unwrap().len();
    }
    times.sort();
    measured(format_args!(
        "ledger-cap start={start_rss} peak={} median_us={} max_us={} bytes={bytes}",
        peak_rss(),
        times[10].as_micros(),
        times[20].as_micros()
    ));
}

/// The capture's parse of changed files filling `LEDGER_PRECHANGE_MAX_BYTES`: databases
/// at the decoder's entry cap, and a last one with what room is left, each read whole and
/// stable, each entry given its two identities, under 100 ms, median of 21.
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn ledger_capture_of_changed_files_up_to_its_cap_under_100_ms() {
    use ledger_shapes::{bugfile, capture};
    use omaddons::limits::{BUGGRABBER_MAX_ENTRIES, LEDGER_PRECHANGE_MAX_BYTES};
    let dir = tempfile::tempdir().unwrap();
    let full = BUGGRABBER_MAX_ENTRIES;
    let per_entry = bugfile(0..full, 1).len() as u64 / full as u64;
    // Full databases while one fits, then one as large as the room left allows.
    let mut first = 0;
    let mut used = 0u64;
    let mut files = Vec::new();
    loop {
        let left = LEDGER_PRECHANGE_MAX_BYTES - used;
        let mut entries = usize::try_from(left / per_entry).unwrap().min(full);
        let mut bytes = bugfile(first..first + entries, 1);
        while entries > 0 && bytes.len() as u64 > left {
            entries -= entries / 100 + 1;
            bytes = bugfile(first..first + entries, 1);
        }
        if entries == 0 {
            break;
        }
        let path = dir.path().join(format!("account{}.lua", files.len()));
        std::fs::write(&path, &bytes).unwrap();
        files.push(path);
        used += bytes.len() as u64;
        first += entries;
    }
    let entries = first;
    let count = files.len();
    std::thread::sleep(Duration::from_millis(30));
    let total: u64 = files
        .iter()
        .map(|p| std::fs::metadata(p).unwrap().len())
        .sum();
    assert!(total <= LEDGER_PRECHANGE_MAX_BYTES);
    let mut read = 0;
    let start = reset_peak_rss();
    let (median, times) = median_of_21(|| {
        let mut budget = LEDGER_PRECHANGE_MAX_BYTES;
        let now = jiff::Timestamp::now();
        read = files
            .iter()
            .map(|p| {
                capture("d", p, &mut budget, now)
                    .read
                    .map_or(0, |r| r.entries.len())
            })
            .sum::<usize>();
    });
    measured(format_args!(
        "ledger capture: {count} files, {total} bytes, {read} entries: median {median:?}, max \
         {:?}, 0 bytes written, peak RSS added {:.1} MiB",
        times[times.len() - 1],
        mib(peak_rss().saturating_sub(start))
    ));
    assert_eq!(read, entries, "every entry was read");
    assert!(
        median < Duration::from_millis(100),
        "median {median:?} ({times:?})"
    );
}

/// `history::read` of 10,000 lines, the judging path: recorded (an index is filed only
/// past 150 ms).
#[test]
#[ignore = "a release-build budget: cargo test --release --test integration perf:: -- --ignored"]
fn ledger_history_read_of_10000_lines_is_recorded() {
    use omaddons::state::history::{self, HistoryFilter};
    let dir = tempfile::tempdir().unwrap();
    let paths = ledger_cap::paths(dir.path());
    let key: omaddons::model::TargetKey = "01M51EX04HHWM46XE9F2B9JXED".parse().unwrap();
    let mut text = String::new();
    for _ in 0..10_000 {
        text.push_str(&serde_json::to_string(&history_line(key, RunId::new())).unwrap());
        text.push('\n');
    }
    std::fs::create_dir_all(paths.history_file().parent().unwrap()).unwrap();
    std::fs::write(paths.history_file(), &text).unwrap();
    let filter = HistoryFilter {
        target: Some(key),
        ..HistoryFilter::default()
    };
    let mut entries = 0;
    let start = reset_peak_rss();
    let (median, times) = median_of_21(|| {
        entries = history::read(&paths, &filter).unwrap().entries.len();
    });
    measured(format_args!(
        "ledger history read: 10000 lines, {} bytes: median {median:?}, max {:?}, 0 bytes \
         written, peak RSS added {:.1} MiB",
        text.len(),
        times[times.len() - 1],
        mib(peak_rss().saturating_sub(start))
    ));
    assert_eq!(entries, 10_000);
}
