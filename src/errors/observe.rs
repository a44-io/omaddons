//! One client's Lua errors, observed (M3-D19, M3-D20): every BugGrabber database
//! [`locate`] finds, read within the run's byte budget, parsed, decoded and attributed,
//! each local entry judged new or not against its database's first-read baseline. The
//! errors memo, the counts, `--check` and `status` derive from this whole observation;
//! filters (`--since`, `--addon`, `--limit`) apply only to the view afterwards, so a
//! filtered read can never make the bar think an addon is clean.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ack::{Ack, TargetAck, db_key};
use super::attribute::{Attributed, Attribution, FolderSet, attribute};
use super::buggrabber::{
    BugDb, BugEntry, DbShape, EntryType, Origin, TimeEvidence, Tri, decode, evidence,
};
use super::locate::{Capture, Coverage, GLOBAL, Listing, Skipped, locate};
use super::memo::TargetMemo;
use super::sanitize::{fingerprint, normalized, strict_message};
use crate::error::{Suggestion, Warning, WarningCode};
use crate::fsx::FileStamp;
use crate::inventory::Inventory;
use crate::limits::ERRORS_NEW_MAX_AGE;
use crate::lua::{is_too_large, parse_saved_variables, read_bounded};
use crate::state::paths::ClientPaths;

/// What an error group is attributed to (the wire's `kind`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Addon,
    /// Only a library embedded in the folder.
    Library,
    Blizzard,
    Unknown,
}

/// How a database's file was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadState {
    Complete,
    /// The input ended inside a value (a file being written): what came before is kept.
    Partial,
    /// Over `SAVEDVARIABLES_MAX_BYTES`, or past the run's byte budget: not read.
    OverBudget,
    /// It could not be read.
    Unreadable,
}

impl ReadState {
    /// Whether what was read is all there is.
    pub fn complete(self) -> bool {
        self == Self::Complete
    }
}

/// One BugGrabber database: one file of one account (or one embedding addon's).
#[derive(Debug, Clone)]
pub struct Database {
    /// `db-<n>`, in the order `locate` found them.
    pub id: String,
    /// The account's ordinal (`account N`), never its name.
    pub account: usize,
    /// `!BugGrabber` or the embedding addon's folder.
    pub provider: String,
    pub shape: DbShape,
    /// The DB's current session, when read.
    pub session: Option<u32>,
    pub state: ReadState,
    /// Sessions above it are new (M3-D19).
    pub baseline: u32,
    /// The user's dismissal, while it is valid (not above the current session).
    pub acknowledged: Option<Ack>,
    /// Relative to the client folder: for the memo only, never shown.
    pub path: String,
    pub stamp: FileStamp,
    /// When the client last wrote the file.
    pub written_at: Option<Timestamp>,
}

/// One entry of a database, attributed.
#[derive(Debug, Clone)]
pub struct Observed {
    /// Its database's index in [`Observation::databases`].
    pub database: usize,
    pub entry: BugEntry,
    pub attribution: Attribution,
    /// Its group: the managed addon, else the folder, else `Blizzard` or `unknown`.
    pub name: String,
    pub managed: bool,
    pub kind: SourceKind,
    /// The folder attribution named, when it named one.
    pub folder: Option<String>,
    pub new: bool,
    pub evidence: TimeEvidence,
    pub fingerprint: String,
}

/// Everything read on one client, before any filter.
#[derive(Debug, Clone)]
pub struct Observation {
    pub capture: Capture,
    pub coverage: Coverage,
    pub skipped: Skipped,
    pub listings: Vec<Listing>,
    pub databases: Vec<Database>,
    /// Every entry, local and received, in database order.
    pub entries: Vec<Observed>,
    pub warnings: Vec<Warning>,
}

impl Observation {
    /// Whether everything that may hold errors was read whole: no database partial,
    /// over budget or unreadable, and no bound hit.
    pub fn complete(&self) -> bool {
        self.coverage == Coverage::Complete && self.databases.iter().all(|d| d.state.complete())
    }

