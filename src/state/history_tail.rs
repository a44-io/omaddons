//! `<cache>/omaddons/history_tail.json`, the history projection (M3-D37): the end of
//! `history.jsonl`, from the first line of the oldest of its newest [`HISTORY_TAIL_RUNS`]
//! runs with an undoable transaction, verbatim and in file order, so that `status
//! --cached` learns what bare `undo` would revert without reading a history of any size.
//! Runs with nothing undoable (backups, settings changes) are kept when they fall inside
//! but never count, so they cannot push the last undoable run out. Past
//! [`HISTORY_TAIL_MAX_BYTES`] the oldest lines go.
//!
//! Lines from concurrent runs interleave in the history, so a cut may fall inside a run:
//! `partial` names every run with lines before the cut, and a selection landing on one
//! needs older lines ([`Answer::NeedsOlder`]), which `status --cached` reports as unknown,
//! never as a guess.
//!
//! It is derived, never trusted: it covers the history up to `covers.bytes`, and
//! [`fresh`] believes it only while `history.jsonl` is exactly that long and ends with its
//! last line. `history::append` keeps it current under the appender's lock ([`appended`]);
//! anything it cannot follow marks it stale (`covers.bytes: null`), and the command
//! rebuilds it from the whole history after releasing the lock ([`rebuild_if_stale`]).
//! Writes are best effort: a projection that cannot be written costs one "unknown".

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::history::{self, HistoryEntry, LineFacts, RunKey, line_facts};
use super::paths::Paths;
use super::store::{self, SchemaVersion, VersionStatus, Versioned};
use crate::error::Result;
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::limits::{HISTORY_TAIL_MAX_BYTES, HISTORY_TAIL_RUNS};
use crate::model::Actor;

/// The projection file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HistoryTail {
    pub covers: Covers,
    /// Runs with lines before `covers.from` (run ids): never selected from here.
    #[serde(default)]
    pub partial: Vec<String>,
    /// When the newest line before `covers.from` was written: a run started later has no
    /// line before it.
    #[serde(default)]
    #[schemars(with = "Option<String>")]
    pub dropped_at: Option<jiff::Timestamp>,
    /// Every line from `covers.from` on, without its newline, in file order.
    #[serde(default)]
    pub lines: Vec<String>,
}

impl Versioned for HistoryTail {
    const KIND: &'static str = "history_tail";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// What part of the history a projection holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Covers {
    /// The byte offset of its first line in `history.jsonl`.
    pub from: u64,
    /// The history's length it covers; `null` when it is stale.
    pub bytes: Option<u64>,
}

/// What bare `undo` would revert, as the projection can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Every transaction of the run, newest first (as [`history::newest_run`] gives them).
    Found(Vec<HistoryEntry>),
    /// Nothing undoable in what it holds: nothing at all when `complete` (it holds the
    /// whole history), else unknown.
    Nothing { complete: bool },
    /// The run selected has lines older than the projection.
    NeedsOlder,
}

impl HistoryTail {
    /// What bare `undo` would revert, from the lines held.
    pub fn answer(&self) -> Answer {
        let entries: Vec<HistoryEntry> =
            history::parse(&self.text(), &history::HistoryFilter::default()).entries;
        let Some(found) = history::newest_run(&entries, None, None) else {
            return Answer::Nothing {
                complete: self.covers.from == 0,
            };
        };
        let partial = found
            .run
            .is_some_and(|run| self.partial.contains(&run.to_string()));
        if partial {
            return Answer::NeedsOlder;
        }
        Answer::Found(found.entries.into_iter().cloned().collect())
    }

    /// The lines held, as history text.
    fn text(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            text.push_str(line);
            text.push('\n');
        }
        text
    }

    /// The history length it covers when complete: `from` plus every line and its newline.
    fn end(&self) -> u64 {
        self.covers.from + self.lines.iter().map(|l| l.len() as u64 + 1).sum::<u64>()
    }
}

