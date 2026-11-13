//! The typed approval summary (M4-D28) every storable dry run returns and every stored
//! plan keeps: what exactly the run changes (`selection`: per client, each addon's
//! identity, source, channel, release, asset and versions), how far it reaches
//! (`scope`), and `reasons`, a closed set naming each effect that needs a person's eyes.
//! `routine` is true exactly when no reason applies. The bar widget and agents apply a
//! plan without asking only when its selection is what the user chose and it is routine,
//! so the policy lives here, in types: no client reads a message to decide.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::plan::{ChangeKind, Plan, WorkMode};
use crate::error::{Error, Result};
use crate::model::{BackupId, Channel, FolderName, TargetKey, TxId};
use crate::resolve::updates::compare_versions;
use crate::sources::SourceId;
use crate::state::records::TargetRecord;

/// A dry run's approval summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    /// No reason applies.
    pub routine: bool,
    pub reasons: Vec<ApprovalReason>,
    /// Sorted by client id, addon, operation, then transaction or backup.
    pub selection: Vec<ApprovedItem>,
    pub scope: ApprovalScope,
}

/// One thing the run changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApprovedItem {
    /// `null` while the client is unregistered.
    pub target_key: Option<TargetKey>,
    pub target_id: String,
    pub addon: Option<String>,
    pub source: Option<SourceId>,
    /// The addon's channel after the change; `null` when it goes.
    pub channel: Option<Channel>,
    pub release_id: Option<String>,
    /// The downloaded asset's file name; `null` for a local directory, an adoption or a
    /// change to the record alone.
    pub asset: Option<String>,
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    pub op: ApprovedOp,
    /// The record fields an edit changes, sorted; empty for anything else.
    pub fields: Vec<String>,
    /// The transaction an undo reverts.
    pub tx: Option<TxId>,
    /// The backup a restore puts back or a prune deletes.
    pub backup: Option<BackupId>,
}

/// What happens to an item.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ApprovedOp {
    Install,
    Update,
    Remove,
    Adopt,
    Edit,
    Undo,
    Restore,
    Prune,
}

/// How far the run reaches beyond the items themselves. Every list is sorted and holds
/// each name once, whichever clients it comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApprovalScope {
    /// Addons installed because something requires them.
    pub dependencies_added: Vec<String>,
    pub folders_removed: Vec<String>,
    pub local_changes_replaced: LocalChangesScope,
    /// Installed addons that would come from another source.
    pub source_changed: Vec<String>,
    /// The run lifts the running-client policy (`--while-running`).
    pub while_running: bool,
}

/// Files changed by hand in the folders the run replaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalChangesScope {
    pub count: u64,
    pub folders: Vec<String>,
}

/// An effect that needs a person's eyes, in this order.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalReason {
    DependencyAdded,
    FoldersRemoved,
    LocalChangesReplaced,
    SourceChanged,
    ChannelChanged,
    WhileRunning,
    Reinstall,
    Force,
    Downgrade,
    /// `remove`, `undo`, `backup restore` and `backup prune`.
    Destructive,
}

/// What a command's options say about its run (`StoredIntent::approval_flags`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApprovalFlags {
    pub reinstall: bool,
    pub force: bool,
    pub while_running: bool,
    pub destructive: bool,
}

