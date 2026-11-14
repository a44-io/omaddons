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
//!
//! The full read is `errors`' (7.1.7): every database it read, whole, sighted with
//! [`SightMode::Full`] in one session; then, outside any lock, each appearance still owed
//! a judgment judged from history and the client's folders (7.2.1), and the verdicts
//! attached in a second session ([`full_read`]). Each error's `first_seen`, link and
//! judgment are looked up in the ledger that stored ([`ClientLedger`]), by its exact
//! fingerprint only.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use crate::app::errors::{
    AfterChange, ChangedNow, FirstSeenBasis, FirstSeenInfo, FirstSeenReason, JudgmentInfo,
    JudgmentStanding, LinkState,
};
use crate::app::history_items::{Reader, transitions_of};
use crate::errors::ack::db_key;
use crate::errors::attribute::{self, FolderSet};
use crate::errors::buggrabber::{BugEntry, DbShape, Origin, evidence};
use crate::errors::frames;
use crate::errors::ledger::{
    self, BaselineReason, ChangeMark, ClientNow, Cluster, DbGeneration, DbLedger, DbRead,
    DbSighting, EntrySighting, ErrorsLedger, FamilyFirst, LedgerUnavailable, Restored, SightMode,
    TargetLedger,
};
use crate::errors::locate::{self, Coverage as WalkCoverage};
use crate::errors::memo::{self, DbProjection, MemoLink, Projection, TargetMemo};
use crate::errors::observe::{Coverage, Observation, Observed, SourceKind, read_stable};
use crate::errors::sanitize::{family_key, identities};
use crate::errors::suspect::{
    Appeared, ChangeFact, ChangeOp, JudgeContext, JudgmentGap, JudgmentState, Suspect, Verdict,
    grade, judge, onset,
};
use crate::fsx::{FileStamp, ScopeTable};
use crate::inventory::Inventory;
use crate::limits::{
    LEDGER_PRECHANGE_MAX_BYTES, MEMO_LINKS_PER_ADDON, SUSPECT_ALGORITHM, SUSPECT_FRESH_MAX_AGE,
    SUSPECT_MAX_AGE,
};
use crate::model::{
    Actor, InstalledRelease, ReleaseTransition, RunId, Target, TargetKey, TxId, TxKind,
};
use crate::output::trace;
use crate::state::history::{self, FolderOp, HistoryEntry, HistoryFilter, HistoryRead, TxResult};
use crate::state::history_tail::{self, HistoryTail};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::TargetRecord;

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
    projected(&history_tail::fresh(paths)?, since, filter)
}

/// The restores committed on `target` at or after `since`, from the history projection
/// alone, as `status --cached` reads it (it opens nothing else): `None` when there is no
/// fresh projection, it does not reach back to `since`, or a line that may concern the
/// client does not read.
pub fn restores_in_projection(
    tail: Option<&HistoryTail>,
    target: &Target,
    since: Timestamp,
) -> Option<Vec<Restored>> {
    let Some(key) = target.key else {
        return Some(Vec::new());
    };
    let filter = HistoryFilter {
        target: Some(key),
        since: Some(since),
        ..HistoryFilter::default()
    };
    restores_of(&projected(tail?, since, &filter)?, key, since)
}

/// The history projection read with `filter`, when it reaches back to `since`.
fn projected(tail: &HistoryTail, since: Timestamp, filter: &HistoryFilter) -> Option<HistoryRead> {
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

/// Since when a restore can concern a database `ledger` (a client's, as loaded) shows:
/// the oldest of their newest proved states, else `loaded_at`. A pure function.
fn restores_horizon(ledger: Option<&TargetLedger>, loaded_at: Timestamp) -> Timestamp {
    ledger
        .into_iter()
        .flat_map(|l| l.databases.values())
        .map(|db| {
            db.reads
                .last()
                .map_or(db.started_at, |r| r.last_observed_at)
        })
        .fold(loaded_at, Timestamp::min)
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
            // The change registers an unregistered client, and its sightings are filed
            // under the key it is given ([`registered`]), where no state of its files is
            // known yet: each is parsed, so that this light read is the first read there
            // and its mark has a proved read to name.
            let compared = ledger.filter(|_| target.key.is_some());
            let dbs = files
                .into_iter()
                .filter_map(|file| look(file, compared, looked_at, &mut budget, &tz))
                .collect();
            let since = restores_horizon(ledger, loaded_at);
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
    actor: Actor,
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
    let mut regenerated: Vec<(String, Vec<String>)> = Vec::new();
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
        let sighted = session.sight(
            &client.filed_as,
            &dbs,
            restores,
            &now,
            discovered,
            SightMode::Light,
        );
        if !sighted.regenerated.is_empty() {
            regenerated.push((client.filed_as.clone(), sighted.regenerated));
        }
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
    match session.commit() {
        // Still under the evidence lock: a database whose lifetime this read started or
        // ended loses its links in the memo (design 7.3.2). It adds none: it has no
        // attribution.
        Ok(_accepted) => drop_links(paths, &regenerated, actor),
        Err(why) => {
            trace::emit(|| format!("errors ledger: the light read was not stored: {why:?}"));
        }
    }
}

/// Removes from the errors memo the links of each database a light read started or ended
/// a lifetime of (`(filed_as, database keys)`), and rings the doorbell when that changed
/// it. Best effort, as every memo write is.
fn drop_links(paths: &Paths, regenerated: &[(String, Vec<String>)], actor: Actor) {
    if regenerated.is_empty() {
        return;
    }
    let (mut memo, writable) = memo::load(paths);
    if !writable {
        return;
    }
    let mut changed = false;
    for (filed_as, keys) in regenerated {
        let Some(target) = memo.targets.get_mut(filed_as) else {
            continue;
        };
        for db in &mut target.databases {
            if !keys.contains(&db_key(&db.path)) {
                continue;
            }
            for addon in &mut db.addons {
                changed |= !addon.links.is_empty();
                addon.links.clear();
            }
            changed |= db.generation.take().is_some();
        }
    }
    if changed {
        memo::store(paths, &memo, actor);
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

// ---------------------------------------------------------------------------------------
// The full read (design 7.1.7): `errors`.

/// One client `errors` read, as the full read sights it.
pub struct FullClient<'a> {
    pub target: &'a Target,
    /// Where its memo and ledger are filed: its key, or its id until registered.
    pub filed_as: &'a str,
    /// Everything read on it, before any view filter.
    pub observation: &'a Observation,
    /// Its folders as `errors` found them, for the judge.
    pub installed: &'a Installed,
}

/// What the judge reads of a client's `AddOns/` (design 7.2.1), kept from the inventory
/// `errors` builds at `Depth::Full`: the folders, to place a stack's frames as
/// attribution does, and each folder's declared dependencies, required and optional.
/// Current evidence: what is installed when the error is judged.
#[derive(Debug, Clone, Default)]
pub struct Installed {
    pub folders: FolderSet,
    /// Each folder's dependencies, by its ASCII-folded name.
    pub requires: BTreeMap<String, Vec<String>>,
}

impl Installed {
    pub fn of(inventory: &Inventory) -> Self {
        Self {
            folders: FolderSet::new(inventory.folders.iter().map(|f| f.name.as_str())),
            requires: inventory
                .folders
                .iter()
                .filter_map(|f| {
                    let toc = f.toc.as_ref()?;
                    let deps = toc.dependencies.iter().chain(&toc.optional_deps);
                    Some((f.key.clone(), deps.cloned().collect()))
                })
                .collect(),
        }
    }

    /// Whether folder `a` declares folder `b`, ignoring case; false for a folder that is
    /// not installed.
    fn depends(&self, a: &str, b: &str) -> bool {
        self.requires
            .get(&a.to_ascii_lowercase())
            .is_some_and(|deps| deps.iter().any(|d| d.eq_ignore_ascii_case(b)))
    }
}

/// What `errors` shows `first_seen` from: the ledger its own read stored, or, when that
/// could not be stored, the ledger as it was on disk and why this read is not in it.
#[derive(Debug, Clone, Default)]
pub struct LedgerView {
    pub ledger: Option<ErrorsLedger>,
    /// Why this run's sightings are not in `ledger`; `None` when they are.
    pub missing: Option<FirstSeenReason>,
}

impl LedgerView {
    /// The view of one client, filed as `filed_as`, at the run's `now`, with its record.
    pub fn client<'a>(
        &'a self,
        filed_as: &str,
        now: Timestamp,
        record: Option<&'a TargetRecord>,
    ) -> ClientLedger<'a> {
        ClientLedger {
            readable: self.ledger.is_some(),
            ledger: self.ledger.as_ref().and_then(|l| l.targets.get(filed_as)),
            missing: self.missing,
            now,
            record,
        }
    }
}

