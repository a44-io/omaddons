//! The transaction journal (vision §5.5): what makes a multi-rename commit crash-safe.
//!
//! `<state>/omaddons/journal/<tx>.jsonl` holds one JSON object per line, each appended
//! with one fsync: `prepared` (with everything roll-forward needs), one `step` before
//! each rename, `swapped` after the last rename, `committed` once the state files are
//! written. Recovery runs only while holding the global lock (D23): it rolls forward
//! from `swapped` and rolls back from anything earlier, replaying the step records in
//! reverse.
//!
//! Each step records the identity (inode and birth time; the device is recorded for
//! diagnosis only) of both paths before its rename.
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

use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::barrier::{Outcome, Passes, Stuck};
use super::fence::{self, Behind};
use super::repair::PreparedRead;
use super::resource::{PathKind, Resource, ResourceSet};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
pub use crate::fsx::FileId;
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

pub use super::simulate::{
    Approximation, Artifact, ArtifactKind, Base, Diagnoses, Direction, DiskBase, Expected,
    JournalDiagnosis, JournalEvidence, JournalFamily, JournalVerdict, Listed, ListingFailure,
    MapBase, Preservation, ProtectedPaths, UnattributedCause, diagnose, diagnose_journals,
};

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
    /// A rollback's write-ahead preservation intent (M3-D24 (c) rule 3).
    Preserve(PreserveIntent),
    /// That intent's completion: only then may recovery prune.
    Preserved(Preserved),
}

/// What a `writes_wtf` rollback gives a durable home before it prunes anything: the
/// displaced versions no other copy holds, as one `recovered` backup under an id reserved
/// here, so a recovery that dies in the protocol resumes it with the same id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PreserveIntent {
    pub id: crate::model::BackupId,
    pub items: Vec<PreserveItem>,
    /// `<meta>/stage/<tx>.wtf/recovered/<id>`: where the backup is assembled before the one
    /// rename that publishes it.
    pub assembly: PathBuf,
}

/// One displaced version to preserve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PreserveItem {
    /// Where recovery found it.
    pub path: PathBuf,
    pub identity: FileId,
    /// Its place under the backup's `WTF/`, with `/`: empty for a whole tree.
    #[serde(default)]
    pub rel: String,
}

/// A preservation intent completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Preserved {
    pub id: crate::model::BackupId,
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
            "preserve" => serde_json::from_value(value)
                .map(Self::Preserve)
                .map_err(D::Error::custom),
            "preserved" => serde_json::from_value(value)
                .map(Self::Preserved)
                .map_err(D::Error::custom),
            other => Err(D::Error::unknown_variant(
                other,
                &[
                    "prepared",
                    "step",
                    "swapped",
                    "committed",
                    "preserve",
                    "preserved",
                ],
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
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
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
    /// A partial restore's staged files (M3-D27), each with its stamp when it was
    /// journaled: a rollback keeps one changed since, or every one whose source backup is
    /// gone. `None` for a whole `WTF/`, whose stage `staged_stamp` judges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_files: Option<Vec<StagedFile>>,
    /// The backup the stage was copied from, and its directory's identity then: a
    /// stage whose source is gone holds the last copy of that content, and is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_backup: Option<SourceBackup>,
    /// The client as the transaction found it: what recovery describes a preserved
    /// backup with (`writes_wtf` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientFacts>,
}

impl Prepared {
    /// Whether the transaction puts settings back: a restore or the undo of one, which
    /// renames `WTF/` or has `wtf` or `saved_variables` ops. A build before M3 cannot read
    /// its `prepared` line and deleted such a journal unread, so it is written under a
    /// name those builds never list ([`Paths::journal_wtf`]), as are its snapshots.
    pub fn puts_settings_back(&self) -> bool {
        self.writes_wtf || history::puts_settings_back(&self.commit.history.ops)
    }
}

/// What a `backup.json` says of the client its backup is of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientFacts {
    pub target_id: String,
    pub family: crate::rules::Family,
    pub version: String,
}

/// One file a partial restore staged: its place under the stage's `WTF/`, with `/`, and
/// its [`file_stamp`] when the transaction was journaled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StagedFile {
    pub rel: String,
    pub stamp: String,
}

/// The stamp digest of the one entry at `path`: its kind, size, mode and mtime, which a
/// rename keeps (its `ctime` and inode are not part of it). `None` when nothing is there.
pub fn file_stamp(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let kind = meta.file_type();
    let kind = if kind.is_file() {
        "file"
    } else if kind.is_dir() {
        "dir"
    } else if kind.is_symlink() {
        "symlink"
    } else {
        "other"
    };
    let mtime = meta
        .mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.mtime_nsec());
    let entry = ("", kind, meta.size(), meta.mode() & 0o7777, mtime);
    Some(fsx::StampDigest::from_entries([entry]).to_string())
}

/// The backup a restore's stage was copied from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceBackup {
    pub id: crate::model::BackupId,
    /// Its directory's identity, compared with [`FileId::same`].
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

// ---------------------------------------------------------------------------------------
// What recovery's decisions read (E11).

/// What is at a path, as recovery's decisions see it: never following a final symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LiveEntry {
    pub kind: LiveKind,
    pub id: FileId,
}

/// The kind of a [`LiveEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// Every read recovery's decision procedure makes (E11). Recovery reads the disk
/// ([`Disk`]); `journal::diagnose` reads a simulation of it, so both decide with the
/// same code.
pub(crate) trait LiveView {
    /// What is at `path`, if anything.
    fn entry(&mut self, path: &Path) -> Option<LiveEntry>;

    /// The identity of what is at `path`, as [`FileId::of`] gives it.
    fn id(&mut self, path: &Path) -> Option<FileId> {
        self.entry(path).map(|e| e.id)
    }

    /// Whether a directory is at `path`. [`Disk`] follows a final symlink, as
    /// `Path::is_dir` does and recovery always has.
    fn is_dir(&mut self, path: &Path) -> bool {
        matches!(
            self.entry(path),
            Some(LiveEntry {
                kind: LiveKind::Dir,
                ..
            })
        )
    }

    /// [`file_stamp`] of the one entry at `path`.
    fn file_stamp(&mut self, path: &Path) -> Option<String>;

    /// `fsx::StampDigest::compute(path)` as text; its error is what recovery stops on.
    fn tree_stamp(&mut self, path: &Path) -> std::io::Result<String>;
}

/// A rollback walk's effects: reversing a step that ran, and the point after each step
/// the walk passes.
pub(crate) trait Replay: LiveView {
    fn reverse(&mut self, step: &StepRecord) -> Result<()>;
    fn stepped(&mut self, step: &StepRecord) -> Result<()>;
}

/// The disk, read as recovery always has.
pub(crate) struct Disk;

impl LiveView for Disk {
    fn entry(&mut self, path: &Path) -> Option<LiveEntry> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        let kind = meta.file_type();
        let kind = if kind.is_file() {
            LiveKind::File
        } else if kind.is_dir() {
            LiveKind::Dir
        } else if kind.is_symlink() {
            LiveKind::Symlink
        } else {
            LiveKind::Other
        };
        Some(LiveEntry {
            kind,
            id: FileId::from_metadata(&meta),
        })
    }

    fn is_dir(&mut self, path: &Path) -> bool {
        path.is_dir()
    }

    fn file_stamp(&mut self, path: &Path) -> Option<String> {
        file_stamp(path)
    }

    fn tree_stamp(&mut self, path: &Path) -> std::io::Result<String> {
        fsx::StampDigest::compute(path).map(|digest| digest.to_string())
    }
}

/// Recovery itself: the disk read as [`Disk`] reads it, and its rollback effects
/// written through `s`.
pub(crate) struct Recovering<'s, 'a> {
    pub s: &'s mut WriteSession<'a>,
}

impl LiveView for Recovering<'_, '_> {
    fn entry(&mut self, path: &Path) -> Option<LiveEntry> {
        Disk.entry(path)
    }

    fn is_dir(&mut self, path: &Path) -> bool {
        Disk.is_dir(path)
    }

    fn file_stamp(&mut self, path: &Path) -> Option<String> {
        Disk.file_stamp(path)
    }

    fn tree_stamp(&mut self, path: &Path) -> std::io::Result<String> {
        Disk.tree_stamp(path)
    }
}

impl Replay for Recovering<'_, '_> {
    /// The rename back ([`undo`]).
    fn reverse(&mut self, step: &StepRecord) -> Result<()> {
        undo(self.s, step)
    }

    /// The failpoint `engine.recover.step`, so a crash between replayed steps can be
    /// simulated.
    fn stepped(&mut self, step: &StepRecord) -> Result<()> {
        failpoint::hit("engine.recover.step")
            .map_err(|e| Error::io(&step.from, &e, Suggestion::command(["omaddons", "status"])))
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
    /// Creates `journal/<tx>.jsonl` (`<tx>.wtf.jsonl` for a transaction that puts
    /// settings back) with its `prepared` line. The guard proves the global lock is held.
    pub fn create(
        s: &mut WriteSession<'_>,
        paths: &Paths,
        _lock: &GlobalLockGuard,
        prepared: Prepared,
    ) -> Result<Self> {
        ensure_dir_all(s, &paths.journal_dir())?;
        let path = if prepared.puts_settings_back() {
            paths.journal_wtf(&prepared.tx)
        } else {
            paths.journal(&prepared.tx)
        };
        let journal = Self {
            path,
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

pub(super) fn append_line(s: &mut WriteSession<'_>, path: &Path, line: &JournalLine) -> Result<()> {
    let mut bytes = serde_json::to_vec(line)
        .map_err(|e| Error::internal(format!("serializing a journal line: {e}")))?;
    bytes.push(b'\n');
    mutate(s, path, Op::AppendFile { content: &bytes })?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Reading.

/// How far a transaction got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
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
    /// A rollback's preservation intents, and the ids of those completed.
    pub preserve: Vec<PreserveIntent>,
    pub preserved: Vec<crate::model::BackupId>,
}

/// Why recovery stops for a journal, as `details.reason` says it (E11): one vocabulary
/// for recovery's stop errors, the warnings that report another target's stop, and the
/// doctor's simulation of recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Started by a newer major version of the journal.
    JournalTooNew,
    /// A complete line does not read, or no `prepared` line does.
    JournalUnreadable,
    /// The journal's client no longer carries its target's marker.
    MarkerMismatch,
    /// A rollback met a step in neither its before nor its after state.
    StepUnrecognized,
    /// A settings restore's step in neither state (`writes_wtf` journals).
    UnexpectedState,
    /// What a settings restore displaced has no durable home yet.
    PreservationIncomplete,
    /// A settings restore waits for its game to exit.
    SavedVariables,
    /// A failure recovery cannot attribute to one target, which fails every committing
    /// command on every target.
    Unattributed,
    /// A repair journal names a skill directory omaddons cannot attribute without the
    /// journal's word (E5).
    SkillDir,
}

impl StopReason {
    pub const ALL: [Self; 9] = [
        Self::JournalTooNew,
        Self::JournalUnreadable,
        Self::MarkerMismatch,
        Self::StepUnrecognized,
        Self::UnexpectedState,
        Self::PreservationIncomplete,
        Self::SavedVariables,
        Self::Unattributed,
        Self::SkillDir,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::JournalTooNew => "journal_too_new",
            Self::JournalUnreadable => "journal_unreadable",
            Self::MarkerMismatch => "marker_mismatch",
            Self::StepUnrecognized => "step_unrecognized",
            Self::UnexpectedState => "unexpected_state",
            Self::PreservationIncomplete => "preservation_incomplete",
            Self::SavedVariables => "saved_variables",
            Self::Unattributed => "unattributed",
            Self::SkillDir => "skill_dir",
        }
    }

    /// The reason `text` names, as [`StopReason::as_str`] writes it.
    pub fn parse(text: &str) -> Option<Self> {
        // ubs:ignore — comparing reason names, not secrets
        Self::ALL.into_iter().find(|reason| reason.as_str() == text)
    }
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
            Self::TooNew => StopReason::JournalTooNew.as_str(),
            Self::Unreadable => StopReason::JournalUnreadable.as_str(),
        }
    }

    /// Its stop reason.
    pub fn stop_reason(self) -> StopReason {
        match self {
            Self::TooNew => StopReason::JournalTooNew,
            Self::Unreadable => StopReason::JournalUnreadable,
        }
    }

    /// What to do about it: the doctor's explanation of its state, without `-t` (M4-D53).
    /// A folder journal's stop names its client through [`stop_fix`].
    pub fn fix(self) -> Suggestion {
        fence::explain_fix(Some(self.stop_reason()))
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
        preserve: Vec::new(),
        preserved: Vec::new(),
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
            JournalLine::Preserve(intent) if parsed.prepared.is_some() => {
                parsed.preserve.push(intent);
            }
            JournalLine::Preserved(done) if parsed.prepared.is_some() => {
                parsed.preserved.push(done.id);
            }
            JournalLine::Preserve(_) | JournalLine::Preserved(_) => parsed.unknown_lines += 1,
        }
    }
    parsed
}

