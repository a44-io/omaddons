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

use crate::error::{Error, Result, Suggestion, Warning, WarningCode};
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

/// A path's identity, which a rename on one filesystem keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
}

impl FileId {
    /// The identity of what is at `path` (not following a symlink), if anything.
    pub fn of(path: &Path) -> Option<Self> {
        std::fs::symlink_metadata(path).ok().map(|m| Self {
            dev: m.dev(),
            ino: m.ino(),
        })
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

/// Recovers every leftover journal. The guard proves the global lock is held, so no
/// writer is alive except ones still cleaning up after commit, whose in-flight lock is
/// held and whose journals are skipped. One `journal_recovered` warning per rolled
/// transaction; committed and unprepared journals are removed silently.
pub fn recover_all(
    lock: &GlobalLockGuard,
    paths: &Paths,
    scopes: &ScopeTable,
) -> Result<Vec<Warning>> {
    let mut warnings = Vec::new();
    for (tx, path) in journals(paths)? {
        if InflightLock::is_live(paths, &tx)? {
            continue;
        }
        warnings.extend(recover_one(lock, paths, scopes, &tx, path)?);
    }
    Ok(warnings)
}

/// Recovers one transaction by its own writer, in process: an error after `prepared`
/// rolls back, after `swapped` rolls forward. No liveness check: the caller is the
/// writer. Leftovers its in-flight lock still protects are pruned at cleanup.
pub fn recover_tx(
    lock: &GlobalLockGuard,
    paths: &Paths,
    scopes: &ScopeTable,
    tx: &TxId,
) -> Result<Option<RecoveryAction>> {
    let path = paths.journal(tx);
    if !path.exists() {
        return Ok(None);
    }
    let parsed = read(&path)?;
    let action = recover_parsed(lock, paths, scopes, tx, &path, &parsed)?;
    Ok(action)
}

/// Recovers one journal and reports it.
fn recover_one(
    lock: &GlobalLockGuard,
    paths: &Paths,
    scopes: &ScopeTable,
    tx: &TxId,
    path: PathBuf,
) -> Result<Option<Warning>> {
    let parsed = read(&path)?;
    let action = recover_parsed(lock, paths, scopes, tx, &path, &parsed)?;
    let (Some(action), Some(prepared)) = (action, &parsed.prepared) else {
        return Ok(None);
    };
    let what = match action {
        RecoveryAction::RolledForward => "completed it (rolled forward)",
        RecoveryAction::RolledBack => "undid it (rolled back)",
    };
    Ok(Some(
        Warning::new(
            WarningCode::JournalRecovered,
            format!(
                "transaction {tx} on target {} was interrupted; recovery {what}",
                prepared.target_key
            ),
        )
        .with_fix(Suggestion::command(["omaddons", "history", "--limit", "5"])),
    ))
}

/// Rolls forward or back by phase, then prunes what the transaction left.
fn recover_parsed(
    lock: &GlobalLockGuard,
    paths: &Paths,
    scopes: &ScopeTable,
    tx: &TxId,
    path: &Path,
    parsed: &Parsed,
) -> Result<Option<RecoveryAction>> {
    let mut s = WriteSession::untracked(Writer::Mutating, scopes, "recovery");
    let action = match (&parsed.prepared, parsed.phase) {
        (None, _) | (_, Phase::Unprepared) => None,
        (Some(_), Phase::Committed) => None,
        (Some(prepared), Phase::Swapped) => {
            roll_forward(&mut s, scopes, paths, path, prepared)?;
            Some(RecoveryAction::RolledForward)
        }
        (Some(_), Phase::Prepared) => {
            roll_back(&mut s, &parsed.steps)?;
            Some(RecoveryAction::RolledBack)
        }
    };
    let mut victims = Vec::new();
    if let Some(prepared) = &parsed.prepared
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
        LockUse::Held(lock),
    )?;
    Ok(action)
}

/// Writes the payload's state (each part only if not already written), then `committed`.
fn roll_forward(
    s: &mut WriteSession<'_>,
    scopes: &ScopeTable,
    paths: &Paths,
    journal: &Path,
    prepared: &Prepared,
) -> Result<()> {
    let key = prepared.target_key;
    let commit = &prepared.commit;
    if let Some(sv) = &commit.sv_restore {
        let mut undo = WriteSession::untracked(Writer::Undo, scopes, "recovery")
            .with_owned_folders(sv.owned_folders());
        let client = crate::state::paths::ClientPaths::new(&prepared.client_dir);
        super::undo::restore_saved_variables(&mut undo, &client, sv, &prepared.tx)?;
    }
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
        .any(|e| e.tx == prepared.tx)
    {
        history::append(s, paths, &commit.history)?;
    }
    append_line(s, journal, &JournalLine::Committed)
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
    if (now_from, now_to) == before {
        return Ok(false);
    }
    if (now_from, now_to) == after {
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
                    }],
                    result: TxResult::Committed,
                    reverts: None,
                    sv_deferred: false,
                    created_addons_dir: true,
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
            from_id: Some(FileId { dev: 1, ino: 2 }),
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
                    }],
                    result: TxResult::Committed,
                    reverts: None,
                    sv_deferred: false,
                    created_addons_dir: false,
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

        let lock = GlobalLock::acquire(&paths, "test").unwrap();
        let crashed = failpoint::with_spec("engine.recover.step=error", || {
            recover_all(&lock, &paths, &scopes)
        });
        assert!(crashed.is_err());
        assert_eq!(
            (
                read(&addons.join("Foo")).as_deref(),
                read(&addons.join("Bar"))
            ),
            (Some("new"), None),
            "Bar was undone, then the crash"
        );

        let warnings = recover_all(&lock, &paths, &scopes).unwrap();
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
