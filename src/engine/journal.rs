//! The transaction journal (vision §5.5): what makes a multi-rename commit crash-safe.
//!
//! `<state>/omaddons/journal/<tx>.jsonl` holds one JSON object per line, each appended
//! with one fsync: `prepared` (with everything roll-forward needs), one `step` before
//! each rename, `swapped` after the last rename, `committed` once the state files are
//! written. Recovery runs only while holding the global lock (D23): it rolls forward
//! from `swapped` and rolls back from anything earlier, replaying the step records in
//! reverse.
//!
//! Each step records the identity (device and inode) of both paths before its rename.
//! Renames on one filesystem keep inodes, so recovery knows exactly whether a step ran:
//! an `exchange` leaves both paths in place either way, and only identity tells. A
//! step that did not run is skipped, which also makes recovery idempotent.
//!
//! Readers never recover: [`pending_warnings`] reports `journal_pending` (lock free) or
//! `tx_in_progress` (lock held) and writes nothing.
//!
//! What this build cannot read, it never acts on (M3-D45). A journal started by a newer
//! major version, or holding a complete line this build cannot decode, is left as it is
//! (journal, in-flight lock, stage and snapshots) for manual repair: treated as
//! "nothing ran", it would be pruned with the only record of the change. Only a torn
//! last line, a crash mid-append, is ignored.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    self, FsxError, GlobalLock, GlobalLockGuard, InflightLock, LockState, LockUse, Op, PruneReason,
    RenameMode, ScopeTable, WriteSession, Writer, ensure_dir_all, failpoint, mutate, probe_global,
};
use crate::model::{Actor, TargetKey, TxId, TxKind};
use crate::state::doorbell;
use crate::state::files_index::{FilesIndex, FolderIndex};
use crate::state::history::{self, HistoryEntry, HistoryFilter};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::TargetRecord;
use crate::state::store::{self, SchemaVersion};

/// The version of the line format. A newer minor adds only optional fields and lines an
/// older build may ignore; a newer major is never recovered by this one.
pub const CURRENT: SchemaVersion = SchemaVersion::new(1, 1);

/// One journal line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalLine {
    Prepared(Box<Prepared>),
    Step(StepRecord),
    Swapped,
    Committed,
}

/// By hand: serde's derived internally tagged enums buffer their content in a form
/// that cannot hold an `i128`, which the files index's `mtime_ns` is. Reading the tag
/// from a JSON value and deserializing the variant's struct from it has no such limit.
impl<'de> Deserialize<'de> for JournalLine {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        let value = serde_json::Value::deserialize(d)?;
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| D::Error::missing_field("type"))?
            .to_owned();
        match kind.as_str() {
            "prepared" => serde_json::from_value::<Prepared>(value)
                .map(|p| Self::Prepared(Box::new(p)))
                .map_err(D::Error::custom),
            "step" => serde_json::from_value(value)
                .map(Self::Step)
                .map_err(D::Error::custom),
            "swapped" => Ok(Self::Swapped),
            "committed" => Ok(Self::Committed),
            other => Err(D::Error::unknown_variant(
                other,
                &["prepared", "step", "swapped", "committed"],
            )),
        }
    }
}

/// The first line: the transaction and what committing it writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Prepared {
    pub schema_version: SchemaVersion,
    pub tx: TxId,
    pub target_key: TargetKey,
    pub client_dir: PathBuf,
    pub kind: TxKind,
    #[schemars(with = "String")]
    pub started_at: Timestamp,
    /// Directories the transaction creates before its first rename: the
    /// `Interface/AddOns` of a client that has never been launched. Recovery leaves
    /// them, empty or not of its doing: the client creates the same folder on its first
    /// launch, and an empty one is inert.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub creates_dirs: Vec<PathBuf>,
    pub commit: CommitPayload,
    /// It renames a whole `WTF/` (a restore, the undo of one, M3-D24): recovery writes
    /// as `BackupRestore`, waits while the game runs, and stops rather than prune what
    /// it cannot account for. Absent (false) on every other transaction.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub writes_wtf: bool,
    /// The staged `WTF/`'s stamp digest (`stamp:sha256:`) when it was journaled: a
    /// rollback that finds the stage changed since (played on) keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_stamp: Option<String>,
    /// The backup the stage was copied from, and its directory's identity then: a
    /// stage whose source is gone holds the last copy of that content, and is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_backup: Option<SourceBackup>,
}

/// The backup a restore's stage was copied from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceBackup {
    pub id: crate::model::BackupId,
    /// Its directory's identity, compared with [`same`].
    pub dir: FileId,
}

/// Everything roll-forward writes, so recovery needs no planning and no network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommitPayload {
    /// The new record (generation + 1).
    pub record: TargetRecord,
    /// The per-file index changes for the folders the transaction touched.
    pub index: IndexChanges,
    pub history: HistoryEntry,
    pub stage_dir: PathBuf,
    /// An undo's SavedVariables restore, which runs after the swap; roll-forward
    /// finishes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sv_restore: Option<super::undo::SvRestore>,
}

/// Changes to `targets/<key>.files.json`; applying them twice changes nothing more.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IndexChanges {
    /// New or replaced folders.
    pub upsert: Vec<FolderIndex>,
    /// Folder keys (`FolderName::key()`) removed.
    pub remove: Vec<String>,
}

impl IndexChanges {
    /// `index` with the changes applied.
    pub fn apply(&self, index: &mut FilesIndex) {
        for key in &self.remove {
            index.folders.remove(key);
        }
        for folder in &self.upsert {
            index
                .folders
                .insert(folder.name.to_ascii_lowercase(), folder.clone());
        }
    }
}

/// The rename a step announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepOp {
    /// `RENAME_EXCHANGE`.
    Exchange,
    /// `RENAME_NOREPLACE`.
    Noreplace,
    /// A plain rename onto an absent path (filesystems without `renameat2` flags).
    Rename,
}

/// Why a step renames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepPurpose {
    SwapIn,
    OldToSnapshot,
    RemoveToSnapshot,
    FallbackLiveToSnapshot,
    FallbackStageToLive,
}

/// A path's identity, which a rename on one filesystem keeps: its inode number and, where
/// the filesystem keeps one, its birth time. The device number is recorded for diagnosis
/// only: btrfs subvolumes, FUSE and device-mapper volumes get a new one on every mount,
/// so a reboot would make every step look like neither state. Compare with [`same`],
/// never `==`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
    /// Nanoseconds since the epoch (statx `STATX_BTIME`); `None` where the filesystem
    /// has none, and in journals written before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub born_ns: Option<i64>,
}

impl FileId {
    /// The identity of what is at `path` (not following a symlink), if anything.
    pub fn of(path: &Path) -> Option<Self> {
        std::fs::symlink_metadata(path).ok().map(|m| Self {
            dev: m.dev(),
            ino: m.ino(),
            born_ns: m
                .created()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_nanos()).ok()),
        })
    }
}

/// Whether two recorded identities name the same file: both absent, or the same inode
/// with the same birth time when both know it. The device is ignored: a step's two paths
/// are on one filesystem by construction, and its number changes across mounts.
fn same(a: Option<FileId>, b: Option<FileId>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.ino == b.ino
                && match (a.born_ns, b.born_ns) {
                    // ubs:ignore — comparing birth times, not secrets
                    (Some(x), Some(y)) => x == y,
                    _ => true,
                }
        }
        _ => false,
    }
}

/// One rename, recorded before it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepRecord {
    /// 1-based.
    pub n: u32,
    pub op: StepOp,
    pub from: PathBuf,
    pub to: PathBuf,
    pub purpose: StepPurpose,
    /// What was at `from` and `to` before the rename.
    pub from_id: Option<FileId>,
    pub to_id: Option<FileId>,
}

/// A step to announce: the rename's arguments, before identities are taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub op: StepOp,
    pub from: PathBuf,
    pub to: PathBuf,
    pub purpose: StepPurpose,
}

// ---------------------------------------------------------------------------------------
// Writing.

/// An open journal. Every append is one write plus fsync.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    tx: TxId,
    steps: u32,
}

impl Journal {
    /// Creates `journal/<tx>.jsonl` with its `prepared` line. The guard proves the
    /// global lock is held.
    pub fn create(
        s: &mut WriteSession<'_>,
        paths: &Paths,
        _lock: &GlobalLockGuard,
        prepared: Prepared,
    ) -> Result<Self> {
        ensure_dir_all(s, &paths.journal_dir())?;
        let journal = Self {
            path: paths.journal(&prepared.tx),
            tx: prepared.tx,
            steps: 0,
        };
        journal.append(s, &JournalLine::Prepared(Box::new(prepared)))?;
        Ok(journal)
    }

    pub fn tx(&self) -> TxId {
        self.tx
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records `step` (with the identities of both paths now) before its rename runs.
    pub fn step(&mut self, s: &mut WriteSession<'_>, step: Step) -> Result<StepRecord> {
        let record = StepRecord {
            n: self.steps + 1,
            op: step.op,
            from_id: FileId::of(&step.from),
            to_id: FileId::of(&step.to),
            from: step.from,
            to: step.to,
            purpose: step.purpose,
        };
        self.append(s, &JournalLine::Step(record.clone()))?;
        self.steps += 1;
        Ok(record)
    }

    /// After the last rename and its directory fsyncs.
    pub fn mark_swapped(&mut self, s: &mut WriteSession<'_>) -> Result<()> {
        self.append(s, &JournalLine::Swapped)
    }

    /// After the record, the files index and the history line are written.
    pub fn mark_committed(&mut self, s: &mut WriteSession<'_>) -> Result<()> {
        self.append(s, &JournalLine::Committed)
    }

    /// Deletes the committed journal.
    pub fn finish(self, scopes: &ScopeTable, lock: LockUse<'_>) -> Result<()> {
        fsx::prune(scopes, PruneReason::PostCommitCleanup, &[self.path], lock)?;
        Ok(())
    }

    fn append(&self, s: &mut WriteSession<'_>, line: &JournalLine) -> Result<()> {
        append_line(s, &self.path, line)
    }
}

fn append_line(s: &mut WriteSession<'_>, path: &Path, line: &JournalLine) -> Result<()> {
    let mut bytes = serde_json::to_vec(line)
        .map_err(|e| Error::internal(format!("serializing a journal line: {e}")))?;
    bytes.push(b'\n');
    mutate(s, path, Op::AppendFile { content: &bytes })?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Reading.

/// How far a transaction got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Created, but no valid `prepared` line: nothing ran.
    Unprepared,
    Prepared,
    Swapped,
    Committed,
}

/// A journal as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub prepared: Option<Prepared>,
    pub steps: Vec<StepRecord>,
    pub phase: Phase,
    /// Complete lines this build cannot read: JSON it does not decode (an unknown line
    /// type or step, a second `prepared`, a marker before `prepared`), or not JSON at all
    /// before the last line. Recovery never acts on a journal that has one (M3-D45).
    pub unknown_lines: usize,
    /// The last line was cut short by a crash mid-append, and was ignored.
    pub torn_tail: bool,
    /// Complete lines, read or not; the torn tail is not one.
    pub lines: usize,
    /// The `prepared` line's own version, read even when the line does not decode.
    pub version: Option<SchemaVersion>,
    /// The `prepared` line's target, read even when the line does not decode, so that a
    /// journal this build cannot read fails only its own target's commands.
    pub target: Option<TargetKey>,
}

/// Why recovery leaves a journal for manual repair (M3-D45).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unrecoverable {
    /// Started by a newer major version of the journal (`journal_too_new`).
    TooNew,
    /// A complete line does not read, or no `prepared` line does (`journal_unreadable`).
    Unreadable,
}

impl Unrecoverable {
    /// The `journal_pending` reason (`details.reason`).
    pub fn reason(self) -> &'static str {
        match self {
            Self::TooNew => "journal_too_new",
            Self::Unreadable => "journal_unreadable",
        }
    }

    /// What to do about it: prose, since no command of this build can help.
    pub fn fix(self) -> Suggestion {
        match self {
            Self::TooNew => Suggestion::text(
                "a newer omaddons started this change; finish it with that version",
            ),
            Self::Unreadable => Suggestion::text(
                "see \"journal_pending that does not clear\" under Troubleshooting in the README",
            ),
        }
    }
}

