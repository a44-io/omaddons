//! `history -v`'s `items`: what each transaction did to each addon, grouped from its
//! ops, with the addon's version before and after.
//!
//! No persisted format changes for this. The versions come from what the store already
//! keeps:
//!
//! - **Before:** the record the transaction found, which its undo snapshot holds
//!   (`<state>/snapshots/<tx>/<key>.json`, or `<tx>.wtf/`).
//! - **After:** the record it left. That is the record the next transaction on the same
//!   client found (its undo snapshot, taken under the global lock after recovery), or
//!   the live record when the line is its client's newest. Every commit raises a
//!   record's generation by exactly one ([`TargetRecord::bump`]), so the record found
//!   there is believed only when it is exactly one generation past the one the
//!   transaction found. A lost snapshot, an unreadable line between the two, or a
//!   commit a crash left for recovery breaks that, and leaves the side unknown.
//! - **Otherwise:** the stored plan the line names (`plan`), while the cache keeps it.
//!   Its approval lists what the run was bound to do, addon by addon.
//! - **Else** `null`.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use schemars::JsonSchema;
use serde::Serialize;

use crate::app::approval::{Approval, ApprovedItem};
use crate::engine::edit::{EditField, changed_fields};
use crate::engine::retention::state_snapshot_of;
use crate::model::{
    AddonRecord, PlanId, SkipIdentity, SkippedNote, SkippedRelease, TargetKey, TxId, TxKind,
};
use crate::state::history::{FolderOp, HistoryEntry, HistoryOp};
use crate::state::paths::Paths;
use crate::state::plans;
use crate::state::records::TargetRecord;
use crate::state::store::{self, VersionStatus};

/// What one transaction did to one addon (`history -v`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct HistoryItem {
    /// The addon's record name, as the transaction's ops recorded it; `null` for a whole
    /// `WTF/` restore and its undo, which are about no one addon.
    pub addon: Option<String>,
    pub change: HistoryChange,
    /// How many folders its ops name: AddOns folders, the folders whose settings a
    /// restore put back, or `WTF` for a whole restore.
    pub folders: usize,
    /// The addon's version before the change; `null` when it was not installed, or when
    /// neither the undo snapshot nor the stored plan says any more.
    pub from_version: Option<String>,
    /// The addon's version after the change; `null` when it was removed, or not known as
    /// for `from_version`.
    pub to_version: Option<String>,
    /// The skip marks the change added (a `skip`, or an undo that marked the release it
    /// left), by what each selects. Empty when it added none, or when the record before
    /// or after it is not known.
    pub skips_marked: Vec<SkippedNote>,
    /// The skip marks the change cleared (an `unskip`, an install of a named version
    /// they held, the undo of a `skip`), as `skips_marked` decides them.
    pub skips_cleared: Vec<SkippedNote>,
}

/// What a transaction did to an addon, from the transaction's kind, its ops on the
/// addon, and the addon's record before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HistoryChange {
    /// It was not installed before.
    Installed,
    /// Another release, or another source, replaced its folders.
    Updated,
    /// The same release replaced its folders (`install --reinstall`).
    Reinstalled,
    /// Its folders went, or its record alone (a forget, or a takeover).
    Removed,
    /// Its folders came under management as they were.
    Adopted,
    /// Its record alone changed: pinned at its release.
    Pinned,
    /// Its record alone changed: its pin cleared.
    Unpinned,
    /// Its record alone changed: releases to pass over were marked (`skip`).
    SkipMarked,
    /// Its record alone changed: skip marks were cleared (`unskip`).
    SkipCleared,
    /// Its record alone changed: one edit both added and cleared skip marks.
    SkipChanged,
    /// Its record alone changed otherwise: its channel, `--any-flavor`, its ref
    /// (`adopt --ref`), its reason, or what requires it.
    Edited,
    /// A restore put back its settings: a whole `WTF/`, or its SavedVariables.
    SettingsRestored,
    /// An undo reverted what an earlier transaction did to it.
    Reverted,
}

impl HistoryChange {
    /// The words `history -v` prints.
    pub fn words(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Updated => "updated",
            Self::Reinstalled => "reinstalled",
            Self::Removed => "removed",
            Self::Adopted => "adopted",
            Self::Pinned => "pinned",
            Self::Unpinned => "unpinned",
            Self::SkipMarked => "skip marked",
            Self::SkipCleared => "skip cleared",
            Self::SkipChanged => "skips changed",
            Self::Edited => "record edited",
            Self::SettingsRestored => "settings restored",
            Self::Reverted => "reverted",
        }
    }
}