    pub fn local(&self) -> impl Iterator<Item = &Observed> {
        self.entries
            .iter()
            .filter(|e| e.entry.origin == Origin::Local)
    }

    pub fn received(&self) -> impl Iterator<Item = &Observed> {
        self.entries
            .iter()
            .filter(|e| e.entry.origin == Origin::Received)
    }
}

/// What reading one client needs.
pub struct Observe<'a> {
    /// The target's id, for warnings' fixes.
    pub target: &'a str,
    pub client: &'a ClientPaths,
    /// At `Depth::Full`: its folders and their loaded TOCs.
    pub inventory: &'a Inventory,
    /// The managed addon that records a folder.
    pub owner: &'a dyn Fn(&str) -> Option<String>,
    /// What the last run observed on this client.
    pub memo: Option<&'a TargetMemo>,
    /// The user's dismissals on this client.
    pub ack: Option<&'a TargetAck>,
    pub now: Timestamp,
    /// Where local-time strings are read.
    pub tz: &'a TimeZone,
}

/// Reads one client's errors, taking what it reads from `budget` (bytes left for this
/// run, every client together).
pub fn read_target(cx: &Observe<'_>, budget: &mut u64) -> Observation {
    let located = locate(cx.client, cx.inventory);
    let folders = FolderSet::new(cx.inventory.folders.iter().map(|f| f.name.as_str()));
    let mut observation = Observation {
        capture: located.capture,
        coverage: located.coverage,
        skipped: located.skipped,
        listings: located.listings,
        databases: Vec::new(),
        entries: Vec::new(),
        warnings: Vec::new(),
    };
    for (at, file) in located.files.iter().enumerate() {
        let path = file
            .path
            .strip_prefix(cx.client.client_dir())
            .unwrap_or(&file.path)
            .to_string_lossy()
            .into_owned();
        let previous = cx
            .memo
            .and_then(|m| m.databases.iter().find(|d| d.path == path));
        let (state, db) = read_db(&file.path, file.stamp, budget);
        let session = db.session.or_else(|| {
            // Not read: what the last run saw stays.
            (!state.complete() && state != ReadState::Partial)
                .then(|| previous.and_then(|p| p.session))
                .flatten()
        });
        let ack = cx
            .ack
            .and_then(|a| a.databases.get(&db_key(&path)))
            .copied();
        let baseline = match previous {
            // Reset (or replaced): the old baseline is not this database's history.
            Some(p) if session.is_some_and(|s| s < p.baseline) => 0,
            Some(p) => p.baseline,
            // A first read starts from the dismissal when there is one, so wiping the
            // cache never resurrects dismissed errors nor hides undismissed ones.
            None => match ack.filter(|a| session.is_none_or(|s| a.acked_session <= s)) {
                Some(a) => a.acked_session,
                None => session.map_or(0, |s| s.saturating_sub(1)),
            },
        };
        let written_at = std::fs::metadata(&file.path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| Timestamp::try_from(t).ok());
        let state_of = DbState {
            session,
            baseline,
            written_at,
        };
        if !state.complete() {
            observation
                .warnings
                .push(partial_warning(cx.target, file.account, state));
        }
        let index = observation.databases.len();
        let legacy = db.shape == DbShape::Legacy;
        let read: Vec<(BugEntry, TimeEvidence)> = db
            .entries
            .into_iter()
            .map(|e| {
                let at = evidence(e.time.as_ref(), cx.tz);
                (e, at)
            })
            .collect();
        // Each session's newest time, which the time clause judges per session.
        let mut newest: std::collections::BTreeMap<u32, TimeEvidence> =
            std::collections::BTreeMap::new();
        for (e, at) in &read {
            if let (Origin::Local, Some(s)) = (e.origin, e.session) {
                newest
                    .entry(s)
                    .and_modify(|n| *n = TimeEvidence::weakest(n, at))
                    .or_insert(*at);
            }
        }
        for (entry, at) in read {
            let attribution = attribute(
                &entry.message,
                entry.stack.as_deref(),
                entry.entry_type == EntryType::Event,
                &folders,
                legacy,
            );
            let (name, managed, kind, folder) = group_of(&attribution.target, cx.owner);
            let new = entry.origin == Origin::Local
                && entry.session.is_some_and(|s| {
                    session_is_new(&state_of, s, newest.get(&s), ack.as_ref(), cx.now)
                });
            observation.entries.push(Observed {
                database: index,
                fingerprint: identity(&entry.message),
                evidence: at,
                attribution,
                name,
                managed,
                kind,
                folder,
                new,
                entry,
            });
        }
        observation.databases.push(Database {
            id: format!("db-{}", at + 1),
            account: file.account,
            provider: file.folder.clone(),
            shape: db.shape,
            session,
            state,
            baseline,
            acknowledged: ack.filter(|a| session.is_none_or(|s| a.acked_session <= s)),
            path,
            stamp: file.stamp,
            written_at,
        });
    }
    observation
}

