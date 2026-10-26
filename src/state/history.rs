//! `history.jsonl`: the append-only transaction log (vision §5.6, D31). One JSON line
//! per transaction records who did it, why, the exact argv, and every folder touched,
//! which is what undo-conflict detection reads.
//!
//! Each line carries its own `schema_version` as its first key. A crash mid-append
//! leaves at most a torn last line; the reader skips it and never fails on a bad line.
//! The next write closes it off first, so a new line never merges with it, and the
//! reader salvages lines an older omaddons merged. Lines that still do not read are
//! counted, and `history` and `undo` warn about them.
//!
//! A repair (M4) and the undo of one name no single target: `target` is `null`,
//! `targets` lists the clients they touched, and `repairs` their filesystem operations.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::paths::Paths;
use super::store::SchemaVersion;
use crate::backup::BackupKind;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{Op, WriteSession, ensure_dir_all, mutate};
use crate::limits::{DISCOVERY_FILE_MAX_BYTES, REASON_MAX_CHARS};
use crate::model::{Actor, BackupId, FolderName, RunId, TargetKey, TxId, TxKind};

/// The version of the line format.
pub const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);

/// One transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HistoryEntry {
    pub schema_version: SchemaVersion,
    pub tx: TxId,
    pub kind: TxKind,
    #[schemars(with = "String")]
    pub at: Timestamp,
    /// The client the transaction changed; `null` on a repair and on the undo of one,
    /// which name the clients they touched in `targets`. Always written; a line without
    /// the key reads as `null`.
    pub target: Option<TargetKey>,
    /// The clients a target-less line touched; omitted when empty, so every line with a
    /// target is written as before M4.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<TargetKey>,
    pub actor: Actor,
    /// `--reason`, verbatim; omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub argv: Vec<String>,
    pub ops: Vec<HistoryOp>,
    pub result: TxResult,
    /// The transaction an undo reverted.
    #[serde(default)]
    pub reverts: Option<TxId>,
    /// An undo that reverted folders but left SavedVariables for later because the
    /// client was running; `undo <reverts> --saved-variables restore` finishes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sv_deferred: bool,
    /// How many SavedVariables files an undo restored (M3-D28), 0 included, so a later
    /// undo of a restore knows whether this one wrote settings since. Absent on every
    /// other kind and on undo lines from before M3, which count as "may have restored".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sv_restored: Option<u64>,
    /// The transaction created the client's `Interface/AddOns` (a client that had
    /// never been launched); undo leaves it in place.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub created_addons_dir: bool,
    /// The command invocation that committed it, shared by its transactions on other
    /// targets; absent on lines written before M2, each of which is a run by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunId>,
    /// The backup a `backup` line published (M3-D33); absent on every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<BackupNote>,
    /// A repair's doctor run directory, named `<UTC time>Z__<ULID>` (never a run id);
    /// absent on every other line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doctor_run_id: Option<String>,
    /// A repair's filesystem operations (and an undo's of a repair), in order; absent on
    /// every other line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repairs: Vec<RepairEntry>,
}

/// One filesystem operation of a repair (M4-D31), as its history line records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepairEntry {
    pub seq: u32,
    /// The fix of the doctor run it belongs to.
    pub fix: u32,
    /// The failure mode the fix repairs, `fm-<subsystem>-<name>`.
    pub fm: String,
    pub op: RepairOp,
    /// The absolute path it acts on.
    pub path: PathBuf,
    /// A rename's destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<PathBuf>,
    /// The mode it sets, as octal digits (`"0600"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// What its paths held before it ran.
    pub pre: RepairEndpoints,
    /// What they hold after it.
    pub post: RepairEndpoints,
    /// The backup an overwriting write reserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
    /// The operation that activates the fix.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub activation: bool,
    /// The client whose files it touches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetKey>,
    /// The AddOns folder its path is or is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderName>,
}

/// What a repair operation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepairOp {
    WriteFile,
    Chmod,
    #[serde(rename = "rename_noreplace")]
    RenameNoReplace,
}

/// What a repair operation's paths hold: a rename's source and destination, or the one
/// path of any other operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RepairEndpoints {
    Rename {
        path: RepairPredicate,
        to: RepairPredicate,
    },
    Path(RepairPredicate),
}

/// What one path holds, as a repair checks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepairPredicate {
    pub kind: PredicateKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<RepairIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The mode, as octal digits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// A directory's stamp digest (`stamp:sha256:…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_target: Option<PathBuf>,
}

/// A file's identity: its inode and birth time, as decimal strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepairIdentity {
    pub ino: String,
    pub born_ns: String,
}

/// What kind of entry a path holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PredicateKind {
    Absent,
    File,
    Dir,
    Symlink,
}

/// The clients a history line touched: these, or, for a target-less line that touched a
/// client's files without naming the client, every one (which fails closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    Targets(BTreeSet<TargetKey>),
    Every,
}

impl RepairEntry {
    /// Whether it acts on a client's files: it names a client or an AddOns folder, or a
    /// path of it lies under a client's `Interface/AddOns` or `Interface/.omaddons`.
    pub fn client_side(&self) -> bool {
        self.target.is_some()
            || self.folder.is_some()
            || self.paths().any(|path| {
                consecutive(path, "AddOns").is_some() || consecutive(path, ".omaddons").is_some()
            })
    }

    /// The AddOns folders it touches: its `folder`, and the component of each path right
    /// after `Interface/AddOns` when that is a folder name.
    pub fn addon_folders(&self) -> BTreeSet<FolderName> {
        let mut folders: BTreeSet<FolderName> = self.folder.iter().cloned().collect();
        for path in self.paths() {
            if let Some(name) = consecutive(path, "AddOns")
                .and_then(OsStr::to_str)
                .and_then(|name| FolderName::new(name).ok())
            {
                folders.insert(name);
            }
        }
        folders
    }

    fn paths(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.path.as_path()).chain(self.to.as_deref())
    }
}

/// Where `path` has a component `Interface` followed by one named `next` (both ignoring
/// ASCII case): the component after those two, or an empty name when the path ends there.
/// One pass, no allocation: `touches_target` runs on every line `status` reads.
fn consecutive<'p>(path: &'p Path, next: &str) -> Option<&'p OsStr> {
    let named = |name: &OsStr, want: &str| {
        name.to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(want))
    };
    let mut parts = path.iter();
    let mut interface = false;
    while let Some(part) = parts.next() {
        if interface && named(part, next) {
            return Some(parts.next().unwrap_or_default());
        }
        interface = named(part, "Interface");
    }
    None
}

/// What a `backup` history line says about the backup it published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackupNote {
    pub id: BackupId,
    pub kind: BackupKind,
    pub files: u64,
    /// The apparent size of its files.
    pub bytes: u64,
}

impl HistoryEntry {
    /// The run this transaction belongs to: its own `run`, or the transaction alone.
    pub fn run_key(&self) -> RunKey {
        self.run.map_or(RunKey::Single(self.tx), RunKey::Run)
    }

    /// Whether this build reads the line: one with a target, or a target-less repair
    /// (or undo of a repair, which carries its inverse operations) that moved no folder
    /// through `ops`. Any other target-less line stays unreadable, as it was before M4,
    /// so undo still assumes it touched whatever it may have (M3-D28 rule 5).
    pub fn well_formed(&self) -> bool {
        self.target.is_some()
            || (self.ops.is_empty()
                && (self.kind == TxKind::Repair
                    || (self.kind == TxKind::Undo && !self.repairs.is_empty())))
    }

    /// The clients the line touched: its target, its `targets` and each repair
    /// operation's; or every client when it is target-less, names none, and a repair
    /// operation acted on some client's files.
    pub fn reach(&self) -> Reach {
        if self.target.is_none()
            && self.targets.is_empty()
            && self.repairs.iter().any(RepairEntry::client_side)
        {
            return Reach::Every;
        }
        let mut keys: BTreeSet<TargetKey> = self.targets.iter().copied().collect();
        keys.extend(self.target);
        keys.extend(self.repairs.iter().filter_map(|repair| repair.target));
        Reach::Targets(keys)
    }

    /// Whether the line touched `key`, as [`HistoryEntry::reach`] says, without
    /// allocating.
    pub fn touches_target(&self, key: TargetKey) -> bool {
        if self.target == Some(key)
            || self.targets.contains(&key)
            || self.repairs.iter().any(|repair| repair.target == Some(key))
        {
            return true;
        }
        self.target.is_none()
            && self.targets.is_empty()
            && self.repairs.iter().any(RepairEntry::client_side)
    }
}

/// One settings change ([`TxKind::is_setting`]: `config set`, a `targets` change,
/// `auth github set` or `clear`, `export --write`), on the same line format as a
/// transaction: who, why, the argv, no folder ops, and the client's key for a `targets`
/// change (`null` otherwise). Never undone; undo, retention and recovery never see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SettingEntry {
    pub schema_version: SchemaVersion,
    pub tx: TxId,
    pub kind: TxKind,
    #[schemars(with = "String")]
    pub at: Timestamp,
    /// The client a `targets` change is about; `null` for the others.
    pub target: Option<TargetKey>,
    pub actor: Actor,
    /// `--reason`, verbatim; omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub argv: Vec<String>,
    /// Always empty: a settings change touches no folder.
    pub ops: Vec<HistoryOp>,
    pub result: TxResult,
    pub run: RunId,
    /// `errors dismiss`: what it acknowledged, as counts (never any error's text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed: Option<Dismissed>,
}