/// Every journal in `journal/`, by transaction, in the order recovery takes them. A
/// directory that does not exist lists nothing; one that cannot be listed is an error.
pub fn list(paths: &Paths) -> Result<Vec<(TxId, PathBuf)>> {
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
            let tx = crate::state::paths::tx_named(name.strip_suffix(".jsonl")?)?;
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

/// `Some(atomic_swap)` when `tx`'s journal reached `committed`, readable throughout: a
/// failure after that line was written leaves the transaction standing, and recovery
/// only prunes the journal. `None` for any other journal, or none.
pub(crate) fn committed_swap(paths: &Paths, tx: &TxId) -> Option<bool> {
    let parsed = read(&existing(paths, tx)?).ok()?;
    // ubs:ignore — comparing a journal phase, not a secret
    (parsed.phase == Phase::Committed && parsed.unrecoverable().is_none()).then(|| {
        !parsed
            .steps
            .iter()
            .any(|step| matches!(step.op, StepOp::Rename))
    })
}

/// Whether `tx`'s journal is one recovery still needs: started (`prepared`) and neither
/// committed nor recovered. One it cannot read (or read at all) counts too: recovery
/// reports it, and leaves it.
pub(crate) fn unfinished(paths: &Paths, tx: &TxId) -> bool {
    existing(paths, tx).is_some_and(|path| {
        read(&path).map_or(true, |parsed| {
            matches!(parsed.phase, Phase::Prepared | Phase::Swapped)
                || parsed.unrecoverable().is_some()
        })
    })
}

/// `tx`'s journal, under whichever of its two names it has.
pub(crate) fn existing(paths: &Paths, tx: &TxId) -> Option<PathBuf> {
    [paths.journal_wtf(tx), paths.journal(tx)]
        .into_iter()
        .find(|path| std::fs::symlink_metadata(path).is_ok())
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
/// Writes nothing. A journal directory that cannot be listed is the failure recovery
/// cannot attribute to one client (`unattributed`): every command reads this first.
pub fn pending(paths: &Paths) -> Result<Vec<Pending>> {
    let mut found = Vec::new();
    for (tx, path) in list(paths).map_err(|error| unattributed(error, None))? {
        let (target, unrecoverable) = match read(&path) {
            Ok(parsed)
                if matches!(parsed.phase, Phase::Committed) && parsed.unrecoverable().is_none() =>
            {
                continue;
            }
            Ok(parsed) => (parsed.target, parsed.unrecoverable()),
            // Pruned by its writer's cleanup since the listing: nothing is pending.
            Err(_) if existing(paths, &tx).is_none() => continue,
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
/// The folder journals come first, then the doctor's repairs (E5), as recovery takes
/// them. Writes nothing.
pub fn pending_warnings(paths: &Paths) -> Result<Vec<Warning>> {
    let pending = pending(paths)?;
    let repairs = super::repair::pending_repairs(paths)?;
    if pending.is_empty() && repairs.is_empty() {
        return Ok(Vec::new());
    }
    let held = matches!(probe_global(paths)?, LockState::Held { .. });
    // A repair recovery will stop at, and a journal it would leave behind a barrier, get
    // the text recovery gives them (E14), so a run that reads them first and recovers
    // later reports each once.
    let mut said = stuck_repairs(paths, &repairs);
    let folder = pending.into_iter().map(
        |Pending {
             tx,
             target,
             unrecoverable,
         }| {
            let on = target
                .map(|t| format!(" on target {t}"))
                .unwrap_or_default();
            said.remove(&tx).unwrap_or_else(|| match unrecoverable {
                Some(why) => unrecoverable_warning(paths, &tx, target, why),
                None => unfinished_warning(&format!("transaction {tx}{on}"), held),
            })
        },
    );
    let folder: Vec<Warning> = folder.collect();
    let repairs = repairs.into_iter().map(|repair| {
        said.remove(&repair.tx)
            .unwrap_or_else(|| match repair.unrecoverable {
                Some(why) => super::repair::recover::unrecoverable_warning(&repair.tx, why),
                None => unfinished_warning(&format!("repair transaction {}", repair.tx), held),
            })
    });
    Ok(folder.into_iter().chain(repairs).collect())
}

#[cfg(test)]
thread_local! {
    /// How many times this thread's readers simulated recovery for their repair journals.
    pub(crate) static REPAIR_DIAGNOSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The `journal_pending` warning recovery gives, by transaction, of each repair journal
/// it stops at (`fence::stuck_warning`) and of each journal of either family it leaves
/// behind a barrier (`fence::behind_warning`, with the stop recovery would follow to).
/// Simulates recovery (`diagnose`) only when a repair journal waits, so a home without
/// one costs a reader nothing more: only a repair holds a journal back.
fn stuck_repairs(
    paths: &Paths,
    repairs: &[super::repair::PendingRepair],
) -> std::collections::BTreeMap<TxId, Warning> {
    if repairs.is_empty() {
        return std::collections::BTreeMap::new();
    }
    #[cfg(test)]
    REPAIR_DIAGNOSES.with(|count| count.set(count.get() + 1));
    let diagnoses = diagnose(paths, &[], &no_saved_variables).journals;
    let closed = |d: &JournalDiagnosis| {
        d.family == JournalFamily::Repair && super::fence::closes(paths, &d.path)
    };
    diagnoses
        .iter()
        .filter_map(|d| match d.verdict {
            JournalVerdict::BehindBarrier {
                owner,
                stop: Some(_),
            } => {
                // ubs:ignore — comparing transaction ids (public ULIDs), not secrets
                let od = diagnoses.iter().find(|od| od.tx == owner)?;
                let root = root_of(&diagnoses, od);
                let stuck = Stuck {
                    stop: super::fence::stop_of(root, closed(root))?,
                    closed: closed(root),
                };
                let behind = match (d.family, od.family) {
                    (JournalFamily::Folder, _) => Behind::Folder {
                        tx: d.tx,
                        target: d.target,
                        owner,
                    },
                    (JournalFamily::Repair, JournalFamily::Folder) => {
                        Behind::Unresolved { tx: d.tx, owner }
                    }
                    (JournalFamily::Repair, JournalFamily::Repair) => {
                        Behind::Repair { tx: d.tx, owner }
                    }
                };
                Some((d.tx, super::fence::behind_warning(behind, &stuck)))
            }
            JournalVerdict::BehindBarrier { stop: None, .. } => None,
            // ubs:ignore — comparing journal families, not secrets
            _ if d.family == JournalFamily::Repair => {
                let Expected::Stops { reason, .. } = d.expected() else {
                    return None;
                };
                Some((
                    d.tx,
                    super::fence::stuck_warning(d.tx, Some(reason), closed(d)),
                ))
            }
            _ => None,
        })
        .collect()
}

/// The journal `d` waits behind at the end of its chain of barriers: the one recovery
/// could not settle, whose stop the chain reports.
fn root_of<'d>(diagnoses: &'d [JournalDiagnosis], d: &'d JournalDiagnosis) -> &'d JournalDiagnosis {
    let mut at = d;
    for _ in 0..diagnoses.len() {
        let JournalVerdict::BehindBarrier { owner, .. } = at.verdict else {
            break;
        };
        // ubs:ignore — comparing transaction ids (public ULIDs), not secrets
        match diagnoses.iter().find(|od| od.tx == owner) {
            Some(od) => at = od,
            None => break,
        }
    }
    at
}

/// What a reader says of an unfinished transaction this build can recover, named by
/// `what`: in progress while the global lock is `held`, else waiting for the next change.
fn unfinished_warning(what: &str, held: bool) -> Warning {
    if held {
        Warning::new(WarningCode::TxInProgress, format!("{what} is in progress")).with_fix(
            Suggestion::text("wait for the running omaddons command to finish"),
        )
    } else {
        Warning::new(
            WarningCode::JournalPending,
            format!(
                "{what} did not finish; the next omaddons command that changes addons, settings or targets recovers it (dry runs never do)"
            ),
        )
        .with_fix(Suggestion::command(["omaddons", "status"]))
    }
}

/// What a reader, and another target's recovery, say of a journal this build cannot
/// recover: the same warning both ways, so a run reports it once, with the fix its stop
/// error carries ([`stop_fix`]).
fn unrecoverable_warning(
    paths: &Paths,
    tx: &TxId,
    target: Option<TargetKey>,
    why: Unrecoverable,
) -> Warning {
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
    .with_fix(stop_fix(paths, why.stop_reason(), target).unwrap_or_else(|| why.fix()))
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

/// Recovers every leftover journal, in three passes (M4-D31, E14; the order is
/// [`Passes`]'): first the doctor's repairs that suspend or reactivate a journal (a
/// set-aside and its undo) and every repair whose `prepared` line cannot be read, then
/// the folder journals, then the other repairs. No writer is alive except ones still
/// cleaning up after commit, whose in-flight lock is held and whose journals are skipped.
/// One `journal_recovered` warning per rolled transaction, each followed by what its
/// roll-forward left; committed and unprepared journals are removed silently.
///
/// A journal behind a repair recovery could not settle is never recovered: a folder
/// journal of the caller's own target (or with no readable target) fails the caller with
/// that repair's stop ([`fence::behind_error`]); anything else is one `journal_pending`
/// warning ([`fence::behind_warning`]). A repair's own stop never fails the caller: the
/// fences every mutating command loads refuse what it protects. Every pass decides from
/// the disk as it finds it, so a recovery killed anywhere is finished by the next
/// lock-taking command, in the same order.
pub fn recover_all(env: &RecoveryEnv<'_>) -> Result<Vec<Warning>> {
    recover_all_with(env, &mut OnDisk)
}

/// What recovery did with one transaction it reached, for a caller that reports each
/// one's outcome rather than warnings to show (the doctor's delegation, M4-D37).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxOutcome {
    pub tx: TxId,
    pub family: JournalFamily,
    pub result: TxResult,
    /// The warnings recovering it gave: its `journal_recovered` first when it rolled,
    /// then what its roll-forward left (`saved_variables_deferred`, …).
    pub warnings: Vec<Warning>,
}

/// How recovering one transaction ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxResult {
    RolledBack,
    RolledForward,
    /// Removed with nothing run: never prepared, or committed or rolled back already.
    Pruned,
    /// Left as it is: recovery stopped at it, with its error's code and `details.reason`.
    Stopped {
        code: ErrorCode,
        reason: Option<String>,
    },
}

/// [`recover_disclosed`]'s result.
#[derive(Debug, Clone)]
pub struct Disclosed {
    /// One per transaction recovery reached, in the order it reached them.
    pub outcomes: Vec<TxOutcome>,
    /// The warnings of no one transaction it reached (a journal it left behind a repair,
    /// one it could not recover on another client), in recovery's order; with no
    /// restriction, every warning.
    pub warnings: Vec<Warning>,
    /// What stopped recovery before it went through every journal, when something did; it
    /// is also a `journal_pending` warning in `warnings`.
    pub error: Option<Error>,
}

/// Recovery's settle, recording each transaction's outcome as it reaches it.
#[derive(Default)]
struct Disclosing {
    outcomes: Vec<TxOutcome>,
}

impl Disclosing {
    fn stopped(error: &Error) -> TxResult {
        TxResult::Stopped {
            code: error.code,
            reason: error.details["reason"].as_str().map(str::to_owned),
        }
    }
}

impl Settle for Disclosing {
    fn folder(&mut self, env: &RecoveryEnv<'_>, tx: &TxId, path: PathBuf) -> Result<Vec<Warning>> {
        let recovered = recover_one_outcome(env, tx, path);
        let (result, warnings) = match &recovered {
            Ok((Some(RecoveryAction::RolledBack), warnings)) => {
                (TxResult::RolledBack, warnings.clone())
            }
            Ok((Some(RecoveryAction::RolledForward), warnings)) => {
                (TxResult::RolledForward, warnings.clone())
            }
            Ok((None, warnings)) => (TxResult::Pruned, warnings.clone()),
            Err(error) => (Self::stopped(error), Vec::new()),
        };
        self.outcomes.push(TxOutcome {
            tx: *tx,
            family: JournalFamily::Folder,
            result,
            warnings,
        });
        recovered.map(|(_, warnings)| warnings)
    }

    fn repair(&mut self, env: &RecoveryEnv<'_>, tx: TxId, path: &Path) -> Result<Option<Warning>> {
        use super::repair::recover::Settled;
        let settled = super::repair::recover::settle_outcome(env, tx, path);
        let result = match &settled {
            Ok((Settled::Live, _)) => None,
            Ok((Settled::Pruned, _)) => Some(TxResult::Pruned),
            Ok((Settled::Rolled(RecoveryAction::RolledBack), _)) => Some(TxResult::RolledBack),
            Ok((Settled::Rolled(RecoveryAction::RolledForward), _)) => {
                Some(TxResult::RolledForward)
            }
            Err(error) => Some(Self::stopped(error)),
        };
        if let Some(result) = result {
            let warnings = match &settled {
                Ok((_, warning)) => warning.iter().cloned().collect(),
                Err(_) => Vec::new(),
            };
            self.outcomes.push(TxOutcome {
                tx,
                family: JournalFamily::Repair,
                result,
                warnings,
            });
        }
        settled.map(|(_, warning)| warning)
    }
}

/// Recovery under the global lock `lock`, as [`recover_under`] runs it, restricted to the
/// transactions `only` names (`None`: every one), with each reached transaction's outcome.
/// A journal `only` does not name is skipped in every pass exactly as one whose writer is
/// alive: left as it is, no warning, no error (so a barrier owner skipped this way holds
/// back what it names, silently). What stopped recovery is in `error`, and a
/// `journal_pending` warning. Rolling any journal back or forward rings the doorbell once.
pub fn recover_disclosed(
    lock: &GlobalLockGuard,
    paths: &Paths,
    sv_writable: &dyn Fn(&Path) -> bool,
    command: &str,
    actor: Actor,
    only: Option<&BTreeSet<TxId>>,
) -> Disclosed {
    // A stop that ends recovery (an `unattributed` journal's, say) keeps the stop's fix.
    let left = |error: &Error| {
        Warning::new(
            WarningCode::JournalPending,
            format!(
                "unfinished transactions could not be recovered: {}",
                error.message
            ),
        )
        .with_fix(if is_stop(error) {
            error.fix.clone()
        } else {
            Suggestion::command(["omaddons", "status"])
        })
    };
    let scopes = match ScopeTable::new(paths, &[], None) {
        Ok(scopes) => scopes,
        Err(error) => {
            return Disclosed {
                outcomes: Vec::new(),
                warnings: vec![left(&error)],
                error: Some(error),
            };
        }
    };
    let env = RecoveryEnv {
        lock,
        paths,
        scopes: &scopes,
        own: &[],
        sv_writable,
    };
    let mut disclosing = Disclosing::default();
    let recovered = match only {
        None => recover_all(&env),
        Some(only) => recover_all_in(&env, &mut disclosing, Some(only)),
    };
    let outcomes = disclosing.outcomes;
    let (warnings, error) = match recovered {
        Ok(mut all) => {
            // What a reached transaction gave is its outcome's; the rest is the run's.
            for warning in outcomes.iter().flat_map(|o| &o.warnings) {
                // ubs:ignore — comparing warnings (a code and its text), not secrets
                if let Some(at) = all.iter().position(|w| w == warning) {
                    all.remove(at);
                }
            }
            (all, None)
        }
        Err(error) => (vec![left(&error)], Some(error)),
    };
    if warnings
        .iter()
        .chain(outcomes.iter().flat_map(|o| &o.warnings))
        .any(|w| matches!(w.code, WarningCode::JournalRecovered))
    {
        doorbell::ring(paths, command, actor);
    }
    Disclosed {
        outcomes,
        warnings,
        error,
    }
}

/// What recovery does with one journal whose writer is gone, behind a seam so the unit
/// tests observe the passes' order without crashing anything.
pub(crate) trait Settle {
    /// A folder journal ([`recover_one`]).
    fn folder(&mut self, env: &RecoveryEnv<'_>, tx: &TxId, path: PathBuf) -> Result<Vec<Warning>>;
    /// A repair journal ([`super::repair::recover::settle_one`]).
    fn repair(&mut self, env: &RecoveryEnv<'_>, tx: TxId, path: &Path) -> Result<Option<Warning>>;
}

/// Recovery itself.
struct OnDisk;

impl Settle for OnDisk {
    fn folder(&mut self, env: &RecoveryEnv<'_>, tx: &TxId, path: PathBuf) -> Result<Vec<Warning>> {
        recover_one(env, tx, path)
    }

    fn repair(&mut self, env: &RecoveryEnv<'_>, tx: TxId, path: &Path) -> Result<Option<Warning>> {
        super::repair::recover::settle_one(env, tx, path)
    }
}

/// [`recover_all`] through `settle`.
pub(crate) fn recover_all_with(
    env: &RecoveryEnv<'_>,
    settle: &mut dyn Settle,
) -> Result<Vec<Warning>> {
    recover_all_in(env, settle, None)
}

/// Whether `only` leaves `tx` out.
fn excluded(only: Option<&BTreeSet<TxId>>, tx: &TxId) -> bool {
    only.is_some_and(|only| !only.contains(tx))
}

/// [`recover_all_with`], restricted to the transactions `only` names ([`recover_disclosed`]).
fn recover_all_in(
    env: &RecoveryEnv<'_>,
    settle: &mut dyn Settle,
    only: Option<&BTreeSet<TxId>>,
) -> Result<Vec<Warning>> {
    // Recovery writes with the caller's scopes but never its fences (E14): it is ordered,
    // never refused mid-way. Order is what keeps it off a stuck repair's resources.
    let unfenced = env.scopes.without_fences();
    let env = &RecoveryEnv {
        scopes: &unfenced,
        ..*env
    };
    let mut warnings = Vec::new();
    let listing = super::repair::recover::listing(env.paths)?;
    let mut passes = Passes::new(
        listing.iter().map(|l| l.listed.clone()).collect(),
        env.paths,
    );
    // Pass 1: the repairs that own a barrier.
    while let Some(owner) = passes.next_owner() {
        // ubs:ignore — comparing transaction ids (public ULIDs), not secrets
        if let Some(listed) = listing.iter().find(|l| l.listed.tx == owner) {
            let outcome = if excluded(only, &owner) {
                Outcome::Live {
                    finished: listed.finished,
                }
            } else {
                repair_pass(env, settle, &passes, listed, &mut warnings)
            };
            passes.record(owner, outcome);
        }
    }
    // Pass 2: the folder journals, listed now (a suspension pass 1 rolled back has put
    // its journal back; a reactivation it rolled back has taken one away).
    let mut unresolved: Vec<(TxId, Stuck)> = Vec::new();
    for (tx, path) in list(env.paths).map_err(|error| unattributed(error, None))? {
        if excluded(only, &tx)
            || InflightLock::is_live(env.paths, &tx)
                .map_err(|error| unattributed(error, Some(&tx)))?
        {
            continue;
        }
        // Whose it is, for isolation, from its `prepared` line even when that line does
        // not decode: a journal whose owner cannot be told fails the caller as before.
        let parsed = read(&path).ok();
        // Gone since the listing: a writer's cleanup prunes its finished journal under its
        // own in-flight lock, never the global one, so it can go between the listing and
        // the read. Nothing is left to recover, as if it had not been listed.
        if parsed.is_none() && existing(env.paths, &tx).is_none() {
            continue;
        }
        let target = parsed.as_ref().and_then(|p| p.target);
        let unrecoverable = parsed.as_ref().and_then(Parsed::unrecoverable);
        // No target to attribute its failure to: it fails every committing command.
        let owner_unknown = target.is_none();
        if let Some(owner) = passes.behind(&tx) {
            // Behind a live writer's repair (seen only for one this build cannot read),
            // it waits for that writer, silently.
            let Some(stuck) = passes.stop(&owner) else {
                continue;
            };
            if owner_unknown || target.is_some_and(|t| env.own.contains(&t)) {
                return Err(fence::behind_error(tx, target, owner, stuck));
            }
            warnings.push(fence::behind_warning(
                Behind::Folder { tx, target, owner },
                stuck,
            ));
            unresolved.push((tx, stuck.clone()));
            continue;
        }
        match settle.folder(env, &tx, path) {
            Ok(recovered) => warnings.extend(recovered),
            Err(error) => {
                let error = with_stop_fix(env.paths, error);
                let reason = error.details["reason"].as_str().and_then(StopReason::parse);
                // A stop's warning carries the stop's own fix; a wait's, `status`.
                let fix = if is_stop(&error) {
                    error.fix.clone()
                } else {
                    Suggestion::command(["omaddons", "status"])
                };
                match (target.filter(|t| !env.own.contains(t)), unrecoverable) {
                    (Some(other), Some(why)) => {
                        warnings.push(unrecoverable_warning(env.paths, &tx, Some(other), why));
                    }
                    // A warning has no details: a stop's or a wait's reason goes in its text.
                    (Some(other), None) => warnings.push(
                        Warning::new(
                            WarningCode::JournalPending,
                            format!(
                                "transaction {tx} on target {other} did not finish and could not be recovered{}: {}",
                                reason
                                    .map(|why| format!(" ({})", why.as_str()))
                                    .unwrap_or_default(),
                                error.message
                            ),
                        )
                        .with_fix(fix),
                    ),
                    (None, _) if owner_unknown => return Err(unattributed(error, Some(&tx))),
                    // The caller's own target keeps its stop's reason.
                    (None, _) => return Err(error),
                }
                unresolved.push((tx, fence::folder_stuck(tx, target, error.code, reason)));
            }
        }
    }
    // Pass 3: the other repairs, listed again after pass 2, in transaction order.
    let relisted = super::repair::recover::listing(env.paths)?;
    let txs: Vec<TxId> = relisted.iter().map(|l| l.listed.tx).collect();
    let mut overlaps: Option<Vec<(TxId, ResourceSet)>> = None;
    for tx in passes.rest(&txs) {
        // ubs:ignore — comparing transaction ids (public ULIDs), not secrets
        let Some(listed) = relisted.iter().find(|l| l.listed.tx == tx) else {
            continue;
        };
        if excluded(only, &tx) {
            passes.record(
                tx,
                Outcome::Live {
                    finished: listed.finished,
                },
            );
            continue;
        }
        let held = listed.listed.live || passes.behind(&tx).is_some();
        if let (false, false, PreparedRead::Decoded(prepared)) =
            (held, unresolved.is_empty(), &listed.listed.prepared)
        {
            let sets = overlaps.get_or_insert_with(|| protected_sets(env, &unresolved));
            // A resource text that does not parse could overlap anything.
            let waits = match prepared.resources.to_set() {
                Ok(own) => passes.waits_for(&own, sets),
                Err(_) => sets.first().map(|(f, _)| *f),
            };
            // ubs:ignore — comparing transaction ids (public ULIDs), not secrets
            if let Some((f, stuck)) = waits.and_then(|f| unresolved.iter().find(|(u, _)| *u == f)) {
                warnings.push(fence::behind_warning(
                    Behind::Unresolved { tx, owner: *f },
                    stuck,
                ));
                passes.record(tx, Outcome::Behind(*f));
                continue;
            }
        }
        let outcome = repair_pass(env, settle, &passes, listed, &mut warnings);
        passes.record(tx, outcome);
    }
    Ok(warnings)
}

/// One repair in pass 1 or 3: skipped while its writer is alive, left as it is behind an
/// owner that holds it (with its behind warning), else recovered (with its stuck warning
/// when recovery stops at it). What happened, for the passes to record.
fn repair_pass(
    env: &RecoveryEnv<'_>,
    settle: &mut dyn Settle,
    passes: &Passes,
    listed: &super::repair::recover::RepairListing,
    warnings: &mut Vec<Warning>,
) -> Outcome {
    let tx = listed.listed.tx;
    if listed.listed.live {
        return Outcome::Live {
            finished: listed.finished,
        };
    }
    if let Some(owner) = passes.behind(&tx) {
        if let Some(stuck) = passes.stop(&owner) {
            warnings.push(fence::behind_warning(Behind::Repair { tx, owner }, stuck));
        }
        return Outcome::Behind(owner);
    }
    match settle.repair(env, tx, &listed.path) {
        Ok(warning) => {
            warnings.extend(warning);
            Outcome::Settled
        }
        Err(error) => {
            let closed = fence::closes(env.paths, &listed.path);
            let stop = fence::stop_of_error(tx, &error, closed);
            warnings.push(fence::stuck_warning(
                tx,
                stop.reason.and_then(StopReason::parse),
                closed,
            ));
            Outcome::Unsettled(Stuck { stop, closed })
        }
    }
}

/// What each folder journal pass 2 left unresolved protects, as `journal::diagnose`
/// fills its `protected` (its own, never recomputed here), as a resource set
/// ([`fence::resources_of`]). A journal the diagnosis does not list stands for everything
/// (fail closed). Diagnoses only when a pass-3 repair might wait: a rare, read-only cost.
fn protected_sets(env: &RecoveryEnv<'_>, unresolved: &[(TxId, Stuck)]) -> Vec<(TxId, ResourceSet)> {
    let diagnoses = diagnose(env.paths, env.scopes.client_dirs(), env.sv_writable);
    unresolved
        .iter()
        .map(|(tx, _)| {
            let found = diagnoses
                .journals
                .iter()
                .find(|d| d.tx == *tx && d.family == JournalFamily::Folder);
            (
                *tx,
                found.map_or_else(everything, |d| fence::resources_of(&d.protected)),
            )
        })
        .collect()
}

/// A set that conflicts with every write: the filesystem's root, written.
fn everything() -> ResourceSet {
    let mut set = ResourceSet::new();
    if let Ok(root) = Resource::from_canonical(Path::new("/"), PathKind::Subtree) {
        set.write(root);
    }
    set
}

/// A recovery failure no single target owns (the journal directory does not list, an
/// in-flight lock cannot be probed, or a journal's target cannot be read): it fails
/// every committing command on every target, so `details.reason` says `unattributed`
/// (over any reason it had), with the transaction when one is known. The code, message
/// and fix still say what failed.
pub(crate) fn unattributed(mut error: Error, tx: Option<&TxId>) -> Error {
    if !error.details.is_object() {
        error.details = serde_json::json!({});
    }
    if let Some(details) = error.details.as_object_mut() {
        details.insert(
            "reason".to_owned(),
            serde_json::json!(StopReason::Unattributed.as_str()),
        );
        if let Some(tx) = tx {
            details
                .entry("journal_pending")
                .or_insert_with(|| serde_json::json!(tx));
        }
    }
    // The doctor's explanation (M4-D53): of the journal it names, machine-scoped; of the
    // state directory when it names none (the journal directory could not be listed).
    error.fix = if error.details["journal_pending"].is_null() {
        crate::doctor::registry::explain_fix(crate::doctor::registry::ids::STATE_CORRUPT, None)
    } else {
        fence::explain_fix(Some(StopReason::Unattributed))
    };
    error
}

/// The fix of a recovery stop (M4-D53): the doctor's explanation of exactly that state,
/// `omaddons doctor explain <fm> [-t <id>]`. `-t` names the journal's target by its id
/// when the registry has it; with no entry, or a registry that cannot be read, there is
/// no `-t`, so the command still runs and lists every instance; never `-t <key>`.
/// `unattributed` is machine-scoped. `None` for `saved_variables`: that journal waits for
/// the game, and its own prose fix is true.
pub(crate) fn stop_fix(
    paths: &Paths,
    reason: StopReason,
    target: Option<TargetKey>,
) -> Option<Suggestion> {
    match reason {
        StopReason::SavedVariables => None,
        StopReason::Unattributed => Some(fence::explain_fix(Some(reason))),
        _ => {
            let id = target.and_then(|key| {
                crate::state::registry::load_registry(paths)
                    .ok()
                    .and_then(|loaded| loaded.value.entry(&key).map(|entry| entry.id.clone()))
            });
            Some(crate::doctor::registry::explain_fix(
                crate::doctor::registry::ids::for_stop_reason(reason),
                id.as_deref(),
            ))
        }
    }
}

/// `error` with its fix replaced by [`stop_fix`] when it is a recovery stop of one
/// journal: its `details.reason` a [`StopReason`] other than `saved_variables`, its
/// target `details.target`. An `unattributed` failure keeps the fix [`unattributed`]
/// gave it. Any other error comes back unchanged.
pub(crate) fn with_stop_fix(paths: &Paths, mut error: Error) -> Error {
    let reason = error.details["reason"].as_str().and_then(StopReason::parse);
    let target = error.details["target"]
        .as_str()
        .and_then(|key| key.parse::<TargetKey>().ok());
    if let Some(reason) = reason.filter(|r| *r != StopReason::Unattributed)
        && let Some(fix) = stop_fix(paths, reason, target)
    {
        error.fix = fix;
    }
    error
}

/// Whether `error` is a recovery stop the doctor explains: its `details.reason` a
/// [`StopReason`] other than `saved_variables`. Its fix is then that explanation.
pub(crate) fn is_stop(error: &Error) -> bool {
    error.details["reason"]
        .as_str()
        .and_then(StopReason::parse)
        .is_some_and(|reason| reason != StopReason::SavedVariables)
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
    recover_disclosed(lock, paths, sv_writable, command, actor, None).warnings
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

/// Whether a journal whose writer is gone waits for recovery: a folder journal, or a
/// repair's.
pub fn recoverable(paths: &Paths) -> Result<bool> {
    let folder = pending(paths)?.into_iter().map(|found| found.tx);
    let repairs = super::repair::pending_repairs(paths)?
        .into_iter()
        .map(|found| found.tx);
    for tx in folder.chain(repairs) {
        if !InflightLock::is_live(paths, &tx)? {
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
    let unfenced = env.scopes.without_fences();
    let env = &RecoveryEnv {
        scopes: &unfenced,
        ..*env
    };
    let Some(path) = existing(env.paths, tx) else {
        return Ok(None);
    };
    let parsed = read(&path)?;
    recover_parsed(env, tx, &path, &parsed).map_err(|error| with_stop_fix(env.paths, error))
}

/// Recovers one journal and reports it.
fn recover_one(env: &RecoveryEnv<'_>, tx: &TxId, path: PathBuf) -> Result<Vec<Warning>> {
    recover_one_outcome(env, tx, path).map(|(_, warnings)| warnings)
}

/// [`recover_one`], with which way it rolled (`None`: it only removed the journal).
fn recover_one_outcome(
    env: &RecoveryEnv<'_>,
    tx: &TxId,
    path: PathBuf,
) -> Result<(Option<RecoveryAction>, Vec<Warning>)> {
    let parsed = read(&path)?;
    let recovered = recover_parsed(env, tx, &path, &parsed)?;
    let (Some(recovered), Some(prepared)) = (recovered, &parsed.prepared) else {
        return Ok((None, Vec::new()));
    };
    let action = recovered.action;
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
    Ok((Some(action), warnings))
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
    // The marker is read only for a journal this build can read, as ever.
    let scoped = match &parsed.prepared {
        Some(prepared) if parsed.unrecoverable().is_none() => journal_scopes(env, prepared),
        _ => None,
    };
    let route = route(parsed, scoped.is_some(), env.sv_writable);
    let paths = env.paths;
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
    let mut s = WriteSession::untracked(writer, scopes, "recovery");
    let atomic_swap = !parsed
        .steps
        .iter()
        .any(|step| matches!(step.op, StepOp::Rename));
    let mut recovered = match route {
        Route::Unrecoverable(why) => return Err(unrecoverable(tx, parsed, why)),
        Route::MarkerMismatch(prepared) => return Err(marker_mismatch(tx, prepared)),
        Route::WaitsForGame(prepared) => return Err(waits_for_the_game(tx, prepared)),
        Route::Prune => None,
        Route::Forward(prepared) => {
            let left = roll_forward(&mut s, env, path, prepared, parsed.torn_tail)?;
            Some(Recovered {
                action: RecoveryAction::RolledForward,
                atomic_swap,
                sv_deferred: left.as_ref().map_or(0, |(files, _)| *files),
                warnings: left.into_iter().map(|(_, warning)| warning).collect(),
                snapshot_kept: None,
            })
        }
        Route::BackPreserving(prepared) => {
            let warnings = roll_back_preserving(&mut s, env, tx, path, prepared, parsed)?;
            Some(Recovered {
                action: RecoveryAction::RolledBack,
                atomic_swap,
                warnings,
                sv_deferred: 0,
                snapshot_kept: None,
            })
        }
        Route::Back(prepared) => {
            roll_back(&mut s, &parsed.steps, tx, prepared.target_key)?;
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

/// What recovery does with a journal, decided before it writes anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route<'a> {
    /// This build cannot read it: left whole ([`Parsed::unrecoverable`]).
    Unrecoverable(Unrecoverable),
    /// Unfinished, and its client no longer carries its target's marker.
    MarkerMismatch(&'a Prepared),
    /// A settings restore with a step, while its game runs or may (M3-D29).
    WaitsForGame(&'a Prepared),
    /// Nothing to finish or undo: never prepared, or committed.
    Prune,
    /// Swapped: roll forward.
    Forward(&'a Prepared),
    /// Prepared, renaming `WTF/`: roll back, preserving what it displaced.
    BackPreserving(&'a Prepared),
    /// Prepared: roll back.
    Back(&'a Prepared),
}

/// [`Route`] for `parsed`, in recovery's order: what this build cannot read, then a
/// marker that is not its target's (`owns_client_dir` is false), then the game, then the
/// phase. `sv_writable` is asked only for an unfinished settings restore with a step,
/// and the check is made now, not trusted from before a failure.
pub(crate) fn route<'a>(
    parsed: &'a Parsed,
    owns_client_dir: bool,
    sv_writable: &dyn Fn(&Path) -> bool,
) -> Route<'a> {
    if let Some(why) = parsed.unrecoverable() {
        return Route::Unrecoverable(why);
    }
    let unfinished = matches!(parsed.phase, Phase::Prepared | Phase::Swapped);
    let Some(prepared) = &parsed.prepared else {
        return Route::Prune;
    };
    if unfinished && !owns_client_dir {
        return Route::MarkerMismatch(prepared);
    }
    // ... and never while the game runs, or may: a step may have moved `WTF/`.
    if prepared.writes_wtf
        && unfinished
        && !parsed.steps.is_empty()
        && !sv_writable(&prepared.client_dir)
    {
        return Route::WaitsForGame(prepared);
    }
    match parsed.phase {
        Phase::Unprepared | Phase::Committed => Route::Prune,
        Phase::Swapped => Route::Forward(prepared),
        Phase::Prepared if prepared.writes_wtf => Route::BackPreserving(prepared),
        Phase::Prepared => Route::Back(prepared),
    }
}

/// An unfinished journal whose client dir's marker does not name its target, or that
/// the scopes cannot add: `scope_denied`, left for manual repair.
fn marker_mismatch(tx: &TxId, prepared: &Prepared) -> Error {
    Error::new(
        ErrorCode::ScopeDenied,
        format!(
            "transaction {tx} names {}, whose marker is not target {}'s; recovery leaves it for manual repair",
            prepared.client_dir.display(),
            prepared.target_key
        ),
        // Without `-t` until recovery's entry point names the client (`with_stop_fix`).
        fence::explain_fix(Some(StopReason::MarkerMismatch)),
    )
    .details(serde_json::json!({
        "reason": StopReason::MarkerMismatch.as_str(),
        "journal_pending": tx,
        "target": prepared.target_key,
    }))
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
        "reason": StopReason::SavedVariables.as_str(),
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
    fn stop_reason(self) -> StopReason {
        match self {
            Self::UnexpectedState => StopReason::UnexpectedState,
            Self::PreservationIncomplete => StopReason::PreservationIncomplete,
        }
    }

    fn reason(self) -> &'static str {
        self.stop_reason().as_str()
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
        // Without `-t` until recovery's entry point names the client (`with_stop_fix`).
        fence::explain_fix(Some(stop.stop_reason())),
    )
    .details(serde_json::json!({
        "reason": stop.reason(),
        "journal_pending": tx,
        "target": prepared.target_key,
    }))
}

/// A `writes_wtf` rollback (M3-D24 (c)): the reverse walk, which leaves what it cannot
/// reverse but can place as it is (rule 2), then a durable home for every displaced
/// version no other copy holds, before any cleanup (rule 3), through a write-ahead intent.
/// A journal whose intent is written has finished its walk: the items may have moved
/// since, so it resumes the protocol instead. Returns a `restore_rolled_back` warning per
/// backup preserved. An identity the walk cannot place stops recovery
/// (`unexpected_state`), and so does any failure in the protocol
/// (`preservation_incomplete`), with everything kept for the next recovery.
fn roll_back_preserving(
    s: &mut WriteSession<'_>,
    env: &RecoveryEnv<'_>,
    tx: &TxId,
    journal: &Path,
    prepared: &Prepared,
    parsed: &Parsed,
) -> Result<Vec<Warning>> {
    let incomplete = |error: Error| {
        let why = super::preserve::redacted(prepared, &error.message);
        stopped(tx, prepared, Stop::PreservationIncomplete, &why)
    };
    // Preservation appends to the journal (`preserve`, then `preserved`): after a torn
    // last line, that line would merge with the fragment, and once not last it would be
    // an unknown line that leaves the journal unreadable for good. The fragment never ran.
    if parsed.torn_tail {
        drop_torn_tail(s, journal).map_err(incomplete)?;
    }
    let mut intents = parsed.preserve.clone();
    if intents.is_empty() {
        let live = ClientPaths::new(&prepared.client_dir).wtf();
        let displaced = roll_back_wtf(s, tx, prepared, &live, &parsed.steps)?;
        let staged = prepared.commit.stage_dir.join("WTF");
        let items = candidates(&mut Disk, prepared, displaced).map_err(|e| {
            incomplete(Error::io(
                &staged,
                &e,
                Suggestion::command(["omaddons", "status"]),
            ))
        })?;
        if !items.is_empty() {
            let id = crate::model::BackupId::new();
            let assembly = prepared
                .commit
                .stage_dir
                .join("recovered")
                .join(id.to_string());
            let intent = PreserveIntent {
                id,
                items,
                assembly,
            };
            append_line(s, journal, &JournalLine::Preserve(intent.clone())).map_err(incomplete)?;
            failpoint::hit("engine.preserve.intent").map_err(|e| {
                incomplete(Error::io(
                    journal,
                    &e,
                    Suggestion::command(["omaddons", "status"]),
                ))
            })?;
            intents.push(intent);
        }
    }
    let mut warnings = Vec::new();
    for intent in &intents {
        if !parsed.preserved.contains(&intent.id) {
            super::preserve::run(s, env.paths, journal, prepared, intent).map_err(incomplete)?;
        }
        warnings.push(super::preserve::rolled_back(prepared, intent));
    }
    Ok(warnings)
}

/// Rewrites `journal` without its torn last line ([`parse`]): cut short, or closed off
/// but not JSON. Everything before it is kept byte for byte.
fn drop_torn_tail(s: &mut WriteSession<'_>, journal: &Path) -> Result<()> {
    let bytes = std::fs::read(journal)
        .map_err(|e| Error::io(journal, &e, Suggestion::command(["omaddons", "status"])))?;
    let body = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let keep = body.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let op = Op::WriteFile {
        content: fsx::Content::Bytes(bytes.get(..keep).unwrap_or_default()),
        mode: None,
    };
    mutate(s, journal, op)?;
    Ok(())
}

/// What a `writes_wtf` rollback does with a step it finds in neither its before nor its
/// after state (M3-D24 (c) rule 2). That proves only that the recorded transition is no
/// longer recognizable, never who changed it (the game, a sync tool, a hand or an
/// interrupted recovery), so the live path is never touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unexpected {
    /// A rename into the live path whose source is empty while the live entry is not the
    /// one it put there: our restored copy (of a backup) was replaced, and nothing of ours
    /// is displaced. The walk goes on.
    Replaced,
    /// The step's displaced side, its path outside `WTF/`, holds what it recorded there:
    /// an item to preserve, and the walk goes on.
    Displaced(PreserveItem),
    /// Anything else: recovery stops and keeps everything.
    Unrecognized,
}

/// [`Unexpected`] for `step`, whose live paths are `live` and below it, in `view`.
pub(crate) fn classify(view: &mut dyn LiveView, step: &StepRecord, live: &Path) -> Unexpected {
    let in_live = |path: &Path| path.starts_with(live);
    // The displaced side holds `recorded`; its place under `WTF/` is the live side's.
    let mut found = |side: &Path, recorded: Option<FileId>, live_side: &Path| match recorded {
        Some(identity) if FileId::same(view.id(side), Some(identity)) => {
            let rel = live_side
                .strip_prefix(live)
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            Unexpected::Displaced(PreserveItem {
                path: side.to_path_buf(),
                identity,
                rel,
            })
        }
        _ => Unexpected::Unrecognized,
    };
    match step.op {
        // What rollback would move back into `WTF/` sits where the exchange put it.
        StepOp::Exchange if in_live(&step.to) => found(&step.from, step.to_id, &step.to),
        StepOp::Exchange if in_live(&step.from) => found(&step.to, step.from_id, &step.from),
        StepOp::Noreplace | StepOp::Rename if in_live(&step.to) => {
            if view.id(&step.from).is_none() && !FileId::same(view.id(&step.to), step.from_id) {
                Unexpected::Replaced
            } else {
                Unexpected::Unrecognized
            }
        }
        StepOp::Noreplace | StepOp::Rename if in_live(&step.from) => {
            found(&step.to, step.from_id, &step.from)
        }
        StepOp::Exchange | StepOp::Noreplace | StepOp::Rename => Unexpected::Unrecognized,
    }
}

/// [`roll_back`] for `prepared`'s journal, which moves `WTF/` (`live`), by rule 2: a step
/// in neither state that [`classify`] can place is left as it is and the walk goes on, its
/// displaced side becoming an item to preserve; one it cannot place stops recovery
/// (`unexpected_state`) with everything kept.
fn roll_back_wtf(
    s: &mut WriteSession<'_>,
    tx: &TxId,
    prepared: &Prepared,
    live: &Path,
    steps: &[StepRecord],
) -> Result<Vec<PreserveItem>> {
    // A rename that fails is the walk's own error: the journal stays, as ever. A partial
    // restore's step names a file under `WTF/`, so its error is redacted.
    match walk_back_wtf(&mut Recovering { s }, live, steps).map_err(|e| redacted(prepared, e))? {
        WtfWalked::Done(displaced) => Ok(displaced),
        WtfWalked::Unplaced(step) => {
            let why = super::preserve::redacted(prepared, &neither_message(step));
            Err(stopped(tx, prepared, Stop::UnexpectedState, &why))
        }
    }
}

/// How a `writes_wtf` rollback walk ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WtfWalked<'a> {
    /// Every step passed, leaving these displaced versions to preserve.
    Done(Vec<PreserveItem>),
    /// This step is in neither state and [`classify`] cannot place it: the walk stopped
    /// there, before its point.
    Unplaced(&'a StepRecord),
}

/// [`walk_back`] by rule 2 (it replaces `roll_back_wtf`'s loop): a step in neither state
/// goes to [`classify`], and the walk goes on past one replaced or displaced, collecting
/// the displaced side; one it cannot place stops it.
pub(crate) fn walk_back_wtf<'a>(
    view: &mut dyn Replay,
    live: &Path,
    steps: &'a [StepRecord],
) -> Result<WtfWalked<'a>> {
    let mut displaced = Vec::new();
    for (at, step) in steps.iter().enumerate().rev() {
        match step_state_after(view, step, &steps[..at]) {
            StepState::After => view.reverse(step)?,
            StepState::Before => {}
            StepState::Neither => match classify(view, step, live) {
                Unexpected::Replaced => {}
                Unexpected::Displaced(item) => displaced.push(item),
                Unexpected::Unrecognized => return Ok(WtfWalked::Unplaced(step)),
            },
        }
        view.stepped(step)?;
    }
    Ok(WtfWalked::Done(displaced))
}

/// `error` with every account, realm and character name `prepared`'s client and stage
/// hold replaced, in its message and its details.
fn redacted(prepared: &Prepared, mut error: Error) -> Error {
    error.message = super::preserve::redacted(prepared, &error.message);
    let details = super::preserve::redacted(prepared, &error.details.to_string());
    error.details = serde_json::from_str(&details).unwrap_or_else(|_| serde_json::json!({}));
    error
}

/// Rule 3's candidates, after the walk: the `displaced` versions, and the staged tree at
/// `stage/<tx>.wtf/WTF` when it is not one of them and its stamps changed since it was
/// journaled (the game played on it), or its source is gone (it holds that content's last
/// copy, which a metadata match cannot vouch for).
/// The staged tree's stamp error is what recovery stops on (`preservation_incomplete`).
pub(crate) fn candidates(
    view: &mut dyn LiveView,
    prepared: &Prepared,
    mut items: Vec<PreserveItem>,
) -> std::result::Result<Vec<PreserveItem>, std::io::Error> {
    if let Some(files) = &prepared.staged_files {
        return Ok(staged_file_candidates(view, prepared, files, items));
    }
    let staged = prepared.commit.stage_dir.join("WTF");
    let Some(identity) = view.id(&staged) else {
        return Ok(items);
    };
    // ubs:ignore — comparing paths, not secrets
    if items.iter().any(|item| item.path == staged) {
        return Ok(items);
    }
    let changed = match &prepared.staged_stamp {
        Some(recorded) => {
            let now = view.tree_stamp(&staged)?;
            // ubs:ignore — comparing stat digests of a tree (evidence of change), not secrets
            now != *recorded
        }
        None => true,
    };
    if changed || source_gone(view, prepared) {
        items.push(PreserveItem {
            path: staged,
            identity,
            rel: String::new(),
        });
    }
    Ok(items)
}

/// [`candidates`] for a partial restore, file by file: each staged copy still at its stage
/// path whose stamp changed since it was journaled (the game wrote it while it was live,
/// before rollback brought it back), or every one when its source backup is gone (it holds
/// that content's last copy).
fn staged_file_candidates(
    view: &mut dyn LiveView,
    prepared: &Prepared,
    files: &[StagedFile],
    mut items: Vec<PreserveItem>,
) -> Vec<PreserveItem> {
    let staged = prepared.commit.stage_dir.join("WTF");
    let gone = source_gone(view, prepared);
    for file in files {
        let path = staged.join(&file.rel);
        let Some(identity) = view.id(&path) else {
            continue;
        };
        // ubs:ignore — comparing paths, not secrets
        if items.iter().any(|item| item.path == path) {
            continue;
        }
        // ubs:ignore — comparing stat digests of a file (evidence of change), not secrets
        if gone || view.file_stamp(&path).as_deref() != Some(file.stamp.as_str()) {
            items.push(PreserveItem {
                path,
                identity,
                rel: file.rel.clone(),
            });
        }
    }
    items
}

/// Whether the tree a `writes_wtf` stage was copied from is gone: a restore's backup, by
/// identity, or, for an undo of one, the reverted transaction's `WTF.previous` (a
/// snapshot named by that transaction, never reused).
pub(crate) fn source_gone(view: &mut dyn LiveView, prepared: &Prepared) -> bool {
    let client = ClientPaths::new(&prepared.client_dir);
    match (prepared.source_backup, prepared.commit.history.reverts) {
        (Some(source), _) => {
            let dir = crate::backup::backup_dir(&client, &source.id);
            !FileId::same(view.id(&dir), Some(source.dir))
        }
        (None, Some(reverted)) => !view.is_dir(
            &super::retention::snapshot_of(&client, &reverted).join(super::retention::WTF_PREVIOUS),
        ),
        (None, None) => true,
    }
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
                // The undo's own snapshot, named as its journal is.
                let snapshot = if prepared.puts_settings_back() {
                    client.snapshots_wtf(&tx)
                } else {
                    client.snapshots(&tx)
                };
                super::undo::restore_saved_variables(
                    &mut undo,
                    &client,
                    sv,
                    &snapshot,
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

/// Undoes the steps that ran, last first ([`walk_back`]); the next recovery skips what
/// was already undone. A step in neither its before nor its after state stops it
/// (`details.reason: "step_unrecognized"`, for transaction `tx` of target `key`),
/// leaving the journal for manual repair.
fn roll_back(
    s: &mut WriteSession<'_>,
    steps: &[StepRecord],
    tx: &TxId,
    key: TargetKey,
) -> Result<()> {
    match walk_back(&mut Recovering { s }, steps)? {
        Walked::Done => Ok(()),
        Walked::StoppedAt(step) => Err(Error::internal(neither_message(step)).details(
            serde_json::json!({
                "reason": StopReason::StepUnrecognized.as_str(),
                "journal_pending": tx,
                "target": key,
            }),
        )),
    }
}

/// Where a recorded step's rename is, from the identities now at its paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepState {
    /// It did not run, or was undone.
    Before,
    /// It ran.
    After,
    /// Neither: something else changed its paths.
    Neither,
}

/// The [`StepState`] of `step` in `view` (it replaces `ran`).
pub(crate) fn step_state(view: &mut dyn LiveView, step: &StepRecord) -> StepState {
    let (now_from, now_to) = (view.id(&step.from), view.id(&step.to));
    let before = (step.from_id, step.to_id);
    let after = match step.op {
        StepOp::Exchange => (step.to_id, step.from_id),
        StepOp::Noreplace | StepOp::Rename => (None, step.from_id),
    };
    let is = |(from, to): (Option<FileId>, Option<FileId>)| {
        FileId::same(now_from, from) && FileId::same(now_to, to)
    };
    if is(before) {
        StepState::Before
    } else if is(after) {
        StepState::After
    } else {
        StepState::Neither
    }
}

/// [`step_state`], for a step recorded after `earlier` (it replaces `ran_after`). Steps
/// run first to last and roll back last first, so while one of `earlier` is in its before
/// state this one cannot have run, or was undone already: an exchange's old side moved on
/// from the stage path, and a recovery that died after undoing both put the staged tree
/// back at that path, which this step then sees in neither of its states. Anything else
/// in neither state stays `Neither`.
pub(crate) fn step_state_after(
    view: &mut dyn LiveView,
    step: &StepRecord,
    earlier: &[StepRecord],
) -> StepState {
    match step_state(view, step) {
        StepState::Neither
            if earlier
                .iter()
                .any(|e| matches!(step_state(view, e), StepState::Before)) =>
        {
            StepState::Before
        }
        decided => decided,
    }
}

/// What recovery says of a step in neither state (it replaces `ran`'s error).
pub(crate) fn neither_message(step: &StepRecord) -> String {
    format!(
        "journal step {} ({} → {}) matches neither its before nor its after state; leaving it for manual repair",
        step.n,
        step.from.display(),
        step.to.display()
    )
}

/// How a rollback walk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Walked<'a> {
    /// Every step passed.
    Done,
    /// This step is in neither state: the walk stopped there, before its point.
    StoppedAt(&'a StepRecord),
}

/// The rollback walk (it replaces `roll_back`'s loop): last step first, one that ran is
/// reversed, one that did not is skipped, and [`Replay::stepped`] follows every step it
/// passes; a step in neither state stops it.
pub(crate) fn walk_back<'a>(view: &mut dyn Replay, steps: &'a [StepRecord]) -> Result<Walked<'a>> {
    for (at, step) in steps.iter().enumerate().rev() {
        match step_state_after(view, step, &steps[..at]) {
            StepState::After => view.reverse(step)?,
            StepState::Before => {}
            StepState::Neither => return Ok(Walked::StoppedAt(step)),
        }
        view.stepped(step)?;
    }
    Ok(Walked::Done)
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
            staged_files: None,
            source_backup: None,
            client: None,
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
                    target: Some(key),
                    targets: Vec::new(),
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
                    sv_restored: None,
                    created_addons_dir: true,
                    run: Some(crate::model::RunId::new()),
                    backup: None,
                    plan: None,
                    doctor_run_id: None,
                    repairs: Vec::new(),
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
        assert_eq!(step_state(&mut Disk, &exchange), StepState::Before);
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &a,
            rustix::fs::CWD,
            &b,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .unwrap();
        assert_eq!(step_state(&mut Disk, &exchange), StepState::After);

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
        assert_eq!(step_state(&mut Disk, &rename), StepState::Before);
        std::fs::rename(&a, &c).unwrap();
        assert_eq!(step_state(&mut Disk, &rename), StepState::After);
        std::fs::create_dir(&a).unwrap();
        assert_eq!(
            step_state(&mut Disk, &rename),
            StepState::Neither,
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
        let key = TargetKey::new();

        // A recovery that undid both and died: the staged tree is back at `stage`.
        std::fs::rename(&kept, &stage).unwrap();
        swap(&stage, &live);
        assert_eq!(
            step_state(&mut Disk, &steps[1]),
            StepState::Neither,
            "on its own, the step is in neither state"
        );
        roll_back(&mut s, &steps, &tx, key).unwrap();
        assert!(stage.join("new").exists() && live.join("old").exists());

        // A tree recovery does not know at `live`: the exchange is in neither state.
        swap(&stage, &live);
        std::fs::rename(&stage, &kept).unwrap();
        std::fs::rename(&live, root.join("aside")).unwrap();
        std::fs::create_dir(&live).unwrap();
        let err = roll_back(&mut s, &steps, &tx, key).unwrap_err();
        assert_eq!(err.details["reason"], "step_unrecognized", "{err:?}");
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
        assert_eq!(
            [
                step_state(&mut Disk, &exchange),
                step_state(&mut Disk, &noreplace)
            ],
            [StepState::Before; 2]
        );
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &a,
            rustix::fs::CWD,
            &b,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .unwrap();
        assert_eq!(step_state(&mut Disk, &exchange), StepState::After);
        // `b` now holds what was `a`: the noreplace step recorded the old `b`.
        let moved = StepRecord {
            from_id: renumbered(FileId::of(&b)),
            ..noreplace
        };
        std::fs::rename(&b, &c).unwrap();
        assert_eq!(step_state(&mut Disk, &moved), StepState::After);
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
        assert_eq!(step_state(&mut Disk, step), StepState::Before);
        std::fs::rename(&a, &c).unwrap();
        assert_eq!(step_state(&mut Disk, step), StepState::After);
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
            assert_eq!(
                step_state(&mut Disk, &step),
                StepState::Neither,
                "a different file at `from`"
            );
        } else {
            // A filesystem without birth times decides by inode alone.
            assert_eq!(step_state(&mut Disk, &step), StepState::Before);
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
            staged_files: None,
            source_backup: None,
            client: None,
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
                    target: Some(key),
                    targets: Vec::new(),
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
                    sv_restored: None,
                    created_addons_dir: false,
                    run: None,
                    backup: None,
                    plan: None,
                    doctor_run_id: None,
                    repairs: Vec::new(),
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
                staged_files: None,
                source_backup: None,
                client: None,
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
                        target: Some(key),
                        targets: Vec::new(),
                        actor: Actor::Agent,
                        reason: None,
                        argv: vec!["omaddons".into(), "undo".into()],
                        ops: Vec::new(),
                        result: TxResult::Committed,
                        reverts: Some(from),
                        sv_deferred: false,
                        sv_restored: None,
                        created_addons_dir: false,
                        run: None,
                        backup: None,
                        plan: None,
                        doctor_run_id: None,
                        repairs: Vec::new(),
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
                staged_files: None,
                source_backup: None,
                client: None,
                commit: CommitPayload {
                    record: TargetRecord::empty(key),
                    index: IndexChanges::default(),
                    history: HistoryEntry {
                        schema_version: history::CURRENT,
                        tx,
                        kind: TxKind::Install,
                        at: Timestamp::now(),
                        target: Some(key),
                        targets: Vec::new(),
                        actor: Actor::Agent,
                        reason: None,
                        argv: vec!["omaddons".into()],
                        ops: Vec::new(),
                        result: TxResult::Committed,
                        reverts: None,
                        sv_deferred: false,
                        sv_restored: None,
                        created_addons_dir: false,
                        run: None,
                        backup: None,
                        plan: None,
                        doctor_run_id: None,
                        repairs: Vec::new(),
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
        let clients = [a.clone(), b.clone()];
        let recover = |own: &[TargetKey]| {
            let (result, _) = crate::engine::testing::predict_then_compare(
                "a_journal_is_recovered_in_its_own_client_and_only_there",
                &paths,
                &clients,
                &|_| false,
                own,
                || {
                    recover_all(&RecoveryEnv {
                        lock: &lock,
                        paths: &paths,
                        scopes: &b_scopes,
                        own,
                        sv_writable: &|_| false,
                    })
                },
            );
            result
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
        assert!(
            warnings[0].message.contains("(marker_mismatch)"),
            "{}",
            warnings[0].message
        );
        let err = recover(std::slice::from_ref(&key)).unwrap_err();
        assert_eq!(err.code, ErrorCode::ScopeDenied, "{err:?}");
        assert_eq!(
            err.details,
            serde_json::json!({
                "reason": "marker_mismatch",
                "journal_pending": tx,
                "target": key,
            }),
            "{err:?}"
        );
        assert!(
            err.message.contains("whose marker is not target"),
            "{err:?}"
        );
        // The doctor's explanation of the state; no `-t`: the registry names no target.
        assert_eq!(
            err.fix,
            crate::doctor::registry::explain_fix(
                crate::doctor::registry::ids::JOURNAL_MARKER_MISMATCH,
                None
            )
        );

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
            let state = scene.paths.state_snapshot(&scene.tx);
            snapshot_state(&mut s, &scene.paths, &state, &scene.key).unwrap();
            let snapshot = scene.snapshot();
            let swapped = failpoint::with_spec("engine.swap.before_rename=error@4", || {
                swap(
                    &mut s,
                    &mut journal,
                    &ops,
                    &scene.staged,
                    &scene.client,
                    &snapshot,
                )
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
            let state = scene.paths.state_snapshot(&scene.tx);
            snapshot_state(&mut s, &scene.paths, &state, &scene.key).unwrap();
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
    /// With E11's differential around it.
    fn recover_planted(scene: &crate::engine::testing::Scene, own: bool) -> Result<Vec<Warning>> {
        let own: &[TargetKey] = if own {
            std::slice::from_ref(&scene.key)
        } else {
            &[]
        };
        let clients = [scene.client.client_dir().to_path_buf()];
        let (result, _) = crate::engine::testing::predict_then_compare(
            "recover_planted",
            &scene.paths,
            &clients,
            &|_| false,
            own,
            || {
                let lock = scene.lock();
                recover_all(&RecoveryEnv {
                    lock: &lock,
                    paths: &scene.paths,
                    scopes: &scene.scopes,
                    own,
                    sv_writable: &|_| false,
                })
            },
        );
        result
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
            // The doctor's explanation of the state (M4-D53); no `-t`: the scene's
            // registry names no target.
            let explained = crate::doctor::registry::explain_fix(
                crate::doctor::registry::ids::for_stop_reason(why.stop_reason()),
                None,
            );
            assert_eq!(err.fix, explained, "{name}");
            assert_eq!(left(&scene), [true; 5], "{name}: everything left");
            // Another target's command goes on, with a warning carrying the same fix.
            let warnings = recover_planted(&scene, false).unwrap();
            assert_eq!(warnings.len(), 1, "{name}: {warnings:?}");
            assert_eq!(warnings[0].code, WarningCode::JournalPending, "{name}");
            assert!(
                warnings[0].message.contains(why.reason()),
                "{name}: {warnings:?}"
            );
            assert_eq!(warnings[0].fix.as_ref(), Some(&explained), "{name}");
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
            assert_eq!(read[0].fix.as_ref(), Some(&explained), "{name}: a reader's");
        }
    }

    /// Paths under a fresh directory whose registry, when `registered`, names `key` as
    /// `retail`; `garbled` writes a registry that is not JSON instead.
    fn paths_with_registry(
        key: TargetKey,
        registered: bool,
        garbled: bool,
    ) -> (tempfile::TempDir, Paths) {
        use crate::state::registry::{Origin, Registry, RegistryEntry, save_registry};
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        if garbled {
            std::fs::write(paths.registry_file(), "not json").unwrap();
        } else if registered {
            let registry = Registry {
                targets: vec![RegistryEntry {
                    key,
                    id: "retail".into(),
                    install_root: "/games/wow".into(),
                    client_dir: "/games/wow/_retail_".into(),
                    flavor_dir: Some("_retail_".into()),
                    product: Some("wow".into()),
                    qualifier: "battlenet".into(),
                    origin: Origin::Discovered,
                    registered_at: "2026-10-14T12:00:00Z".parse().unwrap(),
                }],
                default: None,
            };
            let scopes = ScopeTable::new(&paths, &[], None).unwrap();
            let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "registry.json");
            save_registry(&mut s, &paths, &registry, &store::VersionStatus::Current).unwrap();
        }
        (dir, paths)
    }

    const REGISTERED: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";

    /// Each stop points at the doctor's explanation of its own failure mode, `-t` naming
    /// the target by its registry id; `unattributed` is machine-scoped, and the game wait
    /// keeps its prose (no stop fix).
    #[test]
    fn stop_fix_names_each_stops_failure_mode() {
        use crate::doctor::registry::{explain_fix, ids};
        let key: TargetKey = REGISTERED.parse().unwrap();
        let (_dir, paths) = paths_with_registry(key, true, false);
        for reason in StopReason::ALL {
            let expected = match reason {
                StopReason::SavedVariables => None,
                StopReason::Unattributed => Some(explain_fix(ids::JOURNAL_UNATTRIBUTED, None)),
                _ => Some(explain_fix(ids::for_stop_reason(reason), Some("retail"))),
            };
            assert_eq!(
                stop_fix(&paths, reason, Some(key)),
                expected,
                "{}",
                reason.as_str()
            );
        }
    }

    /// Without a registry entry for the target, a stop's fix has no `-t`, never the key.
    #[test]
    fn a_stop_fix_has_no_t_without_a_registry_entry() {
        use crate::doctor::registry::{explain_fix, ids};
        let key: TargetKey = REGISTERED.parse().unwrap();
        let other: TargetKey = "01M7A1B2C3D4E5F6G7H8J9K0MN".parse().unwrap();
        let expected = Some(explain_fix(ids::JOURNAL_TOO_NEW, None));
        for (case, registered, garbled, target) in [
            ("no_registry_file", false, false, Some(key)),
            ("registry_without_the_key", true, false, Some(other)),
            ("registry_not_json", false, true, Some(key)),
            ("no_target", true, false, None),
        ] {
            let (_dir, paths) = paths_with_registry(key, registered, garbled);
            assert_eq!(
                stop_fix(&paths, StopReason::JournalTooNew, target),
                expected,
                "{case}"
            );
        }
    }

    /// `with_stop_fix` replaces the fix of a stop only; an `io` error, an error of another
    /// reason, the game wait and an `unattributed` failure keep theirs.
    #[test]
    fn with_stop_fix_leaves_other_errors_alone() {
        let key: TargetKey = REGISTERED.parse().unwrap();
        let (_dir, paths) = paths_with_registry(key, true, false);
        let status = Suggestion::command(["omaddons", "status"]);
        let with = |code: ErrorCode, details: serde_json::Value| {
            Error::new(code, "x", status.clone()).details(details)
        };
        for (case, error) in [
            (
                "io_without_a_reason",
                with(ErrorCode::Io, serde_json::json!({})),
            ),
            (
                "another_reason",
                with(
                    ErrorCode::StateCorrupt,
                    serde_json::json!({"reason": "snapshot_mismatch"}),
                ),
            ),
            (
                "the_game_wait",
                with(
                    ErrorCode::GameRunning,
                    serde_json::json!({"reason": "saved_variables", "target": key}),
                ),
            ),
            (
                "unattributed",
                with(ErrorCode::Io, serde_json::json!({"reason": "unattributed"})),
            ),
        ] {
            assert_eq!(with_stop_fix(&paths, error).fix, status, "{case}");
        }
        let stop = with(
            ErrorCode::StateTooNew,
            serde_json::json!({"reason": "journal_too_new", "target": key}),
        );
        assert_eq!(
            with_stop_fix(&paths, stop).fix,
            crate::doctor::registry::explain_fix(
                crate::doctor::registry::ids::JOURNAL_TOO_NEW,
                Some("retail")
            ),
            "a stop on a registered target"
        );
    }

    /// An `unattributed` failure points at the journal-unattributed explanation when it
    /// names a journal, and at the state directory's when it names none.
    #[test]
    fn an_unattributed_failure_points_at_its_explanation() {
        use crate::doctor::registry::{explain_fix, ids};
        let io = || {
            Error::new(
                ErrorCode::Io,
                "x",
                Suggestion::command(["omaddons", "status"]),
            )
        };
        let tx: TxId = "01M7A1B2C3D4E5F6G7H8J9K0MN".parse().unwrap();
        let named = unattributed(io(), Some(&tx));
        assert_eq!(named.fix, explain_fix(ids::JOURNAL_UNATTRIBUTED, None));
        assert_eq!(named.details["journal_pending"], tx.to_string());
        let unlisted = unattributed(io(), None);
        assert_eq!(unlisted.fix, explain_fix(ids::STATE_CORRUPT, None));
        assert_eq!(unlisted.details["reason"], "unattributed");
    }

    #[test]
    fn stop_reasons_are_one_vocabulary() {
        let names = StopReason::ALL.map(StopReason::as_str);
        assert_eq!(
            names,
            [
                "journal_too_new",
                "journal_unreadable",
                "marker_mismatch",
                "step_unrecognized",
                "unexpected_state",
                "preservation_incomplete",
                "saved_variables",
                "unattributed",
                "skill_dir",
            ],
            "the strings stay the ones recovery wrote before"
        );
        let unique: std::collections::BTreeSet<&str> = names.into_iter().collect();
        assert_eq!(unique.len(), names.len(), "{names:?}");
        for (reason, name) in StopReason::ALL.into_iter().zip(names) {
            assert!(
                !name.is_empty() && name.bytes().all(|b| matches!(b, b'a'..=b'z' | b'_')),
                "{name}"
            );
            assert_eq!(serde_json::to_value(reason).unwrap(), name, "serde agrees");
        }
        assert_eq!(
            [
                Unrecoverable::TooNew.reason(),
                Unrecoverable::Unreadable.reason(),
                Stop::UnexpectedState.reason(),
                Stop::PreservationIncomplete.reason(),
            ],
            [
                StopReason::JournalTooNew,
                StopReason::JournalUnreadable,
                StopReason::UnexpectedState,
                StopReason::PreservationIncomplete,
            ]
            .map(StopReason::as_str)
        );
    }

    /// `testing::Scene`'s transaction, prepared and stopped after three `noreplace`
    /// steps (Bar in from the stage, then Gone and Foo out to the snapshot), all run, its
    /// writer gone; step 2's recorded identity is then rewritten, so step 2 is in neither
    /// its before nor its after state while step 1 is in its after state.
    fn step_2_unrecognized() -> crate::engine::testing::Scene {
        let mut scene = crate::engine::testing::Scene::new();
        let (addons, kept) = (scene.client.addons(), scene.snapshot().join("AddOns"));
        let bar = scene.staged.stage_dir.join("Bar");
        assert!(bar.is_dir(), "the scene stages Bar");
        std::fs::create_dir_all(&kept).unwrap();
        let steps = [
            (bar, addons.join("Bar"), StepPurpose::SwapIn),
            (
                addons.join("Gone"),
                kept.join("Gone"),
                StepPurpose::OldToSnapshot,
            ),
            (
                addons.join("Foo"),
                kept.join("Foo"),
                StepPurpose::OldToSnapshot,
            ),
        ];
        {
            let lock = scene.lock();
            let mut s = scene.session();
            let mut journal = scene.journal(&mut s, &lock);
            for (from, to, purpose) in steps {
                let step = Step {
                    op: StepOp::Noreplace,
                    from,
                    to,
                    purpose,
                };
                journal.step(&mut s, step.clone()).unwrap();
                let mode = RenameMode::NoReplace;
                mutate(&mut s, &step.from, Op::Rename { to: &step.to, mode }).unwrap();
            }
        }
        let path = scene.paths.journal(&scene.tx);
        let text = std::fs::read_to_string(&path).unwrap();
        let edited: String = text
            .lines()
            .map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                // ubs:ignore — a journal line's type and step number, not secrets
                if value["type"] != "step" || value["n"] != 2 {
                    return format!("{line}\n");
                }
                let ino = value["from_id"]["ino"].as_u64().unwrap();
                let rewritten = line.replacen(
                    &format!("\"ino\":{ino}"),
                    &format!("\"ino\":{}", ino ^ 1),
                    1,
                );
                format!("{rewritten}\n")
            })
            .collect();
        assert_ne!(edited, text, "step 2's identity is rewritten");
        std::fs::write(&path, edited).unwrap();
        scene.release_inflight();
        scene
    }

    #[test]
    fn a_step_in_neither_state_stops_with_step_unrecognized() {
        let scene = step_2_unrecognized();
        let journal = std::fs::read_to_string(scene.paths.journal(&scene.tx)).unwrap();
        let err = recover_planted(&scene, true).unwrap_err();
        let context = format!("{err:?}\n{journal}");
        assert_eq!(err.code, ErrorCode::Internal, "{context}");
        assert_eq!(
            err.details,
            serde_json::json!({
                "reason": "step_unrecognized",
                "journal_pending": scene.tx,
                "target": scene.key,
            }),
            "{context}"
        );
        assert!(
            err.message.contains("journal step 2 (")
                && err
                    .message
                    .contains("matches neither its before nor its after state"),
            "{context}"
        );
        // The doctor's explanation of the state; no `-t`: the registry names no target.
        assert_eq!(
            err.fix,
            crate::doctor::registry::explain_fix(
                crate::doctor::registry::ids::JOURNAL_STEP_UNRECOGNIZED,
                None
            ),
            "{context}"
        );
        let (addons, kept) = (scene.client.addons(), scene.snapshot().join("AddOns"));
        assert!(
            addons.join("Foo").is_dir() && !kept.join("Foo").exists(),
            "step 3 was undone before the stop: {context}"
        );
        assert!(
            kept.join("Gone").is_dir() && addons.join("Bar").is_dir(),
            "steps 2 and 1 were left: {context}"
        );
        assert!(
            scene.paths.journal(&scene.tx).exists(),
            "the journal is kept"
        );
    }

    #[test]
    fn another_targets_stop_warns_with_its_reason() {
        let scene = step_2_unrecognized();
        let warnings = recover_planted(&scene, false).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(
            warnings[0].code,
            WarningCode::JournalPending,
            "{warnings:?}"
        );
        assert!(
            warnings[0].message.contains("(step_unrecognized)")
                && warnings[0].message.contains("journal step 2 ("),
            "{warnings:?}"
        );
        // The stop's own fix, as the error on its own target carries it.
        assert_eq!(
            warnings[0].fix,
            Some(crate::doctor::registry::explain_fix(
                crate::doctor::registry::ids::JOURNAL_STEP_UNRECOGNIZED,
                None
            )),
            "{warnings:?}"
        );
        assert!(
            scene.paths.journal(&scene.tx).exists(),
            "the journal is kept"
        );
    }

    /// `text` with its `prepared` line's target key garbled, so no target reads from it.
    fn without_target(text: &str) -> String {
        let key = "\"target_key\":\"";
        let at = text.find(key).unwrap() + key.len();
        let end = at + text[at..].find('"').unwrap();
        format!("{}garbage{}", &text[..at], &text[end..])
    }

    /// Runs `f` while `path` has mode `0o000`, then gives it its mode back (a tempdir
    /// cannot remove a directory it cannot list).
    fn sealed<R>(path: &Path, f: impl FnOnce() -> R) -> R {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let out = f();
        std::fs::set_permissions(path, mode).unwrap();
        out
    }

    #[test]
    fn a_journal_with_no_readable_target_is_unattributed() {
        type Edit = fn(&str) -> String;
        let rows: [(&str, Edit, ErrorCode, bool); 3] = [
            (
                "newer_major_without_target",
                |t| {
                    without_target(&t.replacen(
                        "\"schema_version\":\"1.1\"",
                        "\"schema_version\":\"9.0\"",
                        1,
                    ))
                },
                ErrorCode::StateTooNew,
                false,
            ),
            (
                "undecodable_prepared_without_target",
                |t| without_target(&t.replacen("\"kind\":\"install\"", "\"kind\":\"teleport\"", 1)),
                ErrorCode::StateCorrupt,
                false,
            ),
            ("unreadable_file", without_target, ErrorCode::Io, true),
        ];
        for (name, edit, code, unreadable) in rows {
            let scene = planted(edit);
            let path = scene.paths.journal(&scene.tx);
            let journal = std::fs::read_to_string(&path).unwrap();
            // With no target of the caller's, and with the scene's in `own`: the same
            // error, since no target owns the journal.
            let errors = [false, true].map(|own| {
                let recover = || recover_planted(&scene, own).unwrap_err();
                if unreadable {
                    sealed(&path, recover)
                } else {
                    recover()
                }
            });
            for err in &errors {
                let context = format!("{name}: {err:?}\n{journal}");
                assert_eq!(err.code, code, "{context}");
                assert_eq!(err.details["reason"], "unattributed", "{context}");
                assert_eq!(
                    err.details["journal_pending"],
                    scene.tx.to_string(),
                    "{context}"
                );
                assert!(
                    err.details
                        .get("target")
                        .is_none_or(serde_json::Value::is_null),
                    "{context}"
                );
            }
            let [a, b] = &errors;
            assert_eq!(
                (a.code, &a.message, &a.details),
                (b.code, &b.message, &b.details),
                "{name}"
            );
            assert_eq!(left(&scene), [true; 5], "{name}: everything left");
        }
    }

    #[test]
    fn an_own_targets_stop_keeps_its_reason() {
        let scene = planted(|t| {
            t.replacen(
                "\"schema_version\":\"1.1\"",
                "\"schema_version\":\"9.0\"",
                1,
            )
        });
        let err = recover_planted(&scene, true).unwrap_err();
        assert_eq!(err.code, ErrorCode::StateTooNew, "{err:?}");
        assert_eq!(err.details["reason"], "journal_too_new", "{err:?}");
        assert_eq!(err.details["target"], scene.key.to_string(), "{err:?}");
    }

    /// A scene whose transaction is prepared and whose writer is gone.
    fn prepared_scene() -> crate::engine::testing::Scene {
        let mut scene = crate::engine::testing::Scene::new();
        {
            let lock = scene.lock();
            let mut s = scene.session();
            scene.journal(&mut s, &lock);
        }
        scene.release_inflight();
        scene
    }

    #[test]
    fn an_unlistable_journal_directory_is_unattributed() {
        let scene = prepared_scene();
        // Recovery, and the readers every command runs first (found by E11's differential:
        // the command failed there, before recovery, with a bare `io`).
        let (recovered, read, warned) = sealed(&scene.paths.journal_dir(), || {
            (
                recover_planted(&scene, true).unwrap_err(),
                pending(&scene.paths).unwrap_err(),
                pending_warnings(&scene.paths).unwrap_err(),
            )
        });
        for err in [&read, &warned] {
            assert_eq!(
                (err.code, &err.details),
                (recovered.code, &recovered.details),
                "{err:?}"
            );
        }
        let err = recovered;
        assert_eq!(err.code, ErrorCode::Io, "{err:?}");
        assert_eq!(err.details["reason"], "unattributed", "{err:?}");
        assert!(err.details.get("journal_pending").is_none(), "{err:?}");
        assert!(
            scene.paths.journal(&scene.tx).exists(),
            "the journal is kept"
        );
    }

    #[test]
    fn an_unprobeable_inflight_lock_is_unattributed() {
        let scene = prepared_scene();
        let lock = scene.paths.inflight_lock(&scene.tx);
        assert!(lock.is_file(), "the gone writer's lock file stays");
        let err = sealed(&lock, || recover_planted(&scene, false).unwrap_err());
        assert_eq!(err.code, ErrorCode::Io, "{err:?}");
        assert_eq!(err.details["reason"], "unattributed", "{err:?}");
        assert_eq!(
            err.details["journal_pending"],
            scene.tx.to_string(),
            "{err:?}"
        );
        assert!(
            scene.paths.journal(&scene.tx).exists(),
            "the journal is kept"
        );
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
                prepared.client = Some(ClientFacts {
                    target_id: "retail".into(),
                    family: crate::rules::Family::Mainline,
                    version: "12.1.5.70391".into(),
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

        /// Recovery by a command of the journal's own target (`own`) or another's, with
        /// the game `running` or not; E11's differential around it.
        fn recover(&self, own: bool, running: bool) -> Result<Vec<Warning>> {
            let own: &[TargetKey] = if own {
                std::slice::from_ref(&self.scene.key)
            } else {
                &[]
            };
            let clients = [self.scene.client.client_dir().to_path_buf()];
            let sv_writable = |_: &Path| !running;
            let (result, _) = crate::engine::testing::predict_then_compare(
                "WtfScene",
                &self.scene.paths,
                &clients,
                &sv_writable,
                own,
                || {
                    let lock = self.scene.lock();
                    recover_all(&RecoveryEnv {
                        lock: &lock,
                        paths: &self.scene.paths,
                        scopes: &self.scene.scopes,
                        own,
                        sv_writable: &sv_writable,
                    })
                },
            );
            result
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
            existing(&self.scene.paths, &self.scene.tx).is_some()
        }
    }

    /// A journal that puts settings back is `<tx>.wtf.jsonl`, a name pre-M3 builds never
    /// list (they deleted such journals unread); every other keeps `<tx>.jsonl`, which
    /// they read and recover. This build lists and finds both.
    #[test]
    fn a_journal_that_puts_settings_back_is_named_for_this_build_only() {
        let plain = crate::engine::testing::Scene::new();
        let prepared = {
            let lock = plain.lock();
            let mut s = plain.session();
            let journal = plain.journal(&mut s, &lock);
            assert_eq!(journal.path(), plain.paths.journal(&plain.tx));
            let text = std::fs::read_to_string(journal.path()).unwrap();
            parse(&text).prepared.unwrap()
        };
        assert!(!prepared.puts_settings_back(), "folder ops only");
        // Settings ops alone (the undo of an `--addon` restore) are enough.
        let mut undo = prepared.clone();
        undo.commit
            .history
            .ops
            .push(crate::state::history::HistoryOp {
                op: crate::state::history::FolderOp::SavedVariables,
                folder: crate::model::FolderName::new("Foo").unwrap(),
                addon: "Foo".into(),
                edit: None,
                backup: None,
                from: Some(crate::state::history::WtfSource::Snapshot),
                files: Some(1),
            });
        assert!(undo.puts_settings_back());
        let w = WtfScene::new(TxKind::Restore, true, false);
        let (paths, tx) = (&w.scene.paths, &w.scene.tx);
        assert!(paths.journal_wtf(tx).is_file() && !paths.journal(tx).exists());
        assert_eq!(existing(paths, tx), Some(paths.journal_wtf(tx)));
        let listed: Vec<TxId> = list(paths).unwrap().into_iter().map(|(tx, _)| tx).collect();
        assert_eq!(listed, [*tx]);
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
        // Without the flag recovery writes as Mutating, which may not move WTF/. E11's
        // known approximation: `diagnose` does not simulate fsx's scope check, so it
        // predicts a rollback, and flags the path below WTF/.
        let w = WtfScene::new(TxKind::Undo, false, true);
        let clients = [w.scene.client.client_dir().to_path_buf()];
        let found = diagnose(&w.scene.paths, &clients, &|_| true);
        let predicted = &found.journals[0];
        assert_eq!(predicted.expected(), Expected::RolledBack, "{predicted:#?}");
        assert_eq!(
            predicted.evidence.approximations,
            [Approximation::WtfPathWithoutFlag]
        );
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

    impl WtfScene {
        /// The `recovered` backups the client holds.
        fn recovered(&self) -> Vec<crate::backup::BackupRef> {
            crate::backup::list(&self.scene.client)
                .0
                .into_iter()
                .filter(|b| {
                    b.meta
                        .as_ref()
                        .is_some_and(|m| matches!(m.kind, crate::backup::BackupKind::Recovered))
                })
                .collect()
        }
    }

    /// M3-D24 (c) rule 3: a played-on stage, or a stage whose source backup is gone, gets
    /// a durable home as one `recovered` backup before cleanup; recovered again, nothing
    /// more happens. Without either, the stage is pruned and nothing is kept.
    #[test]
    fn a_played_on_stage_or_a_lost_source_is_preserved_before_cleanup() {
        use crate::backup::{BackupSource, Consistency, Integrity};

        // The game played on the restored tree before the crash was recovered.
        let w = WtfScene::new(TxKind::Restore, true, true);
        let played = w.scene.client.wtf().join("Account/restored.txt");
        std::fs::write(&played, "played on").unwrap();
        let warnings = w.recover(true, false).unwrap();
        assert!(!w.live_restored(), "the pre-restore tree is live again");
        let kept = w.recovered();
        assert_eq!(kept.len(), 1, "{warnings:?}");
        assert_eq!(
            std::fs::read_to_string(kept[0].wtf().join("Account/restored.txt")).unwrap(),
            "played on"
        );
        let meta = kept[0].meta.as_ref().unwrap();
        assert!(kept[0].restorable);
        assert_eq!(
            (
                meta.integrity,
                meta.consistency,
                meta.partial,
                &meta.target_id
            ),
            (
                Integrity::MetadataOnly,
                Consistency::BestEffort,
                false,
                &"retail".to_owned()
            )
        );
        assert_eq!(
            meta.source,
            Some(BackupSource::Recovery {
                restore_tx: w.scene.tx
            })
        );
        let rolled = warnings
            .iter()
            .find(|w| matches!(w.code, WarningCode::RestoreRolledBack))
            .unwrap();
        assert!(rolled.message.contains(&meta.id.to_string()), "{rolled:?}");
        assert_eq!(
            rolled.fix.as_ref().unwrap().argv().unwrap()[3],
            meta.id.to_string()
        );
        assert!(
            !w.staged().exists() && !w.journal_kept(),
            "cleaned up after"
        );
        assert!(w.recover(true, false).unwrap().is_empty());
        assert_eq!(w.recovered().len(), 1, "preserved once");

        // Unchanged, but its source backup is gone: the stage is its last copy.
        let w = WtfScene::new(TxKind::Restore, true, true);
        std::fs::remove_dir_all(&w.backup).unwrap();
        w.recover(true, false).unwrap();
        let kept = w.recovered();
        assert_eq!(kept.len(), 1);
        assert!(kept[0].wtf().join("Account/restored.txt").exists());
        assert!(!w.staged().exists() && !w.journal_kept());

        // Neither: nothing to keep, and the stage is pruned.
        let w = WtfScene::new(TxKind::Restore, true, true);
        w.recover(true, false).unwrap();
        assert!(w.recovered().is_empty());
        assert!(!w.staged().exists() && !w.journal_kept());
    }

    /// M3-D24 (c) rule 2: another `WTF/` in the live path is never touched, and the
    /// pre-restore tree the exchange displaced, found where the journal recorded it, is
    /// preserved. With that side gone too, nothing can be placed: recovery stops and
    /// keeps everything.
    #[test]
    fn another_wtf_live_is_left_and_the_displaced_tree_preserved() {
        use crate::engine::testing::tree_snapshot;

        let w = WtfScene::new(TxKind::Restore, true, true);
        let before = tree_snapshot(&w.staged());
        // Someone moved the restored tree away and put another WTF/ in its place.
        let live = w.scene.client.wtf();
        let aside = w.scene.client.client_dir().join("WTF.aside");
        std::fs::rename(&live, &aside).unwrap();
        std::fs::create_dir_all(live.join("Account")).unwrap();
        std::fs::write(live.join("Account/other.txt"), "someone else's").unwrap();
        w.recover(true, false).unwrap();
        assert!(
            live.join("Account/other.txt").exists(),
            "the live path untouched"
        );
        assert!(aside.join("Account/restored.txt").exists());
        let kept = w.recovered();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            tree_snapshot(&kept[0].wtf()),
            before,
            "the pre-restore tree"
        );
        assert!(!w.journal_kept());

        let w = WtfScene::new(TxKind::Restore, true, true);
        let live = w.scene.client.wtf();
        let aside = w.scene.client.client_dir().join("WTF.aside");
        std::fs::rename(&live, &aside).unwrap();
        std::fs::create_dir_all(&live).unwrap();
        let elsewhere = w.scene.client.client_dir().join("moved");
        std::fs::rename(w.staged(), &elsewhere).unwrap();
        let err = w.recover(true, false).unwrap_err();
        assert_eq!(err.details["reason"], "unexpected_state", "{err:?}");
        assert!(
            live.read_dir().unwrap().next().is_none(),
            "the live path untouched"
        );
        assert!(elsewhere.exists() && aside.exists() && w.journal_kept());
        assert!(w.recovered().is_empty());
    }

    /// This build reads its own intents back: a journal with `preserve` and `preserved`
    /// lines parses with nothing unknown, so it is resumed, never `journal_unreadable`.
    #[test]
    fn preservation_lines_read_back_and_need_a_prepared_line() {
        let w = WtfScene::new(TxKind::Restore, true, false);
        let journal = existing(&w.scene.paths, &w.scene.tx).unwrap();
        let id = crate::model::BackupId::new();
        let intent = PreserveIntent {
            id,
            items: vec![PreserveItem {
                path: w.staged(),
                identity: FileId::of(&w.staged()).unwrap(),
                rel: String::new(),
            }],
            assembly: w
                .scene
                .staged
                .stage_dir
                .join("recovered")
                .join(id.to_string()),
        };
        let lines = [
            JournalLine::Preserve(intent.clone()),
            JournalLine::Preserved(Preserved { id }),
        ]
        .map(|line| format!("{}\n", serde_json::to_string(&line).unwrap()))
        .concat();
        let text = std::fs::read_to_string(&journal).unwrap();
        let parsed = parse(&format!("{text}{lines}"));
        assert_eq!(parsed.unknown_lines, 0);
        assert!(parsed.unrecoverable().is_none());
        assert_eq!(
            (parsed.preserve, parsed.preserved),
            (vec![intent], vec![id])
        );
        assert_eq!(parse(&lines).unknown_lines, 2, "before `prepared`, unknown");
    }

    /// Rule 2's table: what a step in neither state leaves to preserve, by what its
    /// displaced side (the path outside `WTF/`) holds.
    #[test]
    fn a_step_in_neither_state_is_classified_by_its_displaced_side() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (live, stage, kept) = (root.join("WTF"), root.join("stage"), root.join("kept"));
        let fresh = |path: &Path| {
            let _ = std::fs::remove_dir_all(path);
            std::fs::create_dir_all(path).unwrap();
            FileId::of(path)
        };
        let step = |op, from: &Path, to: &Path, from_id, to_id| StepRecord {
            n: 1,
            op,
            from: from.to_path_buf(),
            to: to.to_path_buf(),
            purpose: StepPurpose::SwapIn,
            from_id,
            to_id,
        };
        let swap = |a: &Path, b: &Path| {
            let flags = rustix::fs::RenameFlags::EXCHANGE;
            rustix::fs::renameat_with(rustix::fs::CWD, a, rustix::fs::CWD, b, flags).unwrap();
        };

        // An exchange, then something else live: the old tree sits at the stage path.
        let (staged, old) = (fresh(&stage), fresh(&live));
        swap(&stage, &live);
        let exchange = step(StepOp::Exchange, &stage, &live, staged, old);
        fresh(&live);
        assert_eq!(
            classify(&mut Disk, &exchange, &live),
            Unexpected::Displaced(PreserveItem {
                path: stage.clone(),
                identity: old.unwrap(),
                rel: String::new(),
            })
        );
        fresh(&stage);
        assert_eq!(
            classify(&mut Disk, &exchange, &live),
            Unexpected::Unrecognized
        );

        // Out of the live path, then something else live: the destination holds it.
        let gone = fresh(&live);
        std::fs::rename(&live, &kept).unwrap();
        let out = step(StepOp::Noreplace, &live, &kept, gone, None);
        fresh(&live);
        assert_eq!(
            classify(&mut Disk, &out, &live),
            Unexpected::Displaced(PreserveItem {
                path: kept.clone(),
                identity: gone.unwrap(),
                rel: String::new(),
            })
        );

        // Into the live path, its source empty and the live entry another: replaced.
        let _ = std::fs::remove_dir_all(&stage);
        let into = step(StepOp::Noreplace, &stage, &live, Some(gone.unwrap()), None);
        assert_eq!(classify(&mut Disk, &into, &live), Unexpected::Replaced);
        fresh(&stage);
        assert_eq!(classify(&mut Disk, &into, &live), Unexpected::Unrecognized);

        // A file restored where none was (a partial restore's step), then rewritten in
        // place by the game: a new inode, the source empty.
        let file = live.join("Account/X.lua");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "restored").unwrap();
        let restored = FileId::of(&file);
        std::fs::remove_file(&file).unwrap();
        std::fs::write(&file, "the game's").unwrap();
        let file_step = step(
            StepOp::Noreplace,
            &root.join("staged.lua"),
            &file,
            restored,
            None,
        );
        assert_eq!(classify(&mut Disk, &file_step, &live), Unexpected::Replaced);
    }

    /// A recovery that stopped inside the protocol leaves its intent; the next one skips
    /// the reverse walk and finishes it with the same backup id.
    #[cfg(debug_assertions)]
    #[test]
    fn a_preservation_that_stopped_is_resumed_with_the_same_id() {
        for point in [
            "engine.preserve.intent",
            "engine.preserve.moved",
            "engine.preserve.published",
        ] {
            let w = WtfScene::new(TxKind::Restore, true, true);
            let played = w.scene.client.wtf().join("Account/restored.txt");
            std::fs::write(&played, "played on").unwrap();
            let err = failpoint::with_spec(&format!("{point}=error"), || w.recover(true, false))
                .unwrap_err();
            assert_eq!(err.details["reason"], "preservation_incomplete", "{point}");
            let parsed = read(&existing(&w.scene.paths, &w.scene.tx).unwrap()).unwrap();
            assert_eq!(parsed.preserve.len(), 1, "{point}");
            let id = parsed.preserve[0].id;
            let published = matches!(point, "engine.preserve.published");
            assert_eq!(
                w.recovered().len(),
                usize::from(published),
                "{point}: no half-made backup"
            );
            assert!(!w.live_restored(), "{point}: the walk is done");

            w.recover(true, false).unwrap();
            let kept = w.recovered();
            assert_eq!(kept.len(), 1, "{point}");
            assert_eq!(kept[0].id, id, "{point}: the same id");
            assert_eq!(
                std::fs::read_to_string(kept[0].wtf().join("Account/restored.txt")).unwrap(),
                "played on"
            );
            assert!(!w.staged().exists() && !w.journal_kept(), "{point}");
        }
    }

    /// A journal that ends in a torn line (a step cut short, which never ran) gets its
    /// preservation lines on lines of their own: the fragment is dropped first, so the
    /// intent reads back, a stopped preservation resumes with the same id, and no line is
    /// ever unreadable.
    #[cfg(debug_assertions)]
    #[test]
    fn preservation_after_a_torn_line_resumes_and_leaves_no_unreadable_line() {
        let w = WtfScene::new(TxKind::Restore, true, true);
        let played = w.scene.client.wtf().join("Account/restored.txt");
        std::fs::write(&played, "played on").unwrap();
        let journal = existing(&w.scene.paths, &w.scene.tx).unwrap();
        fsx::sys::append_durable(&journal, br#"{"type":"step","n":2,"op""#).unwrap();
        assert!(read(&journal).unwrap().torn_tail);
        let err = failpoint::with_spec("engine.preserve.intent=error", || w.recover(true, false))
            .unwrap_err();
        assert_eq!(err.details["reason"], "preservation_incomplete");
        let parsed = read(&journal).unwrap();
        assert_eq!((parsed.unknown_lines, parsed.torn_tail), (0, false));
        assert_eq!(parsed.preserve.len(), 1, "the intent reads back");
        let id = parsed.preserve[0].id;
        w.recover(true, false).unwrap();
        let kept = w.recovered();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, id, "the same id");
        assert!(!w.journal_kept());
    }

    /// A tree past the backup walk's bounds is never described partially: preservation
    /// stops with it kept, and the journal stays.
    #[test]
    fn a_tree_too_deep_to_describe_stops_preservation_and_keeps_it() {
        let w = WtfScene::new(TxKind::Restore, true, true);
        let mut deep = w.scene.client.wtf();
        for n in 0..=crate::limits::BACKUP_TREE_MAX_DEPTH {
            deep = deep.join(format!("d{n}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        // E11's known approximation: `diagnose` does not simulate the describe walk's
        // bounds, so it predicts a rollback, and flags the preservation it would run.
        let clients = [w.scene.client.client_dir().to_path_buf()];
        for _ in 0..2 {
            let found = diagnose(&w.scene.paths, &clients, &|_| true);
            let predicted = &found.journals[0];
            assert_eq!(predicted.expected(), Expected::RolledBack, "{predicted:#?}");
            assert!(
                predicted
                    .evidence
                    .approximations
                    .contains(&Approximation::PreservationWalk),
                "{predicted:#?}"
            );
            let err = w.recover(true, false).unwrap_err();
            assert_eq!(err.details["reason"], "preservation_incomplete", "{err:?}");
        }
        let parsed = read(&existing(&w.scene.paths, &w.scene.tx).unwrap()).unwrap();
        let assembled = parsed.preserve[0].assembly.join("WTF");
        assert!(assembled.join("d0").exists(), "kept, in the assembly");
        assert!(w.recovered().is_empty());
    }

    // -----------------------------------------------------------------------------------
    // The live view (E11): recovery's decisions, each read through `LiveView`.

    #[test]
    fn the_disk_view_reads_what_recovery_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tree = root.join("tree");
        std::fs::create_dir_all(tree.join("sub")).unwrap();
        std::fs::write(tree.join("sub/x.lua"), "x").unwrap();
        std::fs::write(root.join("file"), "a file").unwrap();
        std::os::unix::fs::symlink(&tree, root.join("link")).unwrap();
        std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).unwrap();
        let cases = [
            ("regular_file", root.join("file"), Some(LiveKind::File)),
            ("directory", tree.clone(), Some(LiveKind::Dir)),
            (
                "symlink_not_followed",
                root.join("link"),
                Some(LiveKind::Symlink),
            ),
            ("absent", root.join("absent"), None),
            (
                "dangling_symlink",
                root.join("dangling"),
                Some(LiveKind::Symlink),
            ),
        ];
        for (name, path, kind) in cases {
            let entry = Disk.entry(&path);
            assert_eq!(entry.map(|e| e.kind), kind, "{name}: {entry:?}");
            assert!(
                FileId::same(entry.map(|e| e.id), FileId::of(&path)),
                "{name}: {entry:?} against {:?}",
                FileId::of(&path)
            );
            assert_eq!(Disk.id(&path), entry.map(|e| e.id), "{name}");
            assert_eq!(Disk.file_stamp(&path), file_stamp(&path), "{name}");
            assert_eq!(Disk.is_dir(&path), path.is_dir(), "{name}: as Path::is_dir");
        }
        assert_eq!(
            Disk.tree_stamp(&tree).unwrap(),
            fsx::StampDigest::compute(&tree).unwrap().to_string()
        );
        let (view, direct) = sealed(&tree.join("sub"), || {
            (
                Disk.tree_stamp(&tree),
                fsx::StampDigest::compute(&tree).map(|d| d.to_string()),
            )
        });
        let kinds = (
            view.as_ref().map_err(std::io::Error::kind),
            direct.as_ref().map_err(std::io::Error::kind),
        );
        assert!(kinds.0.is_err(), "a folder it cannot list: {view:?}");
        assert_eq!(kinds.0, kinds.1, "the view fails as the walk does");
    }

    /// A label for a [`Route`], for the table below.
    fn route_label(route: Route<'_>) -> String {
        match route {
            Route::Unrecoverable(why) => format!("unrecoverable({})", why.reason()),
            Route::MarkerMismatch(_) => "marker_mismatch".into(),
            Route::WaitsForGame(_) => "waits_for_game".into(),
            Route::Prune => "prune".into(),
            Route::Forward(_) => "forward".into(),
            Route::BackPreserving(_) => "back_preserving".into(),
            Route::Back(_) => "back".into(),
        }
    }

    #[test]
    fn route_follows_recoverys_order() {
        let scene = crate::engine::testing::Scene::new();
        let prepared = {
            let lock = scene.lock();
            let mut s = scene.session();
            let journal = scene.journal(&mut s, &lock);
            parse(&std::fs::read_to_string(journal.path()).unwrap())
                .prepared
                .unwrap()
        };
        let prepared_line = |writes_wtf: bool, newer: bool| {
            let mut prepared = prepared.clone();
            prepared.writes_wtf = writes_wtf;
            let line = serde_json::to_string(&JournalLine::Prepared(Box::new(prepared))).unwrap();
            let line = if newer {
                line.replacen(
                    "\"schema_version\":\"1.1\"",
                    "\"schema_version\":\"9.0\"",
                    1,
                )
            } else {
                line
            };
            format!("{line}\n")
        };
        let (step, swapped, committed) = (
            step_line(1),
            "{\"type\":\"swapped\"}\n",
            "{\"type\":\"committed\"}\n",
        );
        // (case, the journal, whether the scopes own its client dir, whether its
        // SavedVariables are writable, the route, how often `sv_writable` is asked)
        let cases: [(&str, String, bool, bool, &str, usize); 9] = [
            (
                "unrecoverable_before_marker",
                format!("{}{step}", prepared_line(false, true)),
                false,
                true,
                "unrecoverable(journal_too_new)",
                0,
            ),
            (
                "marker_before_wait",
                format!("{}{step}", prepared_line(true, false)),
                false,
                false,
                "marker_mismatch",
                0,
            ),
            (
                "committed_ignores_the_marker",
                format!("{}{step}{swapped}{committed}", prepared_line(false, false)),
                false,
                true,
                "prune",
                0,
            ),
            (
                "wait_needs_a_step",
                prepared_line(true, false),
                true,
                false,
                "back_preserving",
                0,
            ),
            (
                "wait_needs_unwritable",
                format!("{}{step}", prepared_line(true, false)),
                true,
                true,
                "back_preserving",
                1,
            ),
            (
                "swapped_wtf_waits",
                format!("{}{step}{swapped}", prepared_line(true, false)),
                true,
                false,
                "waits_for_game",
                1,
            ),
            ("unprepared_prunes", String::new(), false, true, "prune", 0),
            (
                "swapped_forwards",
                format!("{}{step}{swapped}", prepared_line(false, false)),
                true,
                true,
                "forward",
                0,
            ),
            (
                "prepared_backs",
                format!("{}{step}", prepared_line(false, false)),
                true,
                true,
                "back",
                0,
            ),
        ];
        for (name, text, owns, writable, expected, expected_asked) in cases {
            let parsed = parse(&text);
            let asked = std::cell::Cell::new(0);
            let sv_writable = |_: &Path| {
                asked.set(asked.get() + 1);
                writable
            };
            let route = route_label(route(&parsed, owns, &sv_writable));
            assert_eq!(
                (route.as_str(), asked.get()),
                (expected, expected_asked),
                "{name}: the route and how often the game was asked about\n{text}"
            );
        }
    }

    /// A [`Replay`] over the disk that logs its effects: `reverse` renames back as
    /// [`undo`] does (or fails at step `fails`), and `stepped` only logs.
    struct Recording {
        calls: Vec<String>,
        fails: Option<u32>,
    }

    impl Recording {
        fn new(fails: Option<u32>) -> Self {
            Self {
                calls: Vec::new(),
                fails,
            }
        }
    }

    impl LiveView for Recording {
        fn entry(&mut self, path: &Path) -> Option<LiveEntry> {
            Disk.entry(path)
        }

        fn file_stamp(&mut self, path: &Path) -> Option<String> {
            Disk.file_stamp(path)
        }

        fn tree_stamp(&mut self, path: &Path) -> std::io::Result<String> {
            Disk.tree_stamp(path)
        }
    }

    impl Replay for Recording {
        fn reverse(&mut self, step: &StepRecord) -> Result<()> {
            self.calls.push(format!("reverse {}", step.n));
            // ubs:ignore — comparing step numbers, not secrets
            if self.fails == Some(step.n) {
                return Err(Error::internal(format!("reversing step {} failed", step.n)));
            }
            reverse_on_disk(step);
            Ok(())
        }

        fn stepped(&mut self, step: &StepRecord) -> Result<()> {
            self.calls.push(format!("stepped {}", step.n));
            Ok(())
        }
    }

    /// What [`undo`] does to `step`, with plain renames.
    fn reverse_on_disk(step: &StepRecord) {
        match step.op {
            StepOp::Exchange => rustix::fs::renameat_with(
                rustix::fs::CWD,
                &step.from,
                rustix::fs::CWD,
                &step.to,
                rustix::fs::RenameFlags::EXCHANGE,
            )
            .unwrap(),
            StepOp::Noreplace | StepOp::Rename => std::fs::rename(&step.to, &step.from).unwrap(),
        }
    }

    /// Three steps, all run in `root`, each recorded with the identities before its
    /// rename as [`Journal::step`] records them: step 1 exchanges a staged tree with the
    /// live one, step 2 moves the old tree on from the stage to a snapshot, step 3 moves
    /// another tree aside.
    fn three_steps_run(root: &Path) -> [StepRecord; 3] {
        let [stage, live, kept, other, aside] =
            ["stage", "live", "kept", "other", "aside"].map(|name| root.join(name));
        for dir in [&stage, &live, &other] {
            std::fs::create_dir(dir).unwrap();
        }
        let record = |n: u32, op: StepOp, from: &Path, to: &Path, purpose: StepPurpose| {
            let record = StepRecord {
                n,
                op,
                from_id: FileId::of(from),
                to_id: FileId::of(to),
                from: from.to_path_buf(),
                to: to.to_path_buf(),
                purpose,
            };
            match op {
                StepOp::Exchange => rustix::fs::renameat_with(
                    rustix::fs::CWD,
                    from,
                    rustix::fs::CWD,
                    to,
                    rustix::fs::RenameFlags::EXCHANGE,
                )
                .unwrap(),
                StepOp::Noreplace | StepOp::Rename => std::fs::rename(from, to).unwrap(),
            }
            record
        };
        [
            record(1, StepOp::Exchange, &stage, &live, StepPurpose::SwapIn),
            record(
                2,
                StepOp::Noreplace,
                &stage,
                &kept,
                StepPurpose::OldToSnapshot,
            ),
            record(
                3,
                StepOp::Noreplace,
                &other,
                &aside,
                StepPurpose::RemoveToSnapshot,
            ),
        ]
    }

    #[test]
    fn walk_back_hits_the_failpoint_after_every_passed_step() {
        type Setup = fn(&[StepRecord; 3], &Path);
        type Case = (
            &'static str,
            Setup,
            Option<u32>,
            &'static [&'static str],
            &'static str,
        );
        let reverse_from = |from: usize| -> Setup {
            match from {
                0 => |s, _| s.iter().rev().for_each(reverse_on_disk),
                1 => |s, _| s[1..].iter().rev().for_each(reverse_on_disk),
                2 => |s, _| reverse_on_disk(&s[2]),
                _ => |_, _| {},
            }
        };
        // (case, the disk after all three ran, the step `reverse` fails at, the calls,
        // how the walk ends)
        let cases: [Case; 7] = [
            (
                "none_ran",
                reverse_from(0),
                None,
                &["stepped 3", "stepped 2", "stepped 1"],
                "done",
            ),
            (
                "step_1_ran",
                reverse_from(1),
                None,
                &["stepped 3", "stepped 2", "reverse 1", "stepped 1"],
                "done",
            ),
            (
                "steps_1_and_2_ran",
                reverse_from(2),
                None,
                &[
                    "stepped 3",
                    "reverse 2",
                    "stepped 2",
                    "reverse 1",
                    "stepped 1",
                ],
                "done",
            ),
            (
                "all_ran",
                reverse_from(3),
                None,
                &[
                    "reverse 3",
                    "stepped 3",
                    "reverse 2",
                    "stepped 2",
                    "reverse 1",
                    "stepped 1",
                ],
                "done",
            ),
            (
                "step_2_neither",
                |s, root| {
                    std::fs::rename(&s[1].to, root.join("elsewhere")).unwrap();
                    std::fs::create_dir(&s[1].to).unwrap();
                },
                None,
                &["reverse 3", "stepped 3"],
                "stopped at 2",
            ),
            (
                // A rollback that reversed steps 3 and 2 and died: resumed, it goes on
                // as if they had never run.
                "half_done_rollback",
                |s, _| {
                    reverse_on_disk(&s[2]);
                    reverse_on_disk(&s[1]);
                },
                None,
                &["stepped 3", "stepped 2", "reverse 1", "stepped 1"],
                "done",
            ),
            (
                "a_reversal_fails",
                reverse_from(3),
                Some(2),
                &["reverse 3", "stepped 3", "reverse 2"],
                "error",
            ),
        ];
        for (name, setup, fails, calls, ends) in cases {
            let dir = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();
            let steps = three_steps_run(&root);
            setup(&steps, &root);
            let mut view = Recording::new(fails);
            let walked = match walk_back(&mut view, &steps) {
                Ok(Walked::Done) => "done".to_owned(),
                Ok(Walked::StoppedAt(step)) => format!("stopped at {}", step.n),
                Err(_) => "error".to_owned(),
            };
            assert_eq!(view.calls, calls, "{name}: steps {steps:#?}");
            assert_eq!(walked, ends, "{name}: calls {:?}", view.calls);
        }
    }

    #[test]
    fn the_wtf_walk_classifies_as_before() {
        let steps_of = |w: &WtfScene| {
            read(&existing(&w.scene.paths, &w.scene.tx).unwrap())
                .unwrap()
                .steps
        };

        // Untouched: the exchange ran, and is reversed.
        let w = WtfScene::new(TxKind::Restore, true, true);
        let (live, steps) = (w.scene.client.wtf(), steps_of(&w));
        let mut view = Recording::new(None);
        let walked = walk_back_wtf(&mut view, &live, &steps).unwrap();
        assert_eq!(view.calls, ["reverse 1", "stepped 1"], "reversed");
        assert_eq!(walked, WtfWalked::Done(Vec::new()), "reversed");

        // Another WTF/ live: the displaced pre-restore tree is collected, and the walk
        // goes on.
        let w = WtfScene::new(TxKind::Restore, true, true);
        let (live, steps) = (w.scene.client.wtf(), steps_of(&w));
        std::fs::rename(&live, w.scene.client.client_dir().join("WTF.aside")).unwrap();
        std::fs::create_dir_all(&live).unwrap();
        let mut view = Recording::new(None);
        let walked = walk_back_wtf(&mut view, &live, &steps).unwrap();
        assert_eq!(view.calls, ["stepped 1"], "displaced");
        let item = PreserveItem {
            path: w.staged(),
            identity: steps[0].to_id.unwrap(),
            rel: String::new(),
        };
        assert_eq!(walked, WtfWalked::Done(vec![item]), "displaced");

        // With that side gone too, nothing can be placed: the walk stops there, and
        // passes no point.
        std::fs::rename(w.staged(), w.scene.client.client_dir().join("moved")).unwrap();
        let mut view = Recording::new(None);
        let walked = walk_back_wtf(&mut view, &live, &steps).unwrap();
        assert!(view.calls.is_empty(), "unplaced: {:?}", view.calls);
        assert_eq!(walked, WtfWalked::Unplaced(&steps[0]), "unplaced");

        // A rename into the live path whose source is empty while another entry is live:
        // replaced, and the walk goes on.
        let w = WtfScene::new(TxKind::Restore, true, false);
        let live = w.scene.client.wtf();
        let into = [StepRecord {
            n: 1,
            op: StepOp::Noreplace,
            from: w.scene.staged.stage_dir.join("gone"),
            to: live.clone(),
            purpose: StepPurpose::SwapIn,
            from_id: FileId::of(&w.staged()),
            to_id: None,
        }];
        let mut view = Recording::new(None);
        let walked = walk_back_wtf(&mut view, &live, &into).unwrap();
        assert_eq!(view.calls, ["stepped 1"], "replaced");
        assert_eq!(walked, WtfWalked::Done(Vec::new()), "replaced");
    }

    /// The census: every direct read in the production code of `journal.rs` and
    /// `preserve.rs` is in a place that decides no recovery outcome, so
    /// `journal::diagnose` simulates every decision through the view.
    #[test]
    fn recovery_reads_only_through_the_live_view() {
        const READS: [&str; 4] = [
            "FileId::of(",
            "file_stamp(",
            "StampDigest::compute(",
            "symlink_metadata(",
        ];
        // (file, the item it sits in, the method of that item or "", the reads allowed).
        // `FileId::of`'s own read lives in fsx (`fsx/identity.rs`); its callers here are
        // what this census confines.
        const ALLOWED: [(&str, &str, &str, &[&str]); 5] = [
            ("journal.rs", "impl LiveView for Disk", "", &READS),
            (
                "journal.rs",
                "pub fn file_stamp(",
                "",
                &["symlink_metadata("],
            ),
            (
                "journal.rs",
                "impl Journal",
                "pub fn step(",
                &["FileId::of("],
            ),
            (
                "journal.rs",
                "pub(crate) fn existing(",
                "",
                &["symlink_metadata("],
            ),
            (
                "preserve.rs",
                "fn move_items(",
                "",
                &["symlink_metadata(", "StampDigest::compute("],
            ),
        ];
        let sources = [
            ("journal.rs", include_str!("journal.rs")),
            ("preserve.rs", include_str!("preserve.rs")),
        ];
        let mut seen = [0usize; ALLOWED.len()];
        let mut outside = Vec::new();
        for (file, source) in sources {
            let lines: Vec<&str> = source.lines().collect();
            // Production lines: up to the test module.
            let end = lines
                .windows(2)
                .position(|w| w[0] == "#[cfg(test)]" && w[1].starts_with("mod tests"))
                .unwrap_or(lines.len());
            let (mut item, mut method) = ("", "");
            for (at, line) in lines[..end].iter().enumerate() {
                let code = line.trim_start();
                let indent = line.len() - code.len();
                if code.is_empty() || code.starts_with("//") {
                    continue;
                }
                if indent == 0 && !code.starts_with(['#', '}', ')', ']']) {
                    (item, method) = (code, "");
                }
                if indent == 4 {
                    let bare = ["pub ", "pub(crate) ", "pub(super) "]
                        .iter()
                        .find_map(|vis| code.strip_prefix(vis))
                        .unwrap_or(code);
                    if bare.starts_with("fn ") {
                        method = code;
                    }
                }
                for read in READS {
                    // A view's method (`view.file_stamp(`) or a definition is not a read.
                    let direct = line.match_indices(read).any(|(i, _)| {
                        let before = &line[..i];
                        !before.ends_with('.') && !before.ends_with("fn ")
                    });
                    if !direct {
                        continue;
                    }
                    let allowed = ALLOWED.iter().position(|(f, it, m, reads)| {
                        // ubs:ignore — comparing file names, not secrets
                        *f == file
                            && item.starts_with(it)
                            && method.starts_with(m)
                            && reads.contains(&read)
                    });
                    match allowed {
                        Some(i) => seen[i] += 1,
                        None => outside.push(format!(
                            "src/engine/{file}:{} reads the disk outside `LiveView`: a recovery decision that `journal::diagnose` cannot simulate; read through the view\n    {}",
                            at + 1,
                            line.trim()
                        )),
                    }
                }
            }
        }
        assert!(outside.is_empty(), "{}", outside.join("\n"));
        for (place, count) in ALLOWED.iter().zip(seen) {
            assert!(
                count > 0,
                "the census found no read in {place:?}: it no longer sees the code"
            );
        }
    }

    // -----------------------------------------------------------------------------------
    // Recovery restricted to disclosed transactions (M4-D37: the doctor's delegation).

    /// An empty journal of a fresh transaction in `paths`: no complete line, so recovery
    /// only removes it (`Pruned`); its writer is gone.
    fn unprepared(paths: &Paths) -> TxId {
        let tx = TxId::new();
        plant_text(paths, tx, "");
        tx
    }

    /// A `prepared` line with no target that does not decode: it fails every recovery
    /// that reaches it (`unattributed`).
    const UNATTRIBUTED: &str = "{\"type\":\"prepared\",\"schema_version\":\"1.1\"}\n";

    /// Journal `tx` of `paths`, holding `text`.
    fn plant_text(paths: &Paths, tx: TxId, text: &str) {
        std::fs::create_dir_all(paths.journal_dir()).unwrap();
        std::fs::write(paths.journal(&tx), text).unwrap();
    }

    fn disclosed(paths: &Paths, only: Option<&[TxId]>) -> Disclosed {
        let lock = GlobalLock::acquire(paths, "test").unwrap();
        let only: Option<BTreeSet<TxId>> = only.map(|txs| txs.iter().copied().collect());
        recover_disclosed(
            &lock,
            paths,
            &|_| false,
            "doctor fix",
            Actor::Human,
            only.as_ref(),
        )
    }

    /// Two interrupted transactions, recovery restricted to one: the other's journal
    /// bytes, stage and snapshots are left as they were, and only the one given has an
    /// outcome; then the other, alone.
    #[cfg(debug_assertions)]
    #[test]
    fn recovery_restricted_to_one_transaction_leaves_the_other() {
        use crate::engine::testing::tree_hash;
        let scene = interrupted_swap();
        let other = unprepared(&scene.paths);
        let journal = std::fs::read(scene.paths.journal(&scene.tx)).unwrap();
        let stage = tree_hash(&scene.staged.stage_dir);
        let snapshot = tree_hash(&scene.client.snapshots(&scene.tx));
        let got = disclosed(&scene.paths, Some(&[other]));
        assert_eq!(
            got.outcomes,
            [TxOutcome {
                tx: other,
                family: JournalFamily::Folder,
                result: TxResult::Pruned,
                warnings: Vec::new(),
            }],
            "{got:#?}"
        );
        assert!(got.error.is_none() && got.warnings.is_empty(), "{got:#?}");
        assert!(
            !scene.paths.journal(&other).exists(),
            "the one given is settled"
        );
        assert_eq!(
            std::fs::read(scene.paths.journal(&scene.tx)).unwrap(),
            journal,
            "the other's journal, byte for byte"
        );
        assert_eq!(tree_hash(&scene.staged.stage_dir), stage, "its stage");
        assert_eq!(
            tree_hash(&scene.client.snapshots(&scene.tx)),
            snapshot,
            "its snapshot"
        );
        let got = disclosed(&scene.paths, Some(&[scene.tx]));
        let [outcome] = &got.outcomes[..] else {
            panic!("one outcome: {got:#?}"); // ubs:ignore — fails the test
        };
        assert_eq!(
            (outcome.tx, outcome.result.clone()),
            (scene.tx, TxResult::RolledBack),
            "{got:#?}"
        );
        assert!(
            matches!(&outcome.warnings[..], [w] if matches!(w.code, WarningCode::JournalRecovered)),
            "its own journal_recovered is its outcome's: {got:#?}"
        );
        assert!(got.warnings.is_empty(), "{got:#?}");
        assert_eq!(
            scene.notes("Foo").as_deref(),
            Some("old foo"),
            "rolled back"
        );
    }

    /// A journal with no readable target fails every recovery that reaches it: one the
    /// restriction does not name is skipped like a live writer's, so it never stops the
    /// transaction that was disclosed, though it sorts first.
    #[test]
    fn a_restriction_skips_an_unattributed_journal_it_was_not_given() {
        let scene = crate::engine::testing::Scene::new();
        let unattributed: TxId = "01JB8Q2VA00000000000000001".parse().unwrap();
        plant_text(&scene.paths, unattributed, UNATTRIBUTED);
        let given = unprepared(&scene.paths);
        assert!(unattributed < given, "it sorts first");
        // Unrestricted, it stops recovery before the other is reached.
        let all = disclosed(&scene.paths, None);
        assert!(all.error.is_some(), "{all:#?}");
        assert!(scene.paths.journal(&given).exists(), "{all:#?}");
        let got = disclosed(&scene.paths, Some(&[given]));
        assert!(got.error.is_none(), "{got:#?}");
        assert_eq!(
            got.outcomes
                .iter()
                .map(|o| (o.tx, o.result.clone()))
                .collect::<Vec<_>>(),
            [(given, TxResult::Pruned)],
            "{got:#?}"
        );
        assert_eq!(
            std::fs::read_to_string(scene.paths.journal(&unattributed)).unwrap(),
            UNATTRIBUTED,
            "the unattributed journal is left as it is"
        );
    }

    /// A disclosed journal recovery stops at reports its error's code and reason: one
    /// whose client no longer carries its target's marker.
    #[test]
    fn a_disclosed_journal_that_stops_reports_its_reason() {
        let scene = planted(|t| format!("{t}\n"));
        std::fs::remove_file(crate::discover::registry::marker_path(
            scene.client.client_dir(),
        ))
        .unwrap();
        let got = disclosed(&scene.paths, Some(&[scene.tx]));
        assert_eq!(
            got.outcomes
                .iter()
                .map(|o| (o.tx, o.result.clone()))
                .collect::<Vec<_>>(),
            [(
                scene.tx,
                TxResult::Stopped {
                    code: ErrorCode::ScopeDenied,
                    reason: Some("marker_mismatch".to_owned()),
                }
            )],
            "{got:#?}"
        );
        assert_eq!(left(&scene), [true; 5], "left whole");
        assert!(
            got.warnings
                .iter()
                .any(|w| matches!(w.code, WarningCode::JournalPending)),
            "the journal it left is reported: {got:#?}"
        );
    }

    /// With no restriction, recovery is `recover_under`'s: every warning in `warnings`
    /// and no outcome, for a rollback, and for a journal that stops all of recovery.
    #[cfg(debug_assertions)]
    #[test]
    fn no_restriction_is_todays_recover_under() {
        let scene = interrupted_swap();
        let got = disclosed(&scene.paths, None);
        assert!(got.outcomes.is_empty() && got.error.is_none(), "{got:#?}");
        let [warning] = &got.warnings[..] else {
            panic!("one warning: {got:#?}"); // ubs:ignore — fails the test
        };
        assert_eq!(warning.code, WarningCode::JournalRecovered);
        assert!(
            warning.message.ends_with("recovery undid it (rolled back)"),
            "{}",
            warning.message
        );
        let scene = crate::engine::testing::Scene::new();
        plant_text(
            &scene.paths,
            "01JB8Q2VA00000000000000001".parse().unwrap(),
            UNATTRIBUTED,
        );
        let lock = scene.lock();
        let under = recover_under(&lock, &scene.paths, &|_| false, "test", Actor::Human);
        drop(lock);
        let got = disclosed(&scene.paths, None);
        assert_eq!(got.warnings, under, "{got:#?}");
        assert!(
            matches!(&under[..], [w] if matches!(w.code, WarningCode::JournalPending)
                && w.message.starts_with("unfinished transactions could not be recovered")),
            "{under:?}"
        );
    }

    // -----------------------------------------------------------------------------------
    // The passes (E14): the order recovery takes journals in, observed through a fake.

    mod passes {
        use std::borrow::Cow;
        use std::collections::{BTreeMap, BTreeSet};

        use serde_json::json;

        use super::super::*;
        use crate::engine::repair::{PlannedFix, RepairLine, RepairPrepared, provisional_journal};
        use crate::engine::repair_action::{
            Attribution, Fix, JournalSet, Mode, Primitive, QuarantineRoot, SuspendedArtifact,
        };
        use crate::engine::testing::{RepairHome, repair_home, repair_prepared};
        use crate::fsx::{FenceSet, FenceStop};

        const SET_ASIDE: &str = "fm-concurrency_primitives-journal-step-unrecognized";

        fn id(n: u8) -> TxId {
            format!("01JB8Q2VA000000000000000{n:02}").parse().unwrap()
        }

        /// Recovery's per-journal calls, recorded in order. Each settles at once unless
        /// it is one of `stops` (it then stops at an unrecognized step); `then` is what a
        /// settled repair's recovery did on disk.
        #[derive(Default)]
        struct Recording {
            calls: Vec<(&'static str, TxId)>,
            stops: BTreeSet<TxId>,
            then: BTreeMap<TxId, Box<dyn Fn()>>,
        }

        impl Settle for Recording {
            fn folder(
                &mut self,
                _: &RecoveryEnv<'_>,
                tx: &TxId,
                _: PathBuf,
            ) -> Result<Vec<Warning>> {
                self.calls.push(("folder", *tx));
                if self.stops.contains(tx) {
                    return Err(step_unrecognized(*tx));
                }
                Ok(Vec::new())
            }

            fn repair(
                &mut self,
                _: &RecoveryEnv<'_>,
                tx: TxId,
                _: &Path,
            ) -> Result<Option<Warning>> {
                self.calls.push(("repair", tx));
                if self.stops.contains(&tx) {
                    return Err(step_unrecognized(tx));
                }
                if let Some(then) = self.then.get(&tx) {
                    then();
                }
                Ok(None)
            }
        }

        fn step_unrecognized(tx: TxId) -> Error {
            Error::new(
                ErrorCode::Internal,
                format!(
                    "transaction {tx} could not be recovered (step_unrecognized): step 1 is in neither state"
                ),
                Suggestion::text("left for manual repair"),
            )
            .details(json!({"journal_pending": tx, "reason": "step_unrecognized", "target": null}))
        }

        /// `repair_home` without its stuck journal, so each test lists only its own.
        fn bare() -> RepairHome {
            let home = repair_home();
            let stuck = home.stuck();
            std::fs::remove_file(home.paths.journal(&stuck)).unwrap();
            std::fs::remove_file(home.paths.inflight_lock(&stuck)).unwrap();
            home
        }

        /// A folder journal of `tx`, on `target` when there is one, as far as recovery's
        /// isolation reads it.
        fn folder_journal(home: &RepairHome, tx: TxId, target: Option<TargetKey>) {
            let target = target
                .map(|t| format!(",\"target_key\":\"{t}\""))
                .unwrap_or_default();
            std::fs::write(
                home.paths.journal(&tx),
                format!("{{\"type\":\"prepared\",\"schema_version\":\"1.1\"{target}}}\n"),
            )
            .unwrap();
        }

        /// Plants `p` as repair journal `tx`, its `prepared` line alone.
        fn plant(home: &RepairHome, mut p: RepairPrepared, tx: TxId) -> TxId {
            p.tx = tx;
            let line = serde_json::to_string(&RepairLine::Prepared(Box::new(p))).unwrap();
            std::fs::write(home.paths.journal_repair(&tx), format!("{line}\n")).unwrap();
            tx
        }

        /// The set-aside of folder journal `tx`, lowered while it is in place.
        fn set_aside(home: &RepairHome, tx: TxId) -> RepairPrepared {
            let journal = JournalSet {
                tx,
                family: JournalFamily::Folder,
                target: Some(home.key),
                artifacts: vec![SuspendedArtifact {
                    path: home.paths.journal(&tx),
                    root: QuarantineRoot::State,
                }],
                guards: Vec::new(),
                attribution: Attribution::Complete,
                unattributed: Vec::new(),
            };
            let fix = PlannedFix {
                fm: Cow::Borrowed(SET_ASIDE),
                fix: Fix::Suspend { journal },
            };
            repair_prepared(home, &[], &[fix])
        }

        /// Where a set-aside's first rename takes the journal.
        fn quarantined(p: &RepairPrepared) -> PathBuf {
            match &p.primitives[0].op {
                Primitive::RenameNoReplace { to, .. } => to.clone(),
                op => panic!("{op:?}"), // ubs:ignore — fails the test
            }
        }

        /// A repair that names nothing: a chmod.
        fn chmod(home: &RepairHome) -> RepairPrepared {
            let fix = PlannedFix {
                fm: Cow::Borrowed("fm-secrets-credential-file-mode"),
                fix: Fix::Chmod {
                    path: home.credential(),
                    mode: Mode::new(0o600).unwrap(),
                },
            };
            repair_prepared(home, &[], &[fix])
        }

        fn rename(from: &Path, to: &Path) {
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::rename(from, to).unwrap();
        }

        fn recover<'a>(
            home: &'a RepairHome,
            own: &'a [TargetKey],
            fake: &mut Recording,
        ) -> Result<Vec<Warning>> {
            let lock = GlobalLock::acquire(&home.paths, "test").unwrap();
            let env = RecoveryEnv {
                lock: &lock,
                paths: &home.paths,
                scopes: &home.scopes,
                own,
                sv_writable: &no_saved_variables,
            };
            recover_all_with(&env, fake)
        }

        fn stuck_with(owner: TxId) -> Stuck {
            Stuck {
                stop: fence::repair_stop(
                    owner,
                    ErrorCode::Internal,
                    Some(StopReason::StepUnrecognized),
                    false,
                ),
                closed: false,
            }
        }

        #[test]
        fn recover_all_runs_three_passes() {
            let home = bare();
            let other = TargetKey::new();
            folder_journal(&home, id(1), Some(home.key));
            folder_journal(&home, id(2), Some(other));
            let owner = plant(&home, set_aside(&home, id(1)), id(5));
            let rest = plant(&home, chmod(&home), id(3));
            let mut fake = Recording::default();
            recover(&home, &[home.key], &mut fake).unwrap();
            assert_eq!(
                fake.calls,
                [
                    ("repair", owner),
                    ("folder", id(1)),
                    ("folder", id(2)),
                    ("repair", rest)
                ],
                "owners, then folder journals, then the other repairs"
            );
        }

        #[test]
        fn pass_two_lists_after_pass_one() {
            // A suspension rolled back in pass 1 has put its journal back: pass 2 sees it.
            let home = bare();
            folder_journal(&home, id(1), Some(home.key));
            let suspension = set_aside(&home, id(1));
            let away = quarantined(&suspension);
            rename(&home.paths.journal(&id(1)), &away);
            let owner = plant(&home, suspension, id(5));
            let mut fake = Recording::default();
            let journal = home.paths.journal(&id(1));
            fake.then
                .insert(owner, Box::new(move || rename(&away, &journal)));
            recover(&home, &[home.key], &mut fake).unwrap();
            assert_eq!(
                fake.calls,
                [("repair", owner), ("folder", id(1))],
                "rolled_back_suspension"
            );

            // A reactivation rolled back in pass 1 has taken its journal away: pass 2
            // never sees it.
            let home = bare();
            folder_journal(&home, id(1), Some(home.key));
            let mut reactivation = set_aside(&home, id(1));
            let away = quarantined(&reactivation);
            let provisional = provisional_journal(&home.paths, &id(1));
            let template = reactivation.primitives[0].clone();
            reactivation.primitives = [
                (away.clone(), provisional.clone(), false),
                (provisional, home.paths.journal(&id(1)), true),
            ]
            .into_iter()
            .zip(1..)
            .map(|((from, to, activation), seq)| {
                let mut step = template.clone();
                step.seq = seq;
                step.op = Primitive::RenameNoReplace {
                    from,
                    to,
                    activation,
                };
                step
            })
            .collect();
            let owner = plant(&home, reactivation, id(5));
            let mut fake = Recording::default();
            let journal = home.paths.journal(&id(1));
            fake.then
                .insert(owner, Box::new(move || rename(&journal, &away)));
            recover(&home, &[home.key], &mut fake).unwrap();
            assert_eq!(fake.calls, [("repair", owner)], "rolled_back_reactivation");
        }

        #[test]
        fn a_behind_journal_of_the_callers_target_fails_it_with_the_owners_stop() {
            type Case = (&'static str, bool, &'static [TargetKey]);
            // A journal of the caller's own target, and one with no readable target,
            // which fails any caller.
            let cases: [Case; 2] = [("own_target", true, &[]), ("no_target", false, &[])];
            for (case, targeted, extra_own) in cases {
                let home = bare();
                let target = targeted.then_some(home.key);
                folder_journal(&home, id(1), target);
                let owner = plant(&home, set_aside(&home, id(1)), id(5));
                let mut fake = Recording {
                    stops: BTreeSet::from([owner]),
                    ..Recording::default()
                };
                let own: Vec<TargetKey> = target.into_iter().chain(extra_own.to_vec()).collect();
                let err = recover(&home, &own, &mut fake).unwrap_err();
                assert_eq!(fake.calls, [("repair", owner)], "{case}: never recovered");
                assert_eq!(err.code, ErrorCode::Internal, "{case}: {err:?}");
                assert_eq!(
                    err.details,
                    json!({
                        "journal_pending": id(1),
                        "reason": "step_unrecognized",
                        "resource": format!("journal:{}", id(1)),
                        "target": target,
                        "tx": owner,
                    }),
                    "{case}"
                );
                assert_eq!(err.code.check_details(&err.details), Ok(()), "{case}");
                assert_eq!(
                    err.fix,
                    fence::explain_fix(Some(StopReason::StepUnrecognized)),
                    "{case}"
                );
                assert_eq!(
                    err.message,
                    format!(
                        "{} waits behind repair {owner}, which recovery could not settle (step_unrecognized); it is left as it is",
                        fence::folder_named(id(1), target)
                    ),
                    "{case}"
                );
                assert!(home.paths.journal(&id(1)).exists(), "{case}: left as it is");
            }
        }

        #[test]
        fn a_behind_journal_of_another_target_warns_once() {
            let home = bare();
            let other = TargetKey::new();
            folder_journal(&home, id(1), Some(other));
            let owner = plant(&home, set_aside(&home, id(1)), id(5));
            let mut fake = Recording {
                stops: BTreeSet::from([owner]),
                ..Recording::default()
            };
            let warnings = recover(&home, &[home.key], &mut fake).unwrap();
            assert_eq!(fake.calls, [("repair", owner)], "never recovered");
            let behind = fence::behind_warning(
                Behind::Folder {
                    tx: id(1),
                    target: Some(other),
                    owner,
                },
                &stuck_with(owner),
            );
            let about: Vec<&Warning> = warnings
                .iter()
                .filter(|w| w.message.contains(&id(1).to_string()))
                .collect();
            assert_eq!(about, [&behind], "{warnings:#?}");
            assert!(
                warnings.contains(&fence::stuck_warning(
                    owner,
                    Some(StopReason::StepUnrecognized),
                    false
                )),
                "the owner's own warning: {warnings:#?}"
            );
        }

        #[test]
        fn a_repair_stop_never_fails_the_caller() {
            let home = bare();
            folder_journal(&home, id(1), Some(home.key));
            let suspension = set_aside(&home, id(1));
            std::fs::remove_file(home.paths.journal(&id(1))).unwrap();
            let owner = plant(&home, suspension, id(5));
            let rest = plant(&home, chmod(&home), id(3));
            let mut fake = Recording {
                stops: BTreeSet::from([owner, rest]),
                ..Recording::default()
            };
            let warnings = recover(&home, &[home.key], &mut fake)
                .expect("a repair's own stop never fails the caller");
            assert_eq!(fake.calls, [("repair", owner), ("repair", rest)]);
            for tx in [owner, rest] {
                assert!(
                    warnings.contains(&fence::stuck_warning(
                        tx,
                        Some(StopReason::StepUnrecognized),
                        false
                    )),
                    "{tx}: {warnings:#?}"
                );
            }
        }

        #[test]
        fn a_pass_three_repair_waits_behind_an_unresolved_journal() {
            let home = repair_home();
            home.readable_stuck_journal();
            let stuck = home.stuck();
            // A repair recorded as writing below the stuck install's stage, which pass 2
            // leaves unresolved, and one writing elsewhere.
            let mut overlapping = chmod(&home);
            let stage = Resource::from_canonical(&home.client.stage(&stuck), PathKind::Subtree)
                .unwrap()
                .text()
                .unwrap();
            overlapping.resources.writes.push(stage);
            let waiting = plant(&home, overlapping, id(20));
            let disjoint = plant(&home, chmod(&home), id(21));
            let mut fake = Recording {
                stops: BTreeSet::from([stuck]),
                ..Recording::default()
            };
            let warnings = recover(&home, &[], &mut fake).unwrap();
            assert_eq!(
                fake.calls,
                [("folder", stuck), ("repair", disjoint)],
                "the overlapping repair is never recovered ahead of it"
            );
            let behind = fence::behind_warning(
                Behind::Unresolved {
                    tx: waiting,
                    owner: stuck,
                },
                &fence::folder_stuck(
                    stuck,
                    Some(home.key),
                    ErrorCode::Internal,
                    Some(StopReason::StepUnrecognized),
                ),
            );
            assert!(warnings.contains(&behind), "{warnings:#?}");
        }

        /// Recovery writes through the caller's table without its fences: a fence over a
        /// folder journal's stage never stops that journal's rollback from pruning it.
        #[test]
        fn recovery_sessions_are_unfenced() {
            let home = repair_home();
            home.readable_stuck_journal();
            let stuck = home.stuck();
            let stage = home.client.stage(&stuck);
            let mut fences = FenceSet::new();
            fences.fence(
                stage.clone(),
                format!("{}/", stage.display()),
                FenceStop {
                    tx: Some(id(9)),
                    target: None,
                    ..stuck_with(id(9)).stop
                },
            );
            let fenced = home.scopes.clone().with_fences(fences);
            let lock = GlobalLock::acquire(&home.paths, "test").unwrap();
            let env = RecoveryEnv {
                lock: &lock,
                paths: &home.paths,
                scopes: &fenced,
                own: &[home.key],
                sv_writable: &no_saved_variables,
            };
            let warnings = recover_all(&env).unwrap();
            let recovered = |w: &&Warning| {
                matches!(w.code, WarningCode::JournalRecovered)
                    && w.message.contains(&stuck.to_string())
            };
            assert!(warnings.iter().any(|w| recovered(&w)), "{warnings:#?}");
            assert!(!stage.exists(), "its stage was pruned under the fence");
            assert!(!home.paths.journal(&stuck).exists());
        }
    }
}
