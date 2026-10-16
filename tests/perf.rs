//! Time budgets, measured on release builds only:
//! `cargo test --release --test perf -- --ignored` (H3 runs them at the checkpoint).
//! A debug build is several times slower and proves nothing, so each test is ignored.
//! Every budget lives here, and nothing timing-based anywhere else.
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
use support::{addon_release, drift_report};

#[cfg(test)]
mod support {
    use omaddons::discover::DiscoverySource;
    use omaddons::discover::battlenet::{BattlenetHints, discover_root};
    use omaddons::drift::{DriftMode, DriftReport, check};
    use omaddons::rules::RulesTable;

    use super::common::home::Home;
    use super::common::http::{Asset, gh_latest, gh_release_v};
    use super::common::state::{files_index, record};

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
    eprintln!(
        "drift Stat over 5,000 files: {elapsed:?}, {} re-hashed",
        r.files_rehashed
    );
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
    eprintln!("WoWInterface index: {bytes} bytes, median load {median:?}");
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
    eprintln!("status --cached: median {median:?}, memo {memo_bytes} bytes, 100 records");
    assert!(
        median < Duration::from_millis(5),
        "status --cached median {median:?} (memo {memo_bytes} bytes; runs {times:?})"
    );
}
