//! The single writer. Every filesystem mutation in the program lives here: the closed
//! `Op` enum, `mutate()`, `prune()`, the write-scope table, the deny-list, renameat2 and
//! reflink primitives, and the lock files.

pub(crate) mod failpoint;
mod lock;
mod mutate;
mod op;
mod prune;
mod scope;
mod session;
mod stamp;
pub(crate) mod sys;

#[cfg(debug_assertions)]
#[doc(hidden)]
pub use failpoint::with_spec;
pub use lock::{GlobalLock, GlobalLockGuard, HolderInfo, InflightLock, LockState, probe_global};
pub use mutate::{
    ActionRecord, Expect, FsxError, MutateOpts, TreeDigest, ensure_dir_all, mutate, mutate_with,
    sync_filesystem,
};
#[cfg(test)]
pub(crate) use mutate::{count_hashed, files_hashed};
pub use op::Op;
pub use prune::{LockUse, PruneReason, PruneReport, prune};
pub use scope::{Access, OwnedFolders, Refusal, Scope, ScopeMatch, ScopeTable, Side, Writer};
pub use session::{SnapshotRoots, WriteSession};
pub use stamp::{FileStamp, TreeStamp};
pub use sys::{CloneKind, Content, Durability, RenameMode, SyncCounts, WriteResult, sync_counts};