impl Approval {
    /// An engine plan's approval: `install`, `update`, `pin`, `unpin`, `adopt`, `sync`,
    /// `copy` and `remove`. `before` holds each client's record as the run loaded it, by
    /// client id.
    pub fn of_plan(
        plan: &Plan,
        before: &BTreeMap<String, TargetRecord>,
        flags: &ApprovalFlags,
    ) -> Result<Self> {
        let mut selection = Vec::new();
        let mut dependencies = BTreeSet::new();
        let mut removed = BTreeSet::new();
        let mut local = (0u64, BTreeSet::new());
        let mut source_changed = BTreeSet::new();
        let mut channel_changed = false;
        for tp in &plan.targets {
            // Each change is pushed with its work (`PlannedAddon::push`): one addon each.
            if tp.changes.len() != tp.work.len() {
                return Err(Error::internal(format!(
                    "{}'s plan holds {} changes and {} works",
                    tp.target,
                    tp.changes.len(),
                    tp.work.len()
                )));
            }
            for report in tp.dependencies.values() {
                dependencies.extend(report.installs.iter().map(|(_, addon)| addon.clone()));
            }
            let records = before.get(&tp.target);
            for (change, work) in tp.changes.iter().zip(&tp.work) {
                let after = work.record();
                let was =
                    records.and_then(|r| r.addons.get(&TargetRecord::addon_key(&change.addon)));
                removed.extend(change.folders.removed.iter().map(name));
                if let WorkMode::Folders(folders) = &work.mode
                    && let Some(changes) = &folders.local_changes
                {
                    local.0 = local.0.saturating_add(changes.count);
                    local.1.extend(changes.folders.iter().map(name));
                }
                if let Some(was) = was {
                    if was.r#ref != change.r#ref {
                        source_changed.insert(change.addon.clone());
                    }
                    channel_changed |= after.is_some_and(|a| a.channel != was.channel);
                }
                let release_id = after.and_then(|r| r.release_id.clone());
                let asset = release_id
                    .as_ref()
                    .and_then(|id| tp.releases.iter().find(|r| &r.release_id == id))
                    .and_then(|r| r.asset.clone());
                let mut fields: Vec<String> =
                    change.fields.iter().map(|f| f.wire().to_owned()).collect();
                fields.sort_unstable();
                selection.push(ApprovedItem {
                    target_key: tp.key,
                    target_id: tp.target.clone(),
                    addon: Some(change.addon.clone()),
                    source: Some(SourceId::of(&change.r#ref)),
                    channel: after.map(|r| r.channel),
                    release_id,
                    asset,
                    from_version: change.from_version.clone(),
                    to_version: change.to_version.clone(),
                    op: op_of(change.kind),
                    fields,
                    tx: None,
                    backup: None,
                });
            }
        }
        let scope = ApprovalScope {
            dependencies_added: dependencies.into_iter().collect(),
            folders_removed: removed.into_iter().collect(),
            local_changes_replaced: LocalChangesScope {
                count: local.0,
                folders: local.1.into_iter().collect(),
            },
            source_changed: source_changed.into_iter().collect(),
            while_running: flags.while_running,
        };
        Ok(Self::new(selection, scope, channel_changed, flags))
    }

    /// An undo's: one item per transaction it reverts, by (client key, client id,
    /// transaction); `folders_removed`, the folders it takes away.
    pub fn of_undo(
        reverted: &[(Option<TargetKey>, String, TxId)],
        folders_removed: &[FolderName],
        flags: &ApprovalFlags,
    ) -> Self {
        let selection = reverted
            .iter()
            .map(|(key, id, tx)| ApprovedItem {
                tx: Some(*tx),
                ..ApprovedItem::bare(*key, id, ApprovedOp::Undo)
            })
            .collect();
        let removed: BTreeSet<String> = folders_removed.iter().map(name).collect();
        let scope = ApprovalScope {
            folders_removed: removed.into_iter().collect(),
            while_running: flags.while_running,
            ..ApprovalScope::default()
        };
        Self::new(selection, scope, false, &destructive(flags))
    }

    /// A restore's: one item for a whole `WTF/`, else one per `--addon` name.
    pub fn of_restore(
        target_key: Option<TargetKey>,
        target_id: &str,
        backup: BackupId,
        addons: &[String],
    ) -> Self {
        let item = |addon: Option<&String>| ApprovedItem {
            addon: addon.cloned(),
            backup: Some(backup),
            ..ApprovedItem::bare(target_key, target_id, ApprovedOp::Restore)
        };
        let selection = if addons.is_empty() {
            vec![item(None)]
        } else {
            addons.iter().map(|a| item(Some(a))).collect()
        };
        let flags = destructive(&ApprovalFlags::default());
        Self::new(selection, ApprovalScope::default(), false, &flags)
    }

    /// A prune's: one item per backup it deletes, by (client key, client id, backup).
    pub fn of_prune(victims: &[(Option<TargetKey>, String, BackupId)]) -> Self {
        let selection = victims
            .iter()
            .map(|(key, id, backup)| ApprovedItem {
                backup: Some(*backup),
                ..ApprovedItem::bare(*key, id, ApprovedOp::Prune)
            })
            .collect();
        let flags = destructive(&ApprovalFlags::default());
        Self::new(selection, ApprovalScope::default(), false, &flags)
    }

    /// A dry run with nothing to do: no item, and the reasons its options give.
    pub fn nothing(flags: &ApprovalFlags) -> Self {
        let scope = ApprovalScope {
            while_running: flags.while_running,
            ..ApprovalScope::default()
        };
        Self::new(Vec::new(), scope, false, flags)
    }

    /// The approval of `selection` (sorted here) and `scope`, its reasons each raised
    /// exactly when its effect is present.
    fn new(
        mut selection: Vec<ApprovedItem>,
        scope: ApprovalScope,
        channel_changed: bool,
        flags: &ApprovalFlags,
    ) -> Self {
        selection.sort_by(|a, b| {
            (&a.target_id, &a.addon, a.op, a.tx, a.backup).cmp(&(
                &b.target_id,
                &b.addon,
                b.op,
                b.tx,
                b.backup,
            ))
        });
        // A version that is not dotted numbers is never older ("unknown is never
        // older"): a commit or a date never makes an update a downgrade.
        let downgrade = selection.iter().any(|item| {
            item.to_version
                .as_deref()
                .zip(item.from_version.as_deref())
                .is_some_and(|(to, from)| compare_versions(to, from) == Some(Ordering::Less))
        });
        let reasons: Vec<ApprovalReason> = [
            (
                ApprovalReason::DependencyAdded,
                !scope.dependencies_added.is_empty(),
            ),
            (
                ApprovalReason::FoldersRemoved,
                !scope.folders_removed.is_empty(),
            ),
            (
                ApprovalReason::LocalChangesReplaced,
                scope.local_changes_replaced.count > 0,
            ),
            (
                ApprovalReason::SourceChanged,
                !scope.source_changed.is_empty(),
            ),
            (ApprovalReason::ChannelChanged, channel_changed),
            (ApprovalReason::WhileRunning, flags.while_running),
            (ApprovalReason::Reinstall, flags.reinstall),
            (ApprovalReason::Force, flags.force),
            (ApprovalReason::Downgrade, downgrade),
            (ApprovalReason::Destructive, flags.destructive),
        ]
        .into_iter()
        .filter_map(|(reason, present)| present.then_some(reason))
        .collect();
        Self {
            routine: reasons.is_empty(),
            reasons,
            selection,
            scope,
        }
    }
}

impl ApprovedItem {
    /// An item of `op` on a client, naming nothing else yet.
    fn bare(target_key: Option<TargetKey>, target_id: &str, op: ApprovedOp) -> Self {
        Self {
            target_key,
            target_id: target_id.to_owned(),
            addon: None,
            source: None,
            channel: None,
            release_id: None,
            asset: None,
            from_version: None,
            to_version: None,
            op,
            fields: Vec::new(),
            tx: None,
            backup: None,
        }
    }
}

const fn op_of(kind: ChangeKind) -> ApprovedOp {
    match kind {
        ChangeKind::Install => ApprovedOp::Install,
        ChangeKind::Update => ApprovedOp::Update,
        ChangeKind::Remove => ApprovedOp::Remove,
        ChangeKind::Adopt => ApprovedOp::Adopt,
        ChangeKind::Edit => ApprovedOp::Edit,
    }
}

fn name(folder: &FolderName) -> String {
    folder.as_str().to_owned()
}

/// `flags` of a command that is always destructive (`undo`, `backup restore`, `backup
/// prune`).
const fn destructive(flags: &ApprovalFlags) -> ApprovalFlags {
    ApprovalFlags {
        destructive: true,
        ..*flags
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::engine::edit::{EditField, RecordGuard};
    use crate::engine::plan::{
        AddonWork, Change, DepFacts, DepReport, FolderSummary, FolderWork, LocalChanges,
        PlannedRelease, RecordsWork, TargetPlan,
    };
    use crate::model::{
        Actor, AddonReason, AddonRecord, AddonState, ReloadRequired, TxKind, VersionConfidence,
    };

    const RETAIL: &str = "01JB8Q2VA00000000000000002";
    const TX: &str = "01JB8Q2VA00000000000000010";
    const OTHER_TX: &str = "01JB8Q2VA00000000000000011";

    fn key(k: &str) -> TargetKey {
        k.parse().unwrap()
    }

    fn folder(n: &str) -> FolderName {
        FolderName::new(n).unwrap()
    }

    /// `name`'s record from `r` at `version`, released as `release` on `channel`.
    fn record(
        name: &str,
        r: &str,
        version: &str,
        release: Option<&str>,
        channel: Channel,
    ) -> AddonRecord {
        AddonRecord {
            r#ref: r.parse().unwrap(),
            name: name.to_owned(),
            version: version.to_owned(),
            version_confidence: VersionConfidence::Release,
            release_id: release.map(str::to_owned),
            channel,
            pinned: false,
            reason: AddonReason::Explicit,
            required_by: Vec::new(),
            folders: vec![folder(name)],
            state: AddonState::Ok,
            installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
            tx_id: TX.parse().unwrap(),
            blob_sha256: None,
            revision: None,
            left_out: Vec::new(),
            any_flavor: false,
            skipped_releases: Vec::new(),
        }
    }

    fn change(
        addon: &str,
        r: &str,
        kind: ChangeKind,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Change {
        Change {
            addon: addon.to_owned(),
            r#ref: r.parse().unwrap(),
            kind,
            fields: Vec::new(),
            from_version: from.map(str::to_owned),
            to_version: to.map(str::to_owned),
            folders: FolderSummary::default(),
            policy: crate::engine::plan::PolicyDelta::default(),
        }
    }

    /// A staged install, update or remove leaving `after`.
    fn folders_work(
        addon: &str,
        after: Option<AddonRecord>,
        local: Option<LocalChanges>,
    ) -> AddonWork {
        AddonWork {
            addon: addon.to_owned(),
            mode: WorkMode::Folders(Box::new(FolderWork {
                record: after,
                blob: None,
                inspected: None,
                skipped: Vec::new(),
                swap: Vec::new(),
                transfers: Vec::new(),
                degraded: Vec::new(),
                intent: None,
                edits: Vec::new(),
                handed_over: Vec::new(),
                deps: DepFacts::default(),
                local_changes: local,
                archive_warnings: Vec::new(),
                input: None,
            })),
        }
    }

    /// A change to the record alone, from `before` to `after`.
    fn records_work(
        addon: &str,
        before: Option<AddonRecord>,
        after: Option<AddonRecord>,
    ) -> AddonWork {
        AddonWork {
            addon: addon.to_owned(),
            mode: WorkMode::Records(Box::new(RecordsWork {
                before,
                after,
                edits: Vec::new(),
                index_upserts: Vec::new(),
                index_removals: Vec::new(),
                guard: RecordGuard::default(),
            })),
        }
    }

    fn release(id: &str, asset: Option<&str>) -> PlannedRelease {
        PlannedRelease {
            release_id: id.to_owned(),
            url: None,
            expected_hash: None,
            blob_sha256: None,
            deferred: None,
            asset: asset.map(str::to_owned),
        }
    }

    /// One client's plan of `changes` with their works.
    fn target_plan(
        id: &str,
        key: Option<TargetKey>,
        changes: Vec<(Change, AddonWork)>,
        releases: Vec<PlannedRelease>,
        dependencies: BTreeMap<String, DepReport>,
    ) -> TargetPlan {
        let (changes, work) = changes.into_iter().unzip();
        TargetPlan {
            target: id.to_owned(),
            key,
            kind: Some(TxKind::Install),
            changes,
            ops: Vec::new(),
            releases,
            reload_required: ReloadRequired::None,
            warnings: Vec::new(),
            creates_addons_dir: false,
            generation: 1,
            work,
            unchanged_warnings: Vec::new(),
            notes: Vec::new(),
            default_kind: TxKind::Install,
            dependencies,
        }
    }

    fn plan(targets: Vec<TargetPlan>) -> Plan {
        let mut plan = Plan::idle(&[], &[], Actor::Agent);
        plan.targets = targets;
        plan
    }

    /// The records a run loaded: client id → addons.
    fn before(entries: &[(&str, &str, AddonRecord)]) -> BTreeMap<String, TargetRecord> {
        let mut out: BTreeMap<String, TargetRecord> = BTreeMap::new();
        for (id, k, addon) in entries {
            out.entry((*id).to_owned())
                .or_insert_with(|| TargetRecord::empty(key(k)))
                .addons
                .insert(TargetRecord::addon_key(&addon.name), addon.clone());
        }
        out
    }

    const DBM: &str = "gh:DeadlyBossMods/DeadlyBossMods";

    /// An update of DBM on retail, 12.1.14 to 12.1.15, that each row adjusts.
    struct Case {
        was: AddonRecord,
        change: Change,
        after: Option<AddonRecord>,
        local: Option<LocalChanges>,
        dependencies: BTreeMap<String, DepReport>,
        flags: ApprovalFlags,
    }

    impl Case {
        fn plain() -> Self {
            Self {
                was: record("DBM", DBM, "12.1.14", Some("gh:x@12.1.14"), Channel::Stable),
                change: change(
                    "DBM",
                    DBM,
                    ChangeKind::Update,
                    Some("12.1.14"),
                    Some("12.1.15"),
                ),
                after: Some(record(
                    "DBM",
                    DBM,
                    "12.1.15",
                    Some("gh:x@12.1.15"),
                    Channel::Stable,
                )),
                local: None,
                dependencies: BTreeMap::new(),
                flags: ApprovalFlags::default(),
            }
        }

        fn approval(self) -> Approval {
            let work = folders_work("DBM", self.after, self.local);
            let tp = target_plan(
                "retail",
                Some(key(RETAIL)),
                vec![(self.change, work)],
                vec![release("gh:x@12.1.15", Some("DBM-12.1.15.zip"))],
                self.dependencies,
            );
            let before = before(&[("retail", RETAIL, self.was)]);
            let approval = Approval::of_plan(&plan(vec![tp]), &before, &self.flags);
            assert!(approval.is_ok(), "{approval:?}");
            approval.unwrap()
        }
    }

    /// One row: its name, how it changes the plain update, and the reasons it must give.
    type Row = (&'static str, fn(&mut Case), Vec<ApprovalReason>);

    #[test]
    fn each_reason_is_raised_exactly_when_its_effect_is_present() {
        use ApprovalReason as R;
        let rows: Vec<Row> = vec![
            ("plain update", |_| {}, vec![]),
            (
                "dependency added",
                |c| {
                    let report = DepReport {
                        installs: vec![(folder("LibDeflate"), "libdeflate".to_owned())],
                        ..DepReport::default()
                    };
                    c.dependencies.insert("DBM".to_owned(), report);
                },
                vec![R::DependencyAdded],
            ),
            (
                "folder removed",
                |c| c.change.folders.removed = vec![folder("DBM-Old")],
                vec![R::FoldersRemoved],
            ),
            (
                "local changes",
                |c| {
                    c.local = Some(LocalChanges {
                        paths: vec!["DBM-Core/DBM-Core.lua".to_owned()],
                        count: 3,
                        folders: vec![folder("DBM-Core")],
                    });
                },
                vec![R::LocalChangesReplaced],
            ),
            (
                "source changed",
                |c| c.was.r#ref = "gh:someone/DeadlyBossMods".parse().unwrap(),
                vec![R::SourceChanged],
            ),
            (
                "channel changed",
                |c| {
                    if let Some(after) = &mut c.after {
                        after.channel = Channel::Beta;
                    }
                },
                vec![R::ChannelChanged],
            ),
            (
                "while running",
                |c| c.flags.while_running = true,
                vec![R::WhileRunning],
            ),
            (
                "reinstall",
                |c| c.flags.reinstall = true,
                vec![R::Reinstall],
            ),
            ("force", |c| c.flags.force = true, vec![R::Force]),
            (
                "destructive",
                |c| c.flags.destructive = true,
                vec![R::Destructive],
            ),
            (
                "downgrade",
                |c| {
                    c.change.from_version = Some("12.1.15".to_owned());
                    c.change.to_version = Some("12.1.14".to_owned());
                },
                vec![R::Downgrade],
            ),
            (
                "two effects",
                |c| {
                    c.flags.force = true;
                    c.change.to_version = Some("12.1.13".to_owned());
                },
                vec![R::Force, R::Downgrade],
            ),
        ];
        for (row, adjust, expected) in rows {
            let mut case = Case::plain();
            adjust(&mut case);
            let approval = case.approval();
            assert_eq!(
                (&approval.reasons, approval.routine),
                (&expected, expected.is_empty()),
                "{row}: reasons {:?}, expected {expected:?}; {}",
                approval.reasons,
                serde_json::to_string(&approval).unwrap_or_default()
            );
        }
        let mut case = Case::plain();
        case.local = Some(LocalChanges {
            paths: Vec::new(),
            count: 3,
            folders: vec![folder("DBM-Core")],
        });
        assert_eq!(
            case.approval().scope.local_changes_replaced,
            LocalChangesScope {
                count: 3,
                folders: vec!["DBM-Core".to_owned()],
            }
        );
    }

    #[test]
    fn downgrade_ignores_unparseable_versions() {
        for (from, to, downgrade) in [
            ("abc123", "def456", false),
            ("v1", "2024-10-01", false),
            ("1.2", "1.10", false),
            ("1.10", "1.2", true),
        ] {
            let mut case = Case::plain();
            case.change.from_version = Some(from.to_owned());
            case.change.to_version = Some(to.to_owned());
            let reasons = case.approval().reasons;
            assert_eq!(
                reasons.contains(&ApprovalReason::Downgrade),
                downgrade,
                "{from} → {to}: reasons {reasons:?}"
            );
        }
    }

    #[test]
    fn selection_is_sorted_and_carries_release_and_asset() {
        let foo_after = record(
            "Foo",
            "gh:o/foo",
            "v1",
            Some("gh:o/foo@v1"),
            Channel::Stable,
        );
        let bar_was = record("Bar", "gh:o/bar", "2", Some("gh:o/bar@2"), Channel::Stable);
        let mut bar_after = bar_was.clone();
        bar_after.pinned = true;
        let mut pin = change("Bar", "gh:o/bar", ChangeKind::Edit, Some("2"), Some("2"));
        pin.fields = vec![EditField::Pinned];
        let retail = target_plan(
            "retail",
            Some(key(RETAIL)),
            vec![
                (
                    change("Foo", "gh:o/foo", ChangeKind::Install, None, Some("v1")),
                    folders_work("Foo", Some(foo_after), None),
                ),
                (
                    pin,
                    records_work("Bar", Some(bar_was.clone()), Some(bar_after)),
                ),
            ],
            vec![release("gh:o/foo@v1", Some("Foo-v1.zip"))],
            BTreeMap::new(),
        );
        let local = record(
            "Local",
            "file:/w/Local",
            "1",
            Some("file:/w/Local@dir:ab"),
            Channel::Stable,
        );
        let era = target_plan(
            "era",
            None,
            vec![(
                change(
                    "Local",
                    "file:/w/Local",
                    ChangeKind::Install,
                    None,
                    Some("1"),
                ),
                folders_work("Local", Some(local), None),
            )],
            Vec::new(),
            BTreeMap::new(),
        );
        let before = before(&[("retail", RETAIL, bar_was)]);
        let approval =
            Approval::of_plan(&plan(vec![retail, era]), &before, &ApprovalFlags::default())
                .unwrap();
        let shown: Vec<_> = approval
            .selection
            .iter()
            .map(|i| {
                (
                    i.target_id.as_str(),
                    i.addon.as_deref(),
                    i.op,
                    i.asset.as_deref(),
                    i.fields.clone(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("era", Some("Local"), ApprovedOp::Install, None, vec![]),
                (
                    "retail",
                    Some("Bar"),
                    ApprovedOp::Edit,
                    None,
                    vec!["pinned".to_owned()]
                ),
                (
                    "retail",
                    Some("Foo"),
                    ApprovedOp::Install,
                    Some("Foo-v1.zip"),
                    vec![]
                ),
            ],
            "{}",
            serde_json::to_string(&approval).unwrap_or_default()
        );
        let foo = &approval.selection[2];
        assert_eq!(
            (
                foo.target_key,
                foo.source,
                foo.channel,
                foo.release_id.as_deref()
            ),
            (
                Some(key(RETAIL)),
                Some(SourceId::Github),
                Some(Channel::Stable),
                Some("gh:o/foo@v1")
            )
        );
        assert_eq!(
            approval.selection[0].target_key, None,
            "era is unregistered"
        );
        assert_eq!(approval.selection[0].source, Some(SourceId::Local));
        assert!(approval.routine, "{:?}", approval.reasons);
    }

    #[test]
    fn scope_lists_are_sorted_and_unique() {
        let target = |id: &str| {
            let after = record(
                "Little",
                "gh:o/little",
                "1",
                Some("gh:o/little@1"),
                Channel::Stable,
            );
            let mut install = change(
                "Little",
                "gh:o/little",
                ChangeKind::Update,
                Some("0"),
                Some("1"),
            );
            install.folders.removed = vec![folder("Little_Old"), folder("Little_Aaa")];
            let report = DepReport {
                installs: vec![
                    (folder("LibDeflate"), "libdeflate".to_owned()),
                    (folder("BigWigs"), "BigWigs".to_owned()),
                ],
                ..DepReport::default()
            };
            target_plan(
                id,
                None,
                vec![(install, folders_work("Little", Some(after), None))],
                Vec::new(),
                BTreeMap::from([("Little".to_owned(), report)]),
            )
        };
        let approval = Approval::of_plan(
            &plan(vec![target("retail"), target("era")]),
            &BTreeMap::new(),
            &ApprovalFlags::default(),
        )
        .unwrap();
        assert_eq!(
            approval.scope.dependencies_added,
            ["BigWigs", "libdeflate"],
            "once each, sorted"
        );
        assert_eq!(approval.scope.folders_removed, ["Little_Aaa", "Little_Old"]);
        assert_eq!(
            approval.reasons,
            [
                ApprovalReason::DependencyAdded,
                ApprovalReason::FoldersRemoved
            ]
        );
    }

    #[test]
    fn a_plan_whose_changes_and_works_disagree_is_an_internal_error() {
        let mut tp = target_plan("retail", None, Vec::new(), Vec::new(), BTreeMap::new());
        tp.changes.push(change(
            "Foo",
            "gh:o/foo",
            ChangeKind::Install,
            None,
            Some("1"),
        ));
        let approval =
            Approval::of_plan(&plan(vec![tp]), &BTreeMap::new(), &ApprovalFlags::default());
        assert_eq!(
            approval.as_ref().err().map(|e| e.code),
            Some(crate::error::ErrorCode::Internal),
            "{approval:?}"
        );
    }

    #[test]
    fn undo_restore_prune_and_nothing() {
        let tx: TxId = TX.parse().unwrap();
        let other: TxId = OTHER_TX.parse().unwrap();
        let undo = Approval::of_undo(
            &[
                (Some(key(RETAIL)), "retail".to_owned(), other),
                (None, "era".to_owned(), tx),
            ],
            &[folder("Foo"), folder("Bar"), folder("Foo")],
            &ApprovalFlags::default(),
        );
        assert_eq!(
            undo.selection
                .iter()
                .map(|i| (i.target_id.as_str(), i.op, i.tx))
                .collect::<Vec<_>>(),
            [
                ("era", ApprovedOp::Undo, Some(tx)),
                ("retail", ApprovedOp::Undo, Some(other))
            ]
        );
        assert_eq!(undo.scope.folders_removed, ["Bar", "Foo"]);
        assert_eq!(
            undo.reasons,
            [ApprovalReason::FoldersRemoved, ApprovalReason::Destructive]
        );

        let backup: BackupId = "01JB8Q2VA00000000000000012".parse().unwrap();
        let whole = Approval::of_restore(Some(key(RETAIL)), "retail", backup, &[]);
        assert_eq!(whole.selection.len(), 1);
        assert_eq!(
            (
                &whole.selection[0].addon,
                whole.selection[0].op,
                whole.selection[0].backup
            ),
            (&None, ApprovedOp::Restore, Some(backup))
        );
        assert_eq!(whole.reasons, [ApprovalReason::Destructive]);
        let addons = Approval::of_restore(
            Some(key(RETAIL)),
            "retail",
            backup,
            &["WeakAuras".to_owned(), "DBM".to_owned()],
        );
        assert_eq!(
            addons
                .selection
                .iter()
                .map(|i| i.addon.as_deref())
                .collect::<Vec<_>>(),
            [Some("DBM"), Some("WeakAuras")]
        );

        let backups: Vec<BackupId> = ["13", "12", "14"]
            .iter()
            .map(|n| format!("01JB8Q2VA000000000000000{n}").parse().unwrap())
            .collect();
        let victims: Vec<(Option<TargetKey>, String, BackupId)> = backups
            .iter()
            .map(|b| (Some(key(RETAIL)), "retail".to_owned(), *b))
            .collect();
        let prune = Approval::of_prune(&victims);
        let mut sorted = backups.clone();
        sorted.sort_unstable();
        assert_eq!(
            prune
                .selection
                .iter()
                .map(|i| (i.op, i.backup))
                .collect::<Vec<_>>(),
            sorted
                .iter()
                .map(|b| (ApprovedOp::Prune, Some(*b)))
                .collect::<Vec<_>>()
        );
        assert_eq!(prune.reasons, [ApprovalReason::Destructive]);
        assert!(!prune.routine);

        let nothing = Approval::nothing(&ApprovalFlags {
            while_running: true,
            ..ApprovalFlags::default()
        });
        assert_eq!(
            (
                nothing.selection.len(),
                &nothing.reasons,
                nothing.scope.while_running
            ),
            (0, &vec![ApprovalReason::WhileRunning], true)
        );
        assert!(Approval::nothing(&ApprovalFlags::default()).routine);
    }

    #[test]
    fn an_approval_round_trips_and_rejects_unknown_fields() {
        let mut case = Case::plain();
        case.dependencies.insert(
            "DBM".to_owned(),
            DepReport {
                installs: vec![(folder("LibDeflate"), "libdeflate".to_owned())],
                ..DepReport::default()
            },
        );
        let approval = case.approval();
        let value = serde_json::to_value(&approval).unwrap();
        assert_eq!(
            value,
            json!({
                "routine": false, "reasons": ["dependency_added"],
                "selection": [{
                    "target_key": RETAIL, "target_id": "retail", "addon": "DBM",
                    "source": "github", "channel": "stable", "release_id": "gh:x@12.1.15",
                    "asset": "DBM-12.1.15.zip", "from_version": "12.1.14",
                    "to_version": "12.1.15", "op": "update", "fields": [], "tx": null,
                    "backup": null
                }],
                "scope": {
                    "dependencies_added": ["libdeflate"], "folders_removed": [],
                    "local_changes_replaced": {"count": 0, "folders": []},
                    "source_changed": [], "while_running": false
                }
            })
        );
        let back: Approval = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(back, approval);
        type Edit = fn(&mut serde_json::Value);
        let edits: [(&str, Edit); 5] = [
            ("an extra key at the top", |v| v["x"] = json!(1)),
            ("an extra key in an item", |v| {
                v["selection"][0]["x"] = json!(1)
            }),
            ("an extra key in the scope", |v| v["scope"]["x"] = json!(1)),
            ("an extra key in local changes", |v| {
                v["scope"]["local_changes_replaced"]["x"] = json!(1);
            }),
            ("an unknown reason", |v| v["reasons"] = json!(["looks_odd"])),
        ];
        for (edit, apply) in edits {
            let mut v = value.clone();
            apply(&mut v);
            assert!(
                serde_json::from_value::<Approval>(v.clone()).is_err(),
                "{edit}: decoded {v} but must be rejected"
            );
        }
    }
}
