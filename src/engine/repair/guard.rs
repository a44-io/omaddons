//! Guards (M4-D38): what a set-aside records as its suspension manifest, and what each
//! guard reads now. The undo of a set-aside reactivates the journal only while every
//! guard still holds (E7, M4-D46): once the client's record, its marker or a folder the
//! journal would recover changed, the journal's instructions were written against an
//! earlier filesystem, and putting it back would replay them over newer work.
//!
//! Everything here reads; nothing writes. A `WTF/` epoch is one stat walk of the tree (a
//! [`StampDigest`]): it reads no content and names no path below `WTF/`, in a value, a
//! message or an error's details.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::engine::repair_action::{Guard, Sha256, Stamp};
use crate::error::{Error, Result, Suggestion};
use crate::fsx::{Expect, Parts, StampDigest, canonical_lenient};
use crate::model::{FolderName, TargetKey};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::load_record;

/// What a guard reads: the value it recorded, or the one there now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardValue {
    /// A target record's generation.
    Generation(u64),
    /// A marker's content digest.
    Digest(Sha256),
    /// A directory's stamp.
    Stamp(Stamp),
    /// Nothing is there.
    Absent,
    /// Something no guard records: a marker that is not a regular file, or a folder or a
    /// `WTF/` that is not a directory. It equals no recorded value.
    Other,
}

/// The text a token holds: the generation, the digest or stamp in its wire form, `absent`
/// or `other`.
impl fmt::Display for GuardValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Generation(generation) => write!(f, "{generation}"),
            Self::Digest(digest) => write!(f, "{digest}"),
            Self::Stamp(stamp) => write!(f, "{stamp}"),
            Self::Absent => f.write_str("absent"),
            Self::Other => f.write_str("other"),
        }
    }
}

/// The value `guard` recorded when the journal was set aside.
pub fn recorded(guard: &Guard) -> GuardValue {
    match guard {
        Guard::Generation { generation, .. } => GuardValue::Generation(*generation),
        Guard::Marker { sha256, .. } => sha256.map_or(GuardValue::Absent, GuardValue::Digest),
        Guard::Epoch { stamp, .. } | Guard::WtfEpoch { stamp, .. } => {
            stamp.map_or(GuardValue::Absent, GuardValue::Stamp)
        }
    }
}

/// How `details.changed_guards` names a changed guard: `generation`, `marker` or `epochs` (a
/// folder's or `WTF/`'s), E7's closed set.
pub const fn kind(guard: &Guard) -> &'static str {
    match guard {
        Guard::Generation { .. } => "generation",
        Guard::Marker { .. } => "marker",
        Guard::Epoch { .. } | Guard::WtfEpoch { .. } => "epochs",
    }
}

/// The client `guard` is about: its target, or, for a folder's epoch, the client among
/// `clients` whose directory holds the folder.
pub fn target(guard: &Guard, clients: &[(TargetKey, PathBuf)]) -> Option<TargetKey> {
    match guard {
        Guard::Generation { target, .. }
        | Guard::Marker { target, .. }
        | Guard::WtfEpoch { target, .. } => Some(*target),
        Guard::Epoch { path, .. } => clients.iter().find_map(|(key, dir)| {
            let dir = canonical_lenient(dir).unwrap_or_else(|| dir.clone());
            path.starts_with(&dir).then_some(*key)
        }),
    }
}

/// What `guard` reads now. `clients` are the discovered client directories by key: the
/// marker and `WTF/` of a client not among them cannot be found, and read [`Absent`]
/// (so a recorded digest or stamp of one refuses). A folder's epoch is read at exactly
/// the path recorded: a folder renamed since, if only in case, reads as gone, since the
/// suspended journal names it by that path.
///
/// [`Absent`]: GuardValue::Absent
pub fn current(
    guard: &Guard,
    clients: &[(TargetKey, PathBuf)],
    paths: &Paths,
) -> Result<GuardValue> {
    let client = |target: &TargetKey| {
        clients
            .iter()
            .find(|(key, _)| key == target)
            .map(|(_, dir)| ClientPaths::new(dir))
    };
    match guard {
        Guard::Generation { target, .. } => Ok(GuardValue::Generation(
            load_record(paths, target)?.value.generation,
        )),
        Guard::Marker { target, .. } => match client(target) {
            Some(client) => marker_value(&client.marker()),
            None => Ok(GuardValue::Absent),
        },
        Guard::Epoch { path, .. } => stamp_value(path),
        Guard::WtfEpoch { target, .. } => match client(target) {
            Some(client) => stamp_value(&client.wtf()),
            None => Ok(GuardValue::Absent),
        },
    }
}