/// What deciding newness needs of a database, from a read or from the memo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbState {
    /// Its current session.
    pub session: Option<u32>,
    /// The first-read baseline.
    pub baseline: u32,
    /// When the client last wrote the file.
    pub written_at: Option<Timestamp>,
}

/// Whether a database's errors of `session` are new (M3-D19), its file written within
/// [`ERRORS_NEW_MAX_AGE`] of `now`: the session is above the larger of the first-read
/// baseline and the acknowledged session (each void while above the database's current
/// session, which a reset or a replaced file explains), or, after a dismissal, its
/// newest time (`newest`) proves it occurred after `acked_at`. One rule for `errors`,
/// `status` and the memo, so their counts agree.
pub fn session_is_new(
    db: &DbState,
    session: u32,
    newest: Option<&TimeEvidence>,
    ack: Option<&Ack>,
    now: Timestamp,
) -> bool {
    if !db
        .written_at
        .is_some_and(|at| within(at, now, ERRORS_NEW_MAX_AGE))
    {
        return false;
    }
    let valid = |mark: u32| {
        if db.session.is_some_and(|s| mark > s) {
            0
        } else {
            mark
        }
    };
    let baseline = valid(db.baseline).max(ack.map_or(0, |a| valid(a.acked_session)));
    session > baseline
        || ack.is_some_and(|a| newest.is_some_and(|t| t.at_or_after(a.acked_at) == Tri::Yes))
}

/// An error's fingerprint: its message and location, both normalized, so a library
/// version suffix of BugGrabber ≤ v12.0.2 data reads alike either way.
fn identity(message: &str) -> String {
    let location = strict_message(message).location.map(|l| normalized(&l));
    fingerprint(&normalized(message), location.as_deref())
}

/// Whether `at` is no older than `age` at `now` (a time ahead of the clock counts).
fn within(at: Timestamp, now: Timestamp, age: Duration) -> bool {
    at >= now || now.duration_since(at).unsigned_abs() <= age
}

/// One file's database and how it was read, within `budget`.
fn read_db(path: &Path, stamp: FileStamp, budget: &mut u64) -> (ReadState, BugDb) {
    // ubs:ignore — BugGrabber's SavedVariables, not a token
    let empty = || decode(&crate::lua::Value::Nil);
    if stamp.size() > *budget {
        return (ReadState::OverBudget, empty());
    }
    let bytes = match read_bounded(path) {
        Ok(bytes) => bytes,
        Err(err) if is_too_large(&err) => return (ReadState::OverBudget, empty()),
        Err(_) => return (ReadState::Unreadable, empty()),
    };
    *budget = budget.saturating_sub(bytes.len() as u64);
    let parsed = parse_saved_variables(&bytes, &[GLOBAL]);
    let state = if parsed.complete {
        ReadState::Complete
    } else {
        ReadState::Partial
    };
    let db = parsed.globals.get(GLOBAL).map_or_else(empty, decode);
    (state, db)
}

