//! `history.jsonl`: the append-only transaction log (vision §5.6, D31). One JSON line
//! per transaction records who did it, why, the exact argv, and every folder touched,
//! which is what undo-conflict detection reads.
//!
//! Each line carries its own `schema_version` as its first key. A crash mid-append
//! leaves at most a torn last line; the reader skips it and never fails on a bad line.
//! The next write closes it off first, so a new line never merges with it, and the
//! reader salvages lines an older omaddons merged. Lines that still do not read are
//! counted, and `history` and `undo` warn about them.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::paths::Paths;
use super::store::SchemaVersion;
use crate::backup::BackupKind;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{Op, WriteSession, ensure_dir_all, mutate};
use crate::limits::REASON_MAX_CHARS;
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
    pub target: TargetKey,
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
    pub folder: FolderName,
    pub addon: String,
    /// A `record` op that only changed who requires the addon: undone in place, without
    /// a conflict or a snapshot restore (M2-D35). Absent on every other op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<HistoryEdit>,
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
/// ownership transfer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FolderOp {
    Add,
    Replace,
    Remove,
    Record,
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
        self.target.is_none_or(|t| t == entry.target)
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
    /// Other lines that do not read (a hand edit, damage): the transactions on them are
    /// lost to `history` and `undo`.
    pub skipped_lines: usize,
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
                "a line was damaged by a crash or a hand edit; omaddons never rewrites history.jsonl",
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
            Line::Unreadable => read.skipped_lines += 1,
        }
    }
    if let Some(limit) = filter.limit {
        let excess = read.entries.len().saturating_sub(limit);
        read.entries.drain(..excess);
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
/// a transaction.
fn whole(text: &str) -> std::result::Result<Line, serde_json::Error> {
    if let Ok(setting) = serde_json::from_str::<SettingEntry>(text)
        && setting.kind.is_setting()
    {
        return Ok(Line::Setting(Box::new(setting)));
    }
    serde_json::from_str::<HistoryEntry>(text).map(|entry| Line::Entry(Box::new(entry)))
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
    let on_target = |e: &HistoryEntry| target.is_none_or(|key| Some(e.target) == key);
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
            target: TARGET.parse().unwrap(),
            actor,
            reason: None,
            argv: vec!["omaddons".into(), "install".into(), "weakauras".into()],
            ops: vec![HistoryOp {
                op: FolderOp::Add,
                folder: FolderName::new("WeakAuras").unwrap(),
                addon: "WeakAuras".into(),
                edit: None,
            }],
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            created_addons_dir: false,
            run: None,
            backup: None,
        }
    }

    /// A transaction of `run` on `target` (`None`: a line from before M2).
    fn of_run(n: u8, kind: TxKind, run: Option<RunId>, target: &str) -> HistoryEntry {
        HistoryEntry {
            run,
            target: target.parse().unwrap(),
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
    fn in_place_edits_ride_on_record_ops() {
        let mut op = HistoryOp {
            op: FolderOp::Record,
            folder: FolderName::new("BigWigs").unwrap(),
            addon: "BigWigs".into(),
            edit: Some(HistoryEdit::RequiredByAdd("LittleWigs".into())),
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
        for kind in [TxKind::Config, TxKind::Target, TxKind::Auth, TxKind::Export] {
            assert!(kind.is_setting() && !kind.is_undoable(), "{kind:?}");
        }
        for kind in [TxKind::Install, TxKind::Undo, TxKind::Pin, TxKind::Sync] {
            assert!(!kind.is_setting() && kind.is_undoable(), "{kind:?}");
        }
        assert!(!TxKind::Backup.is_undoable() && !TxKind::Backup.is_setting());
    }
}
