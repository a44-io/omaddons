//! Records-only changes (M2-D6): edits to an addon's record that move no folder. A pin,
//! a channel or reason, dependency bookkeeping, re-pointing to another ref, adopting
//! unmanaged folders, and forgetting an addon whose folders are gone. [`apply_edit`]
//! applies one to a record; a [`RecordGuard`] says what must still hold under the lock
//! for the change to commit as planned.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::{Error, ErrorCode, Suggestion};
use crate::inventory::{FolderClass, Inventory};
use crate::model::{AddonReason, AddonRecord, Channel, FolderName, Ref, VersionConfidence};
use crate::state::files_index::FolderIndex;
use crate::state::records::TargetRecord;

/// One change to an addon's record alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordEdit {
    SetPinned(bool),
    SetChannel(Channel),
    SetReason(AddonReason),
    /// `--any-flavor` set or cleared ([`AddonRecord::any_flavor`]).
    SetAnyFlavor(bool),
    /// Another addon requires this one.
    AddRequiredBy(String),
    RemoveRequiredBy(String),
    /// Drops the record of an addon whose folders are all gone from AddOns.
    Forget,
    /// Installs from another ref from now on: the release is forgotten, so the next
    /// update installs from the new ref.
    Repoint(Ref),
    /// Brings unmanaged folders under management as they are.
    Adopt(Box<AdoptSpec>),
}

impl RecordEdit {
    /// The guard this edit needs (see [`RecordGuard`]).
    pub fn guard(&self, before: Option<&AddonRecord>) -> RecordGuard {
        match self {
            Self::SetPinned(_)
            | Self::SetChannel(_)
            | Self::SetReason(_)
            | Self::SetAnyFlavor(_)
            | Self::Repoint(_) => RecordGuard {
                recorded: true,
                release: Some(before.and_then(|r| r.release_id.clone())),
                ..RecordGuard::default()
            },
            Self::AddRequiredBy(_) | Self::RemoveRequiredBy(_) => RecordGuard {
                recorded: true,
                ..RecordGuard::default()
            },
            Self::Forget => RecordGuard {
                recorded: true,
                absent: before.map(|r| r.folders.clone()).unwrap_or_default(),
                ..RecordGuard::default()
            },
            Self::Adopt(_) => RecordGuard {
                adopt: true,
                ..RecordGuard::default()
            },
        }
    }
}

/// An adoption, computed without the lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptSpec {
    /// The new record; its folders are the adopted ones.
    pub record: AddonRecord,
    /// Each adopted folder, indexed at plan time. Its entries' `(size, mtime_ns)` are
    /// what the guard re-checks under the lock.
    pub folders: Vec<(FolderName, FolderIndex)>,
}

/// A record field an edit changes, as `Change.fields` names it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EditField {
    Channel,
    Pinned,
    Reason,
    Ref,
    RequiredBy,
    AnyFlavor,
}

impl EditField {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Channel => "channel",
            Self::Pinned => "pinned",
            Self::Reason => "reason",
            Self::Ref => "ref",
            Self::RequiredBy => "required_by",
            Self::AnyFlavor => "any_flavor",
        }
    }
}

/// The fields `after` changes in `before`, sorted. A record that appears or goes
/// (adopt, forget) has none: its change kind says it.
pub fn changed_fields(before: Option<&AddonRecord>, after: Option<&AddonRecord>) -> Vec<EditField> {
    let (Some(before), Some(after)) = (before, after) else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    if before.channel != after.channel {
        fields.push(EditField::Channel);
    }
    if before.pinned != after.pinned {
        fields.push(EditField::Pinned);
    }
    if before.reason != after.reason {
        fields.push(EditField::Reason);
    }
    if before.r#ref != after.r#ref {
        fields.push(EditField::Ref);
    }
    if before.required_by != after.required_by {
        fields.push(EditField::RequiredBy);
    }
    if before.any_flavor != after.any_flavor {
        fields.push(EditField::AnyFlavor);
    }
    fields
}

/// Addon names sorted and de-duplicated by [`TargetRecord::addon_key`] (the first
/// spelling stays).
pub fn sorted_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut by_key: BTreeMap<String, String> = BTreeMap::new();
    for name in names {
        by_key.entry(TargetRecord::addon_key(&name)).or_insert(name);
    }
    by_key.into_values().collect()
}