fn partial_warning(target: &str, account: usize, state: ReadState) -> Warning {
    let why = match state {
        ReadState::Partial => "it ends early (the game may be writing it)",
        ReadState::OverBudget => "it is over the size this run may read",
        ReadState::Unreadable => "it could not be read",
        ReadState::Complete => "",
    };
    Warning::new(
        WarningCode::ErrorsPartial,
        format!(
            "A BugGrabber file of account {account} on {target} could not be read completely: {why}. Counts are from what was read"
        ),
    )
    .with_fix(Suggestion::command(["omaddons", "errors", "-t", target]))
}

/// An attribution's group: the managed addon recording its folder, else the folder;
/// whether it is managed; its kind; and the folder.
fn group_of(
    target: &Attributed,
    owner: &dyn Fn(&str) -> Option<String>,
) -> (String, bool, SourceKind, Option<String>) {
    let named = |folder: &String, kind| match owner(folder) {
        Some(addon) => (addon, true, kind, Some(folder.clone())),
        None => (folder.clone(), false, kind, Some(folder.clone())),
    };
    match target {
        Attributed::Addon(folder) => named(folder, SourceKind::Addon),
        Attributed::Library(folder) => named(folder, SourceKind::Library),
        Attributed::Blizzard => ("Blizzard".to_owned(), false, SourceKind::Blizzard, None),
        Attributed::Unknown => ("unknown".to_owned(), false, SourceKind::Unknown, None),
    }
}

// ---------------------------------------------------------------------------------------
// Groups.

/// One error: entries of one database with the same fingerprint, merged.
#[derive(Debug, Clone)]
pub struct ErrorGroup<'o> {
    /// The newest of its entries, which is what is shown.
    pub shown: &'o Observed,
    pub count: u64,
    /// The sessions it occurred in.
    pub sessions: BTreeSet<Option<u32>>,
    pub new: bool,
    /// The weakest of what its entries' times prove ([`shown_time`] for the wire).
    pub last_seen: TimeEvidence,
    pub entries: usize,
}

/// One addon's (or folder's) errors, of one origin.
#[derive(Debug, Clone)]
pub struct AddonErrors<'o> {
    pub name: String,
    pub managed: bool,
    pub kind: SourceKind,
    pub origin: Origin,
    pub folders: Vec<String>,
    pub errors: Vec<ErrorGroup<'o>>,
    /// Occurrences: the entries' counters, summed.
    pub count: u64,
    /// Entries that are new.
    pub new: usize,
    pub entries: usize,
    /// The weakest of what its entries' times prove ([`shown_time`] for the wire).
    pub last_seen: TimeEvidence,
}

/// An aggregate time as shown: `None` when no entry had a usable time.
pub fn shown_time(aggregate: TimeEvidence) -> Option<TimeEvidence> {
    aggregate.at.map(|_| aggregate)
}