/// The projection when it matches `history.jsonl` now: one `stat` of the history and,
/// when it holds lines, one read of its last line's length at the end.
pub fn fresh(paths: &Paths) -> Option<HistoryTail> {
    let tail = load(paths)?;
    let bytes = tail.covers.bytes?;
    let len = std::fs::metadata(paths.history_file()).map_or(0, |m| m.len());
    // ubs:ignore — comparing file lengths, not secrets
    (len == bytes && tail.end() == bytes && ends_with_last(paths, &tail, bytes)).then_some(tail)
}

/// Whether the history's bytes before `at` end with `tail`'s last line and a newline (an
/// empty projection holds nothing to compare).
fn ends_with_last(paths: &Paths, tail: &HistoryTail, at: u64) -> bool {
    let Some(last) = tail.lines.last() else {
        return true;
    };
    let len = last.len() as u64 + 1;
    let Some(start) = at.checked_sub(len) else {
        return false;
    };
    let Ok(mut file) = std::fs::File::open(paths.history_file()) else {
        return false;
    };
    let mut bytes = vec![0; usize::try_from(len).unwrap_or(usize::MAX)];
    file.seek(SeekFrom::Start(start)).is_ok()
        && file.read_exact(&mut bytes).is_ok()
        && bytes.strip_suffix(b"\n") == Some(last.as_bytes())
}

/// Follows one append to `history.jsonl` (`appended`: the bytes written at `before`,
/// one line and its newline, after a newline closing a cut-short line when there was
/// one), under the appender's lock. A projection that covered exactly the history before
/// takes the line and drops what it no longer needs; any other is marked stale. Best
/// effort, never the append's failure.
pub fn appended(paths: &Paths, before: u64, appended: &[u8]) {
    let Some(mut tail) = load(paths) else {
        // None yet, or unreadable: the command rebuilds it after its lock.
        return;
    };
    let line = appended
        .strip_suffix(b"\n")
        .filter(|l| !l.contains(&b'\n'))
        .and_then(|l| std::str::from_utf8(l).ok());
    // ubs:ignore — comparing file lengths, not secrets
    let covered = tail.covers.bytes == Some(before);
    // ubs:ignore — comparing file lengths, not secrets
    let follows = covered && tail.end() == before && ends_with_last(paths, &tail, before);
    match line {
        Some(line) if follows => {
            follow(&mut tail, line);
            tail.covers.bytes = Some(tail.end());
        }
        _ if tail.covers.bytes.is_none() => return,
        _ => tail.covers.bytes = None,
    }
    let _ = write(paths, &tail);
}

/// Adds `line` (appended at the projection's end) to `tail`, then [`trim`]s it, reading
/// each line held once. A run it has not seen that started before the newest line it
/// dropped may have lines before it, so it is partial (conservative: a false "partial"
/// costs one rebuild, never a wrong answer).
fn follow(tail: &mut HistoryTail, line: &str) {
    let mut facts = facts_of(&tail.lines);
    let new = line_facts(line);
    if let Some(RunKey::Run(run)) = new.map(|f| f.run) {
        let id = run.to_string();
        let seen = facts.iter().flatten().any(|f| f.run == RunKey::Run(run));
        // A line of it before the projection was written after the run started and no
        // later than `dropped_at`; a run commits nothing within its first millisecond.
        let older = tail.dropped_at.is_some_and(|at| {
            i128::from(run.ulid().timestamp_ms()) < i128::from(at.as_millisecond())
        });
        if !seen && older && !tail.partial.contains(&id) {
            tail.partial.push(id);
        }
    }
    tail.lines.push(line.to_owned());
    facts.push(new);
    trim(tail, &facts);
}

/// Every line's facts, in order.
fn facts_of(lines: &[String]) -> Vec<Option<LineFacts>> {
    lines.iter().map(|l| line_facts(l)).collect()
}