impl Parsed {
    /// Why recovery must leave this journal as it is, when it must: a newer major version,
    /// or a complete line it cannot read (a `prepared` line that does not decode is one).
    /// A journal that is nothing but a torn tail is not: nothing ran.
    pub fn unrecoverable(&self) -> Option<Unrecoverable> {
        if self.version.is_some_and(|v| v.major > CURRENT.major) {
            Some(Unrecoverable::TooNew)
        } else if self.unknown_lines > 0 || (self.prepared.is_none() && self.lines > 0) {
            Some(Unrecoverable::Unreadable)
        } else {
            None
        }
    }
}

/// Parses a journal, never failing: what it cannot read is counted for
/// [`Parsed::unrecoverable`]. A last line without its newline is a crash mid-append and is
/// ignored: a line is fsync'd before what it announces runs, so a torn step never ran.
/// So is a last line that is not JSON: every writer writes JSON, so it is such a fragment
/// that recovery's own append (`committed`) closed off.
pub fn parse(text: &str) -> Parsed {
    let mut parsed = Parsed {
        prepared: None,
        steps: Vec::new(),
        phase: Phase::Unprepared,
        unknown_lines: 0,
        torn_tail: false,
        lines: 0,
        version: None,
        target: None,
    };
    let complete = text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();
    for (i, raw) in lines.iter().enumerate() {
        let last = i + 1 == lines.len();
        if last && !complete {
            parsed.torn_tail = true;
            break;
        }
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(_) if last => {
                parsed.torn_tail = true;
                break;
            }
            Err(_) => {
                parsed.lines += 1;
                parsed.unknown_lines += 1;
                continue;
            }
        };
        parsed.lines += 1;
        if matches!(
            value.get("type").and_then(serde_json::Value::as_str),
            Some("prepared")
        ) && parsed.version.is_none()
        {
            let field = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
            parsed.version = field("schema_version").and_then(|v| v.parse().ok());
            parsed.target = field("target_key").and_then(|k| k.parse().ok());
        }
        let Ok(line) = serde_json::from_value::<JournalLine>(value) else {
            parsed.unknown_lines += 1;
            continue;
        };
        match line {
            JournalLine::Prepared(prepared) if parsed.prepared.is_none() => {
                parsed.prepared = Some(*prepared);
                parsed.phase = Phase::Prepared;
            }
            JournalLine::Prepared(_) => parsed.unknown_lines += 1,
            JournalLine::Step(step) => parsed.steps.push(step),
            JournalLine::Swapped if parsed.prepared.is_some() => parsed.phase = Phase::Swapped,
            JournalLine::Committed if parsed.prepared.is_some() => {
                parsed.phase = Phase::Committed;
            }
            JournalLine::Swapped | JournalLine::Committed => parsed.unknown_lines += 1,
        }
    }
    parsed
}

/// Every journal in `journal/`, by transaction.
fn journals(paths: &Paths) -> Result<Vec<(TxId, PathBuf)>> {
    let dir = paths.journal_dir();
    let listing = match std::fs::read_dir(&dir) {
        Ok(listing) => listing,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(Error::io(
                &dir,
                &e,
                Suggestion::command(["omaddons", "status"]),
            ));
        }
    };
    let mut found: Vec<(TxId, PathBuf)> = listing
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            let tx: TxId = name.strip_suffix(".jsonl")?.parse().ok()?;
            Some((tx, path))
        })
        .collect();
    found.sort();
    Ok(found)
}

fn read(path: &Path) -> Result<Parsed> {
    let bytes = std::fs::read(path)
        .map_err(|e| Error::io(path, &e, Suggestion::command(["omaddons", "status"])))?;
    Ok(parse(&String::from_utf8_lossy(&bytes)))
}

/// Whether `tx`'s journal is one recovery still needs: started (`prepared`) and neither
/// committed nor recovered. One it cannot read (or read at all) counts too: recovery
/// reports it, and leaves it.
pub(crate) fn unfinished(paths: &Paths, tx: &TxId) -> bool {
    let path = paths.journal(tx);
    path.exists()
        && read(&path).map_or(true, |parsed| {
            matches!(parsed.phase, Phase::Prepared | Phase::Swapped)
                || parsed.unrecoverable().is_some()
        })
}

// ---------------------------------------------------------------------------------------
// Reader reporting.

/// An unfinished transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending {
    pub tx: TxId,
    /// The target its `prepared` line names; `None` when it has none or is unreadable.
    pub target: Option<TargetKey>,
    /// Why recovery cannot finish it, when it cannot ([`Unrecoverable`]).
    pub unrecoverable: Option<Unrecoverable>,
}

/// Every journal that is not `committed` (or that recovery cannot read), oldest first.
/// Writes nothing.
pub fn pending(paths: &Paths) -> Result<Vec<Pending>> {
    let mut found = Vec::new();
    for (tx, path) in journals(paths)? {
        let (target, unrecoverable) = match read(&path) {
            Ok(parsed)
                if matches!(parsed.phase, Phase::Committed) && parsed.unrecoverable().is_none() =>
            {
                continue;
            }
            Ok(parsed) => (parsed.target, parsed.unrecoverable()),
            Err(_) => (None, None),
        };
        found.push(Pending {
            tx,
            target,
            unrecoverable,
        });
    }
    Ok(found)
}

/// Unfinished transactions, for read commands: `journal_pending` when the global lock
/// is free (the next mutating command recovers them), `tx_in_progress` when it is held.
/// Writes nothing.
pub fn pending_warnings(paths: &Paths) -> Result<Vec<Warning>> {
    let pending = pending(paths)?;
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    let held = matches!(probe_global(paths)?, LockState::Held { .. });
    Ok(pending
        .into_iter()
        .map(|Pending { tx, target, unrecoverable }| {
            let on = target.map(|t| format!(" on target {t}")).unwrap_or_default();
            if let Some(why) = unrecoverable {
                unrecoverable_warning(&tx, target, why)
            } else if held {
                Warning::new(
                    WarningCode::TxInProgress,
                    format!("transaction {tx}{on} is in progress"),
                )
                .with_fix(Suggestion::text("wait for the running omaddons command to finish"))
            } else {
                Warning::new(
                    WarningCode::JournalPending,
                    format!(
                        "transaction {tx}{on} did not finish; the next omaddons command that changes addons, settings or targets recovers it (dry runs never do)"
                    ),
                )
                .with_fix(Suggestion::command(["omaddons", "status"]))
            }
        })
        .collect())
}

/// What a reader, and another target's recovery, say of a journal this build cannot
/// recover: the same warning both ways, so a run reports it once.
fn unrecoverable_warning(tx: &TxId, target: Option<TargetKey>, why: Unrecoverable) -> Warning {
    let on = target
        .map(|t| format!(" on target {t}"))
        .unwrap_or_default();
    Warning::new(
        WarningCode::JournalPending,
        format!(
            "transaction {tx}{on} did not finish, and this omaddons cannot recover it ({}); it is left as it is",
            why.reason()
        ),
    )
    .with_fix(why.fix())
}

// ---------------------------------------------------------------------------------------
// Recovery.

/// What recovery did to one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    RolledForward,
    RolledBack,
}

/// What recovery runs with. The guard proves the global lock is held.
#[derive(Clone, Copy)]
pub struct RecoveryEnv<'a> {
    pub lock: &'a GlobalLockGuard,
    pub paths: &'a Paths,
    /// The caller's writable scopes. Each journal is also given its own client dir,
    /// once that dir's target marker names the journal's target.
    pub scopes: &'a ScopeTable,
    /// The targets the caller is about to commit. A journal of one of them that cannot
    /// be recovered fails the caller; any other target's becomes a `journal_pending`
    /// warning, so one client's leftover never blocks the others.
    pub own: &'a [TargetKey],
    /// Whether the SavedVariables of the client at this dir may be written now: only
    /// when the command can tell that its game is not running (vision §5.5: never while
    /// it runs). When not, roll-forward leaves an undo's SavedVariables for
    /// `undo <tx> --saved-variables restore`.
    pub sv_writable: &'a dyn Fn(&Path) -> bool,
}

/// What recovering one transaction did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    pub action: RecoveryAction,
    /// Whether the journaled swap was atomic: false when any step is a plain rename (the
    /// fallback for filesystems without `renameat2` flags).
    pub atomic_swap: bool,
    /// `saved_variables_deferred`, when roll-forward left an undo's SavedVariables.
    pub warnings: Vec<Warning>,
    /// How many SavedVariables files roll-forward left as they are.
    pub sv_deferred: usize,
    /// The client snapshot a rollback kept, and how many folders are still in it.
    pub snapshot_kept: Option<(PathBuf, usize)>,
}

/// Recovers every leftover journal. No writer is alive except ones still cleaning up
/// after commit, whose in-flight lock is held and whose journals are skipped. One
/// `journal_recovered` warning per rolled transaction, each followed by what its
/// roll-forward left; committed and unprepared journals are removed silently.
pub fn recover_all(env: &RecoveryEnv<'_>) -> Result<Vec<Warning>> {
    let mut warnings = Vec::new();
    for (tx, path) in journals(env.paths)? {
        if InflightLock::is_live(env.paths, &tx)? {
            continue;
        }
        // Whose it is, for isolation, from its `prepared` line even when that line does
        // not decode: a journal whose owner cannot be told fails the caller as before.
        let parsed = read(&path).ok();
        let target = parsed.as_ref().and_then(|p| p.target);
        let unrecoverable = parsed.as_ref().and_then(Parsed::unrecoverable);
        match recover_one(env, &tx, path) {
            Ok(recovered) => warnings.extend(recovered),
            Err(error) => match (target.filter(|t| !env.own.contains(t)), unrecoverable) {
                (Some(other), Some(why)) => {
                    warnings.push(unrecoverable_warning(&tx, Some(other), why));
                }
                // A warning has no details: a stop's or a wait's reason goes in its text.
                (Some(other), None) => warnings.push(
                    Warning::new(
                        WarningCode::JournalPending,
                        format!(
                            "transaction {tx} on target {other} did not finish and could not be recovered{}: {}",
                            error.details["reason"]
                                .as_str()
                                .map(|why| format!(" ({why})"))
                                .unwrap_or_default(),
                            error.message
                        ),
                    )
                    .with_fix(Suggestion::command(["omaddons", "status"])),
                ),
                (None, _) => return Err(error),
            },
        }
    }
    Ok(warnings)
}

/// Recovery under the global lock `lock`, for a command that takes it without committing
/// a target: one outside the engine that changes something else (`config set`,
/// `targets`, `auth github`, `export --write`), or a change command that turned out to
/// change nothing. Every journal whose writer is gone is finished or rolled back; one
/// that cannot be only warns (`journal_pending`). A command that runs no discovery
/// passes [`no_saved_variables`], so an undo's SavedVariables restore waits
/// (`saved_variables_deferred`). Rolling any journal back or forward rings the doorbell
/// once, for `command` run by `actor` (a commit window rings for its own recovery).
pub fn recover_under(
    lock: &GlobalLockGuard,
    paths: &Paths,
    sv_writable: &dyn Fn(&Path) -> bool,
    command: &str,
    actor: Actor,
) -> Vec<Warning> {
    let left = |error: Error| {
        vec![
            Warning::new(
                WarningCode::JournalPending,
                format!(
                    "unfinished transactions could not be recovered: {}",
                    error.message
                ),
            )
            .with_fix(Suggestion::command(["omaddons", "status"])),
        ]
    };
    let scopes = match ScopeTable::new(paths, &[], None) {
        Ok(scopes) => scopes,
        Err(error) => return left(error),
    };
    let env = RecoveryEnv {
        lock,
        paths,
        scopes: &scopes,
        own: &[],
        sv_writable,
    };
    let warnings = recover_all(&env).unwrap_or_else(left);
    if warnings
        .iter()
        .any(|w| matches!(w.code, WarningCode::JournalRecovered))
    {
        doorbell::ring(paths, command, actor);
    }
    warnings
}