/// The kinds of `guards` whose value now (`values`, in the same order) differs from the
/// one recorded, sorted and without duplicates.
pub fn changed_kinds(guards: &[Guard], values: &[GuardValue]) -> BTreeSet<&'static str> {
    guards
        .iter()
        .zip(values)
        .filter(|(guard, value)| recorded(guard) != **value)
        .map(|(guard, _)| kind(guard))
        .collect()
}

// ---------------------------------------------------------------------------------------
// Observing, for a set-aside's manifest (F2) and the test fixer.

/// The target's record generation now.
pub fn observe_generation(paths: &Paths, key: TargetKey) -> Result<Guard> {
    Ok(Guard::Generation {
        target: key,
        generation: load_record(paths, &key)?.value.generation,
    })
}

/// The digest of the client's marker now, or its absence.
pub fn observe_marker(client_dir: &Path, key: TargetKey) -> Result<Guard> {
    let marker = ClientPaths::new(client_dir).marker();
    let sha256 = match marker_value(&marker)? {
        GuardValue::Digest(digest) => Some(digest),
        GuardValue::Absent => None,
        _ => return Err(unrecordable(&marker, "it is not a regular file")),
    };
    Ok(Guard::Marker {
        target: key,
        sha256,
    })
}

/// The stamp of `<client dir>/Interface/AddOns/<folder>` now, or its absence: the folder
/// found by its key, ignoring case as Wine does, and recorded under the spelling found
/// (`folder` itself when none is there), canonical.
pub fn observe_folder_epoch(client_dir: &Path, folder: &FolderName) -> Result<Guard> {
    let addons = ClientPaths::new(client_dir).addons();
    let addons = canonical_lenient(&addons).unwrap_or(addons);
    let name = spelled(&addons, folder).unwrap_or_else(|| folder.as_str().to_owned());
    let path = addons.join(name);
    let stamp = match stamp_value(&path)? {
        GuardValue::Stamp(stamp) => Some(stamp),
        GuardValue::Absent => None,
        _ => return Err(unrecordable(&path, "it is not a folder")),
    };
    Ok(Guard::Epoch { path, stamp })
}

/// The stamp of the client's whole `WTF/` now, or its absence: one stat walk, no content
/// and no path below it.
pub fn observe_wtf_epoch(client_dir: &Path, key: TargetKey) -> Result<Guard> {
    let wtf = ClientPaths::new(client_dir).wtf();
    let stamp = match stamp_value(&wtf)? {
        GuardValue::Stamp(stamp) => Some(stamp),
        GuardValue::Absent => None,
        _ => return Err(unrecordable(&wtf, "it is not a folder")),
    };
    Ok(Guard::WtfEpoch { target: key, stamp })
}

// ---------------------------------------------------------------------------------------
// Reading.

/// The marker's digest, streamed; `Absent` when there is none, `Other` when it is not a
/// regular file.
fn marker_value(marker: &Path) -> Result<GuardValue> {
    let parts = Parts {
        sha256: true,
        ..Parts::default()
    };
    match Expect::capture(marker, parts) {
        Ok(Expect::Absent) => Ok(GuardValue::Absent),
        Ok(Expect::Present(found)) => Ok(found.sha256.map_or(GuardValue::Other, |digest| {
            GuardValue::Digest(Sha256(digest))
        })),
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == io::ErrorKind::InvalidInput => Ok(GuardValue::Other),
        Err(err) => Err(unreadable(marker, &err)),
    }
}

/// The stamp of the directory at `path`, read without following it; `Absent` when there
/// is nothing, `Other` when it is not a directory. An error names `path` alone: the stamp
/// walk's own errors carry no path, so one below `WTF/` is never named.
fn stamp_value(path: &Path) -> Result<GuardValue> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(GuardValue::Absent),
        Err(err) => return Err(unreadable(path, &err)),
    };
    if !meta.is_dir() {
        return Ok(GuardValue::Other);
    }
    let digest = StampDigest::compute(path).map_err(|err| unreadable(path, &err))?;
    Stamp::from_str(&digest.to_string())
        .map(GuardValue::Stamp)
        .map_err(|err| Error::internal(format!("a stamp digest that does not read: {err}")))
}

