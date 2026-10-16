//! The files index's per-folder stamps end to end (M2-D16): a commit stamps only the
//! folders it adds or replaces, under the lock and at least `RACY_SLACK_NS` after their
//! newest staged mtime; undo records the restored copies as they are on disk; a crash
//! after the swap rolls forward to exactly the journaled stamps, and one before it
//! leaves the index alone; and `index_live_folder` over a committed folder equals the
//! commit's entries.

mod common;

use std::collections::BTreeMap;
use std::fs;

use common::engine::Scene;
use common::state::files_index;
use omaddons::engine::inspect::ArchiveLimits;
use omaddons::limits::RACY_SLACK_NS;
use omaddons::state::files_index::{FolderIndex, index_live_folder, mtime_ns};
use serde_json::Value;
use support::{journaled_stamps, stamps};

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::path::Path;

    use serde_json::Value;

    use super::common::engine::Scene;
    use super::common::state::files_index;

    /// Each folder's `recorded_at_ns`, by its key.
    pub fn stamps(s: &Scene) -> BTreeMap<String, Option<i128>> {
        files_index(&s.home, "retail")
            .folders
            .into_iter()
            .map(|(key, f)| (key, f.recorded_at_ns))
            .collect()
    }

    /// The upserts' stamps in a journal's `prepared` line, by folder key.
    pub fn journaled_stamps(journal: &Path) -> BTreeMap<String, Option<i128>> {
        let text = std::fs::read_to_string(journal).unwrap();
        let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        first["commit"]["index"]["upsert"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["name"].as_str().unwrap().to_ascii_lowercase(),
                    f["recorded_at_ns"].as_i64().map(i128::from),
                )
            })
            .collect()
    }
}

/// The folder's newest mtime, when its filesystem keeps sub-second mtimes.
fn newest_fine_mtime(folder: &FolderIndex) -> Option<i128> {
    let fine = folder.files.iter().any(|f| f.mtime_ns % 1_000_000_000 != 0);
    fine.then(|| folder.files.iter().map(|f| f.mtime_ns).max())
        .flatten()
}

/// What the commit and the indexer must agree on: paths, sizes, hashes and the tree.
fn content(folder: &FolderIndex) -> (Vec<(String, u64, String)>, String) {
    (
        folder
            .files
            .iter()
            .map(|f| (f.path.clone(), f.size, f.sha256.clone()))
            .collect(),
        folder.tree.clone(),
    )
}

#[test]
fn a_commit_stamps_only_its_folders_after_the_racy_slack() {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    let first = stamps(&s);
    assert!(
        first["foo"].is_some() && first["fooold"].is_some(),
        "{first:?}"
    );

    s.install("gh:o/bar", "bar");
    let second = stamps(&s);
    assert_eq!(
        second["foo"], first["foo"],
        "a folder B left alone keeps A's stamp"
    );
    assert_eq!(second["fooold"], first["fooold"]);
    assert!(second["bar"] > first["foo"], "{second:?}");

    let index = files_index(&s.home, "retail");
    for folder in index.folders.values() {
        if let Some(newest) = newest_fine_mtime(folder) {
            let stamp = folder.recorded_at_ns.unwrap();
            assert!(
                stamp >= newest + RACY_SLACK_NS,
                "{}: stamped {stamp}, newest mtime {newest}",
                folder.name
            );
        }
        // The live indexer sees exactly what the commit recorded.
        let live =
            index_live_folder(&s.addons().join(&folder.name), &ArchiveLimits::default()).unwrap();
        assert_eq!(content(&live), content(folder), "{}", folder.name);
        assert_eq!(live.name, folder.name);
        let mtimes = |f: &FolderIndex| f.files.iter().map(|e| e.mtime_ns).collect::<Vec<_>>();
        assert_eq!(mtimes(&live), mtimes(folder), "renames keep mtimes");
    }
    assert!(
        index.written_at_ns >= second["bar"].unwrap(),
        "the index is written after its stamps"
    );
}

#[test]
fn undo_records_the_restored_files_as_they_are_on_disk() {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    s.install("gh:o/foo", "v2");
    let undo = ["undo", "--offline", "--json"].map(str::to_owned);
    s.run(&undo, "").expect(0);
    assert_eq!(s.history().last().map(String::as_str), Some("undo"));

    let index = files_index(&s.home, "retail");
    assert!(index.folders.contains_key("fooold"), "v1's FooOld is back");
    for folder in index.folders.values() {
        for entry in &folder.files {
            let path = s.addons().join(&folder.name).join(&entry.path);
            let meta = fs::metadata(&path).unwrap();
            assert_eq!(
                (entry.size, entry.mtime_ns),
                (meta.len(), mtime_ns(&meta)),
                "{}",
                path.display()
            );
        }
        if let Some(newest) = newest_fine_mtime(folder) {
            assert!(folder.recorded_at_ns.unwrap() >= newest + RACY_SLACK_NS);
        }
    }
}

#[test]
fn a_crash_after_the_swap_rolls_forward_to_the_journaled_stamps() {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    let r = s.run(
        &s.install_args("gh:o/foo", "v2"),
        "engine.record.written=abort",
    );
    assert_eq!(r.code, -1, "{}", r.context());
    let journals = s.journals();
    assert_eq!(journals.len(), 1);
    let journaled = journaled_stamps(&journals[0]);
    assert!(
        journaled.get("foo").is_some_and(Option::is_some),
        "{journaled:?}"
    );

    // The next command that takes the lock rolls forward.
    s.install("gh:o/bar", "bar");
    assert!(s.journals().is_empty());
    let now = stamps(&s);
    for (key, stamp) in &journaled {
        assert_eq!(now.get(key), Some(stamp), "{key}");
    }
    assert!(!now.contains_key("fooold"), "v2 dropped FooOld");
}

#[test]
fn a_crash_before_the_swap_leaves_the_index_alone() {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    let path = s.home.state().join("targets").join(format!(
        "{}.files.json",
        common::state::key_of(&s.home, "retail")
    ));
    let before = fs::read(&path).unwrap();
    let foo_before: BTreeMap<String, Value> = {
        let index = files_index(&s.home, "retail");
        index
            .folders
            .iter()
            .map(|(k, f)| (k.clone(), serde_json::to_value(f).unwrap()))
            .collect()
    };
    let r = s.run(
        &s.install_args("gh:o/foo", "v2"),
        "engine.journal.prepared=abort",
    );
    assert_eq!(r.code, -1, "{}", r.context());
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "byte-identical after the crash"
    );

    // Rolled back by the next command: Foo's entries are exactly as before.
    s.install("gh:o/bar", "bar");
    let index = files_index(&s.home, "retail");
    for (key, entry) in &foo_before {
        assert_eq!(
            &serde_json::to_value(&index.folders[key]).unwrap(),
            entry,
            "{key}"
        );
    }
}
