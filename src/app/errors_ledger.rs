//! The errors ledger around changes: its input from history (design 7.1.4), and the
//! light read every change that moves folders or puts settings back makes (7.1.8).
//!
//! The restores omaddons committed on a client end its BugGrabber databases' lifetimes.
//! The ledger reads them from history, never from a flag a restore's own best-effort
//! ledger write would set: that write can be skipped when the evidence lock is busy.
//! Callers look at the client's BugGrabber files first, then read the restores, then
//! open the ledger session: a restore committed after this read is then also after
//! every look, and the next session finds it.
//!
//! The light read is two halves around the engine call. [`capture`], before it and
//! with no lock, looks at each database, parses only a file the ledger has not seen in
//! its state, and asks whether the client runs. [`commit`], after it (whether the
//! change succeeded or not), looks again, reads the restores, and in one session
//! records the sightings and a change mark per database whose state was a proved read.
//! Neither half can fail or delay a change past `LEDGER_LOCK_TIMEOUT`: what goes wrong
//! is one `-vv` line, and that change has no mark.

use std::path::PathBuf;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::errors::ack::db_key;
use crate::errors::buggrabber::{Origin, evidence};
use crate::errors::ledger::{
    self, ChangeMark, ClientNow, DbLedger, DbRead, DbSighting, EntrySighting, Restored, SightMode,
    TargetLedger,
};
use crate::errors::locate::{self, Coverage as WalkCoverage};
use crate::errors::memo::{self, TargetMemo};
use crate::errors::observe::read_stable;
use crate::errors::sanitize::identities;
use crate::fsx::{FileStamp, ScopeTable};
use crate::limits::LEDGER_PRECHANGE_MAX_BYTES;
use crate::model::{RunId, Target, TargetKey, TxKind};
use crate::output::trace;
use crate::state::history::{self, FolderOp, HistoryEntry, HistoryFilter, HistoryRead};
use crate::state::history_tail;
use crate::state::paths::{ClientPaths, Paths};

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