/// One client's ledger as `errors` shows it.
#[derive(Debug, Clone, Copy)]
pub struct ClientLedger<'a> {
    /// A ledger was had at all (stored by this read, or read from the file).
    pub readable: bool,
    /// What it holds of this client; `None` when it holds nothing of it.
    pub ledger: Option<&'a TargetLedger>,
    pub missing: Option<FirstSeenReason>,
    /// The run's one time (common rule 13).
    pub now: Timestamp,
    /// The client's record now: what a link's change left installed is compared with it.
    pub record: Option<&'a TargetRecord>,
}

impl<'a> ClientLedger<'a> {
    /// The cluster that has `fingerprint` as a member in the database keyed `database`:
    /// the exact route of design 7.1.2, the only one a verdict is shown through. Sharing
    /// a family never suffices.
    fn member(&self, database: &str, fingerprint: &str) -> Option<&'a Cluster> {
        self.ledger?
            .clusters
            .values()
            .filter(|c| c.appearance.database == database)
            .filter(|c| c.fingerprints.iter().any(|f| f == fingerprint))
            .max_by_key(|c| c.appearance.found.observed_at)
    }

    /// The link from the local error `fingerprint` in `database` to the change before it
    /// (design 7.3.1), and where its judgment stands. Both come only through a cluster
    /// that has the fingerprint as a member, so an error that is not `observed` has
    /// neither; the link only from a `complete` judgment that named a change.
    pub fn after_change(
        &self,
        database: &str,
        fingerprint: &str,
    ) -> (Option<AfterChange>, Option<JudgmentInfo>) {
        if !self.readable {
            return (None, None);
        }
        let Some(cluster) = self.member(database, fingerprint) else {
            return (None, None);
        };
        let Some(verdict) = &cluster.judgment else {
            let pending = JudgmentInfo {
                state: JudgmentStanding::Pending,
                gap: None,
            };
            return (None, Some(pending));
        };
        let judgment = JudgmentInfo {
            state: match verdict.state {
                JudgmentState::Complete => JudgmentStanding::Complete,
                JudgmentState::Deferred => JudgmentStanding::Deferred,
                JudgmentState::Unsupported => JudgmentStanding::Unsupported,
            },
            gap: verdict.gap,
        };
        let link = match (&verdict.state, &verdict.suspect) {
            (JudgmentState::Complete, Some(suspect)) => Some(self.link(cluster, verdict, suspect)),
            _ => None,
        };
        (link, Some(judgment))
    }

    /// A stored suspect, with what is true now: whether the link still stands, and
    /// whether each addon is still at the release the change left it at.
    fn link(&self, cluster: &Cluster, verdict: &Verdict, suspect: &Suspect) -> AfterChange {
        let ledger = self.ledger;
        let current = ledger
            .and_then(|l| l.databases.get(&cluster.appearance.database))
            .is_some_and(|db| db.generation == cluster.appearance.generation);
        let (bounds, _) = onset(&cluster.appearance);
        let stale = self.now >= bounds.by
            && self.now.duration_since(bounds.by).unsigned_abs() >= SUSPECT_FRESH_MAX_AGE;
        let state = if !current {
            LinkState::DatabaseReplaced
        } else if stale {
            LinkState::Expired
        } else {
            LinkState::Current
        };
        AfterChange {
            tx: suspect.tx,
            run: suspect.run,
            kind: suspect.kind,
            at: suspect.at,
            actor: suspect.actor,
            relation: suspect.relation,
            confidence: suspect.confidence,
            state,
            algorithm: verdict.algorithm,
            judged_at: verdict.judged_at,
            changed: suspect
                .changed
                .iter()
                .map(|t| ChangedNow {
                    addon: t.addon.clone(),
                    change: t.change,
                    from_version: t.from.as_ref().map(|r| r.version.clone()),
                    to_version: t.to.as_ref().map(|r| r.version.clone()),
                    from_release: t.from.as_ref().and_then(|r| r.release.clone()),
                    to_release: t.to.as_ref().and_then(|r| r.release.clone()),
                    still_installed: still_installed(self.record, t, suspect.tx),
                })
                .collect(),
            evidence: suspect.evidence.clone(),
            commands: Vec::new(),
        }
    }
    /// What the ledger knows of when the local error `fingerprint` (of family `family`)
    /// began, in the database keyed `database`, which this read of it vouched for
    /// (`proved`) or not (design 7.1.2's lookup):
    ///
    /// - a member of a cluster that appeared in that database is `observed`, with the
    ///   bounds of its appearance (`suspect::onset`);
    /// - else its family decides: one watched appear before is `related` (naming that
    ///   appearance's first member), a baseline is `baseline` with its reason, one met
    ///   only in reads that proved nothing is `unknown` (`absence_unproven`);
    /// - a family the ledger does not hold is `unknown`, with why: the client is
    ///   untracked (`ledger_full`), this read was not stored (its reason), or this read
    ///   of the database could not vouch for anything (`absence_unproven`).
    ///
    /// A pure function. Nothing here ties an error to a change.
    pub fn first_seen(
        &self,
        database: &str,
        proved: bool,
        fingerprint: &str,
        family: &str,
    ) -> FirstSeenInfo {
        if !self.readable {
            return unknown(self.missing.unwrap_or(FirstSeenReason::LedgerUnavailable));
        }
        // A ledger that holds nothing of this client (its first read proved nothing yet).
        let empty = TargetLedger::default();
        let ledger = self.ledger.unwrap_or(&empty);
        if let Some(cluster) = self.member(database, fingerprint) {
            let (bounds, _) = onset(&cluster.appearance);
            return FirstSeenInfo {
                at: Some(bounds.by),
                basis: FirstSeenBasis::Observed,
                reason: None,
                observed_at: Some(cluster.appearance.found.observed_at),
                onset_after_at: bounds.after,
                onset_by_at: Some(bounds.by),
                related_fingerprint: None,
                exact: bounds.exact,
            };
        }
        match ledger.families.get(family).map(|f| &f.first) {
            Some(FamilyFirst::Observed { cluster }) => match ledger.clusters.get(cluster) {
                Some(first) => FirstSeenInfo {
                    at: Some(first.appearance.found.observed_at),
                    basis: FirstSeenBasis::Related,
                    reason: None,
                    observed_at: None,
                    onset_after_at: None,
                    onset_by_at: None,
                    related_fingerprint: first.fingerprints.first().cloned(),
                    exact: false,
                },
                // The appearance it names is gone: nothing to point at.
                None => unknown(FirstSeenReason::LedgerUnavailable),
            },
            Some(FamilyFirst::Baseline { at, reason }) => FirstSeenInfo {
                at: Some(*at),
                basis: FirstSeenBasis::Baseline,
                reason: Some(match reason {
                    BaselineReason::FirstRead => FirstSeenReason::FirstRead,
                    BaselineReason::NewGeneration => FirstSeenReason::NewGeneration,
                    BaselineReason::CoverageIncomplete => FirstSeenReason::CoverageIncomplete,
                }),
                observed_at: None,
                onset_after_at: None,
                onset_by_at: None,
                related_fingerprint: None,
                exact: false,
            },
            Some(FamilyFirst::Pending(_)) => unknown(FirstSeenReason::AbsenceUnproven),
            None if ledger.untracked => unknown(FirstSeenReason::LedgerFull),
            None => unknown(match self.missing {
                Some(reason) => reason,
                None if !proved => FirstSeenReason::AbsenceUnproven,
                None => FirstSeenReason::LedgerUnavailable,
            }),
        }
    }
}