/// Drops `tail`'s lines (whose `facts` these are) before the first line of the oldest of
/// its newest [`HISTORY_TAIL_RUNS`] runs with an undoable transaction, then the oldest
/// lines while it holds more than [`HISTORY_TAIL_MAX_BYTES`]; a run cut there becomes
/// partial.
fn trim(tail: &mut HistoryTail, facts: &[Option<LineFacts>]) {
    let mut first: BTreeMap<RunKey, usize> = BTreeMap::new();
    for (i, f) in facts.iter().enumerate() {
        if let Some(f) = f {
            first.entry(f.run).or_insert(i);
        }
    }
    // The newest undoable runs, by their newest undoable line.
    let mut newest: Vec<RunKey> = Vec::new();
    for f in facts.iter().rev().flatten() {
        if f.undoable && !newest.contains(&f.run) {
            newest.push(f.run);
            if newest.len() == HISTORY_TAIL_RUNS {
                break;
            }
        }
    }
    let mut start = if newest.len() < HISTORY_TAIL_RUNS {
        0
    } else {
        newest
            .iter()
            .filter_map(|r| first.get(r))
            .copied()
            .min()
            .unwrap_or(0)
    };
    let mut held: u64 = tail.lines[start..].iter().map(|l| l.len() as u64 + 1).sum();
    while held > HISTORY_TAIL_MAX_BYTES && start < tail.lines.len() {
        held -= tail.lines[start].len() as u64 + 1;
        start += 1;
    }
    if start == 0 {
        return;
    }
    let dropped: BTreeSet<RunKey> = facts[..start].iter().flatten().map(|f| f.run).collect();
    let kept: BTreeSet<RunKey> = facts[start..].iter().flatten().map(|f| f.run).collect();
    let mut partial: BTreeSet<String> = tail
        .partial
        .iter()
        .filter(|id| {
            kept.iter()
                .any(|r| run_id(*r).as_deref() == Some(id.as_str()))
        })
        .cloned()
        .collect();
    partial.extend(dropped.intersection(&kept).filter_map(|r| run_id(*r)));
    let newest_dropped = facts[..start].iter().flatten().map(|f| f.at).max();
    tail.dropped_at = tail.dropped_at.max(newest_dropped);
    tail.covers.from += tail.lines[..start]
        .iter()
        .map(|l| l.len() as u64 + 1)
        .sum::<u64>();
    tail.lines.drain(..start);
    tail.partial = partial.into_iter().collect();
}

/// A run key's id; a transaction from before M2 is a run by itself and has none.
fn run_id(run: RunKey) -> Option<String> {
    match run {
        RunKey::Run(id) => Some(id.to_string()),
        RunKey::Single(_) => None,
    }
}

/// The projection built from the whole history (its complete lines).
pub fn build(paths: &Paths) -> Result<HistoryTail> {
    Ok(HistoryTail::from_text(&history::text(paths)?))
}

impl HistoryTail {
    /// The projection of a whole history's `text`.
    fn from_text(text: &str) -> Self {
        // A last line without its newline is not covered: the next append closes it off.
        let complete = text.rfind('\n').map_or(0, |at| at + 1);
        let mut tail = Self {
            lines: text[..complete].lines().map(str::to_owned).collect(),
            ..Self::default()
        };
        let facts = facts_of(&tail.lines);
        trim(&mut tail, &facts);
        tail.covers.bytes = Some(tail.end());
        tail
    }
}

