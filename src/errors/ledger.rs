//! The errors ledger (design 7.1): which errors each client's BugGrabber databases have
//! held and when each first appeared, so an error omaddons watched appear can be told
//! from one that happened again. Keys, sessions, stamps and times only: never a message,
//! a stack or an account name.
//!
//! Three identities, each with one job (design 7.1.2). An error's **fingerprint**
//! (`e:`) is exact, and is what the wire shows. Its **family** (`a:`,
//! [`crate::errors::sanitize::family_key`]) answers only "was something of this shape
//! already known on this client", and may only suppress a regression claim. A
//! **cluster** (`c:`, [`cluster_id`]) is one watched appearance: the exact fingerprints
//! of a family's entries in the read that found it, which alone carry its verdict.
//!
//! This module holds the ledger's vocabulary and identities; [`sight`], the one pure
//! function that updates it; and its store: [`open`] takes the evidence lock and loads,
//! a [`LedgerSession`] applies what a process saw as input, and [`LedgerSession::commit`]
//! writes one whole revision. [`load`] is for display. There is no merge: one writer at
//! a time (design 7.1.6).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;
use crate::errors::buggrabber::{TimeEvidence, TimeMeaning, TzEvidence};
use crate::errors::observe::Coverage;
use crate::errors::sanitize::short_digest;
use crate::errors::suspect::{JudgmentGap, JudgmentState, Verdict};
use crate::fsx::{
    Content, Lease, LeaseError, LeaseGuard, Op, ScopeTable, WriteSession, Writer, ensure_dir_all,
    failpoint, mutate,
};
use crate::limits::{
    LEDGER_ABSENT_MAX, LEDGER_ABSENT_MAX_AGE, LEDGER_CLUSTER_FINGERPRINTS_MAX, LEDGER_FAMILIES_MAX,
    LEDGER_IDENTITY, LEDGER_LOCK_TIMEOUT, LEDGER_MARKS_MAX, LEDGER_MAX_BYTES, LEDGER_READS_MAX,
    RACY_SLACK_COARSE_NS, SUSPECT_ALGORITHM, SUSPECT_ALGORITHM_TRUSTED_MIN, SUSPECT_MAX_AGE,
};
use crate::model::RunId;
use crate::state::paths::Paths;
use crate::state::store::{self, SchemaVersion, VersionStatus, Versioned};

/// `<cache>/errors_ledger.json`: which errors each client's BugGrabber databases have
/// held and when each first appeared, so a newly appeared error can be told from one
/// that happened again. Keys, sessions and times only: never a message, a stack or an
/// account name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorsLedger {
    /// The version of `family_key` the keys below were made with (`LEDGER_IDENTITY`). A
    /// ledger made with another version is discarded: every error baselines again.
    #[serde(default)]
    pub identity: u32,
    /// Goes up by one at every accepted store. A memo link names the revision it was
    /// projected from.
    #[serde(default)]
    pub revision: u64,
    /// Keyed exactly as the errors memo keys its targets: the target's key, or its id
    /// while it is unregistered.
    #[serde(default)]
    pub targets: BTreeMap<String, TargetLedger>,
}

impl Versioned for ErrorsLedger {
    const KIND: &'static str = "errors_ledger";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// What the ledger knows of one client.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TargetLedger {
    /// By database key (`errors::ack::db_key`: a hash of the file's path, never the
    /// path).
    #[serde(default)]
    pub databases: BTreeMap<String, DbLedger>,
    /// Recurrence memory, by family key. A family known in any database of the client is
    /// known.
    #[serde(default)]
    pub families: BTreeMap<String, Family>,
    /// The appearances omaddons watched, by cluster id. Only these are judged.
    #[serde(default)]
    pub clusters: BTreeMap<String, Cluster>,
    /// The client's families did not fit `LEDGER_FAMILIES_MAX`: nothing is recorded for
    /// it until they do. Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub untracked: bool,
}

/// One lifetime of a database file. Session numbers mean something only inside one:
/// nothing compares a session of one generation with a session of another.
/// `g:` plus 16 hex digits of a hash of the database key, the stamp of the read that
/// started it and that read's time ([`DbGeneration::of`]). Opaque: only equality is
/// used. Never a bare counter, so a ledger that was deleted and rebuilt cannot reuse an
/// old id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct DbGeneration(pub String);

impl DbGeneration {
    /// The generation a proved read of `database` starts: its stamp
    /// (`FileStamp::fingerprint`) and the time it was observed, with the database key,
    /// hashed. A pure function of its inputs.
    pub fn of(database: &str, stamp: &str, observed_at: Timestamp) -> Self {
        let at = observed_at.as_nanosecond().to_string();
        Self(format!("g:{}", short_digest(&[database, stamp, &at])))
    }
}

/// A watched appearance's id: `c:` plus 16 hex digits of a hash of its family key, its
/// database key and generation, the stamp of the read that found it, and its member
/// fingerprints, sorted and without repeats, so their order never matters. Opaque: only
/// equality is used. Because it holds the generation and the found stamp, a verdict
/// computed for one appearance can never be attached to another.
pub fn cluster_id(
    family: &str,
    database: &str,
    generation: &DbGeneration,
    found: &str,
    members: &[String],
) -> String {
    let mut sorted: Vec<&str> = members.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts = vec![family, database, generation.0.as_str(), found];
    parts.extend(sorted);
    format!("c:{}", short_digest(&parts))
}

/// What the ledger knows of one database file, within its current generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DbLedger {
    pub generation: DbGeneration,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub started_at: Timestamp,
    /// The highest session any proved read of this generation saved.
    pub max_session: Option<u32>,
    /// The latest write time any proved read of this generation had.
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub max_written_at: Option<Timestamp>,
    /// omaddons itself replaced the file (a restore), at this time: the first proved
    /// read made after it starts a new generation whatever its counter says, and a
    /// sighting made before it is stale. Omitted when it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub invalidated_at: Option<Timestamp>,
    /// Distinct **proved** states of the file as omaddons saw them, oldest first, at
    /// most `LEDGER_READS_MAX`. Two reads of an unchanged file are one entry.
    #[serde(default)]
    pub reads: Vec<LedgerRead>,
    /// The stamp of the newest state that was read and did not prove absence, so a
    /// damaged file that has not changed is not parsed again at every change. Omitted
    /// when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unproved_stamp: Option<String>,
    /// The file's state at each change omaddons made to this client, oldest first.
    /// Written once per change and never altered. Dropped when older than
    /// `SUSPECT_MAX_AGE`, beyond `LEDGER_MARKS_MAX` (oldest first), or when the
    /// generation ends.
    #[serde(default)]
    pub marks: Vec<ChangeMark>,
}

/// One proved state of a database file, and the span over which omaddons saw it
/// unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LedgerRead {
    /// `FileStamp::fingerprint` of a settled stamp: the file's identity, size and times.
    pub stamp: String,
    /// When the game wrote it (the file's mtime).
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub written_at: Option<Timestamp>,
    /// The database's session counter as saved.
    pub session: Option<u32>,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub first_observed_at: Timestamp,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub last_observed_at: Timestamp,
    /// The client's version when this state was first observed. Never overwritten.
    pub client_version: Option<String>,
}

/// The state of a database file at one change: what the session arithmetic of the
/// judge rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeMark {
    /// The command invocation that made the change (a history line's `run`).
    pub run: RunId,
    /// When the file was looked at, just before the change began.
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub at: Timestamp,
    /// The generation the mark belongs to. A mark is never read for another one.
    pub generation: DbGeneration,
    /// The file's stamp just before the change.
    pub stamp: String,
    /// The session saved in the file just before the change; `None` for a database
    /// that keeps no counter. (A state that was not a proved read gets no mark.)
    pub session: Option<u32>,
    /// Whether the client ran at the change: true when it ran at the look before the
    /// change or at the look after it; false when neither look found it running;
    /// `None` when either look could not tell. Two samples, not a recording of the time
    /// between them: `settled` covers a game that came and went in between.
    pub running: Option<bool>,
    /// Whether the file was still in that state after the change committed. False
    /// means the game wrote it in between (a `/reload` or a logout during a download),
    /// so the session above is not the session the change landed in.
    pub settled: bool,
}

/// What omaddons knows about one family of errors on one client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Family {
    pub first: FamilyFirst,
    /// The last read that found a member of the family in any database (a degraded read
    /// counts: what it holds is real).
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub last_present_at: Timestamp,
}

/// How a family first became known on its client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub enum FamilyFirst {
    /// Known, but never watched appear: its age is unknown.
    Baseline {
        #[schemars(schema_with = "crate::contract::schema::timestamp")]
        at: Timestamp,
        reason: BaselineReason,
    },
    /// Proved absent at one read and present at a later one: the cluster holds how.
    Observed { cluster: String },
    /// Met so far only in reads that could not vouch for absence. The next proved read
    /// of its database decides what it is. Until then this remembers when it was first
    /// met, so that a change made afterwards can never be named for it.
    Pending(Box<PositiveSighting>),
}

/// The first time a family was met, in a read that did not prove absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PositiveSighting {
    pub database: String,
    pub generation: DbGeneration,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub observed_at: Timestamp,
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub written_at: Option<Timestamp>,
    /// The lowest session among the family's entries in that read, when all have one.
    pub entry_session: Option<u32>,
    pub time: TimeEvidence,
}

/// Why a family is a baseline: known, with no watched appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BaselineReason {
    /// In the database at its first proved read.
    FirstRead,
    /// In the database at the first proved read of a new generation.
    NewGeneration,
    /// First found while another database of the client could not vouch for its
    /// absence, so "new on this client" could not be established.
    CoverageIncomplete,
}

/// One watched appearance: the errors that showed up together, where, and the judgment
/// of what preceded them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Cluster {
    pub family: String,
    pub appearance: Appearance,
    /// The exact fingerprints (`e:…`) of the family's entries in the read that found
    /// it, sorted, at most `LEDGER_CLUSTER_FINGERPRINTS_MAX`. Only these carry the
    /// verdict.
    pub fingerprints: Vec<String>,
    /// More members were present than the list holds: the rest are not members.
    #[serde(default)]
    pub overflowed: bool,
    /// `None` until a full read judges it. Omitted until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judgment: Option<Verdict>,
    /// The addon (or folder) the error was attributed to when it was judged. A name,
    /// never error text. Omitted until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addon: Option<String>,
}

/// How a family appeared: the read that first held it and the proved read before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Appearance {
    /// The database it appeared in (its key), and that database's generation then.
    pub database: String,
    pub generation: DbGeneration,
    /// The folder of the BugGrabber that owns that database (`!BugGrabber`, or the
    /// addon that embeds it).
    pub provider: String,
    /// The newest proved read before, in which the family was absent.
    pub before: ReadMark,
    /// The proved read that first held it.
    pub found: ReadMark,
    /// An earlier read that already held it but could not vouch for absence, when there
    /// was one. It can only tighten the bounds: the error had occurred by then. Omitted
    /// when there was none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earliest: Option<PositiveSighting>,
    /// The lowest session among the members at that read (each member's own session is
    /// the one it last occurred in); `None` when any member has none.
    pub entry_session: Option<u32>,
    /// The earliest time among the members that carry a unix time; else the weakest
    /// evidence among them (`TimeEvidence::weakest`).
    pub time: TimeEvidence,
    /// The members' occurrence counters, added up.
    pub counter: u32,
}

/// The part of a [`LedgerRead`] an appearance keeps, so pruning reads never changes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadMark {
    pub stamp: String,
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub written_at: Option<Timestamp>,
    pub session: Option<u32>,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub observed_at: Timestamp,
    pub client_version: Option<String>,
}

// ---------------------------------------------------------------------------------------
// Sighting (design 7.1.5): the one function that updates the ledger.

/// What one read found in one database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbSighting {
    /// `errors::ack::db_key` of the file: a hash of its path, never the path.
    pub key: String,
    /// The folder of the BugGrabber that owns the database (`!BugGrabber`, or the addon
    /// that embeds it).
    pub provider: String,
    /// `FileStamp::fingerprint` of the file's stamp.
    pub stamp: String,
    /// When the game wrote it (the file's mtime).
    pub written_at: Option<Timestamp>,
    /// When this process looked at the file.
    pub observed_at: Timestamp,
    /// The database's session counter as saved.
    pub session: Option<u32>,
    /// `None`: not parsed, because its stamp equalled a stamp the caller found in the
    /// ledger before it took the lock.
    pub read: Option<DbRead>,
}

/// What a parsed read holds, and what it can vouch for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbRead {
    pub coverage: Coverage,
    pub entries: Vec<EntrySighting>,
}

/// One entry of a read, as the ledger sees it: identities, a session, a time and a
/// count. Never its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySighting {
    /// `sanitize::family_key` of its message.
    pub family: String,
    /// Its fingerprint (`e:…`), the identity the wire shows.
    pub fingerprint: String,
    /// The session it last occurred in.
    pub session: Option<u32>,
    pub time: TimeEvidence,
    /// How often it occurred.
    pub counter: u32,
}

