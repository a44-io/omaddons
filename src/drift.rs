//! Drift: managed files that changed on disk since omaddons recorded them (M2 goal
//! G-M2-4), found against the per-file index every commit writes
//! (`<state>/omaddons/targets/<key>.files.json`).
//!
//! Three depths ([`DriftMode`]):
//! - `presence`: one `lstat` per managed folder; a folder that is gone, or replaced by a
//!   symlink, is missing. Reads no index and opens no file (`status --cached`).
//! - `stat`: walks each folder (no symlink followed). A file the index lists but the disk
//!   lacks is `deleted`, one on disk the index lacks is `added`. A file whose size or
//!   mtime differs is re-hashed, and so is a racy one (its mtime within the slack of its
//!   folder's `recorded_at_ns`, M2-D16): an equal hash is clean, another is `modified`.
//!   A stat match outside the racy window is trusted.
//! - `deep`: re-hashes every file.
//!
//! A pure reader (M2-D15): no lock, no request, no write, nothing persisted. Results are
//! typed and silent; `status`, `list` and `info` render them, and the M4 doctor adapts
//! them into findings.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

use crate::limits::{DRIFT_LIST_MAX, RACY_SLACK_COARSE_NS, RACY_SLACK_NS};
use crate::model::{FolderName, Target};
use crate::state::files_index::{FilesIndex, FolderIndex, hash_file_streaming, mtime_ns};
use crate::state::records::TargetRecord;

/// How deep a check looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriftMode {
    /// Each folder exists and is a real directory.
    Presence,
    /// Stat first; re-hash what changed and what is racy.
    Stat,
    /// Re-hash every file.
    Deep,
}

/// An addon's state, worst first: `missing`, `modified`, `unindexed`, `clean`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriftState {
    Clean,
    Modified,
    Missing,
    Unindexed,
}

/// One target's check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DriftReport {
    pub mode: DriftMode,
    /// Files compared (indexed or on disk).
    pub files_checked: u64,
    /// Files whose content was hashed.
    pub files_rehashed: u64,
    /// Every managed addon, by name ignoring case.
    pub addons: Vec<AddonDrift>,
}

/// One addon's folders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AddonDrift {
    pub addon: String,
    pub state: DriftState,
    /// By key.
    pub folders: Vec<FolderDrift>,
}

/// One managed folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FolderDrift {
    pub folder: FolderName,
    /// A real directory (not a symlink) is in AddOns under this name, ignoring case.
    pub present: bool,
    /// The index has its files, so it was judged (always `true` under `presence`,
    /// which reads no index).
    pub indexed: bool,
    /// Modified, deleted and added files, all of them.
    pub changed: u64,
    /// Folder-relative `/` paths, bytewise, at most `DRIFT_LIST_MAX` each.
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    pub added: Vec<String>,
    /// A list was cut at `DRIFT_LIST_MAX`.
    pub truncated: bool,
}

impl FolderDrift {
    fn new(folder: &FolderName, present: bool, indexed: bool) -> Self {
        Self {
            folder: folder.clone(),
            present,
            indexed,
            changed: 0,
            modified: Vec::new(),
            deleted: Vec::new(),
            added: Vec::new(),
            truncated: false,
        }
    }
}

/// A folder's check and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walked {
    pub drift: FolderDrift,
    pub files_checked: u64,
    pub files_rehashed: u64,
}

/// Checks every managed folder of `record` on `target`, against `index` (`None`: every
/// present folder is unindexed, except under `presence`).
pub fn check(
    target: &Target,
    record: &TargetRecord,
    index: Option<&FilesIndex>,
    mode: DriftMode,
) -> DriftReport {
    let addons_dir = target.addons_dir();
    let listing = Listing::read(&addons_dir);
    let mut report = DriftReport {
        mode,
        files_checked: 0,
        files_rehashed: 0,
        addons: Vec::new(),
    };
    // Keyed by the lowercase name: by name ignoring case.
    for addon in record.addons.values() {
        let mut names = addon.folders.clone();
        names.sort_by(|a, b| a.key().cmp(b.key()));
        let mut folders = Vec::with_capacity(names.len());
        for folder in &names {
            let entry = index.and_then(|i| i.folders.get(folder.key()));
            let recorded_at = match (index, entry) {
                (Some(index), Some(entry)) => index.recorded_at_ns(entry),
                _ => 0,
            };
            let walked = walk(&listing, &addons_dir, folder, entry, recorded_at, mode);
            report.files_checked += walked.files_checked;
            report.files_rehashed += walked.files_rehashed;
            folders.push(walked.drift);
        }
        report.addons.push(AddonDrift {
            addon: addon.name.clone(),
            state: state_of(&folders),
            folders,
        });
    }
    report
}