/// For a command that cannot tell whether a client runs: never write SavedVariables.
pub fn no_saved_variables(_: &Path) -> bool {
    false
}

/// For a change command with nothing to commit: a journal whose writer is gone is
/// recovered as a commit would have, under the global lock taken for that alone. With
/// none waiting the lock is never taken (D24), so a repeat install stays lock-free. A
/// lock held past its timeout leaves the journal for the next command.
pub fn recover_waiting(
    paths: &Paths,
    command: &str,
    actor: Actor,
    sv_writable: &dyn Fn(&Path) -> bool,
) -> Vec<Warning> {
    if !recoverable(paths).unwrap_or(false) {
        return Vec::new();
    }
    let Ok(guard) = GlobalLock::acquire(paths, command) else {
        return Vec::new();
    };
    recover_under(&guard, paths, sv_writable, command, actor)
}

/// Whether a journal whose writer is gone waits for recovery.
pub fn recoverable(paths: &Paths) -> Result<bool> {
    for found in pending(paths)? {
        if !InflightLock::is_live(paths, &found.tx)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The scopes a journal's recovery writes with: the caller's, plus the journal's client
/// dir once that dir's target marker names the journal's target (registration writes
/// markers under the lock, before any journal). `None`: the journal names a directory
/// that is not its target's now, which recovery never writes.
fn journal_scopes(env: &RecoveryEnv<'_>, prepared: &Prepared) -> Option<ScopeTable> {
    let marker = crate::discover::registry::read_marker(&prepared.client_dir);
    // ubs:ignore — comparing target keys (public ULIDs), not secrets
    if marker != Some(prepared.target_key) {
        return None;
    }
    let mut scopes = env.scopes.clone();
    scopes.add_client_dir(&prepared.client_dir).ok()?;
    Some(scopes)
}

/// Recovers one transaction by its own writer, in process: an error after `prepared`
/// rolls back, after `swapped` rolls forward. No liveness check: the caller is the
/// writer. Leftovers its in-flight lock still protects are pruned at cleanup.
pub fn recover_tx(env: &RecoveryEnv<'_>, tx: &TxId) -> Result<Option<Recovered>> {
    let path = env.paths.journal(tx);
    if !path.exists() {
        return Ok(None);
    }
    let parsed = read(&path)?;
    recover_parsed(env, tx, &path, &parsed)
}

/// Recovers one journal and reports it.
fn recover_one(env: &RecoveryEnv<'_>, tx: &TxId, path: PathBuf) -> Result<Vec<Warning>> {
    let parsed = read(&path)?;
    let recovered = recover_parsed(env, tx, &path, &parsed)?;
    let (Some(recovered), Some(prepared)) = (recovered, &parsed.prepared) else {
        return Ok(Vec::new());
    };
    let what = match recovered.action {
        RecoveryAction::RolledForward => "completed it (rolled forward)",
        RecoveryAction::RolledBack => "undid it (rolled back)",
    };
    let kept = match &recovered.snapshot_kept {
        Some((snapshot, 1)) => format!(
            "; its snapshot {} is kept: 1 folder is still in it",
            snapshot.display()
        ),
        Some((snapshot, n)) => format!(
            "; its snapshot {} is kept: {n} folders are still in it",
            snapshot.display()
        ),
        None => String::new(),
    };
    let mut warnings = vec![
        Warning::new(
            WarningCode::JournalRecovered,
            format!(
                "transaction {tx} on target {} was interrupted; recovery {what}{kept}",
                prepared.target_key
            ),
        )
        .with_fix(Suggestion::command(["omaddons", "history", "--limit", "5"])),
    ];
    warnings.extend(recovered.warnings);
    Ok(warnings)
}

/// Rolls forward or back by phase, then prunes what the transaction left: its snapshots
/// too when it never committed (rolled back, or never prepared). A journal this build
/// cannot read ([`Parsed::unrecoverable`]) is left whole, with everything it owns, and
/// fails with its `journal_pending` details.
fn recover_parsed(
    env: &RecoveryEnv<'_>,
    tx: &TxId,
    path: &Path,
    parsed: &Parsed,
) -> Result<Option<Recovered>> {
    if let Some(why) = parsed.unrecoverable() {
        return Err(unrecoverable(tx, parsed, why));
    }
    let paths = env.paths;
    let scoped = match &parsed.prepared {
        Some(prepared) => journal_scopes(env, prepared),
        None => None,
    };
    let unfinished = matches!(parsed.phase, Phase::Prepared | Phase::Swapped);
    if let (Some(prepared), None, true) = (&parsed.prepared, &scoped, unfinished) {
        return Err(Error::new(
            ErrorCode::ScopeDenied,
            format!(
                "transaction {tx} names {}, whose marker is not target {}'s; recovery leaves it for manual repair",
                prepared.client_dir.display(),
                prepared.target_key
            ),
            Suggestion::command(["omaddons", "status"]),
        ));
    }
    let env = &RecoveryEnv {
        scopes: scoped.as_ref().unwrap_or(env.scopes),
        ..*env
    };
    let scopes = env.scopes;
    // A journal that renames `WTF/` is recovered as what wrote it, by its content, not
    // its kind (the undo of a restore is an undo): only `BackupRestore` may move `WTF`.
    let writes_wtf = parsed.prepared.as_ref().is_some_and(|p| p.writes_wtf);
    let writer = if writes_wtf {
        Writer::BackupRestore
    } else {
        Writer::Mutating
    };
    // ... and never while the game runs, or may (M3-D29): a step may have moved `WTF/`.
    // In process too: the check is made now, not trusted from before the failure.
    if let Some(prepared) = &parsed.prepared
        && writes_wtf
        && unfinished
        && !parsed.steps.is_empty()
        && !(env.sv_writable)(&prepared.client_dir)
    {
        return Err(waits_for_the_game(tx, prepared));
    }
    let mut s = WriteSession::untracked(writer, scopes, "recovery");
    let atomic_swap = !parsed
        .steps
        .iter()
        .any(|step| matches!(step.op, StepOp::Rename));
    let mut recovered = match (&parsed.prepared, parsed.phase) {
        (None, _) | (_, Phase::Unprepared) => None,
        (Some(_), Phase::Committed) => None,
        (Some(prepared), Phase::Swapped) => {
            let left = roll_forward(&mut s, env, path, prepared, parsed.torn_tail)?;
            Some(Recovered {
                action: RecoveryAction::RolledForward,
                atomic_swap,
                sv_deferred: left.as_ref().map_or(0, |(files, _)| *files),
                warnings: left.into_iter().map(|(_, warning)| warning).collect(),
                snapshot_kept: None,
            })
        }
        (Some(prepared), Phase::Prepared) if prepared.writes_wtf => {
            // Conservative (M3-D24 (c), until D14's preservation): a step in neither its
            // before nor its after state, or a stage nothing else holds, stops recovery
            // before any cleanup. The live `WTF/` is put back first when the steps allow.
            roll_back(&mut s, &parsed.steps)
                .map_err(|err| stopped(tx, prepared, Stop::UnexpectedState, &err.message))?;
            stage_accounted_for(tx, prepared)?;
            Some(Recovered {
                action: RecoveryAction::RolledBack,
                atomic_swap,
                warnings: Vec::new(),
                sv_deferred: 0,
                snapshot_kept: None,
            })
        }
        (Some(_), Phase::Prepared) => {
            roll_back(&mut s, &parsed.steps)?;
            Some(Recovered {
                action: RecoveryAction::RolledBack,
                atomic_swap,
                warnings: Vec::new(),
                sv_deferred: 0,
                snapshot_kept: None,
            })
        }
    };
    let mut victims = Vec::new();
    // The client side only where the journal's directory is its target's.
    if let (Some(prepared), Some(_)) = (&parsed.prepared, &scoped)
        && prepared.commit.stage_dir.exists()
    {
        victims.push(prepared.commit.stage_dir.clone());
    }
    victims.push(path.to_path_buf());
    if paths.inflight_lock(tx).exists() {
        victims.push(paths.inflight_lock(tx));
    }
    fsx::prune(
        scopes,
        PruneReason::PostCommitCleanup,
        &victims,
        LockUse::Held(env.lock),
    )?;
    // Rolled back or never prepared, the transaction will never commit: its snapshots
    // are retention's garbage (vision §5.5). Only the state half is known without a
    // `prepared` line, and a client half is written only after one. Best effort:
    // retention sweeps what is left once it is old enough.
    if matches!(parsed.phase, Phase::Prepared | Phase::Unprepared) {
        let client = parsed
            .prepared
            .as_ref()
            .map(|p| ClientPaths::new(&p.client_dir));
        let dead = super::retention::dead_snapshots(paths, client.as_ref(), tx);
        if !dead.victims.is_empty() {
            let _ = fsx::prune(
                scopes,
                PruneReason::Retention,
                &dead.victims,
                LockUse::Held(env.lock),
            );
        }
        if let Some(recovered) = recovered.as_mut() {
            recovered.snapshot_kept = dead.kept;
        }
    }
    Ok(recovered)
}

/// The failure of a journal recovery leaves as it is: `state_too_new` for a newer major
/// version, `state_corrupt` for lines this build cannot read, each with
/// `details.journal_pending` (the transaction), `details.reason` and `details.target`.
fn unrecoverable(tx: &TxId, parsed: &Parsed, why: Unrecoverable) -> Error {
    let on = parsed
        .target
        .map(|t| format!(" on target {t}"))
        .unwrap_or_default();
    let (code, message) = match why {
        Unrecoverable::TooNew => (
            ErrorCode::StateTooNew,
            format!(
                "transaction {tx}{on} was started by a newer omaddons (journal version {}; this one reads {CURRENT}), so it is left as it is",
                parsed
                    .version
                    .map_or_else(|| "unknown".to_owned(), |v| v.to_string())
            ),
        ),
        Unrecoverable::Unreadable => (
            ErrorCode::StateCorrupt,
            format!(
                "transaction {tx}{on} did not finish, and its journal holds lines this omaddons cannot read, so it is left for manual repair"
            ),
        ),
    };
    Error::new(code, message, why.fix()).details(serde_json::json!({
        "journal_pending": tx,
        "reason": why.reason(),
        "target": parsed.target,
    }))
}

/// A `writes_wtf` journal left while its client runs, or may (M3-D29): `game_running`,
/// with the transaction it waits on.
fn waits_for_the_game(tx: &TxId, prepared: &Prepared) -> Error {
    Error::new(
        ErrorCode::GameRunning,
        format!(
            "transaction {tx} on target {} was putting settings back and did not finish; it waits for the game to exit",
            prepared.target_key
        ),
        Suggestion::text("quit the game; the next change finishes or undoes it"),
    )
    .details(serde_json::json!({
        "reason": "saved_variables",
        "journal_pending": tx,
        "target": prepared.target_key,
    }))
}

/// Why a `writes_wtf` recovery stopped before any cleanup (M3-D24 (c)); D14 replaces
/// the stop with a journaled preservation of what it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// A step in neither its before nor its after state: never attributed, the live
    /// path never touched.
    UnexpectedState,
    /// Something displaced has no durable home yet: a stage played on since it was
    /// journaled, or one whose source backup is gone.
    PreservationIncomplete,
}

impl Stop {
    fn reason(self) -> &'static str {
        match self {
            Self::UnexpectedState => "unexpected_state",
            Self::PreservationIncomplete => "preservation_incomplete",
        }
    }
}

