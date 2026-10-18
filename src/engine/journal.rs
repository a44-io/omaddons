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

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    self, FsxError, GlobalLockGuard, InflightLock, LockState, LockUse, Op, PruneReason, RenameMode,
    ScopeTable, WriteSession, Writer, ensure_dir_all, failpoint, mutate, probe_global,
};
use crate::model::{TargetKey, TxId, TxKind};
use crate::state::files_index::{FilesIndex, FolderIndex};
use crate::state::history::{self, HistoryEntry, HistoryFilter};
use crate::state::paths::Paths;
use crate::state::records::TargetRecord;
use crate::state::store::{self, SchemaVersion};

/// The version of the line format.
pub const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);

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
    /// Lines with a `type` this build does not know, skipped.
    pub unknown_lines: usize,
}

/// Parses a journal. A torn last line (no newline, or not JSON) is a crash mid-append
/// and is ignored: a step line is fsync'd before its rename, so a torn one never ran.
/// An unreadable line before the last is corruption, an error.
pub fn parse(text: &str) -> Result<Parsed> {
    let mut parsed = Parsed {
        prepared: None,
        steps: Vec::new(),
        phase: Phase::Unprepared,
        unknown_lines: 0,
    };
    let complete = text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();
    for (i, raw) in lines.iter().enumerate() {
        let last = i + 1 == lines.len();
        if last && !complete {
            break;
        }
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(_) if last => break,
            Err(e) => {
                return Err(Error::internal(format!(
                    "journal line {} is not JSON: {e}",
                    i + 1
                )));
            }
        };
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
    Ok(parsed)
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
    parse(&String::from_utf8_lossy(&bytes))
        .map_err(|e| Error::internal(format!("{}: {}", path.display(), e.message)))
}