/// Rebuilds the projection when it is missing, stale or cannot answer (the run it would
/// select has lines older than it), and rings the doorbell when that rewrote it: `status
/// --cached` then reports a last change it could not before (M3-D4). Run by a command
/// that changes something, after it released the global lock; one `stat` and one small
/// read while the projection is fresh. Best effort.
pub fn rebuild_if_stale(paths: &Paths, command: &str, actor: Actor) {
    // ubs:ignore — comparing an enum variant, not a secret
    if fresh(paths).is_some_and(|t| t.answer() != Answer::NeedsOlder) {
        return;
    }
    let Ok(tail) = build(paths) else {
        return;
    };
    // ubs:ignore — comparing projections, not secrets
    if load(paths).as_ref() != Some(&tail) && write(paths, &tail).is_ok() {
        super::doorbell::ring(paths, command, actor);
    }
}

/// The projection file; missing, corrupt or newer reads as none.
fn load(paths: &Paths) -> Option<HistoryTail> {
    match store::load::<HistoryTail>(&paths.history_tail()) {
        Ok(Some(loaded)) if loaded.status != VersionStatus::Newer => Some(loaded.value),
        _ => None,
    }
}

/// Writes the projection unless a newer omaddons wrote it.
fn write(paths: &Paths, tail: &HistoryTail) -> Result<()> {
    let path = paths.history_tail();
    if let Ok(Some(loaded)) = store::load::<HistoryTail>(&path)
        && loaded.status == VersionStatus::Newer
    {
        return Ok(());
    }
    let json = store::encode(tail)?;
    let scopes = ScopeTable::new(paths, &[], None)?;
    let mut s = WriteSession::untracked(Writer::Derived, &scopes, "history-tail");
    ensure_dir_all(&mut s, paths.cache_dir())?;
    let op = Op::WriteFile {
        content: Content::Bytes(&json),
        mode: Some(0o644),
    };
    mutate(&mut s, &path, op)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use ulid::Ulid;

    use super::*;
    use crate::fsx::{ScopeTable, WriteSession, Writer};
    use crate::model::{FolderName, RunId, TargetKey, TxId, TxKind};
    use crate::state::history::{
        FolderOp, HistoryEntry, HistoryFilter, HistoryOp, SettingEntry, TxResult, newest_run,
    };

    const TARGET: &str = "01JB8Q2V9W0000000000000000";

    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        scopes: ScopeTable,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        Fixture {
            _dir: dir,
            paths,
            scopes,
        }
    }

    impl Fixture {
        fn append(&self, entry: &HistoryEntry) {
            let mut s = WriteSession::untracked(Writer::Mutating, &self.scopes, "test");
            history::append(&mut s, &self.paths, entry).unwrap();
        }

        fn setting(&self) {
            let mut s = WriteSession::untracked(Writer::Mutating, &self.scopes, "test");
            let entry = SettingEntry::now(
                TxKind::Config,
                None,
                Actor::Human,
                None,
                vec!["omaddons".into(), "config".into(), "set".into()],
            );
            history::append_setting(&mut s, &self.paths, &entry).unwrap();
        }

        /// What bare `undo` reverts over the whole history, by transaction.
        fn full(&self) -> Option<Vec<TxId>> {
            let entries = history::read(&self.paths, &HistoryFilter::default())
                .unwrap()
                .entries;
            newest_run(&entries, None, None).map(|r| r.entries.iter().map(|e| e.tx).collect())
        }

        fn rebuild(&self) {
            rebuild_if_stale(&self.paths, "test", Actor::Human);
        }
    }

    /// A transaction of `run` with `ops` folder ops.
    fn entry(kind: TxKind, run: RunId, ops: usize) -> HistoryEntry {
        HistoryEntry {
            schema_version: history::CURRENT,
            tx: TxId::new(),
            kind,
            at: crate::clock::now(),
            target: TARGET.parse::<TargetKey>().unwrap(),
            actor: Actor::Agent,
            reason: None,
            argv: vec!["omaddons".into(), "install".into()],
            ops: (0..ops)
                .map(|i| HistoryOp {
                    op: FolderOp::Add,
                    folder: FolderName::new(&format!("Folder{i:06}")).unwrap(),
                    addon: "Big".into(),
                    edit: None,
                    backup: None,
                    from: None,
                    files: None,
                })
                .collect(),
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: Some(run),
            backup: None,
        }
    }

    fn found(tail: &HistoryTail) -> Option<Vec<TxId>> {
        match tail.answer() {
            Answer::Found(run) => Some(run.iter().map(|e| e.tx).collect()),
            _ => None,
        }
    }

    #[test]
    fn a_projection_follows_every_append_and_equals_a_rebuild() {
        let f = fixture();
        f.rebuild();
        let (a, b) = (RunId::new(), RunId::new());
        f.append(&entry(TxKind::Install, a, 1));
        f.append(&entry(TxKind::Update, b, 1));
        f.setting();
        f.append(&entry(TxKind::Install, a, 1));
        let tail = fresh(&f.paths).expect("kept current by every append");
        assert_eq!(tail.covers.from, 0, "the whole history while it is short");
        assert_eq!(tail, build(&f.paths).unwrap());
        assert_eq!(found(&tail), f.full());
        assert_eq!(found(&tail).unwrap().len(), 2, "run a's two lines");
    }

    #[test]
    fn sixteen_undoable_runs_stay_whole_and_backups_and_settings_never_count() {
        let f = fixture();
        f.rebuild();
        let update = RunId::new();
        f.append(&entry(TxKind::Update, update, 1));
        // A run's later line, after 40 backups and 20 settings changes.
        for _ in 0..40 {
            f.append(&entry(TxKind::Backup, RunId::new(), 0));
        }
        for _ in 0..20 {
            f.setting();
        }
        let tail = fresh(&f.paths).unwrap();
        assert_eq!(
            tail.covers.from, 0,
            "nothing undoable pushed the update out"
        );
        assert_eq!(found(&tail), f.full());
        // Twenty more undoable runs, each started as its command starts: only the newest
        // sixteen stay, whole.
        let mut runs = Vec::new();
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(2));
            let run = RunId::new();
            f.append(&entry(TxKind::Install, run, 1));
            runs.push(run);
        }
        // The oldest run held commits again: the newest now, with both its lines.
        f.append(&entry(TxKind::Install, runs[4], 1));
        let tail = fresh(&f.paths).unwrap();
        assert!(tail.covers.from > 0);
        let held: BTreeSet<RunKey> = tail
            .lines
            .iter()
            .filter_map(|l| line_facts(l))
            .filter(|f| f.undoable)
            .map(|f| f.run)
            .collect();
        assert_eq!(held.len(), HISTORY_TAIL_RUNS, "{held:?}");
        assert_eq!(found(&tail), f.full());
        assert_eq!(found(&tail).unwrap().len(), 2);
        assert_eq!(tail, build(&f.paths).unwrap(), "a rebuild agrees");
    }

    #[test]
    fn the_byte_cap_drops_the_oldest_lines_and_marks_a_run_it_cuts() {
        let f = fixture();
        f.rebuild();
        let straddling = RunId::new();
        std::thread::sleep(std::time::Duration::from_millis(2));
        f.append(&entry(TxKind::Install, straddling, 1));
        // About 100 KB a line: twelve of them pass the cap, within sixteen runs.
        for _ in 0..12 {
            f.append(&entry(TxKind::Install, RunId::new(), 2_000));
        }
        f.append(&entry(TxKind::Update, straddling, 1));
        let tail = fresh(&f.paths).unwrap();
        let held: u64 = tail.lines.iter().map(|l| l.len() as u64 + 1).sum();
        assert!(held <= HISTORY_TAIL_MAX_BYTES, "{held}");
        assert!(tail.covers.from > 0);
        assert_eq!(tail.partial, [straddling.to_string()]);
        assert_eq!(tail.answer(), Answer::NeedsOlder, "never a guess");
        assert_eq!(tail, build(&f.paths).unwrap(), "a rebuild cuts the same");
    }

    #[test]
    fn a_new_run_started_before_the_newest_line_dropped_may_have_older_lines() {
        let early = RunId::new();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let first = serde_json::to_string(&entry(TxKind::Install, RunId::new(), 1)).unwrap();
        let mut tail = HistoryTail {
            covers: Covers {
                from: 1_000,
                bytes: None,
            },
            dropped_at: Some(crate::clock::now()),
            lines: vec![first],
            ..HistoryTail::default()
        };
        std::thread::sleep(std::time::Duration::from_millis(5));
        let later = serde_json::to_string(&entry(TxKind::Install, early, 1)).unwrap();
        follow(&mut tail, &later);
        assert_eq!(tail.partial, [early.to_string()]);
        let newer = serde_json::to_string(&entry(TxKind::Install, RunId::new(), 1)).unwrap();
        follow(&mut tail, &newer);
        assert_eq!(tail.partial.len(), 1, "a run started after it is new");
        // Nothing dropped yet: no run can have older lines.
        let mut whole = HistoryTail::default();
        follow(&mut whole, &later);
        assert!(whole.partial.is_empty());
    }

    #[test]
    fn a_projection_is_believed_only_while_it_matches_the_history() {
        let f = fixture();
        assert_eq!(fresh(&f.paths), None, "missing");
        f.rebuild();
        let tail = fresh(&f.paths).unwrap();
        assert_eq!(tail.answer(), Answer::Nothing { complete: true });
        f.append(&entry(TxKind::Install, RunId::new(), 1));
        assert!(fresh(&f.paths).is_some());

        // A line written behind its back.
        let line = serde_json::to_string(&entry(TxKind::Install, RunId::new(), 1)).unwrap();
        let mut text = std::fs::read_to_string(f.paths.history_file()).unwrap();
        text.push_str(&line);
        text.push('\n');
        std::fs::write(f.paths.history_file(), text).unwrap();
        assert_eq!(fresh(&f.paths), None);
        // The next append cannot follow it: stale until a rebuild.
        f.append(&entry(TxKind::Install, RunId::new(), 1));
        assert_eq!(load(&f.paths).unwrap().covers.bytes, None);
        assert_eq!(fresh(&f.paths), None);
        f.rebuild();
        let tail = fresh(&f.paths).unwrap();
        assert_eq!(found(&tail), f.full());

        // Corrupt, or deleted: never believed.
        std::fs::write(f.paths.history_tail(), b"{ not json").unwrap();
        assert_eq!(fresh(&f.paths), None);
        std::fs::remove_file(f.paths.history_tail()).unwrap();
        assert_eq!(fresh(&f.paths), None);
        f.append(&entry(TxKind::Install, RunId::new(), 1));
        assert!(
            !f.paths.history_tail().exists(),
            "an append never builds one"
        );
        f.rebuild();
        assert!(fresh(&f.paths).is_some());
    }

    #[test]
    fn a_rebuild_rings_only_when_it_rewrote_the_projection() {
        let f = fixture();
        let bell = f.paths.doorbell_file();
        f.rebuild();
        assert!(bell.exists());
        std::fs::remove_file(&bell).unwrap();
        f.rebuild();
        assert!(!bell.exists(), "a fresh projection is left alone");
    }

    #[test]
    fn a_projection_that_cannot_be_written_never_fails_an_append() {
        let f = fixture();
        std::fs::create_dir_all(f.paths.cache_dir().parent().unwrap()).unwrap();
        std::fs::write(f.paths.cache_dir(), b"a file where the cache dir belongs").unwrap();
        f.rebuild();
        f.append(&entry(TxKind::Install, RunId::new(), 1));
        assert_eq!(fresh(&f.paths), None);
    }

    /// A small deterministic generator for the interleavings.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self, below: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % below
        }

        fn pick<T: Copy>(&mut self, from: &[T]) -> T {
            from[usize::try_from(self.next(from.len() as u64)).unwrap()]
        }
    }

    /// A history line of `kind` by `run`, written at `ms`.
    fn line_at(kind: TxKind, run: RunId, ms: i64) -> String {
        let at = jiff::Timestamp::from_millisecond(ms).unwrap();
        if kind == TxKind::Config {
            let mut setting =
                SettingEntry::now(kind, None, Actor::Human, None, vec!["omaddons".into()]);
            setting.run = run;
            setting.at = at;
            serde_json::to_string(&setting).unwrap()
        } else {
            serde_json::to_string(&HistoryEntry {
                at,
                ..entry(kind, run, 1)
            })
            .unwrap()
        }
    }

    #[test]
    fn the_projection_selects_what_the_whole_history_does_over_random_interleavings() {
        let kinds = [
            TxKind::Install,
            TxKind::Update,
            TxKind::Remove,
            TxKind::Undo,
            TxKind::Backup,
            TxKind::Config,
        ];
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let (mut found, mut needs_older) = (0, 0);
        for case in 0..1_000 {
            // Commands start on a clock, a few at a time, and commit their transactions
            // later, interleaved; one that is done commits nothing more.
            let mut ms: i64 = 1_790_000_000_000;
            let mut running: Vec<RunId> = Vec::new();
            let mut tail = HistoryTail::default();
            let mut text = String::new();
            for _ in 0..=rng.next(80) {
                ms += i64::try_from(rng.next(50)).unwrap() + 2;
                if running.is_empty() || (running.len() < 4 && rng.next(3) == 0) {
                    let started = u64::try_from(ms - 1).unwrap();
                    running.push(RunId::from_ulid(Ulid::from_parts(
                        started,
                        u128::from(rng.next(u64::MAX)),
                    )));
                }
                let run = rng.pick(&running);
                let line = line_at(rng.pick(&kinds), run, ms);
                follow(&mut tail, &line);
                text.push_str(&line);
                text.push('\n');
                if rng.next(3) == 0 {
                    running.retain(|r| *r != run);
                }
            }
            let entries = history::parse(&text, &HistoryFilter::default()).entries;
            let full = newest_run(&entries, None, None)
                .map(|r| r.entries.iter().map(|e| e.tx).collect::<Vec<TxId>>());
            match tail.answer() {
                Answer::Found(run) => {
                    found += 1;
                    let txs: Vec<TxId> = run.iter().map(|e| e.tx).collect();
                    assert_eq!(Some(txs), full, "case {case}");
                }
                Answer::Nothing { .. } => assert_eq!(full, None, "case {case}"),
                // Unknown, never a wrong run: the command's rebuild answers it.
                Answer::NeedsOlder => {
                    needs_older += 1;
                    let rebuilt = match HistoryTail::from_text(&text).answer() {
                        Answer::Found(run) => Some(run.iter().map(|e| e.tx).collect()),
                        _ => None,
                    };
                    assert_eq!(rebuilt, full, "case {case}: the rebuild answers");
                    assert!(rebuilt.is_some(), "case {case}");
                }
            }
        }
        // A run outlives sixteen others only rarely.
        assert!(
            needs_older * 20 < found,
            "{found} found, {needs_older} needing older lines"
        );
    }

    #[test]
    fn commands_one_after_another_never_need_a_rebuild() {
        let mut rng = Rng(7);
        let mut ms: i64 = 1_790_000_000_000;
        let mut tail = HistoryTail::default();
        for _ in 0..200 {
            ms += 1_000;
            let run = RunId::from_ulid(Ulid::from_parts(
                u64::try_from(ms).unwrap(),
                u128::from(rng.next(u64::MAX)),
            ));
            for _ in 0..=rng.next(3) {
                ms += 10;
                follow(&mut tail, &line_at(TxKind::Update, run, ms));
            }
            assert!(tail.partial.is_empty());
            assert!(matches!(tail.answer(), Answer::Found(_)));
        }
    }
}