/// What one history line put back, if anything: a whole `WTF/` (`wtf`), one addon's
/// SavedVariables (`saved_variables`), or, for an undo, the SavedVariables of the
/// folders it reverted. An undo's line counts the files it restored, not whose they
/// were, so each folder it names counts unless it restored none (`sv_restored` 0: it
/// deferred them, or none had changed); a line without the count counts too, as undo's
/// own conflict check reads it. Taking a folder for replaced when it was not costs
/// links, never makes one.
fn restore(entry: &HistoryEntry) -> Option<Restored> {
    let whole = entry.ops.iter().any(|op| op.op == FolderOp::Wtf);
    let undone = entry.kind == TxKind::Undo && entry.sv_restored != Some(0);
    let providers: Vec<String> = entry
        .ops
        .iter()
        .filter(|op| {
            op.op == FolderOp::SavedVariables || (undone && op.edit.is_none() && op.op.is_folder())
        })
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

// ---------------------------------------------------------------------------------------
// The light read around a change (design 7.1.8).

/// What the capture saw of the clients a change is about to touch: each database's
/// state, and whether each client ran. Held in memory across the engine call, then
/// given to [`commit`]. Empty when there was nothing to look at.
#[derive(Debug, Default)]
pub struct Captured {
    clients: Vec<CapturedClient>,
}

impl Captured {
    /// The clients looked at, for the second look at whether they run.
    pub fn targets(&self) -> Vec<Target> {
        self.clients.iter().map(|c| c.target.clone()).collect()
    }
}

#[derive(Debug)]
struct CapturedClient {
    target: Target,
    /// The ledger's (and the errors memo's) key for the client: its key, or its id while
    /// it is unregistered.
    filed_as: String,
    /// The walk that listed its databases was not cut by a bound.
    discovered: bool,
    /// Whether it ran at the look before the change.
    running: Option<bool>,
    /// When the capture looked: every sighting's `observed_at`, every mark's `at`, and
    /// the batch's `now`.
    looked_at: Timestamp,
    /// No restore before this concerns a database the capture's load showed: the oldest
    /// of their newest proved states, or the load's time.
    since: Timestamp,
    /// The capture's load of the ledger held something of this client.
    known: bool,
    dbs: Vec<CapturedDb>,
}

impl CapturedClient {
    /// Nothing a session could record: no database to sight or mark, and nothing in the
    /// ledger a restore could end. A client with no BugGrabber file costs no lock.
    fn idle(&self) -> bool {
        self.dbs.is_empty() && !self.known
    }
}

#[derive(Debug)]
struct CapturedDb {
    /// Where the file is, for the second look only: never recorded.
    path: PathBuf,
    sighting: DbSighting,
}

/// The ledger's key for `target`, as the errors memo files it.
fn filed_as(target: &Target) -> String {
    target
        .key
        .map_or_else(|| target.id.clone(), |key| key.to_string())
}

/// Whether the capture must parse a database file whose stamp is `stamp` (design 7.1.8
/// step 2): not when it is the stamp of the database's newest proved read, nor that of
/// the newest state that was read and could not vouch for absence. A pure function.
fn needs_read(db: Option<&DbLedger>, stamp: &str) -> bool {
    let Some(db) = db else {
        return true;
    };
    let newest = db.reads.last().is_some_and(|read| read.stamp == stamp);
    let unproved = db.unproved_stamp.as_deref() == Some(stamp);
    !(newest || unproved)
}

/// Whether the client ran at a change, from the look before it and the look after it
/// (design 7.1.8 step 4): true when either look found it running (a game launched
/// during a download loaded the old code), `None` when neither did and either could not
/// tell, false only when both said no.
fn ran(before: Option<bool>, after: Option<bool>) -> Option<bool> {
    match (before, after) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

/// The database files of a client the light read looks at, with whether the walk that
/// listed them was complete: those the errors memo lists, when it lists any, else each
/// account's `!BugGrabber.lua` found by `locate`'s own walk (databases other addons
/// embed are found by the first `errors` run). Each is `(where, relative to the client
/// folder, provider)`.
fn database_files(
    target: &Target,
    memo: Option<&TargetMemo>,
) -> (Vec<(PathBuf, String, String)>, bool) {
    if let Some(memo) = memo.filter(|m| !m.databases.is_empty()) {
        let files = memo
            .databases
            .iter()
            .map(|db| {
                (
                    target.client_dir.join(&db.path),
                    db.path.clone(),
                    db.provider.clone(),
                )
            })
            .collect();
        return (files, !memo.coverage.truncated);
    }
    let client = ClientPaths::new(&target.client_dir);
    let located = locate::locate_in(&client, true, &[]);
    let files = located
        .files
        .into_iter()
        .map(|file| {
            let rel = file
                .path
                .strip_prefix(&target.client_dir)
                .unwrap_or(&file.path)
                .to_string_lossy()
                .into_owned();
            (file.path, rel, file.folder)
        })
        .collect();
    (files, located.coverage == WalkCoverage::Complete)
}

/// One database as the capture sees it, or `None` when there is no file to look at.
/// A file whose stamp the ledger already knows is not parsed; another is read stably
/// within `budget` (the light read's own, `LEDGER_PRECHANGE_MAX_BYTES` in all), and
/// its local entries are given their two identities. Received entries are other
/// players' errors and are not sighted.
fn look(
    (path, rel, provider): (PathBuf, String, String),
    ledger: Option<&TargetLedger>,
    looked_at: Timestamp,
    budget: &mut u64,
    tz: &TimeZone,
) -> Option<CapturedDb> {
    let key = db_key(&rel);
    let stamp = FileStamp::read(&path).ok().flatten()?.fingerprint();
    let known = ledger.and_then(|l| l.databases.get(&key));
    if !needs_read(known, &stamp) {
        return Some(CapturedDb {
            path,
            sighting: DbSighting {
                key,
                provider,
                stamp,
                written_at: None,
                observed_at: looked_at,
                session: None,
                read: None,
            },
        });
    }
    // The wall clock, not the run's: it is compared with the file's `ctime`.
    let (_, coverage, db, first) = read_stable(&path, budget, Timestamp::now().as_nanosecond());
    let stamp = first?.fingerprint();
    let written_at = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| Timestamp::try_from(t).ok());
    let entries = db
        .entries
        .iter()
        .filter(|e| e.origin == Origin::Local)
        .map(|e| {
            let (fingerprint, family) = identities(&e.message);
            EntrySighting {
                family,
                fingerprint,
                session: e.session,
                time: evidence(e.time.as_ref(), tz),
                counter: e.counter,
            }
        })
        .collect();
    Some(CapturedDb {
        path,
        sighting: DbSighting {
            key,
            provider,
            stamp,
            written_at,
            observed_at: looked_at,
            session: db.session,
            read: Some(DbRead { coverage, entries }),
        },
    })
}

/// The light read's first half, before a change to `targets` (design 7.1.8 steps 1 to
/// 3): no lock, no inventory, nothing written. `running` answers whether a client runs
/// now (`None`: it cannot tell).
pub fn capture(
    paths: &Paths,
    targets: &[&Target],
    running: &dyn Fn(&Target) -> Option<bool>,
) -> Captured {
    if targets.is_empty() {
        return Captured::default();
    }
    let loaded_at = crate::clock::now();
    let Some(known) = ledger::load(paths) else {
        trace::emit(|| {
            "errors ledger: no light read: a newer omaddons wrote the ledger".to_owned()
        });
        return Captured::default();
    };
    let (memos, _) = memo::load(paths);
    let tz = TimeZone::system();
    let mut budget = LEDGER_PRECHANGE_MAX_BYTES;
    let clients = targets
        .iter()
        .map(|target| {
            let filed_as = filed_as(target);
            let ledger = known.targets.get(&filed_as);
            let (files, discovered) = database_files(target, memos.targets.get(&filed_as));
            let looked_at = crate::clock::now();
            let dbs = files
                .into_iter()
                .filter_map(|file| look(file, ledger, looked_at, &mut budget, &tz))
                .collect();
            let since = ledger
                .into_iter()
                .flat_map(|l| l.databases.values())
                .map(|db| {
                    db.reads
                        .last()
                        .map_or(db.started_at, |r| r.last_observed_at)
                })
                .fold(loaded_at, Timestamp::min);
            CapturedClient {
                target: (*target).clone(),
                filed_as,
                discovered,
                running: running(target),
                looked_at,
                since,
                known: ledger.is_some(),
                dbs,
            }
        })
        .collect();
    Captured { clients }
}

/// The light read's second half, after the change returned (design 7.1.8 steps 4 and
/// 5): each database looked at again, the clients' restores read from history, then one
/// ledger session under the evidence lock (never the transaction lock) that records
/// what the capture saw (`SightMode::Light`), adds a change mark for `run` on each
/// database whose captured state is now its newest proved read, and stores the ledger.
/// What cannot be done is one `-vv` line, never an error: that change has no mark.
pub fn commit(
    paths: &Paths,
    captured: Captured,
    run: RunId,
    running: &dyn Fn(&Target) -> Option<bool>,
    command: &str,
) {
    if captured.clients.iter().all(CapturedClient::idle) {
        return;
    }
    // Look again, then read the restores: a restore that commits after this read is
    // after every look, and the next session finds it.
    let looked: Vec<Looked> = captured
        .clients
        .into_iter()
        .map(registered)
        .map(|client| {
            let settled = client
                .dbs
                .iter()
                .map(|db| {
                    FileStamp::read(&db.path)
                        .ok()
                        .flatten()
                        .is_some_and(|now| now.fingerprint() == db.sighting.stamp)
                })
                .collect();
            let ran = ran(client.running, running(&client.target));
            let restores = restores_since(paths, &client.target, client.since);
            Looked {
                client,
                settled,
                ran,
                restores,
            }
        })
        .collect();
    debug_assert!(
        !crate::fsx::global_lock_held(),
        "the errors ledger's light read runs outside the transaction lock"
    );
    let scopes = match ScopeTable::new(paths, &[], None) {
        Ok(scopes) => scopes,
        Err(err) => {
            trace::emit(|| format!("errors ledger: no light read recorded: {}", err.message));
            return;
        }
    };
    let mut session = match ledger::open(paths, &scopes, command) {
        Ok(session) => session,
        Err(why) => {
            trace::emit(|| format!("errors ledger: no light read recorded: {why:?}"));
            return;
        }
    };
    for Looked {
        client,
        settled,
        ran,
        restores,
    } in &looked
    {
        let dbs: Vec<DbSighting> = client.dbs.iter().map(|db| db.sighting.clone()).collect();
        // A history that cannot be read whole may hide a restore: nothing appears.
        let (restores, discovered) = match restores {
            Some(restores) => (restores.as_slice(), client.discovered),
            None => {
                trace::emit(|| {
                    format!(
                        "errors ledger: {}: the history does not read whole, so no error is recorded as newly appeared",
                        client.target.id
                    )
                });
                (&[][..], false)
            }
        };
        let now = ClientNow {
            version: Some(client.target.version.to_string()),
            now: client.looked_at,
        };
        session.sight(
            &client.filed_as,
            &dbs,
            restores,
            &now,
            discovered,
            SightMode::Light,
        );
        let mut marked = 0;
        for (db, settled) in client.dbs.iter().zip(settled) {
            let Some(mark) = mark_of(&session.ledger, client, db, run, *ran, *settled) else {
                continue;
            };
            if session.add_mark(&client.filed_as, &db.sighting.key, mark) {
                marked += 1;
            }
        }
        if marked < client.dbs.len() {
            trace::emit(|| {
                format!(
                    "errors ledger: {}: {marked} of {} databases marked (a state that is not a proved read, or a replaced file, gets none)",
                    client.target.id,
                    client.dbs.len()
                )
            });
        }
    }
    if let Err(why) = session.commit() {
        trace::emit(|| format!("errors ledger: the light read was not stored: {why:?}"));
    }
}

/// The client as the change left it. A client the change registered (its first change
/// does) is filed under the key it was given, as every later run files it, so that its
/// first change's mark is among the others, not under the id it had while unregistered.
fn registered(mut client: CapturedClient) -> CapturedClient {
    if client.target.key.is_none()
        && let Some(key) = crate::discover::registry::read_marker(&client.target.client_dir)
    {
        client.target.key = Some(key);
        client.filed_as = key.to_string();
    }
    client
}

/// One client after the change: the capture, whether each database is still in the
/// captured state (`settled`, in the capture's order), whether the client ran at either
/// look, and its restores (`None`: the history does not read whole).
struct Looked {
    client: CapturedClient,
    settled: Vec<bool>,
    ran: Option<bool>,
    restores: Option<Vec<Restored>>,
}

/// The change mark of one captured database, when its captured state is the newest
/// proved read of its database's current generation in the ledger as the session holds
/// it after its sightings (a database whose first read was this one has its generation
/// only now). [`ledger::LedgerSession::add_mark`] checks the same, and that the file
/// was not replaced since.
fn mark_of(
    ledger: &ledger::ErrorsLedger,
    client: &CapturedClient,
    db: &CapturedDb,
    run: RunId,
    running: Option<bool>,
    settled: bool,
) -> Option<ChangeMark> {
    let state = ledger
        .targets
        .get(&client.filed_as)?
        .databases
        .get(&db.sighting.key)?;
    let newest = state
        .reads
        .last()
        .filter(|read| read.stamp == db.sighting.stamp)?;
    Some(ChangeMark {
        run,
        at: db.sighting.observed_at,
        generation: state.generation.clone(),
        stamp: db.sighting.stamp.clone(),
        session: newest.session,
        running,
        settled,
    })
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
        line_at(target, at(seconds), kind, ops)
    }

    /// A committed line on `target` at `when`, with `ops`.
    fn line_at(target: &str, when: Timestamp, kind: TxKind, ops: Vec<HistoryOp>) -> String {
        serde_json::to_string(&HistoryEntry {
            schema_version: history::CURRENT,
            tx: TxId::new(),
            kind,
            at: when,
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
    fn an_undo_put_back_the_settings_of_its_folders_unless_it_restored_none() {
        let undo = |folder: &str, sv_restored: Option<u64>, sv_deferred: bool| {
            let mut entry: serde_json::Value = serde_json::from_str(&line(
                KEY,
                110,
                TxKind::Undo,
                vec![op(FolderOp::Replace, folder)],
            ))
            .unwrap();
            entry["sv_restored"] = serde_json::json!(sv_restored);
            entry["sv_deferred"] = serde_json::json!(sv_deferred);
            serde_json::from_value::<HistoryEntry>(entry).unwrap()
        };
        let mut record = undo("Foo", Some(1), false);
        record.ops[0].edit = Some(history::HistoryEdit::RequiredByRemove("Bar".to_owned()));
        // (case, the line, what it put back)
        let cases = [
            (
                "an undo that restored BugGrabber's settings",
                undo("!BugGrabber", Some(2), false),
                Some(restored(110, false, &["!BugGrabber"])),
            ),
            (
                "an undo that deferred them",
                undo("!BugGrabber", Some(0), true),
                None,
            ),
            (
                "an undo with none changed",
                undo("!BugGrabber", Some(0), false),
                None,
            ),
            (
                "an undo whose line has no count",
                undo("!BugGrabber", None, false),
                Some(restored(110, false, &["!BugGrabber"])),
            ),
            ("an undo of a record edit", record, None),
        ];
        for (case, entry, want) in cases {
            assert_eq!(restore(&entry), want, "{case}");
        }
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

    // -----------------------------------------------------------------------------------
    // The light read

    use crate::errors::ledger::{BaselineReason, FamilyFirst, LedgerRead};

    #[test]
    fn a_file_is_parsed_only_when_its_state_is_new_to_the_ledger() {
        let read = |stamp: &str| LedgerRead {
            stamp: stamp.to_owned(),
            written_at: None,
            session: Some(3),
            first_observed_at: at(1),
            last_observed_at: at(2),
            client_version: None,
        };
        let db = |reads: Vec<LedgerRead>, unproved: Option<&str>| DbLedger {
            generation: ledger::DbGeneration("g:1".to_owned()),
            started_at: at(0),
            max_session: Some(3),
            max_written_at: None,
            invalidated_at: None,
            reads,
            unproved_stamp: unproved.map(str::to_owned),
            marks: Vec::new(),
        };
        // (case, what the ledger holds of the database, the stamp now, parsed)
        let cases = [
            ("a database the ledger has not met", None, "s1", true),
            (
                "the newest proved read's state",
                Some(db(vec![read("s0"), read("s1")], None)),
                "s1",
                false,
            ),
            (
                "an older proved read's state is new again",
                Some(db(vec![read("s0"), read("s1")], None)),
                "s0",
                true,
            ),
            (
                "the state last read that could not vouch",
                Some(db(vec![read("s0")], Some("s1"))),
                "s1",
                false,
            ),
            (
                "a state neither proved nor unproved",
                Some(db(vec![read("s0")], Some("s1"))),
                "s2",
                true,
            ),
            (
                "no proved read yet, the unproved state",
                Some(db(vec![], Some("s1"))),
                "s1",
                false,
            ),
        ];
        for (case, known, stamp, want) in cases {
            assert_eq!(needs_read(known.as_ref(), stamp), want, "{case}");
        }
    }

    #[test]
    fn the_client_ran_at_a_change_when_either_look_found_it_running() {
        let (yes, no, unknown) = (Some(true), Some(false), None);
        // (before, after, ran)
        let cases = [
            (yes, yes, yes),
            (yes, no, yes),
            (yes, unknown, yes),
            (no, yes, yes),
            (no, no, no),
            (no, unknown, unknown),
            (unknown, yes, yes),
            (unknown, no, unknown),
            (unknown, unknown, unknown),
        ];
        for (before, after, want) in cases {
            assert_eq!(
                ran(before, after),
                want,
                "before {before:?}, after {after:?}"
            );
        }
    }

    /// A client with one account whose `!BugGrabber.lua` the tests write, in a home of
    /// its own.
    struct Client {
        _dir: tempfile::TempDir,
        paths: Paths,
        target: Target,
        file: PathBuf,
    }

    impl Client {
        fn new() -> Self {
            let (dir, paths) = home();
            let client_dir = dir.path().join("_retail_");
            let mut target = crate::engine::testing::target(&client_dir);
            target.key = KEY.parse().ok();
            let file = client_dir.join("WTF/Account/ACCOUNT/SavedVariables/!BugGrabber.lua");
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            Self {
                _dir: dir,
                paths,
                target,
                file,
            }
        }

        /// The file saved in `session`, holding one error per message in `errors`, then
        /// past the timestamp slack, so a read of it is stable.
        fn save(&self, session: u32, errors: &[&str]) {
            let entries: String = errors
                .iter()
                .map(|m| {
                    format!(
                        "{{\n[\"message\"] = \"{m}\",\n[\"session\"] = {session},\n[\"counter\"] = 1,\n}},\n"
                    )
                })
                .collect();
            let text = format!(
                "\nBugGrabberDB = {{\n[\"session\"] = {session},\n[\"errors\"] = {{\n{entries}}},\n}}\n"
            );
            std::fs::write(&self.file, text).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(30));
        }

        /// One light read around a change by `run`, the client running at neither look.
        fn light_read(&self, run: RunId) {
            self.light_read_with(run, Some(false), Some(false));
        }

        fn light_read_with(&self, run: RunId, before: Option<bool>, after: Option<bool>) {
            let captured = capture(&self.paths, &[&self.target], &|_| before);
            commit(&self.paths, captured, run, &|_| after, "test");
        }

        fn ledger(&self) -> TargetLedger {
            ledger::load(&self.paths)
                .unwrap()
                .targets
                .get(KEY)
                .cloned()
                .unwrap_or_default()
        }

        /// The one database's ledger.
        fn db(&self) -> DbLedger {
            let ledger = self.ledger();
            assert_eq!(ledger.databases.len(), 1, "{ledger:?}");
            ledger.databases.into_values().next().unwrap()
        }

        /// A whole-`WTF/` restore committed now, and the file put back in `session`.
        fn restore_whole(&self, session: u32, errors: &[&str]) {
            self.restore(vec![op(FolderOp::Wtf, "WTF")], session, errors);
        }

        fn restore(&self, ops: Vec<HistoryOp>, session: u32, errors: &[&str]) {
            std::fs::remove_file(&self.file).unwrap();
            self.save(session, errors);
            let mut text = std::fs::read_to_string(self.paths.history_file()).unwrap_or_default();
            text.push_str(&line_at(KEY, Timestamp::now(), TxKind::Restore, ops));
            text.push('\n');
            std::fs::write(self.paths.history_file(), text).unwrap();
        }
    }

    fn basis(ledger: &TargetLedger, message: &str) -> Option<FamilyFirst> {
        let family = crate::errors::sanitize::family_key(message);
        ledger.families.get(&family).map(|f| f.first.clone())
    }

    const OLD: &str = "Interface/AddOns/Foo/core.lua:12: attempt to index a nil value (field 'db')";
    const NEW: &str = "Interface/AddOns/Bar/init.lua:7: attempt to call a nil value (method 'Go')";

    #[test]
    fn a_first_light_read_starts_a_generation_baselines_and_marks_the_change() {
        let client = Client::new();
        client.save(5, &[OLD]);
        let run = RunId::new();
        client.light_read(run);
        let db = client.db();
        assert_eq!(db.reads.len(), 1, "{db:?}");
        assert_eq!(db.marks.len(), 1, "{db:?}");
        let mark = &db.marks[0];
        assert_eq!(
            (
                mark.run,
                &mark.generation,
                &mark.stamp,
                mark.session,
                mark.running,
                mark.settled
            ),
            (
                run,
                &db.generation,
                &db.reads[0].stamp,
                Some(5),
                Some(false),
                true
            )
        );
        assert!(matches!(
            basis(&client.ledger(), OLD),
            Some(FamilyFirst::Baseline {
                reason: BaselineReason::FirstRead,
                ..
            })
        ));
    }

    #[test]
    fn a_client_without_a_buggrabber_file_costs_no_lock_and_writes_nothing() {
        let client = Client::new();
        client.light_read(RunId::new());
        assert!(!client.paths.errors_ledger_lock().exists(), "no lock taken");
        assert!(!client.paths.errors_ledger().exists(), "no ledger written");
    }

    #[test]
    fn an_unchanged_file_is_not_parsed_and_its_state_is_marked_again() {
        let client = Client::new();
        client.save(5, &[OLD]);
        client.light_read(RunId::new());
        let captured = capture(&client.paths, &[&client.target], &|_| Some(false));
        let sighting = &captured.clients[0].dbs[0].sighting;
        assert!(sighting.read.is_none(), "{sighting:?}");
        let second = RunId::new();
        commit(&client.paths, captured, second, &|_| Some(false), "test");
        let db = client.db();
        assert_eq!(db.reads.len(), 1, "one state, seen twice");
        assert_eq!(
            db.marks.iter().map(|m| m.run).collect::<Vec<_>>().last(),
            Some(&second)
        );
    }

    #[test]
    fn a_new_state_of_a_covered_client_records_its_new_families_unjudged() {
        let client = Client::new();
        client.save(5, &[OLD]);
        client.light_read(RunId::new());
        client.save(6, &[OLD, NEW]);
        client.light_read(RunId::new());
        let ledger = client.ledger();
        let Some(FamilyFirst::Observed { cluster }) = basis(&ledger, NEW) else {
            unreachable!("{:?}", basis(&ledger, NEW));
        };
        assert_eq!(ledger.clusters[&cluster].judgment, None);
        assert!(matches!(
            basis(&ledger, OLD),
            Some(FamilyFirst::Baseline { .. })
        ));
    }

    #[test]
    fn a_file_the_game_wrote_between_the_halves_is_marked_unsettled() {
        let client = Client::new();
        client.save(5, &[OLD]);
        client.light_read(RunId::new());
        let captured = capture(&client.paths, &[&client.target], &|_| Some(false));
        client.save(6, &[OLD]);
        let run = RunId::new();
        commit(&client.paths, captured, run, &|_| Some(false), "test");
        let db = client.db();
        let mark = db.marks.iter().find(|m| m.run == run).unwrap();
        assert!(!mark.settled, "{mark:?}");
        assert_eq!(mark.session, Some(5), "the session before the change");
    }

    #[test]
    fn a_game_started_during_the_change_ran_at_it() {
        let client = Client::new();
        client.save(5, &[OLD]);
        for (before, after, want) in [
            (Some(false), Some(true), Some(true)),
            (Some(false), None, None),
            (Some(false), Some(false), Some(false)),
        ] {
            let run = RunId::new();
            client.light_read_with(run, before, after);
            let db = client.db();
            let mark = db.marks.iter().find(|m| m.run == run).unwrap();
            assert_eq!(mark.running, want, "before {before:?}, after {after:?}");
        }
    }

    #[test]
    fn a_state_that_is_not_a_proved_read_gets_no_mark() {
        use std::os::unix::fs::PermissionsExt;
        let client = Client::new();
        client.save(5, &[OLD]);
        std::fs::set_permissions(&client.file, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&client.file).is_ok() {
            // Running as root, the mode does not stop a read.
            return;
        }
        client.light_read(RunId::new());
        std::fs::set_permissions(&client.file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(client.ledger().databases.len(), 0, "{:?}", client.ledger());
        // Once readable (and past the slack of the mode's change), it is read and marked.
        std::thread::sleep(std::time::Duration::from_millis(30));
        client.light_read(RunId::new());
        assert_eq!(client.db().marks.len(), 1);
    }

    #[test]
    fn a_busy_evidence_lock_or_a_ledger_that_is_a_directory_records_nothing_and_waits_little() {
        let client = Client::new();
        client.save(5, &[OLD]);
        let scopes = ScopeTable::new(&client.paths, &[], None).unwrap();
        let held = ledger::open(&client.paths, &scopes, "another").unwrap();
        let started = std::time::Instant::now();
        client.light_read(RunId::new());
        let waited = started.elapsed();
        drop(held);
        assert!(
            waited < crate::limits::LEDGER_LOCK_TIMEOUT + std::time::Duration::from_secs(1),
            "{waited:?}"
        );
        assert!(!client.paths.errors_ledger().exists(), "nothing stored");
        std::fs::create_dir_all(client.paths.errors_ledger()).unwrap();
        client.light_read(RunId::new());
        assert!(client.paths.errors_ledger().is_dir());
    }

    #[test]
    fn a_restore_in_history_ends_the_databases_lifetime_whatever_its_counter() {
        for (case, session) in [("lower", 3), ("equal", 5), ("higher", 9)] {
            let client = Client::new();
            client.save(5, &[OLD]);
            client.light_read(RunId::new());
            let before = client.db().generation;
            // The restored file brings an error the database never held.
            client.restore_whole(session, &[OLD, NEW]);
            client.light_read(RunId::new());
            let ledger = client.ledger();
            let db = client.db();
            assert_ne!(db.generation, before, "{case}: a new generation");
            assert!(
                matches!(
                    basis(&ledger, NEW),
                    Some(FamilyFirst::Baseline {
                        reason: BaselineReason::NewGeneration,
                        ..
                    })
                ),
                "{case}: what a restore brings is no appearance: {:?}",
                basis(&ledger, NEW)
            );
            assert!(ledger.clusters.is_empty(), "{case}");
            assert_eq!(db.marks.len(), 1, "{case}: the new lifetime's mark only");
        }
    }

    #[test]
    fn only_a_restore_of_the_provider_ends_its_databases_lifetime() {
        for (case, folder, ends) in [
            ("BugGrabber's own settings", "!BugGrabber", true),
            ("spelled another way", "!bugGrabber", true),
            ("another addon's settings", "Foo", false),
        ] {
            let client = Client::new();
            client.save(5, &[OLD]);
            client.light_read(RunId::new());
            let before = client.db().generation;
            client.restore(vec![op(FolderOp::SavedVariables, folder)], 6, &[OLD]);
            client.light_read(RunId::new());
            assert_eq!(client.db().generation != before, ends, "{case}");
        }
    }

    #[test]
    fn a_restore_whose_own_light_read_was_lost_is_found_by_the_next() {
        let client = Client::new();
        client.save(5, &[OLD]);
        client.light_read(RunId::new());
        let before = client.db().generation;
        // The restore's own light read: it looks at the file, the restore commits, and
        // its session cannot be had.
        let captured = capture(&client.paths, &[&client.target], &|_| Some(false));
        client.restore_whole(5, &[OLD, NEW]);
        let scopes = ScopeTable::new(&client.paths, &[], None).unwrap();
        let held = ledger::open(&client.paths, &scopes, "another").unwrap();
        commit(
            &client.paths,
            captured,
            RunId::new(),
            &|_| Some(false),
            "test",
        );
        drop(held);
        assert_eq!(client.db().generation, before, "nothing recorded yet");
        client.light_read(RunId::new());
        assert_ne!(client.db().generation, before);
        assert!(client.ledger().clusters.is_empty());
    }

    #[test]
    fn a_look_made_before_a_restore_is_never_recorded_as_the_restored_file() {
        let client = Client::new();
        client.save(5, &[OLD]);
        client.light_read(RunId::new());
        let before = client.db().generation;
        // Looked at before the restore, recorded after it: the look is stale and leaves
        // no mark, and the next read starts the new lifetime with no appearance.
        let captured = capture(&client.paths, &[&client.target], &|_| Some(false));
        client.restore_whole(5, &[OLD, NEW]);
        let run = RunId::new();
        commit(&client.paths, captured, run, &|_| Some(false), "test");
        let db = client.db();
        assert!(db.invalidated_at.is_some(), "{db:?}");
        assert!(!db.marks.iter().any(|m| m.run == run), "{db:?}");
        client.light_read(RunId::new());
        assert_ne!(client.db().generation, before);
        assert!(client.ledger().clusters.is_empty());
    }

    #[test]
    fn the_databases_the_errors_memo_lists_are_the_ones_looked_at() {
        let client = Client::new();
        client.save(5, &[OLD]);
        // An embedded database the standard walk would not find, listed by the memo.
        let embedded = client
            .target
            .client_dir
            .join("WTF/Account/ACCOUNT/SavedVariables/bdUI.lua");
        std::fs::copy(&client.file, &embedded).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let memo = memo::ErrorsMemo {
            targets: std::collections::BTreeMap::from([(
                KEY.to_owned(),
                TargetMemo {
                    capture: "buggrabber".to_owned(),
                    coverage: memo::MemoCoverage {
                        truncated: false,
                        accounts: 1,
                        files: 1,
                    },
                    listings: Vec::new(),
                    received: 0,
                    databases: vec![memo::DbMemo {
                        path: "WTF/Account/ACCOUNT/SavedVariables/bdUI.lua".to_owned(),
                        account: 1,
                        provider: "bdUI".to_owned(),
                        stamp: String::new(),
                        written_at: None,
                        state: crate::errors::observe::ReadState::Complete,
                        session: Some(5),
                        baseline: 4,
                        sessions: std::collections::BTreeMap::new(),
                        entries: 1,
                        occurrences: 1,
                        addons: Vec::new(),
                    }],
                },
            )]),
        };
        assert!(memo::store(&client.paths, &memo, Actor::Human));
        client.light_read(RunId::new());
        assert_eq!(
            client.db().reads.len(),
            1,
            "the memo's database, and not the standard file"
        );
        let key = db_key("WTF/Account/ACCOUNT/SavedVariables/bdUI.lua");
        assert!(client.ledger().databases.contains_key(&key));
    }

    #[test]
    fn a_file_past_the_light_reads_byte_budget_is_not_read_and_proves_nothing() {
        let client = Client::new();
        client.save(5, &[OLD]);
        let size = std::fs::metadata(&client.file).unwrap().len();
        let file = (
            client.file.clone(),
            "WTF/Account/ACCOUNT/SavedVariables/!BugGrabber.lua".to_owned(),
            "!BugGrabber".to_owned(),
        );
        let mut budget = size - 1;
        let looked = look(file, None, Timestamp::now(), &mut budget, &TimeZone::UTC).unwrap();
        let read = looked.sighting.read.unwrap();
        assert_eq!(
            (read.coverage, read.entries.len(), budget),
            (
                crate::errors::observe::Coverage::Degraded(crate::errors::observe::Gap::Unread),
                0,
                size - 1
            ),
            "nothing of it is read or charged"
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "outside the transaction lock")]
    fn the_light_read_is_never_recorded_inside_a_commit_window() {
        let client = Client::new();
        client.save(5, &[OLD]);
        let captured = capture(&client.paths, &[&client.target], &|_| Some(false));
        let _held = crate::fsx::GlobalLock::acquire(&client.paths, "test").unwrap();
        commit(
            &client.paths,
            captured,
            RunId::new(),
            &|_| Some(false),
            "test",
        );
    }
}