/// What one `errors dismiss` acknowledged on its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Dismissed {
    /// Databases acknowledged.
    pub databases: u64,
    /// New local errors that stopped being new.
    pub errors: u64,
}

impl SettingEntry {
    /// A settings change of `kind` made now by `actor` with `argv`, about `target`.
    pub fn now(
        kind: TxKind,
        target: Option<TargetKey>,
        actor: Actor,
        reason: Option<String>,
        argv: Vec<String>,
    ) -> Self {
        Self {
            schema_version: CURRENT,
            tx: TxId::new(),
            kind,
            at: Timestamp::now(),
            target,
            actor,
            reason,
            argv,
            ops: Vec::new(),
            result: TxResult::Committed,
            run: RunId::new(),
            dismissed: None,
        }
    }
}

/// What groups transactions into one run: a run id, or, for a line written before M2,
/// the transaction itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RunKey {
    Run(RunId),
    Single(TxId),
}

/// One folder a transaction touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HistoryOp {
    pub op: FolderOp,
    /// An AddOns folder; `WTF` for a `wtf` op; the addon's folder for `saved_variables`.
    pub folder: FolderName,
    /// The addon's record name; empty for a `wtf` op.
    pub addon: String,
    /// A `record` op that only changed who requires the addon: undone in place, without
    /// a conflict or a snapshot restore (M2-D35). Absent on every other op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<HistoryEdit>,
    /// A `wtf` or `saved_variables` op: the backup its files came from, when they came
    /// from one. Absent on every other op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<BackupId>,
    /// A `wtf` or `saved_variables` op: where its files came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<WtfSource>,
    /// A `wtf` or `saved_variables` op: how many files it put back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<u64>,
}

/// Where a `wtf` or `saved_variables` op's files came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WtfSource {
    /// A `WTF/` backup (`backup restore`).
    Backup,
    /// A transaction's snapshot (an undo of a restore).
    Snapshot,
}

/// An in-place record edit an op stands for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HistoryEdit {
    /// The requester was added to the addon's `required_by`.
    RequiredByAdd(String),
    /// The requester was removed from it.
    RequiredByRemove(String),
}

impl HistoryEdit {
    /// The edit that undoes this one.
    #[must_use]
    pub fn inverse(&self) -> Self {
        match self {
            Self::RequiredByAdd(name) => Self::RequiredByRemove(name.clone()),
            Self::RequiredByRemove(name) => Self::RequiredByAdd(name.clone()),
        }
    }
}

/// What happened to a folder. `record` is a record-only change (adoption, an
/// ownership transfer). `wtf` and `saved_variables` put settings back (M3-D24): a whole
/// `WTF/`, or one addon's SavedVariables; they name no AddOns folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FolderOp {
    Add,
    Replace,
    Remove,
    Record,
    Wtf,
    #[serde(rename = "saved_variables")]
    SavedVariables,
}

impl FolderOp {
    /// Whether the op is about an AddOns folder, as every op was before M3; a `wtf` or
    /// `saved_variables` op is about settings, and AddOns readers skip it.
    pub fn is_folder(self) -> bool {
        match self {
            Self::Add | Self::Replace | Self::Remove | Self::Record => true,
            Self::Wtf | Self::SavedVariables => false,
        }
    }
}

/// Whether a transaction with `ops` puts settings back (a restore, a partial one, or the
/// undo of either): any op about settings. Its journal, stage and snapshots are named
/// `<tx>.wtf`, which a pre-M3 build never lists.
pub fn puts_settings_back(ops: &[HistoryOp]) -> bool {
    ops.iter().any(|op| !op.op.is_folder())
}

/// How a transaction ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TxResult {
    Committed,
}

/// Appends one line through `fsx::mutate(AppendFile)`: a single write plus fsync,
/// inside the commit's transaction session while the global lock is held. A last line
/// a crash cut short is closed off in the same write, so the new line never merges
/// with it.
pub fn append(s: &mut WriteSession<'_>, paths: &Paths, entry: &HistoryEntry) -> Result<()> {
    append_line(s, paths, entry, entry.reason.as_deref())
}

/// [`append`] for a settings change, in the session of the command that made it, under
/// the global lock it holds.
pub fn append_setting(s: &mut WriteSession<'_>, paths: &Paths, entry: &SettingEntry) -> Result<()> {
    append_line(s, paths, entry, entry.reason.as_deref())
}

/// Records a settings change in the session of the command that made it, under the
/// global lock: its line, or, when that cannot be written, `history_not_recorded` (the
/// change itself stands).
pub fn record_setting(
    s: &mut WriteSession<'_>,
    paths: &Paths,
    entry: &SettingEntry,
) -> Option<Warning> {
    let written = (|| -> Result<()> {
        ensure_dir_all(s, paths.state_dir())?;
        append_setting(s, paths, entry)
    })();
    let error = written.err()?;
    Some(
        Warning::new(
            WarningCode::HistoryNotRecorded,
            format!(
                "the change was made, but its history line could not be written: {}",
                error.message
            ),
        )
        .with_fix(Suggestion::text(format!(
            "check that {} is writable",
            paths.state_dir().display()
        ))),
    )
}

fn append_line<T: Serialize>(
    s: &mut WriteSession<'_>,
    paths: &Paths,
    entry: &T,
    reason: Option<&str>,
) -> Result<()> {
    if let Some(reason) = reason
        && reason.chars().count() > REASON_MAX_CHARS
    {
        return Err(Error::new(
            ErrorCode::Usage,
            format!("--reason is longer than {REASON_MAX_CHARS} characters"),
            Suggestion::text("shorten --reason"),
        ));
    }
    let path = paths.history_file();
    let before = std::fs::metadata(&path).map_or(0, |m| m.len());
    let mut line = Vec::new();
    if unterminated(&path) {
        line.push(b'\n');
    }
    serde_json::to_writer(&mut line, entry)
        .map_err(|err| Error::internal(format!("serializing a history line: {err}")))?;
    line.push(b'\n');
    mutate(s, &path, Op::AppendFile { content: &line })?;
    // The projection follows the durable line, still under the caller's lock; it never
    // fails the append.
    super::history_tail::appended(paths, before, &line);
    Ok(())
}

/// Closes off a last line a crash cut short, under the global lock: a line that was
/// complete but for its newline then reads back, and the next append starts a fresh
/// line. Roll-forward does this before it looks for its own line, so it neither misses
/// nor duplicates it.
pub fn close_torn_tail(s: &mut WriteSession<'_>, paths: &Paths) -> Result<()> {
    let path = paths.history_file();
    if unterminated(&path) {
        mutate(s, &path, Op::AppendFile { content: b"\n" })?;
    }
    Ok(())
}

/// Whether `path` is a non-empty file whose last byte is not a newline.
fn unterminated(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1)).is_ok() && file.read_exact(&mut last).is_ok() && last[0] != b'\n'
}

/// Which entries to return.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryFilter {
    /// A transaction on this client; a target-less line (a repair) matches each client
    /// it touches ([`HistoryEntry::touches_target`]).
    pub target: Option<TargetKey>,
    pub actor: Option<Actor>,
    pub kind: Option<TxKind>,
    pub since: Option<Timestamp>,
    /// One run's lines only.
    pub run: Option<RunId>,
    /// The most recent N.
    pub limit: Option<usize>,
}

impl HistoryFilter {
    fn matches(&self, entry: &HistoryEntry) -> bool {
        self.target.is_none_or(|t| entry.touches_target(t))
            && self.actor.is_none_or(|a| a == entry.actor)
            && self.kind.is_none_or(|k| k == entry.kind)
            && self.since.is_none_or(|since| entry.at >= since)
            && self.run.is_none_or(|run| entry.run == Some(run))
    }

    /// The same for a settings change: one with no target never matches a target.
    fn matches_setting(&self, entry: &SettingEntry) -> bool {
        self.target.is_none_or(|t| entry.target == Some(t))
            && self.actor.is_none_or(|a| a == entry.actor)
            && self.kind.is_none_or(|k| k == entry.kind)
            && self.since.is_none_or(|since| entry.at >= since)
            && self.run.is_none_or(|run| entry.run == run)
    }
}

/// The matching entries, oldest first, and what was skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryRead {
    pub entries: Vec<HistoryEntry>,
    /// The matching settings changes, oldest first (`--limit` applies to `entries`
    /// alone): `history` lists them, undo never sees them.
    pub settings: Vec<SettingEntry>,
    /// The last line has no newline yet (cut short by a crash, or still being written)
    /// and was skipped.
    pub torn_tail: bool,
    /// Lines a crash cut short that a later write closed off. No transaction is lost on
    /// one: recovery appends the whole line again.
    pub torn_lines: usize,
    /// Other lines that do not read (a hand edit, damage, a newer omaddons): the
    /// transactions on them are lost to `history` and `undo`.
    pub skipped_lines: usize,
    /// Those lines, for undo's conflict analysis, which assumes each touched whatever it
    /// may have (M3-D28 rule 5).
    pub unreadable: Vec<UnreadableLine>,
}

