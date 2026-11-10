//! `status`' view of a client's backups (M3-D37, M3-D30, M3-D31, M3-D48): how protected
//! its `WTF/` is, whether automatic backups work there, and the space left. Every number
//! but the free space comes from the inventory projection, validated by one listing of
//! `<meta>/backups/` and one of `<meta>/snapshots/`: no `backup.json` is read and no tree
//! walked, so `status --cached` stays within its budget however many backups there are.
//! A stale projection is reported as such and never rebuilt here; `status` writes nothing.
//!
//! Beside them, the client's undo snapshots (`undo_snapshots`): counted by retention's own
//! rule from the history plain `status` reads, with one `stat` per transaction, and sized
//! only under `-v`, by a walk bounded by `UNDO_SNAPSHOT_SIZE_MAX_ENTRIES`.

use std::collections::BTreeSet;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::backup::{AutoView, size, when};
use crate::backup::fs::{CloneHint, clone_hint, free_bytes};
use crate::backup::inventory::{self, Capability, Inventory, InventoryFile, Latest, Qualifying};
use crate::backup::{self, BackupKind, Trigger};
use crate::contract::actions::Action;
use crate::engine::retention::{self, RetentionLimits, Sizes};
use crate::error::CommandHint;
use crate::limits::{BACKUP_STALE_AFTER, UNDO_SNAPSHOT_SIZE_MAX_ENTRIES};
use crate::model::{TargetKey, TxId};
use crate::state::config::AutoBackup;
use crate::state::history::HistoryEntry;
use crate::state::paths::{ClientPaths, Paths};

/// One client's backups at a glance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct BackupSummary {
    /// Backups of this client, from the listing of `<meta>/backups/`.
    pub count: u64,
    /// The listing differs from what the projection was built from, or there is no
    /// projection for backups that exist: the other numbers may be out of date, and
    /// `backup list` refreshes them.
    pub stale: bool,
    /// The newest backup.
    pub latest: Option<Latest>,
    /// The newest capture that counts as a checkpoint (M3-D31).
    pub latest_qualifying: Option<Qualifying>,
    /// The configured mode, and what automatic attempts observed (M3-D30).
    pub auto: AutoView,
    /// The backups' apparent size; `null` without a projection.
    pub bytes: Option<u64>,
    /// The apparent size of undo's `WTF.previous` trees; `null` without a projection.
    pub snapshot_bytes: Option<u64>,
    /// Free to unprivileged users on the filesystem holding the backups (M3-D48).
    pub free_bytes: u64,
    /// `backup create` is due: `WTF/` holds SavedVariables and no qualifying capture
    /// from the last [`BACKUP_STALE_AFTER`] is known.
    #[serde(skip)]
    pub due: bool,
}

/// `client`'s summary from `file`, the projection read once for every target. `mode`
/// is the configured `auto_backup`.
pub fn summary(
    file: &InventoryFile,
    client: &ClientPaths,
    key: Option<&TargetKey>,
    mode: AutoBackup,
    now: Timestamp,
) -> BackupSummary {
    let (count, stale, projection) = match inventory::validate(file, client, key) {
        Inventory::Fresh(p) => (p.covers.backups.len() as u64, false, Some(p)),
        Inventory::Stale(p, count) => (count, true, Some(p)),
        // Nothing to refresh where nothing was ever backed up.
        Inventory::Missing(count) => (count, count > 0, None),
    };
    let auto = projection
        .as_ref()
        .map(|p| p.auto.clone())
        .unwrap_or_default();
    // Never tried: what the filesystem says it can do (M3-D30).
    let capability = match auto.capability {
        Capability::Unknown if matches!(clone_hint(&client.backups()), CloneHint::Incapable) => {
            Capability::Unsupported
        }
        observed => observed,
    };
    let latest_qualifying = projection
        .as_ref()
        .and_then(|p| p.latest_qualifying.clone());
    // A projection knows the newest checkpoint, and an empty listing knows there is
    // none; backups without a projection are refreshed first.
    let known = projection.is_some() || count == 0;
    let due = known
        && checkpoint_old(latest_qualifying.as_ref(), now)
        && backup::has_saved_variables(&client.wtf());
    BackupSummary {
        count,
        stale,
        latest: projection.as_ref().and_then(|p| p.latest.clone()),
        latest_qualifying,
        auto: AutoView {
            mode,
            capability,
            last: auto.last,
        },
        bytes: projection.as_ref().map(|p| p.bytes),
        snapshot_bytes: projection.as_ref().map(|p| p.snapshot_bytes),
        free_bytes: free_bytes(&client.backups()),
        due,
    }
}