/// Whether `record` still holds the release `transition` left its addon at (design
/// 7.3.1, by the one release identity of 7.5.1): the same release key, revision
/// included, and the same bytes where both sides know a hash or a commit
/// ([`InstalledRelease::same_artifact`]). A release without a key (a local file) must
/// also still be the one transaction `tx` installed. A removed addon, an unknown
/// transition, or a known key against an unknown one is not still installed.
pub fn still_installed(
    record: Option<&TargetRecord>,
    transition: &ReleaseTransition,
    tx: TxId,
) -> bool {
    let Some(to) = &transition.to else {
        return false;
    };
    let Some(addon) =
        record.and_then(|r| r.addons.get(&TargetRecord::addon_key(&transition.addon)))
    else {
        return false;
    };
    let now = InstalledRelease::of(addon);
    now.same_artifact(to) && (to.release.is_some() || addon.tx_id == tx)
}

/// `first_seen` of an error omaddons cannot place, for `reason`.
pub fn unknown(reason: FirstSeenReason) -> FirstSeenInfo {
    FirstSeenInfo {
        at: None,
        basis: FirstSeenBasis::Unknown,
        reason: Some(reason),
        observed_at: None,
        onset_after_at: None,
        onset_by_at: None,
        related_fingerprint: None,
        exact: false,
    }
}

/// What `observation` read, as the ledger sights it at `now` (the run's one time,
/// common rule 13): one sighting per database, with its local entries (their family
/// keys and fingerprints), whatever the view will filter. Received entries are other
/// players' errors and are not sighted. A pure function.
pub fn sightings(observation: &Observation, now: Timestamp) -> Vec<DbSighting> {
    observation
        .databases
        .iter()
        .enumerate()
        .map(|(index, db)| {
            let entries = observation
                .local()
                .filter(|o| o.database == index)
                .map(|o| EntrySighting {
                    family: family_key(&o.entry.message),
                    fingerprint: o.fingerprint.clone(),
                    session: o.entry.session,
                    time: o.evidence,
                    counter: o.entry.counter,
                })
                .collect();
            DbSighting {
                key: db_key(&db.path),
                provider: db.provider.clone(),
                stamp: db.stamp.fingerprint(),
                written_at: db.written_at,
                observed_at: now,
                session: db.session,
                read: Some(DbRead {
                    coverage: db.coverage,
                    entries,
                }),
            }
        })
        .collect()
}

/// Whether `observation` read database `index` whole enough to vouch for an error's
/// absence (design 7.1.3).
pub fn proved(observation: &Observation, index: usize) -> bool {
    observation
        .databases
        .get(index)
        .is_some_and(|db| db.coverage == Coverage::Proved)
}

/// One client of a full read, ready for the session: its sightings and the restores its
/// databases may have met (`None`: the history does not read whole).
type Prepared<'c, 'a> = (&'c FullClient<'a>, Vec<DbSighting>, Option<Vec<Restored>>);

