//! The transaction engine: plan, download, inspect, stage, lock and revalidate, journal,
//! swap, commit, recovery, undo, retention and the running policy.

pub mod blobs;
pub mod inspect;
pub mod journal;
pub mod ownership;
pub mod plan;
pub mod running;
pub mod stage;
pub mod swap;
pub mod tx;
pub mod undo;

pub use tx::{EngineCtx, FAILPOINTS, TxOutcome, TxOutcomes, TxRequest, TxTarget, run};
pub use undo::{SvCounts, SvPolicy, UndoOutcome, UndoRequest, UndoSelector, undo};
#[cfg(test)]
pub(crate) mod testing;