/// A client's undo snapshots, as retention counts them (`status`'s `undo_snapshots`):
/// every transaction that touched the client and still has a snapshot half on disk, its
/// client side (`Interface/.omaddons/snapshots/`) or its state side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct UndoSnapshots {
    /// The transactions whose undo snapshots are kept.
    pub count: u64,
    /// When the oldest of them committed; `null` when there is none.
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub oldest_at: Option<Timestamp>,
    /// How many of them retention would prune now by its count and age rules (the size
    /// cap needs every snapshot's size, which `status` never measures for this).
    pub prunable: u64,
    /// Their apparent size, both halves, a restore's `WTF.previous` included; `null`
    /// without `-v`.
    pub bytes: Option<u64>,
    /// The `-v` walk stopped at `UNDO_SNAPSHOT_SIZE_MAX_ENTRIES` entries, so `bytes` is a
    /// lower bound.
    pub truncated: bool,
}

/// `client`'s undo snapshots at `now`, from the whole history `entries` (oldest first)
/// and `pinned` ([`retention::pinned_by`] of them), by retention's own rule; sized, within
/// [`UNDO_SNAPSHOT_SIZE_MAX_ENTRIES`], only when `measure`.
#[allow(clippy::too_many_arguments)]
pub fn undo_snapshots(
    paths: &Paths,
    client: &ClientPaths,
    key: TargetKey,
    entries: &[HistoryEntry],
    pinned: &BTreeSet<TxId>,
    limits: &RetentionLimits,
    now: Timestamp,
    measure: bool,
) -> UndoSnapshots {
    let judged = retention::judge(
        paths,
        client,
        key,
        entries,
        pinned,
        limits,
        now,
        Sizes::Unmeasured,
    );
    let (bytes, truncated) = if measure {
        let txs: Vec<TxId> = judged.txs.iter().map(|t| t.tx).collect();
        let (bytes, complete) =
            retention::snapshot_bytes(paths, client, &txs, UNDO_SNAPSHOT_SIZE_MAX_ENTRIES);
        (Some(bytes), !complete)
    } else {
        (None, false)
    };
    UndoSnapshots {
        count: judged.txs.len() as u64,
        oldest_at: judged.txs.iter().map(|t| t.at).min(),
        prunable: judged.selection.prune.len() as u64,
        bytes,
        truncated,
    }
}

/// What the backups line adds for the undo snapshots: with their size (`-v`),
/// ` · undo: 30 snapshots, 891 MB, oldest 13 days`; else ` · undo: 30 snapshots (over
/// policy)` only when retention would prune some, and nothing otherwise.
pub fn undo_phrase(u: &UndoSnapshots, now: Timestamp) -> Option<String> {
    let snapshots =
        crate::output::count(usize::try_from(u.count).unwrap_or(usize::MAX), "snapshot");
    let over = if u.prunable > 0 { " (over policy)" } else { "" };
    let Some(bytes) = u.bytes else {
        return (u.prunable > 0).then(|| format!(" · undo: {snapshots}{over}"));
    };
    let mut phrase = format!(" · undo: {snapshots}");
    if u.count > 0 {
        let at_least = if u.truncated { "at least " } else { "" };
        phrase.push_str(&format!(", {at_least}{}", size(bytes)));
    }
    if let Some(oldest) = u.oldest_at {
        let days = now.as_second().saturating_sub(oldest.as_second()) / 86_400;
        phrase.push_str(&match days {
            0 => ", oldest under a day".to_owned(),
            1 => ", oldest 1 day".to_owned(),
            n => format!(", oldest {n} days"),
        });
    }
    phrase.push_str(over);
    Some(phrase)
}

/// Whether `latest` is missing or captured [`BACKUP_STALE_AFTER`] or longer before `now`
/// (its capture time, never when it was published or imported).
fn checkpoint_old(latest: Option<&Qualifying>, now: Timestamp) -> bool {
    let window = i64::try_from(BACKUP_STALE_AFTER.as_secs()).unwrap_or(i64::MAX);
    latest.is_none_or(|q| now.as_second().saturating_sub(q.captured_at.as_second()) >= window)
}