/// A history line that does not read, where it is and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableLine {
    /// How many of the matching transactions come before it (`entries[..after]`): the
    /// history is append-only, so it is newer than each of them.
    pub after: usize,
    /// The line, cut at [`DISCOVERY_FILE_MAX_BYTES`] (a line cut there reads as no JSON).
    pub raw: String,
}

impl UnreadableLine {
    fn of(line: &str, after: usize) -> Self {
        let max = usize::try_from(DISCOVERY_FILE_MAX_BYTES).unwrap_or(usize::MAX);
        let mut end = line.len().min(max);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            after,
            raw: line[..end].to_owned(),
        }
    }
}

impl HistoryRead {
    /// `history_lines_skipped`, when lines other than cut-short writes did not read.
    pub fn skipped_warning(&self) -> Option<Warning> {
        let message = match self.skipped_lines {
            0 => return None,
            1 => "1 line of history.jsonl could not be read and was skipped; the transaction on it is not listed and cannot be undone".to_owned(),
            n => format!(
                "{n} lines of history.jsonl could not be read and were skipped; the transactions on them are not listed and cannot be undone"
            ),
        };
        Some(
            Warning::new(WarningCode::HistoryLinesSkipped, message).with_fix(Suggestion::text(
                "a line was damaged by a crash or a hand edit, or written by a newer omaddons; omaddons never rewrites history.jsonl",
            )),
        )
    }
}

/// Reads the history, skipping bad lines rather than failing.
pub fn read(paths: &Paths, filter: &HistoryFilter) -> Result<HistoryRead> {
    Ok(parse(&text(paths)?, filter))
}

/// The history as it is on disk, torn and merged lines included; empty before the first
/// transaction.
pub fn text(paths: &Paths) -> Result<String> {
    let path = paths.history_file();
    match std::fs::read(&path) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(Error::io(
            &path,
            &err,
            Suggestion::command(["omaddons", "status"]),
        )),
    }
}

/// The entries of history `text` that match `filter`, skipping bad lines.
pub fn parse(text: &str, filter: &HistoryFilter) -> HistoryRead {
    let mut read = HistoryRead::default();
    let complete = text.ends_with('\n');
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let last = lines.len().saturating_sub(1);
    for (index, line) in lines.iter().enumerate() {
        if index == last && !complete {
            read.torn_tail = true;
            continue;
        }
        match read_line(line) {
            Line::Entry(entry) => {
                if filter.matches(&entry) {
                    read.entries.push(*entry);
                }
            }
            Line::Setting(entry) => {
                if filter.matches_setting(&entry) {
                    read.settings.push(*entry);
                }
            }
            Line::Torn => read.torn_lines += 1,
            Line::Unreadable => {
                read.skipped_lines += 1;
                read.unreadable
                    .push(UnreadableLine::of(line, read.entries.len()));
            }
        }
    }
    if let Some(limit) = filter.limit {
        let excess = read.entries.len().saturating_sub(limit);
        read.entries.drain(..excess);
        for line in &mut read.unreadable {
            line.after = line.after.saturating_sub(excess);
        }
    }
    read
}

/// How every line starts: `schema_version` is the first key of [`HistoryEntry`]. Inside
/// a string value the quotes would be escaped, so this marks a line's start only.
const LINE_START: &str = "{\"schema_version\":";

/// One complete line, read.
enum Line {
    Entry(Box<HistoryEntry>),
    Setting(Box<SettingEntry>),
    /// The JSON ends early: a write a crash cut short.
    Torn,
    /// Anything else that does not read.
    Unreadable,
}

/// Reads one complete line. An older omaddons appended right after a cut-short line,
/// merging the two (`<fragment><line>`); when the whole does not read, the part from
/// the last line start on is tried, which salvages the line appended after it.
fn read_line(line: &str) -> Line {
    let error = match whole(line) {
        Ok(read) => return read,
        Err(error) => error,
    };
    if let Some(at) = line.rfind(LINE_START).filter(|at| *at > 0)
        && let Ok(read) = whole(&line[at..])
    {
        return read;
    }
    match error.classify() {
        serde_json::error::Category::Eof => Line::Torn,
        _ => Line::Unreadable,
    }
}

/// `text` as one line: a settings change by its kind (its target may be `null`), else
/// a transaction, which reads only when [`HistoryEntry::well_formed`]. A settings line
/// cut short is torn: both parses end at the cut, and the settings parse's end-of-input
/// error is the one returned (a complete line never gives one).
fn whole(text: &str) -> std::result::Result<Line, serde_json::Error> {
    let setting = match serde_json::from_str::<SettingEntry>(text) {
        Ok(setting) if setting.kind.is_setting() => return Ok(Line::Setting(Box::new(setting))),
        other => other.err(),
    };
    serde_json::from_str::<HistoryEntry>(text)
        .map(|entry| {
            if entry.well_formed() {
                Line::Entry(Box::new(entry))
            } else {
                Line::Unreadable
            }
        })
        .map_err(|err| match setting {
            Some(cut) if cut.is_eof() => cut,
            _ => err,
        })
}

/// What one history line is, for the history projection.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LineFacts {
    pub run: RunKey,
    /// A transaction undo can revert (never a settings change or a backup).
    pub undoable: bool,
    pub at: Timestamp,
}

/// The fields of a line [`LineFacts`] needs; the rest (a transaction's ops) is skipped
/// without being built, so the projection reads a megabyte of lines in a few
/// milliseconds.
#[derive(Deserialize)]
struct FactFields {
    kind: TxKind,
    at: Timestamp,
    #[serde(default)]
    run: Option<RunId>,
    #[serde(default)]
    tx: Option<TxId>,
}

/// One complete line's facts, salvaged like [`read_line`] does; `None` for a line that
/// does not read.
pub(crate) fn line_facts(line: &str) -> Option<LineFacts> {
    let fields = serde_json::from_str::<FactFields>(line).ok().or_else(|| {
        let at = line.rfind(LINE_START).filter(|at| *at > 0)?;
        serde_json::from_str::<FactFields>(&line[at..]).ok()
    })?;
    let run = match (fields.run, fields.tx) {
        (Some(run), _) => RunKey::Run(run),
        (None, Some(tx)) if !fields.kind.is_setting() => RunKey::Single(tx),
        _ => return None,
    };
    Some(LineFacts {
        run,
        undoable: fields.kind.is_undoable(),
        at: fields.at,
    })
}

/// The transactions one run selection reverts, newest first, and their run (`None` for a
/// transaction from before M2, or one transaction picked on a target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSelection<'e> {
    pub entries: Vec<&'e HistoryEntry>,
    pub run: Option<RunId>,
}

/// What bare `undo` reverts in `entries` (oldest first): the newest undoable transaction
/// (by `last_by`, on `target`), and, without a target, every other undoable transaction
/// of its run wherever its lines fell among other runs'. `target` `Some(None)` names a
/// target never registered, so it selects nothing. Pure, so `undo` (over the whole
/// history) and `status` (over the history projection) never disagree (M3-D37).
pub fn newest_run(
    entries: &[HistoryEntry],
    last_by: Option<Actor>,
    target: Option<Option<TargetKey>>,
) -> Option<RunSelection<'_>> {
    // A target never registered (`Some(None)`) selects nothing; a target-less line (a
    // repair) is on each target it touches.
    let on_target = |e: &HistoryEntry| match target {
        None => true,
        Some(None) => false,
        Some(Some(key)) => e.touches_target(key),
    };
    let newest = entries.iter().rev().find(|e| {
        e.kind.is_undoable() && on_target(e) && last_by.is_none_or(|actor| e.actor == actor)
    })?;
    if target.is_some() {
        return Some(RunSelection {
            entries: vec![newest],
            run: None,
        });
    }
    let key = newest.run_key();
    Some(RunSelection {
        entries: entries
            .iter()
            .rev()
            .filter(|e| e.kind.is_undoable() && e.run_key() == key)
            .collect(),
        run: newest.run,
    })
}