/// The client as one run sees it: its version, and the run's one `now` (common rule 13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientNow {
    pub version: Option<String>,
    pub now: Timestamp,
}

/// How much one batch of sightings may conclude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SightMode {
    /// `errors`: absence across the whole client may be concluded, so a family long
    /// gone may be forgotten.
    Full,
    /// The light read before a change: nothing is forgotten.
    Light,
}

/// A restore omaddons committed on this client, read from history (design 7.1.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// The history line's time.
    pub at: Timestamp,
    /// A whole `WTF/` restore: it replaced every database of the client.
    pub whole: bool,
    /// The folders whose SavedVariables it put back: a database whose provider is one of
    /// them was replaced.
    pub providers: Vec<String>,
}

/// What one batch changed that its caller reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sighted {
    /// The ids of the clusters this call created, sorted.
    pub appeared: Vec<String>,
    /// The databases whose generation this call started or ended, sorted.
    pub regenerated: Vec<String>,
}

/// Records what `dbs` hold now in a client's ledger (design 7.1.5). A pure function: no
/// I/O and no clock, its time `client.now`. `discovered` is false when the walk that
/// found the client's databases was cut by a bound, so `dbs` may be short. The result
/// does not depend on the order of `dbs`.
///
/// Uncertainty may lose a link and never creates one: an error is an appearance only
/// when a proved read without its family is followed by a proved read with it, in one
/// generation of one database, no database of the client has held it, and the whole
/// client was seen (it is *covered*). Otherwise it is a baseline, whose age is unknown.
///
/// A batch that would take the client past [`LEDGER_FAMILIES_MAX`] first forgets the
/// oldest families absent from every database, when that absence is proved; otherwise
/// nothing of it is recorded and the client becomes untracked, until a `Full`, covered
/// batch that fits starts every database afresh.
pub fn sight(
    ledger: &mut TargetLedger,
    dbs: &[DbSighting],
    restores: &[Restored],
    client: &ClientNow,
    discovered: bool,
    mode: SightMode,
) -> Sighted {
    if ledger.untracked {
        let mut fresh = TargetLedger::default();
        let batch = record(&mut fresh, dbs, restores, client, discovered, mode);
        if batch.tracked && batch.covered && mode == SightMode::Full {
            *ledger = fresh;
            return batch.sighted;
        }
        return Sighted::default();
    }
    let batch = record(ledger, dbs, restores, client, discovered, mode);
    if batch.tracked {
        return batch.sighted;
    }
    *ledger = TargetLedger {
        untracked: true,
        ..TargetLedger::default()
    };
    Sighted::default()
}

/// How one sighting relates to what the ledger holds of its database (step 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Another process recorded a newer state (`behind_proved`, a proved read), or
    /// omaddons replaced the file after this look: ignored.
    Stale { behind_proved: bool },
    /// Not parsed; `current` when its stamp is the newest proved read's.
    Unparsed { current: bool },
    /// Parsed, but it cannot vouch for absence.
    Degraded,
    /// A proved read that starts a generation; `first` for the database's first.
    NewGeneration { first: bool },
    /// A proved read of the current generation; `fresh` when it is a new state.
    Continuing { fresh: bool },
}

/// What [`record`] concluded.
struct Batch {
    sighted: Sighted,
    covered: bool,
    /// False when the batch did not fit and nothing of it may be kept.
    tracked: bool,
}

fn record(
    ledger: &mut TargetLedger,
    dbs: &[DbSighting],
    restores: &[Restored],
    client: &ClientNow,
    discovered: bool,
    mode: SightMode,
) -> Batch {
    let now = client.now;
    let dbs = distinct(dbs);
    invalidate(ledger, &dbs, restores);
    let classes: Vec<Class> = dbs
        .iter()
        .map(|s| classify(ledger.databases.get(&s.key), s))
        .collect();
    let every_listed = ledger
        .databases
        .keys()
        .all(|key| dbs.iter().any(|s| &s.key == key));
    let covered = discovered
        && (mode == SightMode::Full || every_listed)
        && classes.iter().all(|class| {
            matches!(
                class,
                Class::NewGeneration { .. }
                    | Class::Continuing { .. }
                    | Class::Unparsed { current: true }
                    | Class::Stale {
                        behind_proved: true
                    }
            )
        });
    // Forgetting claims absence across the client: it needs every database read now.
    let evictable = mode == SightMode::Full
        && covered
        && classes.iter().all(|class| {
            matches!(
                class,
                Class::NewGeneration { .. } | Class::Continuing { .. }
            )
        });
    // Every family some read of the batch holds, a stale one's included: holding is
    // never in doubt, only absence is.
    let present: BTreeSet<&str> = dbs
        .iter()
        .flat_map(|s| entries(s))
        .map(|e| e.family.as_str())
        .collect();
    let mut sighted = Sighted::default();

    // Step 1a: what is known and present again.
    for (s, class) in dbs.iter().zip(&classes) {
        if matches!(
            class,
            Class::Degraded | Class::NewGeneration { .. } | Class::Continuing { fresh: true }
        ) {
            for e in entries(s) {
                if let Some(family) = ledger.families.get_mut(&e.family) {
                    family.last_present_at = now;
                }
            }
        }
    }

    // Step 1b: new generations, and what their reads hold as baselines.
    for (s, class) in dbs.iter().zip(&classes) {
        if let Class::NewGeneration { first } = *class {
            start_generation(ledger, s, client, first);
            sighted.regenerated.push(s.key.clone());
        }
    }

    // Step 1c: families first met in a read that proves nothing.
    let mut notes: BTreeMap<&str, (Timestamp, &str, PositiveSighting)> = BTreeMap::new();
    for (s, class) in dbs.iter().zip(&classes) {
        if *class != Class::Degraded {
            continue;
        }
        let Some(db) = ledger.databases.get_mut(&s.key) else {
            continue;
        };
        db.unproved_stamp = Some(s.stamp.clone());
        let bound = seen_by(s.observed_at, s.written_at);
        for (family, members) in by_family(entries(s)) {
            if ledger.families.contains_key(family) {
                continue;
            }
            let note = PositiveSighting {
                database: s.key.clone(),
                generation: db.generation.clone(),
                observed_at: s.observed_at,
                written_at: s.written_at,
                entry_session: lowest_session(&members),
                time: earliest_time(&members),
            };
            let earlier = notes
                .get(family)
                .is_none_or(|(b, key, _)| (bound, s.key.as_str()) < (*b, *key));
            if earlier {
                notes.insert(family, (bound, s.key.as_str(), note));
            }
        }
    }
    for (family, (_, _, note)) in notes {
        ledger.families.insert(
            family.to_owned(),
            Family {
                first: FamilyFirst::Pending(Box::new(note)),
                last_present_at: now,
            },
        );
    }

    // Step 1d: new states of continuing databases, and looks at unchanged ones.
    let mut before: BTreeMap<&str, LedgerRead> = BTreeMap::new();
    for (s, class) in dbs.iter().zip(&classes) {
        let Some(db) = ledger.databases.get_mut(&s.key) else {
            continue;
        };
        match class {
            Class::Continuing { fresh: true } => {
                if let Some(newest) = db.reads.last() {
                    before.insert(s.key.as_str(), newest.clone());
                }
                db.reads.push(read_of(s, client));
                let over = db.reads.len().saturating_sub(LEDGER_READS_MAX);
                db.reads.drain(..over);
                db.max_session = db.max_session.max(s.session);
                db.max_written_at = db.max_written_at.max(s.written_at);
                db.unproved_stamp = None;
            }
            Class::Continuing { fresh: false } | Class::Unparsed { current: true } => {
                if let Some(newest) = db.reads.last_mut() {
                    newest.last_observed_at = newest.last_observed_at.max(s.observed_at);
                }
            }
            _ => {}
        }
    }

    // Step 2: appearances, each in the database whose read bounds its onset earliest.
    let mut fresh: Vec<&DbSighting> = dbs
        .iter()
        .zip(&classes)
        .filter(|(_, class)| **class == Class::Continuing { fresh: true })
        .map(|(s, _)| *s)
        .collect();
    fresh.sort_by(|a, b| {
        (a.written_at.is_none(), a.written_at, &a.key).cmp(&(
            b.written_at.is_none(),
            b.written_at,
            &b.key,
        ))
    });
    let mut decided: BTreeSet<&str> = BTreeSet::new();
    for s in &fresh {
        for (family, members) in by_family(entries(s)) {
            let earliest = match ledger.families.get(family).map(|f| &f.first) {
                None => None,
                Some(FamilyFirst::Pending(note)) => Some((**note).clone()),
                Some(_) => continue,
            };
            if !decided.insert(family) {
                continue;
            }
            let (Some(previous), Some(db), true) = (
                before.get(s.key.as_str()),
                ledger.databases.get(&s.key),
                covered,
            ) else {
                ledger.families.insert(
                    family.to_owned(),
                    Family {
                        first: FamilyFirst::Baseline {
                            at: now,
                            reason: BaselineReason::CoverageIncomplete,
                        },
                        last_present_at: now,
                    },
                );
                continue;
            };
            let (id, cluster) = appearance(family, s, &members, previous, db, earliest, client);
            ledger.clusters.insert(id.clone(), cluster);
            ledger.families.insert(
                family.to_owned(),
                Family {
                    first: FamilyFirst::Observed {
                        cluster: id.clone(),
                    },
                    last_present_at: now,
                },
            );
            sighted.appeared.push(id);
        }
    }

    // A note its own database's proved read no longer holds, and no read here does.
    let proved_fresh: BTreeSet<&str> = fresh.iter().map(|s| s.key.as_str()).collect();
    ledger.families.retain(|family, f| match &f.first {
        FamilyFirst::Pending(note) => {
            !proved_fresh.contains(note.database.as_str()) || present.contains(family.as_str())
        }
        _ => true,
    });

    for db in ledger.databases.values_mut() {
        db.marks
            .retain(|mark| !older_than(mark.at, now, SUSPECT_MAX_AGE));
        let over = db.marks.len().saturating_sub(LEDGER_MARKS_MAX);
        db.marks.drain(..over);
    }

    if ledger.families.len() > LEDGER_FAMILIES_MAX {
        if evictable {
            let excess = ledger.families.len() - LEDGER_FAMILIES_MAX;
            forget_oldest_absent(ledger, &present, excess);
        }
        if ledger.families.len() > LEDGER_FAMILIES_MAX {
            return Batch {
                sighted: Sighted::default(),
                covered,
                tracked: false,
            };
        }
    }

    if evictable {
        let expired: Vec<String> = ledger
            .families
            .iter()
            .filter(|(family, f)| {
                !present.contains(family.as_str())
                    && older_than(f.last_present_at, now, LEDGER_ABSENT_MAX_AGE)
            })
            .map(|(family, _)| family.clone())
            .collect();
        for family in expired {
            forget(ledger, &family);
        }
        let absent = ledger
            .families
            .keys()
            .filter(|family| !present.contains(family.as_str()))
            .count();
        forget_oldest_absent(ledger, &present, absent.saturating_sub(LEDGER_ABSENT_MAX));
        let listed: BTreeSet<&str> = dbs.iter().map(|s| s.key.as_str()).collect();
        ledger.databases.retain(|key, db| {
            listed.contains(key.as_str())
                || db.reads.last().is_none_or(|newest| {
                    !older_than(newest.last_observed_at, now, LEDGER_ABSENT_MAX_AGE)
                })
        });
    }

    sighted.appeared.sort();
    sighted.regenerated.sort();
    Batch {
        sighted,
        covered,
        tracked: true,
    }
}

/// One sighting per database, by key: for a key listed twice, the later look.
fn distinct(dbs: &[DbSighting]) -> Vec<&DbSighting> {
    let mut sorted: Vec<&DbSighting> = dbs.iter().collect();
    sorted
        .sort_by(|a, b| (&a.key, a.observed_at, &a.stamp).cmp(&(&b.key, b.observed_at, &b.stamp)));
    sorted.reverse();
    sorted.dedup_by(|a, b| a.key == b.key);
    sorted.reverse();
    sorted
}

/// Step 0's first half: each restore later than a database's newest proved state marks
/// that database replaced (design 7.1.4). A whole `WTF/` restore replaced every database
/// of the client; one addon's restore, the databases that addon provides (folder names
/// match ignoring case, as Wine's do). A database not sighted now has no known provider:
/// its addon restores are applied when it is sighted, from the same history.
fn invalidate(ledger: &mut TargetLedger, dbs: &[&DbSighting], restores: &[Restored]) {
    for restore in restores {
        for (key, db) in &mut ledger.databases {
            let provider = dbs.iter().find(|s| &s.key == key).map(|s| &s.provider);
            let replaced = restore.whole
                || provider.is_some_and(|provider| {
                    restore
                        .providers
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(provider))
                });
            let newest = db
                .reads
                .last()
                .map_or(db.started_at, |r| r.last_observed_at);
            if replaced && restore.at > newest {
                db.invalidated_at = db.invalidated_at.max(Some(restore.at));
            }
        }
    }
}