impl HistoryItem {
    /// The item as `history -v` prints it under its line: `DBM: updated 12.1.14 →
    /// 12.1.15 (22 folders)`.
    pub fn human(&self) -> String {
        // A skip names the releases it marked or cleared, not the installed one.
        let marks = |notes: &[SkippedNote]| {
            notes
                .iter()
                .map(|n| n.version.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let versions = match self.change {
            HistoryChange::SkipMarked if !self.skips_marked.is_empty() => {
                format!(" {}", marks(&self.skips_marked))
            }
            HistoryChange::SkipCleared if !self.skips_cleared.is_empty() => {
                format!(" {}", marks(&self.skips_cleared))
            }
            HistoryChange::SkipChanged => format!(
                " {} (cleared {})",
                marks(&self.skips_marked),
                marks(&self.skips_cleared)
            ),
            _ => match (self.from_version.as_deref(), self.to_version.as_deref()) {
                (Some(from), Some(to)) if from != to => format!(" {from} → {to}"),
                (_, Some(version)) | (Some(version), None) => format!(" {version}"),
                (None, None) => String::new(),
            },
        };
        format!(
            "{}: {}{versions} ({})",
            self.addon.as_deref().unwrap_or("WTF/"),
            self.change.words(),
            crate::output::count(self.folders, "folder")
        )
    }
}

/// The records around one transaction on its client; `None` where not known.
#[derive(Debug, Clone, Copy, Default)]
pub struct Around<'a> {
    /// The record the transaction found.
    pub before: Option<&'a TargetRecord>,
    /// The record it left.
    pub after: Option<&'a TargetRecord>,
}

/// `entry`'s items: its ops grouped by addon (an empty name, a whole `WTF/`, is one
/// group of its own), in the order the line recorded them. A side `around` does not know
/// takes `plan`'s versions for the addon on the line's client.
pub fn items(
    entry: &HistoryEntry,
    around: Around<'_>,
    plan: Option<&Approval>,
) -> Vec<HistoryItem> {
    let mut groups: Vec<(Option<String>, Vec<&HistoryOp>)> = Vec::new();
    for op in &entry.ops {
        let key = (!op.addon.is_empty()).then(|| TargetRecord::addon_key(&op.addon));
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, ops)) => ops.push(op),
            None => groups.push((key, vec![op])),
        }
    }
    groups
        .into_iter()
        .filter_map(|(key, ops)| {
            let first = ops.first()?;
            let (before, after) = (
                addon_in(around.before, key.as_deref()),
                addon_in(around.after, key.as_deref()),
            );
            let approved = || approved_item(plan, entry.target, key.as_deref());
            let from_version = match before {
                Some(addon) => addon.map(|a| a.version.clone()),
                None => approved().and_then(|i| i.from_version.clone()),
            };
            let to_version = match after {
                Some(addon) => addon.map(|a| a.version.clone()),
                None => approved().and_then(|i| i.to_version.clone()),
            };
            let mut folders: Vec<&str> = ops.iter().map(|o| o.folder.key()).collect();
            folders.sort_unstable();
            folders.dedup();
            let (skips_marked, skips_cleared) = match (before, after) {
                (Some(Some(b)), Some(Some(a))) => skip_notes(b, a),
                _ => (Vec::new(), Vec::new()),
            };
            Some(HistoryItem {
                addon: key.as_ref().map(|_| first.addon.clone()),
                change: change_of(entry.kind, &ops, before, after),
                folders: folders.len(),
                from_version,
                to_version,
                skips_marked,
                skips_cleared,
            })
        })
        .collect()
}

/// The addon `key` (a record key) in `record`: `None` when the record is not known,
/// `Some(None)` when it holds no such addon (or `key` names none, a whole `WTF/`).
fn addon_in<'r>(
    record: Option<&'r TargetRecord>,
    key: Option<&str>,
) -> Option<Option<&'r AddonRecord>> {
    record.map(|r| key.and_then(|k| r.addons.get(k)))
}

/// The first item of `plan`'s approval about `addon` (a record key) on `target`.
fn approved_item<'a>(
    plan: Option<&'a Approval>,
    target: Option<TargetKey>,
    addon: Option<&str>,
) -> Option<&'a ApprovedItem> {
    let (plan, target, addon) = (plan?, target?, addon?);
    plan.selection.iter().find(|item| {
        item.target_key == Some(target)
            && item
                .addon
                .as_deref()
                .is_some_and(|a| TargetRecord::addon_key(a) == addon)
    })
}

