//! Time budgets, measured on release builds only:
//! `cargo test --release --test perf -- --ignored --skip bench_home --skip live_home`
//! (the milestone checkpoint runs them and quotes the printed numbers). A debug build is
//! several times slower and proves nothing, so each test is ignored. Every speed budget
//! lives here. Elsewhere, time appears only as a correctness bound with a wide margin: a
//! wait that must last at least its timeout, a sanity ceiling of a second or more, or a
//! killed child that must not run its course. Never as a speed target.
//!
//! Also here: `argc bench`'s home generator (ignored, and a no-op without `BENCH_HOME`),
//! and the request-economy assertions the other suites do not make, which run in the
//! normal gate because they count `net.log` lines, never time.

mod common;

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
use jiff::Timestamp;
use omaddons::drift::DriftMode;
use omaddons::model::{Channel, FolderName, Ref};
use omaddons::sources::wowi::index::{IndexPolicy, StoredIndex, WowiEntry, WowiIndex};
use omaddons::sources::{SourceConfigView, SourceRegistry, Wanted, fetch_once};
use omaddons::state::memo::{self, MemoEntry, ReleaseMemo};
use support::{addon_release, drift_report, measured, median_of_21};

#[cfg(test)]
mod support {
    use omaddons::discover::DiscoverySource;
    use omaddons::discover::battlenet::{BattlenetHints, discover_root};
    use omaddons::drift::{DriftMode, DriftReport, check};
    use omaddons::rules::RulesTable;

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
#[ignore = "argc bench's home (a debug build, which replays fixtures): BENCH_HOME=<dir> cargo test --test perf bench_home -- --ignored"]
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
#[ignore = "the live run's home: LIVE_HOME=<dir> cargo test --test perf live_home -- --ignored"]
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

/// The budget: 5,000 files in 20 folders, installed through the engine, checked by Stat
/// right after in under 30 ms warm, with at most 1% re-hashed. (Moved from `drift.rs`.)
#[test]
#[ignore = "a release-build budget: cargo test --release --test perf -- --ignored"]
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
#[ignore = "a release-build budget: cargo test --release --test perf -- --ignored"]
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

/// `status --cached` (the widget's view) with 100 records and a memo of 200 entries
/// copied from recorded release metadata (DBM, BigWigs and WeakAuras), median of 21
/// runs of the binary: under 5 ms.
#[test]
#[ignore = "a release-build budget: cargo test --release --test perf -- --ignored"]
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

    // Retail, registered by a first install, then 100 records.
    let home = Home::new(&["wow"]);
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

    // Warm, then measure.
    let check = home.run(&["status", "--cached"]).expect(0);
    let updates = &check["data"]["targets"][0]["updates"];
    assert!(updates["checked_at"].is_string(), "{updates}");
    assert_eq!(
        check["data"]["targets"][0]["drift"],
        serde_json::json!([]),
        "every managed folder is present"
    );
    let mut times: Vec<Duration> = (0..21)
        .map(|_| {
            let start = Instant::now();
            home.run(&["status", "--cached"]).expect(0);
            start.elapsed()
        })
        .collect();
    times.sort();
    let median = times[times.len() / 2];
    measured(format_args!(
        "status --cached: median {median:?}, memo {memo_bytes} bytes, 100 records"
    ));
    assert!(
        median < Duration::from_millis(5),
        "status --cached median {median:?} (memo {memo_bytes} bytes; runs {times:?})"
    );
}

/// Every commit holds the global lock under 50 ms (vision §7 M1): 20 updates of one
/// addon, then three addons installed in one transaction, removed together, and that
/// remove undone. Local archives only: release builds replay no fixture.
#[test]
#[ignore = "a release-build budget: cargo test --release --test perf -- --ignored"]
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

/// 16 MiB of SavedVariables and a 64 MiB folder still hold the lock under 50 ms: what
/// can be copied or hashed is done before the lock. A release build ignores failpoints,
/// so SavedVariables are copied the way this filesystem allows (reflinks where it can).
#[test]
#[ignore = "a release-build budget: cargo test --release --test perf -- --ignored"]
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
#[ignore = "a release-build budget: cargo test --release --test perf -- --ignored"]
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
#[ignore = "a release-build budget: cargo test --release --test perf -- --ignored"]
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
#[ignore = "a release-build budget: cargo test --release --test perf -- --ignored"]
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
#[ignore = "a release-build budget: cargo test --release --test perf -- --ignored"]
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
#[ignore = "a release-build budget on btrfs: cargo test --release --test perf -- --ignored"]
fn clone_tree_500_mb() {
    use common::wtf::Wtf;
    use omaddons::backup::tree::{CloneOpts, clone_tree};
    use omaddons::fsx::{ClonePolicy, ScopeTable, WriteSession, Writer};
    use omaddons::output::ProgressSink;
    use omaddons::state::paths::Paths;

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
