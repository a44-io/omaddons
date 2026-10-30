//! The transaction engine: plan, download, inspect, stage, lock and revalidate, journal,
//! swap, commit, recovery, undo, retention and the running policy.

pub mod blobs;
pub mod edit;
pub mod indexes;
pub mod inspect;
pub mod journal;
pub mod ownership;
pub mod plan;
mod preserve;
pub mod repair_action;
pub mod resource;
pub mod restore;
pub mod restore_addons;
pub mod retention;
pub mod running;
pub mod simulate;
pub mod stage;
pub mod swap;
pub mod tx;
pub mod undo;
pub mod witness;

pub use tx::{Binding, EngineCtx, FAILPOINTS, TxOutcome, TxOutcomes, TxRequest, TxTarget, run};
pub use undo::{
    SvCounts, SvPolicy, UndoFacts, UndoOutcome, UndoRequest, UndoSelector, WtfUndo, undo,
};
#[cfg(test)]
pub(crate) mod testing;
