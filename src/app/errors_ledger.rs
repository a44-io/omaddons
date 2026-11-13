//! The errors ledger's input from history (design 7.1.4): the restores omaddons
//! committed on a client, which end its BugGrabber databases' lifetimes. The ledger
//! reads them from history, never from a flag a restore's own best-effort ledger write
//! would set: that write can be skipped when the evidence lock is busy.
//!
//! Callers look at the client's BugGrabber files first, then read the restores, then
//! open the ledger session: a restore committed after this read is then also after
//! every look, and the next session finds it.

use jiff::Timestamp;

use crate::errors::ledger::Restored;
use crate::model::{Target, TargetKey};
use crate::state::history::{self, FolderOp, HistoryEntry, HistoryFilter, HistoryRead};
use crate::state::history_tail;
use crate::state::paths::Paths;

/// The restores committed on `target` at or after `since`: each history line whose ops
/// put a whole `WTF/` back (`whole`) or one addon's SavedVariables (`providers`, the
/// folders). From the history projection when it is fresh and reaches back to `since`,
/// else from `history.jsonl`. An unregistered client has none: history names clients by
/// key.
///
/// `None` when the history cannot be read whole: a missed restore would let a restored
/// file's errors read as newly appeared, so the caller records no appearance then. A
/// line that does not read counts unless it names another client or is older than
/// `since`, as undo's conflict check reads such lines.
pub fn restores_since(paths: &Paths, target: &Target, since: Timestamp) -> Option<Vec<Restored>> {
    restores_on(paths, target.key, since)
}

/// [`restores_since`] for a client's key, `None` while it is unregistered.
fn restores_on(paths: &Paths, key: Option<TargetKey>, since: Timestamp) -> Option<Vec<Restored>> {
    let Some(key) = key else {
        return Some(Vec::new());
    };
    let filter = HistoryFilter {
        target: Some(key),
        since: Some(since),
        ..HistoryFilter::default()
    };
    let read = match from_projection(paths, since, &filter) {
        Some(read) => read,
        None => history::read(paths, &filter).ok()?,
    };
    restores_of(&read, key, since)
}

/// The projection's lines read with `filter`, when the projection is fresh and holds
/// every line from `since` on: it starts at the history's first byte, or its oldest
/// line that reads is not later than `since`.
fn from_projection(paths: &Paths, since: Timestamp, filter: &HistoryFilter) -> Option<HistoryRead> {
    let tail = history_tail::fresh(paths)?;
    let mut text = tail.lines.join("\n");
    text.push('\n');
    let reaches = tail.covers.from == 0
        || history::parse(&text, &HistoryFilter::default())
            .entries
            .first()
            .is_some_and(|oldest| oldest.at <= since);
    reaches.then(|| history::parse(&text, filter))
}

fn restores_of(read: &HistoryRead, key: TargetKey, since: Timestamp) -> Option<Vec<Restored>> {
    let doubtful = read
        .unreadable
        .iter()
        .any(|line| may_concern(&line.raw, key, since));
    if doubtful {
        return None;
    }
    Some(read.entries.iter().filter_map(restore).collect())
}

/// What one history line put back, if anything.
fn restore(entry: &HistoryEntry) -> Option<Restored> {
    let whole = entry.ops.iter().any(|op| op.op == FolderOp::Wtf);
    let providers: Vec<String> = entry
        .ops
        .iter()
        .filter(|op| op.op == FolderOp::SavedVariables)
        .map(|op| op.folder.as_str().to_owned())
        .collect();
    let put_back = whole || !providers.is_empty();
    put_back.then_some(Restored {
        at: entry.at,
        whole,
        providers,
    })
}