/// The most recent matching entry (`undo`, `undo --last-by`).
pub fn last(paths: &Paths, filter: &HistoryFilter) -> Result<Option<HistoryEntry>> {
    let filter = HistoryFilter {
        limit: Some(1),
        ..filter.clone()
    };
    Ok(read(paths, &filter)?.entries.pop())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::{ScopeTable, Writer, failpoint, sys};

    const TARGET: &str = "01JB8Q2V9W0000000000000000";

    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        scopes: ScopeTable,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(
            &root,
            root.join(".config"),
            root.join(".local/state"),
            root.join(".cache"),
        );
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        Fixture {
            _dir: dir,
            paths,
            scopes,
        }
    }

    fn entry(n: u8, actor: Actor, kind: TxKind, at: &str) -> HistoryEntry {
        HistoryEntry {
            schema_version: CURRENT,
            tx: format!("01JB8Q2VA0000000000000000{}", char::from(b'0' + n))
                .parse()
                .unwrap(),
            kind,
            at: at.parse().unwrap(),
            target: Some(TARGET.parse().unwrap()),
            targets: Vec::new(),
            actor,
            reason: None,
            argv: vec!["omaddons".into(), "install".into(), "weakauras".into()],
            ops: vec![HistoryOp {
                op: FolderOp::Add,
                folder: FolderName::new("WeakAuras").unwrap(),
                addon: "WeakAuras".into(),
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
            doctor_run_id: None,
            repairs: Vec::new(),
        }
    }

    /// A transaction of `run` on `target` (`None`: a line from before M2).
    fn of_run(n: u8, kind: TxKind, run: Option<RunId>, target: &str) -> HistoryEntry {
        HistoryEntry {
            run,
            target: Some(target.parse().unwrap()),
            ..entry(n, Actor::Human, kind, "2026-10-21T18:00:00Z")
        }
    }

    fn selected(found: &RunSelection<'_>) -> Vec<String> {
        found.entries.iter().map(|e| e.tx.to_string()).collect()
    }

    #[test]
    fn the_newest_run_gathers_its_interleaved_lines_and_skips_what_undo_never_reverts() {
        const OTHER: &str = "01JB8Q2V9W0000000000000001";
        let (a, b) = (RunId::new(), RunId::new());
        // Two runs from two terminals, alternating; then a backup line.
        let entries = vec![
            of_run(1, TxKind::Install, Some(a), TARGET),
            of_run(2, TxKind::Update, Some(b), TARGET),
            of_run(3, TxKind::Install, Some(a), OTHER),
            of_run(4, TxKind::Update, Some(b), OTHER),
            of_run(5, TxKind::Backup, Some(RunId::new()), TARGET),
        ];
        let found = newest_run(&entries, None, None).unwrap();
        assert_eq!(found.run, Some(b), "a backup is never the last change");
        assert_eq!(
            selected(&found),
            [entries[3].tx.to_string(), entries[1].tx.to_string()],
            "every line of the run, newest first"
        );
        // -t picks one transaction on that target; --last-by filters by actor.
        let on_target = newest_run(&entries, None, Some(Some(TARGET.parse().unwrap()))).unwrap();
        assert_eq!(selected(&on_target), [entries[1].tx.to_string()]);
        assert_eq!(on_target.run, None);
        assert!(newest_run(&entries, Some(Actor::Agent), None).is_none());
        assert!(
            newest_run(&entries, None, Some(None)).is_none(),
            "never registered"
        );
        // An undo is the newest change, itself undoable (a redo).
        let mut with_undo = entries.clone();
        with_undo.push(HistoryEntry {
            reverts: Some(entries[3].tx),
            ..of_run(6, TxKind::Undo, Some(RunId::new()), OTHER)
        });
        assert_eq!(
            selected(&newest_run(&with_undo, None, None).unwrap()),
            [with_undo[5].tx.to_string()]
        );
        // A line from before M2 is a run by itself.
        let old = vec![
            of_run(1, TxKind::Install, None, TARGET),
            of_run(2, TxKind::Install, None, TARGET),
        ];
        let found = newest_run(&old, None, None).unwrap();
        assert_eq!(
            (selected(&found), found.run),
            (vec![old[1].tx.to_string()], None)
        );
        assert!(newest_run(&[], None, None).is_none());
    }

    #[test]
    fn restore_ops_round_trip_and_older_ops_are_unchanged() {
        let backup: BackupId = "01M6K2VA000000000000000000".parse().unwrap();
        let wtf = HistoryOp {
            op: FolderOp::Wtf,
            folder: FolderName::new("WTF").unwrap(),
            addon: String::new(),
            edit: None,
            backup: Some(backup),
            from: Some(WtfSource::Backup),
            files: Some(410),
        };
        let json = serde_json::to_string(&wtf).unwrap();
        assert_eq!(
            json,
            r#"{"op":"wtf","folder":"WTF","addon":"","backup":"01M6K2VA000000000000000000","from":"backup","files":410}"#
        );
        assert_eq!(serde_json::from_str::<HistoryOp>(&json).unwrap(), wtf);
        let sv = HistoryOp {
            op: FolderOp::SavedVariables,
            folder: FolderName::new("Foo").unwrap(),
            addon: "Foo".into(),
            from: Some(WtfSource::Snapshot),
            backup: None,
            ..wtf.clone()
        };
        let json = serde_json::to_string(&sv).unwrap();
        assert!(json.contains(r#""op":"saved_variables""#), "{json}");
        assert!(json.contains(r#""from":"snapshot""#), "{json}");
        assert!(
            !json.contains("backup"),
            "an absent key is left out: {json}"
        );
        assert_eq!(serde_json::from_str::<HistoryOp>(&json).unwrap(), sv);
        // An AddOns op is written exactly as before M3.
        let add = HistoryOp {
            op: FolderOp::Add,
            folder: FolderName::new("Foo").unwrap(),
            addon: "Foo".into(),
            edit: None,
            backup: None,
            from: None,
            files: None,
        };
        assert_eq!(
            serde_json::to_string(&add).unwrap(),
            r#"{"op":"add","folder":"Foo","addon":"Foo"}"#
        );
        assert!(FolderOp::Add.is_folder() && FolderOp::Record.is_folder());
        assert!(!FolderOp::Wtf.is_folder() && !FolderOp::SavedVariables.is_folder());
    }

    #[test]
    fn in_place_edits_ride_on_record_ops() {
        let mut op = HistoryOp {
            op: FolderOp::Record,
            folder: FolderName::new("BigWigs").unwrap(),
            addon: "BigWigs".into(),
            edit: Some(HistoryEdit::RequiredByAdd("LittleWigs".into())),
            backup: None,
            from: None,
            files: None,
        };
        assert_eq!(
            serde_json::to_string(&op).unwrap(),
            r#"{"op":"record","folder":"BigWigs","addon":"BigWigs","edit":{"required_by_add":"LittleWigs"}}"#
        );
        op.edit = Some(HistoryEdit::RequiredByRemove("LittleWigs".into()));
        let line = serde_json::to_string(&op).unwrap();
        assert!(
            line.ends_with(r#""edit":{"required_by_remove":"LittleWigs"}}"#),
            "{line}"
        );
        assert_eq!(serde_json::from_str::<HistoryOp>(&line).unwrap(), op);
        assert_eq!(
            op.edit.as_ref().map(HistoryEdit::inverse),
            Some(HistoryEdit::RequiredByAdd("LittleWigs".into()))
        );
        // An M1 op: no edit, and none written back.
        let m1 = r#"{"op":"add","folder":"Foo","addon":"Foo"}"#;
        let parsed: HistoryOp = serde_json::from_str(m1).unwrap();
        assert_eq!(parsed.edit, None);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), m1);
    }

    #[test]
    fn runs_round_trip_and_filter() {
        let mut e = entry(1, Actor::Agent, TxKind::Update, "2026-10-14T21:00:00Z");
        assert!(
            !serde_json::to_string(&e).unwrap().contains("run"),
            "omitted when absent"
        );
        assert_eq!(e.run_key(), RunKey::Single(e.tx));
        let run = RunId::new();
        e.run = Some(run);
        let line = serde_json::to_string(&e).unwrap();
        assert!(line.ends_with(&format!(r#","run":"{run}"}}"#)), "{line}");
        let back: HistoryEntry = serde_json::from_str(&line).unwrap();
        assert_eq!((back.run, back.run_key()), (Some(run), RunKey::Run(run)));

        let f = fixture();
        let other = entry(2, Actor::Human, TxKind::Install, "2026-10-14T22:00:00Z");
        append_all(&f, &[e.clone(), other]);
        let filter = HistoryFilter {
            run: Some(run),
            ..HistoryFilter::default()
        };
        let read = read(&f.paths, &filter).unwrap();
        assert_eq!(read.entries, [e]);
    }

    #[test]
    fn a_backup_line_carries_its_note_and_older_lines_read_without_one() {
        let mut e = entry(1, Actor::Human, TxKind::Backup, "2026-10-21T18:00:00Z");
        e.ops.clear();
        e.run = Some(RunId::new());
        e.backup = Some(BackupNote {
            id: "01M6Q2VA000000000000000000".parse().unwrap(),
            kind: BackupKind::Manual,
            files: 412,
            bytes: 15_937_536,
        });
        let line = serde_json::to_string(&e).unwrap();
        assert!(
            line.ends_with(r#","backup":{"id":"01M6Q2VA000000000000000000","kind":"manual","files":412,"bytes":15937536}}"#),
            "{line}"
        );
        let back: HistoryEntry = serde_json::from_str(&line).unwrap();
        assert_eq!(back, e);
        let older = entry(2, Actor::Human, TxKind::Install, "2026-10-21T18:00:00Z");
        let line = serde_json::to_string(&older).unwrap();
        assert!(!line.contains("backup"), "omitted when absent: {line}");
        let back: HistoryEntry = serde_json::from_str(&line).unwrap();
        assert_eq!(back.backup, None);
    }

    fn append_all(f: &Fixture, entries: &[HistoryEntry]) {
        let mut s = crate::fsx::WriteSession::untracked(Writer::Mutating, &f.scopes, "test");
        for e in entries {
            append(&mut s, &f.paths, e).unwrap();
        }
    }

    #[test]
    fn lines_have_the_documented_shape() {
        let mut e = entry(1, Actor::Agent, TxKind::Install, "2026-10-14T21:00:00Z");
        e.reason = Some("keep raid addons current".into());
        let line = serde_json::to_string(&e).unwrap();
        assert!(
            line.starts_with(r#"{"schema_version":"1.0","tx":"#),
            "{line}"
        );
        assert!(line.contains(r#""ops":[{"op":"add","folder":"WeakAuras","addon":"WeakAuras"}],"result":"committed","reverts":null}"#), "{line}");
        e.reason = None;
        assert!(!serde_json::to_string(&e).unwrap().contains("reason"));
    }

    #[test]
    fn append_writes_one_line_and_rejects_long_reasons() {
        let f = fixture();
        let mut undo = entry(2, Actor::Human, TxKind::Undo, "2026-10-14T22:00:00Z");
        undo.reverts = Some(entry(1, Actor::Agent, TxKind::Install, "2026-10-14T21:00:00Z").tx);
        append_all(
            &f,
            &[
                entry(1, Actor::Agent, TxKind::Install, "2026-10-14T21:00:00Z"),
                undo.clone(),
            ],
        );
        let text = std::fs::read_to_string(f.paths.history_file()).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with('\n'));
        assert_eq!(
            read(&f.paths, &HistoryFilter::default()).unwrap().entries[1],
            undo
        );

        let mut long = entry(3, Actor::Agent, TxKind::Install, "2026-10-14T23:00:00Z");
        long.reason = Some("é".repeat(201));
        let mut s = crate::fsx::WriteSession::untracked(Writer::Mutating, &f.scopes, "test");
        let err = append(&mut s, &f.paths, &long).unwrap_err();
        assert_eq!((err.code, err.exit().code()), (ErrorCode::Usage, 2));
        long.reason = Some("é".repeat(200));
        append(&mut s, &f.paths, &long).unwrap();
    }

    #[test]
    fn torn_tails_and_corrupt_lines_are_skipped() {
        let f = fixture();
        append_all(
            &f,
            &[entry(
                1,
                Actor::Agent,
                TxKind::Install,
                "2026-10-14T21:00:00Z",
            )],
        );
        sys::append_durable(&f.paths.history_file(), b"{not json}\n").unwrap();
        append_all(
            &f,
            &[entry(
                2,
                Actor::Human,
                TxKind::Remove,
                "2026-10-14T22:00:00Z",
            )],
        );
        sys::append_durable(
            &f.paths.history_file(),
            br#"{"schema_version":"1.0","tx":"01JB"#,
        )
        .unwrap();
        let read = read(&f.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(read.entries.len(), 2);
        assert!(read.torn_tail);
        assert_eq!(read.skipped_lines, 1);
    }

    fn json(entry: &HistoryEntry) -> String {
        serde_json::to_string(entry).unwrap()
    }

    fn txs(read: &HistoryRead) -> Vec<String> {
        read.entries.iter().map(|e| e.tx.to_string()).collect()
    }

    #[test]
    fn an_append_after_a_cut_short_line_starts_a_fresh_one() {
        let f = fixture();
        let [one, two, three] =
            [1, 2, 3].map(|n| entry(n, Actor::Agent, TxKind::Install, "2026-10-14T21:00:00Z"));
        append_all(&f, std::slice::from_ref(&one));
        sys::append_durable(&f.paths.history_file(), &json(&two).as_bytes()[..40]).unwrap();
        append_all(&f, std::slice::from_ref(&three));

        let text = std::fs::read_to_string(f.paths.history_file()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(lines[1], &json(&two)[..40], "the fragment stands alone");
        let read = read(&f.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(txs(&read), [one.tx.to_string(), three.tx.to_string()]);
        assert_eq!(
            (read.torn_tail, read.torn_lines, read.skipped_lines),
            (false, 1, 0)
        );
        assert_eq!(
            read.skipped_warning(),
            None,
            "a closed fragment loses nothing"
        );
    }

    /// A settings line has a `null` target, which fails as a transaction before the
    /// cut: cut short anywhere after it, it is still a torn line, never an unreadable one.
    #[test]
    fn a_cut_short_settings_line_is_torn() {
        let f = fixture();
        let setting = SettingEntry::now(
            TxKind::Config,
            None,
            Actor::Human,
            None,
            vec!["omaddons".into(), "config".into()],
        );
        let line = serde_json::to_string(&setting).unwrap();
        let cut = line.find("\"target\":null").unwrap() + "\"target\":null,".len() + 4;
        sys::append_durable(&f.paths.history_file(), &line.as_bytes()[..cut]).unwrap();
        let one = entry(1, Actor::Agent, TxKind::Install, "2026-10-14T21:00:00Z");
        append_all(&f, std::slice::from_ref(&one));

        let read = read(&f.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(txs(&read), [one.tx.to_string()]);
        assert_eq!((read.torn_lines, read.skipped_lines), (1, 0));
    }

    #[test]
    fn a_line_merged_onto_a_fragment_by_an_older_omaddons_is_salvaged() {
        let f = fixture();
        let [one, two] =
            [1, 2].map(|n| entry(n, Actor::Agent, TxKind::Install, "2026-10-14T21:00:00Z"));
        append_all(&f, std::slice::from_ref(&one));
        let merged = format!("{}{}\n", &json(&two)[..40], json(&two));
        sys::append_durable(&f.paths.history_file(), merged.as_bytes()).unwrap();
        // A whole line without its newline, then the same line again.
        let doubled = format!("{}{}\n", json(&one), json(&two));
        sys::append_durable(&f.paths.history_file(), doubled.as_bytes()).unwrap();

        let read = read(&f.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(
            txs(&read),
            [one.tx.to_string(), two.tx.to_string(), two.tx.to_string()]
        );
        assert_eq!((read.torn_lines, read.skipped_lines), (0, 0));
    }

    #[test]
    fn unreadable_lines_are_counted_apart_from_cut_short_ones() {
        let f = fixture();
        assert_eq!(
            read(&f.paths, &HistoryFilter::default()).unwrap(),
            HistoryRead::default(),
            "no file"
        );
        let one = entry(1, Actor::Agent, TxKind::Install, "2026-10-14T21:00:00Z");
        append_all(&f, std::slice::from_ref(&one));
        sys::append_durable(&f.paths.history_file(), b"not json\n").unwrap();
        sys::append_durable(
            &f.paths.history_file(),
            b"{\"schema_version\":\"1.0\",\"tx\"\n",
        )
        .unwrap();

        let read = read(&f.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(txs(&read), [one.tx.to_string()]);
        assert_eq!(
            (read.torn_tail, read.torn_lines, read.skipped_lines),
            (false, 1, 1)
        );
        let warning = read.skipped_warning().unwrap();
        assert_eq!(warning.code, WarningCode::HistoryLinesSkipped);
        assert!(
            warning
                .message
                .starts_with("1 line of history.jsonl could not be read"),
            "{}",
            warning.message
        );
        assert_eq!(warning.fix.as_ref().and_then(|f| f.argv()), None);
    }

    #[test]
    fn a_line_missing_only_its_newline_reads_once_closed_off() {
        let f = fixture();
        let one = entry(1, Actor::Agent, TxKind::Install, "2026-10-14T21:00:00Z");
        sys::append_durable(&f.paths.history_file(), json(&one).as_bytes()).unwrap();
        let before = read(&f.paths, &HistoryFilter::default()).unwrap();
        assert!(before.entries.is_empty() && before.torn_tail);

        let mut s = crate::fsx::WriteSession::untracked(Writer::Mutating, &f.scopes, "test");
        close_torn_tail(&mut s, &f.paths).unwrap();
        let after = read(&f.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(txs(&after), [one.tx.to_string()]);
        assert!(!after.torn_tail);
        let len = std::fs::metadata(f.paths.history_file()).unwrap().len();
        close_torn_tail(&mut s, &f.paths).unwrap();
        assert_eq!(
            std::fs::metadata(f.paths.history_file()).unwrap().len(),
            len,
            "a terminated file is left alone"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_failed_append_leaves_a_readable_log() {
        let f = fixture();
        append_all(
            &f,
            &[entry(
                1,
                Actor::Agent,
                TxKind::Install,
                "2026-10-14T21:00:00Z",
            )],
        );
        let mut s = crate::fsx::WriteSession::untracked(Writer::Mutating, &f.scopes, "test");
        let e = entry(2, Actor::Agent, TxKind::Install, "2026-10-14T22:00:00Z");
        failpoint::with_spec("fsx.append_durable.before=error", || {
            append(&mut s, &f.paths, &e)
        })
        .unwrap_err();
        let read = read(&f.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(
            (read.entries.len(), read.torn_tail, read.skipped_lines),
            (1, false, 0)
        );
    }

    #[test]
    fn filters_select_by_target_actor_kind_since_and_limit() {
        let f = fixture();
        append_all(
            &f,
            &[
                entry(1, Actor::Agent, TxKind::Install, "2026-10-12T10:00:00Z"),
                entry(2, Actor::Human, TxKind::Install, "2026-10-14T10:00:00Z"),
                entry(3, Actor::Agent, TxKind::Update, "2026-10-14T11:00:00Z"),
                entry(4, Actor::Agent, TxKind::Remove, "2026-10-14T12:00:00Z"),
            ],
        );
        let tx = |e: &HistoryEntry| e.tx.to_string().chars().last().unwrap();
        let pick = |filter: HistoryFilter| -> String {
            read(&f.paths, &filter)
                .unwrap()
                .entries
                .iter()
                .map(tx)
                .collect()
        };
        let since: Timestamp = "2026-10-13T12:00:00Z".parse().unwrap();
        assert_eq!(
            pick(HistoryFilter {
                actor: Some(Actor::Agent),
                since: Some(since),
                ..HistoryFilter::default()
            }),
            "34"
        );
        assert_eq!(
            pick(HistoryFilter {
                limit: Some(2),
                ..HistoryFilter::default()
            }),
            "34"
        );
        assert_eq!(
            pick(HistoryFilter {
                kind: Some(TxKind::Install),
                ..HistoryFilter::default()
            }),
            "12"
        );
        let other: TargetKey = "01JB8Q2V9X0000000000000000".parse().unwrap();
        assert_eq!(
            pick(HistoryFilter {
                target: Some(other),
                ..HistoryFilter::default()
            }),
            ""
        );
        let last_agent = last(
            &f.paths,
            &HistoryFilter {
                actor: Some(Actor::Agent),
                ..HistoryFilter::default()
            },
        )
        .unwrap();
        assert_eq!(last_agent.map(|e| tx(&e)), Some('4'));
        assert_eq!(
            read(&f.paths, &HistoryFilter::default())
                .unwrap()
                .entries
                .len(),
            4
        );
    }

    #[test]
    fn a_missing_log_reads_empty() {
        let f = fixture();
        assert_eq!(
            read(&f.paths, &HistoryFilter::default()).unwrap(),
            HistoryRead::default()
        );
        assert_eq!(last(&f.paths, &HistoryFilter::default()).unwrap(), None);
    }

    /// Settings changes share the log with transactions: written by their command's own
    /// writer, read back apart (with or without a target), never among `entries`, and
    /// filtered the same way; a `null` target never matches `-t`.
    #[test]
    fn settings_changes_are_lines_of_their_own() {
        let f = fixture();
        let mut s = WriteSession::untracked(Writer::Mutating, &f.scopes, "test");
        append(
            &mut s,
            &f.paths,
            &entry(1, Actor::Human, TxKind::Install, "2026-10-15T10:00:00Z"),
        )
        .unwrap();
        let argv = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect();
        let config = SettingEntry::now(
            TxKind::Config,
            None,
            Actor::Agent,
            Some("x".into()),
            argv(&["omaddons", "config", "set", "default_channel", "beta"]),
        );
        // `config set` writes as `Config`, which may append to history and nothing else
        // in state.
        let mut as_config = WriteSession::untracked(Writer::Config, &f.scopes, "config.toml");
        assert_eq!(record_setting(&mut as_config, &f.paths, &config), None);
        let refused = mutate(
            &mut as_config,
            &f.paths.registry_file(),
            Op::WriteFile {
                content: crate::fsx::Content::Bytes(b"{}"),
                mode: None,
            },
        );
        assert!(refused.is_err(), "Config may not write the registry");
        let key: TargetKey = TARGET.parse().unwrap();
        let target = SettingEntry::now(
            TxKind::Target,
            Some(key),
            Actor::Human,
            None,
            argv(&["omaddons", "targets", "default", "retail"]),
        );
        assert_eq!(record_setting(&mut s, &f.paths, &target), None);

        let text = std::fs::read_to_string(f.paths.history_file()).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[1]["kind"], "config");
        assert_eq!(lines[1]["target"], serde_json::Value::Null);
        assert_eq!(lines[1]["ops"], serde_json::json!([]));
        assert_eq!(lines[1]["result"], "committed");
        assert!(
            text.starts_with(LINE_START),
            "every line starts the same way"
        );

        let all = read(&f.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(all.entries.len(), 1, "undo never sees a settings change");
        assert_eq!(all.settings, [config.clone(), target.clone()]);
        assert_eq!(all.skipped_lines, 0);
        let on_target = HistoryFilter {
            target: Some(key),
            ..HistoryFilter::default()
        };
        assert_eq!(read(&f.paths, &on_target).unwrap().settings, [target]);
        let by_agent = HistoryFilter {
            actor: Some(Actor::Agent),
            ..HistoryFilter::default()
        };
        assert_eq!(read(&f.paths, &by_agent).unwrap().settings, [config]);
        for kind in [
            TxKind::Config,
            TxKind::Target,
            TxKind::Auth,
            TxKind::Export,
            TxKind::Dismiss,
        ] {
            assert!(kind.is_setting() && !kind.is_undoable(), "{kind:?}");
        }
        for kind in [TxKind::Install, TxKind::Undo, TxKind::Pin, TxKind::Sync] {
            assert!(!kind.is_setting() && kind.is_undoable(), "{kind:?}");
        }
        assert!(!TxKind::Backup.is_undoable() && !TxKind::Backup.is_setting());
    }

    // -----------------------------------------------------------------------------------
    // Target-less lines (M4-D31).

    const KEY_A: &str = "01JB8Q2V9W0000000000000000";
    const KEY_B: &str = "01JB8Q2V9W0000000000000001";
    const KEY_C: &str = "01JB8Q2V9W0000000000000002";

    fn key(text: &str) -> TargetKey {
        text.parse().unwrap()
    }

    fn file(kind: PredicateKind, mode: &str) -> RepairPredicate {
        RepairPredicate {
            kind,
            identity: Some(RepairIdentity {
                ino: "4215".into(),
                born_ns: "1792900000000000000".into(),
            }),
            sha256: None,
            mode: Some(mode.into()),
            stamp: None,
            link_target: None,
        }
    }

    fn absent() -> RepairPredicate {
        RepairPredicate {
            kind: PredicateKind::Absent,
            identity: None,
            sha256: None,
            mode: None,
            stamp: None,
            link_target: None,
        }
    }

    /// The plan's chmod of a credential file: home-side, no client.
    fn chmod() -> RepairEntry {
        RepairEntry {
            seq: 1,
            fix: 1,
            fm: "fm-secrets-credential-file-mode".into(),
            op: RepairOp::Chmod,
            path: "/home/u/.config/omaddons/credentials/github".into(),
            to: None,
            mode: None,
            pre: RepairEndpoints::Path(file(PredicateKind::File, "0644")),
            post: RepairEndpoints::Path(file(PredicateKind::File, "0600")),
            backup: None,
            activation: false,
            target: None,
            folder: None,
        }
    }

    /// The plan's flatten of a nested addon folder: a rename inside a client's AddOns.
    fn flatten() -> RepairEntry {
        let mut dir = file(PredicateKind::Dir, "0755");
        dir.mode = None;
        dir.stamp = Some("stamp:sha256:ab".into());
        let mut moved = dir.clone();
        moved.stamp = None;
        RepairEntry {
            seq: 1,
            fix: 1,
            fm: "fm-external_artifacts-nested-addon-folder".into(),
            op: RepairOp::RenameNoReplace,
            path: "/g/_retail_/Interface/AddOns/X/Y".into(),
            to: Some("/g/_retail_/Interface/AddOns/Y".into()),
            mode: None,
            pre: RepairEndpoints::Rename {
                path: dir,
                to: absent(),
            },
            post: RepairEndpoints::Rename {
                path: absent(),
                to: moved,
            },
            backup: None,
            activation: false,
            target: None,
            folder: None,
        }
    }

    /// A repair line: target-less, touching `targets`, with `repairs`.
    fn repair(n: u8, targets: &[&str], repairs: Vec<RepairEntry>) -> HistoryEntry {
        let mut e = entry(n, Actor::Human, TxKind::Repair, "2026-10-25T12:00:00Z");
        e.target = None;
        e.targets = targets.iter().map(|t| key(t)).collect();
        e.ops.clear();
        e.argv = vec!["omaddons".into(), "doctor".into(), "fix".into()];
        e.run = Some(RunId::new());
        e.doctor_run_id = Some("2026-10-25T12:00:00Z__01M9Z00000000000000000000".into());
        e.repairs = repairs;
        e
    }

    #[test]
    fn a_line_with_a_target_is_written_exactly_as_before() {
        let mut e = entry(1, Actor::Agent, TxKind::Install, "2026-10-14T21:00:00Z");
        e.reason = Some("keep raid addons current".into());
        e.run = Some("01JB8Q2VB00000000000000000".parse().unwrap());
        let line = serde_json::to_string(&e).unwrap();
        assert_eq!(
            line,
            concat!(
                r#"{"schema_version":"1.0","tx":"01JB8Q2VA00000000000000001","kind":"install","#,
                r#""at":"2026-10-14T21:00:00Z","target":"01JB8Q2V9W0000000000000000","#,
                r#""actor":"agent","reason":"keep raid addons current","#,
                r#""argv":["omaddons","install","weakauras"],"#,
                r#""ops":[{"op":"add","folder":"WeakAuras","addon":"WeakAuras"}],"#,
                r#""result":"committed","reverts":null,"run":"01JB8Q2VB00000000000000000"}"#
            )
        );
        for absent in ["targets", "repairs", "doctor_run_id"] {
            assert!(!line.contains(absent), "{absent} in {line}");
        }
    }

    #[test]
    fn a_repair_line_round_trips_in_the_documented_shape() {
        assert_eq!(
            serde_json::to_string(&chmod()).unwrap(),
            concat!(
                r#"{"seq":1,"fix":1,"fm":"fm-secrets-credential-file-mode","op":"chmod","#,
                r#""path":"/home/u/.config/omaddons/credentials/github","#,
                r#""pre":{"kind":"file","identity":{"ino":"4215","born_ns":"1792900000000000000"},"mode":"0644"},"#,
                r#""post":{"kind":"file","identity":{"ino":"4215","born_ns":"1792900000000000000"},"mode":"0600"}}"#
            )
        );
        assert_eq!(
            serde_json::to_string(&flatten()).unwrap(),
            concat!(
                r#"{"seq":1,"fix":1,"fm":"fm-external_artifacts-nested-addon-folder","op":"rename_noreplace","#,
                r#""path":"/g/_retail_/Interface/AddOns/X/Y","to":"/g/_retail_/Interface/AddOns/Y","#,
                r#""pre":{"path":{"kind":"dir","identity":{"ino":"4215","born_ns":"1792900000000000000"},"stamp":"stamp:sha256:ab"},"to":{"kind":"absent"}},"#,
                r#""post":{"path":{"kind":"absent"},"to":{"kind":"dir","identity":{"ino":"4215","born_ns":"1792900000000000000"}}}}"#
            )
        );
        let line = repair(2, &[KEY_A], vec![chmod(), flatten()]);
        let json = serde_json::to_string(&line).unwrap();
        assert!(json.contains(r#""target":null,"targets":["#), "{json}");
        assert!(
            json.contains(
                r#""doctor_run_id":"2026-10-25T12:00:00Z__01M9Z00000000000000000000","repairs":[{"#
            ),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<HistoryEntry>(&json).unwrap(), line);
    }

    #[test]
    fn repair_ops_and_predicate_kinds_use_their_wire_names() {
        for (op, wire) in [
            (RepairOp::WriteFile, "\"write_file\""),
            (RepairOp::Chmod, "\"chmod\""),
            (RepairOp::RenameNoReplace, "\"rename_noreplace\""),
        ] {
            let json = serde_json::to_string(&op).unwrap();
            assert_eq!(json, wire, "{op:?}");
            assert_eq!(serde_json::from_str::<RepairOp>(&json).unwrap(), op);
        }
        for (kind, wire) in [
            (PredicateKind::Absent, "\"absent\""),
            (PredicateKind::File, "\"file\""),
            (PredicateKind::Dir, "\"dir\""),
            (PredicateKind::Symlink, "\"symlink\""),
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, wire, "{kind:?}");
            assert_eq!(serde_json::from_str::<PredicateKind>(&json).unwrap(), kind);
        }
    }

    /// `line` with its JSON edited by `edit`.
    fn edited(line: &HistoryEntry, edit: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut value = serde_json::to_value(line).unwrap();
        edit(&mut value);
        serde_json::to_string(&value).unwrap()
    }

    #[test]
    fn target_less_lines_read_only_as_repairs_or_their_undos() {
        let good = repair(1, &[KEY_A], vec![chmod()]);
        let mut undo = repair(2, &[KEY_A], vec![chmod()]);
        undo.kind = TxKind::Undo;
        undo.reverts = Some(good.tx);
        let mut undo_bare = undo.clone();
        undo_bare.repairs.clear();
        let mut install = entry(3, Actor::Human, TxKind::Install, "2026-10-25T12:00:00Z");
        install.target = None;
        install.ops.clear();
        let with_ops = HistoryEntry {
            ops: entry(4, Actor::Human, TxKind::Install, "2026-10-25T12:00:00Z").ops,
            ..good.clone()
        };
        let json = |e: &HistoryEntry| serde_json::to_string(e).unwrap();
        /// What a read counted: entries, skipped, torn, unreadable, a torn tail.
        type Counts = (usize, usize, usize, usize, bool);
        let cases: Vec<(&str, String, Counts)> = vec![
            (
                "repair_with_primitives",
                json(&good) + "\n",
                (1, 0, 0, 0, false),
            ),
            (
                "repair_without_primitives",
                json(&repair(5, &[], Vec::new())) + "\n",
                (1, 0, 0, 0, false),
            ),
            (
                "undo_with_inverse_primitives",
                json(&undo) + "\n",
                (1, 0, 0, 0, false),
            ),
            (
                "undo_without_primitives",
                json(&undo_bare) + "\n",
                (0, 1, 0, 1, false),
            ),
            (
                "install_without_target",
                json(&install) + "\n",
                (0, 1, 0, 1, false),
            ),
            (
                "repair_with_folder_ops",
                json(&with_ops) + "\n",
                (0, 1, 0, 1, false),
            ),
            (
                "repair_with_bad_folder",
                edited(&good, |v| v["repairs"][0]["folder"] = "../Escape".into()) + "\n",
                (0, 1, 0, 1, false),
            ),
            (
                "repair_with_unknown_op",
                edited(&good, |v| v["repairs"][0]["op"] = "symlink".into()) + "\n",
                (0, 1, 0, 1, false),
            ),
            (
                "missing_target_key",
                edited(&good, |v| {
                    v.as_object_mut().unwrap().remove("target");
                }) + "\n",
                (1, 0, 0, 0, false),
            ),
            ("repair_cut_short", json(&good), (0, 0, 0, 0, true)),
        ];
        for (name, line, expected) in cases {
            let f = fixture();
            sys::append_durable(&f.paths.history_file(), line.as_bytes()).unwrap();
            let read = read(&f.paths, &HistoryFilter::default()).unwrap();
            let output = (
                read.entries.len(),
                read.skipped_lines,
                read.torn_lines,
                read.unreadable.len(),
                read.torn_tail,
            );
            assert_eq!(output, expected, "{name}: input {line} gave {output:?}");
            if name == "missing_target_key" {
                assert_eq!(read.entries[0].target, None, "{name}");
            }
        }
    }

    /// A repair line cut short right after its `null` target, then a complete line: a
    /// torn line, as a cut settings line is, never an unreadable one.
    #[test]
    fn a_cut_short_repair_line_is_torn() {
        let f = fixture();
        let line = serde_json::to_string(&repair(1, &[KEY_A], vec![chmod()])).unwrap();
        let cut = line.find("\"target\":null,").unwrap() + "\"target\":null,".len();
        sys::append_durable(&f.paths.history_file(), &line.as_bytes()[..cut]).unwrap();
        let one = entry(2, Actor::Agent, TxKind::Install, "2026-10-25T13:00:00Z");
        append_all(&f, std::slice::from_ref(&one));

        let read = read(&f.paths, &HistoryFilter::default()).unwrap();
        assert_eq!(txs(&read), [one.tx.to_string()]);
        assert_eq!((read.torn_lines, read.skipped_lines), (1, 0));
    }

    /// A home-side repair operation at `path`, naming `target` and `folder` as given.
    fn op_at(path: &str, target: Option<&str>, folder: Option<&str>) -> RepairEntry {
        RepairEntry {
            path: path.into(),
            target: target.map(key),
            folder: folder.map(|f| FolderName::new(f).unwrap()),
            ..chmod()
        }
    }

    fn keys(list: &[&str]) -> Reach {
        Reach::Targets(list.iter().map(|k| key(k)).collect())
    }

    #[test]
    fn reach_follows_targets_and_fails_closed() {
        let home = "/home/u/.config/omaddons/credentials/github";
        let with_target = entry(1, Actor::Human, TxKind::Install, "2026-10-25T12:00:00Z");
        let mut with_extra = with_target.clone();
        with_extra.targets = vec![key(KEY_B)];
        let cases = [
            ("with_target", with_target, keys(&[KEY_A])),
            (
                "with_target_and_extra_targets",
                with_extra,
                keys(&[KEY_A, KEY_B]),
            ),
            (
                "targets_only",
                repair(2, &[KEY_A, KEY_B], vec![op_at(home, None, None)]),
                keys(&[KEY_A, KEY_B]),
            ),
            (
                "home_side_only",
                repair(3, &[], vec![op_at(home, None, None)]),
                keys(&[]),
            ),
            (
                "entry_with_target_no_targets",
                repair(4, &[], vec![op_at(home, Some(KEY_A), None)]),
                Reach::Every,
            ),
            (
                "entry_with_folder_no_targets",
                repair(5, &[], vec![op_at(home, None, Some("Foo"))]),
                Reach::Every,
            ),
            (
                "path_under_addons_no_targets",
                repair(
                    6,
                    &[],
                    vec![op_at("/g/_retail_/Interface/AddOns/Foo/x.lua", None, None)],
                ),
                Reach::Every,
            ),
            (
                "path_under_meta_no_targets",
                repair(
                    7,
                    &[],
                    vec![op_at(
                        "/g/_retail_/Interface/.omaddons/journal/x",
                        None,
                        None,
                    )],
                ),
                Reach::Every,
            ),
            (
                "addons_elsewhere_in_path",
                repair(8, &[], vec![op_at("/home/u/AddOns/Foo", None, None)]),
                keys(&[]),
            ),
            (
                "targets_and_an_unnamed_client_entry",
                repair(
                    9,
                    &[KEY_A],
                    vec![op_at("/g/_retail_/Interface/AddOns/Foo", None, None)],
                ),
                keys(&[KEY_A]),
            ),
            (
                "targets_and_entry_on_another_target",
                repair(1, &[KEY_A], vec![op_at(home, Some(KEY_B), None)]),
                keys(&[KEY_A, KEY_B]),
            ),
        ];
        for (name, line, expected) in cases {
            let output = line.reach();
            assert_eq!(output, expected, "{name}: input {line:?} gave {output:?}");
            for k in [KEY_A, KEY_B, KEY_C] {
                let in_reach = match &output {
                    Reach::Every => true,
                    Reach::Targets(set) => set.contains(&key(k)),
                };
                assert_eq!(
                    line.touches_target(key(k)),
                    in_reach,
                    "{name}: touches_target({k}) disagrees with {output:?}"
                );
            }
        }
    }

    #[test]
    fn addon_folders_come_from_the_field_and_the_paths() {
        let renamed = RepairEntry {
            to: Some("/g/_retail_/Interface/AddOns/Baz".into()),
            ..op_at("/g/_retail_/Interface/AddOns/X/Y", None, None)
        };
        let cases = [
            (
                "folder_only",
                op_at("/home/u/x", None, Some("Qux")),
                vec!["Qux"],
            ),
            (
                "path",
                op_at("/g/_retail_/Interface/AddOns/Foo/Bar.lua", None, None),
                vec!["Foo"],
            ),
            ("rename_to", renamed, vec!["Baz", "X"]),
            (
                "case",
                op_at("/g/_retail_/interface/addons/Foo", None, None),
                vec!["Foo"],
            ),
            (
                "flatten_stage",
                op_at(
                    "/g/_retail_/Interface/AddOns/.omaddons-flatten-01M9Z00000000000000000000",
                    None,
                    None,
                ),
                vec![],
            ),
            (
                "meta",
                op_at(
                    "/g/_retail_/Interface/.omaddons/quarantine/x/AddOns/X",
                    None,
                    None,
                ),
                vec![],
            ),
        ];
        for (name, entry, expected) in cases {
            let output: Vec<String> = entry
                .addon_folders()
                .iter()
                .map(|f| f.as_str().to_owned())
                .collect();
            assert_eq!(output, expected, "{name}: input {entry:?} gave {output:?}");
        }
    }

    #[test]
    fn filters_select_a_repair_line_on_each_target_it_touches() {
        let f = fixture();
        let install = entry(1, Actor::Human, TxKind::Install, "2026-10-25T11:00:00Z");
        let on_both = repair(2, &[KEY_A, KEY_B], vec![chmod()]);
        let home_side = repair(3, &[], vec![chmod()]);
        let every = repair(
            4,
            &[],
            vec![op_at("/g/_retail_/Interface/AddOns/Foo", None, None)],
        );
        append_all(
            &f,
            &[
                install.clone(),
                on_both.clone(),
                home_side.clone(),
                every.clone(),
            ],
        );
        let tx = |e: &HistoryEntry| e.tx.to_string();
        let cases = [
            (
                "all",
                None,
                vec![tx(&install), tx(&on_both), tx(&home_side), tx(&every)],
            ),
            (
                "on_a",
                Some(KEY_A),
                vec![tx(&install), tx(&on_both), tx(&every)],
            ),
            ("on_b", Some(KEY_B), vec![tx(&on_both), tx(&every)]),
            ("on_c", Some(KEY_C), vec![tx(&every)]),
        ];
        for (name, target, expected) in cases {
            let filter = HistoryFilter {
                target: target.map(key),
                ..HistoryFilter::default()
            };
            let output = txs(&read(&f.paths, &filter).unwrap());
            assert_eq!(
                output, expected,
                "{name}: target {target:?} gave {output:?}"
            );
        }
    }

    #[test]
    fn the_newest_run_may_be_a_repair() {
        let mut on_c = entry(1, Actor::Human, TxKind::Install, "2026-10-25T11:00:00Z");
        on_c.target = Some(key(KEY_C));
        let fix = repair(2, &[KEY_A], vec![chmod()]);
        let entries = vec![on_c.clone(), fix.clone()];
        let txs_of = |found: Option<RunSelection<'_>>| found.map(|f| selected(&f));
        assert_eq!(
            txs_of(newest_run(&entries, None, None)),
            Some(vec![fix.tx.to_string()]),
            "bare: the repair is the newest change"
        );
        assert_eq!(
            txs_of(newest_run(&entries, None, Some(Some(key(KEY_A))))),
            Some(vec![fix.tx.to_string()]),
            "-t A: the repair touched A"
        );
        assert_eq!(
            txs_of(newest_run(&entries, None, Some(Some(key(KEY_C))))),
            Some(vec![on_c.tx.to_string()]),
            "-t C: the older install"
        );
        assert_eq!(
            txs_of(newest_run(&entries, None, Some(None))),
            None,
            "never registered"
        );
    }

    /// The line format of `c0ab5e9`, before M4: a required target.
    #[derive(Debug, Deserialize)]
    #[allow(dead_code, reason = "fields are read only by serde")]
    struct M3Entry {
        schema_version: SchemaVersion,
        tx: TxId,
        kind: TxKind,
        at: Timestamp,
        target: TargetKey,
        actor: Actor,
        #[serde(default)]
        reason: Option<String>,
        argv: Vec<String>,
        ops: Vec<HistoryOp>,
        result: TxResult,
        #[serde(default)]
        reverts: Option<TxId>,
    }

    #[test]
    fn a_build_with_a_required_target_cannot_read_a_repair_line() {
        let line = serde_json::to_string(&repair(1, &[KEY_A], vec![chmod()])).unwrap();
        let error = serde_json::from_str::<M3Entry>(&line).unwrap_err();
        assert_eq!(
            error.classify(),
            serde_json::error::Category::Data,
            "{error}"
        );
        let facts = line_facts(&line).unwrap();
        assert!(
            facts.undoable,
            "the projection counts it as a change to undo"
        );
    }

    #[test]
    fn no_reader_unwraps_or_expects_a_history_target() {
        use crate::source_scan::{Literals, production_code, sources};

        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let files = sources(&src);
        assert!(files.len() >= 100, "scanned {} files", files.len());
        let mut found = Vec::new();
        for (path, text) in files {
            let code = production_code(&text, Literals::Blank).unwrap_or_default();
            for (n, line) in code.lines().enumerate() {
                for pattern in [
                    ".target.unwrap()",
                    ".target.expect(",
                    ".target.as_ref().unwrap()",
                    ".target.as_ref().expect(",
                ] {
                    if line.contains(pattern) {
                        found.push(format!("{}:{}: {pattern}", path.display(), n + 1));
                    }
                }
            }
        }
        assert!(
            found.is_empty(),
            "a history target may be null (a repair): read it through `undo::target_of` or `touches_target`\n{}",
            found.join("\n")
        );
    }

    mod generated {
        use proptest::prelude::*;

        use super::*;

        fn any_key() -> impl Strategy<Value = TargetKey> {
            prop_oneof![Just(key(KEY_A)), Just(key(KEY_B)), Just(key(KEY_C))]
        }

        fn any_repair() -> impl Strategy<Value = RepairEntry> {
            (
                proptest::option::of(any_key()),
                proptest::option::of(Just("Foo")),
                prop_oneof![
                    Just("/home/u/.config/omaddons/credentials/github"),
                    Just("/g/_retail_/Interface/AddOns/Foo/x.lua"),
                ],
            )
                .prop_map(|(target, folder, path)| RepairEntry {
                    target,
                    folder: folder.map(|f| FolderName::new(f).unwrap()),
                    ..op_at(path, None, None)
                })
        }

        fn any_line() -> impl Strategy<Value = HistoryEntry> {
            (
                proptest::option::of(any_key()),
                proptest::collection::vec(any_key(), 0..=3),
                proptest::collection::vec(any_repair(), 0..=4),
            )
                .prop_map(|(target, targets, repairs)| HistoryEntry {
                    target,
                    targets,
                    ops: Vec::new(),
                    ..repair(1, &[], repairs)
                })
        }

        proptest! {
            #[test]
            fn generated_lines_round_trip_and_reach_names_every_target(line in any_line()) {
                prop_assume!(line.well_formed());
                let json = serde_json::to_string(&line).unwrap();
                prop_assert_eq!(&serde_json::from_str::<HistoryEntry>(&json).unwrap(), &line);
                if let Reach::Targets(reached) = line.reach() {
                    let named = line
                        .target
                        .iter()
                        .chain(&line.targets)
                        .chain(line.repairs.iter().filter_map(|r| r.target.as_ref()));
                    for k in named {
                        prop_assert!(reached.contains(k), "{k} missing from {reached:?}: {json}");
                    }
                }
            }
        }
    }
}