/// Whether `tx`'s journal is one recovery still needs: started (`prepared`) and neither
/// committed nor recovered. An unreadable one counts too: recovery reports it.
pub(crate) fn unfinished(paths: &Paths, tx: &TxId) -> bool {
    let path = paths.journal(tx);
    path.exists()
        && read(&path).map_or(true, |parsed| {
            matches!(parsed.phase, Phase::Prepared | Phase::Swapped)
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
}

/// Every journal that is not `committed`, oldest first. Writes nothing.
pub fn pending(paths: &Paths) -> Result<Vec<Pending>> {
    let mut found = Vec::new();
    for (tx, path) in journals(paths)? {
        let target = match read(&path) {
            Ok(parsed) if matches!(parsed.phase, Phase::Committed) => continue,
            Ok(parsed) => parsed.prepared.map(|p| p.target_key),
            Err(_) => None,
        };
        found.push(Pending { tx, target });
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
        .map(|Pending { tx, target }| {
            let on = target.map(|t| format!(" on target {t}")).unwrap_or_default();
            if held {
                Warning::new(
                    WarningCode::TxInProgress,
                    format!("transaction {tx}{on} is in progress"),
                )
                .with_fix(Suggestion::text("wait for the running omaddons command to finish"))
            } else {
                Warning::new(
                    WarningCode::JournalPending,
                    format!(
                        "transaction {tx}{on} did not finish; the next command that changes addons recovers it"
                    ),
                )
                .with_fix(Suggestion::command(["omaddons", "status"]))
            }
        })
        .collect())
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
    /// `saved_variables_deferred`, when roll-forward left an undo's SavedVariables.
    pub warnings: Vec<Warning>,
    /// How many SavedVariables files roll-forward left as they are.
    pub sv_deferred: usize,
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
        // Whose it is, for isolation: an unreadable journal's owner is unknown, and
        // fails the caller as before.
        let target = read(&path)
            .ok()
            .and_then(|p| p.prepared)
            .map(|p| p.target_key);
        match recover_one(env, &tx, path) {
            Ok(recovered) => warnings.extend(recovered),
            Err(error) => match target.filter(|t| !env.own.contains(t)) {
                Some(other) => warnings.push(
                    Warning::new(
                        WarningCode::JournalPending,
                        format!(
                            "transaction {tx} on target {other} did not finish and could not be recovered: {}",
                            error.message
                        ),
                    )
                    .with_fix(Suggestion::command(["omaddons", "status"])),
                ),
                None => return Err(error),
            },
        }
    }
    Ok(warnings)
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
    let mut warnings = vec![
        Warning::new(
            WarningCode::JournalRecovered,
            format!(
                "transaction {tx} on target {} was interrupted; recovery {what}",
                prepared.target_key
            ),
        )
        .with_fix(Suggestion::command(["omaddons", "history", "--limit", "5"])),
    ];
    warnings.extend(recovered.warnings);
    Ok(warnings)
}

/// Rolls forward or back by phase, then prunes what the transaction left.
fn recover_parsed(
    env: &RecoveryEnv<'_>,
    tx: &TxId,
    path: &Path,
    parsed: &Parsed,
) -> Result<Option<Recovered>> {
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
    let mut s = WriteSession::untracked(Writer::Mutating, scopes, "recovery");
    let recovered = match (&parsed.prepared, parsed.phase) {
        (None, _) | (_, Phase::Unprepared) => None,
        (Some(_), Phase::Committed) => None,
        (Some(prepared), Phase::Swapped) => {
            let left = roll_forward(&mut s, env, path, prepared)?;
            Some(Recovered {
                action: RecoveryAction::RolledForward,
                sv_deferred: left.as_ref().map_or(0, |(files, _)| *files),
                warnings: left.into_iter().map(|(_, warning)| warning).collect(),
            })
        }
        (Some(_), Phase::Prepared) => {
            roll_back(&mut s, &parsed.steps)?;
            Some(Recovered {
                action: RecoveryAction::RolledBack,
                warnings: Vec::new(),
                sv_deferred: 0,
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
    Ok(recovered)
}

/// Writes the payload's state (each part only if not already written), then `committed`.
/// Returns the SavedVariables files an undo's restore left, with the warning that says
/// so.
fn roll_forward(
    s: &mut WriteSession<'_>,
    env: &RecoveryEnv<'_>,
    journal: &Path,
    prepared: &Prepared,
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
    append_line(s, journal, &JournalLine::Committed)?;
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
                super::undo::restore_saved_variables(&mut undo, &client, sv, &tx)
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
    for step in steps.iter().rev() {
        if ran(step)? {
            undo(s, step)?;
        }
        failpoint::hit("engine.recover.step")
            .map_err(|e| Error::io(&step.from, &e, Suggestion::command(["omaddons", "status"])))?;
    }
    Ok(())
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
                    }],
                    result: TxResult::Committed,
                    reverts: None,
                    sv_deferred: false,
                    created_addons_dir: true,
                    run: Some(crate::model::RunId::new()),
                },
                stage_dir: "/c/_retail_/Interface/.omaddons/stage/x".into(),
                sv_restore: None,
            },
        }));
        let text = format!("{}\n", serde_json::to_string(&line).unwrap());
        let parsed = parse(&text).unwrap();
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
        let empty = parse("").unwrap();
        assert_eq!(
            (empty.phase, empty.prepared.is_none()),
            (Phase::Unprepared, true)
        );
        let torn = parse("{\"type\":\"prepa").unwrap();
        assert_eq!(
            torn.phase,
            Phase::Unprepared,
            "a torn prepared line means nothing ran"
        );

        let no_prepared = format!("{}{}", step_line(1), "{\"type\":\"swapped\"}\n");
        let parsed = parse(&no_prepared).unwrap();
        assert_eq!(
            (parsed.phase, parsed.steps.len(), parsed.unknown_lines),
            (Phase::Unprepared, 1, 1)
        );

        let text = format!(
            "{}{}{{\"type\":\"future\",\"x\":1}}\n{}",
            step_line(1),
            step_line(2),
            "{\"type\":\"step\",\"n\":3,\"op\""
        );
        let parsed = parse(&text).unwrap();
        assert_eq!(
            (parsed.steps.len(), parsed.unknown_lines),
            (2, 1),
            "torn third step ignored"
        );
        let complete_but_invalid = format!("{}not json\n", step_line(1));
        assert_eq!(
            parse(&complete_but_invalid).unwrap().steps.len(),
            1,
            "an invalid last line is torn too"
        );
        let corrupt = format!("not json\n{}", step_line(1));
        assert!(
            parse(&corrupt).is_err(),
            "corruption before the end is an error"
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
        let parsed = parse(&line).unwrap();
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
                    }],
                    result: TxResult::Committed,
                    reverts: None,
                    sv_deferred: false,
                    created_addons_dir: false,
                    run: None,
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
}