/// The entry of `dir` whose name is `folder`'s, ignoring ASCII case: the exact spelling
/// when it is there, else the one other spelling there is. Several other spellings name
/// none of them.
fn spelled(dir: &Path, folder: &FolderName) -> Option<String> {
    let entries = fs::read_dir(dir).ok()?;
    let names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.eq_ignore_ascii_case(folder.as_str()))
        .collect();
    if names.iter().any(|name| name == folder.as_str()) {
        return Some(folder.as_str().to_owned());
    }
    match &names[..] {
        [one] => Some(one.clone()),
        _ => None,
    }
}

fn unreadable(path: &Path, err: &io::Error) -> Error {
    Error::io(
        path,
        err,
        Suggestion::text(format!(
            "check {} and its permissions; nothing was changed",
            path.display()
        )),
    )
}

/// A set-aside cannot record what is at `path`: nothing is set aside without its guard.
fn unrecordable(path: &Path, why: &str) -> Error {
    unreadable(
        path,
        &io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{why}, so no guard can record it"),
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::engine::testing::{REPAIR_ACCOUNT, RepairHome, repair_home, tree_hash};
    use crate::state::records::TargetRecord;
    use crate::state::store;

    /// [`repair_home`] with a target record that reads, at generation 1.
    fn home() -> RepairHome {
        let home = repair_home();
        record_at(&home, 1);
        home
    }

    fn record_at(home: &RepairHome, generation: u64) {
        let mut record = TargetRecord::empty(home.key);
        record.generation = generation;
        fs::write(home.record(), store::encode(&record).unwrap()).unwrap();
    }

    /// Every guard kind observed on `home`'s planned client, its `Stray` folder and an
    /// absent `Gone` folder included.
    fn observed(home: &RepairHome) -> Vec<Guard> {
        let dir = home.client.client_dir();
        vec![
            observe_generation(&home.paths, home.key).unwrap(),
            observe_marker(dir, home.key).unwrap(),
            observe_folder_epoch(dir, &FolderName::new("Stray").unwrap()).unwrap(),
            observe_folder_epoch(dir, &FolderName::new("Gone").unwrap()).unwrap(),
            observe_wtf_epoch(dir, home.key).unwrap(),
        ]
    }

    fn clients(home: &RepairHome) -> Vec<(TargetKey, PathBuf)> {
        vec![(home.key, home.client.client_dir().to_path_buf())]
    }

    fn values(home: &RepairHome, guards: &[Guard]) -> Vec<GuardValue> {
        guards
            .iter()
            .map(|guard| current(guard, &clients(home), &home.paths).unwrap())
            .collect()
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn an_unchanged_world_passes_every_guard() {
        let home = home();
        let guards = observed(&home);
        assert!(
            matches!(&guards[3], Guard::Epoch { stamp: None, .. }),
            "an absent folder is recorded as absent: {:?}",
            guards[3]
        );
        let before = tree_hash(&home.base);
        let now = values(&home, &guards);
        assert_eq!(
            changed_kinds(&guards, &now),
            BTreeSet::new(),
            "guards {guards:?} read {now:?}"
        );
        assert_eq!(tree_hash(&home.base), before, "reading writes nothing");
    }

    #[test]
    fn each_guard_reports_its_own_change() {
        type Change = fn(&RepairHome);
        let cases: Vec<(&str, Change, &[&str])> = vec![
            ("generation_moved", |h| record_at(h, 9), &["generation"]),
            (
                "marker_rewritten",
                |h| write(&h.client.marker(), "{\"key\":\"another\"}\n"),
                &["marker"],
            ),
            (
                "marker_removed",
                |h| fs::remove_file(h.client.marker()).unwrap(),
                &["marker"],
            ),
            (
                "folder_file_edited",
                |h| write(&h.addons("Stray/stray.lua"), "-- edited, a little longer\n"),
                &["epochs"],
            ),
            (
                "folder_removed",
                |h| fs::remove_dir_all(h.addons("Stray")).unwrap(),
                &["epochs"],
            ),
            (
                "folder_added_where_absent",
                |h| write(&h.addons("Gone/Gone.toc"), "## Interface: 120105\n"),
                &["epochs"],
            ),
            (
                "folder_now_a_file",
                |h| {
                    fs::remove_dir_all(h.addons("Stray")).unwrap();
                    write(&h.addons("Stray"), "a file now\n");
                },
                &["epochs"],
            ),
            (
                "wtf_file_touched",
                |h| {
                    write(
                        &h.client.wtf().join("Config.wtf"),
                        "SET realmList \"elsewhere\"\n",
                    )
                },
                &["epochs"],
            ),
            (
                "wtf_removed",
                |h| fs::remove_dir_all(h.client.wtf()).unwrap(),
                &["epochs"],
            ),
            (
                "marker_and_folder",
                |h| {
                    write(&h.client.marker(), "{\"key\":\"another\"}\n");
                    fs::remove_dir_all(h.addons("Stray")).unwrap();
                },
                &["epochs", "marker"],
            ),
        ];
        for (case, change, want) in cases {
            let home = home();
            let guards = observed(&home);
            change(&home);
            let now = values(&home, &guards);
            let want: BTreeSet<&str> = want.iter().copied().collect();
            assert_eq!(
                changed_kinds(&guards, &now),
                want,
                "case {case}: guards {guards:?} read {now:?}"
            );
        }
    }

    #[test]
    fn a_client_not_discovered_reads_absent() {
        let home = home();
        let guards = observed(&home);
        let now: Vec<GuardValue> = guards
            .iter()
            .map(|guard| current(guard, &[], &home.paths).unwrap())
            .collect();
        assert_eq!(
            changed_kinds(&guards, &now),
            ["marker", "epochs"].into_iter().collect(),
            "the marker and WTF/ of a client not found cannot hold: {now:?}"
        );
    }

    #[test]
    fn a_folder_epoch_is_found_by_key_and_kept_by_path() {
        let home = home();
        let dir = home.client.client_dir();
        let guard = observe_folder_epoch(dir, &FolderName::new("stray").unwrap()).unwrap();
        let Guard::Epoch { path, stamp } = &guard else {
            panic!("{guard:?}"); // ubs:ignore — fails the test
        };
        assert_eq!(path.file_name().unwrap(), "Stray", "the spelling on disk");
        assert!(stamp.is_some(), "{guard:?}");
        assert_eq!(target(&guard, &clients(&home)), Some(home.key));
        // Renamed in case only: the journal names it by its old path, so it is gone.
        fs::rename(home.addons("Stray"), home.addons("stray")).unwrap();
        assert_eq!(
            current(&guard, &clients(&home), &home.paths).unwrap(),
            GuardValue::Absent
        );
    }

    #[test]
    fn a_wtf_epoch_never_names_a_path_below_wtf() {
        let home = home();
        let dir = home.client.client_dir();
        let character = home
            .client
            .wtf()
            .join(format!("Account/{REPAIR_ACCOUNT}/FakeRealm/FakeChar"));
        write(&character.join("SavedVariables/X.lua"), "X = {}\n");
        let guard = observe_wtf_epoch(dir, home.key).unwrap();
        let value = current(&guard, &clients(&home), &home.paths).unwrap();
        // An unreadable folder below WTF/ fails the walk without naming it.
        fs::set_permissions(&character, fs::Permissions::from_mode(0o000)).unwrap();
        let err = current(&guard, &clients(&home), &home.paths);
        fs::set_permissions(&character, fs::Permissions::from_mode(0o755)).unwrap();
        let shown = format!(
            "{} {value} {}",
            serde_json::to_string(&guard).unwrap(),
            err.as_ref().map_or_else(
                |e| format!("{} {} {}", e.message, e.fix, e.details),
                |v| v.to_string()
            )
        );
        for name in [REPAIR_ACCOUNT, "FakeRealm", "FakeChar"] {
            assert!(!shown.contains(name), "{name} in {shown}");
        }
    }

    #[test]
    fn what_cannot_be_recorded_is_refused() {
        let home = home();
        let dir = home.client.client_dir();
        fs::remove_dir_all(home.addons("Stray")).unwrap();
        write(&home.addons("Stray"), "a file\n");
        let err = observe_folder_epoch(dir, &FolderName::new("Stray").unwrap()).unwrap_err();
        assert!(err.message.contains("not a folder"), "{}", err.message);
        fs::remove_file(home.client.marker()).unwrap();
        fs::create_dir(home.client.marker()).unwrap();
        let err = observe_marker(dir, home.key).unwrap_err();
        assert!(
            err.message.contains("not a regular file"),
            "{}",
            err.message
        );
    }
}