/// A stopped recovery's failure: journal, stage, snapshots and in-flight lock are all
/// kept, and every later change on the client fails until it is resolved.
fn stopped(tx: &TxId, prepared: &Prepared, stop: Stop, why: &str) -> Error {
    let code = match stop {
        Stop::UnexpectedState => ErrorCode::Internal,
        Stop::PreservationIncomplete => ErrorCode::StateCorrupt,
    };
    Error::new(
        code,
        format!(
            "transaction {tx} on target {} was putting settings back and did not finish; recovery stopped and keeps everything: {why}",
            prepared.target_key
        ),
        Suggestion::text(
            "see \"journal_pending that does not clear\" under Troubleshooting in the README",
        ),
    )
    .details(serde_json::json!({
        "reason": stop.reason(),
        "journal_pending": tx,
        "target": prepared.target_key,
    }))
}

/// After a `writes_wtf` rollback the stage holds the tree that was to be restored
/// again. It is pruned only when it is still the copy the journal recorded (stamps
/// unchanged) and its source backup still exists with the identity it had: otherwise it
/// may be the only copy of what someone played on, or of the backup's content.
fn stage_accounted_for(tx: &TxId, prepared: &Prepared) -> Result<()> {
    let staged = prepared.commit.stage_dir.join("WTF");
    if !staged.exists() {
        return Ok(());
    }
    let now = fsx::StampDigest::compute(&staged)
        .map_err(|e| Error::io(&staged, &e, Suggestion::command(["omaddons", "status"])))?;
    // ubs:ignore — comparing stat digests of a tree (evidence of change), not secrets
    if prepared.staged_stamp.as_deref() != Some(now.to_string().as_str()) {
        return Err(stopped(
            tx,
            prepared,
            Stop::PreservationIncomplete,
            "the staged WTF/ changed since it was journaled, so it is kept as it is",
        ));
    }
    let source_gone = prepared.source_backup.is_none_or(|source| {
        let client = ClientPaths::new(&prepared.client_dir);
        let dir = crate::backup::backup_dir(&client, &source.id);
        !same(FileId::of(&dir), Some(source.dir))
    });
    if source_gone {
        return Err(stopped(
            tx,
            prepared,
            Stop::PreservationIncomplete,
            "the backup the stage was copied from is gone, so the stage is its last copy",
        ));
    }
    Ok(())
}

/// Writes the payload's state (each part only if not already written), then `committed`.
/// Returns the SavedVariables files an undo's restore left, with the warning that says
/// so.
fn roll_forward(
    s: &mut WriteSession<'_>,
    env: &RecoveryEnv<'_>,
    journal: &Path,
    prepared: &Prepared,
    torn_tail: bool,
) -> Result<Option<(usize, Warning)>> {
    failpoint::hit("engine.recover.forward")
        .map_err(|e| Error::io(journal, &e, Suggestion::command(["omaddons", "status"])))?;
    let paths = env.paths;
    let key = prepared.target_key;
    let commit = &prepared.commit;
    let left = match &commit.sv_restore {
        Some(sv) => finish_saved_variables(env, prepared, sv),
        None => None,
    };
    ensure_dir_all(s, &paths.targets_dir())?;
    let record_path = paths.target_record(&key);
    let current = store::load::<TargetRecord>(&record_path)?;
    let behind = current
        .as_ref()
        .is_none_or(|c| c.value.generation < commit.record.generation);
    if behind {
        store::save(
            s,
            &record_path,
            &commit.record,
            current.as_ref().map(|c| &c.status),
        )?;
    }

    let index_path = paths.files_index(&key);
    let loaded = store::load::<FilesIndex>(&index_path)?;
    let mut index = loaded.as_ref().map_or_else(
        || FilesIndex {
            key,
            written_at_ns: 0,
            folders: Default::default(),
        },
        |l| l.value.clone(),
    );
    commit.index.apply(&mut index);
    index.written_at_ns = Timestamp::now().as_nanosecond();
    store::save(s, &index_path, &index, loaded.as_ref().map(|l| &l.status))?;

    // A crash mid-append left this transaction's line cut short, perhaps only its
    // newline: closed off, a whole line reads back and is not appended twice, and a
    // fragment stays apart from the line appended after it.
    history::close_torn_tail(s, paths)?;
    let filter = HistoryFilter {
        target: Some(key),
        ..HistoryFilter::default()
    };
    if !history::read(paths, &filter)?
        .entries
        .iter()
        // ubs:ignore — comparing transaction ids, not secrets
        .any(|e| e.tx == prepared.tx)
    {
        // A restore left for later is pending, as a live undo's deferred one is.
        let mut line = commit.history.clone();
        line.sv_deferred |= left.is_some();
        history::append(s, paths, &line)?;
    }
    // Never after a torn last line: the line would merge with the fragment, and the next
    // append would leave that merge a complete line no build can read. Without
    // `committed`, a crash before the prune only rolls forward again, which writes
    // nothing twice.
    if !torn_tail {
        append_line(s, journal, &JournalLine::Committed)?;
    }
    Ok(left)
}

/// An interrupted undo's SavedVariables restore. It runs only when the client's
/// SavedVariables may be written now; otherwise, or when it fails, the files stay as
/// they are and `saved_variables_deferred` names the follow-up, exactly as a live
/// `--saved-variables auto` undo does. Never a recovery failure, so roll-forward cannot
/// get stuck on SavedVariables. `None` when nothing is left to restore.
fn finish_saved_variables(
    env: &RecoveryEnv<'_>,
    prepared: &Prepared,
    sv: &super::undo::SvRestore,
) -> Option<(usize, Warning)> {
    let client = crate::state::paths::ClientPaths::new(&prepared.client_dir);
    let (tx, key) = (prepared.tx, prepared.target_key);
    let failed = if (env.sv_writable)(&prepared.client_dir) {
        let restored = failpoint::hit("engine.recover.sv")
            .map_err(|e| {
                Error::io(
                    &prepared.client_dir,
                    &e,
                    Suggestion::command(["omaddons", "status"]),
                )
            })
            .and_then(|()| {
                let mut undo = WriteSession::untracked(Writer::Undo, env.scopes, "recovery")
                    .with_owned_folders(sv.owned_folders());
                super::undo::restore_saved_variables(
                    &mut undo,
                    &client,
                    sv,
                    &tx,
                    &super::undo::SvCopies::default(),
                    &mut super::undo::SvHashes::default(),
                )
            });
        match restored {
            Ok(_) => return None,
            Err(error) => Some(error.code),
        }
    } else {
        None
    };
    let files = super::undo::unrestored(&client, sv);
    if files == 0 {
        return None;
    }
    let message = match failed {
        Some(code) => format!(
            "transaction {tx} on target {key} was finished by recovery, but {files} SavedVariables files could not be restored ({code}) and were left as they are"
        ),
        None => format!(
            "transaction {tx} on target {key} was finished by recovery while its game client was running (or not found by this command), so {files} SavedVariables files were left as they are"
        ),
    };
    let warning = Warning::new(WarningCode::SavedVariablesDeferred, message)
        .with_fix(super::undo::sv_follow_up(&sv.from));
    Some((files, warning))
}

/// Undoes the steps that ran, last first. The failpoint `engine.recover.step` sits
/// after each undo, so a crash between replayed steps can be simulated; the next
/// recovery skips what was already undone.
fn roll_back(s: &mut WriteSession<'_>, steps: &[StepRecord]) -> Result<()> {
    for (at, step) in steps.iter().enumerate().rev() {
        if ran_after(step, &steps[..at])? {
            undo(s, step)?;
        }
        failpoint::hit("engine.recover.step")
            .map_err(|e| Error::io(&step.from, &e, Suggestion::command(["omaddons", "status"])))?;
    }
    Ok(())
}

/// [`ran`], for a step recorded after `earlier`. Steps run first to last and roll back
/// last first, so while one of `earlier` is in its before state this one cannot have
/// run, or was undone already: an exchange's old side moved on from the stage path, and
/// a recovery that died after undoing both put the staged tree back at that path, which
/// this step then sees in neither of its states. Anything else in neither state still
/// stops.
fn ran_after(step: &StepRecord, earlier: &[StepRecord]) -> Result<bool> {
    match ran(step) {
        Err(neither) if earlier.iter().any(|e| matches!(ran(e), Ok(false))) => {
            let _ = neither;
            Ok(false)
        }
        decided => decided,
    }
}

/// Whether a recorded step's rename happened, from the identities now at its paths.
fn ran(step: &StepRecord) -> Result<bool> {
    let (now_from, now_to) = (FileId::of(&step.from), FileId::of(&step.to));
    let before = (step.from_id, step.to_id);
    let after = match step.op {
        StepOp::Exchange => (step.to_id, step.from_id),
        StepOp::Noreplace | StepOp::Rename => (None, step.from_id),
    };
    let is =
        |(from, to): (Option<FileId>, Option<FileId>)| same(now_from, from) && same(now_to, to);
    if is(before) {
        return Ok(false);
    }
    if is(after) {
        return Ok(true);
    }
    Err(Error::internal(format!(
        "journal step {} ({} → {}) matches neither its before nor its after state; leaving it for manual repair",
        step.n,
        step.from.display(),
        step.to.display()
    )))
}

