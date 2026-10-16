//! Time budgets, measured on release builds only:
//! `cargo test --release --test perf -- --ignored` (H3 runs them at the checkpoint).
//! A debug build is several times slower and proves nothing, so each test is ignored.

mod common;

use std::fs;
use std::time::{Duration, Instant};

use common::addons::toc;
use common::archives::zip_text;
use common::env::TestEnv;
use common::home::Home;
use common::http::{paths, replay, scenario};
use jiff::Timestamp;
use omaddons::model::{Channel, FolderName, Ref};
use omaddons::sources::{SourceConfigView, SourceRegistry, Wanted, fetch_once};
use omaddons::state::memo::{self, MemoEntry, ReleaseMemo};

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
