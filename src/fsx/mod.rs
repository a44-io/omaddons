//! The single writer. Every filesystem mutation in the program lives here: the closed
//! `Op` enum, `mutate()`, `prune()`, the write-scope table, the deny-list, renameat2 and
//! reflink primitives, and the lock files.

mod expect;
pub(crate) mod failpoint;
mod fence;
mod identity;
mod lock;
mod mutate;
mod op;
mod prune;
mod scope;
mod session;
mod stamp;
pub(crate) mod sys;

pub use expect::{Described, Expect, FileKind, Found, Mismatch, Parts, Predicate};
#[cfg(debug_assertions)]
#[doc(hidden)]
pub use failpoint::{with_spec, with_spec_in};
pub use fence::{Fence, FenceSet, FenceStop, Fenced, FencedOp};
pub use identity::FileId;
pub use lock::{
    DirLock, GlobalLock, GlobalLockGuard, HolderInfo, InflightLock, Lease, LeaseError, LeaseGuard,
    LockState, probe_global,
};
pub(crate) use mutate::count_hashed;
#[cfg(test)]
pub(crate) use mutate::files_hashed;
pub use mutate::{
    ActionRecord, FsxError, MutateOpts, StampDigest, TreeDigest, ensure_dir_all, mutate,
    mutate_with, probe_exchange, sha256_from_hex, sync_filesystem,
};
pub use op::Op;
pub(crate) use prune::canonical_lenient;
pub use prune::{LockUse, PruneReason, PruneReport, prune, remove_purged_meta};
pub use scope::{Access, OwnedFolders, Refusal, Scope, ScopeMatch, ScopeTable, Side, Writer};
pub use session::{RepairPaths, SnapshotRoots, WriteSession};
pub use stamp::{FileStamp, TreeStamp, WalkError};
pub use sys::{
    CloneKind, ClonePolicy, Content, Durability, RenameMode, SyncCounts, WriteResult, mount_id,
    sync_counts,
};
