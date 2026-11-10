//! `state_files`: records that no longer match the disk.

use jiff::Timestamp;

use super::TargetFacts;
use crate::doctor::budget::Budget;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::drift::{DriftMode, DriftState};
use crate::engine::retention::{self, Judgment, RetentionLimits, Sizes};
use crate::model::TargetKey;
use crate::state::paths::ClientPaths;

/// `record-folder-missing`, from the Discovery and Records layers: a presence check, as
/// `status --cached` makes it (a deeper report answers: the gone folders are the same).
pub(crate) fn record_folder_missing(
    view: &DoctorView,
    sel: &Selection,
    _: &mut Budget,
) -> Detected {
    let targets = super::drift_facts(view, sel, DriftMode::Presence);
    Detected::complete(record_folder_missing_in(&targets))
}

/// `snapshots-past-retention`, from the Discovery and History layers: per registered
/// discovered client in scope, the undo snapshots retention would prune now, by
/// [`retention::judge`], the rule a commit's retention applies. The quick tier walks no
/// tree, so the size cap, which needs every snapshot's size, stays a commit's to apply:
/// only the count and age rules decide here. A client whose directory is gone is never
/// reached, as retention never reaches it. Retention's clock is the system's, and so is
/// this check's.
pub(crate) fn snapshots_past_retention(
    view: &DoctorView,
    sel: &Selection,
    budget: &mut Budget,
) -> Detected {
    let (Some(world), Some(history)) = (view.world(), view.history()) else {
        return Detected::complete(Vec::new());
    };
    let scope = sel.scope(view).ok();
    let limits = view
        .config()
        .and_then(|c| c.config.effective_limits().ok())
        .map_or_else(RetentionLimits::default, |l| RetentionLimits::from(&l));
    let pinned = retention::pinned_by(&history.entries);
    let now = Timestamp::now();
    let mut found = Vec::new();
    for d in &world.resolved.targets {
        let Some(key) = d.target.key else {
            continue;
        };
        if !scope
            .as_ref()
            .is_none_or(|scope| scope.includes_client(Some(key), &d.target.id))
        {
            continue;
        }
        // One `stat` or two per transaction that touched the client.
        let lines = history
            .entries
            .iter()
            .filter(|e| e.touches_target(key))
            .count();
        if budget.charge(lines as u64).is_err() {
            return Detected::cut_short(found, budget);
        }
        let client = ClientPaths::new(&d.target.client_dir);
        let judged = retention::judge(
            view.paths(),
            &client,
            key,
            &history.entries,
            &pinned,
            &limits,
            now,
            Sizes::Unmeasured,
        );
        if let Some(o) =
            snapshots_past_retention_of(&view.target_ref(key).id, key, &client, &judged)
        {
            found.push(o);
        }
    }
    Detected::complete(found)
}

/// `snapshots-past-retention`'s rule over one client's judgment: one finding naming the
/// transactions retention would prune, oldest first, when there are any.
pub(crate) fn snapshots_past_retention_of(
    id: &str,
    key: TargetKey,
    client: &ClientPaths,
    judged: &Judgment,
) -> Option<Observation> {
    let due = judged.due();
    let oldest = due.iter().map(|t| t.at).min()?;
    let n = due.len();
    let s = if n == 1 { "" } else { "s" };
    Some(Observation {
        subject: Subject::target(id, key),
        title: format!("{id} keeps {n} undo snapshot{s} retention would prune now"),
        confidence: Confidence::High,
        addon: None,
        folder: None,
        evidence: Evidence {
            path: Some(client.meta_dir().join("snapshots")),
            count: Some(n as u64),
            txs: Some(due.iter().map(|t| t.tx).collect()),
            oldest_at: Some(oldest),
            ..Evidence::default()
        },
    })
}

/// `record-folder-missing`'s rule: one finding per managed addon whose folder is gone.
pub(crate) fn record_folder_missing_in(targets: &[TargetFacts]) -> Vec<Observation> {
    targets
        .iter()
        .flat_map(|t| {
            t.drift
                .iter()
                .filter(|d| d.state == DriftState::Missing)
                .map(|d| Observation {
                    subject: Subject::target(&t.id, t.key).addon(&d.addon),
                    title: format!("{}'s folder is gone", d.addon),
                    confidence: Confidence::High,
                    addon: Some(d.addon.clone()),
                    folder: None,
                    evidence: Evidence {
                        paths: (!d.missing_folders.is_empty()).then(|| d.missing_folders.clone()),
                        ..Evidence::default()
                    },
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::retention::{Selection, SnapshotTx};
    use crate::model::TxId;

    fn tx(n: u64, at: &str) -> SnapshotTx {
        SnapshotTx {
            tx: TxId::from_ulid(ulid::Ulid::from_parts(n, u128::from(n))),
            at: at.parse().unwrap(),
            bytes: 0,
            most_recent: false,
            pinned: false,
        }
    }

    #[test]
    fn one_finding_names_what_retention_would_prune_oldest_first() {
        let key = TargetKey::from_ulid(ulid::Ulid::from_parts(9, 9));
        let client = ClientPaths::new(std::path::Path::new("/games/_retail_"));
        let txs = vec![
            tx(1, "2026-10-01T00:00:00Z"),
            tx(2, "2026-10-02T00:00:00Z"),
            tx(3, "2026-10-14T00:00:00Z"),
        ];
        let within = Judgment {
            txs: txs.clone(),
            selection: Selection::default(),
        };
        assert!(snapshots_past_retention_of("retail", key, &client, &within).is_none());
        let past = Judgment {
            selection: Selection {
                prune: vec![txs[1].tx, txs[0].tx],
                kept: 1,
                cap_exceeded: None,
            },
            txs: txs.clone(),
        };
        let o = snapshots_past_retention_of("retail", key, &client, &past).unwrap();
        assert_eq!(
            o.title,
            "retail keeps 2 undo snapshots retention would prune now"
        );
        assert_eq!(o.evidence.count, Some(2));
        assert_eq!(o.evidence.txs, Some(vec![txs[0].tx, txs[1].tx]));
        assert_eq!(o.evidence.oldest_at, Some(txs[0].at));
        assert_eq!(
            o.evidence.path.as_deref(),
            Some(std::path::Path::new(
                "/games/_retail_/Interface/.omaddons/snapshots"
            ))
        );
        assert!(o.evidence.is_private_safe());
    }
}
