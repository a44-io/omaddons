//! `status`' view of a client's backups (M3-D37, M3-D30, M3-D31, M3-D48): how protected
//! its `WTF/` is, whether automatic backups work there, and the space left. Every number
//! but the free space comes from the inventory projection, validated by one listing of
//! `<meta>/backups/` and one of `<meta>/snapshots/`: no `backup.json` is read and no tree
//! walked, so `status --cached` stays within its budget however many backups there are.
//! A stale projection is reported as such and never rebuilt here; `status` writes nothing.

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::backup::{AutoView, size, when};
use crate::backup::fs::{CloneHint, clone_hint, free_bytes};
use crate::backup::inventory::{self, Capability, Inventory, InventoryFile, Latest, Qualifying};
use crate::backup::{self, BackupKind, Trigger};
use crate::error::CommandHint;
use crate::limits::BACKUP_STALE_AFTER;
use crate::model::TargetKey;
use crate::state::config::AutoBackup;
use crate::state::paths::ClientPaths;

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
            "refresh_backups",
            ["omaddons", "backup", "list", "-t", id],
            false,
        ));
    }
    if s.due {
        hints.push(CommandHint::new(
            "backup_create",
            ["omaddons", "backup", "create", "-t", id],
            false,
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
    fn hints_refresh_first_then_back_up() {
        let mut s = summary_of(3, None, true);
        s.due = true;
        let actions: Vec<(String, Vec<String>)> = hints("retail", &s)
            .iter()
            .map(|h| (h.action().to_owned(), h.argv().to_vec()))
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