/// `record` (the addon `addon`'s, on `target`) after `edit`. An edit of an addon with no
/// record is `not_managed`, adopting a name already recorded `ownership_conflict`.
pub fn apply_edit(
    record: Option<AddonRecord>,
    edit: &RecordEdit,
    addon: &str,
    target: &str,
) -> crate::error::Result<Option<AddonRecord>> {
    let mut record = match (record, edit) {
        (None, RecordEdit::Adopt(spec)) => return Ok(Some(spec.record.clone())),
        (Some(existing), RecordEdit::Adopt(_)) => {
            return Err(Error::new(
                ErrorCode::OwnershipConflict,
                format!("{} is already managed on {target}", existing.name),
                Suggestion::command(["omaddons", "list", "-t", target]),
            ));
        }
        (None, _) => return Err(not_managed(addon, target)),
        (Some(_), RecordEdit::Forget) => return Ok(None),
        (Some(record), _) => record,
    };
    match edit {
        RecordEdit::SetPinned(pinned) => record.pinned = *pinned,
        RecordEdit::SetChannel(channel) => record.channel = *channel,
        RecordEdit::SetReason(reason) => record.reason = *reason,
        RecordEdit::SetAnyFlavor(any) => record.any_flavor = *any,
        RecordEdit::AddRequiredBy(name) => {
            let names = record.required_by.iter().cloned().chain([name.clone()]);
            record.required_by = sorted_names(names);
        }
        RecordEdit::RemoveRequiredBy(name) => {
            let key = TargetRecord::addon_key(name);
            let names = record.required_by.iter().cloned();
            record.required_by = sorted_names(names)
                .into_iter()
                .filter(|n| TargetRecord::addon_key(n) != key)
                .collect();
        }
        RecordEdit::Repoint(r) => {
            record.r#ref = r.clone();
            record.release_id = None;
            record.version_confidence = VersionConfidence::Toc;
            // Marks name releases of the old source; the undo of the repoint puts the
            // record, marks included, back from its snapshot.
            record.skipped_releases.clear();
        }
        RecordEdit::Forget | RecordEdit::Adopt(_) => {}
    }
    Ok(Some(record))
}

/// `edits` applied in order.
pub fn apply_edits(
    mut record: Option<AddonRecord>,
    edits: &[RecordEdit],
    addon: &str,
    target: &str,
) -> crate::error::Result<Option<AddonRecord>> {
    for edit in edits {
        record = apply_edit(record, edit, addon, target)?;
    }
    Ok(record)
}

/// `edits` without a `required_by` edit that changes nothing on `record` (another
/// transaction made it already): history logs what this one changes, so undo never
/// takes back a requester someone else added.
pub fn effective_edits(
    mut record: Option<AddonRecord>,
    edits: &[RecordEdit],
    addon: &str,
    target: &str,
) -> crate::error::Result<Vec<RecordEdit>> {
    let mut kept = Vec::new();
    for edit in edits {
        let next = apply_edit(record.clone(), edit, addon, target)?;
        let required_by = |r: &Option<AddonRecord>| r.as_ref().map(|r| r.required_by.clone());
        let idle = matches!(
            edit,
            RecordEdit::AddRequiredBy(_) | RecordEdit::RemoveRequiredBy(_)
        ) && required_by(&next) == required_by(&record);
        if !idle {
            kept.push(edit.clone());
        }
        record = next;
    }
    Ok(kept)
}

/// `not_managed` for an edit of an addon with no record.
pub fn not_managed(addon: &str, target: &str) -> Error {
    Error::new(
        ErrorCode::NotManaged,
        format!("{addon} is not managed by omaddons on {target}"),
        Suggestion::command(["omaddons", "list", "-t", target]),
    )
}