/// What `kind` did to an addon with these `ops` (at least one), whose record was
/// `before` and is `after`: `None` where the record is not known, `Some(None)` where it
/// holds no such addon.
fn change_of(
    kind: TxKind,
    ops: &[&HistoryOp],
    before: Option<Option<&AddonRecord>>,
    after: Option<Option<&AddonRecord>>,
) -> HistoryChange {
    use HistoryChange::*;
    if kind == TxKind::Undo {
        return Reverted;
    }
    if ops.iter().all(|o| !o.op.is_folder()) {
        return SettingsRestored;
    }
    // Only who requires it changed (M2-D35).
    if ops.iter().all(|o| o.edit.is_some()) {
        return Edited;
    }
    let was = before.map(|a| a.is_some());
    let is = after.map(|a| a.is_some());
    match (was, is) {
        (Some(false), Some(true)) if kind == TxKind::Adopt => return Adopted,
        (Some(false), Some(true)) => return Installed,
        (Some(true), Some(false)) => return Removed,
        _ => {}
    }
    if kind == TxKind::Adopt {
        // An adopted folder's addon is new to the record; one already there was
        // re-pointed (`adopt --ref`).
        return if was == Some(true) { Edited } else { Adopted };
    }
    let moves = ops
        .iter()
        .any(|o| matches!(o.op, FolderOp::Add | FolderOp::Replace | FolderOp::Remove));
    if moves {
        if let (Some(Some(b)), Some(Some(a))) = (before, after) {
            return if same_release(b, a) {
                Reinstalled
            } else {
                Updated
            };
        }
        if ops.iter().all(|o| o.op == FolderOp::Add) {
            return Installed;
        }
        if ops.iter().all(|o| o.op == FolderOp::Remove) {
            return Removed;
        }
        return Updated;
    }
    // Records only.
    if kind == TxKind::Remove {
        return Removed;
    }
    let pinned = |side: Option<Option<&AddonRecord>>| side.flatten().map(|a| a.pinned);
    match (pinned(before), pinned(after)) {
        (Some(false), Some(true)) => return Pinned,
        (Some(true), Some(false)) => return Unpinned,
        _ => {}
    }
    // The skip marks, by the set difference of what they select (RG-D16): never the
    // lengths, which one mark removed and another added in one edit leave as they were.
    if let (Some(Some(b)), Some(Some(a))) = (before, after)
        && changed_fields(Some(b), Some(a)) == [EditField::SkippedReleases]
    {
        let (marked, cleared) = skip_notes(b, a);
        return match (marked.is_empty(), cleared.is_empty()) {
            (false, true) => SkipMarked,
            (true, false) => SkipCleared,
            (false, false) => SkipChanged,
            // The same selections, made again: no mark changed what it selects.
            (true, true) => Edited,
        };
    }
    Edited
}

/// The marks `after` adds to `before` and the marks it clears, each by what it selects
/// (a set difference), as notes, sorted.
pub fn skip_notes(
    before: &AddonRecord,
    after: &AddonRecord,
) -> (Vec<SkippedNote>, Vec<SkippedNote>) {
    let only = |of: &AddonRecord, other: &AddonRecord| {
        let theirs: BTreeSet<SkipIdentity> = other
            .skipped_releases
            .iter()
            .map(SkippedRelease::selects)
            .collect();
        let mut notes: Vec<SkippedNote> = of
            .skipped_releases
            .iter()
            .filter(|m| !theirs.contains(&m.selects()))
            .map(|m| SkippedNote::of(&of.name, m))
            .collect();
        notes.sort();
        notes
    };
    (only(after, before), only(before, after))
}

/// Whether `before` and `after` are one release from one source: a reinstall, not an
/// update. A WowAce re-tag keeps its version and release id at another revision.
fn same_release(before: &AddonRecord, after: &AddonRecord) -> bool {
    before.version == after.version
        && before.release_id == after.release_id
        && before.revision == after.revision
        && before.r#ref == after.r#ref
}

/// Whether `entry` wrote its client's record: every line with a client but a backup's.
fn writes_record(entry: &HistoryEntry) -> bool {
    entry.target.is_some() && entry.kind != TxKind::Backup
}

/// Reads what [`items`] needs for the lines `history -v` shows, each file once.
pub struct Reader<'a> {
    paths: &'a Paths,
    /// Each line that wrote its client's record, to the next such line on that client in
    /// commit order. A line absent here is its client's newest.
    next: BTreeMap<TxId, TxId>,
    /// The record each transaction found, from its undo snapshot.
    found: BTreeMap<TxId, Option<Rc<TargetRecord>>>,
    /// Each client's live record.
    live: BTreeMap<TargetKey, Option<Rc<TargetRecord>>>,
    /// Each stored plan's approval.
    plans: BTreeMap<PlanId, Option<Rc<Approval>>>,
}