/// Step 0's second half: the first class that matches (design 7.1.5).
fn classify(db: Option<&DbLedger>, s: &DbSighting) -> Class {
    if let Some(db) = db {
        if db.invalidated_at.is_some_and(|at| at >= s.observed_at) {
            return Class::Stale {
                behind_proved: false,
            };
        }
        let newer = db
            .reads
            .iter()
            .any(|r| r.last_observed_at > s.observed_at && r.stamp != s.stamp);
        if newer {
            return Class::Stale {
                behind_proved: true,
            };
        }
    }
    let current = db
        .and_then(|db| db.reads.last())
        .is_some_and(|newest| newest.stamp == s.stamp);
    let Some(read) = &s.read else {
        return Class::Unparsed { current };
    };
    if matches!(read.coverage, Coverage::Degraded(_)) {
        return Class::Degraded;
    }
    let Some(db) = db else {
        return Class::NewGeneration { first: true };
    };
    let counter_went_down = db
        .max_session
        .zip(s.session)
        .is_some_and(|(max, session)| session < max);
    let file_went_back = db
        .max_written_at
        .zip(s.written_at)
        .is_some_and(|(max, at)| max.duration_since(at).as_nanos() > RACY_SLACK_COARSE_NS);
    if db.reads.is_empty() || db.invalidated_at.is_some() || counter_went_down || file_went_back {
        return Class::NewGeneration { first: false };
    }
    Class::Continuing { fresh: !current }
}

/// Starts a generation of `s`'s database from this proved read (design 7.1.4): its reads,
/// marks and notes start afresh, the clusters of the ended generation keep their record
/// but an unfinished judgment becomes final, and the read's unknown families are
/// baselines. Recurrence memory (`families`) is kept.
fn start_generation(ledger: &mut TargetLedger, s: &DbSighting, client: &ClientNow, first: bool) {
    let now = client.now;
    if let Some(old) = ledger.databases.remove(&s.key) {
        for cluster in ledger.clusters.values_mut() {
            let ended = cluster.appearance.database == s.key
                && cluster.appearance.generation == old.generation;
            let settled = matches!(
                &cluster.judgment,
                Some(Verdict {
                    state: JudgmentState::Complete | JudgmentState::Unsupported,
                    ..
                })
            );
            if ended && !settled {
                cluster.judgment = Some(Verdict {
                    algorithm: SUSPECT_ALGORITHM,
                    state: JudgmentState::Unsupported,
                    judged_at: now,
                    suspect: None,
                    gap: Some(JudgmentGap::GenerationEnded),
                });
            }
        }
    }
    ledger
        .families
        .retain(|_, f| !matches!(&f.first, FamilyFirst::Pending(note) if note.database == s.key));
    ledger.databases.insert(
        s.key.clone(),
        DbLedger {
            generation: DbGeneration::of(&s.key, &s.stamp, s.observed_at),
            started_at: s.observed_at,
            max_session: s.session,
            max_written_at: s.written_at,
            invalidated_at: None,
            reads: vec![read_of(s, client)],
            unproved_stamp: None,
            marks: Vec::new(),
        },
    );
    let reason = if first {
        BaselineReason::FirstRead
    } else {
        BaselineReason::NewGeneration
    };
    for e in entries(s) {
        if ledger.families.contains_key(&e.family) {
            continue;
        }
        ledger.families.insert(
            e.family.clone(),
            Family {
                first: FamilyFirst::Baseline { at: now, reason },
                last_present_at: now,
            },
        );
    }
}

/// The cluster of `family`, watched to appear in `s` after `previous` (design 7.1.5):
/// its members are the family's exact fingerprints in this read, sorted and cut to the
/// cap; its session, time and count are taken over every entry of the family in the
/// read, so a cut list never makes the onset later.
fn appearance(
    family: &str,
    s: &DbSighting,
    members: &[&EntrySighting],
    previous: &LedgerRead,
    db: &DbLedger,
    earliest: Option<PositiveSighting>,
    client: &ClientNow,
) -> (String, Cluster) {
    let mut fingerprints: Vec<String> = members.iter().map(|e| e.fingerprint.clone()).collect();
    fingerprints.sort_unstable();
    fingerprints.dedup();
    let overflowed = fingerprints.len() > LEDGER_CLUSTER_FINGERPRINTS_MAX;
    fingerprints.truncate(LEDGER_CLUSTER_FINGERPRINTS_MAX);
    let id = cluster_id(family, &s.key, &db.generation, &s.stamp, &fingerprints);
    let cluster = Cluster {
        family: family.to_owned(),
        appearance: Appearance {
            database: s.key.clone(),
            generation: db.generation.clone(),
            provider: s.provider.clone(),
            before: ReadMark {
                stamp: previous.stamp.clone(),
                written_at: previous.written_at,
                session: previous.session,
                observed_at: previous.last_observed_at,
                client_version: previous.client_version.clone(),
            },
            found: ReadMark {
                stamp: s.stamp.clone(),
                written_at: s.written_at,
                session: s.session,
                observed_at: s.observed_at,
                client_version: client.version.clone(),
            },
            earliest,
            entry_session: lowest_session(members),
            time: earliest_time(members),
            counter: members
                .iter()
                .fold(0u32, |sum, e| sum.saturating_add(e.counter)),
        },
        fingerprints,
        overflowed,
        judgment: None,
        addon: None,
    };
    (id, cluster)
}

/// A proved state of `s`'s file, first seen now.
fn read_of(s: &DbSighting, client: &ClientNow) -> LedgerRead {
    LedgerRead {
        stamp: s.stamp.clone(),
        written_at: s.written_at,
        session: s.session,
        first_observed_at: s.observed_at,
        last_observed_at: s.observed_at,
        client_version: client.version.clone(),
    }
}

fn entries(s: &DbSighting) -> &[EntrySighting] {
    s.read.as_ref().map_or(&[], |read| read.entries.as_slice())
}

/// A read's entries by family.
fn by_family(entries: &[EntrySighting]) -> BTreeMap<&str, Vec<&EntrySighting>> {
    let mut families: BTreeMap<&str, Vec<&EntrySighting>> = BTreeMap::new();
    for e in entries {
        families.entry(e.family.as_str()).or_default().push(e);
    }
    families
}

/// The lowest session among `members`, when every one has one.
fn lowest_session(members: &[&EntrySighting]) -> Option<u32> {
    members
        .iter()
        .map(|e| e.session)
        .collect::<Option<Vec<u32>>>()?
        .into_iter()
        .min()
}

/// The earliest time among `members` that carry a unix time (the weaker meaning on a
/// tie); else the weakest evidence among them (design 7.1.1).
fn earliest_time(members: &[&EntrySighting]) -> TimeEvidence {
    let unix = members
        .iter()
        .map(|e| e.time)
        .filter(|t| t.tz == TzEvidence::Explicit && t.at.is_some())
        .min_by_key(|t| (t.at, t.meaning));
    unix.unwrap_or_else(|| {
        members
            .iter()
            .map(|e| e.time)
            .reduce(|a, b| TimeEvidence::weakest(&a, &b))
            .unwrap_or(TimeEvidence {
                at: None,
                meaning: TimeMeaning::Unknown,
                tz: TzEvidence::SystemAssumed,
            })
    })
}

/// The earlier of when a read looked at a file and when the game wrote it, a write time
/// later than the look left out.
fn seen_by(observed_at: Timestamp, written_at: Option<Timestamp>) -> Timestamp {
    written_at.map_or(observed_at, |written| written.min(observed_at))
}

/// Whether `at` is more than `age` before `now` (a time ahead of the clock never is).
fn older_than(at: Timestamp, now: Timestamp, age: Duration) -> bool {
    at < now && now.duration_since(at).unsigned_abs() > age
}

/// Forgets a family, with its cluster.
fn forget(ledger: &mut TargetLedger, family: &str) {
    if let Some(Family {
        first: FamilyFirst::Observed { cluster },
        ..
    }) = ledger.families.remove(family)
    {
        ledger.clusters.remove(&cluster);
    }
}

/// Forgets up to `count` families absent from every read of the batch, the oldest last
/// presence first. A family present anywhere is never forgotten.
fn forget_oldest_absent(ledger: &mut TargetLedger, present: &BTreeSet<&str>, count: usize) {
    let mut absent: Vec<(Timestamp, String)> = ledger
        .families
        .iter()
        .filter(|(family, _)| !present.contains(family.as_str()))
        .map(|(family, f)| (f.last_present_at, family.clone()))
        .collect();
    absent.sort();
    for (_, family) in absent.into_iter().take(count) {
        forget(ledger, &family);
    }
}

// ---------------------------------------------------------------------------------------
// Loading and storing (design 7.1.6): one writer at a time, and no merge.

/// Why a session could not be had, or could not store. The run's evidence is lost, and
/// its caller says so in one `-vv` trace line; it is never a command's failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerUnavailable {
    /// Another process held the lock past `LEDGER_LOCK_TIMEOUT`.
    Busy,
    /// A newer omaddons wrote the file: it is neither believed nor overwritten.
    Newer,
    /// The store would pass `LEDGER_MAX_BYTES`.
    TooLarge { bytes: u64 },
    /// The lock or the file could not be had.
    Io(String),
}

/// The ledger, loaded under the evidence lock (`<cache>/errors_ledger.lock`, an
/// `fsx::Lease`, never the global lock). What a process learned before it took the lock
/// goes in only through the three input rules ([`Self::sight`], [`Self::add_mark`],
/// [`Self::attach`]), as input to the ledger found under it. Dropping the session
/// without [`Self::commit`] records nothing. Nothing is parsed, fetched or committed
/// while it is open.
pub struct LedgerSession {
    pub ledger: ErrorsLedger,
    /// As loaded, so an unchanged ledger is not written again.
    loaded: ErrorsLedger,
    paths: Paths,
    lease: LeaseGuard,
}

/// A stored ledger, with the lock still held until it is dropped, so the caller can
/// write the memo links that match it.
pub struct Accepted {
    pub ledger: ErrorsLedger,
    _lease: LeaseGuard,
}

/// Takes the evidence lock, waiting at most `LEDGER_LOCK_TIMEOUT`, then loads the
/// ledger. A ledger made with another family key, or a file that does not read, loads
/// as empty and writable; one a newer omaddons wrote is neither believed nor
/// overwritten.
pub fn open(
    paths: &Paths,
    scopes: &ScopeTable,
    command: &str,
) -> std::result::Result<LedgerSession, LedgerUnavailable> {
    let lease = Lease::acquire(
        scopes,
        &paths.errors_ledger_lock(),
        command,
        LEDGER_LOCK_TIMEOUT,
    )
    .map_err(|err| match err {
        LeaseError::Busy { .. } => LedgerUnavailable::Busy,
        other => LedgerUnavailable::Io(other.to_string()),
    })?;
    let ledger = match store::load::<ErrorsLedger>(&paths.errors_ledger()) {
        Ok(Some(loaded)) if loaded.status == VersionStatus::Newer => {
            return Err(LedgerUnavailable::Newer);
        }
        Ok(Some(loaded)) => as_read(loaded.value),
        Ok(None) => as_read(ErrorsLedger::default()),
        Err(err) if err.code == ErrorCode::StateTooNew => return Err(LedgerUnavailable::Newer),
        Err(_) => as_read(ErrorsLedger::default()),
    };
    failpoint::hit("errors.ledger.loaded").map_err(|e| LedgerUnavailable::Io(e.to_string()))?;
    Ok(LedgerSession {
        loaded: ledger.clone(),
        ledger,
        paths: paths.clone(),
        lease,
    })
}

/// The ledger as it is on disk, for display only. It takes no lock: the file is
/// replaced atomically, so a reader always sees one whole revision. `None` when a newer
/// omaddons wrote a file this one cannot read; a newer file that reads is believed.
pub fn load(paths: &Paths) -> Option<ErrorsLedger> {
    match store::load::<ErrorsLedger>(&paths.errors_ledger()) {
        Ok(Some(loaded)) => Some(as_read(loaded.value)),
        Ok(None) => Some(as_read(ErrorsLedger::default())),
        Err(err) if err.code == ErrorCode::StateTooNew => None,
        Err(_) => Some(as_read(ErrorsLedger::default())),
    }
}