/// What must still hold under the lock for a records-only change to commit as planned,
/// re-checked offline by `rejudge`. Each edit adds its checks (see [`RecordEdit::guard`]),
/// so several folded edits need all of them: the strictest guard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordGuard {
    /// The addon is still recorded (every edit but an adoption).
    pub recorded: bool,
    /// ...still with this `release_id` (a pin, channel, reason or ref): an edit planned
    /// against one release never lands on another.
    pub release: Option<Option<String>>,
    /// These folders are still absent from AddOns (forget).
    pub absent: Vec<FolderName>,
    /// The adoption's folders are as indexed, and its name is still free.
    pub adopt: bool,
}

/// Why a guard no longer holds. An adoption's failure is always `plan_stale`: a file
/// edited after it was hashed never bumps the generation, and dropping the work would
/// lose the request.
#[derive(Debug, Clone)]
pub struct GuardFailure {
    pub adopt: bool,
    /// A forget's folder is in AddOns again: the forget no longer applies, and the same
    /// command run again would remove that folder (omad-z60g). Not "someone else already
    /// did it", so it is never dropped silently from an unbound run.
    pub back: bool,
    /// The underlying reason, an existing code (`details.cause` of `plan_stale`).
    pub cause: Error,
}

impl RecordGuard {
    /// Both guards' checks.
    #[must_use]
    pub fn and(mut self, other: Self) -> Self {
        self.recorded |= other.recorded;
        self.release = self.release.or(other.release);
        for folder in other.absent {
            if !self.absent.contains(&folder) {
                self.absent.push(folder);
            }
        }
        self.adopt |= other.adopt;
        self
    }

    /// Checks the guard against the addon's current `record`, AddOns as `live` lists it
    /// (under `addons`), and the work's `edits` (an adoption's spec).
    pub fn check(
        &self,
        addon: &str,
        target: &str,
        record: Option<&AddonRecord>,
        live: &Inventory,
        addons: &Path,
        edits: &[RecordEdit],
    ) -> std::result::Result<(), GuardFailure> {
        let fail = |cause: Error| GuardFailure {
            adopt: self.adopt,
            back: false,
            cause,
        };
        if self.adopt {
            for edit in edits {
                if let RecordEdit::Adopt(spec) = edit {
                    check_adopt(spec, target, record, live, addons).map_err(fail)?;
                }
            }
        }
        if self.recorded && record.is_none() {
            return Err(fail(not_managed(addon, target)));
        }
        if let (Some(expected), Some(record)) = (&self.release, record)
            && record.release_id != *expected
        {
            return Err(fail(Error::new(
                ErrorCode::NotFound,
                format!(
                    "{} is no longer at the release this change was planned against (now {})",
                    record.name, record.version
                ),
                Suggestion::command(["omaddons", "list", "-t", target]),
            )));
        }
        // A forget needs every folder gone: the planned ones, and those the record holds
        // now (an update meanwhile may have installed others).
        let now = record
            .filter(|_| !self.absent.is_empty())
            .map(|r| r.folders.as_slice())
            .unwrap_or_default();
        if let Some(back) = self
            .absent
            .iter()
            .chain(now)
            .find(|f| live.folders.iter().any(|e| e.key == f.key()))
        {
            return Err(GuardFailure {
                back: true,
                ..fail(Error::new(
                    ErrorCode::UnmanagedFolder,
                    format!(
                        "{} is in AddOns again, so {addon} is not forgotten",
                        back.as_str()
                    ),
                    Suggestion::command(["omaddons", "list", "-t", target]),
                ))
            });
        }
        Ok(())
    }
}