/// `errors`' full read of `clients` (design 7.1.7): the restores each client's databases
/// may have met, read before the lock; one session under the evidence lock that sights
/// every database each read whole (`SightMode::Full`) and stores the ledger; then the
/// judgments that ledger owes, made outside any lock ([`judge_owed`]) and attached in a
/// second session ([`publish`]). `on_stored` runs with the last ledger stored while its
/// evidence lock is still held (the memo's links are written there, design 7.3.2); not
/// at all when no session stored one. Returns the ledger the view reads. What cannot be
/// had is one `-vv` line, never an error: the view then reads the ledger as it is on
/// disk (or as the first session stored it, the judgments waiting for the next run), and
/// says why an error it does not know cannot be placed.
pub fn full_read(
    paths: &Paths,
    clients: &[FullClient<'_>],
    now: Timestamp,
    command: &str,
    on_stored: &mut dyn FnMut(&ErrorsLedger),
) -> LedgerView {
    let loaded = ledger::load(paths);
    let unstored = |why: &LedgerUnavailable, loaded: Option<ErrorsLedger>| {
        trace::emit(|| format!("errors ledger: this read was not recorded: {why:?}"));
        LedgerView {
            ledger: loaded,
            missing: Some(match why {
                LedgerUnavailable::Busy => FirstSeenReason::LedgerBusy,
                LedgerUnavailable::TooLarge { .. } => FirstSeenReason::LedgerFull,
                LedgerUnavailable::Newer | LedgerUnavailable::Io(_) => {
                    FirstSeenReason::LedgerUnavailable
                }
            }),
        }
    };
    // Looked at first, the restores after: a restore that commits after this read is
    // after every look, and the next session finds it.
    let prepared: Vec<Prepared<'_, '_>> = clients
        .iter()
        .map(|client| {
            let known = loaded.as_ref().and_then(|l| l.targets.get(client.filed_as));
            let since = restores_horizon(known, now);
            (
                client,
                sightings(client.observation, now),
                restores_since(paths, client.target, since),
            )
        })
        .collect();
    let scopes = match ScopeTable::new(paths, &[], None) {
        Ok(scopes) => scopes,
        Err(err) => return unstored(&LedgerUnavailable::Io(err.message.clone()), loaded),
    };
    let mut session = match ledger::open(paths, &scopes, command) {
        Ok(session) => session,
        Err(why) => return unstored(&why, loaded),
    };
    for (client, dbs, restores) in &prepared {
        // A history that cannot be read whole may hide a restore: nothing appears.
        let (restores, discovered) = match restores {
            Some(restores) => (
                restores.as_slice(),
                client.observation.coverage == WalkCoverage::Complete,
            ),
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
        let at = ClientNow {
            version: Some(client.target.version.to_string()),
            now,
        };
        let sighted = session.sight(
            client.filed_as,
            dbs,
            restores,
            &at,
            discovered,
            SightMode::Full,
        );
        if !sighted.appeared.is_empty() {
            trace::emit(|| {
                format!(
                    "errors ledger: {}: {} newly appeared",
                    client.target.id,
                    sighted.appeared.len()
                )
            });
        }
    }
    let accepted = match session.commit() {
        Ok(accepted) => accepted,
        Err(why) => return unstored(&why, loaded),
    };
    let owes_any = clients.iter().any(|client| {
        accepted
            .ledger
            .targets
            .get(client.filed_as)
            .is_some_and(|ledger| !owed(ledger, client.observation).is_empty())
    });
    if !owes_any {
        trace::emit(|| "errors ledger: judged 0".to_owned());
        on_stored(&accepted.ledger);
        return LedgerView {
            ledger: Some(accepted.ledger.clone()),
            missing: None,
        };
    }
    // The lock is let go here: judging happens outside any lock (phase 3).
    let stored = accepted.ledger.clone();
    drop(accepted);
    let judged = judge_owed(paths, clients, &stored, now);
    let ledger = match publish(paths, &scopes, command, judged) {
        Some(accepted) => {
            on_stored(&accepted.ledger);
            accepted.ledger.clone()
        }
        None => stored,
    };
    LedgerView {
        ledger: Some(ledger),
        missing: None,
    }
}

/// What `errors` projects into the memo from `ledger` for one client (design 7.3.2), by
/// database of `observation`: each database's generation, and for each cluster that
/// appeared in it, was judged `complete` with a `high` or `medium` change, is still
/// `current` at `now`, and has a member this read holds there, a link on the addon that
/// member is filed under in that database. Nothing is keyed by addon name across
/// databases; a `low` link is never projected.
pub fn project(
    ledger: Option<&TargetLedger>,
    observation: &Observation,
    now: Timestamp,
) -> Projection {
    let Some(ledger) = ledger else {
        return Projection {
            databases: vec![DbProjection::default(); observation.databases.len()],
        };
    };
    let databases = observation
        .databases
        .iter()
        .enumerate()
        .map(|(at, db)| {
            let key = db_key(&db.path);
            let generation = ledger.databases.get(&key).map(|d| d.generation.clone());
            let mut links: BTreeMap<(String, SourceKind), Vec<MemoLink>> = BTreeMap::new();
            for (id, cluster) in &ledger.clusters {
                let Some(link) = link_of(id, cluster, generation.as_ref(), &key, now) else {
                    continue;
                };
                let members: Vec<&Observed> = observation
                    .local()
                    .filter(|o| o.database == at && cluster.fingerprints.contains(&o.fingerprint))
                    .collect();
                let Some(first) = members.first() else {
                    continue;
                };
                let session = members.iter().filter_map(|o| o.entry.session).max();
                let link = MemoLink {
                    session: session.unwrap_or(0),
                    addon: if link.addon.is_empty() {
                        first.name.clone()
                    } else {
                        link.addon
                    },
                    ..link
                };
                links
                    .entry((first.name.to_ascii_lowercase(), first.kind))
                    .or_default()
                    .push(link);
            }
            for best in links.values_mut() {
                best.sort_by(|a, b| {
                    grade(b.confidence)
                        .cmp(&grade(a.confidence))
                        .then(b.at.cmp(&a.at))
                        .then(a.cluster.cmp(&b.cluster))
                });
                best.truncate(MEMO_LINKS_PER_ADDON);
            }
            DbProjection { generation, links }
        })
        .collect();
    Projection { databases }
}

/// A cluster's link as the memo keeps it, when it qualifies: it appeared in the database
/// keyed `key`, in its `generation` (current), was judged `complete` with a `high` or
/// `medium` change, and is fresh at `now`. `session` and an unnamed `addon` are the
/// caller's to fill from the read.
fn link_of(
    id: &str,
    cluster: &Cluster,
    generation: Option<&DbGeneration>,
    key: &str,
    now: Timestamp,
) -> Option<MemoLink> {
    if cluster.appearance.database != key || generation != Some(&cluster.appearance.generation) {
        return None;
    }
    let verdict = cluster.judgment.as_ref()?;
    let suspect = verdict.suspect.as_ref()?;
    let strong = matches!(
        suspect.confidence,
        crate::errors::attribute::Confidence::High | crate::errors::attribute::Confidence::Medium
    );
    if verdict.state != JudgmentState::Complete || !strong {
        return None;
    }
    let (bounds, _) = onset(&cluster.appearance);
    let fresh_until_at = bounds.by.checked_add(SUSPECT_FRESH_MAX_AGE).ok()?;
    if now >= fresh_until_at {
        return None;
    }
    let first = suspect.changed.first();
    Some(MemoLink {
        cluster: id.to_owned(),
        fingerprints: cluster.fingerprints.clone(),
        generation: cluster.appearance.generation.clone(),
        session: 0,
        tx: suspect.tx,
        kind: suspect.kind,
        at: suspect.at,
        addon: first.map(|t| t.addon.clone()).unwrap_or_default(),
        release: first
            .and_then(|t| t.to.as_ref())
            .and_then(|r| r.release.clone()),
        relation: suspect.relation,
        confidence: suspect.confidence,
        algorithm: verdict.algorithm,
        fresh_until_at,
    })
}

/// One judgment a full read made, for its second session to attach.
struct Judged {
    filed_as: String,
    cluster: String,
    verdict: Verdict,
    /// The addon (or folder) the error is filed under.
    addon: String,
}

/// The clusters of `ledger` a full read of `observation` owes a judgment (design 7.1.7,
/// phase 2's result), each with the member it is judged by: an appearance in its
/// database's current generation, not judged yet or deferred, one of whose members this
/// read holds (the first in the cluster's sorted list).
fn owed<'c>(
    ledger: &'c TargetLedger,
    observation: &'c Observation,
) -> Vec<(&'c str, &'c Cluster, &'c Observed)> {
    let key_of: Vec<String> = observation
        .databases
        .iter()
        .map(|db| db_key(&db.path))
        .collect();
    ledger
        .clusters
        .iter()
        .filter(|(_, c)| owes(ledger, c))
        .filter_map(|(id, c)| {
            let member = c.fingerprints.iter().find_map(|f| {
                observation.local().find(|o| {
                    &o.fingerprint == f && key_of.get(o.database) == Some(&c.appearance.database)
                })
            })?;
            Some((id.as_str(), c, member))
        })
        .collect()
}

/// Whether `cluster` is still owed a judgment: its appearance is in its database's current
/// generation (an ended one is final, `sight` saw to it) and it has none, or a deferred
/// one. A complete judgment is kept as made.
fn owes(ledger: &TargetLedger, cluster: &Cluster) -> bool {
    let current = ledger
        .databases
        .get(&cluster.appearance.database)
        .is_some_and(|db| db.generation == cluster.appearance.generation);
    let open = cluster
        .judgment
        .as_ref()
        .is_none_or(|v| v.state == JudgmentState::Deferred);
    current && open
}

/// Whether a history line ran on the client keyed `key`. A line that moves folders
/// always names its client: a target-less line moves none through `ops`
/// (`HistoryEntry::well_formed`).
fn on_client(entry: &HistoryEntry, key: TargetKey) -> bool {
    entry.target == Some(key)
}

/// The folders a line added, replaced or removed: what can put code in place, or take
/// away what another addon needs. Pins, skips, backups, settings, restores and
/// `required_by` bookkeeping (`record`, `wtf` and `saved_variables` ops) move none.
fn moved_folders(entry: &HistoryEntry) -> Vec<ChangeOp> {
    entry
        .ops
        .iter()
        .filter(|o| matches!(o.op, FolderOp::Add | FolderOp::Replace | FolderOp::Remove))
        .map(|o| ChangeOp {
            op: o.op,
            folder: o.folder.to_string(),
            addon: o.addon.clone(),
        })
        .collect()
}

/// The changes on the client keyed `key` the judge may weigh (design 7.2.1): its
/// committed lines from `since` to `until` that moved a folder, oldest first, each with
/// its transitions: the line's own, else worked out from its snapshots while they last,
/// else none (`Unavailable`; nothing is made up).
fn change_facts(
    all: &[HistoryEntry],
    reader: &mut Reader<'_>,
    key: TargetKey,
    since: Timestamp,
    until: Timestamp,
) -> Vec<ChangeFact> {
    all.iter()
        .filter(|e| e.result == TxResult::Committed && on_client(e, key))
        .filter(|e| e.at >= since && e.at <= until)
        .filter_map(|e| {
            let ops = moved_folders(e);
            if ops.is_empty() {
                return None;
            }
            let (transitions, recorded) = transitions_of(e, reader);
            Some(ChangeFact {
                tx: e.tx,
                run: e.run,
                kind: e.kind,
                at: e.at,
                actor: e.actor,
                ops,
                transitions,
                recorded,
            })
        })
        .collect()
}

/// The folders some committed line on the client keyed `key` moved after `after`: what
/// the inventory says of them now is not what ran when the error appeared.
fn changed_since(all: &[HistoryEntry], key: TargetKey, after: Timestamp) -> BTreeSet<String> {
    all.iter()
        .filter(|e| e.result == TxResult::Committed && on_client(e, key) && e.at > after)
        .flat_map(moved_folders)
        .map(|o| o.folder)
        .collect()
}

/// The AddOns folder of each frame of `entry`'s stack, in order, without repeats, placed
/// as attribution places them (design 7.2.1). Frames with no path, or a path in no
/// folder, are dropped.
fn stack_folders(entry: &BugEntry, legacy: bool, folders: &FolderSet) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for frame in frames::frames(&entry.message, entry.stack.as_deref(), legacy) {
        let Some(folder) = frame
            .path
            .and_then(|path| attribute::folder_of(path, folders, legacy))
        else {
            continue;
        };
        if !out.iter().any(|f| f.eq_ignore_ascii_case(&folder)) {
            out.push(folder);
        }
    }
    out
}

/// A judgment that waits for the history: the next full read tries again.
fn deferred(now: Timestamp) -> Verdict {
    Verdict {
        algorithm: SUSPECT_ALGORITHM,
        state: JudgmentState::Deferred,
        judged_at: now,
        suspect: None,
        gap: Some(JudgmentGap::HistoryUnreadable),
    }
}

