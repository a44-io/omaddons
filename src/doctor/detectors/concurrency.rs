//! `concurrency_primitives`: interrupted transactions.

use std::collections::BTreeSet;

use crate::app::status::pending_journals;
use crate::doctor::budget::Budget;
use crate::doctor::facts::{MachineFacts, PendingJournal, StatusFacts, TargetFacts};
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::fsx::LockState;
use crate::model::TargetKey;

const MANUAL: &str = "An interrupted change needs manual repair";
const WAITS: &str = "An interrupted change waits to be finished";

/// `journal-pending`, from the Journals and Lock layers: none while an omaddons process
/// holds the lock (a change is committing, not interrupted).
pub(crate) fn journal_pending(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let (Some(journals), Some(lock)) = (view.journals(), view.lock()) else {
        return Detected::default();
    };
    let held = matches!(lock, LockState::Held { .. });
    let keys: BTreeSet<TargetKey> = journals.pending.iter().filter_map(|p| p.target).collect();
    let facts = StatusFacts {
        targets: keys
            .into_iter()
            .map(|key| TargetFacts {
                journals: pending_journals(&journals.pending, Some(key), held),
                ..TargetFacts::new(view.target_ref(key).id, key)
            })
            .collect(),
        machine: MachineFacts {
            unattributed: pending_journals(&journals.pending, None, held),
            widget: None,
        },
    };
    Detected::complete(journal_pending_in(&facts))
}

/// `journal-pending`'s rule: one finding per pending journal, on its target, or on the
/// machine when it names none. A journal that names no client needs manual repair
/// whatever it says, since no target's change can recover it.
pub(crate) fn journal_pending_in(facts: &StatusFacts) -> Vec<Observation> {
    let on_targets = facts.targets.iter().flat_map(|t| {
        t.journals.iter().map(|j| {
            let title = if j.unrecoverable.is_some() {
                MANUAL
            } else {
                WAITS
            };
            observation(Subject::target(&t.id, t.key), j, title)
        })
    });
    let on_machine = facts
        .machine
        .unattributed
        .iter()
        .map(|j| observation(Subject::machine(), j, MANUAL));
    on_targets.chain(on_machine).collect()
}

fn observation(subject: Subject, journal: &PendingJournal, title: &str) -> Observation {
    Observation {
        subject: subject.tx(journal.tx),
        title: title.to_owned(),
        confidence: Confidence::High,
        addon: None,
        folder: None,
        evidence: Evidence {
            tx: Some(journal.tx),
            reason: journal.unrecoverable.map(|u| u.reason()),
            ..Evidence::default()
        },
    }
}
