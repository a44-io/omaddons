//! `concurrency_primitives`: interrupted changes.

use crate::doctor::budget::Budget;
use crate::doctor::facts::PendingJournal;
use crate::doctor::finding::{Confidence, Detected, Evidence, Observation, Subject};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;

const MANUAL: &str = "An interrupted change needs manual repair";
const WAITS: &str = "An interrupted change waits to be finished";

/// `journal-pending`: one finding per pending journal, on its target, or on the machine
/// when it names none. A journal that names no client needs manual repair whatever it
/// says, since no target's change can recover it.
pub(crate) fn journal_pending(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let facts = view.status_facts();
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
    Detected {
        found: on_targets.chain(on_machine).collect(),
        incomplete: None,
    }
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
