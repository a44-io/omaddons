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
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{Op, WriteSession, mutate};
use crate::limits::REASON_MAX_CHARS;
use crate::model::{Actor, FolderName, RunId, TargetKey, TxId, TxKind};

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
}

impl HistoryEntry {
    /// The run this transaction belongs to: its own `run`, or the transaction alone.
    pub fn run_key(&self) -> RunKey {
        self.run.map_or(RunKey::Single(self.tx), RunKey::Run)
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
    if let Some(reason) = &entry.reason
        && reason.chars().count() > REASON_MAX_CHARS
    {
        return Err(Error::new(
            ErrorCode::Usage,
            format!("--reason is longer than {REASON_MAX_CHARS} characters"),
            Suggestion::text("shorten --reason"),
        ));
    }
    let path = paths.history_file();
    let mut line = Vec::new();
    if unterminated(&path) {
        line.push(b'\n');
    }
    serde_json::to_writer(&mut line, entry)
        .map_err(|err| Error::internal(format!("serializing a history line: {err}")))?;
    line.push(b'\n');
    mutate(s, &path, Op::AppendFile { content: &line })?;
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
}

/// The matching entries, oldest first, and what was skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryRead {
    pub entries: Vec<HistoryEntry>,
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
    /// The JSON ends early: a write a crash cut short.
    Torn,
    /// Anything else that does not read.
    Unreadable,
}

/// Reads one complete line. An older omaddons appended right after a cut-short line,
/// merging the two (`<fragment><line>`); when the whole does not read, the part from
/// the last line start on is tried, which salvages the line appended after it.
fn read_line(line: &str) -> Line {
    let error = match serde_json::from_str::<HistoryEntry>(line) {
        Ok(entry) => return Line::Entry(Box::new(entry)),
        Err(error) => error,
    };
    if let Some(at) = line.rfind(LINE_START).filter(|at| *at > 0)
        && let Ok(entry) = serde_json::from_str::<HistoryEntry>(&line[at..])
    {
        return Line::Entry(Box::new(entry));
    }
    match error.classify() {
        serde_json::error::Category::Eof => Line::Torn,
        _ => Line::Unreadable,
    }
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
        }
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
}