/// An adoption still holds: its name is free, and every folder is present, unmanaged,
/// a real directory, and holds exactly the indexed files at their indexed size and
/// mtime.
fn check_adopt(
    spec: &AdoptSpec,
    target: &str,
    record: Option<&AddonRecord>,
    live: &Inventory,
    addons: &Path,
) -> std::result::Result<(), Error> {
    let fix = || Suggestion::command(["omaddons", "list", "-t", target, "--unmanaged"]);
    if let Some(taken) = record {
        return Err(Error::new(
            ErrorCode::OwnershipConflict,
            format!("{} is already managed on {target}", taken.name),
            fix(),
        ));
    }
    for (folder, index) in &spec.folders {
        let entry = live.folders.iter().find(|e| e.key == folder.key());
        let class = entry.map(|e| e.class);
        if class != Some(FolderClass::Unmanaged) {
            let (code, why) = match class {
                None => (ErrorCode::NotFound, "is no longer in AddOns"),
                Some(FolderClass::Linked) => (ErrorCode::UnmanagedFolder, "is a symlink"),
                _ => (ErrorCode::OwnershipConflict, "is no longer unmanaged"),
            };
            return Err(Error::new(
                code,
                format!("{} {why}", folder.as_str()),
                fix(),
            ));
        }
        let dir = addons.join(entry.map_or(folder.as_str(), |e| e.name.as_str()));
        let mut found = Vec::new();
        list_files(&dir, "", &mut found);
        let expected: Vec<(&str, u64, i128)> = index
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.size, f.mtime_ns))
            .collect();
        found.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let same = found.len() == expected.len()
            && found
                .iter()
                .zip(&expected)
                .all(|(a, b)| (a.0.as_str(), a.1, a.2) == *b);
        if !same {
            return Err(Error::new(
                ErrorCode::HashMismatch,
                format!(
                    "{} changed after it was indexed for adoption",
                    folder.as_str()
                ),
                fix(),
            ));
        }
    }
    Ok(())
}

