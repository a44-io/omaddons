//! Write sessions (DEC-10). A transaction session runs the full `mutate()` pipeline:
//! hashes, verbatim backups and an `actions.jsonl` line per operation. An untracked
//! session keeps the deny-list, the scopes and atomic execution but records nothing;
//! it is for derived data and user config (caches, `status.json`, `net.log`,
//! `config set`, the discovery cache). A logged untracked session records each
//! operation as an actions line in a log of its own, with no digests or backups: the
//! doctor runtime's `runtime.jsonl` (M4-D32).

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::mutate::{ActionRecord, FsxError};
use super::scope::{OwnedFolders, ScopeTable, Side, Writer};
use super::sys;
use crate::model::TxId;
use crate::state::paths::{ClientPaths, Paths};

/// The mode a logged session creates its log with.
const LOG_MODE: u32 = 0o600;

/// Where a transaction's verbatim backups go, per side: home-side paths back up under
/// `home_side`; paths inside a client dir back up under that client's
/// `Interface/.omaddons/snapshots/<tx>/files/` (`<tx>.wtf/` for a transaction that puts
/// settings back, `<tx>.repair/` for a doctor repair), on the same filesystem, so backups
/// are reflinks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRoots {
    home_side: PathBuf,
    client_half: ClientHalf,
}

/// The name of a transaction's client-side snapshot half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientHalf {
    /// `<tx>`.
    Plain,
    /// `<tx>.wtf`.
    Wtf,
    /// `<tx>.repair`.
    Repair,
}

impl SnapshotRoots {
    pub fn new(home_side: impl Into<PathBuf>) -> Self {
        Self {
            home_side: home_side.into(),
            client_half: ClientHalf::Plain,
        }
    }

    /// A doctor repair's layout (M4-D31): home-side backups under `home_side` (the
    /// repair passes `<run>/backups/`), client-side ones under the client's
    /// `<tx>.repair` half ([`ClientPaths::snapshots_repair`]).
    pub fn for_repair(home_side: impl Into<PathBuf>, _tx: &TxId) -> Self {
        Self {
            home_side: home_side.into(),
            client_half: ClientHalf::Repair,
        }
    }

    /// The engine's layout: `<state>/omaddons/snapshots/<tx>/files/`.
    pub fn for_tx(paths: &Paths, tx: &TxId) -> Self {
        Self::new(paths.state_snapshot(tx).join("files"))
    }

    /// The layout of a transaction that puts settings back: both halves named
    /// `<tx>.wtf` ([`Paths::state_snapshot_wtf`], [`ClientPaths::snapshots_wtf`]).
    pub fn for_wtf_tx(paths: &Paths, tx: &TxId) -> Self {
        Self {
            home_side: paths.state_snapshot_wtf(tx).join("files"),
            client_half: ClientHalf::Wtf,
        }
    }

    pub(crate) fn dir_for(&self, side: &Side, tx: &TxId) -> PathBuf {
        let Side::Client(client_dir) = side else {
            return self.home_side.clone();
        };
        let client = ClientPaths::new(client_dir);
        let half = match self.client_half {
            ClientHalf::Plain => client.snapshots(tx),
            ClientHalf::Wtf => client.snapshots_wtf(tx),
            ClientHalf::Repair => client.snapshots_repair(tx),
        };
        half.join("files")
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

/// Where a logged untracked session writes its lines.
pub(crate) struct LogSink {
    /// The log as it is named now: a rename of a directory holding it moves it.
    path: PathBuf,
    /// Opened once its directory exists, and kept: the descriptor still names the log
    /// after a rename, and still appends after a chmod to 0400.
    file: Option<File>,
    /// The lines of operations made before the log's directory existed, oldest first.
    pending: VecDeque<ActionRecord>,
    /// The last sequence number given.
    seq: u64,
}

impl LogSink {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            pending: VecDeque::new(),
            seq: 0,
        }
    }

