//! Write sessions (DEC-10). A transaction session runs the full `mutate()` pipeline:
//! hashes, verbatim backups and an `actions.jsonl` line per operation. An untracked
//! session keeps the deny-list, the scopes and atomic execution but records nothing;
//! it is for derived data and user config (caches, `status.json`, `net.log`,
//! `config set`, the discovery cache).

use std::path::{Path, PathBuf};

use super::scope::{OwnedFolders, ScopeTable, Side, Writer};
use crate::model::TxId;
use crate::state::paths::{ClientPaths, Paths};

/// Where a transaction's verbatim backups go, per side: home-side paths back up under
/// `home_side`; paths inside a client dir back up under that client's
/// `Interface/.omaddons/snapshots/<tx>/files/`, on the same filesystem, so backups are
/// reflinks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRoots {
    home_side: PathBuf,
}

impl SnapshotRoots {
    pub fn new(home_side: impl Into<PathBuf>) -> Self {
        Self {
            home_side: home_side.into(),
        }
    }

    /// The engine's layout: `<state>/omaddons/snapshots/<tx>/files/`.
    pub fn for_tx(paths: &Paths, tx: &TxId) -> Self {
        Self::new(paths.state_snapshot(tx).join("files"))
    }

    pub(crate) fn dir_for(&self, side: &Side, tx: &TxId) -> PathBuf {
        match side {
            Side::Home => self.home_side.clone(),
            Side::Client(client_dir) => ClientPaths::new(client_dir).snapshots(tx).join("files"),
        }
    }
}

pub(crate) struct TxState {
    pub(crate) tx: TxId,
    pub(crate) snapshot_roots: SnapshotRoots,
    pub(crate) actions_log: PathBuf,
    next_seq: u64,
}

impl TxState {
    /// The next action's sequence number, starting at 1.
    pub(crate) fn next_seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }
}

pub(crate) enum SessionKind {
    Tx(TxState),
    Untracked { reason: &'static str },
}

/// Everything `mutate()` needs to decide and record a write.
pub struct WriteSession<'a> {
    pub(crate) scopes: &'a ScopeTable,
    pub(crate) writer: Writer,
    pub(crate) owned: OwnedFolders,
    pub(crate) kind: SessionKind,
}

impl<'a> WriteSession<'a> {
    /// A transaction session. The engine passes
    /// `actions_log = <state>/omaddons/snapshots/<tx>/actions.jsonl`; the doctor (M4)
    /// passes its run directory.
    pub fn tx(
        tx: TxId,
        writer: Writer,
        scopes: &'a ScopeTable,
        snapshot_roots: SnapshotRoots,
        actions_log: PathBuf,
    ) -> Self {
        Self {
            scopes,
            writer,
            owned: OwnedFolders::default(),
            kind: SessionKind::Tx(TxState {
                tx,
                snapshot_roots,
                actions_log,
                next_seq: 0,
            }),
        }
    }

    /// An untracked session; `reason` names what it writes, e.g. `"status.json"`.
    pub fn untracked(writer: Writer, scopes: &'a ScopeTable, reason: &'static str) -> Self {
        Self {
            scopes,
            writer,
            owned: OwnedFolders::default(),
            kind: SessionKind::Untracked { reason },
        }
    }

    /// The folders whose SavedVariables an `Undo` session may restore.
    #[must_use]
    pub fn with_owned_folders(mut self, owned: OwnedFolders) -> Self {
        self.owned = owned;
        self
    }

    pub fn writer(&self) -> Writer {
        self.writer
    }

    /// `Some` for transaction sessions.
    pub fn tx_id(&self) -> Option<TxId> {
        match &self.kind {
            SessionKind::Tx(state) => Some(state.tx),
            SessionKind::Untracked { .. } => None,
        }
    }

    /// The actions log of a transaction session.
    pub fn actions_log(&self) -> Option<&Path> {
        match &self.kind {
            SessionKind::Tx(state) => Some(&state.actions_log),
            SessionKind::Untracked { .. } => None,
        }
    }

    /// What an untracked session writes, for diagnostics.
    pub fn untracked_reason(&self) -> Option<&'static str> {
        match &self.kind {
            SessionKind::Tx(_) => None,
            SessionKind::Untracked { reason } => Some(reason),
        }
    }
}
