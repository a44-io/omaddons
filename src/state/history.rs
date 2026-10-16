//! `history.jsonl`: the append-only transaction log (vision §5.6, D31). One JSON line
//! per transaction records who did it, why, the exact argv, and every folder touched,
//! which is what undo-conflict detection reads.
//!
//! Each line carries its own `schema_version` as its first key. A crash mid-append
//! leaves at most a torn last line; the reader skips it and never fails on a bad line.

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::paths::Paths;
use super::store::SchemaVersion;
use crate::error::{Error, ErrorCode, Result, Suggestion};
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
/// inside the commit's transaction session while the global lock is held.
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
    let mut line = serde_json::to_vec(entry)
        .map_err(|err| Error::internal(format!("serializing a history line: {err}")))?;
    line.push(b'\n');
    mutate(s, &paths.history_file(), Op::AppendFile { content: &line })?;
    Ok(())
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
    /// The last line was cut short by a crash (or does not parse) and was skipped.
    pub torn_tail: bool,
    /// Other lines that do not parse.
    pub skipped_lines: usize,
}

/// Reads the history, skipping bad lines rather than failing.
pub fn read(paths: &Paths, filter: &HistoryFilter) -> Result<HistoryRead> {
    let path = paths.history_file();
    let text = match std::fs::read(&path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(HistoryRead::default()),
        Err(err) => {
            return Err(Error::io(
                &path,
                &err,
                Suggestion::command(["omaddons", "status"]),
            ));
        }
    };
    let mut read = HistoryRead::default();
    let complete = text.ends_with('\n');
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let last = lines.len().saturating_sub(1);
    for (index, line) in lines.iter().enumerate() {
        let is_last = index == last;
        match serde_json::from_str::<HistoryEntry>(line) {
            Ok(entry) if !(is_last && !complete) => {
                if filter.matches(&entry) {
                    read.entries.push(entry);
                }
            }
            _ if is_last => read.torn_tail = true,
            _ => read.skipped_lines += 1,
        }
    }
    if let Some(limit) = filter.limit {
        let excess = read.entries.len().saturating_sub(limit);
        read.entries.drain(..excess);
    }
    Ok(read)
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
            }],
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            created_addons_dir: false,
            run: None,
        }
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