    /// The next line's sequence number, starting at 1.
    pub(crate) fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// Records the line of an operation that ran, whether it succeeded or not. A rename
    /// that moved the log (`moved`: its source, its destination, and whether the two
    /// were exchanged) renames it here first. Then every waiting line is written, in
    /// order, once the log's directory exists; the operations create that directory, so
    /// the sink never does.
    pub(crate) fn record(
        &mut self,
        record: ActionRecord,
        moved: Option<(&Path, &Path, bool)>,
    ) -> Result<(), FsxError> {
        if let Some((from, to, exchange)) = moved {
            self.follow(from, to, exchange);
        }
        self.pending.push_back(record);
        self.flush()
    }

    /// The log's new name when `from` moved to `to` (and, exchanged, `to` to `from`).
    fn follow(&mut self, from: &Path, to: &Path, exchange: bool) {
        let under = |a: &Path, b: &Path| {
            self.path.strip_prefix(a).ok().map(|rest| {
                if rest.as_os_str().is_empty() {
                    b.to_path_buf()
                } else {
                    b.join(rest)
                }
            })
        };
        let moved = under(from, to).or_else(|| exchange.then(|| under(to, from)).flatten());
        if let Some(path) = moved {
            self.path = path;
        }
    }

    fn flush(&mut self) -> Result<(), FsxError> {
        let io_err = |path: &Path, source: io::Error| FsxError::Io {
            path: path.to_path_buf(),
            source,
        };
        if self.file.is_none() {
            if !self.path.parent().is_some_and(Path::is_dir) {
                return Ok(());
            }
            let file = sys::open_log(&self.path, LOG_MODE).map_err(|e| io_err(&self.path, e))?;
            self.file = Some(file);
        }
        let Some(file) = &self.file else {
            return Ok(());
        };
        while let Some(record) = self.pending.front() {
            let mut line =
                serde_json::to_vec(record).map_err(|e| io_err(&self.path, io::Error::other(e)))?;
            line.push(b'\n');
            sys::append_durable_to(file, &line).map_err(|e| io_err(&self.path, e))?;
            self.pending.pop_front();
        }
        Ok(())
    }
}

pub(crate) enum SessionKind {
    Tx(TxState),
    Untracked {
        reason: &'static str,
    },
    /// Untracked, with each operation recorded in its own log.
    UntrackedLogged {
        reason: &'static str,
        log: LogSink,
    },
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

    /// An untracked session that records each operation it executes as one actions line
    /// in `log`, with its own sequence and no digests or backups (the doctor runtime's
    /// `runtime.jsonl`). An operation refused before it runs is not recorded. Lines of
    /// operations made before the log's directory exists wait for it; a session dropped
    /// before then loses them.
    pub fn untracked_logged(
        writer: Writer,
        scopes: &'a ScopeTable,
        reason: &'static str,
        log: PathBuf,
    ) -> Self {
        Self {
            scopes,
            writer,
            owned: OwnedFolders::default(),
            kind: SessionKind::UntrackedLogged {
                reason,
                log: LogSink::new(log),
            },
        }
    }

    /// The log of a logged untracked session, as it is named now.
    pub fn log_path(&self) -> Option<&Path> {
        match &self.kind {
            SessionKind::UntrackedLogged { log, .. } => Some(&log.path),
            SessionKind::Tx(_) | SessionKind::Untracked { .. } => None,
        }
    }

    /// The last sequence number a logged untracked session gave, written or waiting; 0
    /// for the other kinds.
    pub fn logged_seq(&self) -> u64 {
        match &self.kind {
            SessionKind::UntrackedLogged { log, .. } => log.seq,
            SessionKind::Tx(_) | SessionKind::Untracked { .. } => 0,
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
            SessionKind::Untracked { .. } | SessionKind::UntrackedLogged { .. } => None,
        }
    }

    /// The actions log of a transaction session.
    pub fn actions_log(&self) -> Option<&Path> {
        match &self.kind {
            SessionKind::Tx(state) => Some(&state.actions_log),
            SessionKind::Untracked { .. } | SessionKind::UntrackedLogged { .. } => None,
        }
    }