/// A ledger as this build reads it. One made with another family key is discarded
/// (every error baselines again), keeping its revision so a memo link to the discarded
/// one never matches. A verdict made by rules older than
/// `SUSPECT_ALGORITHM_TRUSTED_MIN` is dropped: its cluster is judged again while its
/// generation is current, and can never be otherwise (design 7.2.6).
fn as_read(ledger: ErrorsLedger) -> ErrorsLedger {
    if ledger.identity != LEDGER_IDENTITY {
        return ErrorsLedger {
            identity: LEDGER_IDENTITY,
            revision: ledger.revision,
            targets: BTreeMap::new(),
        };
    }
    let mut ledger = ledger;
    for target in ledger.targets.values_mut() {
        for cluster in target.clusters.values_mut() {
            let Some(verdict) = &cluster.judgment else {
                continue;
            };
            if verdict.algorithm >= SUSPECT_ALGORITHM_TRUSTED_MIN {
                continue;
            }
            let current = target
                .databases
                .get(&cluster.appearance.database)
                .is_some_and(|db| db.generation == cluster.appearance.generation);
            cluster.judgment = (!current).then_some(Verdict {
                algorithm: SUSPECT_ALGORITHM,
                state: JudgmentState::Unsupported,
                judged_at: verdict.judged_at,
                suspect: None,
                gap: Some(JudgmentGap::GenerationEnded),
            });
        }
    }
    ledger
}

impl LedgerSession {
    /// Records what `dbs` held for `target` (its key, or its id while unregistered), by
    /// [`sight`] against the ledger found under the lock: a sighting older than what it
    /// holds is stale and ignored, never retried.
    pub fn sight(
        &mut self,
        target: &str,
        dbs: &[DbSighting],
        restores: &[Restored],
        client: &ClientNow,
        discovered: bool,
        mode: SightMode,
    ) -> Sighted {
        let ledger = self.ledger.targets.entry(target.to_owned()).or_default();
        let sighted = sight(ledger, dbs, restores, client, discovered, mode);
        if *ledger == TargetLedger::default() {
            self.ledger.targets.remove(target);
        }
        sighted
    }

    /// Adds a change mark (design 7.1.6), only when its generation is the database's
    /// current one, its stamp is the stamp of that generation's newest proved read (after
    /// this session's own sightings), the file was not replaced since, and its run has no
    /// mark yet. Otherwise that change has no mark, and its sessions read as unknown.
    pub fn add_mark(&mut self, target: &str, database: &str, mark: ChangeMark) -> bool {
        let Some(db) = self
            .ledger
            .targets
            .get_mut(target)
            .and_then(|t| t.databases.get_mut(database))
        else {
            return false;
        };
        let current = db.invalidated_at.is_none()
            && mark.generation == db.generation
            && db
                .reads
                .last()
                .is_some_and(|newest| newest.stamp == mark.stamp)
            && !db.marks.iter().any(|m| m.run == mark.run);
        if !current {
            return false;
        }
        db.marks.push(mark);
        db.marks.sort_by_key(|m| m.at);
        let over = db.marks.len().saturating_sub(LEDGER_MARKS_MAX);
        db.marks.drain(..over);
        true
    }

    /// Attaches a verdict to the cluster it was computed for (design 7.1.6), only while
    /// that cluster's appearance is in its database's current generation and it has no
    /// final judgment (`Complete` is kept as made; `Unsupported` is final). A baseline
    /// has no cluster, so it can never be given one.
    pub fn attach(
        &mut self,
        target: &str,
        cluster: &str,
        verdict: Verdict,
        addon: Option<String>,
    ) -> bool {
        let Some(t) = self.ledger.targets.get_mut(target) else {
            return false;
        };
        let Some(c) = t.clusters.get_mut(cluster) else {
            return false;
        };
        let current = t
            .databases
            .get(&c.appearance.database)
            .is_some_and(|db| db.generation == c.appearance.generation);
        let open = c
            .judgment
            .as_ref()
            .is_none_or(|v| v.state == JudgmentState::Deferred);
        if !(current && open) {
            return false;
        }
        c.judgment = Some(verdict);
        c.addon = addon;
        true
    }

    /// Stores the ledger, its revision one higher, when it changed and fits
    /// `LEDGER_MAX_BYTES`, by one atomic replace; an unchanged ledger is not written and
    /// keeps its revision. The lock stays held until the [`Accepted`] is dropped.
    pub fn commit(self) -> std::result::Result<Accepted, LedgerUnavailable> {
        let LedgerSession {
            mut ledger,
            loaded,
            paths,
            lease,
        } = self;
        if ledger == loaded {
            return Ok(Accepted {
                ledger,
                _lease: lease,
            });
        }
        ledger.revision = ledger.revision.saturating_add(1);
        let bytes =
            store::encode_compact(&ledger).map_err(|e| LedgerUnavailable::Io(e.message.clone()))?;
        let size = bytes.len() as u64;
        if size > LEDGER_MAX_BYTES {
            return Err(LedgerUnavailable::TooLarge { bytes: size });
        }
        failpoint::hit("errors.ledger.store").map_err(|e| LedgerUnavailable::Io(e.to_string()))?;
        write(&paths, &bytes).map_err(|e| LedgerUnavailable::Io(e.message.clone()))?;
        Ok(Accepted {
            ledger,
            _lease: lease,
        })
    }
}