/// Checks one managed folder in `addons_dir` (found ignoring case) against its index
/// `entry`, whose files are trusted from `recorded_at_ns`
/// ([`FilesIndex::recorded_at_ns`]). For a plan that replaces a few folders.
pub fn check_folder(
    addons_dir: &Path,
    folder: &FolderName,
    entry: Option<&FolderIndex>,
    recorded_at_ns: i128,
    mode: DriftMode,
) -> Walked {
    let listing = Listing::read(addons_dir);
    walk(&listing, addons_dir, folder, entry, recorded_at_ns, mode)
}

/// `missing` when a folder is missing, else `modified` when one changed, else
/// `unindexed` when one was not judged, else `clean`.
fn state_of(folders: &[FolderDrift]) -> DriftState {
    if folders.iter().any(|f| !f.present) {
        DriftState::Missing
    } else if folders.iter().any(|f| f.changed > 0) {
        DriftState::Modified
    } else if folders.iter().any(|f| !f.indexed) {
        DriftState::Unindexed
    } else {
        DriftState::Clean
    }
}

/// AddOns' entries by key (ASCII case-folded), each with its on-disk names; `None` when
/// the directory cannot be read (every folder is then missing).
struct Listing(Option<BTreeMap<String, Vec<String>>>);

impl Listing {
    fn read(addons_dir: &Path) -> Self {
        let Ok(entries) = fs::read_dir(addons_dir) else {
            return Self(None);
        };
        let mut by_key: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in entries.filter_map(std::result::Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            by_key
                .entry(name.to_ascii_lowercase())
                .or_default()
                .push(name);
        }
        Self(Some(by_key))
    }

    /// Where `folder` is on disk: the index's spelling when it is there, else the first
    /// spelling with its key that is a real directory (a stray file or link spelled
    /// another way beside it never decides, whatever order the directory lists them in),
    /// else the first.
    fn path(
        &self,
        addons_dir: &Path,
        folder: &FolderName,
        entry: Option<&FolderIndex>,
    ) -> Option<PathBuf> {
        let names = self.0.as_ref()?.get(folder.key())?;
        let name = entry
            .and_then(|e| names.iter().find(|n| **n == e.name))
            .or_else(|| names.iter().find(|n| is_real_dir(&addons_dir.join(n))))
            .or_else(|| names.first())?;
        Some(addons_dir.join(name))
    }
}

/// A real directory, not a symlink to one.
fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

fn walk(
    listing: &Listing,
    addons_dir: &Path,
    folder: &FolderName,
    entry: Option<&FolderIndex>,
    recorded_at_ns: i128,
    mode: DriftMode,
) -> Walked {
    let dir = listing
        .path(addons_dir, folder, entry)
        .filter(|p| is_real_dir(p));
    let done = |drift| Walked {
        drift,
        files_checked: 0,
        files_rehashed: 0,
    };
    let Some(dir) = dir else {
        return done(FolderDrift::new(
            folder,
            false,
            entry.is_some() || mode == DriftMode::Presence,
        ));
    };
    if mode == DriftMode::Presence {
        return done(FolderDrift::new(folder, true, true));
    }
    let Some(entry) = entry else {
        return done(FolderDrift::new(folder, true, false));
    };
    compare(&dir, folder, entry, recorded_at_ns, mode)
}

/// What a walk found at one path.
enum OnDisk {
    File {
        size: u64,
        mtime_ns: i128,
    },
    Dir,
    /// A symlink, device, FIFO or socket: never followed.
    Other,
}

/// Every path under `dir`, relative and `/`-separated, without following symlinks.
fn list(dir: &Path, prefix: &str, out: &mut BTreeMap<String, OnDisk>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => {
                list(&path, &rel, out);
                out.insert(rel, OnDisk::Dir);
            }
            Ok(meta) if meta.is_file() => {
                out.insert(
                    rel,
                    OnDisk::File {
                        size: meta.len(),
                        mtime_ns: mtime_ns(&meta),
                    },
                );
            }
            _ => {
                out.insert(rel, OnDisk::Other);
            }
        }
    }
}