    /// What an untracked session writes, for diagnostics.
    pub fn untracked_reason(&self) -> Option<&'static str> {
        match &self.kind {
            SessionKind::Tx(_) => None,
            SessionKind::Untracked { reason } | SessionKind::UntrackedLogged { reason, .. } => {
                Some(reason)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_snapshot_roots_put_client_backups_under_the_repair_half() {
        let tx: TxId = "01JB8Q2VA00000000000000000".parse().unwrap();
        let paths = Paths::with_roots("/h", "/c", "/s", "/k");
        let client = Side::Client(PathBuf::from("/wow/_retail_"));
        let snapshots = "/wow/_retail_/Interface/.omaddons/snapshots";
        let repair = SnapshotRoots::for_repair("/s/omaddons/doctor/runs/R/backups", &tx);
        let cases = [
            (
                "repair_home",
                repair.dir_for(&Side::Home, &tx),
                "/s/omaddons/doctor/runs/R/backups".to_owned(),
            ),
            (
                "repair_client",
                repair.dir_for(&client, &tx),
                format!("{snapshots}/{tx}.repair/files"),
            ),
            (
                "tx_client",
                SnapshotRoots::for_tx(&paths, &tx).dir_for(&client, &tx),
                format!("{snapshots}/{tx}/files"),
            ),
            (
                "tx_home",
                SnapshotRoots::for_tx(&paths, &tx).dir_for(&Side::Home, &tx),
                format!("/s/omaddons/snapshots/{tx}/files"),
            ),
            (
                "wtf_client",
                SnapshotRoots::for_wtf_tx(&paths, &tx).dir_for(&client, &tx),
                format!("{snapshots}/{tx}.wtf/files"),
            ),
        ];
        for (case, got, want) in cases {
            assert_eq!(got, PathBuf::from(&want), "{case}: {got:?} → {want}");
        }
    }

    #[test]
    fn the_log_follows_the_renames_that_move_it() {
        let log = "/s/doctor/runs/.x.tmp/runtime.jsonl";
        let cases = [
            (
                "its directory",
                "/s/doctor/runs/.x.tmp",
                "/s/doctor/runs/x",
                false,
                "/s/doctor/runs/x/runtime.jsonl",
            ),
            (
                "an ancestor",
                "/s/doctor",
                "/s/doctor.old",
                false,
                "/s/doctor.old/runs/.x.tmp/runtime.jsonl",
            ),
            (
                "the log itself",
                log,
                "/s/doctor/runs/.x.tmp/runtime.old",
                false,
                "/s/doctor/runs/.x.tmp/runtime.old",
            ),
            (
                "a sibling",
                "/s/doctor/runs/.x",
                "/s/doctor/runs/y",
                false,
                log,
            ),
            (
                "a name that only starts alike",
                "/s/doctor/runs/.x.t",
                "/s/doctor/runs/z",
                false,
                log,
            ),
            (
                "exchanged, as the source",
                "/s/doctor/runs/.x.tmp",
                "/s/doctor/runs/x",
                true,
                "/s/doctor/runs/x/runtime.jsonl",
            ),
            (
                "exchanged, as the destination",
                "/s/doctor/runs/x",
                "/s/doctor/runs/.x.tmp",
                true,
                "/s/doctor/runs/x/runtime.jsonl",
            ),
            (
                "renamed onto, not exchanged",
                "/s/doctor/runs/x",
                "/s/doctor/runs/.x.tmp",
                false,
                log,
            ),
        ];
        for (case, from, to, exchange, expected) in cases {
            let mut sink = LogSink::new(log.into());
            sink.follow(Path::new(from), Path::new(to), exchange);
            assert_eq!(
                sink.path,
                Path::new(expected),
                "{case}: {from} → {to} (exchange {exchange})"
            );
        }
    }
}
