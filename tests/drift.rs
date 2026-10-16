//! The drift detector end to end: addons installed through the binary (replayed GitHub
//! releases), then changed on disk with the drift mutators, then checked in-process
//! against the records and files index the commits wrote.

mod common;

use std::fs;

use common::drift::{add_stray_file, delete_managed, edit_managed, touch_identical};
use common::home::Home;
use common::netlog::net_log;
use common::trees::tree_hash;
use omaddons::drift::{DriftMode, DriftState};
use support::{foo_release, report};

#[cfg(test)]
mod support {
    use omaddons::discover::DiscoverySource;
    use omaddons::discover::battlenet::{BattlenetHints, discover_root};
    use omaddons::drift::{DriftMode, DriftReport, check};
    use omaddons::model::Target;
    use omaddons::rules::RulesTable;

    use super::common::home::Home;
    use super::common::http::{Asset, gh_release};
    use super::common::state::{files_index, record};

    /// gh:o/foo `tag`: folders `Foo` and `FooOptions`, each with `files` more files.
    pub fn foo_release(home: &Home, tag: &str, files: usize) {
        let toc = format!("## Interface: 120105\n## Version: {tag}\n");
        let mut entries: Vec<(String, String)> = vec![
            ("Foo/Foo.toc".into(), toc.clone()),
            ("FooOptions/FooOptions.toc".into(), toc),
        ];
        for i in 0..files {
            entries.push((format!("Foo/libs/f{i:04}.lua"), format!("-- {tag} {i}\n")));
        }
        let borrowed: Vec<(&str, &str)> = entries
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let assets: [Asset<'_>; 1] = [("Foo.zip", &[("mainline", 120_105)], borrowed)];
        gh_release(&home.http, "https://github.com/o/foo", tag, &assets);
    }

    /// The home's retail client, as discovery finds it.
    pub fn retail(home: &Home) -> Target {
        let (targets, _) = discover_root(
            &home.root,
            &RulesTable::builtin(),
            &BattlenetHints::default(),
            "battlenet",
            &DiscoverySource::Scan,
        );
        let found = targets
            .into_iter()
            .find(|t| t.target.flavor_dir.as_deref() == Some("_retail_"));
        assert!(found.is_some(), "no retail target");
        found.unwrap().target
    }

    /// Retail checked against its records and files index.
    pub fn report(home: &Home, mode: DriftMode) -> DriftReport {
        let index = files_index(home, "retail");
        check(&retail(home), &record(home, "retail"), Some(&index), mode)
    }
}

#[test]
fn a_fresh_install_is_clean_without_hashing_and_a_touch_is_rehashed_clean() {
    let home = Home::new(&["wow"]);
    foo_release(&home, "v1", 3);
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let r = report(&home, DriftMode::Stat);
    assert_eq!(r.addons.len(), 1);
    assert_eq!(r.addons[0].state, DriftState::Clean, "{r:#?}");
    assert_eq!(r.files_rehashed, 0, "nothing racy after the commit's wait");
    assert_eq!(r.files_checked, 5);

    touch_identical(&home.addons("_retail_").join("Foo/Foo.toc"));
    let r = report(&home, DriftMode::Stat);
    assert_eq!(
        (r.addons[0].state, r.files_rehashed),
        (DriftState::Clean, 1)
    );
}

#[test]
fn edits_deletions_and_stray_files_are_listed_by_folder() {
    let home = Home::new(&["wow"]);
    foo_release(&home, "v1", 3);
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let foo = home.addons("_retail_").join("Foo");
    edit_managed(&foo.join("libs/f0000.lua"), b"-- edited by hand\n");
    delete_managed(&foo.join("libs/f0001.lua"));
    add_stray_file(&foo, "stray.lua", b"-- mine\n");
    let r = report(&home, DriftMode::Stat);
    let addon = &r.addons[0];
    assert_eq!(addon.state, DriftState::Modified, "{r:#?}");
    let folder = addon
        .folders
        .iter()
        .find(|f| f.folder.as_str() == "Foo")
        .unwrap();
    assert_eq!(
        (
            &folder.modified,
            &folder.deleted,
            &folder.added,
            folder.changed
        ),
        (
            &vec!["libs/f0000.lua".to_owned()],
            &vec!["libs/f0001.lua".to_owned()],
            &vec!["stray.lua".to_owned()],
            3
        )
    );
    // The other folder is clean.
    let options = addon
        .folders
        .iter()
        .find(|f| f.folder.as_str() == "FooOptions")
        .unwrap();
    assert_eq!(options.changed, 0);
}

#[test]
fn a_folder_replaced_by_a_symlink_is_missing_and_not_followed() {
    let home = Home::new(&["wow"]);
    foo_release(&home, "v1", 1);
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let addons = home.addons("_retail_");
    let elsewhere = home.env.root().join("elsewhere");
    fs::rename(addons.join("FooOptions"), &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, addons.join("FooOptions")).unwrap();
    for mode in [DriftMode::Presence, DriftMode::Stat] {
        let r = report(&home, mode);
        assert_eq!(r.addons[0].state, DriftState::Missing, "{mode:?}: {r:#?}");
        let options = r.addons[0]
            .folders
            .iter()
            .find(|f| f.folder.as_str() == "FooOptions")
            .unwrap();
        assert!(!options.present);
    }
    // Presence alone opens no file.
    assert_eq!(report(&home, DriftMode::Presence).files_checked, 0);
}

#[test]
fn after_an_undo_the_restored_folders_are_not_rehashed() {
    let mut home = Home::new(&["wow"]);
    foo_release(&home, "v1", 3);
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    home.scenario("v2");
    foo_release(&home, "v2", 3);
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    home.run(&["undo"]).expect(0);
    let r = report(&home, DriftMode::Stat);
    assert_eq!(r.addons[0].state, DriftState::Clean, "{r:#?}");
    assert_eq!(r.files_rehashed, 0);
}

#[test]
fn an_index_without_folder_stamps_is_checked_against_its_write_time() {
    let home = Home::new(&["wow"]);
    foo_release(&home, "v1", 3);
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let mut index = common::state::files_index(&home, "retail");
    for folder in index.folders.values_mut() {
        folder.recorded_at_ns = None;
    }
    let target = support::retail(&home);
    let record = common::state::record(&home, "retail");
    // Written long after every file: nothing racy.
    index.written_at_ns = i128::MAX / 2;
    let late = omaddons::drift::check(&target, &record, Some(&index), DriftMode::Stat);
    assert_eq!(late.files_rehashed, 0, "every file is older than the stamp");
    // Written before every file: every file is racy, so re-hashed (and clean).
    index.written_at_ns = 0;
    let early = omaddons::drift::check(&target, &record, Some(&index), DriftMode::Stat);
    assert_eq!(
        early.files_rehashed, 5,
        "every file is newer than the stamp"
    );
    assert_eq!(early.addons[0].state, DriftState::Clean);
    // No index at all: every present folder is unindexed.
    let none = omaddons::drift::check(&target, &record, None, DriftMode::Stat);
    assert_eq!(none.addons[0].state, DriftState::Unindexed);
}

#[test]
fn a_check_writes_nothing_and_asks_nothing() {
    let home = Home::new(&["wow"]);
    foo_release(&home, "v1", 3);
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    edit_managed(
        &home.addons("_retail_").join("Foo/libs/f0002.lua"),
        b"-- changed\n",
    );
    let (state, cache, addons) = (
        tree_hash(&home.state()),
        tree_hash(&home.env.cache()),
        tree_hash(&home.addons("_retail_")),
    );
    let requests = net_log(&home).len();
    for mode in [DriftMode::Presence, DriftMode::Stat, DriftMode::Deep] {
        report(&home, mode);
    }
    assert_eq!(tree_hash(&home.state()), state);
    assert_eq!(tree_hash(&home.env.cache()), cache);
    assert_eq!(tree_hash(&home.addons("_retail_")), addons);
    assert_eq!(net_log(&home).len(), requests);
}

/// The budget: 5,000 files in 20 folders, installed through the engine, checked by Stat
/// right after in under 30 ms warm, with at most 1% re-hashed. Release only (H3 runs
/// it: `cargo test --release --test drift -- --ignored`).
#[test]
#[ignore = "a release-build budget"]
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
    let warm = report(&home, DriftMode::Stat);
    assert_eq!(warm.files_checked, 5_000);
    let started = std::time::Instant::now();
    let r = report(&home, DriftMode::Stat);
    let elapsed = started.elapsed();
    assert!(r.files_rehashed <= 50, "{} re-hashed", r.files_rehashed);
    assert!(
        elapsed < std::time::Duration::from_millis(30),
        "Stat took {elapsed:?}"
    );
}