/// Phase 3 of the full read (design 7.1.7): judges the clusters `stored` owes, outside
/// any lock. The history is read here once, and only when some cluster is owed; a read
/// that fails defers every judgment. One `-vv` line says how many were judged.
fn judge_owed(
    paths: &Paths,
    clients: &[FullClient<'_>],
    stored: &ErrorsLedger,
    now: Timestamp,
) -> Vec<Judged> {
    let owed: Vec<_> = clients
        .iter()
        .filter_map(|client| {
            let ledger = stored.targets.get(client.filed_as)?;
            let owed = owed(ledger, client.observation);
            (!owed.is_empty()).then_some((client, ledger, owed))
        })
        .collect();
    let count: usize = owed.iter().map(|(_, _, o)| o.len()).sum();
    trace::emit(|| format!("errors ledger: judged {count}"));
    if count == 0 {
        return Vec::new();
    }
    let read = match history::read(paths, &HistoryFilter::default()) {
        Ok(read) => Some(read),
        Err(err) => {
            trace::emit(|| format!("errors ledger: judgments deferred: {}", err.message));
            None
        }
    };
    let mut reader = read.as_ref().map(|r| Reader::new(paths, &r.entries));
    let mut judged = Vec::new();
    for (client, ledger, clusters) in owed {
        let facts = match (&read, reader.as_mut(), client.target.key) {
            (Some(read), Some(reader), Some(key)) => {
                let bounds = clusters.iter().map(|(_, c, _)| onset(&c.appearance).0.by);
                let until = bounds.clone().max().unwrap_or(now);
                let window =
                    SignedDuration::try_from(SUSPECT_MAX_AGE).unwrap_or(SignedDuration::MAX);
                let since = bounds
                    .min()
                    .and_then(|by| by.checked_sub(window).ok())
                    .unwrap_or(Timestamp::MIN);
                Some(change_facts(&read.entries, reader, key, since, until))
            }
            // An unregistered client: no history line names it.
            (Some(_), Some(_), None) => Some(Vec::new()),
            _ => None,
        };
        for (id, cluster, member) in clusters {
            let verdict = match (&facts, &read) {
                (Some(facts), Some(read)) => {
                    judge_cluster(client, ledger, id, cluster, member, facts, read, now)
                }
                _ => deferred(now),
            };
            judged.push(Judged {
                filed_as: client.filed_as.to_owned(),
                cluster: id.to_owned(),
                verdict,
                addon: member.name.clone(),
            });
        }
    }
    judged
}

/// Judges one cluster from its member's attribution and stack, the client's folders and
/// its changes (design 7.2.1).
#[expect(
    clippy::too_many_arguments,
    reason = "the judge's inputs, each from another place"
)]
fn judge_cluster(
    client: &FullClient<'_>,
    ledger: &TargetLedger,
    id: &str,
    cluster: &Cluster,
    member: &Observed,
    facts: &[ChangeFact],
    read: &HistoryRead,
    now: Timestamp,
) -> Verdict {
    let Some(db) = ledger.databases.get(&cluster.appearance.database) else {
        return deferred(now);
    };
    let legacy = client
        .observation
        .databases
        .get(member.database)
        .is_some_and(|d| d.shape == DbShape::Legacy);
    let error = Appeared {
        cluster: id,
        appearance: &cluster.appearance,
        addon: &member.name,
        managed: member.managed,
        folder: member.folder.as_deref(),
        attribution: member.attribution.confidence,
        stack_folders: stack_folders(&member.entry, legacy, &client.installed.folders),
    };
    let since = match client.target.key {
        Some(key) => changed_since(&read.entries, key, cluster.appearance.found.observed_at),
        None => BTreeSet::new(),
    };
    let depends = |a: &str, b: &str| client.installed.depends(a, b);
    let cx = JudgeContext {
        now,
        generation: &db.generation,
        marks: &db.marks,
        databases: ledger.databases.len(),
        depends: &depends,
        changed_since: &since,
    };
    judge(&error, facts, &cx)
}