/// Every entry under `dir` as `(relative path, size, mtime_ns)`, not following
/// symlinks. Anything but a regular file or directory (a symlink, a device) is listed
/// with size `u64::MAX`, so it never matches an index entry.
fn list_files(dir: &Path, prefix: &str, out: &mut Vec<(String, u64, i128)>) {
    let Ok(listing) = std::fs::read_dir(dir) else {
        out.push((prefix.to_owned(), u64::MAX, 0));
        return;
    };
    for entry in listing.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        match std::fs::symlink_metadata(entry.path()) {
            Ok(m) if m.is_dir() => list_files(&entry.path(), &path, out),
            Ok(m) if m.is_file() => out.push((
                path,
                m.size(),
                i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
            )),
            _ => out.push((path, u64::MAX, 0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AddonState, TxId};

    fn record(name: &str, folders: &[&str]) -> AddonRecord {
        AddonRecord {
            r#ref: "gh:o/foo".parse().unwrap(),
            name: name.into(),
            version: "1.0".into(),
            version_confidence: VersionConfidence::Release,
            release_id: Some("gh:o/foo@1.0".into()),
            channel: Channel::Stable,
            pinned: false,
            reason: AddonReason::Explicit,
            required_by: vec!["Zed".into()],
            folders: folders
                .iter()
                .map(|f| FolderName::new(f).unwrap())
                .collect(),
            state: AddonState::Ok,
            installed_at: "2026-10-01T00:00:00Z".parse().unwrap(),
            tx_id: TxId::new(),
            blob_sha256: None,
            revision: None,
            left_out: Vec::new(),
            any_flavor: false,
            skipped_releases: Vec::new(),
        }
    }

    fn apply(
        r: Option<AddonRecord>,
        edit: RecordEdit,
    ) -> crate::error::Result<Option<AddonRecord>> {
        apply_edit(r, &edit, "Foo", "retail")
    }

    fn adopt(name: &str) -> RecordEdit {
        RecordEdit::Adopt(Box::new(AdoptSpec {
            record: record(name, &["Baz"]),
            folders: vec![(
                FolderName::new("Baz").unwrap(),
                FolderIndex::new("Baz", Vec::new()),
            )],
        }))
    }

    /// A requester another commit added (or dropped) meanwhile is not this edit list's:
    /// history logs only what changes, so undo never takes it back.
    #[test]
    fn a_required_by_edit_made_already_is_not_effective() {
        let mut foo = record("Foo", &["Foo"]);
        foo.required_by = vec!["Bar".into()];
        let edits = [
            RecordEdit::AddRequiredBy("bar".into()),
            RecordEdit::AddRequiredBy("Zed".into()),
            RecordEdit::RemoveRequiredBy("Gone".into()),
            RecordEdit::SetPinned(true),
        ];
        assert_eq!(
            effective_edits(Some(foo), &edits, "Foo", "retail").unwrap(),
            [
                RecordEdit::AddRequiredBy("Zed".into()),
                RecordEdit::SetPinned(true)
            ]
        );
    }

    #[test]
    fn each_edit_sets_its_field() {
        let foo = || Some(record("Foo", &["Foo"]));
        let applied = |edit| apply(foo(), edit).unwrap().unwrap();
        assert!(applied(RecordEdit::SetPinned(true)).pinned);
        assert_eq!(
            applied(RecordEdit::SetChannel(Channel::Beta)).channel,
            Channel::Beta
        );
        assert_eq!(
            applied(RecordEdit::SetReason(AddonReason::Dependency)).reason,
            AddonReason::Dependency
        );
        assert_eq!(
            applied(RecordEdit::AddRequiredBy("bar".into())).required_by,
            ["bar", "Zed"],
            "sorted by addon key"
        );
        assert_eq!(
            applied(RecordEdit::AddRequiredBy("ZED".into())).required_by,
            ["Zed"],
            "de-duplicated by addon key, the first spelling kept"
        );
        assert!(
            applied(RecordEdit::RemoveRequiredBy("zed".into()))
                .required_by
                .is_empty()
        );
        let moved = applied(RecordEdit::Repoint("gh:o/elsewhere".parse().unwrap()));
        assert_eq!(
            (
                moved.r#ref.to_string(),
                moved.release_id,
                moved.version_confidence,
                moved.version.as_str()
            ),
            (
                "gh:o/elsewhere".to_owned(),
                None,
                VersionConfidence::Toc,
                "1.0"
            ),
            "the next update installs from the new ref"
        );
        let mut marked = record("Foo", &["Foo"]);
        marked.skipped_releases = vec![crate::model::SkippedRelease {
            release: crate::model::ReleaseKey::parse("gh:o/r@v2", None),
            version: "v2".into(),
            at: "2026-11-01T00:00:00Z".parse().unwrap(),
            origin: crate::model::SkipOrigin::Undo,
            tx: crate::model::TxId::new(),
        }];
        let repointed = apply(
            Some(marked),
            RecordEdit::Repoint("gh:o/elsewhere".parse().unwrap()),
        )
        .unwrap()
        .unwrap();
        assert!(
            repointed.skipped_releases.is_empty(),
            "marks name the old source's releases: a repoint clears them"
        );
        assert_eq!(apply(foo(), RecordEdit::Forget).unwrap(), None);
    }

    #[test]
    fn edits_need_a_record_and_adoptions_a_free_name() {
        for edit in [
            RecordEdit::SetPinned(true),
            RecordEdit::AddRequiredBy("Bar".into()),
            RecordEdit::Forget,
            RecordEdit::Repoint("gh:o/r".parse().unwrap()),
        ] {
            let error = apply(None, edit.clone()).unwrap_err();
            assert_eq!(error.code, ErrorCode::NotManaged, "{edit:?}");
            assert_eq!(
                error.fix.argv().map(<[String]>::to_vec),
                Some(
                    ["omaddons", "list", "-t", "retail"]
                        .map(String::from)
                        .to_vec()
                )
            );
        }
        let adopted = apply(None, adopt("Baz")).unwrap().unwrap();
        assert_eq!(adopted.name, "Baz");
        let taken = apply(Some(record("Baz", &["Baz"])), adopt("Baz")).unwrap_err();
        assert_eq!(taken.code, ErrorCode::OwnershipConflict);
    }

    #[test]
    fn fields_name_what_changed() {
        let before = record("Foo", &["Foo"]);
        let mut after = before.clone();
        assert!(changed_fields(Some(&before), Some(&after)).is_empty());
        after.pinned = true;
        after.required_by.push("Bar".into());
        after.r#ref = "gh:o/other".parse().unwrap();
        after.channel = Channel::Alpha;
        after.reason = AddonReason::Dependency;
        assert_eq!(
            changed_fields(Some(&before), Some(&after)),
            [
                EditField::Channel,
                EditField::Pinned,
                EditField::Reason,
                EditField::Ref,
                EditField::RequiredBy
            ]
        );
        assert!(changed_fields(None, Some(&after)).is_empty());
        assert!(changed_fields(Some(&before), None).is_empty());
        let wires: Vec<String> = [EditField::RequiredBy, EditField::Ref]
            .iter()
            .map(|f| {
                serde_json::to_value(f)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(wires, ["required_by", "ref"]);
        assert_eq!(EditField::RequiredBy.wire(), "required_by");
    }

    #[test]
    fn each_edit_has_its_guard_and_folding_takes_the_strictest() {
        let foo = record("Foo", &["Foo", "FooOld"]);
        let release = Some(Some("gh:o/foo@1.0".to_owned()));
        let pin = RecordEdit::SetPinned(true).guard(Some(&foo));
        assert_eq!((pin.recorded, &pin.release), (true, &release));
        for edit in [
            RecordEdit::SetChannel(Channel::Beta),
            RecordEdit::SetReason(AddonReason::Dependency),
            RecordEdit::Repoint("gh:o/r".parse().unwrap()),
        ] {
            assert_eq!(edit.guard(Some(&foo)), pin, "{edit:?}");
        }
        let required = RecordEdit::AddRequiredBy("Bar".into()).guard(Some(&foo));
        assert_eq!(
            required,
            RecordGuard {
                recorded: true,
                ..RecordGuard::default()
            },
            "any release"
        );
        assert_eq!(
            RecordEdit::RemoveRequiredBy("Bar".into()).guard(Some(&foo)),
            required
        );
        let forget = RecordEdit::Forget.guard(Some(&foo));
        assert_eq!(forget.absent, foo.folders);
        assert!(adopt("Baz").guard(None).adopt);

        let both = required.clone().and(pin.clone()).and(forget.clone());
        assert_eq!(
            both,
            RecordGuard {
                recorded: true,
                release,
                absent: foo.folders.clone(),
                adopt: false,
            }
        );
        assert_eq!(
            pin.clone().and(required.clone()),
            required.and(pin),
            "order does not matter"
        );
    }

    #[test]
    fn guards_fail_with_an_existing_code_as_the_cause() {
        let dir = tempfile::tempdir().unwrap();
        let empty = Inventory {
            addons_dir_missing: false,
            folders: Vec::new(),
            missing: Vec::new(),
            counts: Default::default(),
            stray_files: Vec::new(),
        };
        let foo = record("Foo", &["Foo"]);
        let check = |guard: &RecordGuard, r: Option<&AddonRecord>, live: &Inventory| {
            guard
                .check("Foo", "retail", r, live, dir.path(), &[])
                .map_err(|f| (f.adopt, f.back, f.cause.code))
        };
        let pin = RecordEdit::SetPinned(true).guard(Some(&foo));
        assert_eq!(check(&pin, Some(&foo), &empty), Ok(()));
        assert_eq!(
            check(&pin, None, &empty),
            Err((false, false, ErrorCode::NotManaged))
        );
        let mut moved = foo.clone();
        moved.release_id = Some("gh:o/foo@2.0".into());
        assert_eq!(
            check(&pin, Some(&moved), &empty),
            Err((false, false, ErrorCode::NotFound)),
            "planned against another release"
        );
        let required = RecordEdit::AddRequiredBy("Bar".into()).guard(Some(&foo));
        assert_eq!(
            check(&required, Some(&moved), &empty),
            Ok(()),
            "any release"
        );

        let forget = RecordEdit::Forget.guard(Some(&foo));
        assert_eq!(check(&forget, Some(&foo), &empty), Ok(()));
        let mut live = empty.clone();
        live.folders.push(crate::inventory::FolderEntry {
            name: "FOO".into(),
            key: "foo".into(),
            class: FolderClass::Unmanaged,
            owners: Vec::new(),
            case_duplicate: false,
            ownership_conflict: false,
            unreadable: false,
            toc: None,
            verdict: None,
            link_target: None,
            owned_on: Vec::new(),
        });
        assert_eq!(
            check(&forget, Some(&foo), &live),
            Err((false, true, ErrorCode::UnmanagedFolder)),
            "a forgotten folder put back by hand"
        );
        // An update meanwhile moved it to a folder that is there: nothing to forget.
        let renamed = record("Foo", &["Foo"]);
        let forget = RecordEdit::Forget.guard(Some(&record("Foo", &["FooOld"])));
        assert_eq!(
            check(&forget, Some(&renamed), &live),
            Err((false, true, ErrorCode::UnmanagedFolder)),
            "the record's folders as they are now"
        );
    }
}
