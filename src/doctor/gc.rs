//! `doctor gc` (M4-D36): the one command that deletes what the doctor keeps (old run
//! directories, the `.repair` client halves of repair transactions, the cache
//! quarantines), and never what an unfinished transaction, an interrupted undo or a
//! set-aside that can still be reactivated needs.
//!
//! The retention plan holds every safety rule of gc, and nothing in reach of it can
//! write: [`view`] reads the home once ([`GcView::load`]), and [`plan`](plan::plan) turns
//! what it read into the deletions, the exclusions with their pins, the undos lost,
//! `latest`'s move and the cap. The deletion itself is this module's alone (M4-D36: in
//! `src/doctor/`, only this file may prune).

mod plan;
mod view;

pub use plan::{
    DeletionKind, ExcludedKind, GcDeletion, GcExclusion, GcPlan, GcReport, LatestChange, Pin,
    Victim, invariant_violations, plan,
};
pub use view::{
    Anchor, Candidate, Client, GcInputs, GcView, Group, GuardState, JournalView, LatestState,
    RepairLineView, RepairOpView, SuspendedRepair,
};