fn undo(s: &mut WriteSession<'_>, step: &StepRecord) -> Result<()> {
    match step.op {
        StepOp::Exchange => {
            mutate(
                s,
                &step.from,
                Op::Rename {
                    to: &step.to,
                    mode: RenameMode::Exchange,
                },
            )?;
        }
        StepOp::Noreplace | StepOp::Rename => {
            let back = |mode| Op::Rename {
                to: &step.from,
                mode,
            };
            match mutate(s, &step.to, back(RenameMode::NoReplace)) {
                // `ran` saw nothing at `from`, so a plain rename cannot overwrite.
                Err(FsxError::Unsupported) => {
                    mutate(s, &step.to, back(RenameMode::Replace))?;
                }
                other => {
                    other?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prepared_line_with_a_files_index_round_trips() {
        use crate::model::{Actor, FolderName};
        use crate::state::files_index::FileEntry;
        use crate::state::history::{FolderOp, HistoryOp, TxResult};
        let tx = TxId::new();
        let key = TargetKey::new();
        let entry = FileEntry {
            path: "Foo.toc".into(),
            size: 3,
            mtime_ns: 1_760_000_000_123_456_789,
            sha256: "ab".repeat(32),
        };
        let line = JournalLine::Prepared(Box::new(Prepared {
            schema_version: CURRENT,
            tx,
            target_key: key,
            client_dir: "/c/_retail_".into(),
            kind: TxKind::Install,
            started_at: Timestamp::now(),
            creates_dirs: vec!["/c/_retail_/Interface/AddOns".into()],
            writes_wtf: false,
            staged_stamp: None,
            source_backup: None,
            commit: CommitPayload {
                record: TargetRecord::empty(key),
                index: IndexChanges {
                    upsert: vec![FolderIndex::new("Foo", vec![entry])],
                    remove: vec!["old".into()],
                },
                history: HistoryEntry {
                    schema_version: history::CURRENT,
                    tx,
                    kind: TxKind::Install,
                    at: Timestamp::now(),
                    target: key,
                    actor: Actor::Agent,
                    reason: Some("why".into()),
                    argv: vec!["omaddons".into()],
                    ops: vec![HistoryOp {
                        op: FolderOp::Add,
                        folder: FolderName::new("Foo").unwrap(),
                        addon: "Foo".into(),
                        edit: None,
                        backup: None,
                        from: None,
                        files: None,
                    }],
                    result: TxResult::Committed,
                    reverts: None,
                    sv_deferred: false,
                    created_addons_dir: true,
                    run: Some(crate::model::RunId::new()),
                    backup: None,
                },
                stage_dir: "/c/_retail_/Interface/.omaddons/stage/x".into(),
                sv_restore: None,
            },
        }));
        let text = format!("{}\n", serde_json::to_string(&line).unwrap());
        let parsed = parse(&text);
        assert_eq!(
            (parsed.phase, parsed.unknown_lines),
            (Phase::Prepared, 0),
            "{text}"
        );
        assert_eq!(
            parsed.prepared.map(|p| JournalLine::Prepared(Box::new(p))),
            Some(line)
        );
    }

    #[test]
    fn lines_round_trip_and_tag_their_type() {
        let step = JournalLine::Step(StepRecord {
            n: 1,
            op: StepOp::Exchange,
            from: "/c/Interface/.omaddons/stage/t/Foo".into(),
            to: "/c/Interface/AddOns/Foo".into(),
            purpose: StepPurpose::SwapIn,
            from_id: Some(FileId {
                dev: 1,
                ino: 2,
                born_ns: Some(3),
            }),
            to_id: None,
        });
        let json = serde_json::to_value(&step).unwrap();
        assert_eq!(
            (
                json["type"].as_str(),
                json["op"].as_str(),
                json["purpose"].as_str()
            ),
            (Some("step"), Some("exchange"), Some("swap_in"))
        );
        assert_eq!(serde_json::from_value::<JournalLine>(json).unwrap(), step);
        for marker in [JournalLine::Swapped, JournalLine::Committed] {
            let json = serde_json::to_string(&marker).unwrap();
            assert!(json.starts_with("{\"type\":"), "{json}");
            assert_eq!(serde_json::from_str::<JournalLine>(&json).unwrap(), marker);
        }
    }

    fn step_line(n: u32) -> String {
        format!(
            "{{\"type\":\"step\",\"n\":{n},\"op\":\"noreplace\",\"from\":\"/a\",\"to\":\"/b\",\"purpose\":\"old_to_snapshot\",\"from_id\":null,\"to_id\":null}}\n"
        )
    }

    #[test]
    fn phases_torn_lines_and_unknown_types() {
        let empty = parse("");
        assert_eq!(
            (empty.phase, empty.prepared.is_none(), empty.unrecoverable()),
            (Phase::Unprepared, true, None)
        );
        let torn = parse("{\"type\":\"prepa");
        assert_eq!(
            (torn.phase, torn.torn_tail, torn.lines, torn.unrecoverable()),
            (Phase::Unprepared, true, 0, None),
            "a torn prepared line means nothing ran"
        );

        let no_prepared = format!("{}{}", step_line(1), "{\"type\":\"swapped\"}\n");
        let parsed = parse(&no_prepared);
        assert_eq!(
            (parsed.phase, parsed.steps.len(), parsed.unknown_lines),
            (Phase::Unprepared, 1, 1)
        );
        assert_eq!(
            parsed.unrecoverable(),
            Some(Unrecoverable::Unreadable),
            "lines without a prepared one never mean nothing ran"
        );

        let text = format!(
            "{}{}{{\"type\":\"future\",\"x\":1}}\n{}",
            step_line(1),
            step_line(2),
            "{\"type\":\"step\",\"n\":3,\"op\""
        );
        let parsed = parse(&text);
        assert_eq!(
            (
                parsed.steps.len(),
                parsed.unknown_lines,
                parsed.torn_tail,
                parsed.lines
            ),
            (2, 1, true, 3),
            "torn third step ignored, the unknown line counted"
        );
        let complete_but_invalid = format!("{}not json\n", step_line(1));
        let parsed = parse(&complete_but_invalid);
        assert_eq!(
            (parsed.steps.len(), parsed.torn_tail, parsed.unknown_lines),
            (1, true, 0),
            "an invalid last line is a fragment recovery's append closed off"
        );
        let corrupt = format!("not json\n{}", step_line(1));
        let parsed = parse(&corrupt);
        assert_eq!(
            (parsed.unknown_lines, parsed.torn_tail),
            (1, false),
            "corruption before the end is a line it cannot read"
        );
    }

    #[test]
    fn step_identity_decides_whether_it_ran() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let exchange = StepRecord {
            n: 1,
            op: StepOp::Exchange,
            from_id: FileId::of(&a),
            to_id: FileId::of(&b),
            from: a.clone(),
            to: b.clone(),
            purpose: StepPurpose::SwapIn,
        };
        assert!(!ran(&exchange).unwrap());
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &a,
            rustix::fs::CWD,
            &b,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .unwrap();
        assert!(ran(&exchange).unwrap());

        let c = dir.path().join("c");
        let rename = StepRecord {
            n: 2,
            op: StepOp::Noreplace,
            from_id: FileId::of(&a),
            to_id: None,
            from: a.clone(),
            to: c.clone(),
            purpose: StepPurpose::OldToSnapshot,
        };
        assert!(!ran(&rename).unwrap());
        std::fs::rename(&a, &c).unwrap();
        assert!(ran(&rename).unwrap());
        std::fs::create_dir(&a).unwrap();
        assert!(
            ran(&rename).is_err(),
            "something new at `from` is neither state"
        );
    }

    /// An exchange of `stage` with `live`, then the old side moved on from `stage` to
    /// `kept`: the shape of a replace, and of a restore. Rolled back whole by a recovery
    /// that died before pruning, it rolls back again as a no-op; with a foreign tree at
    /// `live` instead, it still stops.
    #[test]
    fn a_rollback_repeated_after_both_steps_were_undone_skips_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let client = root.join("_retail_");
        let tx = TxId::new();
        let meta = client.join("Interface/.omaddons");
        let stage = meta.join(format!("stage/{tx}/Foo"));
        let live = client.join("Interface/AddOns/Foo");
        let kept = meta.join(format!("snapshots/{tx}/AddOns/Foo"));
        for (dir, file) in [(&stage, "new"), (&live, "old")] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(file), "").unwrap();
        }
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        let exchange = StepRecord {
            n: 1,
            op: StepOp::Exchange,
            from_id: FileId::of(&stage),
            to_id: FileId::of(&live),
            from: stage.clone(),
            to: live.clone(),
            purpose: StepPurpose::SwapIn,
        };
        let swap = |a: &Path, b: &Path| {
            let flags = rustix::fs::RenameFlags::EXCHANGE;
            rustix::fs::renameat_with(rustix::fs::CWD, a, rustix::fs::CWD, b, flags).unwrap();
        };
        swap(&stage, &live);
        let onward = StepRecord {
            n: 2,
            op: StepOp::Noreplace,
            from_id: FileId::of(&stage),
            to_id: None,
            from: stage.clone(),
            to: kept.clone(),
            purpose: StepPurpose::OldToSnapshot,
        };
        std::fs::rename(&stage, &kept).unwrap();
        let steps = [exchange, onward];
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
        let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "test");

        // A recovery that undid both and died: the staged tree is back at `stage`.
        std::fs::rename(&kept, &stage).unwrap();
        swap(&stage, &live);
        assert!(
            ran(&steps[1]).is_err(),
            "on its own, the step is in neither state"
        );
        roll_back(&mut s, &steps).unwrap();
        assert!(stage.join("new").exists() && live.join("old").exists());

        // A tree recovery does not know at `live`: the exchange is in neither state.
        swap(&stage, &live);
        std::fs::rename(&stage, &kept).unwrap();
        std::fs::rename(&live, root.join("aside")).unwrap();
        std::fs::create_dir(&live).unwrap();
        assert!(roll_back(&mut s, &steps).is_err());
        assert!(
            live.read_dir().unwrap().next().is_none(),
            "the foreign tree stays"
        );
    }

    /// What a reboot does to a recorded identity: another device number, same inode.
    fn renumbered(id: Option<FileId>) -> Option<FileId> {
        id.map(|id| FileId {
            dev: id.dev ^ 0x5a5a,
            ..id
        })
    }

    #[test]
    fn a_renumbered_device_still_decides_whether_a_step_ran() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let exchange = StepRecord {
            n: 1,
            op: StepOp::Exchange,
            from_id: renumbered(FileId::of(&a)),
            to_id: renumbered(FileId::of(&b)),
            from: a.clone(),
            to: b.clone(),
            purpose: StepPurpose::SwapIn,
        };
        let noreplace = StepRecord {
            n: 2,
            op: StepOp::Noreplace,
            from_id: renumbered(FileId::of(&b)),
            to_id: None,
            from: b.clone(),
            to: c.clone(),
            purpose: StepPurpose::OldToSnapshot,
        };
        assert!(!ran(&exchange).unwrap() && !ran(&noreplace).unwrap());
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &a,
            rustix::fs::CWD,
            &b,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .unwrap();
        assert!(ran(&exchange).unwrap());
        // `b` now holds what was `a`: the noreplace step recorded the old `b`.
        let moved = StepRecord {
            from_id: renumbered(FileId::of(&b)),
            ..noreplace
        };
        std::fs::rename(&b, &c).unwrap();
        assert!(ran(&moved).unwrap());
    }

    #[test]
    fn a_legacy_step_without_birth_time_decides_by_inode() {
        let dir = tempfile::tempdir().unwrap();
        let (a, c) = (dir.path().join("a"), dir.path().join("c"));
        std::fs::create_dir(&a).unwrap();
        let ino = FileId::of(&a).unwrap().ino;
        let line = format!(
            "{{\"type\":\"step\",\"n\":1,\"op\":\"noreplace\",\"from\":{},\"to\":{},\"purpose\":\"old_to_snapshot\",\"from_id\":{{\"dev\":7,\"ino\":{ino}}},\"to_id\":null}}\n",
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&c).unwrap(),
        );
        let parsed = parse(&line);
        let step = &parsed.steps[0];
        assert_eq!(step.from_id.unwrap().born_ns, None);
        assert!(!ran(step).unwrap());
        std::fs::rename(&a, &c).unwrap();
        assert!(ran(step).unwrap());
        assert!(
            serde_json::to_string(step)
                .unwrap()
                .contains("\"from_id\":{\"dev\":7,\"ino\""),
            "no born_ns key written when unknown"
        );
    }

    #[test]
    fn a_reused_inode_with_another_birth_time_is_neither_state() {
        let dir = tempfile::tempdir().unwrap();
        let (a, c) = (dir.path().join("a"), dir.path().join("c"));
        std::fs::create_dir(&a).unwrap();
        let now = FileId::of(&a).unwrap();
        // As if `a` had been removed and a new directory had got its inode number.
        let step = StepRecord {
            n: 1,
            op: StepOp::Noreplace,
            from_id: Some(FileId {
                born_ns: Some(now.born_ns.unwrap_or(0) - 1_000_000_000),
                ..now
            }),
            to_id: None,
            from: a.clone(),
            to: c.clone(),
            purpose: StepPurpose::OldToSnapshot,
        };
        if now.born_ns.is_some() {
            assert!(ran(&step).is_err(), "a different file at `from`");
        } else {
            // A filesystem without birth times decides by inode alone.
            assert!(!ran(&step).unwrap());
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_crash_between_replayed_steps_is_recovered_by_the_next_recovery() {
        use crate::fsx::GlobalLock;
        use crate::model::{Actor, FolderName};
        use crate::state::history::{FolderOp, HistoryOp, TxResult};

        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let client = root.join("_retail_");
        let addons = client.join("Interface/AddOns");
        let tx = TxId::new();
        let stage_dir = client.join(format!("Interface/.omaddons/stage/{tx}"));
        for (dir, text) in [
            (addons.join("Foo"), "old"),
            (stage_dir.join("Foo"), "new"),
            (stage_dir.join("Bar"), "bar"),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("x.lua"), text).unwrap();
        }
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
        let key = TargetKey::new();
        let prepared = Prepared {
            schema_version: CURRENT,
            tx,
            target_key: key,
            client_dir: client.clone(),
            kind: TxKind::Install,
            started_at: Timestamp::now(),
            creates_dirs: Vec::new(),
            writes_wtf: false,
            staged_stamp: None,
            source_backup: None,
            commit: CommitPayload {
                record: TargetRecord {
                    key,
                    generation: 1,
                    addons: Default::default(),
                },
                index: IndexChanges::default(),
                history: HistoryEntry {
                    schema_version: history::CURRENT,
                    tx,
                    kind: TxKind::Install,
                    at: Timestamp::now(),
                    target: key,
                    actor: Actor::Agent,
                    reason: None,
                    argv: vec!["omaddons".into(), "install".into()],
                    ops: vec![HistoryOp {
                        op: FolderOp::Add,
                        folder: FolderName::new("Bar").unwrap(),
                        addon: "Bar".into(),
                        edit: None,
                        backup: None,
                        from: None,
                        files: None,
                    }],
                    result: TxResult::Committed,
                    reverts: None,
                    sv_deferred: false,
                    created_addons_dir: false,
                    run: None,
                    backup: None,
                },
                stage_dir: stage_dir.clone(),
                sv_restore: None,
            },
        };
        let steps = [
            Step {
                op: StepOp::Exchange,
                from: stage_dir.join("Foo"),
                to: addons.join("Foo"),
                purpose: StepPurpose::SwapIn,
            },
            Step {
                op: StepOp::Noreplace,
                from: stage_dir.join("Bar"),
                to: addons.join("Bar"),
                purpose: StepPurpose::SwapIn,
            },
        ];
        {
            let lock = GlobalLock::acquire(&paths, "test").unwrap();
            let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "test");
            let mut journal = Journal::create(&mut s, &paths, &lock, prepared).unwrap();
            for step in steps.clone() {
                let mode = match step.op {
                    StepOp::Exchange => RenameMode::Exchange,
                    StepOp::Noreplace | StepOp::Rename => RenameMode::NoReplace,
                };
                journal.step(&mut s, step.clone()).unwrap();
                mutate(&mut s, &step.from, Op::Rename { to: &step.to, mode }).unwrap();
            }
        }
        let read = |p: &Path| std::fs::read_to_string(p.join("x.lua")).ok();
        assert_eq!(
            (
                read(&addons.join("Foo")).as_deref(),
                read(&addons.join("Bar")).as_deref()
            ),
            (Some("new"), Some("bar"))
        );

        crate::engine::testing::mark(&client, key);
        let lock = GlobalLock::acquire(&paths, "test").unwrap();
        let env = RecoveryEnv {
            lock: &lock,
            paths: &paths,
            scopes: &scopes,
            own: std::slice::from_ref(&key),
            sv_writable: &|_| false,
        };
        let crashed = failpoint::with_spec("engine.recover.step=error", || recover_all(&env));
        assert!(crashed.is_err());
        assert_eq!(
            (
                read(&addons.join("Foo")).as_deref(),
                read(&addons.join("Bar"))
            ),
            (Some("new"), None),
            "Bar was undone, then the crash"
        );

        let warnings = recover_all(&env).unwrap();
        assert_eq!(read(&addons.join("Foo")).as_deref(), Some("old"));
        assert!(
            !addons.join("Bar").exists() && !stage_dir.exists() && !paths.journal(&tx).exists()
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, WarningCode::JournalRecovered);
        assert!(
            warnings[0].message.contains("rolled back"),
            "{}",
            warnings[0].message
        );
    }

    /// An undo of `from` interrupted after `swapped`, before its SavedVariables restore:
    /// `Foo.lua` was `before` when `from` committed and is `after` now.
    struct SvScene {
        _dir: tempfile::TempDir,
        paths: Paths,
        scopes: ScopeTable,
        client: PathBuf,
        live: PathBuf,
        from: TxId,
        undo: TxId,
    }

    impl SvScene {
        const REL: &str = "Account/ACCT/SavedVariables/Foo.lua";

        fn new() -> Self {
            use crate::engine::undo::SvRestore;
            use crate::fsx::GlobalLock;
            use crate::model::Actor;
            use crate::state::history::TxResult;

            let dir = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();
            let client = root.join("_retail_");
            std::fs::create_dir_all(client.join("Interface/AddOns")).unwrap();
            let (from, undo) = (TxId::new(), TxId::new());
            let snapshot = client.join(format!("Interface/.omaddons/snapshots/{from}/WTF"));
            let live = client.join("WTF").join(Self::REL);
            for (path, text) in [
                (snapshot.join(Self::REL), "FooDB = { v = 0 }"),
                (live.clone(), "FooDB = { v = 1 }"),
            ] {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, text).unwrap();
            }
            let sha256 = crate::engine::swap::sha256_file(&snapshot.join(Self::REL)).unwrap();
            let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
            let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
            let key = TargetKey::new();
            crate::engine::testing::mark(&client, key);
            let prepared = Prepared {
                schema_version: CURRENT,
                tx: undo,
                target_key: key,
                client_dir: client.clone(),
                kind: TxKind::Undo,
                started_at: Timestamp::now(),
                creates_dirs: Vec::new(),
                writes_wtf: false,
                staged_stamp: None,
                source_backup: None,
                commit: CommitPayload {
                    record: TargetRecord {
                        key,
                        generation: 2,
                        addons: Default::default(),
                    },
                    index: IndexChanges::default(),
                    history: HistoryEntry {
                        schema_version: history::CURRENT,
                        tx: undo,
                        kind: TxKind::Undo,
                        at: Timestamp::now(),
                        target: key,
                        actor: Actor::Agent,
                        reason: None,
                        argv: vec!["omaddons".into(), "undo".into()],
                        ops: Vec::new(),
                        result: TxResult::Committed,
                        reverts: Some(from),
                        sv_deferred: false,
                        created_addons_dir: false,
                        run: None,
                        backup: None,
                    },
                    stage_dir: client.join(format!("Interface/.omaddons/stage/{undo}")),
                    sv_restore: Some(SvRestore {
                        from,
                        files: vec![crate::engine::swap::SvEntry {
                            path: Self::REL.into(),
                            sha256: Some(sha256),
                        }],
                    }),
                },
            };
            {
                let lock = GlobalLock::acquire(&paths, "test").unwrap();
                let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "test");
                let mut journal = Journal::create(&mut s, &paths, &lock, prepared).unwrap();
                journal.mark_swapped(&mut s).unwrap();
            }
            Self {
                _dir: dir,
                paths,
                scopes,
                client,
                live,
                from,
                undo,
            }
        }

        /// Recovers with the client's SavedVariables `writable` or not.
        fn recover(&self, writable: bool) -> Result<Vec<Warning>> {
            let lock = crate::fsx::GlobalLock::acquire(&self.paths, "test").unwrap();
            let client = self.client.clone();
            // ubs:ignore — comparing paths, not secrets
            let sv_writable = move |dir: &Path| writable && dir == client;
            recover_all(&RecoveryEnv {
                lock: &lock,
                paths: &self.paths,
                scopes: &self.scopes,
                own: &[],
                sv_writable: &sv_writable,
            })
        }

        fn live(&self) -> String {
            std::fs::read_to_string(&self.live).unwrap()
        }

        fn line(&self) -> HistoryEntry {
            let mut entries = history::read(&self.paths, &HistoryFilter::default())
                .unwrap()
                .entries;
            assert_eq!(entries.len(), 1, "{entries:?}");
            entries.remove(0)
        }

        /// The undo finished but left `Foo.lua`: history marks it, and the warning
        /// names the follow-up.
        fn assert_deferred(&self, warnings: &[Warning], why: &str) {
            assert_eq!(self.live(), "FooDB = { v = 1 }", "left as it is");
            let line = self.line();
            assert_eq!((line.tx, line.sv_deferred), (self.undo, true));
            let codes: Vec<_> = warnings.iter().map(|w| w.code).collect();
            assert_eq!(
                codes,
                [
                    WarningCode::JournalRecovered,
                    WarningCode::SavedVariablesDeferred
                ]
            );
            let deferred = &warnings[1];
            assert!(deferred.message.contains(why), "{}", deferred.message);
            assert!(!deferred.message.contains("ACCT"), "{}", deferred.message);
            let from = self.from.to_string();
            assert_eq!(
                deferred.fix.as_ref().unwrap().argv().unwrap(),
                ["omaddons", "undo", &from, "--saved-variables", "restore"]
            );
            assert!(!self.paths.journal(&self.undo).exists());
        }
    }

    #[test]
    fn recovery_restores_saved_variables_only_where_it_may_write_them() {
        // The client runs (or the command did not find it): the files wait.
        let scene = SvScene::new();
        let warnings = scene.recover(false).unwrap();
        scene.assert_deferred(&warnings, "while its game client was running");

        // It does not: roll-forward restores them, as before.
        let scene = SvScene::new();
        let warnings = scene.recover(true).unwrap();
        assert_eq!(scene.live(), "FooDB = { v = 0 }");
        assert!(!scene.line().sv_deferred);
        let codes: Vec<_> = warnings.iter().map(|w| w.code).collect();
        assert_eq!(codes, [WarningCode::JournalRecovered]);
        assert!(!scene.paths.journal(&scene.undo).exists());
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_failed_saved_variables_restore_in_recovery_is_deferred() {
        let scene = SvScene::new();
        let warnings =
            failpoint::with_spec("engine.recover.sv=error", || scene.recover(true)).unwrap();
        scene.assert_deferred(&warnings, "could not be restored (io)");
    }

    /// A journal of client A, recovered by a command whose scopes cover only client B:
    /// A is written only once its marker names the journal's target; until then the
    /// journal is left, a warning for another target's command, an error for its own.
    #[test]
    fn a_journal_is_recovered_in_its_own_client_and_only_there() {
        use crate::fsx::GlobalLock;
        use crate::model::Actor;
        use crate::state::history::TxResult;

        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (a, b) = (root.join("_retail_"), root.join("_classic_"));
        let tx = TxId::new();
        let stage_dir = a.join(format!("Interface/.omaddons/stage/{tx}"));
        std::fs::create_dir_all(stage_dir.join("Bar")).unwrap();
        std::fs::create_dir_all(a.join("Interface/AddOns")).unwrap();
        std::fs::create_dir_all(b.join("Interface/AddOns")).unwrap();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        let (key, other) = (TargetKey::new(), TargetKey::new());
        // Written by A's own command, whose scopes held A.
        {
            let scopes = ScopeTable::new(&paths, std::slice::from_ref(&a), None).unwrap();
            let lock = GlobalLock::acquire(&paths, "test").unwrap();
            let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "test");
            let prepared = Prepared {
                schema_version: CURRENT,
                tx,
                target_key: key,
                client_dir: a.clone(),
                kind: TxKind::Install,
                started_at: Timestamp::now(),
                creates_dirs: Vec::new(),
                writes_wtf: false,
                staged_stamp: None,
                source_backup: None,
                commit: CommitPayload {
                    record: TargetRecord::empty(key),
                    index: IndexChanges::default(),
                    history: HistoryEntry {
                        schema_version: history::CURRENT,
                        tx,
                        kind: TxKind::Install,
                        at: Timestamp::now(),
                        target: key,
                        actor: Actor::Agent,
                        reason: None,
                        argv: vec!["omaddons".into()],
                        ops: Vec::new(),
                        result: TxResult::Committed,
                        reverts: None,
                        sv_deferred: false,
                        created_addons_dir: false,
                        run: None,
                        backup: None,
                    },
                    stage_dir: stage_dir.clone(),
                    sv_restore: None,
                },
            };
            let mut journal = Journal::create(&mut s, &paths, &lock, prepared).unwrap();
            let step = Step {
                op: StepOp::Noreplace,
                from: stage_dir.join("Bar"),
                to: a.join("Interface/AddOns/Bar"),
                purpose: StepPurpose::SwapIn,
            };
            journal.step(&mut s, step.clone()).unwrap();
            let mode = RenameMode::NoReplace;
            mutate(&mut s, &step.from, Op::Rename { to: &step.to, mode }).unwrap();
        }
        let bar = a.join("Interface/AddOns/Bar");
        let b_scopes = ScopeTable::new(&paths, std::slice::from_ref(&b), None).unwrap();
        let lock = GlobalLock::acquire(&paths, "test").unwrap();
        let recover = |own: &[TargetKey]| {
            recover_all(&RecoveryEnv {
                lock: &lock,
                paths: &paths,
                scopes: &b_scopes,
                own,
                sv_writable: &|_| false,
            })
        };

        // A's marker names another target: A is never written.
        crate::engine::testing::mark(&a, other);
        let warnings = recover(&[]).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].code, WarningCode::JournalPending);
        assert!(
            warnings[0].message.contains("manual repair"),
            "{}",
            warnings[0].message
        );
        assert!(bar.is_dir() && paths.journal(&tx).exists());
        let err = recover(std::slice::from_ref(&key)).unwrap_err();
        assert_eq!(err.code, ErrorCode::ScopeDenied);

        // Its own marker: B's command rolls A back and prunes A's stage dir.
        crate::engine::testing::mark(&a, key);
        let warnings = recover(&[]).unwrap();
        assert_eq!(warnings[0].code, WarningCode::JournalRecovered);
        assert!(!bar.exists() && !stage_dir.exists() && !paths.journal(&tx).exists());
    }

    /// `testing::Scene`'s swap (replace Foo, add Bar, remove Gone) after both snapshot
    /// halves were written, stopped by an error before its fourth rename: the old Foo
    /// already sits in the client snapshot's `AddOns/`. Its writer is gone.
    #[cfg(debug_assertions)]
    fn interrupted_swap() -> crate::engine::testing::Scene {
        use crate::engine::swap::{SwapOp, snapshot_state, swap};
        use crate::engine::testing::snapshot_saved_variables;
        use crate::model::FolderName;
        let name = |n: &str| FolderName::new(n).unwrap().into_mutable().unwrap();
        let ops = [
            SwapOp::Replace {
                name: name("Foo"),
                live: name("Foo"),
                old_tree: None,
            },
            SwapOp::Add { name: name("Bar") },
            SwapOp::Remove {
                live: name("Gone"),
                old_tree: None,
            },
        ];
        let mut scene = crate::engine::testing::Scene::new();
        {
            let lock = scene.lock();
            let mut s = scene.session();
            let mut journal = scene.journal(&mut s, &lock);
            let touched = ["Foo", "Bar", "Gone"].map(|n| FolderName::new(n).unwrap());
            snapshot_saved_variables(&mut s, &scene.tx, &scene.client, &touched).unwrap();
            snapshot_state(&mut s, &scene.paths, &scene.tx, &scene.key).unwrap();
            let swapped = failpoint::with_spec("engine.swap.before_rename=error@4", || {
                swap(&mut s, &mut journal, &ops, &scene.staged, &scene.client)
            });
            assert!(swapped.is_err());
        }
        let snapshot = scene.client.snapshots(&scene.tx);
        assert!(
            snapshot.join("AddOns/Foo").is_dir() && snapshot.join("WTF").is_dir(),
            "the old Foo and the SavedVariables are in the snapshot"
        );
        assert!(scene.paths.state_snapshot(&scene.tx).is_dir());
        scene.release_inflight();
        scene
    }

    fn recover_scene(scene: &crate::engine::testing::Scene) -> Vec<Warning> {
        let lock = scene.lock();
        recover_all(&RecoveryEnv {
            lock: &lock,
            paths: &scene.paths,
            scopes: &scene.scopes,
            own: std::slice::from_ref(&scene.key),
            sv_writable: &|_| false,
        })
        .unwrap()
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_rollback_prunes_both_snapshot_halves() {
        let scene = interrupted_swap();
        let warnings = recover_scene(&scene);
        assert_eq!(
            scene.notes("Foo").as_deref(),
            Some("old foo"),
            "rolled back"
        );
        assert!(!scene.client.snapshots(&scene.tx).exists(), "client half");
        assert!(
            !scene.paths.state_snapshot(&scene.tx).exists(),
            "state half"
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0]
                .message
                .ends_with("recovery undid it (rolled back)"),
            "{}",
            warnings[0].message
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_rollback_keeps_a_client_snapshot_that_still_holds_a_folder() {
        let scene = interrupted_swap();
        // What a recovery bug would leave behind: a folder no step brings back.
        let stray = scene.client.snapshots(&scene.tx).join("AddOns/Stray");
        crate::engine::testing::folder(&stray, "stray");
        let warnings = recover_scene(&scene);
        assert_eq!(
            scene.notes("Foo").as_deref(),
            Some("old foo"),
            "rolled back"
        );
        assert!(stray.is_dir(), "the folder's snapshot is kept");
        assert!(
            !scene.paths.state_snapshot(&scene.tx).exists(),
            "state half"
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let message = &warnings[0].message;
        assert!(
            message.contains(&format!(
                "its snapshot {} is kept: 1 folder is still in it",
                scene.client.snapshots(&scene.tx).display()
            )),
            "{message}"
        );
    }

    /// `testing::Scene`'s transaction, swapped, then cut short while appending its
    /// history line: its last `cut` bytes never reached the disk. Its writer is gone.
    fn swapped_with_its_line_cut(cut: usize) -> (crate::engine::testing::Scene, HistoryEntry) {
        let mut scene = crate::engine::testing::Scene::new();
        let line = {
            let lock = scene.lock();
            let mut s = scene.session();
            let mut journal = scene.journal(&mut s, &lock);
            journal.mark_swapped(&mut s).unwrap();
            let text = std::fs::read_to_string(journal.path()).unwrap();
            parse(&text).prepared.unwrap().commit.history
        };
        let mut bytes = serde_json::to_vec(&line).unwrap();
        bytes.push(b'\n');
        bytes.truncate(bytes.len() - cut);
        std::fs::write(scene.paths.history_file(), bytes).unwrap();
        scene.release_inflight();
        (scene, line)
    }

    #[test]
    fn roll_forward_finds_its_own_line_that_lost_only_its_newline() {
        let (scene, line) = swapped_with_its_line_cut(1);
        let warnings = recover_scene(&scene);
        assert!(
            warnings[0].message.contains("rolled forward"),
            "{warnings:?}"
        );
        let read = history::read(&scene.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(read.entries, [line], "one line, not two");
        assert_eq!((read.torn_lines, read.skipped_lines), (0, 0));
    }

    #[test]
    fn roll_forward_appends_its_line_apart_from_its_own_fragment() {
        let (scene, line) = swapped_with_its_line_cut(40);
        recover_scene(&scene);
        let read = history::read(&scene.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(read.entries, [line]);
        assert_eq!(
            (read.torn_lines, read.skipped_lines),
            (1, 0),
            "the fragment is a line of its own"
        );
    }

    #[test]
    fn a_journal_that_never_prepared_leaves_no_state_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        let tx = TxId::new();
        let snapshot = paths.state_snapshot(&tx);
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::write(snapshot.join("actions.jsonl"), b"").unwrap();
        std::fs::create_dir_all(paths.journal_dir()).unwrap();
        std::fs::write(paths.journal(&tx), "{\"type\":\"prepa").unwrap();
        std::fs::create_dir_all(paths.cache_dir()).unwrap();
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let lock = crate::fsx::GlobalLock::acquire(&paths, "test").unwrap();
        let warnings = recover_all(&RecoveryEnv {
            lock: &lock,
            paths: &paths,
            scopes: &scopes,
            own: &[],
            sv_writable: &|_| false,
        })
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!paths.journal(&tx).exists() && !snapshot.exists());
    }

    #[test]
    fn index_changes_are_idempotent() {
        let folder = |name: &str| FolderIndex::new(name, Vec::new());
        let mut index = FilesIndex {
            key: "01JB8Q2V9W0000000000000000".parse().unwrap(),
            written_at_ns: 0,
            folders: [
                ("old".to_owned(), folder("Old")),
                ("keep".to_owned(), folder("Keep")),
            ]
            .into(),
        };
        let changes = IndexChanges {
            upsert: vec![folder("New")],
            remove: vec!["old".into()],
        };
        changes.apply(&mut index);
        let once = index.clone();
        changes.apply(&mut index);
        assert_eq!(index, once);
        assert_eq!(index.folders.keys().collect::<Vec<_>>(), ["keep", "new"]);
    }

    // -----------------------------------------------------------------------------------
    // Fail closed (M3-D45): planted journals.

    /// `testing::Scene`'s transaction, prepared, with its stage and both snapshot halves
    /// in place and its writer gone; `edit` rewrites the journal's text first (and must
    /// change it).
    fn planted(edit: impl FnOnce(&str) -> String) -> crate::engine::testing::Scene {
        use crate::engine::swap::snapshot_state;
        use crate::engine::testing::snapshot_saved_variables;
        use crate::model::FolderName;
        let mut scene = crate::engine::testing::Scene::new();
        {
            let lock = scene.lock();
            let mut s = scene.session();
            scene.journal(&mut s, &lock);
            let touched = [FolderName::new("Foo").unwrap()];
            snapshot_saved_variables(&mut s, &scene.tx, &scene.client, &touched).unwrap();
            snapshot_state(&mut s, &scene.paths, &scene.tx, &scene.key).unwrap();
        }
        let path = scene.paths.journal(&scene.tx);
        let text = std::fs::read_to_string(&path).unwrap();
        let edited = edit(&text);
        assert_ne!(edited, text, "the edit plants something");
        std::fs::write(&path, edited).unwrap();
        scene.release_inflight();
        assert_eq!(left(&scene), [true; 5], "planted whole");
        scene
    }

    /// What of the transaction is on disk: its journal, in-flight lock, stage, and both
    /// snapshot halves.
    fn left(scene: &crate::engine::testing::Scene) -> [bool; 5] {
        [
            scene.paths.journal(&scene.tx).exists(),
            scene.paths.inflight_lock(&scene.tx).exists(),
            scene.staged.stage_dir.exists(),
            scene.client.snapshots(&scene.tx).exists(),
            scene.paths.state_snapshot(&scene.tx).exists(),
        ]
    }

    /// Recovery by a command of the journal's own target (`own`), or of another one's.
    fn recover_planted(scene: &crate::engine::testing::Scene, own: bool) -> Result<Vec<Warning>> {
        let lock = scene.lock();
        let own: &[TargetKey] = if own {
            std::slice::from_ref(&scene.key)
        } else {
            &[]
        };
        recover_all(&RecoveryEnv {
            lock: &lock,
            paths: &scene.paths,
            scopes: &scene.scopes,
            own,
            sv_writable: &|_| false,
        })
    }

    #[test]
    fn a_journal_this_build_cannot_read_is_left_whole() {
        type Edit = Box<dyn Fn(&str) -> String>;
        let unknown_step = step_line(1).replace("\"noreplace\"", "\"teleport\"");
        let rows: [(&str, Edit, Unrecoverable); 5] = [
            (
                "a newer major version",
                Box::new(|t| {
                    t.replacen(
                        "\"schema_version\":\"1.1\"",
                        "\"schema_version\":\"2.0\"",
                        1,
                    )
                }),
                Unrecoverable::TooNew,
            ),
            (
                "a prepared line that does not decode",
                Box::new(|t| t.replacen("\"kind\":\"install\"", "\"kind\":\"teleport\"", 1)),
                Unrecoverable::Unreadable,
            ),
            (
                "an unknown step op",
                Box::new(move |t| format!("{t}{unknown_step}")),
                Unrecoverable::Unreadable,
            ),
            (
                "an unknown line type",
                Box::new(|t| format!("{t}{{\"type\":\"preserve\",\"x\":1}}\n")),
                Unrecoverable::Unreadable,
            ),
            (
                "a line that is not JSON before the last",
                Box::new(|t| format!("{t}not json\n{}", "{\"type\":\"swapped\"}\n")),
                Unrecoverable::Unreadable,
            ),
        ];
        for (name, edit, why) in rows {
            let scene = planted(edit);
            // Its own target's command fails, with what to do about it.
            let err = recover_planted(&scene, true).unwrap_err();
            let code = match why {
                Unrecoverable::TooNew => ErrorCode::StateTooNew,
                Unrecoverable::Unreadable => ErrorCode::StateCorrupt,
            };
            assert_eq!(err.code, code, "{name}: {err:?}");
            assert_eq!(err.details["reason"], why.reason(), "{name}");
            assert_eq!(
                err.details["journal_pending"],
                scene.tx.to_string(),
                "{name}"
            );
            assert_eq!(err.details["target"], scene.key.to_string(), "{name}");
            assert_eq!(err.fix.argv(), None, "{name}: prose, no command helps");
            assert_eq!(left(&scene), [true; 5], "{name}: everything left");
            // Another target's command goes on, with a warning.
            let warnings = recover_planted(&scene, false).unwrap();
            assert_eq!(warnings.len(), 1, "{name}: {warnings:?}");
            assert_eq!(warnings[0].code, WarningCode::JournalPending, "{name}");
            assert!(
                warnings[0].message.contains(why.reason()),
                "{name}: {warnings:?}"
            );
            assert_eq!(left(&scene), [true; 5], "{name}: everything still left");
            // Readers report it, with why; its own cleanup never prunes it.
            let pending = pending(&scene.paths).unwrap();
            assert_eq!(
                pending,
                [Pending {
                    tx: scene.tx,
                    target: Some(scene.key),
                    unrecoverable: Some(why),
                }],
                "{name}"
            );
            assert!(unfinished(&scene.paths, &scene.tx), "{name}");
            let read = pending_warnings(&scene.paths).unwrap();
            assert!(read[0].message.contains(why.reason()), "{name}: {read:?}");
        }
    }

    #[test]
    fn a_journal_with_what_this_build_may_ignore_is_recovered() {
        type Edit = Box<dyn Fn(&str) -> String>;
        let rows: [(&str, Edit); 4] = [
            (
                "an unknown field",
                Box::new(|t| {
                    t.replacen(
                        "{\"type\":\"prepared\",",
                        "{\"type\":\"prepared\",\"writes_wtf\":true,",
                        1,
                    )
                }),
            ),
            (
                "a newer minor version",
                Box::new(|t| {
                    t.replacen(
                        "\"schema_version\":\"1.1\"",
                        "\"schema_version\":\"1.9\"",
                        1,
                    )
                }),
            ),
            (
                "a torn final step line",
                Box::new(|t| format!("{t}{}", &step_line(1)[..40])),
            ),
            (
                "an older version",
                Box::new(|t| {
                    t.replacen(
                        "\"schema_version\":\"1.1\"",
                        "\"schema_version\":\"1.0\"",
                        1,
                    )
                }),
            ),
        ];
        for (name, edit) in rows {
            let scene = planted(edit);
            let warnings = recover_planted(&scene, true).unwrap();
            assert_eq!(warnings.len(), 1, "{name}: {warnings:?}");
            assert!(
                warnings[0]
                    .message
                    .ends_with("recovery undid it (rolled back)"),
                "{name}: {warnings:?}"
            );
            assert_eq!(left(&scene), [false; 5], "{name}: all of it pruned");
        }

        // Only a torn `prepared` line: nothing ran, so it is pruned as before (the client
        // half, unknown without a `prepared` line, is the orphan sweep's).
        let scene = planted(|t| t[..20].to_owned());
        assert_eq!(recover_planted(&scene, true).unwrap(), []);
        let [journal, inflight, _, _, state] = left(&scene);
        assert_eq!([journal, inflight, state], [false; 3]);
    }

    #[test]
    fn roll_forward_never_appends_after_a_torn_line() {
        // Swapped, then cut short while writing `committed`.
        let scene = planted(|t| format!("{t}{{\"type\":\"swapped\"}}\n{{\"type\":\"comm"));
        let path = scene.paths.journal(&scene.tx);
        let before = std::fs::read_to_string(&path).unwrap();
        let parsed = parse(&before);
        assert_eq!((parsed.phase, parsed.torn_tail), (Phase::Swapped, true));
        // A crash right after roll-forward, before its prune: the journal is as it was,
        // with no line merged into the fragment.
        {
            let lock = scene.lock();
            let env = RecoveryEnv {
                lock: &lock,
                paths: &scene.paths,
                scopes: &scene.scopes,
                own: &[],
                sv_writable: &|_| false,
            };
            let mut s = WriteSession::untracked(Writer::Mutating, &scene.scopes, "recovery");
            let prepared = parsed.prepared.as_ref().unwrap();
            roll_forward(&mut s, &env, &path, prepared, true).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        // The next recovery rolls forward again, and writes nothing twice.
        let warnings = recover_planted(&scene, true).unwrap();
        assert!(
            warnings[0].message.contains("rolled forward"),
            "{warnings:?}"
        );
        assert!(!path.exists());
        let read = history::read(&scene.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(read.entries.len(), 1, "one history line");
        let record = crate::state::records::load_record(&scene.paths, &scene.key).unwrap();
        assert_eq!(record.value.generation, 1);
    }

    // -----------------------------------------------------------------------------------
    // Journals that rename `WTF/` (M3-D24): writer, wait, conservative stop.

    /// `testing::Scene`'s client with a restore journaled on it: its stage holds a `WTF/`
    /// (a `restored.txt` marker) copied from a backup dir, recorded with its stamp and the
    /// backup's identity; when `exchanged`, the exchange with the live `WTF/` is journaled
    /// and done. Its writer is gone.
    struct WtfScene {
        scene: crate::engine::testing::Scene,
        backup: PathBuf,
    }

    impl WtfScene {
        fn new(kind: TxKind, writes_wtf: bool, exchanged: bool) -> Self {
            use crate::model::BackupId;
            let mut scene = crate::engine::testing::Scene::new();
            let staged = scene.staged.stage_dir.join("WTF");
            std::fs::create_dir_all(staged.join("Account")).unwrap();
            std::fs::write(staged.join("Account/restored.txt"), "from the backup").unwrap();
            let id = BackupId::new();
            let backup = crate::backup::backup_dir(&scene.client, &id);
            std::fs::create_dir_all(backup.join("WTF")).unwrap();
            let stamp = fsx::StampDigest::compute(&staged).unwrap().to_string();
            {
                let lock = scene.lock();
                crate::engine::testing::mark(scene.client.client_dir(), scene.key);
                let mut s = WriteSession::untracked(Writer::BackupRestore, &scene.scopes, "test");
                let mut prepared = {
                    let mut probe = scene.session();
                    let journal = scene.journal(&mut probe, &lock);
                    let text = std::fs::read_to_string(journal.path()).unwrap();
                    std::fs::remove_file(journal.path()).unwrap();
                    parse(&text).prepared.unwrap()
                };
                prepared.kind = kind;
                prepared.writes_wtf = writes_wtf;
                prepared.staged_stamp = Some(stamp);
                prepared.source_backup = Some(SourceBackup {
                    id,
                    dir: FileId::of(&backup).unwrap(),
                });
                let mut journal = Journal::create(&mut s, &scene.paths, &lock, prepared).unwrap();
                if exchanged {
                    let live = scene.client.wtf();
                    let step = Step {
                        op: StepOp::Exchange,
                        from: staged.clone(),
                        to: live.clone(),
                        purpose: StepPurpose::SwapIn,
                    };
                    journal.step(&mut s, step).unwrap();
                    let op = Op::Rename {
                        to: &live,
                        mode: RenameMode::Exchange,
                    };
                    mutate(&mut s, &staged, op).unwrap();
                }
            }
            scene.release_inflight();
            Self { scene, backup }
        }

        fn recover(&self, own: bool, running: bool) -> Result<Vec<Warning>> {
            let lock = self.scene.lock();
            let own: &[TargetKey] = if own {
                std::slice::from_ref(&self.scene.key)
            } else {
                &[]
            };
            recover_all(&RecoveryEnv {
                lock: &lock,
                paths: &self.scene.paths,
                scopes: &self.scene.scopes,
                own,
                sv_writable: &|_| !running,
            })
        }

        /// Whether the live `WTF/` is the restored tree.
        fn live_restored(&self) -> bool {
            self.scene
                .client
                .wtf()
                .join("Account/restored.txt")
                .exists()
        }

        fn staged(&self) -> PathBuf {
            self.scene.staged.stage_dir.join("WTF")
        }

        fn journal_kept(&self) -> bool {
            self.scene.paths.journal(&self.scene.tx).exists()
        }
    }

    #[test]
    fn a_wtf_journal_is_recovered_as_backup_restore_whatever_its_kind() {
        for kind in [TxKind::Restore, TxKind::Undo] {
            let w = WtfScene::new(kind, true, true);
            assert!(w.live_restored());
            let warnings = w.recover(true, false).unwrap();
            assert!(
                warnings[0].message.contains("rolled back"),
                "{kind:?}: {warnings:?}"
            );
            assert!(!w.live_restored(), "{kind:?}: the old WTF/ is live again");
            assert!(
                !w.staged().exists() && !w.journal_kept(),
                "{kind:?}: cleaned up"
            );
        }
        // Without the flag recovery writes as Mutating, which may not move WTF/.
        let w = WtfScene::new(TxKind::Undo, false, true);
        let err = w.recover(true, false).unwrap_err();
        assert_eq!(err.code, ErrorCode::ScopeDenied, "{err:?}");
        assert!(w.live_restored() && w.journal_kept());
    }

    #[test]
    fn a_wtf_journal_waits_while_the_game_runs() {
        let w = WtfScene::new(TxKind::Restore, true, true);
        // Its own target's change fails, with the transaction it waits on.
        let err = w.recover(true, true).unwrap_err();
        assert_eq!(err.code, ErrorCode::GameRunning);
        assert_eq!(err.details["reason"], "saved_variables");
        assert_eq!(err.details["journal_pending"], w.scene.tx.to_string());
        // Others go on, and are told.
        let warnings = w.recover(false, true).unwrap();
        assert_eq!(warnings[0].code, WarningCode::JournalPending);
        assert!(warnings[0].message.contains("waits for the game to exit"));
        assert!(w.live_restored() && w.journal_kept(), "nothing moved");
        // Once the game exits, it rolls back.
        w.recover(true, false).unwrap();
        assert!(!w.live_restored() && !w.journal_kept());

        // With no step recorded, nothing moved: it is recovered at once.
        let w = WtfScene::new(TxKind::Restore, true, false);
        let warnings = w.recover(true, true).unwrap();
        assert!(warnings[0].message.contains("rolled back"), "{warnings:?}");
        assert!(!w.journal_kept() && !w.staged().exists());
    }

    #[test]
    fn a_played_on_stage_or_a_lost_source_stops_recovery_and_keeps_everything() {
        // The game played on the restored tree before the crash was recovered.
        let w = WtfScene::new(TxKind::Restore, true, true);
        std::fs::write(
            w.scene.client.wtf().join("Account/restored.txt"),
            "played on",
        )
        .unwrap();
        for _ in 0..3 {
            let err = w.recover(true, false).unwrap_err();
            assert_eq!(err.code, ErrorCode::StateCorrupt, "{err:?}");
            assert_eq!(err.details["reason"], "preservation_incomplete");
            assert!(!w.live_restored(), "the pre-restore tree is live again");
            assert_eq!(
                std::fs::read_to_string(w.staged().join("Account/restored.txt")).unwrap(),
                "played on",
                "the played-on tree is kept in the stage"
            );
            assert!(w.journal_kept());
        }

        // Unchanged, but its source backup is gone: the stage is its last copy.
        let w = WtfScene::new(TxKind::Restore, true, true);
        std::fs::remove_dir_all(&w.backup).unwrap();
        let err = w.recover(true, false).unwrap_err();
        assert_eq!(err.details["reason"], "preservation_incomplete");
        assert!(w.staged().join("Account/restored.txt").exists() && w.journal_kept());

        // Without the change, recovery completes and prunes the stage.
        let w = WtfScene::new(TxKind::Restore, true, true);
        w.recover(true, false).unwrap();
        assert!(!w.staged().exists() && !w.journal_kept());
    }

    #[test]
    fn an_unexpected_identity_stops_recovery_and_touches_nothing() {
        let w = WtfScene::new(TxKind::Restore, true, true);
        // Someone moved the restored tree away and put another WTF/ in its place.
        let live = w.scene.client.wtf();
        let aside = w.scene.client.client_dir().join("WTF.aside");
        std::fs::rename(&live, &aside).unwrap();
        std::fs::create_dir_all(live.join("Account")).unwrap();
        std::fs::write(live.join("Account/other.txt"), "someone else's").unwrap();
        let err = w.recover(true, false).unwrap_err();
        assert_eq!(err.details["reason"], "unexpected_state", "{err:?}");
        assert!(
            live.join("Account/other.txt").exists(),
            "the live path untouched"
        );
        assert!(w.staged().exists() && w.journal_kept());
    }
}