impl<'a> Reader<'a> {
    /// A reader over `all`: every readable line of the history, in the order it was
    /// written, which is commit order (lines are appended under the global lock).
    pub fn new(paths: &'a Paths, all: &[HistoryEntry]) -> Self {
        let mut next = BTreeMap::new();
        let mut newest: BTreeMap<TargetKey, TxId> = BTreeMap::new();
        for entry in all.iter().filter(|e| writes_record(e)) {
            let Some(key) = entry.target else { continue };
            if let Some(previous) = newest.insert(key, entry.tx)
                && previous != entry.tx
            {
                next.insert(previous, entry.tx);
            }
        }
        Self {
            paths,
            next,
            found: BTreeMap::new(),
            live: BTreeMap::new(),
            plans: BTreeMap::new(),
        }
    }

    /// `entry`'s items, reading only what its ops need.
    pub fn items(&mut self, entry: &HistoryEntry) -> Vec<HistoryItem> {
        if entry.ops.is_empty() {
            return Vec::new();
        }
        let (before, after) = match entry.target {
            Some(key) if writes_record(entry) => {
                let before = self.found(entry.tx, key);
                let after = match self.next.get(&entry.tx).copied() {
                    Some(next) => self.found(next, key),
                    None => self.live(key),
                };
                // The record it left is one commit past the one it found, or unknown.
                let after = match (&before, after) {
                    (Some(b), Some(a)) if b.generation.checked_add(1) == Some(a.generation) => {
                        Some(a)
                    }
                    _ => None,
                };
                (before, after)
            }
            _ => (None, None),
        };
        let plan = match (entry.plan, &before, &after) {
            (Some(id), _, _) if before.is_none() || after.is_none() => self.plan(id),
            _ => None,
        };
        let around = Around {
            before: before.as_deref(),
            after: after.as_deref(),
        };
        items(entry, around, plan.as_deref())
    }

    /// The record `tx` found on `key`'s client: its undo snapshot's copy, an empty record
    /// when the client had none yet, `None` when the snapshot is gone or does not read.
    fn found(&mut self, tx: TxId, key: TargetKey) -> Option<Rc<TargetRecord>> {
        let paths = self.paths;
        self.found
            .entry(tx)
            .or_insert_with(|| {
                let dir = state_snapshot_of(paths, &tx);
                if !dir.is_dir() {
                    return None;
                }
                let name = paths.target_record(&key).file_name()?.to_owned();
                match store::load::<TargetRecord>(&dir.join(name)) {
                    Ok(Some(loaded)) if loaded.status != VersionStatus::Newer => {
                        Some(Rc::new(loaded.value))
                    }
                    // `snapshot_state` copies a record only when there is one.
                    Ok(None) => Some(Rc::new(TargetRecord::empty(key))),
                    _ => None,
                }
            })
            .clone()
    }

    /// `key`'s live record; `None` when it is gone (a purged client) or does not read.
    fn live(&mut self, key: TargetKey) -> Option<Rc<TargetRecord>> {
        let paths = self.paths;
        self.live
            .entry(key)
            .or_insert_with(
                || match store::load::<TargetRecord>(&paths.target_record(&key)) {
                    Ok(Some(loaded)) if loaded.status != VersionStatus::Newer => {
                        Some(Rc::new(loaded.value))
                    }
                    _ => None,
                },
            )
            .clone()
    }