/// `entries` grouped by addon and kind (and origin), each group's entries merged by
/// database and fingerprint; addons by new entries, then occurrences, then name.
pub fn group<'o>(entries: &[&'o Observed]) -> Vec<AddonErrors<'o>> {
    let mut addons: Vec<AddonErrors<'o>> = Vec::new();
    let mut addon_at: std::collections::BTreeMap<(String, SourceKind, bool), usize> =
        std::collections::BTreeMap::new();
    let mut error_at: std::collections::BTreeMap<(usize, usize, &'o str), usize> =
        std::collections::BTreeMap::new();
    for &e in entries {
        let received = e.entry.origin == Origin::Received;
        let key = (e.name.to_ascii_lowercase(), e.kind, received);
        let a = *addon_at.entry(key).or_insert_with(|| {
            addons.push(AddonErrors {
                name: e.name.clone(),
                managed: e.managed,
                kind: e.kind,
                origin: e.entry.origin,
                folders: Vec::new(),
                errors: Vec::new(),
                count: 0,
                new: 0,
                entries: 0,
                last_seen: e.evidence,
            });
            addons.len() - 1
        });
        let addon = &mut addons[a];
        if let Some(folder) = &e.folder
            && !addon.folders.iter().any(|f| f.eq_ignore_ascii_case(folder))
        {
            addon.folders.push(folder.clone());
        }
        let counter = u64::from(e.entry.counter);
        addon.count += counter;
        addon.entries += 1;
        addon.new += usize::from(e.new);
        addon.last_seen = TimeEvidence::weakest(&addon.last_seen, &e.evidence);
        match error_at.get(&(a, e.database, e.fingerprint.as_str())) {
            Some(&g) => {
                let g = &mut addon.errors[g];
                g.count += counter;
                g.entries += 1;
                g.new |= e.new;
                g.sessions.insert(e.entry.session);
                g.last_seen = TimeEvidence::weakest(&g.last_seen, &e.evidence);
                // Later entries are newer: repeats move to the end.
                g.shown = e;
            }
            None => {
                error_at.insert((a, e.database, e.fingerprint.as_str()), addon.errors.len());
                addon.errors.push(ErrorGroup {
                    shown: e,
                    count: counter,
                    sessions: BTreeSet::from([e.entry.session]),
                    new: e.new,
                    last_seen: e.evidence,
                    entries: 1,
                });
            }
        }
    }
    for addon in &mut addons {
        addon.errors.sort_by(|a, b| {
            (b.new, b.count, &a.shown.fingerprint).cmp(&(a.new, a.count, &b.shown.fingerprint))
        });
        addon.folders.sort_by_key(|f| f.to_ascii_lowercase());
    }
    addons.sort_by(|a, b| {
        (b.new, b.count).cmp(&(a.new, a.count)).then_with(|| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        })
    });
    addons
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::attribute::{Confidence, Method};
    use crate::errors::buggrabber::{TimeMeaning, TzEvidence};

    fn observed(
        database: usize,
        name: &str,
        message: &str,
        session: u32,
        counter: u32,
        new: bool,
    ) -> Observed {
        Observed {
            database,
            entry: BugEntry {
                message: message.to_owned(),
                stack: None,
                locals: None,
                session: Some(session),
                time: None,
                counter,
                entry_type: EntryType::Error,
                origin: Origin::Local,
            },
            attribution: Attribution {
                target: Attributed::Addon(name.to_owned()),
                method: Method::FirstAddonFrame,
                confidence: Confidence::High,
            },
            name: name.to_owned(),
            managed: true,
            kind: SourceKind::Addon,
            folder: Some(name.to_owned()),
            new,
            evidence: TimeEvidence {
                at: None,
                meaning: TimeMeaning::Unknown,
                tz: TzEvidence::SystemAssumed,
            },
            fingerprint: identity(message),
        }
    }

    #[test]
    fn duplicates_merge_within_a_database_and_never_across() {
        let message = "Foo-1.2\\core.lua:12: attempt to index a nil value";
        let entries = [
            // r152 writes one entry per session and duplicate.
            observed(0, "Foo", message, 5, 2, false),
            observed(0, "Foo", message, 6, 1, true),
            // The same error in another database is another error.
            observed(1, "Foo", message, 6, 1, true),
            // ≤ v12.0.2 library suffixes read alike.
            observed(
                0,
                "Foo",
                "AceAddon-3.0-13 (Foo):5: stack overflow",
                6,
                1,
                false,
            ),
            observed(
                0,
                "Foo",
                "AceAddon-3.0 (Foo):5: stack overflow",
                6,
                1,
                false,
            ),
            observed(0, "Bar", "Bar\\x.lua:1: stack overflow", 6, 9, false),
        ];
        let refs: Vec<&Observed> = entries.iter().collect();
        let groups = group(&refs);
        let names: Vec<(&str, u64, usize, usize)> = groups
            .iter()
            .map(|g| (g.name.as_str(), g.count, g.new, g.entries))
            .collect();
        assert_eq!(names, [("Foo", 6, 2, 5), ("Bar", 9, 0, 1)], "new first");
        let foo: Vec<(usize, u64, usize, bool)> = groups[0]
            .errors
            .iter()
            .map(|e| (e.shown.database, e.count, e.sessions.len(), e.new))
            .collect();
        assert_eq!(foo, [(0, 3, 2, true), (1, 1, 1, true), (0, 2, 1, false)]);
    }

    #[test]
    fn received_entries_group_apart() {
        let mut received = observed(0, "Foo", "Foo\\a.lua:1: stack overflow", 3, 1, false);
        received.entry.origin = Origin::Received;
        let local = observed(0, "Foo", "Foo\\a.lua:1: stack overflow", 3, 1, true);
        let groups = group(&[&local, &received]);
        let origins: Vec<Origin> = groups.iter().map(|g| g.origin).collect();
        assert_eq!(origins, [Origin::Local, Origin::Received]);
    }

    #[test]
    fn an_age_ahead_of_the_clock_counts_as_fresh() {
        let now: Timestamp = "2026-10-15T18:30:00Z".parse().unwrap();
        let day = Duration::from_secs(86_400);
        assert!(within(now, now, day));
        assert!(within("2026-10-20T00:00:00Z".parse().unwrap(), now, day));
        assert!(within("2026-10-14T18:30:00Z".parse().unwrap(), now, day));
        assert!(!within("2026-10-14T18:29:59Z".parse().unwrap(), now, day));
    }

    mod new {
        use super::*;

        const NOW: &str = "2026-10-15T18:30:00Z";

        fn at(s: &str) -> Timestamp {
            s.parse().unwrap()
        }

        fn db(session: u32, baseline: u32) -> DbState {
            DbState {
                session: Some(session),
                baseline,
                written_at: Some(at(NOW)),
            }
        }

        fn ack(session: u32, when: &str) -> Ack {
            Ack {
                acked_session: session,
                acked_at: at(when),
            }
        }

        fn seen(when: &str, meaning: TimeMeaning) -> TimeEvidence {
            TimeEvidence {
                at: Some(at(when)),
                meaning,
                tz: TzEvidence::Explicit,
            }
        }

        #[test]
        fn sessions_above_the_baseline_are_new_while_the_file_is_fresh() {
            let now = at(NOW);
            assert!(session_is_new(&db(3, 0), 3, None, None, now));
            assert!(!session_is_new(&db(5, 5), 5, None, None, now));
            assert!(session_is_new(&db(6, 5), 6, None, None, now));
            let stale = DbState {
                written_at: Some(at("2026-10-01T00:00:00Z")),
                ..db(6, 0)
            };
            assert!(!session_is_new(&stale, 6, None, None, now));
            let unknown = DbState {
                written_at: None,
                ..db(6, 0)
            };
            assert!(!session_is_new(&unknown, 6, None, None, now));
        }

        #[test]
        fn a_dismissal_covers_its_session_and_no_later_one() {
            let now = at(NOW);
            let a = ack(10, "2026-10-15T18:00:00Z");
            assert!(!session_is_new(&db(10, 0), 10, None, Some(&a), now));
            assert!(!session_is_new(&db(11, 0), 9, None, Some(&a), now));
            assert!(session_is_new(&db(11, 0), 11, None, Some(&a), now));
        }

        #[test]
        fn a_reset_voids_the_dismissal_until_the_session_passes_it_again() {
            let now = at(NOW);
            let a = ack(48, "2026-10-15T18:00:00Z");
            // `/bugsack reset`: 48 is above the database's session, so it means nothing.
            assert!(session_is_new(&db(1, 0), 1, None, Some(&a), now));
            // Past 48 again: session 2 is below it, but its time is after the dismissal.
            let after = seen("2026-10-15T18:10:00Z", TimeMeaning::Last);
            assert!(session_is_new(&db(50, 0), 2, Some(&after), Some(&a), now));
            // Before it, or not known to be after it: covered.
            let before = seen("2026-10-15T17:00:00Z", TimeMeaning::Last);
            assert!(!session_is_new(&db(50, 0), 2, Some(&before), Some(&a), now));
            let unsure = seen("2026-10-15T17:00:00Z", TimeMeaning::FirstOrLast);
            assert!(!session_is_new(&db(50, 0), 2, Some(&unsure), Some(&a), now));
            assert!(!session_is_new(&db(50, 0), 2, None, Some(&a), now));
        }

        #[test]
        fn a_baseline_above_the_session_is_void_too() {
            let now = at(NOW);
            assert!(session_is_new(&db(2, 30), 2, None, None, now));
        }
    }
}