/// The racy slack of a folder: nanosecond mtimes (some indexed mtime has a sub-second
/// part) get `RACY_SLACK_NS`, whole-second ones `RACY_SLACK_COARSE_NS`.
pub(crate) fn slack(entry: &FolderIndex) -> i128 {
    if entry
        .files
        .iter()
        .any(|f| f.mtime_ns.rem_euclid(1_000_000_000) != 0)
    {
        RACY_SLACK_NS
    } else {
        RACY_SLACK_COARSE_NS
    }
}

fn compare(
    dir: &Path,
    folder: &FolderName,
    entry: &FolderIndex,
    recorded_at_ns: i128,
    mode: DriftMode,
) -> Walked {
    let mut on_disk = BTreeMap::new();
    list(dir, "", &mut on_disk);
    let racy_from = recorded_at_ns - slack(entry);
    let (mut modified, mut deleted, mut added) = (Vec::new(), Vec::new(), Vec::new());
    let (mut checked, mut rehashed) = (0u64, 0u64);
    let mut indexed: BTreeMap<&str, ()> = BTreeMap::new();
    for file in &entry.files {
        indexed.insert(file.path.as_str(), ());
        checked += 1;
        match on_disk.get(&file.path) {
            None => deleted.push(file.path.clone()),
            Some(OnDisk::Dir | OnDisk::Other) => modified.push(file.path.clone()),
            // A different size is a different file: no hash needed.
            Some(OnDisk::File { size, .. }) if *size != file.size => {
                modified.push(file.path.clone());
            }
            Some(OnDisk::File { mtime_ns, .. }) => {
                let trusted =
                    mode == DriftMode::Stat && *mtime_ns == file.mtime_ns && *mtime_ns < racy_from;
                if !trusted {
                    rehashed += 1;
                    let same = hash_file_streaming(&dir.join(&file.path))
                        .is_ok_and(|(_, sha)| sha.eq_ignore_ascii_case(&file.sha256));
                    if !same {
                        modified.push(file.path.clone());
                    }
                }
            }
        }
    }
    for (path, kind) in &on_disk {
        if matches!(kind, OnDisk::Dir) || indexed.contains_key(path.as_str()) {
            continue;
        }
        checked += 1;
        added.push(path.clone());
    }
    let changed = (modified.len() + deleted.len() + added.len()) as u64;
    let mut truncated = false;
    for list in [&mut modified, &mut deleted, &mut added] {
        list.sort_unstable();
        if list.len() > DRIFT_LIST_MAX {
            list.truncate(DRIFT_LIST_MAX);
            truncated = true;
        }
    }
    Walked {
        drift: FolderDrift {
            folder: folder.clone(),
            present: true,
            indexed: true,
            changed,
            modified,
            deleted,
            added,
            truncated,
        },
        files_checked: checked,
        files_rehashed: rehashed,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::state::files_index::{FileEntry, index_live_folder};

    fn f(name: &str) -> FolderName {
        FolderName::new(name).unwrap()
    }

    /// `files` written under `addons/<name>`.
    fn folder(addons: &Path, name: &str, files: &[(&str, &str)]) {
        for (rel, text) in files {
            let path = addons.join(name).join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    /// The live index of `addons/<name>`, stamped `at` (default: well after every mtime,
    /// so nothing is racy).
    fn index(addons: &Path, name: &str, at: Option<i128>) -> FolderIndex {
        let live = index_live_folder(&addons.join(name), &Default::default()).unwrap();
        let newest = live.files.iter().map(|e| e.mtime_ns).max().unwrap_or(0);
        live.stamped(at.unwrap_or(newest + 10_000_000_000))
    }

    /// Without an index, the spelling that is a real directory is the folder, whatever
    /// order the directory lists a stray file spelled another way in.
    #[test]
    fn a_stray_file_spelled_another_way_never_hides_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path();
        folder(addons, "Foo", &[("Foo.toc", "## Interface: 120105\n")]);
        fs::write(addons.join("foo"), "stray").unwrap();
        for order in [["foo", "Foo"], ["Foo", "foo"]] {
            let names = order.iter().map(|n| (*n).to_owned()).collect();
            let listing = Listing(Some(BTreeMap::from([("foo".to_owned(), names)])));
            assert_eq!(
                listing.path(addons, &f("Foo"), None),
                Some(addons.join("Foo")),
                "{order:?}"
            );
        }
    }

    fn run(addons: &Path, name: &str, entry: &FolderIndex, mode: DriftMode) -> Walked {
        let at = entry.recorded_at_ns.unwrap_or(0);
        check_folder(addons, &f(name), Some(entry), at, mode)
    }

    fn set_mtime(path: &Path, ns: i128) {
        let at = SystemTime::UNIX_EPOCH + Duration::from_nanos(u64::try_from(ns).unwrap());
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    #[test]
    fn modified_deleted_and_added_files_are_listed_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path();
        folder(
            addons,
            "Foo",
            &[
                ("Foo.toc", "toc"),
                ("core.lua", "core"),
                ("libs/a.lua", "a"),
            ],
        );
        let entry = index(addons, "Foo", None);
        fs::write(addons.join("Foo/core.lua"), "CHANGED, LONGER").unwrap();
        fs::remove_file(addons.join("Foo/libs/a.lua")).unwrap();
        folder(addons, "Foo", &[("extra/new.lua", "new")]);
        let walked = run(addons, "Foo", &entry, DriftMode::Stat);
        let d = &walked.drift;
        assert_eq!(
            (&d.modified, &d.deleted, &d.added),
            (
                &vec!["core.lua".to_owned()],
                &vec!["libs/a.lua".to_owned()],
                &vec!["extra/new.lua".to_owned()]
            )
        );
        assert_eq!(
            (d.changed, d.truncated, d.present, d.indexed),
            (3, false, true, true)
        );
        assert_eq!(walked.files_rehashed, 0, "a size change needs no hash");
    }

    #[test]
    fn a_touched_identical_file_is_rehashed_and_clean() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path();
        folder(addons, "Foo", &[("Foo.toc", "toc")]);
        let entry = index(addons, "Foo", None);
        let file = addons.join("Foo/Foo.toc");
        set_mtime(&file, entry.files[0].mtime_ns + 2_000_000_000);
        let walked = run(addons, "Foo", &entry, DriftMode::Stat);
        assert_eq!((walked.drift.changed, walked.files_rehashed), (0, 1));
    }

    #[test]
    fn racy_files_are_rehashed_and_others_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path();
        folder(addons, "Foo", &[("Foo.toc", "toc1")]);
        let file = addons.join("Foo/Foo.toc");
        let mtime = 1_760_000_000_123_456_789_i128;
        set_mtime(&file, mtime);
        // Same size and mtime, other bytes: only a hash can tell.
        let swap = |entry: &FolderIndex| {
            fs::write(&file, "toc2").unwrap();
            set_mtime(&file, mtime);
            let walked = run(addons, "Foo", entry, DriftMode::Stat);
            fs::write(&file, "toc1").unwrap();
            set_mtime(&file, mtime);
            walked
        };
        // Recorded 5 ms after the mtime: inside the 20 ms slack.
        let racy = index(addons, "Foo", Some(mtime + 5_000_000));
        let walked = swap(&racy);
        assert_eq!(walked.drift.modified, ["Foo.toc"]);
        assert_eq!(walked.files_rehashed, 1);
        // Recorded a second later: the stat match is trusted.
        let settled = index(addons, "Foo", Some(mtime + 1_000_000_000));
        let walked = swap(&settled);
        assert_eq!((walked.drift.changed, walked.files_rehashed), (0, 0));
        // Deep re-hashes it anyway.
        fs::write(&file, "toc2").unwrap();
        set_mtime(&file, mtime);
        let walked = run(addons, "Foo", &settled, DriftMode::Deep);
        assert_eq!(walked.drift.modified, ["Foo.toc"]);
    }

    #[test]
    fn whole_second_mtimes_get_the_coarse_slack() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path();
        folder(addons, "Foo", &[("Foo.toc", "toc")]);
        let file = addons.join("Foo/Foo.toc");
        let mtime = 1_760_000_000_000_000_000_i128;
        set_mtime(&file, mtime);
        // A second after a whole-second mtime: racy on a coarse filesystem.
        let entry = index(addons, "Foo", Some(mtime + 1_000_000_000));
        assert_eq!(slack(&entry), RACY_SLACK_COARSE_NS);
        assert_eq!(
            run(addons, "Foo", &entry, DriftMode::Stat).files_rehashed,
            1
        );
        let fine = FolderIndex::new(
            "Foo",
            vec![FileEntry {
                path: "Foo.toc".into(),
                size: 3,
                mtime_ns: mtime + 1,
                sha256: String::new(),
            }],
        );
        assert_eq!(slack(&fine), RACY_SLACK_NS);
    }

    #[test]
    fn lists_are_capped_and_the_count_stays_exact() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path();
        folder(addons, "Foo", &[("Foo.toc", "toc")]);
        let entry = index(addons, "Foo", None);
        let stray: Vec<(String, &str)> = (0..DRIFT_LIST_MAX + 7)
            .map(|i| (format!("stray{i:03}.lua"), "x"))
            .collect();
        let refs: Vec<(&str, &str)> = stray.iter().map(|(p, t)| (p.as_str(), *t)).collect();
        folder(addons, "Foo", &refs);
        let d = run(addons, "Foo", &entry, DriftMode::Stat).drift;
        assert_eq!(d.added.len(), DRIFT_LIST_MAX);
        assert_eq!(
            (d.changed, d.truncated),
            ((DRIFT_LIST_MAX + 7) as u64, true)
        );
        assert_eq!(d.added.first().map(String::as_str), Some("stray000.lua"));
    }

    #[test]
    fn special_entries_are_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path();
        folder(addons, "Foo", &[("Foo.toc", "toc"), ("data.lua", "d")]);
        let entry = index(addons, "Foo", None);
        // An indexed file replaced by a symlink is modified; a new symlink is added.
        fs::remove_file(addons.join("Foo/data.lua")).unwrap();
        symlink("/etc/hostname", addons.join("Foo/data.lua")).unwrap();
        symlink("/etc", addons.join("Foo/etc")).unwrap();
        fs::create_dir_all(addons.join("Foo/empty")).unwrap();
        let d = run(addons, "Foo", &entry, DriftMode::Stat).drift;
        assert_eq!(
            (d.modified, d.added),
            (vec!["data.lua".to_owned()], vec!["etc".to_owned()]),
            "the link is not followed and the empty directory is ignored"
        );
    }

    #[test]
    fn presence_finds_folders_ignoring_case_and_never_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path().join("AddOns");
        folder(&addons, "foo", &[("Foo.toc", "toc")]);
        let real = dir.path().join("elsewhere");
        folder(&real, "Bar", &[("Bar.toc", "toc")]);
        symlink(real.join("Bar"), addons.join("Bar")).unwrap();
        for mode in [DriftMode::Presence, DriftMode::Stat] {
            let foo = check_folder(&addons, &f("Foo"), None, 0, mode).drift;
            assert!(foo.present, "{mode:?}: found ignoring case");
            let bar = check_folder(&addons, &f("Bar"), None, 0, mode).drift;
            assert!(!bar.present, "{mode:?}: a symlinked folder is missing");
        }
        let gone = check_folder(
            &dir.path().join("NoAddOns"),
            &f("Foo"),
            None,
            0,
            DriftMode::Presence,
        );
        assert!(!gone.drift.present, "no AddOns: every folder is missing");
    }

    #[test]
    fn an_unindexed_folder_is_reported_not_judged() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path();
        folder(addons, "Foo", &[("Foo.toc", "toc")]);
        let d = check_folder(addons, &f("Foo"), None, 0, DriftMode::Stat).drift;
        assert_eq!((d.present, d.indexed, d.changed), (true, false, 0));
    }

    #[test]
    fn the_addon_state_is_the_worst_folder() {
        let folder = |present, indexed, changed| FolderDrift {
            changed,
            ..FolderDrift::new(&f("Foo"), present, indexed)
        };
        assert_eq!(state_of(&[folder(true, true, 0)]), DriftState::Clean);
        assert_eq!(
            state_of(&[folder(true, false, 0), folder(true, true, 0)]),
            DriftState::Unindexed
        );
        assert_eq!(
            state_of(&[folder(true, false, 0), folder(true, true, 2)]),
            DriftState::Modified
        );
        assert_eq!(
            state_of(&[folder(false, true, 0), folder(true, true, 2)]),
            DriftState::Missing
        );
    }
}