/// The commands `id`'s summary recommends: `backup list` to refresh a stale projection,
/// `backup create` when a checkpoint is due, whatever the automatic mode.
pub fn hints(id: &str, s: &BackupSummary) -> Vec<CommandHint> {
    let mut hints = Vec::new();
    if s.stale {
        hints.push(CommandHint::new(
            Action::RefreshBackups,
            ["omaddons", "backup", "list", "-t", id],
        ));
    }
    if s.due {
        hints.push(CommandHint::new(
            Action::BackupCreate,
            ["omaddons", "backup", "create", "-t", id],
        ));
    }
    hints
}

/// The human line under a client's row: `  backups: 5 (newest today 18:02, after play)
/// · auto: daily, working · 212 GB free`. It names no account, realm or character.
pub fn human_line(s: &BackupSummary, now: Timestamp) -> String {
    let mut parts = vec![match (&s.latest, s.count) {
        (_, 0) => "backups: none".to_owned(),
        (Some(latest), n) => format!(
            "backups: {n} (newest {}, {})",
            when(latest.captured_at, now),
            kind_label(latest.kind, latest.trigger)
        ),
        (None, n) => format!("backups: {n}"),
    }];
    if s.stale {
        parts.push("details out of date".to_owned());
    }
    parts.push(auto_phrase(&s.auto));
    parts.push(format!("{} free", size(s.free_bytes)));
    format!("  {}", parts.join(" · "))
}

/// A backup's kind, as the line names the newest.
fn kind_label(kind: BackupKind, trigger: Option<Trigger>) -> &'static str {
    match (kind, trigger) {
        (BackupKind::Auto, Some(Trigger::PostSession)) => "after play",
        (BackupKind::Auto, Some(Trigger::PreChange)) => "before a change",
        (BackupKind::Auto, None) => "automatic",
        (BackupKind::Manual, _) => "manual",
        (BackupKind::BeforeRestore, _) => "before a restore",
        (BackupKind::Imported, _) => "imported",
        (BackupKind::Recovered, _) => "recovered",
    }
}