    /// Plan `id`'s approval, while the cache keeps the plan and it reads.
    fn plan(&mut self, id: PlanId) -> Option<Rc<Approval>> {
        let paths = self.paths;
        self.plans
            .entry(id)
            .or_insert_with(|| {
                plans::load_id(paths, &id)
                    .ok()
                    .flatten()
                    .map(|plan| Rc::new(plan.approval))
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Actor, AddonReason, AddonState, Channel, FolderName, VersionConfidence};
    use crate::state::history::{self as history_state, HistoryEdit, TxResult, WtfSource};

    const KEY: &str = "01JB8Q2V9W0000000000000000";
    const TX: &str = "01M9Q00000000000000000000A";

    fn key() -> TargetKey {
        KEY.parse().unwrap()
    }

    fn op(op: FolderOp, folder: &str, addon: &str) -> HistoryOp {
        HistoryOp {
            op,
            folder: FolderName::new(folder).unwrap(),
            addon: addon.into(),
            edit: None,
            backup: None,
            from: None,
            files: None,
        }
    }

    fn entry(kind: TxKind, ops: Vec<HistoryOp>) -> HistoryEntry {
        HistoryEntry {
            schema_version: history_state::CURRENT,
            tx: TX.parse().unwrap(),
            kind,
            at: "2026-11-10T08:00:00Z".parse().unwrap(),
            target: Some(key()),
            targets: Vec::new(),
            actor: Actor::Human,
            reason: None,
            argv: vec!["omaddons".into()],
            ops,
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: None,
            backup: None,
            plan: None,
            doctor_run_id: None,
            repairs: Vec::new(),
        }
    }

    /// A record of `addons`, each `(name, version, pinned)` at release `<name>@<version>`
    /// from `gh:o/<name>`.
    fn record(generation: u64, addons: &[(&str, &str, bool)]) -> TargetRecord {
        let mut record = TargetRecord::empty(key());
        record.generation = generation;
        for (name, version, pinned) in addons {
            let slug: String = name
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_ascii_lowercase();
            let addon = AddonRecord {
                r#ref: format!("gh:o/{slug}").parse().unwrap(),
                name: (*name).into(),
                version: (*version).into(),
                version_confidence: VersionConfidence::Release,
                release_id: Some(format!("{name}@{version}")),
                channel: Channel::Stable,
                pinned: *pinned,
                reason: AddonReason::Explicit,
                required_by: vec![],
                folders: vec![FolderName::new(&slug).unwrap()],
                state: AddonState::Ok,
                installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
                tx_id: "01JB8Q2VA00000000000000000".parse().unwrap(),
                blob_sha256: None,
                revision: None,
                left_out: Vec::new(),
                any_flavor: false,
                skipped_releases: Vec::new(),
            };
            record.addons.insert(TargetRecord::addon_key(name), addon);
        }
        record
    }

    /// A mark on Foo: exact on `tag` when given, else a label mark reading `version`.
    fn mark(tag: Option<&str>, version: &str, tx: &str) -> crate::model::SkippedRelease {
        crate::model::SkippedRelease {
            release: tag
                .and_then(|t| crate::model::ReleaseKey::parse(&format!("gh:o/foo@{t}"), None)),
            version: version.into(),
            at: "2026-11-01T00:00:00Z".parse().unwrap(),
            origin: crate::model::SkipOrigin::Manual,
            tx: tx.parse().unwrap(),
        }
    }

    #[test]
    fn skip_changes_are_a_set_difference_of_what_the_marks_select() {
        const TX_1: &str = "01JB8Q2VA00000000000000001";
        const TX_2: &str = "01JB8Q2VA00000000000000002";
        let base = record(1, &[("Foo", "v1", false)]);
        let foo = |marks: Vec<crate::model::SkippedRelease>| {
            let mut a = base.addons["foo"].clone();
            a.skipped_releases = marks;
            a
        };
        let ops = [op(FolderOp::Record, "foo", "Foo")];
        let ops: Vec<&HistoryOp> = ops.iter().collect();
        // (case, marks before, marks after, the change, how many marked, how many cleared)
        let cases = [
            (
                "one mark added",
                vec![],
                vec![mark(Some("v2"), "v2", TX_1)],
                HistoryChange::SkipMarked,
                1,
                0,
            ),
            (
                "one mark removed",
                vec![mark(Some("v2"), "v2", TX_1)],
                vec![],
                HistoryChange::SkipCleared,
                0,
                1,
            ),
            (
                "one removed and another added in one edit: the lengths are equal",
                vec![mark(Some("v2"), "v2", TX_1)],
                vec![mark(Some("v3"), "v3", TX_2)],
                HistoryChange::SkipChanged,
                1,
                1,
            ),
            (
                "the same selection made again (another time and transaction): no skip change",
                vec![mark(None, "v4", TX_1)],
                vec![mark(None, "4", TX_2)],
                HistoryChange::Edited,
                0,
                0,
            ),
            (
                "a label mark and an exact one for one version are two selections",
                vec![mark(Some("v4"), "v4", TX_1)],
                vec![mark(Some("v4"), "v4", TX_1), mark(None, "v4", TX_2)],
                HistoryChange::SkipMarked,
                1,
                0,
            ),
        ];
        for (case, before, after, change, marked, cleared) in cases {
            let (b, a) = (foo(before), foo(after));
            assert_eq!(
                change_of(TxKind::Skip, &ops, Some(Some(&b)), Some(Some(&a))),
                change,
                "{case}: before={:?} after={:?}",
                b.skipped_releases,
                a.skipped_releases
            );
            let (m, c) = skip_notes(&b, &a);
            assert_eq!((m.len(), c.len()), (marked, cleared), "{case}: {m:?} {c:?}");
        }
    }

    /// One item's `(addon, change, folders, from, to)`, for compact assertions.
    type Row = (
        Option<String>,
        HistoryChange,
        usize,
        Option<String>,
        Option<String>,
    );

    fn rows(items: &[HistoryItem]) -> Vec<Row> {
        items
            .iter()
            .map(|i| {
                (
                    i.addon.clone(),
                    i.change,
                    i.folders,
                    i.from_version.clone(),
                    i.to_version.clone(),
                )
            })
            .collect()
    }

    /// One case: its name, the transaction's kind and ops, the records before and after,
    /// and the rows expected.
    type Case = (
        &'static str,
        TxKind,
        Vec<HistoryOp>,
        TargetRecord,
        TargetRecord,
        Vec<Row>,
    );

    fn row(addon: &str, change: HistoryChange, folders: usize, from: &str, to: &str) -> Row {
        let version = |v: &str| (!v.is_empty()).then(|| v.to_owned());
        (
            (!addon.is_empty()).then(|| addon.to_owned()),
            change,
            folders,
            version(from),
            version(to),
        )
    }

    #[test]
    fn each_kind_of_change_is_named_with_both_versions() {
        use HistoryChange::*;
        let edit = |o: HistoryOp, e| HistoryOp { edit: Some(e), ..o };
        let settings = |o: FolderOp, folder: &str, addon: &str| HistoryOp {
            from: Some(WtfSource::Backup),
            files: Some(3),
            ..op(o, folder, addon)
        };
        let cases: Vec<Case> = vec![
            (
                "install with a dependency",
                TxKind::Install,
                vec![
                    op(FolderOp::Add, "LittleWigs", "LittleWigs"),
                    op(FolderOp::Add, "BigWigs", "BigWigs"),
                    op(FolderOp::Add, "BigWigs_Core", "BigWigs"),
                ],
                record(0, &[]),
                record(1, &[("LittleWigs", "v1", false), ("BigWigs", "v2", false)]),
                vec![
                    row("LittleWigs", Installed, 1, "", "v1"),
                    row("BigWigs", Installed, 2, "", "v2"),
                ],
            ),
            (
                "update",
                TxKind::Update,
                vec![op(FolderOp::Replace, "DBM-Core", "DBM")],
                record(4, &[("DBM", "12.1.14", false)]),
                record(5, &[("DBM", "12.1.15", false)]),
                vec![row("DBM", Updated, 1, "12.1.14", "12.1.15")],
            ),
            (
                "an update that drops a folder",
                TxKind::Update,
                vec![
                    op(FolderOp::Replace, "DBM-Core", "DBM"),
                    op(FolderOp::Remove, "DBM-Old", "DBM"),
                ],
                record(4, &[("DBM", "1", false)]),
                record(5, &[("DBM", "2", false)]),
                vec![row("DBM", Updated, 2, "1", "2")],
            ),
            (
                "reinstall",
                TxKind::Update,
                vec![op(FolderOp::Replace, "Plater", "Plater")],
                record(2, &[("Plater", "v661", false)]),
                record(3, &[("Plater", "v661", false)]),
                vec![row("Plater", Reinstalled, 1, "v661", "v661")],
            ),
            (
                "remove, and a requester dropped",
                TxKind::Remove,
                vec![
                    op(FolderOp::Remove, "LittleWigs", "LittleWigs"),
                    edit(
                        op(FolderOp::Record, "BigWigs", "BigWigs"),
                        HistoryEdit::RequiredByRemove("LittleWigs".into()),
                    ),
                ],
                record(6, &[("LittleWigs", "v1", false), ("BigWigs", "v2", false)]),
                record(7, &[("BigWigs", "v2", false)]),
                vec![
                    row("LittleWigs", Removed, 1, "v1", ""),
                    row("BigWigs", Edited, 1, "v2", "v2"),
                ],
            ),
            (
                "a forget: folders already gone",
                TxKind::Remove,
                vec![op(FolderOp::Record, "Foo", "Foo")],
                record(1, &[("Foo", "1", false)]),
                record(2, &[]),
                vec![row("Foo", Removed, 1, "1", "")],
            ),
            (
                "adopt",
                TxKind::Adopt,
                vec![
                    op(FolderOp::Record, "Bagnon", "Bagnon"),
                    op(FolderOp::Record, "Bagnon_Config", "Bagnon"),
                ],
                record(1, &[]),
                record(2, &[("Bagnon", "10.2", false)]),
                vec![row("Bagnon", Adopted, 2, "", "10.2")],
            ),
            (
                "adopt --ref re-points a managed addon",
                TxKind::Adopt,
                vec![op(FolderOp::Record, "Bagnon", "Bagnon")],
                record(2, &[("Bagnon", "10.2", false)]),
                record(3, &[("Bagnon", "10.2", false)]),
                vec![row("Bagnon", Edited, 1, "10.2", "10.2")],
            ),
            (
                "pin",
                TxKind::Pin,
                vec![op(FolderOp::Record, "Questie", "Questie")],
                record(3, &[("Questie", "1.0", false)]),
                record(4, &[("Questie", "1.0", true)]),
                vec![row("Questie", Pinned, 1, "1.0", "1.0")],
            ),
            (
                "unpin",
                TxKind::Pin,
                vec![op(FolderOp::Record, "Questie", "Questie")],
                record(4, &[("Questie", "1.0", true)]),
                record(5, &[("Questie", "1.0", false)]),
                vec![row("Questie", Unpinned, 1, "1.0", "1.0")],
            ),
            (
                "a channel switch, records only",
                TxKind::Install,
                vec![op(FolderOp::Record, "WeakAuras", "WeakAuras")],
                record(1, &[("WeakAuras", "5", false)]),
                record(2, &[("WeakAuras", "5", false)]),
                vec![row("WeakAuras", Edited, 1, "5", "5")],
            ),
            (
                "a takeover forgets the old addon",
                TxKind::Install,
                vec![
                    op(FolderOp::Replace, "Bar", "Bar"),
                    op(FolderOp::Record, "Bar", "Bar (old)"),
                ],
                record(1, &[("Bar (old)", "2.1", false)]),
                record(2, &[("Bar", "2.2", false)]),
                vec![
                    row("Bar", Installed, 1, "", "2.2"),
                    row("Bar (old)", Removed, 1, "2.1", ""),
                ],
            ),
            (
                "a whole restore",
                TxKind::Restore,
                vec![settings(FolderOp::Wtf, "WTF", "")],
                record(1, &[("Foo", "1", false)]),
                record(2, &[("Foo", "1", false)]),
                vec![row("", SettingsRestored, 1, "", "")],
            ),
            (
                "one addon's settings",
                TxKind::Restore,
                vec![
                    settings(FolderOp::SavedVariables, "WeakAuras", "WeakAuras"),
                    settings(FolderOp::SavedVariables, "WeakAurasOptions", "WeakAuras"),
                ],
                record(1, &[("WeakAuras", "5.19.2", false)]),
                record(2, &[("WeakAuras", "5.19.2", false)]),
                vec![row("WeakAuras", SettingsRestored, 2, "5.19.2", "5.19.2")],
            ),
            (
                "the undo of an install",
                TxKind::Undo,
                vec![op(FolderOp::Remove, "Bar", "Bar")],
                record(2, &[("Bar", "2.1.0", false)]),
                record(3, &[]),
                vec![row("Bar", Reverted, 1, "2.1.0", "")],
            ),
            (
                "the undo of a whole restore",
                TxKind::Undo,
                vec![HistoryOp {
                    from: Some(WtfSource::Snapshot),
                    ..op(FolderOp::Wtf, "WTF", "")
                }],
                record(2, &[]),
                record(3, &[]),
                vec![row("", Reverted, 1, "", "")],
            ),
        ];
        for (name, kind, ops, before, after, expected) in cases {
            let e = entry(kind, ops);
            let around = Around {
                before: Some(&before),
                after: Some(&after),
            };
            assert_eq!(rows(&items(&e, around, None)), expected, "{name}: {e:?}");
        }
    }

    #[test]
    fn unknown_records_fall_back_on_the_ops_and_leave_versions_null() {
        use HistoryChange::*;
        let cases = [
            ("adds", TxKind::Install, FolderOp::Add, Installed),
            ("replaces", TxKind::Update, FolderOp::Replace, Updated),
            ("removes", TxKind::Remove, FolderOp::Remove, Removed),
            (
                "records on a remove",
                TxKind::Remove,
                FolderOp::Record,
                Removed,
            ),
            ("records on a pin", TxKind::Pin, FolderOp::Record, Edited),
            (
                "records on an adopt",
                TxKind::Adopt,
                FolderOp::Record,
                Adopted,
            ),
            ("an undo", TxKind::Undo, FolderOp::Replace, Reverted),
        ];
        for (name, kind, folder_op, expected) in cases {
            let e = entry(kind, vec![op(folder_op, "Foo", "Foo")]);
            assert_eq!(
                rows(&items(&e, Around::default(), None)),
                vec![row("Foo", expected, 1, "", "")],
                "{name}"
            );
        }
    }

    #[test]
    fn the_plan_gives_the_versions_the_records_do_not() {
        let e = entry(
            TxKind::Update,
            vec![
                op(FolderOp::Replace, "DBM-Core", "DBM"),
                op(FolderOp::Replace, "Plater", "Plater"),
            ],
        );
        let approved = |target: Option<TargetKey>, addon: &str, from: &str, to: &str| {
            serde_json::from_value::<ApprovedItem>(serde_json::json!({
                "target_key": target,
                "target_id": "retail",
                "addon": addon,
                "source": "github",
                "channel": "stable",
                "release_id": null,
                "asset": null,
                "from_version": from,
                "to_version": to,
                "op": "update",
                "fields": [],
                "tx": null,
                "backup": null,
            }))
            .unwrap()
        };
        let other: TargetKey = "01JB8Q2V9W0000000000000001".parse().unwrap();
        let plan = Approval {
            routine: true,
            reasons: Vec::new(),
            selection: vec![
                approved(Some(other), "DBM", "0", "0"),
                approved(Some(key()), "dbm", "12.1.14", "12.1.15"),
            ],
            scope: crate::app::approval::ApprovalScope::default(),
        };
        let before = record(4, &[("DBM", "12.1.14", false), ("Plater", "v661", false)]);
        let cases = [
            (
                "no record: both from the plan, matched by client and addon key",
                Around::default(),
                vec![
                    row("DBM", HistoryChange::Updated, 1, "12.1.14", "12.1.15"),
                    row("Plater", HistoryChange::Updated, 1, "", ""),
                ],
            ),
            (
                "the record before only: the plan gives the after",
                Around {
                    before: Some(&before),
                    after: None,
                },
                vec![
                    row("DBM", HistoryChange::Updated, 1, "12.1.14", "12.1.15"),
                    row("Plater", HistoryChange::Updated, 1, "v661", ""),
                ],
            ),
        ];
        for (name, around, expected) in cases {
            assert_eq!(rows(&items(&e, around, Some(&plan))), expected, "{name}");
        }
    }

    #[test]
    fn lines_without_ops_have_no_items() {
        for kind in [TxKind::Repair, TxKind::Backup, TxKind::Undo] {
            let e = HistoryEntry {
                target: None,
                ..entry(kind, Vec::new())
            };
            assert_eq!(items(&e, Around::default(), None), Vec::new(), "{kind:?}");
        }
    }

    #[test]
    fn human_lines_name_the_change_and_its_versions() {
        let item = |addon: Option<&str>, change, folders, from: Option<&str>, to: Option<&str>| {
            HistoryItem {
                addon: addon.map(str::to_owned),
                change,
                folders,
                from_version: from.map(str::to_owned),
                to_version: to.map(str::to_owned),
                skips_marked: Vec::new(),
                skips_cleared: Vec::new(),
            }
        };
        let cases = [
            (
                item(
                    Some("DBM"),
                    HistoryChange::Updated,
                    22,
                    Some("1"),
                    Some("2"),
                ),
                "DBM: updated 1 → 2 (22 folders)",
            ),
            (
                item(
                    Some("Bar"),
                    HistoryChange::Installed,
                    1,
                    None,
                    Some("2.1.0"),
                ),
                "Bar: installed 2.1.0 (1 folder)",
            ),
            (
                item(Some("Bar"), HistoryChange::Removed, 1, Some("2.1.0"), None),
                "Bar: removed 2.1.0 (1 folder)",
            ),
            (
                item(
                    Some("Q"),
                    HistoryChange::Pinned,
                    1,
                    Some("1.0"),
                    Some("1.0"),
                ),
                "Q: pinned 1.0 (1 folder)",
            ),
            (
                item(None, HistoryChange::SettingsRestored, 1, None, None),
                "WTF/: settings restored (1 folder)",
            ),
            (
                item(Some("Foo"), HistoryChange::Edited, 2, None, None),
                "Foo: record edited (2 folders)",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(input.human(), expected, "{input:?}");
        }
    }

    #[test]
    fn every_change_word_is_its_wire_name_in_words() {
        use HistoryChange::*;
        for change in [
            Installed,
            Updated,
            Reinstalled,
            Removed,
            Adopted,
            Pinned,
            Unpinned,
            Edited,
            SettingsRestored,
            Reverted,
        ] {
            let wire = serde_json::to_value(change).unwrap();
            let wire = wire.as_str().unwrap().replace('_', " ");
            let words = change.words();
            assert!(
                words == wire || (change == Edited && words == "record edited"),
                "{change:?}: {words:?} against {wire:?}"
            );
        }
    }
}