/// Phase 4 of the full read (design 7.1.7): a second session attaches each judgment to
/// its cluster, under the rule of 7.1.6, and stores the ledger. `None` when the session
/// cannot be had: the judgments wait for the next read.
fn publish(
    paths: &Paths,
    scopes: &ScopeTable,
    command: &str,
    judged: Vec<Judged>,
) -> Option<ledger::Accepted> {
    let mut session = match ledger::open(paths, scopes, command) {
        Ok(session) => session,
        Err(why) => {
            trace::emit(|| format!("errors ledger: judgments not stored: {why:?}"));
            return None;
        }
    };
    for j in judged {
        session.attach(&j.filed_as, &j.cluster, j.verdict, Some(j.addon));
    }
    match session.commit() {
        Ok(accepted) => Some(accepted),
        Err(why) => {
            trace::emit(|| format!("errors ledger: judgments not stored: {why:?}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Actor, AddonRecord, FolderName, TransitionSource, TxId, TxKind};
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
            transitions: Vec::new(),
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
            commit(&self.paths, captured, run, &|_| after, "test", Actor::Human);
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
        commit(
            &client.paths,
            captured,
            second,
            &|_| Some(false),
            "test",
            Actor::Human,
        );
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
        commit(
            &client.paths,
            captured,
            run,
            &|_| Some(false),
            "test",
            Actor::Human,
        );
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
            Actor::Human,
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
        commit(
            &client.paths,
            captured,
            run,
            &|_| Some(false),
            "test",
            Actor::Human,
        );
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
                        generation: None,
                    }],
                    ledger_revision: None,
                    projected_at: None,
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
            Actor::Human,
        );
    }

    /// An entry of family `family` and fingerprint `fingerprint`, in `session`, once,
    /// at the unix time `seconds` (its last occurrence).
    fn entry(family: &str, fingerprint: &str, session: u32, seconds: i64) -> EntrySighting {
        EntrySighting {
            family: family.to_owned(),
            fingerprint: fingerprint.to_owned(),
            session: Some(session),
            time: crate::errors::buggrabber::TimeEvidence {
                at: Some(at(seconds)),
                meaning: crate::errors::buggrabber::TimeMeaning::Last,
                tz: crate::errors::buggrabber::TzEvidence::Explicit,
            },
            counter: 1,
        }
    }

    /// Database `d1` in state `stamp`, written at `written`, read at `observed` in
    /// `session`: proved, or degraded by a dropped entry.
    fn read(
        stamp: &str,
        written: i64,
        observed: i64,
        session: u32,
        proved: bool,
        entries: Vec<EntrySighting>,
    ) -> DbSighting {
        DbSighting {
            key: "d1".to_owned(),
            provider: "!BugGrabber".to_owned(),
            stamp: stamp.to_owned(),
            written_at: Some(at(written)),
            observed_at: at(observed),
            session: Some(session),
            read: Some(DbRead {
                coverage: if proved {
                    Coverage::Proved
                } else {
                    Coverage::Degraded(crate::errors::observe::Gap::Dropped)
                },
                entries,
            }),
        }
    }

    /// The ledger after three full reads of `d1`: A alone (the first read), then A and
    /// B (B appeared), then a read that could not vouch for anything holding P.
    fn watched() -> TargetLedger {
        let mut ledger = TargetLedger::default();
        let full = |ledger: &mut TargetLedger, db: DbSighting, now: i64| {
            let client = ClientNow {
                version: Some("12.1.5.70391".to_owned()),
                now: at(now),
            };
            ledger::sight(ledger, &[db], &[], &client, true, SightMode::Full);
        };
        full(
            &mut ledger,
            read("s1", 100, 110, 4, true, vec![entry("a:a", "e:a1", 4, 90)]),
            110,
        );
        full(
            &mut ledger,
            read(
                "s2",
                200,
                210,
                5,
                true,
                vec![entry("a:a", "e:a1", 4, 90), entry("a:b", "e:b1", 5, 150)],
            ),
            210,
        );
        full(
            &mut ledger,
            read(
                "s3",
                300,
                310,
                6,
                false,
                vec![
                    entry("a:a", "e:a1", 4, 90),
                    entry("a:b", "e:b1", 5, 150),
                    entry("a:p", "e:p1", 6, 290),
                ],
            ),
            310,
        );
        ledger
    }

    #[test]
    fn first_seen_follows_the_ledger_s_lookup_and_never_more() {
        use FirstSeenBasis::{Baseline, Observed, Related, Unknown};
        use FirstSeenReason::{
            AbsenceUnproven, FirstRead, LedgerBusy, LedgerFull, LedgerUnavailable,
        };
        let ledger = watched();
        let mut untracked = ledger.clone();
        untracked.untracked = true;
        let known = |missing| ClientLedger {
            readable: true,
            ledger: Some(&ledger),
            missing,
            now: at(400),
            record: None,
        };
        let none = |readable, missing| ClientLedger {
            readable,
            ledger: None,
            missing,
            now: at(400),
            record: None,
        };
        // (case, view, database, proved, fingerprint, family, basis, reason, at)
        type Case<'a> = (
            &'a str,
            ClientLedger<'a>,
            &'a str,
            bool,
            &'a str,
            &'a str,
            FirstSeenBasis,
            Option<FirstSeenReason>,
            Option<Timestamp>,
        );
        let cases: Vec<Case<'_>> = vec![
            (
                "there at the first read",
                known(None),
                "d1",
                true,
                "e:a1",
                "a:a",
                Baseline,
                Some(FirstRead),
                Some(at(110)),
            ),
            (
                "the bug moved: another line of a baseline family",
                known(None),
                "d1",
                true,
                "e:a9",
                "a:a",
                Baseline,
                Some(FirstRead),
                Some(at(110)),
            ),
            (
                "watched appear",
                known(None),
                "d1",
                true,
                "e:b1",
                "a:b",
                Observed,
                None,
                Some(at(150)),
            ),
            (
                "another line of an observed family",
                known(None),
                "d1",
                true,
                "e:b2",
                "a:b",
                Related,
                None,
                Some(at(210)),
            ),
            (
                "a member in another database is only related",
                known(None),
                "d2",
                true,
                "e:b1",
                "a:b",
                Related,
                None,
                Some(at(210)),
            ),
            (
                "met only in a read that proved nothing",
                known(None),
                "d1",
                false,
                "e:p1",
                "a:p",
                Unknown,
                Some(AbsenceUnproven),
                None,
            ),
            (
                "unknown to the ledger, this read degraded",
                known(None),
                "d1",
                false,
                "e:x1",
                "a:x",
                Unknown,
                Some(AbsenceUnproven),
                None,
            ),
            (
                "unknown to a ledger that holds this read",
                known(None),
                "d1",
                true,
                "e:x1",
                "a:x",
                Unknown,
                Some(LedgerUnavailable),
                None,
            ),
            (
                "unknown while the ledger was busy",
                known(Some(LedgerBusy)),
                "d1",
                true,
                "e:x1",
                "a:x",
                Unknown,
                Some(LedgerBusy),
                None,
            ),
            (
                "known while the ledger was busy: as it has it",
                known(Some(LedgerBusy)),
                "d1",
                true,
                "e:a1",
                "a:a",
                Baseline,
                Some(FirstRead),
                Some(at(110)),
            ),
            (
                "an untracked client",
                ClientLedger {
                    readable: true,
                    ledger: Some(&untracked),
                    missing: None,
                    now: at(400),
                    record: None,
                },
                "d1",
                true,
                "e:x1",
                "a:x",
                Unknown,
                Some(LedgerFull),
                None,
            ),
            (
                "a ledger holding nothing of the client, this read degraded",
                none(true, None),
                "d1",
                false,
                "e:a1",
                "a:a",
                Unknown,
                Some(AbsenceUnproven),
                None,
            ),
            (
                "a ledger holding nothing of the client, busy",
                none(true, Some(LedgerBusy)),
                "d1",
                true,
                "e:a1",
                "a:a",
                Unknown,
                Some(LedgerBusy),
                None,
            ),
            (
                "no ledger, busy",
                none(false, Some(LedgerBusy)),
                "d1",
                true,
                "e:a1",
                "a:a",
                Unknown,
                Some(LedgerBusy),
                None,
            ),
            (
                "no ledger at all",
                none(false, None),
                "d1",
                true,
                "e:a1",
                "a:a",
                Unknown,
                Some(LedgerUnavailable),
                None,
            ),
        ];
        for (case, view, database, proved, fingerprint, family, basis, reason, when) in cases {
            let got = view.first_seen(database, proved, fingerprint, family);
            assert_eq!(
                (got.basis, got.reason, got.at),
                (basis, reason, when),
                "{case}: {got:?}"
            );
            // Bounds and a pointer only where the basis has them.
            assert_eq!(got.onset_by_at.is_some(), basis == Observed, "{case}");
            assert_eq!(got.observed_at.is_some(), basis == Observed, "{case}");
            assert_eq!(
                got.related_fingerprint.is_some(),
                basis == Related,
                "{case}"
            );
        }
        // The appearance's bounds: absent from the file written at 100, present by its
        // own trusted time (150, before the file holding it was written at 200), found
        // by the read at 210; one occurrence at a trusted time is exact.
        let b = known(None).first_seen("d1", true, "e:b1", "a:b");
        assert_eq!(
            (b.onset_after_at, b.onset_by_at, b.observed_at, b.exact),
            (Some(at(100)), Some(at(150)), Some(at(210)), true)
        );
        let related = known(None).first_seen("d1", true, "e:b2", "a:b");
        assert_eq!(related.related_fingerprint.as_deref(), Some("e:b1"));
    }

    #[test]
    fn restores_horizon_is_the_oldest_newest_read() {
        let ledger = watched();
        assert_eq!(restores_horizon(None, at(1_000)), at(1_000));
        assert_eq!(restores_horizon(Some(&ledger), at(1_000)), at(210));
        assert_eq!(restores_horizon(Some(&ledger), at(50)), at(50));
    }

    // -----------------------------------------------------------------------------------
    // The judge's adapter and the link's view (RG-16).

    #[test]
    fn the_judge_weighs_the_committed_lines_on_its_client_that_moved_a_folder() {
        let (_dir, paths) = home();
        let key: TargetKey = KEY.parse().unwrap();
        write_history(
            &paths,
            &[
                line(
                    KEY,
                    100,
                    TxKind::Install,
                    vec![op(FolderOp::Add, "Foo"), op(FolderOp::Record, "Foo")],
                ),
                line(KEY, 110, TxKind::Pin, vec![op(FolderOp::Record, "Foo")]),
                line(KEY, 120, TxKind::Restore, vec![op(FolderOp::Wtf, "WTF")]),
                line(
                    OTHER,
                    130,
                    TxKind::Update,
                    vec![op(FolderOp::Replace, "Foo")],
                ),
                line(KEY, 140, TxKind::Remove, vec![op(FolderOp::Remove, "Bar")]),
                line(KEY, 500, TxKind::Update, vec![op(FolderOp::Replace, "Lib")]),
            ],
        );
        let read = history::read(&paths, &HistoryFilter::default()).unwrap();
        let mut reader = Reader::new(&paths, &read.entries);
        let facts = change_facts(&read.entries, &mut reader, key, at(0), at(400));
        let got: Vec<_> = facts
            .iter()
            .map(|f| {
                let ops: Vec<_> = f.ops.iter().map(|o| (o.op, o.folder.as_str())).collect();
                (f.at, f.kind, ops)
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (at(100), TxKind::Install, vec![(FolderOp::Add, "Foo")]),
                (at(140), TxKind::Remove, vec![(FolderOp::Remove, "Bar")]),
            ],
            "pins, restores, another client's lines and lines past the window are not weighed"
        );
        assert_eq!(
            facts[0].recorded,
            TransitionSource::Unavailable,
            "a line from before transitions whose snapshots are gone: nothing made up"
        );
        assert!(facts[0].transitions.is_empty());
        assert_eq!(
            changed_since(&read.entries, key, at(115)),
            ["Bar".to_owned(), "Lib".to_owned()].into()
        );
    }

    #[test]
    fn a_cluster_is_owed_a_judgment_until_one_completes_or_its_generation_ends() {
        let ledger = watched();
        let (id, cluster) = ledger.clusters.iter().next().unwrap();
        assert!(owes(&ledger, cluster), "not judged yet");
        let judged = |state| {
            let mut judged = ledger.clone();
            judged.clusters.get_mut(id).unwrap().judgment = Some(Verdict {
                algorithm: SUSPECT_ALGORITHM,
                state,
                judged_at: at(300),
                suspect: None,
                gap: None,
            });
            judged
        };
        for (state, want) in [
            (JudgmentState::Deferred, true),
            (JudgmentState::Complete, false),
            (JudgmentState::Unsupported, false),
        ] {
            let judged = judged(state);
            assert_eq!(owes(&judged, &judged.clusters[id]), want, "{state:?}");
        }
        let mut ended = ledger.clone();
        ended.databases.get_mut("d1").unwrap().generation =
            crate::errors::ledger::DbGeneration("g:later".to_owned());
        assert!(!owes(&ended, &ended.clusters[id]), "its generation ended");
    }

    #[test]
    fn a_stack_names_each_of_its_folders_once_in_order() {
        use crate::errors::buggrabber::EntryType;
        let stack = [
            "[string \"@Interface/AddOns/GuidePack/guide.lua\"]:12: in function <x>",
            "[string \"@Interface/AddOns/Guidelime/core.lua\"]:40: in function 'y'",
            "[C]: in function 'pcall'",
            "[string \"@Interface/AddOns/guidepack/other.lua\"]:3: in function 'z'",
            "[string \"@Interface/FrameXML/UIParent.lua\"]:1: in function 'w'",
        ];
        let entry = BugEntry {
            message: "[Interface/AddOns/GuidePack/guide.lua]:12: attempt to index a nil value"
                .to_owned(),
            stack: Some(stack.join("\n")),
            locals: None,
            session: Some(3),
            time: None,
            counter: 1,
            entry_type: EntryType::Error,
            origin: Origin::Local,
        };
        let folders = FolderSet::new(["!BugGrabber", "GuidePack", "Guidelime"]);
        assert_eq!(
            stack_folders(&entry, false, &folders),
            vec!["GuidePack".to_owned(), "Guidelime".to_owned()]
        );
    }

    const TX: &str = "01M9Q00000000000000000000A";

    /// Foo at `version` of `gh:o/foo` (revision `revision`, archive `blob`), installed by
    /// transaction `tx`.
    fn foo_at(version: &str, revision: Option<u32>, blob: Option<&str>, tx: &str) -> AddonRecord {
        AddonRecord {
            r#ref: "gh:o/foo".parse().unwrap(),
            name: "Foo".into(),
            version: version.into(),
            version_confidence: crate::model::VersionConfidence::Release,
            release_id: Some(format!("gh:o/foo@{version}")),
            channel: crate::model::Channel::Stable,
            pinned: false,
            reason: crate::model::AddonReason::Explicit,
            required_by: vec![],
            folders: vec![FolderName::new("Foo").unwrap()],
            state: crate::model::AddonState::Ok,
            installed_at: at(0),
            tx_id: tx.parse().unwrap(),
            blob_sha256: blob.map(str::to_owned),
            revision,
            left_out: Vec::new(),
            any_flavor: false,
            skipped_releases: Vec::new(),
        }
    }

    /// Foo installed from a local zip whose content hashes to `content`.
    fn foo_local(version: &str, content: &str, tx: &str) -> AddonRecord {
        AddonRecord {
            r#ref: "file:/x/foo.zip".parse().unwrap(),
            release_id: Some(format!("file:/x/foo.zip@{content}")),
            ..foo_at(version, None, None, tx)
        }
    }

    fn holding(addons: Vec<AddonRecord>) -> TargetRecord {
        let mut record = TargetRecord::empty(KEY.parse().unwrap());
        for a in addons {
            record.addons.insert(TargetRecord::addon_key(&a.name), a);
        }
        record
    }

    /// The change that took Foo to `to`.
    fn to(to: Option<&AddonRecord>) -> ReleaseTransition {
        ReleaseTransition {
            addon: "Foo".to_owned(),
            change: crate::model::TransitionChange::Updated,
            from: None,
            to: to.map(InstalledRelease::of),
        }
    }

    #[test]
    fn still_installed_is_the_same_artifact_the_change_left() {
        let tx: TxId = TX.parse().unwrap();
        let other = "01M9Q00000000000000000000B";
        let left = foo_at("2", Some(5), Some("bb"), TX);
        let local = foo_local("2", "c1", TX);
        // (case, the record now, what the change left, still installed)
        let cases: Vec<(&str, Option<AddonRecord>, ReleaseTransition, bool)> = vec![
            (
                "the same release, revision and archive",
                Some(left.clone()),
                to(Some(&left)),
                true,
            ),
            (
                "a re-tag: the same tag at another revision",
                Some(foo_at("2", Some(6), Some("bb"), other)),
                to(Some(&left)),
                false,
            ),
            (
                "the same tag from an archive with another hash",
                Some(foo_at("2", Some(5), Some("cc"), other)),
                to(Some(&left)),
                false,
            ),
            (
                "an archive hash only one side knows decides nothing",
                Some(foo_at("2", Some(5), None, TX)),
                to(Some(&left)),
                true,
            ),
            (
                "another release since",
                Some(foo_at("3", Some(5), Some("bb"), other)),
                to(Some(&left)),
                false,
            ),
            ("the addon removed since", None, to(Some(&left)), false),
            ("the change removed it", Some(left.clone()), to(None), false),
            (
                "a known key against none",
                Some(local.clone()),
                to(Some(&left)),
                false,
            ),
            (
                "a local file the change installed",
                Some(local.clone()),
                to(Some(&local)),
                true,
            ),
            (
                "the same local file, installed again since",
                Some(foo_local("2", "c1", other)),
                to(Some(&local)),
                false,
            ),
        ];
        for (case, now, transition, want) in cases {
            let record = holding(now.into_iter().collect());
            assert_eq!(
                still_installed(Some(&record), &transition, tx),
                want,
                "{case}"
            );
        }
        assert!(!still_installed(None, &to(Some(&left)), tx), "no record");
    }

    /// `watched()` with its one cluster given `verdict`.
    fn judged(verdict: Option<Verdict>) -> (TargetLedger, String) {
        let mut ledger = watched();
        let id = ledger.clusters.keys().next().unwrap().clone();
        ledger.clusters.get_mut(&id).unwrap().judgment = verdict;
        (ledger, id)
    }

    fn verdict(state: JudgmentState, suspect: Option<Suspect>) -> Verdict {
        Verdict {
            algorithm: SUSPECT_ALGORITHM,
            state,
            judged_at: at(400),
            suspect,
            gap: (state == JudgmentState::Deferred).then_some(JudgmentGap::HistoryUnreadable),
        }
    }

    /// Foo's update to 2 at 120, `own` and `high`, the first session after it.
    fn suspect(tx: TxId) -> Suspect {
        use crate::errors::suspect::{Evidence, Relation};
        let from = foo_at("1", None, Some("aa"), TX);
        let left = foo_at("2", None, Some("bb"), TX);
        Suspect {
            tx,
            run: None,
            kind: TxKind::Update,
            at: at(120),
            actor: Actor::Human,
            relation: Relation::Own,
            confidence: crate::errors::attribute::Confidence::High,
            changed: vec![ReleaseTransition {
                from: Some(InstalledRelease::of(&from)),
                ..to(Some(&left))
            }],
            evidence: Evidence {
                gap_s: 30,
                exact: false,
                same_session: Some(false),
                clean_sessions: Some(0),
                client_update: None,
                other_changes: 0,
                unknowns: Vec::new(),
            },
        }
    }

    fn view<'a>(
        ledger: &'a TargetLedger,
        now: i64,
        record: Option<&'a TargetRecord>,
    ) -> ClientLedger<'a> {
        ClientLedger {
            readable: true,
            ledger: Some(ledger),
            missing: None,
            now: at(now),
            record,
        }
    }

    #[test]
    fn a_link_shows_only_through_a_member_with_what_is_true_now() {
        let tx: TxId = TX.parse().unwrap();
        let (ledger, _) = judged(Some(verdict(JudgmentState::Complete, Some(suspect(tx)))));
        let record = holding(vec![foo_at("2", None, Some("bb"), TX)]);
        let (link, judgment) = view(&ledger, 400, Some(&record)).after_change("d1", "e:b1");
        let link = link.unwrap();
        assert_eq!(
            (link.tx, link.state, link.judged_at, link.algorithm),
            (tx, LinkState::Current, at(400), SUSPECT_ALGORITHM)
        );
        let changed = &link.changed[0];
        assert_eq!(
            (
                changed.from_version.as_deref(),
                changed.to_version.as_deref(),
                changed.still_installed
            ),
            (Some("1"), Some("2"), true)
        );
        assert!(link.commands.is_empty());
        assert_eq!(
            judgment,
            Some(JudgmentInfo {
                state: JudgmentStanding::Complete,
                gap: None
            })
        );
        // Only an exact member: another error of the family, or another database, gets
        // neither the link nor a judgment.
        assert_eq!(
            view(&ledger, 400, Some(&record)).after_change("d1", "e:b2"),
            (None, None)
        );
        assert_eq!(
            view(&ledger, 400, Some(&record)).after_change("d2", "e:b1"),
            (None, None)
        );
        let unreadable = ClientLedger {
            readable: false,
            ..view(&ledger, 400, Some(&record))
        };
        assert_eq!(unreadable.after_change("d1", "e:b1"), (None, None));

        // What is true now: its release moved on, 30 days passed, the file was replaced.
        let moved = holding(vec![foo_at("3", None, Some("cc"), TX)]);
        let state_at = |ledger: &TargetLedger, now: i64| {
            view(ledger, now, Some(&record))
                .after_change("d1", "e:b1")
                .0
                .unwrap()
                .state
        };
        let link_now = view(&ledger, 400, Some(&moved))
            .after_change("d1", "e:b1")
            .0;
        assert!(!link_now.unwrap().changed[0].still_installed);
        let thirty_days = 30 * 24 * 60 * 60;
        // Its onset is bounded by the entry's trusted time, 150.
        assert_eq!(state_at(&ledger, 150 + thirty_days - 1), LinkState::Current);
        assert_eq!(state_at(&ledger, 150 + thirty_days), LinkState::Expired);
        let mut replaced = ledger.clone();
        replaced.databases.get_mut("d1").unwrap().generation =
            crate::errors::ledger::DbGeneration("g:later".to_owned());
        assert_eq!(state_at(&replaced, 400), LinkState::DatabaseReplaced);
    }

    #[test]
    fn a_re_tag_reads_as_two_releases_under_one_label() {
        let tx: TxId = TX.parse().unwrap();
        let (before, after) = (
            foo_at("2", Some(5), Some("aa"), TX),
            foo_at("2", Some(6), Some("bb"), TX),
        );
        let mut retag = suspect(tx);
        retag.changed = vec![ReleaseTransition {
            from: Some(InstalledRelease::of(&before)),
            ..to(Some(&after))
        }];
        let (ledger, _) = judged(Some(verdict(JudgmentState::Complete, Some(retag))));
        let now = holding(vec![after.clone()]);
        let link = view(&ledger, 400, Some(&now))
            .after_change("d1", "e:b1")
            .0
            .unwrap();
        let changed = &link.changed[0];
        assert_eq!(
            changed.from_version, changed.to_version,
            "one version label"
        );
        let revision = |r: &Option<crate::model::ReleaseKey>| r.as_ref().and_then(|k| k.revision);
        assert_eq!(
            (
                revision(&changed.from_release),
                revision(&changed.to_release)
            ),
            (Some(5), Some(6)),
            "two releases"
        );
        assert!(changed.still_installed);
        let back = holding(vec![before]);
        let gone = view(&ledger, 400, Some(&back)).after_change("d1", "e:b1").0;
        assert!(
            !gone.unwrap().changed[0].still_installed,
            "the earlier revision of the same tag is not the release the change left"
        );
    }

    #[test]
    fn an_observed_error_says_where_its_judgment_stands() {
        let standing = |judgment: Option<Verdict>| {
            let (ledger, _) = judged(judgment);
            view(&ledger, 400, None).after_change("d1", "e:b1")
        };
        let info = |state, gap| Some(JudgmentInfo { state, gap });
        assert_eq!(
            standing(None),
            (None, info(JudgmentStanding::Pending, None))
        );
        assert_eq!(
            standing(Some(verdict(JudgmentState::Deferred, None))),
            (
                None,
                info(
                    JudgmentStanding::Deferred,
                    Some(JudgmentGap::HistoryUnreadable)
                )
            )
        );
        assert_eq!(
            standing(Some(verdict(JudgmentState::Complete, None))),
            (None, info(JudgmentStanding::Complete, None)),
            "judged, and no change qualified"
        );
        let mut unsupported = verdict(JudgmentState::Unsupported, None);
        unsupported.gap = Some(JudgmentGap::ProviderChanged);
        assert_eq!(
            standing(Some(unsupported)),
            (
                None,
                info(
                    JudgmentStanding::Unsupported,
                    Some(JudgmentGap::ProviderChanged)
                )
            )
        );
    }

    proptest::proptest! {
        /// Design 7.1.2's lookup: an error is given a cluster's verdict only when its exact
        /// fingerprint is one of that cluster's members, in the same database. Sharing a
        /// family never suffices.
        #[test]
        fn a_verdict_is_shown_only_for_an_exact_member(
            clusters in proptest::collection::vec(
                (0usize..2, 0usize..3, proptest::collection::btree_set(0usize..6, 1..4), proptest::bool::ANY),
                0..5,
            ),
            entries in proptest::collection::vec((0usize..2, 0usize..6), 1..12),
        ) {
            use crate::errors::ledger::{Appearance, ReadMark};
            let generation = crate::errors::ledger::DbGeneration("g:1".to_owned());
            let mut ledger = TargetLedger::default();
            for db in ["d0", "d1"] {
                ledger.databases.insert(db.to_owned(), DbLedger {
                    generation: generation.clone(),
                    started_at: at(0),
                    max_session: Some(9),
                    max_written_at: None,
                    invalidated_at: None,
                    reads: Vec::new(),
                    unproved_stamp: None,
                    marks: Vec::new(),
                });
            }
            let mark = |observed: i64| ReadMark {
                stamp: format!("s{observed}"),
                written_at: Some(at(observed - 5)),
                session: Some(3),
                observed_at: at(observed),
                client_version: None,
            };
            for (i, (db, family, members, judged)) in clusters.iter().enumerate() {
                let n = i64::try_from(i).unwrap();
                let tx = format!("01M9Q{i:021}").parse().unwrap();
                ledger.clusters.insert(format!("c:{i}"), Cluster {
                    family: format!("a:{family}"),
                    appearance: Appearance {
                        database: format!("d{db}"),
                        generation: generation.clone(),
                        provider: "!BugGrabber".to_owned(),
                        before: mark(100 + n),
                        found: mark(200 + n),
                        earliest: None,
                        entry_session: Some(4),
                        time: crate::errors::buggrabber::TimeEvidence {
                            at: None,
                            meaning: crate::errors::buggrabber::TimeMeaning::Unknown,
                            tz: crate::errors::buggrabber::TzEvidence::SystemAssumed,
                        },
                        counter: 1,
                    },
                    fingerprints: members.iter().map(|m| format!("e:{m}")).collect(),
                    overflowed: false,
                    judgment: judged.then(|| verdict(JudgmentState::Complete, Some(suspect(tx)))),
                    addon: None,
                });
            }
            for (db, f) in entries {
                let (database, fingerprint) = (format!("d{db}"), format!("e:{f}"));
                let (link, judgment) = view(&ledger, 400, None).after_change(&database, &fingerprint);
                let members: Vec<&Cluster> = ledger
                    .clusters
                    .values()
                    .filter(|c| c.appearance.database == database)
                    .filter(|c| c.fingerprints.contains(&fingerprint))
                    .collect();
                if members.is_empty() {
                    proptest::prop_assert_eq!((&link, judgment), (&None, None));
                }
                if let Some(link) = link {
                    let named = members.iter().any(|c| {
                        c.judgment
                            .as_ref()
                            .and_then(|v| v.suspect.as_ref())
                            .is_some_and(|s| s.tx == link.tx)
                    });
                    proptest::prop_assert!(named, "a link no member's verdict names");
                }
            }
        }
    }
}
