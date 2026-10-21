//! The closed set of filesystem operations. There is no delete: removal is a rename
//! into a snapshot or trash dir, and only `prune()` ever deletes.

use std::path::Path;

use super::sys::{ClonePolicy, Content, RenameMode};

/// One filesystem mutation, applied to a path by `mutate()`. Adding a variant needs a
/// comment on the vision bead.
pub enum Op<'a> {
    /// Create or replace a file atomically (DEC-11: bytes or a streamed reader).
    WriteFile {
        content: Content<'a>,
        mode: Option<u32>,
    },
    /// Append and fsync; creates the file (0644) if missing.
    AppendFile { content: &'a [u8] },
    /// Create one directory level; the parent must exist.
    CreateDir,
    /// Rename the path to `to`.
    Rename { to: &'a Path, mode: RenameMode },
    /// Copy the regular file `from` to the path (which must not exist): a reflink where
    /// possible, else a byte copy unless `policy` requires the reflink.
    CloneFile { from: &'a Path, policy: ClonePolicy },
    /// Set the mode of the path.
    Chmod { mode: u32 },
    /// Point the symlink at the path to `target`, atomically.
    SymlinkAtomic { target: &'a Path },
}

impl Op<'_> {
    /// The `op` wire name in `actions.jsonl`.
    pub fn wire(&self) -> &'static str {
        match self {
            Self::WriteFile { .. } => "write_file",
            Self::AppendFile { .. } => "append_file",
            Self::CreateDir => "create_dir",
            Self::Rename {
                mode: RenameMode::Replace,
                ..
            } => "rename_replace",
            Self::Rename {
                mode: RenameMode::NoReplace,
                ..
            } => "rename_noreplace",
            Self::Rename {
                mode: RenameMode::Exchange,
                ..
            } => "rename_exchange",
            Self::CloneFile { .. } => "clone_file",
            Self::Chmod { .. } => "chmod",
            Self::SymlinkAtomic { .. } => "symlink_atomic",
        }
    }
}