/// One atomic replace of the ledger file, through the one writer.
fn write(paths: &Paths, bytes: &[u8]) -> crate::error::Result<()> {
    let scopes = ScopeTable::new(paths, &[], None)?;
    let mut s = WriteSession::untracked(Writer::Derived, &scopes, "errors-ledger");
    ensure_dir_all(&mut s, paths.cache_dir())?;
    let op = Op::WriteFile {
        content: Content::Bytes(bytes),
        mode: Some(0o644),
    };
    mutate(&mut s, &paths.errors_ledger(), op)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::attribute::Confidence;
    use crate::errors::buggrabber::{TimeMeaning, TzEvidence};
    use crate::errors::suspect::{
        ClientUpdated, Evidence, JudgmentGap, JudgmentState, Relation, Suspect, Unknown,
    };
    use crate::model::{Actor, ReleaseTransition, TransitionChange, TxKind};

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + seconds).unwrap()
    }

    fn generation(n: u8) -> DbGeneration {
        DbGeneration::of(&format!("db{n}"), "1:2:f:644:10:20:30", at(0))
    }

    fn members(list: &[&str]) -> Vec<String> {
        list.iter().map(|m| (*m).to_owned()).collect()
    }

    #[test]
    fn a_cluster_id_is_its_members_as_a_set_within_one_appearance() {
        let g = generation(1);
        let id = |family: &str, db: &str, g: &DbGeneration, found: &str, list: &[&str]| {
            cluster_id(family, db, g, found, &members(list))
        };
        let base = id("a:1", "db1", &g, "s", &["e:1", "e:2", "e:3"]);
        assert!(base.starts_with("c:") && base.len() == 18, "{base}");
        // (case, id, the same cluster)
        for (case, other, same) in [
            (
                "the same inputs",
                id("a:1", "db1", &g, "s", &["e:1", "e:2", "e:3"]),
                true,
            ),
            (
                "members in another order",
                id("a:1", "db1", &g, "s", &["e:3", "e:1", "e:2"]),
                true,
            ),
            (
                "a member repeated",
                id("a:1", "db1", &g, "s", &["e:2", "e:1", "e:3", "e:1"]),
                true,
            ),
            (
                "another generation",
                id("a:1", "db1", &generation(2), "s", &["e:1", "e:2", "e:3"]),
                false,
            ),
            (
                "another found stamp",
                id("a:1", "db1", &g, "t", &["e:1", "e:2", "e:3"]),
                false,
            ),
            (
                "another family",
                id("a:2", "db1", &g, "s", &["e:1", "e:2", "e:3"]),
                false,
            ),
            (
                "another database",
                id("a:1", "db2", &g, "s", &["e:1", "e:2", "e:3"]),
                false,
            ),
            (
                "a member fewer",
                id("a:1", "db1", &g, "s", &["e:1", "e:2"]),
                false,
            ),
            (
                "another member",
                id("a:1", "db1", &g, "s", &["e:1", "e:2", "e:4"]),
                false,
            ),
            ("no members", id("a:1", "db1", &g, "s", &[]), false),
        ] {
            assert_eq!(other == base, same, "{case}: {other} against {base}");
        }
    }

    #[test]
    fn a_generation_id_is_opaque_and_stable_for_its_inputs() {
        let g = DbGeneration::of("db", "stamp", at(5));
        assert!(g.0.starts_with("g:") && g.0.len() == 18, "{}", g.0);
        assert!(!g.0.contains("db") && !g.0.contains("stamp"), "{}", g.0);
        // (case, generation, the same one)
        for (case, other, same) in [
            (
                "the same inputs",
                DbGeneration::of("db", "stamp", at(5)),
                true,
            ),
            (
                "another database",
                DbGeneration::of("db2", "stamp", at(5)),
                false,
            ),
            (
                "another stamp",
                DbGeneration::of("db", "stamp2", at(5)),
                false,
            ),
            (
                "another time",
                DbGeneration::of("db", "stamp", at(6)),
                false,
            ),
            (
                "a nanosecond later",
                DbGeneration::of(
                    "db",
                    "stamp",
                    at(5)
                        .checked_add(jiff::SignedDuration::from_nanos(1))
                        .unwrap(),
                ),
                false,
            ),
            (
                "the parts moved across the boundary",
                DbGeneration::of("dbs", "tamp", at(5)),
                false,
            ),
        ] {
            assert_eq!(other == g, same, "{case}: {} against {}", other.0, g.0);
        }
    }

    fn time(at: Option<Timestamp>) -> TimeEvidence {
        TimeEvidence {
            at,
            meaning: TimeMeaning::Last,
            tz: TzEvidence::Explicit,
        }
    }

    fn read_mark(stamp: &str, session: Option<u32>) -> ReadMark {
        ReadMark {
            stamp: stamp.to_owned(),
            written_at: Some(at(10)),
            session,
            observed_at: at(11),
            client_version: Some("12.1.5.70391".to_owned()),
        }
    }

    fn sighting() -> PositiveSighting {
        PositiveSighting {
            database: "d:1".to_owned(),
            generation: generation(1),
            observed_at: at(20),
            written_at: None,
            entry_session: Some(7),
            time: time(None),
        }
    }

    fn verdict() -> Verdict {
        Verdict {
            algorithm: crate::limits::SUSPECT_ALGORITHM,
            state: JudgmentState::Complete,
            judged_at: at(40),
            suspect: Some(Suspect {
                tx: "01M51EX645RCVFN6Z4ZV5GRWVS".parse().unwrap(),
                run: Some("01M51EX04HHWM46XE9F2B9JXED".parse().unwrap()),
                kind: TxKind::Update,
                at: at(30),
                actor: Actor::Widget,
                relation: Relation::InStack,
                confidence: Confidence::Medium,
                changed: vec![ReleaseTransition {
                    addon: "Foo".to_owned(),
                    change: TransitionChange::Updated,
                    from: None,
                    to: None,
                }],
                evidence: Evidence {
                    gap_s: 19,
                    exact: false,
                    same_session: None,
                    clean_sessions: Some(2),
                    client_update: Some(ClientUpdated {
                        from_version: "12.1.4".to_owned(),
                        to_version: "12.1.5".to_owned(),
                    }),
                    other_changes: 1,
                    unknowns: vec![Unknown::Running, Unknown::ClientVersion],
                },
            }),
            gap: None,
        }
    }

    /// A ledger with every type, every family basis and every optional field set.
    fn full() -> ErrorsLedger {
        let db = DbLedger {
            generation: generation(1),
            started_at: at(1),
            max_session: Some(9),
            max_written_at: Some(at(10)),
            invalidated_at: Some(at(2)),
            reads: vec![LedgerRead {
                stamp: "1:2:f:644:10:20:30".to_owned(),
                written_at: Some(at(10)),
                session: Some(9),
                first_observed_at: at(11),
                last_observed_at: at(12),
                client_version: None,
            }],
            unproved_stamp: Some("1:2:f:644:11:21:31".to_owned()),
            marks: vec![ChangeMark {
                run: "01M51EX04HHWM46XE9F2B9JXED".parse().unwrap(),
                at: at(13),
                generation: generation(1),
                stamp: "1:2:f:644:10:20:30".to_owned(),
                session: Some(9),
                running: Some(false),
                settled: true,
            }],
        };
        let cluster = Cluster {
            family: "a:0123456789abcdef".to_owned(),
            appearance: Appearance {
                database: "d:1".to_owned(),
                generation: generation(1),
                provider: "!BugGrabber".to_owned(),
                before: read_mark("s1", Some(8)),
                found: read_mark("s2", None),
                earliest: Some(sighting()),
                entry_session: Some(9),
                time: time(Some(at(15))),
                counter: 3,
            },
            fingerprints: members(&["e:0000000000000001", "e:0000000000000002"]),
            overflowed: true,
            judgment: Some(verdict()),
            addon: Some("Foo".to_owned()),
        };
        let family = |first| Family {
            first,
            last_present_at: at(50),
        };
        let target = TargetLedger {
            databases: BTreeMap::from([("d:1".to_owned(), db)]),
            families: BTreeMap::from([
                (
                    "a:1".to_owned(),
                    family(FamilyFirst::Baseline {
                        at: at(3),
                        reason: BaselineReason::CoverageIncomplete,
                    }),
                ),
                (
                    "a:2".to_owned(),
                    family(FamilyFirst::Observed {
                        cluster: "c:1".to_owned(),
                    }),
                ),
                (
                    "a:3".to_owned(),
                    family(FamilyFirst::Pending(Box::new(sighting()))),
                ),
            ]),
            clusters: BTreeMap::from([("c:1".to_owned(), cluster)]),
            untracked: true,
        };
        ErrorsLedger {
            identity: crate::limits::LEDGER_IDENTITY,
            revision: 4,
            targets: BTreeMap::from([("01M51EX04HHWM46XE9F2B9JXED".to_owned(), target)]),
        }
    }

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        let json = serde_json::to_string(value).unwrap();
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}: {json}"))
    }

    #[test]
    fn every_ledger_and_verdict_type_round_trips() {
        let ledger = full();
        assert_eq!(round_trip(&ledger), ledger);
        assert_eq!(
            round_trip(&ErrorsLedger::default()),
            ErrorsLedger::default()
        );
        let undecided = Verdict {
            state: JudgmentState::Deferred,
            suspect: None,
            gap: Some(JudgmentGap::HistoryUnreadable),
            ..verdict()
        };
        assert_eq!(round_trip(&undecided), undecided);
        for reason in [
            BaselineReason::FirstRead,
            BaselineReason::NewGeneration,
            BaselineReason::CoverageIncomplete,
        ] {
            assert_eq!(round_trip(&reason), reason);
        }
        for state in [
            JudgmentState::Complete,
            JudgmentState::Deferred,
            JudgmentState::Unsupported,
        ] {
            assert_eq!(round_trip(&state), state);
        }
        for gap in [
            JudgmentGap::HistoryUnreadable,
            JudgmentGap::GenerationEnded,
            JudgmentGap::ProviderChanged,
        ] {
            assert_eq!(round_trip(&gap), gap);
        }
        for relation in [
            Relation::Own,
            Relation::InStack,
            Relation::Dependency,
            Relation::None,
        ] {
            assert_eq!(round_trip(&relation), relation);
        }
        for unknown in [
            Unknown::Mark,
            Unknown::Sessions,
            Unknown::Running,
            Unknown::Accounts,
            Unknown::Time,
            Unknown::ClientVersion,
            Unknown::Dependencies,
            Unknown::Transition,
        ] {
            assert_eq!(round_trip(&unknown), unknown);
        }
        for confidence in [Confidence::High, Confidence::Medium, Confidence::Low] {
            assert_eq!(round_trip(&confidence), confidence);
        }
    }

    #[test]
    fn the_wire_names_are_the_designs() {
        let json = serde_json::to_value(full()).unwrap();
        let target = &json["targets"]["01M51EX04HHWM46XE9F2B9JXED"];
        assert_eq!(
            target["families"]["a:1"]["first"],
            serde_json::json!({"basis": "baseline", "at": "2027-01-15T08:00:03Z", "reason": "coverage_incomplete"})
        );
        assert_eq!(
            target["families"]["a:2"]["first"],
            serde_json::json!({"basis": "observed", "cluster": "c:1"})
        );
        assert_eq!(target["families"]["a:3"]["first"]["basis"], "pending");
        assert_eq!(target["families"]["a:3"]["first"]["entry_session"], 7);
        let suspect = &target["clusters"]["c:1"]["judgment"]["suspect"];
        assert_eq!(suspect["relation"], "in_stack");
        assert_eq!(suspect["confidence"], "medium");
        assert_eq!(
            suspect["evidence"]["unknowns"],
            serde_json::json!(["running", "client_version"])
        );
        assert_eq!(target["clusters"]["c:1"]["judgment"]["state"], "complete");
    }

    #[test]
    fn absent_optional_fields_are_left_out_and_read_back_as_defaults() {
        let mut ledger = full();
        let target = ledger.targets.values_mut().next().unwrap();
        target.untracked = false;
        let db = target.databases.values_mut().next().unwrap();
        db.invalidated_at = None;
        db.unproved_stamp = None;
        let cluster = target.clusters.values_mut().next().unwrap();
        cluster.judgment = None;
        cluster.addon = None;
        cluster.appearance.earliest = None;
        let json = serde_json::to_string(&ledger).unwrap();
        for key in [
            "untracked",
            "invalidated_at",
            "unproved_stamp",
            "judgment",
            "addon",
            "earliest",
        ] {
            assert!(!json.contains(&format!("\"{key}\"")), "{key} in {json}");
        }
        assert_eq!(round_trip(&ledger), ledger);
        // Every collection and counter may be missing.
        let bare: ErrorsLedger = serde_json::from_str(r#"{"targets": {"k": {}}}"#).unwrap();
        assert_eq!(bare.identity, 0);
        assert_eq!(bare.targets["k"], TargetLedger::default());
    }

    /// Every property name in the ledger's schema, the verdict's included. A new field
    /// fails this test until it is added here, after checking that it cannot hold an
    /// error's message or stack text: the ledger holds keys, sessions, stamps, times,
    /// versions and names only.
    #[cfg(feature = "schemas")]
    #[test]
    fn no_field_could_hold_error_text() {
        fn properties(value: &serde_json::Value, names: &mut std::collections::BTreeSet<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    if let Some(serde_json::Value::Object(props)) = map.get("properties") {
                        names.extend(props.keys().cloned());
                    }
                    map.values().for_each(|v| properties(v, names));
                }
                serde_json::Value::Array(list) => list.iter().for_each(|v| properties(v, names)),
                _ => {}
            }
        }
        let schema = serde_json::to_value(schemars::schema_for!(ErrorsLedger)).unwrap();
        let mut names = std::collections::BTreeSet::new();
        properties(&schema, &mut names);
        // Free text among them: names (`addon`, `provider`), keys and hashes (`database`,
        // `family`, `cluster`, `fingerprints`, `generation`, `blob_sha256`, `commit`),
        // file stamps (`stamp`, `unproved_stamp`) and release or client labels (`source`,
        // `tag`, `version`, `client_version`, `from_version`, `to_version`).
        let reviewed = [
            "actor",
            "addon",
            "algorithm",
            "appearance",
            "at",
            "basis",
            "before",
            "blob_sha256",
            "change",
            "changed",
            "clean_sessions",
            "client_update",
            "client_version",
            "cluster",
            "clusters",
            "commit",
            "confidence",
            "counter",
            "database",
            "databases",
            "earliest",
            "entry_session",
            "evidence",
            "exact",
            "families",
            "family",
            "fingerprints",
            "first",
            "first_observed_at",
            "found",
            "from",
            "from_version",
            "gap",
            "gap_s",
            "generation",
            "identity",
            "invalidated_at",
            "judged_at",
            "judgment",
            "kind",
            "last_observed_at",
            "last_present_at",
            "marks",
            "max_session",
            "max_written_at",
            "meaning",
            "observed_at",
            "other_changes",
            "overflowed",
            "provider",
            "reads",
            "reason",
            "relation",
            "release",
            "revision",
            "run",
            "running",
            "same_session",
            "session",
            "settled",
            "source",
            "stamp",
            "started_at",
            "state",
            "suspect",
            "tag",
            "targets",
            "time",
            "to",
            "to_version",
            "tx",
            "tz",
            "unknowns",
            "unproved_stamp",
            "untracked",
            "version",
            "witness",
            "written_at",
        ];
        assert_eq!(
            names.iter().map(String::as_str).collect::<Vec<_>>(),
            reviewed,
            "a field was added to or removed from the errors ledger"
        );
    }

    // -----------------------------------------------------------------------------------
    // sight

    use crate::errors::observe::Gap;

    const A: &str = "d:a";
    const B: &str = "d:b";
    const C: &str = "d:c";
    const DAY: i64 = 24 * 60 * 60;

    fn no_time() -> TimeEvidence {
        TimeEvidence {
            at: None,
            meaning: TimeMeaning::Unknown,
            tz: TzEvidence::SystemAssumed,
        }
    }

    /// An entry of `family` with fingerprint `fp`, once, in session 1, with no time.
    fn e(family: &str, fp: &str) -> EntrySighting {
        EntrySighting {
            family: family.to_owned(),
            fingerprint: fp.to_owned(),
            session: Some(1),
            time: no_time(),
            counter: 1,
        }
    }

    fn proved(entries: Vec<EntrySighting>) -> Option<DbRead> {
        Some(DbRead {
            coverage: Coverage::Proved,
            entries,
        })
    }

    fn degraded(gap: Gap, entries: Vec<EntrySighting>) -> Option<DbRead> {
        Some(DbRead {
            coverage: Coverage::Degraded(gap),
            entries,
        })
    }

    /// Database `key` in state `stamp`, written at `written`, looked at at `observed`.
    fn seen(
        key: &str,
        stamp: &str,
        written: i64,
        observed: i64,
        session: Option<u32>,
        read: Option<DbRead>,
    ) -> DbSighting {
        DbSighting {
            key: key.to_owned(),
            provider: "!BugGrabber".to_owned(),
            stamp: stamp.to_owned(),
            written_at: Some(at(written)),
            observed_at: at(observed),
            session,
            read,
        }
    }

    fn client(now: i64) -> ClientNow {
        ClientNow {
            version: Some("12.1.5.70391".to_owned()),
            now: at(now),
        }
    }

    fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
        if items.len() <= 1 {
            return vec![items.to_vec()];
        }
        let mut all = Vec::new();
        for i in 0..items.len() {
            let mut rest = items.to_vec();
            let first = rest.remove(i);
            for mut tail in permutations(&rest) {
                tail.insert(0, first.clone());
                all.push(tail);
            }
        }
        all
    }

    /// `sight` over every order of `dbs`, which must all give one ledger and one result.
    fn run_with(
        ledger: &TargetLedger,
        dbs: &[DbSighting],
        restores: &[Restored],
        now: i64,
        discovered: bool,
        mode: SightMode,
    ) -> (TargetLedger, Sighted) {
        let mut outcomes = permutations(dbs).into_iter().map(|order| {
            let mut next = ledger.clone();
            let sighted = sight(&mut next, &order, restores, &client(now), discovered, mode);
            (next, sighted)
        });
        let first = outcomes.next().unwrap();
        for other in outcomes {
            assert_eq!(other, first, "the order of dbs changed the outcome");
        }
        first
    }

    fn run(ledger: &TargetLedger, dbs: &[DbSighting], now: i64) -> (TargetLedger, Sighted) {
        run_with(ledger, dbs, &[], now, true, SightMode::Full)
    }

    fn basis(ledger: &TargetLedger, family: &str) -> Option<FamilyFirst> {
        ledger.families.get(family).map(|f| f.first.clone())
    }

    fn baseline(ledger: &TargetLedger, family: &str) -> Option<BaselineReason> {
        match basis(ledger, family)? {
            FamilyFirst::Baseline { reason, .. } => Some(reason),
            _ => None,
        }
    }

    fn observed_cluster<'a>(ledger: &'a TargetLedger, family: &str) -> &'a Cluster {
        match basis(ledger, family) {
            Some(FamilyFirst::Observed { cluster }) => &ledger.clusters[&cluster],
            other => panic!("{family} is {other:?}, not observed"),
        }
    }

    /// A's first proved read: state `s1`, written at 100, looked at at 110, session 3,
    /// holding family `a:1`.
    fn first_read() -> TargetLedger {
        run(
            &TargetLedger::default(),
            &[seen(
                A,
                "s1",
                100,
                110,
                Some(3),
                proved(vec![e("a:1", "e:1")]),
            )],
            110,
        )
        .0
    }

    #[test]
    fn a_first_proved_read_starts_a_generation_and_baselines_what_it_holds() {
        let (ledger, sighted) = run(
            &TargetLedger::default(),
            &[seen(
                A,
                "s1",
                100,
                110,
                Some(3),
                proved(vec![e("a:1", "e:1")]),
            )],
            115,
        );
        assert_eq!(sighted.regenerated, [A]);
        assert!(sighted.appeared.is_empty());
        let db = &ledger.databases[A];
        assert_eq!(db.generation, DbGeneration::of(A, "s1", at(110)));
        assert_eq!(db.started_at, at(110));
        assert_eq!(
            (db.max_session, db.max_written_at),
            (Some(3), Some(at(100)))
        );
        assert_eq!(
            db.reads,
            [read_of(
                &seen(A, "s1", 100, 110, Some(3), None),
                &client(115)
            )]
        );
        assert_eq!(
            ledger.families["a:1"],
            Family {
                first: FamilyFirst::Baseline {
                    at: at(115),
                    reason: BaselineReason::FirstRead,
                },
                last_present_at: at(115),
            }
        );
        assert!(ledger.clusters.is_empty());
    }

    #[test]
    fn an_unchanged_file_extends_its_read_and_touches_nothing_else() {
        let ledger = first_read();
        for (case, read) in [
            ("parsed", proved(vec![e("a:1", "e:1")])),
            ("not parsed", None),
        ] {
            let (after, sighted) = run(&ledger, &[seen(A, "s1", 100, 200, Some(3), read)], 200);
            let mut expected = ledger.clone();
            expected.databases.get_mut(A).unwrap().reads[0].last_observed_at = at(200);
            assert_eq!(after, expected, "{case}");
            assert_eq!(sighted, Sighted::default(), "{case}");
        }
    }

    #[test]
    fn a_new_proved_state_records_the_appearance_of_a_family_it_lacked() {
        let ledger = first_read();
        let mut late = e("a:2", "e:3");
        late.session = Some(5);
        late.counter = 3;
        late.time = time(Some(at(305)));
        let mut early = e("a:2", "e:2");
        early.session = Some(4);
        early.counter = 2;
        let (after, sighted) = run(
            &ledger,
            &[seen(
                A,
                "s2",
                300,
                310,
                Some(5),
                proved(vec![e("a:1", "e:1"), late, early]),
            )],
            320,
        );
        let g = ledger.databases[A].generation.clone();
        let fps = members(&["e:2", "e:3"]);
        let id = cluster_id("a:2", A, &g, "s2", &fps);
        assert_eq!(sighted.appeared, [id.as_str()]);
        assert!(sighted.regenerated.is_empty());
        assert_eq!(
            basis(&after, "a:2"),
            Some(FamilyFirst::Observed {
                cluster: id.clone()
            })
        );
        assert_eq!(
            after.clusters[&id],
            Cluster {
                family: "a:2".to_owned(),
                appearance: Appearance {
                    database: A.to_owned(),
                    generation: g,
                    provider: "!BugGrabber".to_owned(),
                    before: ReadMark {
                        stamp: "s1".to_owned(),
                        written_at: Some(at(100)),
                        session: Some(3),
                        observed_at: at(110),
                        client_version: Some("12.1.5.70391".to_owned()),
                    },
                    found: ReadMark {
                        stamp: "s2".to_owned(),
                        written_at: Some(at(300)),
                        session: Some(5),
                        observed_at: at(310),
                        client_version: Some("12.1.5.70391".to_owned()),
                    },
                    earliest: None,
                    entry_session: Some(4),
                    time: time(Some(at(305))),
                    counter: 5,
                },
                fingerprints: fps,
                overflowed: false,
                judgment: None,
                addon: None,
            }
        );
        assert_eq!(after.families["a:1"].last_present_at, at(320));
        let db = &after.databases[A];
        assert_eq!(db.reads.len(), 2);
        assert_eq!(
            (db.max_session, db.max_written_at),
            (Some(5), Some(at(300)))
        );
    }

    #[test]
    fn a_degraded_read_refreshes_and_notes_and_records_nothing_else() {
        let ledger = first_read();
        for gap in [
            Gap::NotAFile,
            Gap::Unread,
            Gap::Unstable,
            Gap::Truncated,
            Gap::Unrecognized,
            Gap::Dropped,
        ] {
            let (after, sighted) = run(
                &ledger,
                &[seen(
                    A,
                    "s2",
                    200,
                    210,
                    Some(4),
                    degraded(gap, vec![e("a:1", "e:1"), e("a:3", "e:3")]),
                )],
                220,
            );
            assert_eq!(sighted, Sighted::default(), "{gap:?}");
            let mut expected = ledger.clone();
            expected.families.get_mut("a:1").unwrap().last_present_at = at(220);
            expected.databases.get_mut(A).unwrap().unproved_stamp = Some("s2".to_owned());
            expected.families.insert(
                "a:3".to_owned(),
                Family {
                    first: FamilyFirst::Pending(Box::new(PositiveSighting {
                        database: A.to_owned(),
                        generation: ledger.databases[A].generation.clone(),
                        observed_at: at(210),
                        written_at: Some(at(200)),
                        entry_session: Some(1),
                        time: no_time(),
                    })),
                    last_present_at: at(220),
                },
            );
            assert_eq!(after, expected, "{gap:?}");
        }
    }

    #[test]
    fn a_degraded_read_is_neither_before_nor_found_but_tightens_as_earliest() {
        let ledger = first_read();
        let (noted, _) = run(
            &ledger,
            &[seen(
                A,
                "s2",
                200,
                210,
                Some(4),
                degraded(Gap::Dropped, vec![e("a:3", "e:3")]),
            )],
            210,
        );
        let Some(FamilyFirst::Pending(note)) = basis(&noted, "a:3") else {
            panic!("a:3 is not pending");
        };
        let (after, sighted) = run(
            &noted,
            &[seen(
                A,
                "s3",
                300,
                310,
                Some(5),
                proved(vec![e("a:3", "e:3")]),
            )],
            310,
        );
        assert_eq!(sighted.appeared.len(), 1);
        let cluster = observed_cluster(&after, "a:3");
        assert_eq!(cluster.appearance.before.stamp, "s1");
        assert_eq!(cluster.appearance.found.stamp, "s3");
        assert_eq!(cluster.appearance.earliest.as_ref(), Some(&*note));
        assert_eq!(after.databases[A].unproved_stamp, None);
    }

    #[test]
    fn a_pending_note_goes_when_its_databases_next_proved_read_lacks_it() {
        let ledger = first_read();
        let (noted, _) = run(
            &ledger,
            &[seen(
                A,
                "s2",
                200,
                210,
                Some(4),
                degraded(Gap::Unstable, vec![e("a:3", "e:3")]),
            )],
            210,
        );
        assert!(matches!(
            basis(&noted, "a:3"),
            Some(FamilyFirst::Pending(_))
        ));
        let (lacking, _) = run(
            &noted,
            &[seen(
                A,
                "s3",
                300,
                310,
                Some(5),
                proved(vec![e("a:1", "e:1")]),
            )],
            310,
        );
        assert_eq!(basis(&lacking, "a:3"), None);
        // A new generation drops the database's notes and baselines what its read holds.
        let (regenerated, sighted) = run(
            &noted,
            &[seen(
                A,
                "s3",
                300,
                310,
                Some(1),
                proved(vec![e("a:4", "e:4")]),
            )],
            310,
        );
        assert_eq!(sighted.regenerated, [A]);
        assert_eq!(basis(&regenerated, "a:3"), None);
        assert_eq!(
            baseline(&regenerated, "a:4"),
            Some(BaselineReason::NewGeneration)
        );
    }

    /// A and B, each after its first proved read: A holding `a:1`, B holding `b:1`.
    fn two_databases() -> TargetLedger {
        run(
            &TargetLedger::default(),
            &[
                seen(A, "s1", 100, 110, Some(3), proved(vec![e("a:1", "e:1")])),
                seen(B, "t1", 100, 110, Some(3), proved(vec![e("b:1", "e:b1")])),
            ],
            110,
        )
        .0
    }

    #[test]
    fn an_appearance_needs_the_whole_client_seen() {
        let ledger = two_databases();
        let a_new = || seen(A, "s2", 300, 310, Some(4), proved(vec![e("a:5", "e:5")]));
        let b = |stamp: &str, read| seen(B, stamp, 100, 310, Some(3), read);
        // (case, sightings, discovered, mode, watched to appear)
        let cases: Vec<(&str, Vec<DbSighting>, bool, SightMode, bool)> = vec![
            (
                "B read unchanged",
                vec![a_new(), b("t1", proved(vec![e("b:1", "e:b1")]))],
                true,
                SightMode::Full,
                true,
            ),
            (
                "B not parsed, unchanged",
                vec![a_new(), b("t1", None)],
                true,
                SightMode::Full,
                true,
            ),
            (
                "B degraded",
                vec![a_new(), b("t2", degraded(Gap::Unread, vec![]))],
                true,
                SightMode::Full,
                false,
            ),
            (
                "B not parsed, changed",
                vec![a_new(), b("t2", None)],
                true,
                SightMode::Full,
                false,
            ),
            (
                "the walk was cut",
                vec![a_new(), b("t1", None)],
                false,
                SightMode::Full,
                false,
            ),
            (
                "B deleted, in full",
                vec![a_new()],
                true,
                SightMode::Full,
                true,
            ),
            (
                "B not listed, in light",
                vec![a_new()],
                true,
                SightMode::Light,
                false,
            ),
            (
                "B listed, in light",
                vec![a_new(), b("t1", None)],
                true,
                SightMode::Light,
                true,
            ),
        ];
        for (case, dbs, discovered, mode, appears) in cases {
            let (after, sighted) = run_with(&ledger, &dbs, &[], 320, discovered, mode);
            if appears {
                assert_eq!(sighted.appeared.len(), 1, "{case}");
                assert_eq!(
                    observed_cluster(&after, "a:5").appearance.database,
                    A,
                    "{case}"
                );
            } else {
                assert!(sighted.appeared.is_empty(), "{case}");
                assert_eq!(
                    baseline(&after, "a:5"),
                    Some(BaselineReason::CoverageIncomplete),
                    "{case}"
                );
            }
        }
    }

    #[test]
    fn a_family_known_anywhere_on_the_client_never_appears() {
        let ledger = two_databases();
        let (after, sighted) = run(
            &ledger,
            &[
                seen(A, "s1", 100, 310, Some(3), proved(vec![e("a:1", "e:1")])),
                seen(
                    B,
                    "t2",
                    300,
                    310,
                    Some(4),
                    proved(vec![e("a:1", "e:9"), e("b:1", "e:b1")]),
                ),
            ],
            320,
        );
        assert!(sighted.appeared.is_empty());
        assert_eq!(baseline(&after, "a:1"), Some(BaselineReason::FirstRead));
        assert_eq!(after.families["a:1"].last_present_at, at(320));
    }

    #[test]
    fn a_stale_sighting_is_ignored() {
        let ledger = first_read();
        let (newer, _) = run(
            &ledger,
            &[seen(
                A,
                "s2",
                300,
                310,
                Some(4),
                proved(vec![e("a:1", "e:1")]),
            )],
            310,
        );
        // Parsed before the newer state was recorded, with another stamp.
        let (after, sighted) = run(
            &newer,
            &[seen(
                A,
                "s9",
                250,
                260,
                Some(4),
                proved(vec![e("a:7", "e:7")]),
            )],
            320,
        );
        assert_eq!(after, newer);
        assert_eq!(sighted, Sighted::default());
    }

    #[test]
    fn a_new_family_in_a_continuing_database_and_a_new_accounts_first_read_is_a_baseline() {
        let ledger = first_read();
        let (after, sighted) = run(
            &ledger,
            &[
                seen(A, "s2", 300, 310, Some(4), proved(vec![e("a:6", "e:6")])),
                seen(C, "u1", 50, 310, Some(9), proved(vec![e("a:6", "e:6")])),
            ],
            320,
        );
        assert!(sighted.appeared.is_empty());
        assert_eq!(sighted.regenerated, [C]);
        assert_eq!(baseline(&after, "a:6"), Some(BaselineReason::FirstRead));
    }

    #[test]
    fn one_new_family_in_two_databases_is_one_cluster_where_it_was_written_first() {
        let ledger = two_databases();
        for (a_written, b_written, home) in [(300, 250, B), (250, 300, A), (300, 300, A)] {
            let (after, sighted) = run(
                &ledger,
                &[
                    seen(
                        A,
                        "s2",
                        a_written,
                        310,
                        Some(4),
                        proved(vec![e("a:8", "e:8")]),
                    ),
                    seen(
                        B,
                        "t2",
                        b_written,
                        310,
                        Some(4),
                        proved(vec![e("a:8", "e:8b")]),
                    ),
                ],
                320,
            );
            assert_eq!(sighted.appeared.len(), 1);
            assert_eq!(after.clusters.len(), 1);
            assert_eq!(
                observed_cluster(&after, "a:8").appearance.database,
                home,
                "A written at {a_written}, B at {b_written}"
            );
        }
    }

    #[test]
    fn eviction_needs_full_mode_and_the_whole_client_read() {
        // a:9 appears at 310, then is gone from A from 410 on.
        let ledger = first_read();
        let (ledger, _) = run(
            &ledger,
            &[seen(
                A,
                "s2",
                300,
                310,
                Some(4),
                proved(vec![e("a:1", "e:1"), e("a:9", "e:9")]),
            )],
            310,
        );
        let id = match basis(&ledger, "a:9") {
            Some(FamilyFirst::Observed { cluster }) => cluster,
            other => panic!("{other:?}"),
        };
        let (ledger, _) = run(
            &ledger,
            &[seen(
                A,
                "s3",
                400,
                410,
                Some(5),
                proved(vec![e("a:1", "e:1")]),
            )],
            410,
        );
        let late = 410 + 91 * DAY;
        let unchanged = || seen(A, "s3", 400, late, Some(5), proved(vec![e("a:1", "e:1")]));
        // (case, sightings, discovered, mode, a:9 forgotten)
        let cases: Vec<(&str, Vec<DbSighting>, bool, SightMode, bool)> = vec![
            (
                "full, the client read",
                vec![unchanged()],
                true,
                SightMode::Full,
                true,
            ),
            ("light", vec![unchanged()], true, SightMode::Light, false),
            (
                "full, the walk cut",
                vec![unchanged()],
                false,
                SightMode::Full,
                false,
            ),
            (
                "full, a file not parsed",
                vec![seen(A, "s3", 400, late, Some(5), None)],
                true,
                SightMode::Full,
                false,
            ),
        ];
        for (case, dbs, discovered, mode, forgotten) in cases {
            let (after, _) = run_with(&ledger, &dbs, &[], late, discovered, mode);
            assert_eq!(!after.families.contains_key("a:9"), forgotten, "{case}");
            assert_eq!(!after.clusters.contains_key(&id), forgotten, "{case}");
            // a:1 is present, however old its last presence: never forgotten.
            assert!(after.families.contains_key("a:1"), "{case}");
        }
    }

    #[test]
    fn absent_families_past_the_cap_go_oldest_first_and_present_ones_never() {
        let mut ledger = first_read();
        for i in 0..=LEDGER_ABSENT_MAX {
            ledger.families.insert(
                format!("a:x{i:05}"),
                Family {
                    first: FamilyFirst::Baseline {
                        at: at(0),
                        reason: BaselineReason::FirstRead,
                    },
                    last_present_at: at(i as i64),
                },
            );
        }
        let (after, _) = run(
            &ledger,
            &[seen(
                A,
                "s1",
                100,
                200,
                Some(3),
                proved(vec![e("a:1", "e:1")]),
            )],
            200,
        );
        assert!(!after.families.contains_key("a:x00000"));
        assert!(after.families.contains_key("a:x00001"));
        assert!(after.families.contains_key("a:1"));
        assert_eq!(after.families.len(), LEDGER_ABSENT_MAX + 1);
    }

    #[test]
    fn a_deleted_accounts_database_is_forgotten_only_in_full_mode() {
        let ledger = two_databases();
        let late = 110 + 91 * DAY;
        let a = || seen(A, "s1", 100, late, Some(3), proved(vec![e("a:1", "e:1")]));
        let (full, _) = run_with(&ledger, &[a()], &[], late, true, SightMode::Full);
        assert!(!full.databases.contains_key(B));
        let (light, _) = run_with(&ledger, &[a()], &[], late, true, SightMode::Light);
        assert!(light.databases.contains_key(B));
    }

    #[test]
    fn reads_and_marks_are_bounded_and_a_cluster_outlives_its_reads() {
        let mut ledger = first_read();
        let (appeared, _) = run(
            &ledger,
            &[seen(
                A,
                "s2",
                200,
                210,
                Some(4),
                proved(vec![e("a:2", "e:2")]),
            )],
            210,
        );
        ledger = appeared;
        let cluster = observed_cluster(&ledger, "a:2").clone();
        for i in 3..=40u32 {
            let t = i64::from(i) * 100;
            ledger = run(
                &ledger,
                &[seen(
                    A,
                    &format!("s{i}"),
                    t,
                    t + 10,
                    Some(i + 2),
                    proved(vec![e("a:2", "e:2")]),
                )],
                t + 10,
            )
            .0;
        }
        let reads = &ledger.databases[A].reads;
        assert_eq!(reads.len(), LEDGER_READS_MAX);
        assert_eq!(reads[0].stamp, "s9");
        assert_eq!(reads[LEDGER_READS_MAX - 1].stamp, "s40");
        assert_eq!(observed_cluster(&ledger, "a:2"), &cluster);

        let now = 5_000;
        let mark = |at_s: i64| ChangeMark {
            run: "01M51EX04HHWM46XE9F2B9JXED".parse().unwrap(),
            at: at(at_s),
            generation: ledger.databases[A].generation.clone(),
            stamp: "s40".to_owned(),
            session: Some(42),
            running: None,
            settled: true,
        };
        let mut marks = vec![mark(now - 15 * DAY)];
        marks.extend((0..=LEDGER_MARKS_MAX as i64).map(|i| mark(now - 1_000 + i)));
        ledger.databases.get_mut(A).unwrap().marks = marks;
        let (after, _) = run(&ledger, &[seen(A, "s40", 4_000, now, Some(42), None)], now);
        let kept = &after.databases[A].marks;
        assert_eq!(kept.len(), LEDGER_MARKS_MAX);
        assert_eq!(
            kept[0].at,
            at(now - 1_000 + 1),
            "the too-old and the oldest went"
        );
    }

    #[test]
    fn a_cluster_past_its_cap_is_overflowed() {
        let ledger = first_read();
        let many: Vec<EntrySighting> = (0..=LEDGER_CLUSTER_FINGERPRINTS_MAX)
            .map(|i| e("a:2", &format!("e:{i:02}")))
            .collect();
        let (after, _) = run(
            &ledger,
            &[seen(A, "s2", 200, 210, Some(4), proved(many))],
            210,
        );
        let cluster = observed_cluster(&after, "a:2");
        assert!(cluster.overflowed);
        assert_eq!(cluster.fingerprints.len(), LEDGER_CLUSTER_FINGERPRINTS_MAX);
        assert_eq!(cluster.fingerprints[0], "e:00");
        assert_eq!(
            cluster.appearance.counter,
            LEDGER_CLUSTER_FINGERPRINTS_MAX as u32 + 1,
            "every entry counts, member or not"
        );
    }

    #[test]
    fn a_batch_past_the_families_cap_forgets_absent_ones_or_leaves_the_client_untracked() {
        let mut full = first_read();
        for i in 0..LEDGER_FAMILIES_MAX - 1 {
            full.families.insert(
                format!("a:x{i:05}"),
                Family {
                    first: FamilyFirst::Baseline {
                        at: at(0),
                        reason: BaselineReason::FirstRead,
                    },
                    last_present_at: at(105),
                },
            );
        }
        assert_eq!(full.families.len(), LEDGER_FAMILIES_MAX);
        let next = || {
            seen(
                A,
                "s2",
                200,
                210,
                Some(4),
                proved(vec![e("a:1", "e:1"), e("a:2", "e:2")]),
            )
        };
        // Covered and full: absent families make room, oldest first (and the same pass
        // then holds the absent ones to their own cap).
        let (room, sighted) = run(&full, &[next()], 210);
        assert!(room.families.len() <= LEDGER_FAMILIES_MAX);
        assert!(!room.families.contains_key("a:x00000"));
        assert_eq!(sighted.appeared.len(), 1);
        assert!(room.families.contains_key("a:1"));
        // Not covered: nothing is recorded, and the client is untracked.
        let (untracked, sighted) = run_with(&full, &[next()], &[], 210, false, SightMode::Full);
        assert_eq!(
            untracked,
            TargetLedger {
                untracked: true,
                ..TargetLedger::default()
            }
        );
        assert_eq!(sighted, Sighted::default());
        // Light, or uncovered, never tracks it again.
        for (discovered, mode) in [(true, SightMode::Light), (false, SightMode::Full)] {
            let (still, _) = run_with(&untracked, &[next()], &[], 300, discovered, mode);
            assert_eq!(still, untracked);
        }
        // A full, covered batch that fits starts every database afresh: what was present
        // while untracked is a baseline, never an appearance.
        let (tracked, sighted) = run(&untracked, &[next()], 300);
        assert!(!tracked.untracked);
        assert!(sighted.appeared.is_empty());
        assert_eq!(sighted.regenerated, [A]);
        assert_eq!(baseline(&tracked, "a:2"), Some(BaselineReason::FirstRead));
    }

    #[test]
    fn generations_start_only_where_the_files_lifetime_changed() {
        let ledger = first_read(); // s1, written 100, looked at 110, session 3.
        let whole = |t: i64| Restored {
            at: at(t),
            whole: true,
            providers: vec![],
        };
        let addon = |t: i64, folder: &str| Restored {
            at: at(t),
            whole: false,
            providers: vec![folder.to_owned()],
        };
        let next = |stamp: &str, written: i64, observed: i64, session: Option<u32>| {
            seen(
                A,
                stamp,
                written,
                observed,
                session,
                proved(vec![e("a:1", "e:1")]),
            )
        };
        // (case, sighting, restores, a new generation, or stale and ignored)
        let cases: Vec<(&str, DbSighting, Vec<Restored>, Option<bool>)> = vec![
            (
                "the counter went down",
                next("s2", 200, 210, Some(2)),
                vec![],
                Some(true),
            ),
            (
                "a restore, then an equal counter",
                next("s2", 200, 210, Some(3)),
                vec![whole(150)],
                Some(true),
            ),
            (
                "a restore, then a higher counter",
                next("s2", 200, 210, Some(9)),
                vec![whole(150)],
                Some(true),
            ),
            (
                "an addon restore of its provider, any case",
                next("s2", 200, 210, Some(4)),
                vec![addon(150, "!bugGRABBER")],
                Some(true),
            ),
            (
                "another addon's restore",
                next("s2", 200, 210, Some(4)),
                vec![addon(150, "Foo")],
                Some(false),
            ),
            (
                "a restore older than the newest read",
                next("s2", 200, 210, Some(4)),
                vec![whole(105)],
                Some(false),
            ),
            (
                "looked at before the restore",
                next("s1", 100, 140, Some(3)),
                vec![whole(150)],
                None,
            ),
            (
                "looked at as the restore committed",
                next("s2", 200, 150, Some(4)),
                vec![whole(150)],
                None,
            ),
            (
                "an older file",
                next("s2", 97, 210, Some(4)),
                vec![],
                Some(true),
            ),
            (
                "a file older within the coarse slack",
                next("s2", 99, 210, Some(4)),
                vec![],
                Some(false),
            ),
            (
                "a new stamp, later, a higher counter",
                next("s2", 200, 210, Some(4)),
                vec![],
                Some(false),
            ),
            (
                "no counter now",
                next("s2", 200, 210, None),
                vec![],
                Some(false),
            ),
        ];
        for (case, sighting, restores, outcome) in cases {
            let (after, sighted) =
                run_with(&ledger, &[sighting], &restores, 220, true, SightMode::Full);
            let regenerated = after.databases[A].generation != ledger.databases[A].generation;
            match outcome {
                Some(new) => {
                    assert_eq!(regenerated, new, "{case}");
                    assert_eq!(sighted.regenerated.len(), usize::from(new), "{case}");
                    assert_eq!(after.databases[A].invalidated_at, None, "{case}");
                }
                None => {
                    assert!(!regenerated, "{case}");
                    assert_eq!(
                        after.databases[A].reads, ledger.databases[A].reads,
                        "{case}"
                    );
                    assert_eq!(after.databases[A].invalidated_at, Some(at(150)), "{case}");
                }
            }
        }
        // A database that never saved a counter never starts a generation by it.
        let unnumbered = run(
            &TargetLedger::default(),
            &[seen(A, "s1", 100, 110, None, proved(vec![]))],
            110,
        )
        .0;
        let (after, sighted) = run(
            &unnumbered,
            &[seen(A, "s2", 200, 210, Some(1), proved(vec![]))],
            210,
        );
        assert!(sighted.regenerated.is_empty());
        assert_eq!(
            after.databases[A].generation,
            unnumbered.databases[A].generation
        );
    }

    #[test]
    fn a_new_generation_clears_its_database_and_ends_unfinished_judgments() {
        let ledger = first_read();
        let (ledger, _) = run(
            &ledger,
            &[seen(
                A,
                "s2",
                200,
                210,
                Some(4),
                proved(vec![e("a:1", "e:1"), e("a:2", "e:2"), e("a:3", "e:3")]),
            )],
            210,
        );
        let mut ledger = ledger;
        let complete = Verdict {
            algorithm: SUSPECT_ALGORITHM,
            state: JudgmentState::Complete,
            judged_at: at(215),
            suspect: None,
            gap: None,
        };
        let id3 = match basis(&ledger, "a:3") {
            Some(FamilyFirst::Observed { cluster }) => cluster,
            other => panic!("{other:?}"),
        };
        ledger.clusters.get_mut(&id3).unwrap().judgment = Some(complete.clone());
        ledger.databases.get_mut(A).unwrap().marks = vec![ChangeMark {
            run: "01M51EX04HHWM46XE9F2B9JXED".parse().unwrap(),
            at: at(212),
            generation: ledger.databases[A].generation.clone(),
            stamp: "s2".to_owned(),
            session: Some(4),
            running: Some(false),
            settled: true,
        }];
        let (after, sighted) = run(
            &ledger,
            &[seen(
                A,
                "s3",
                300,
                310,
                Some(1),
                proved(vec![e("a:1", "e:1"), e("a:4", "e:4")]),
            )],
            320,
        );
        assert_eq!(sighted.regenerated, [A]);
        let db = &after.databases[A];
        assert_eq!(db.reads.len(), 1);
        assert!(db.marks.is_empty());
        assert_eq!(db.started_at, at(310));
        assert_eq!(after.families.len(), 4, "recurrence memory is kept");
        assert_eq!(baseline(&after, "a:4"), Some(BaselineReason::NewGeneration));
        assert_eq!(
            observed_cluster(&after, "a:2").judgment,
            Some(Verdict {
                algorithm: SUSPECT_ALGORITHM,
                state: JudgmentState::Unsupported,
                judged_at: at(320),
                suspect: None,
                gap: Some(JudgmentGap::GenerationEnded),
            })
        );
        assert_eq!(observed_cluster(&after, "a:3").judgment, Some(complete));
    }

    #[test]
    fn sight_takes_its_time_from_the_client_and_keeps_no_error_text() {
        use crate::errors::sanitize::{family_key, fingerprint, normalized, strict_message};
        let message = "Interface/AddOns/Guidelime/Guidelime.lua:96: attempt to index local 'guide' (a nil value)";
        let entry = EntrySighting {
            family: family_key(message),
            fingerprint: fingerprint(
                &normalized(message),
                strict_message(message).location.as_deref(),
            ),
            session: Some(7),
            time: no_time(),
            counter: 2,
        };
        let ledger = first_read();
        let now = -20 * 365 * DAY;
        let (after, _) = run(
            &ledger,
            &[seen(A, "s2", 200, 210, Some(7), proved(vec![entry]))],
            now,
        );
        assert_eq!(
            after.families[&family_key(message)].last_present_at,
            at(now)
        );
        let json = serde_json::to_string(&after).unwrap();
        for text in [
            "Guidelime",
            "guide",
            "attempt to index",
            "nil value",
            "registerGuide",
        ] {
            assert!(!json.contains(text), "{text} in {json}");
        }
    }

    // -----------------------------------------------------------------------------------
    // The store

    use std::os::unix::fs::MetadataExt;

    use crate::fsx::Lease;

    fn store_home() -> (tempfile::TempDir, Paths, ScopeTable) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::with_roots(root, root.join("c"), root.join("s"), root.join("k"));
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        (dir, paths, scopes)
    }

    fn inode(paths: &Paths) -> Option<u64> {
        std::fs::metadata(paths.errors_ledger())
            .ok()
            .map(|m| m.ino())
    }

    /// Opens a session, records A's first read on client `k`, and commits.
    fn first_commit(paths: &Paths, scopes: &ScopeTable) -> ErrorsLedger {
        let mut session = open(paths, scopes, "test").unwrap();
        session.sight(
            "k",
            &[seen(
                A,
                "s1",
                100,
                110,
                Some(3),
                proved(vec![e("a:1", "e:1")]),
            )],
            &[],
            &client(110),
            true,
            SightMode::Full,
        );
        session.commit().unwrap().ledger
    }

    #[test]
    fn a_commit_round_trips_and_an_unchanged_one_writes_nothing() {
        let (_dir, paths, scopes) = store_home();
        // Nothing recorded on a missing file writes nothing.
        open(&paths, &scopes, "test").unwrap().commit().unwrap();
        assert_eq!(inode(&paths), None);
        let stored = first_commit(&paths, &scopes);
        assert_eq!(stored.revision, 1);
        assert_eq!(stored.identity, LEDGER_IDENTITY);
        assert_eq!(load(&paths), Some(stored.clone()));
        let before = inode(&paths);
        let again = open(&paths, &scopes, "test").unwrap().commit().unwrap();
        assert_eq!(again.ledger, stored);
        drop(again);
        assert_eq!(inode(&paths), before, "an unchanged ledger is not written");
        // A change raises the revision by one.
        let mut session = open(&paths, &scopes, "test").unwrap();
        session.sight(
            "k",
            &[seen(
                A,
                "s2",
                200,
                210,
                Some(4),
                proved(vec![e("a:1", "e:1")]),
            )],
            &[],
            &client(210),
            true,
            SightMode::Full,
        );
        assert_eq!(session.commit().unwrap().ledger.revision, 2);
    }

    #[test]
    fn a_newer_file_is_neither_believed_nor_overwritten() {
        let (_dir, paths, scopes) = store_home();
        std::fs::create_dir_all(paths.cache_dir()).unwrap();
        // (case, bytes, what load believes)
        for (case, bytes, believed) in [
            (
                "newer",
                r#"{"schema_version":"9.0","identity":1,"revision":4,"targets":{}}"#,
                true,
            ),
            (
                "newer, reshaped",
                r#"{"schema_version":"9.0","targets":"a shape this build cannot decode"}"#,
                false,
            ),
        ] {
            std::fs::write(paths.errors_ledger(), bytes).unwrap();
            assert_eq!(
                open(&paths, &scopes, "test").err(),
                Some(LedgerUnavailable::Newer),
                "{case}"
            );
            assert_eq!(load(&paths).is_some(), believed, "{case}");
            assert_eq!(
                std::fs::read_to_string(paths.errors_ledger()).unwrap(),
                bytes,
                "{case}"
            );
        }
    }

    #[test]
    fn another_identity_or_a_corrupt_file_loads_empty_and_writable() {
        let (_dir, paths, scopes) = store_home();
        let mut other = first_commit(&paths, &scopes);
        other.identity = LEDGER_IDENTITY + 1;
        other.revision = 7;
        std::fs::write(paths.errors_ledger(), store::encode(&other).unwrap()).unwrap();
        let session = open(&paths, &scopes, "test").unwrap();
        assert!(session.ledger.targets.is_empty());
        assert_eq!(
            session.ledger.revision, 7,
            "a discarded ledger keeps its revision"
        );
        drop(session);
        assert_eq!(first_commit(&paths, &scopes).revision, 8);
        std::fs::write(paths.errors_ledger(), "{ not json").unwrap();
        assert!(
            open(&paths, &scopes, "test")
                .unwrap()
                .ledger
                .targets
                .is_empty()
        );
        assert_eq!(first_commit(&paths, &scopes).revision, 1);
    }

    #[test]
    fn a_busy_lock_is_given_up_after_its_timeout() {
        let (_dir, paths, scopes) = store_home();
        let held = Lease::acquire(
            &scopes,
            &paths.errors_ledger_lock(),
            "holder",
            LEDGER_LOCK_TIMEOUT,
        )
        .unwrap();
        let started = std::time::Instant::now();
        assert_eq!(
            open(&paths, &scopes, "test").err(),
            Some(LedgerUnavailable::Busy)
        );
        let waited = started.elapsed();
        assert!(waited >= LEDGER_LOCK_TIMEOUT, "{waited:?}");
        assert!(waited < LEDGER_LOCK_TIMEOUT * 5, "{waited:?}");
        drop(held);
        assert!(open(&paths, &scopes, "test").is_ok());
    }

    #[test]
    fn a_ledger_past_its_bound_is_not_stored() {
        let (_dir, paths, scopes) = store_home();
        let mut session = open(&paths, &scopes, "test").unwrap();
        let family = Family {
            first: FamilyFirst::Baseline {
                at: at(0),
                reason: BaselineReason::FirstRead,
            },
            last_present_at: at(0),
        };
        let filler = "x".repeat(1_000);
        let target = session.ledger.targets.entry("k".to_owned()).or_default();
        for i in 0..20_000 {
            target
                .families
                .insert(format!("a:{i:05}{filler}"), family.clone());
        }
        assert!(matches!(
            session.commit().err(),
            Some(LedgerUnavailable::TooLarge { bytes }) if bytes > LEDGER_MAX_BYTES
        ));
        assert_eq!(inode(&paths), None);
    }

    /// A session over client `k` holding A at state `s2` (generation `g`), with `a:2`
    /// watched to appear there.
    fn session_with_cluster(paths: &Paths, scopes: &ScopeTable) -> (LedgerSession, String) {
        first_commit(paths, scopes);
        let mut session = open(paths, scopes, "test").unwrap();
        let sighted = session.sight(
            "k",
            &[seen(
                A,
                "s2",
                200,
                210,
                Some(4),
                proved(vec![e("a:2", "e:2")]),
            )],
            &[],
            &client(210),
            true,
            SightMode::Full,
        );
        let id = sighted.appeared[0].clone();
        (session, id)
    }

    fn mark(generation: &DbGeneration, stamp: &str, run: &str) -> ChangeMark {
        ChangeMark {
            run: run.parse().unwrap(),
            at: at(205),
            generation: generation.clone(),
            stamp: stamp.to_owned(),
            session: Some(4),
            running: Some(false),
            settled: true,
        }
    }

    #[test]
    fn a_mark_is_added_only_for_the_current_generations_newest_state() {
        let (_dir, paths, scopes) = store_home();
        let (mut session, _) = session_with_cluster(&paths, &scopes);
        let g = session.ledger.targets["k"].databases[A].generation.clone();
        let run = "01M51EX04HHWM46XE9F2B9JXED";
        let other_run = "01M51EX645RCVFN6Z4ZV5GRWVS";
        // (case, mark, database, added)
        for (case, m, db, added) in [
            ("an older state", mark(&g, "s1", run), A, false),
            (
                "another generation",
                mark(&generation(9), "s2", run),
                A,
                false,
            ),
            ("an unknown database", mark(&g, "s2", run), B, false),
            ("the newest state", mark(&g, "s2", run), A, true),
            ("a second mark of one run", mark(&g, "s2", run), A, false),
            ("another run", mark(&g, "s2", other_run), A, true),
        ] {
            assert_eq!(session.add_mark("k", db, m), added, "{case}");
        }
        assert_eq!(session.ledger.targets["k"].databases[A].marks.len(), 2);
        // Not while the file was replaced and no proved read has followed.
        session.sight(
            "k",
            &[],
            &[Restored {
                at: at(300),
                whole: true,
                providers: vec![],
            }],
            &client(300),
            true,
            SightMode::Light,
        );
        let third = "01M51EXA0000000000000000AA";
        assert!(!session.add_mark("k", A, mark(&g, "s2", third)));
    }

    #[test]
    fn a_verdict_attaches_only_to_its_open_cluster_in_the_current_generation() {
        let verdict = |state| Verdict {
            algorithm: SUSPECT_ALGORITHM,
            state,
            judged_at: at(220),
            suspect: None,
            gap: None,
        };
        let (_dir, paths, scopes) = store_home();
        let (mut session, id) = session_with_cluster(&paths, &scopes);
        assert!(!session.attach("k", "c:nothing", verdict(JudgmentState::Complete), None));
        assert!(!session.attach("other", &id, verdict(JudgmentState::Complete), None));
        assert!(
            session.attach("k", &id, verdict(JudgmentState::Deferred), None),
            "an open cluster"
        );
        assert!(
            session.attach(
                "k",
                &id,
                verdict(JudgmentState::Complete),
                Some("Foo".to_owned())
            ),
            "a deferred one"
        );
        assert!(
            !session.attach("k", &id, verdict(JudgmentState::Complete), None),
            "a complete one is kept"
        );
        assert_eq!(
            session.ledger.targets["k"].clusters[&id].addon.as_deref(),
            Some("Foo")
        );
        // A cluster of an ended generation takes no verdict.
        let (_dir, paths, scopes) = store_home();
        let (mut session, id) = session_with_cluster(&paths, &scopes);
        session.sight(
            "k",
            &[seen(
                A,
                "s3",
                300,
                310,
                Some(1),
                proved(vec![e("a:2", "e:2")]),
            )],
            &[],
            &client(310),
            true,
            SightMode::Full,
        );
        assert!(!session.attach("k", &id, verdict(JudgmentState::Complete), None));
        assert_eq!(
            session.ledger.targets["k"].clusters[&id]
                .judgment
                .as_ref()
                .map(|v| v.state),
            Some(JudgmentState::Unsupported)
        );
    }

    #[test]
    fn a_verdict_of_untrusted_rules_is_dropped_when_read() {
        let (_dir, paths, scopes) = store_home();
        let (mut session, id) = session_with_cluster(&paths, &scopes);
        let old = Verdict {
            algorithm: SUSPECT_ALGORITHM_TRUSTED_MIN - 1,
            state: JudgmentState::Complete,
            judged_at: at(220),
            suspect: None,
            gap: None,
        };
        session
            .ledger
            .targets
            .get_mut("k")
            .unwrap()
            .clusters
            .get_mut(&id)
            .unwrap()
            .judgment = Some(old.clone());
        let current = as_read(session.ledger.clone());
        assert_eq!(
            current.targets["k"].clusters[&id].judgment, None,
            "judged again"
        );
        let mut ended = session.ledger.clone();
        ended
            .targets
            .get_mut("k")
            .unwrap()
            .databases
            .get_mut(A)
            .unwrap()
            .generation = generation(9);
        let ended = as_read(ended);
        assert_eq!(
            ended.targets["k"].clusters[&id].judgment,
            Some(Verdict {
                algorithm: SUSPECT_ALGORITHM,
                state: JudgmentState::Unsupported,
                judged_at: at(220),
                suspect: None,
                gap: Some(JudgmentGap::GenerationEnded),
            }),
            "never judged again"
        );
    }
}