/// `auto: daily, working`, or what keeps automatic backups from working here.
fn auto_phrase(auto: &AutoView) -> String {
    let mode = match auto.mode {
        AutoBackup::Off => return "auto: off".to_owned(),
        AutoBackup::Daily => "daily",
        AutoBackup::DailyCopy => "daily-copy",
    };
    if matches!(auto.mode, AutoBackup::Daily) && matches!(auto.capability, Capability::Unsupported)
    {
        return "auto: daily, but this filesystem cannot clone; use daily-copy or back up by hand"
            .to_owned();
    }
    let state = match auto.last.as_ref().map(|l| l.result.as_str()) {
        Some("created" | "unchanged" | "done_today" | "running" | "in_progress") => "working",
        Some("space_low") => "but space ran low (limits.backup_min_free_mb)",
        Some("no_reflink") => "but this filesystem cannot clone; use daily-copy or back up by hand",
        Some("failed") => "but the last one failed",
        Some("no_wtf") => "no WTF/ yet",
        _ => "not tried yet",
    };
    format!("auto: {mode}, {state}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::inventory::AutoLast;
    use crate::model::BackupId;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn view(mode: AutoBackup, capability: Capability, result: Option<&str>) -> AutoView {
        AutoView {
            mode,
            capability,
            last: result.map(|r| AutoLast {
                at: at("2026-10-15T18:00:00Z"),
                result: r.to_owned(),
            }),
        }
    }

    fn summary_of(count: u64, latest: Option<Latest>, stale: bool) -> BackupSummary {
        BackupSummary {
            count,
            stale,
            latest,
            latest_qualifying: None,
            auto: view(AutoBackup::Daily, Capability::Supported, Some("created")),
            bytes: Some(63_963_136),
            snapshot_bytes: Some(0),
            free_bytes: 212_000_000_000,
            due: false,
        }
    }

    #[test]
    fn a_checkpoint_is_old_from_seven_days_on_or_when_there_is_none() {
        let now = at("2026-10-15T18:00:00Z");
        let q = |captured: &str| Qualifying {
            id: BackupId::new(),
            captured_at: at(captured),
        };
        assert!(checkpoint_old(None, now));
        assert!(!checkpoint_old(Some(&q("2026-10-08T18:00:01Z")), now));
        assert!(checkpoint_old(Some(&q("2026-10-08T18:00:00Z")), now));
        assert!(checkpoint_old(Some(&q("2026-09-01T00:00:00Z")), now));
    }

    #[test]
    fn the_auto_phrase_says_what_keeps_backups_from_working() {
        let cases = [
            (
                view(AutoBackup::Off, Capability::Supported, None),
                "auto: off",
            ),
            (
                view(AutoBackup::Daily, Capability::Supported, Some("created")),
                "auto: daily, working",
            ),
            (
                view(AutoBackup::Daily, Capability::Unsupported, None),
                "auto: daily, but this filesystem cannot clone; use daily-copy or back up by hand",
            ),
            (
                view(
                    AutoBackup::DailyCopy,
                    Capability::Unsupported,
                    Some("created"),
                ),
                "auto: daily-copy, working",
            ),
            (
                view(AutoBackup::Daily, Capability::Supported, Some("space_low")),
                "auto: daily, but space ran low (limits.backup_min_free_mb)",
            ),
            (
                view(AutoBackup::DailyCopy, Capability::Unknown, Some("failed")),
                "auto: daily-copy, but the last one failed",
            ),
            (
                view(AutoBackup::DailyCopy, Capability::Unknown, None),
                "auto: daily-copy, not tried yet",
            ),
        ];
        for (auto, expected) in cases {
            assert_eq!(auto_phrase(&auto), expected, "{auto:?}");
        }
    }

    #[test]
    fn the_line_names_the_newest_and_says_when_it_is_out_of_date() {
        let now = at("2026-10-15T18:30:00Z");
        let latest = Latest {
            id: BackupId::new(),
            captured_at: at("2026-10-15T18:02:00Z"),
            kind: BackupKind::Auto,
            trigger: Some(Trigger::PostSession),
        };
        let line = human_line(&summary_of(5, Some(latest), false), now);
        assert!(line.starts_with("  backups: 5 (newest "), "{line}");
        assert!(
            line.ends_with(", after play) · auto: daily, working · 212 GB free"),
            "{line}"
        );
        assert_eq!(
            human_line(&summary_of(0, None, false), now),
            "  backups: none · auto: daily, working · 212 GB free"
        );
        assert_eq!(
            human_line(&summary_of(3, None, true), now),
            "  backups: 3 · details out of date · auto: daily, working · 212 GB free"
        );
    }

    #[test]
    fn the_undo_phrase_sizes_under_v_and_says_over_policy_only_when_it_is() {
        let now = at("2026-10-15T18:00:00Z");
        let undo = |count, oldest: Option<&str>, prunable, bytes, truncated| UndoSnapshots {
            count,
            oldest_at: oldest.map(at),
            prunable,
            bytes,
            truncated,
        };
        let cases = [
            (
                "default, within policy",
                undo(30, Some("2026-10-10T18:00:00Z"), 0, None, false),
                None,
            ),
            (
                "default, over policy",
                undo(30, Some("2026-10-02T18:00:00Z"), 3, None, false),
                Some(" · undo: 30 snapshots (over policy)"),
            ),
            (
                "-v",
                undo(
                    30,
                    Some("2026-10-02T18:00:00Z"),
                    0,
                    Some(891_000_000),
                    false,
                ),
                Some(" · undo: 30 snapshots, 891 MB, oldest 13 days"),
            ),
            (
                "-v, over policy and cut short",
                undo(1, Some("2026-10-14T17:00:00Z"), 1, Some(5_000), true),
                Some(" · undo: 1 snapshot, at least 5.0 kB, oldest 1 day (over policy)"),
            ),
            (
                "-v, young",
                undo(2, Some("2026-10-15T08:00:00Z"), 0, Some(10), false),
                Some(" · undo: 2 snapshots, 10 B, oldest under a day"),
            ),
            (
                "-v, none",
                undo(0, None, 0, Some(0), false),
                Some(" · undo: 0 snapshots"),
            ),
        ];
        for (case, u, expected) in cases {
            assert_eq!(undo_phrase(&u, now).as_deref(), expected, "{case}");
        }
    }

    #[test]
    fn hints_refresh_first_then_back_up() {
        let mut s = summary_of(3, None, true);
        s.due = true;
        let actions: Vec<(String, Vec<String>)> = hints("retail", &s)
            .iter()
            .map(|h| (h.action().wire().to_owned(), h.argv().to_vec()))
            .collect();
        assert_eq!(
            actions,
            [
                (
                    "refresh_backups".to_owned(),
                    ["omaddons", "backup", "list", "-t", "retail"]
                        .map(str::to_owned)
                        .to_vec()
                ),
                (
                    "backup_create".to_owned(),
                    ["omaddons", "backup", "create", "-t", "retail"]
                        .map(str::to_owned)
                        .to_vec()
                ),
            ]
        );
        assert!(hints("retail", &summary_of(3, None, false)).is_empty());
    }
}