/// Whether a line that does not read may be a restore on `key` at or after `since`: yes,
/// unless what of it still reads names another client, or a time before `since`.
fn may_concern(raw: &str, key: TargetKey, since: Timestamp) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return true;
    };
    let other_client = value
        .get("target")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|target| target != key.to_string());
    let older = value
        .get("at")
        .and_then(serde_json::Value::as_str)
        .and_then(|at| at.parse::<Timestamp>().ok())
        .is_some_and(|at| at < since);
    !(other_client || older)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Actor, FolderName, TxId, TxKind};
    use crate::state::history::{HistoryOp, TxResult};

    const KEY: &str = "01M51EX04HHWM46XE9F2B9JXED";
    const OTHER: &str = "01M51EX645RCVFN6Z4ZV5GRWVS";

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + seconds).unwrap()
    }

    fn op(op: FolderOp, folder: &str) -> HistoryOp {
        HistoryOp {
            op,
            folder: FolderName::new(folder).unwrap(),
            addon: String::new(),
            edit: None,
            backup: None,
            from: None,
            files: None,
        }
    }

    /// A committed line on `target` at `seconds`, with `ops`.
    fn line(target: &str, seconds: i64, kind: TxKind, ops: Vec<HistoryOp>) -> String {
        serde_json::to_string(&HistoryEntry {
            schema_version: history::CURRENT,
            tx: TxId::new(),
            kind,
            at: at(seconds),
            target: Some(target.parse().unwrap()),
            targets: Vec::new(),
            actor: Actor::Human,
            reason: None,
            argv: vec!["omaddons".into()],
            ops,
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
        })
        .unwrap()
    }

    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::with_roots(root, root.join("c"), root.join("s"), root.join("k"));
        std::fs::create_dir_all(paths.history_file().parent().unwrap()).unwrap();
        (dir, paths)
    }

    fn write_history(paths: &Paths, lines: &[String]) {
        let mut text = lines.join("\n");
        text.push('\n');
        std::fs::write(paths.history_file(), text).unwrap();
    }

    fn restored(seconds: i64, whole: bool, providers: &[&str]) -> Restored {
        Restored {
            at: at(seconds),
            whole,
            providers: providers.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    #[test]
    fn restores_since_reads_what_each_line_put_back() {
        let lines = [
            line(KEY, 50, TxKind::Restore, vec![op(FolderOp::Wtf, "WTF")]),
            line(KEY, 110, TxKind::Restore, vec![op(FolderOp::Wtf, "WTF")]),
            line(
                KEY,
                120,
                TxKind::Restore,
                vec![op(FolderOp::SavedVariables, "!BugGrabber")],
            ),
            line(
                KEY,
                130,
                TxKind::Undo,
                vec![op(FolderOp::SavedVariables, "Foo")],
            ),
            line(OTHER, 140, TxKind::Restore, vec![op(FolderOp::Wtf, "WTF")]),
            line(KEY, 150, TxKind::Install, vec![op(FolderOp::Add, "Bar")]),
        ];
        let (_dir, paths) = home();
        write_history(&paths, &lines);
        let key = KEY.parse().ok();
        assert_eq!(
            restores_on(&paths, key, at(100)),
            Some(vec![
                restored(110, true, &[]),
                restored(120, false, &["!BugGrabber"]),
                restored(130, false, &["Foo"]),
            ]),
            "a line older than since, another client's, and an install are left out"
        );
        assert_eq!(
            restores_on(&paths, None, at(100)),
            Some(vec![]),
            "unregistered"
        );
    }

    #[test]
    fn a_line_that_does_not_read_counts_unless_it_names_another_client_or_an_older_time() {
        let good = line(KEY, 110, TxKind::Restore, vec![op(FolderOp::Wtf, "WTF")]);
        let key = KEY.parse().ok();
        // (case, the unreadable line, the history still reads)
        for (case, bad, reads) in [
            ("no JSON at all", "{ not json".to_owned(), false),
            (
                "this client's, at no readable time",
                format!(r#"{{"target":"{KEY}","kind":"from_the_future"}}"#),
                false,
            ),
            (
                "another client's",
                format!(r#"{{"target":"{OTHER}","kind":"from_the_future"}}"#),
                true,
            ),
            (
                "older than since",
                r#"{"at":"2026-01-01T00:00:00Z","kind":"from_the_future"}"#.to_owned(),
                true,
            ),
        ] {
            let (_dir, paths) = home();
            write_history(&paths, &[good.clone(), bad]);
            let got = restores_on(&paths, key, at(100));
            assert_eq!(got.is_some(), reads, "{case}: {got:?}");
        }
    }

    #[test]
    fn a_fresh_projection_answers_and_a_stale_one_falls_back_to_the_history() {
        let (_dir, paths) = home();
        let key = KEY.parse().ok();
        write_history(
            &paths,
            &[line(
                KEY,
                110,
                TxKind::Restore,
                vec![op(FolderOp::Wtf, "WTF")],
            )],
        );
        history_tail::rebuild_if_stale(&paths, "test", Actor::Human);
        let filter = HistoryFilter {
            target: key,
            since: Some(at(100)),
            ..HistoryFilter::default()
        };
        let from_tail = from_projection(&paths, at(100), &filter).expect("a fresh projection");
        assert_eq!(from_tail.entries.len(), 1);
        // A line appended behind the projection's back makes it stale.
        let mut text = std::fs::read_to_string(paths.history_file()).unwrap();
        text.push_str(&line(
            KEY,
            120,
            TxKind::Restore,
            vec![op(FolderOp::Wtf, "WTF")],
        ));
        text.push('\n');
        std::fs::write(paths.history_file(), text).unwrap();
        assert!(from_projection(&paths, at(100), &filter).is_none());
        assert_eq!(
            restores_on(&paths, key, at(100)),
            Some(vec![restored(110, true, &[]), restored(120, true, &[])])
        );
    }
}
