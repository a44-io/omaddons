//! The single writer. Every filesystem mutation in the program lives here: the closed
//! `Op` enum, `mutate()`, `prune()`, the write-scope table, the deny-list, renameat2 and
//! reflink primitives, and the lock files.

pub(crate) mod failpoint;
mod mutate;
mod op;
mod scope;
mod session;
pub(crate) mod sys;

pub use mutate::{
    ActionRecord, Expect, FsxError, MutateOpts, TreeDigest, ensure_dir_all, mutate, mutate_with,
    sync_filesystem,
};
pub use op::Op;
pub use scope::{Access, OwnedFolders, Refusal, Scope, ScopeMatch, ScopeTable, Side, Writer};
pub use session::{SnapshotRoots, WriteSession};
pub use sys::{CloneKind, Content, Durability, RenameMode, WriteResult};
