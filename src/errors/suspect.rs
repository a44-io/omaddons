//! What preceded a newly appeared Lua error (design 7.2): the verdict a watched
//! appearance is given, and the evidence it rests on. The errors ledger
//! ([`crate::errors::ledger`]) stores each verdict with the appearance it judged. The
//! judge is here, in two halves: the onset, which changes are candidates and how each
//! connects to the error (RG-14); then whether the change's code was running, the grade
//! and the choice among candidates, in [`judge`] (RG-15).
//!
//! A verdict is evidence, not proof: the wire says `after_change`, never "caused by".
//! The judge takes domain types only and imports nothing from `src/app/`.

use std::collections::BTreeSet;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::errors::attribute::Confidence;
use crate::errors::buggrabber::{TimeEvidence, TimeMeaning, TzEvidence};
use crate::errors::ledger::{Appearance, ChangeMark, DbGeneration};
use crate::limits::{
    SUSPECT_ALGORITHM, SUSPECT_CLEAN_SESSIONS_MAX, SUSPECT_MAX_AGE, SUSPECT_TIMING_MAX_AGE,
};
use crate::model::{Actor, ReleaseTransition, RunId, TransitionSource, TxId, TxKind};
use crate::state::history::FolderOp;

/// When a watched appearance's errors began, as far as the evidence bounds it (design
/// 7.2.2). BugGrabber keeps when an error last occurred, never when it first did, so
/// this is an interval, not a value. What omaddons looked at, what the game saved and
/// what an entry's own time proves are kept apart: doubt only ever widens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Onset {
    /// The error was absent from the file the game wrote at this moment; `None` when
    /// that write time is unknown.
    pub after: Option<Timestamp>,
    /// The error had occurred by this moment.
    pub by: Timestamp,
    /// `by` is the moment itself: one occurrence, with a trusted time.
    pub exact: bool,
}

/// The bounds on when `appearance`'s errors began, and whether the entry's own time was
/// ignored because it contradicts the reads (the judge then records [`Unknown::Time`]).
/// A pure function: it reads no clock.
///
/// - `after` is the write time of the proved read without the errors, not when omaddons
///   looked at it: errors of a session still running then are not in the file.
/// - `by` is the earliest of the read that found them, that file's write time (when not
///   later than the read), the same two times of an earlier read that met them without
///   proving their absence, and the entry's time when it is trusted.
/// - The entry's time is trusted only when it is a unix time meaning the last
///   occurrence, which is never earlier than the first, and it fits the reads: not
///   after the read that found it, not before `after`. A time that does not fit is
///   ignored and reported, never clamped. A local-time string is never trusted: the
///   zone it was read in may not be the zone that wrote it.
/// - `exact` holds for one occurrence whose trusted time is `by` itself. A single
///   occurrence timed after the file that holds it was written, or after an earlier
///   read already met it, is not exact.
pub fn onset(appearance: &Appearance) -> (Onset, bool) {
    let after = appearance.before.written_at;
    let mut by = seen_by(appearance.found.observed_at, appearance.found.written_at);
    if let Some(earliest) = &appearance.earliest {
        by = by.min(seen_by(earliest.observed_at, earliest.written_at));
    }
    let (trusted, contradicted) = match trusted_time(&appearance.time) {
        Some(at) if at > appearance.found.observed_at || after.is_some_and(|after| at < after) => {
            (None, true)
        }
        trusted => (trusted, false),
    };
    if let Some(at) = trusted {
        by = by.min(at);
    }
    let exact = appearance.counter == 1 && trusted == Some(by);
    (Onset { after, by, exact }, contradicted)
}

/// The earlier of when a read looked at the file and when the game wrote it, a write
/// time later than the look (a clock that went back) left out.
fn seen_by(observed_at: Timestamp, written_at: Option<Timestamp>) -> Timestamp {
    written_at.map_or(observed_at, |written| written.min(observed_at))
}

/// An entry's time that may bound when its error began: a unix time (`tz: explicit`)
/// that is the last occurrence.
fn trusted_time(time: &TimeEvidence) -> Option<Timestamp> {
    match time {
        TimeEvidence {
            at: Some(at),
            meaning: TimeMeaning::Last,
            tz: TzEvidence::Explicit,
        } => Some(*at),
        _ => None,
    }
}

/// The judgment of one watched appearance (one cluster of the errors ledger).
///
/// `Complete` is kept as made: the same algorithm never recomputes it. `Deferred` is
/// tried again at every full read until it completes or its database's generation
/// ends. `Unsupported` is final. A verdict whose `algorithm` is below
/// `SUSPECT_ALGORITHM_TRUSTED_MIN` is discarded when the ledger is loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Verdict {
    /// The version of the judging rules that made it (`SUSPECT_ALGORITHM`).
    pub algorithm: u32,
    pub state: JudgmentState,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub judged_at: Timestamp,
    /// `Complete`: the change that most plausibly preceded the error, or `None` when
    /// none qualifies. Always `None` otherwise.
    pub suspect: Option<Suspect>,
    /// `Deferred` and `Unsupported`: why.
    pub gap: Option<JudgmentGap>,
}

/// Where a judgment stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JudgmentState {
    /// Judged. Kept as made.
    Complete,
    /// An input was missing; the next full read tries again.
    Deferred,
    /// It can never be judged.
    Unsupported,
}

/// Why a judgment is not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JudgmentGap {
    /// The history could not be read (`Deferred`).
    HistoryUnreadable,
    /// The database's lifetime ended before the appearance was judged (`Unsupported`):
    /// its session numbers can no longer be compared.
    GenerationEnded,
    /// BugGrabber itself changed between the read without the error and the read with
    /// it (`Unsupported`): a different BugGrabber may write the same error differently.
    ProviderChanged,
}

/// The change that most plausibly preceded a newly appeared error, and how it relates to
/// the error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Suspect {
    pub tx: TxId,
    pub run: Option<RunId>,
    pub kind: TxKind,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub at: Timestamp,
    pub actor: Actor,
    pub relation: Relation,
    pub confidence: Confidence,
    /// What the change did to the addons that make the relation (for `none`: every
    /// addon it changed, at most 5), as its history line recorded it.
    pub changed: Vec<ReleaseTransition>,
    pub evidence: Evidence,
}

/// How a change connects to an error, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// The change added or replaced a folder of the erroring addon.
    Own,
    /// The change added or replaced a folder that appears in the error's stack.
    InStack,
    /// The erroring folder depends on a folder the change added, replaced or removed, or
    /// a folder the change added or replaced depends on it.
    Dependency,
    /// None of these: only the order in time connects them.
    None,
}

/// What a verdict rests on, and what could not be established.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    /// Seconds from the change to `onset.by`: the longest the error can have taken to
    /// appear.
    pub gap_s: u64,
    /// Whether `gap_s` is the real gap: one occurrence, with a trusted time.
    pub exact: bool,
    /// Whether every possible first occurrence is in the session that was running
    /// during the change; `None` when that depends on whether the client ran, which is
    /// unknown.
    pub same_session: Option<bool>,
    /// Sessions that ran the changed code before the error first occurred; `None` when
    /// they could not be counted, or the count depends on something unknown.
    pub clean_sessions: Option<u32>,
    /// The game's own version changed between the read without the error and the read
    /// with it.
    pub client_update: Option<ClientUpdated>,
    /// Other changes in the window that also qualify.
    pub other_changes: u32,
    /// What could not be established, sorted and without repeats. Each entry is a
    /// reason the grade is not higher or the remedy not exact; empty when everything
    /// was known.
    pub unknowns: Vec<Unknown>,
}

/// The game's own version at the read without an error and at the read with it, when
/// both are known and differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientUpdated {
    pub from_version: String,
    pub to_version: String,
}

/// One thing a verdict could not establish.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Unknown {
    /// No settled mark of this generation for that change.
    Mark,
    /// The session numbers are missing or do not fit together.
    Sessions,
    /// Whether the client was running at the change.
    Running,
    /// Which of several accounts was playing.
    Accounts,
    /// A trusted time would have changed the outcome and there was none: the entry's
    /// time contradicts the reads, or a single occurrence in the mixed session has no
    /// unix time.
    Time,
    /// The client's version at one of the two reads.
    ClientVersion,
    /// A dependency edge could not be asserted: a folder changed since.
    Dependencies,
    /// The history line recorded no transition, and its snapshot is gone.
    Transition,
}

// ---------------------------------------------------------------------------------------
// The judge, first half (design 7.2.1 to 7.2.3): its inputs, which changes are
// candidates, and how each connects to the error. Pure functions over domain types: the
// app layer feeds them from history and the inventory (RG-16).

impl Relation {
    /// Its strength: `own` 3, `in_stack` 2, `dependency` 1, `none` 0 (design 7.2.3).
    pub fn rank(self) -> u8 {
        match self {
            Self::Own => 3,
            Self::InStack => 2,
            Self::Dependency => 1,
            Self::None => 0,
        }
    }
}

/// A newly appeared cluster, as the full read knows it (design 7.2.1).
#[derive(Debug, Clone)]
pub struct Appeared<'a> {
    pub cluster: &'a str,
    /// From the ledger.
    pub appearance: &'a Appearance,
    /// The group the error is attributed to: the managed addon's name, else the folder.
    pub addon: &'a str,
    pub managed: bool,
    /// The folder attribution named, if any.
    pub folder: Option<&'a str>,
    pub attribution: Confidence,
    /// The AddOns folder of each stack frame, in order, without repeats.
    pub stack_folders: Vec<String>,
}

/// One earlier change on the client, from history (design 7.2.1).
#[derive(Debug, Clone)]
pub struct ChangeFact {
    pub tx: TxId,
    pub run: Option<RunId>,
    pub kind: TxKind,
    pub at: Timestamp,
    pub actor: Actor,
    /// The folders it added, replaced or removed, each with its addon.
    pub ops: Vec<ChangeOp>,
    pub transitions: Vec<ReleaseTransition>,
    /// Where `transitions` came from.
    pub recorded: TransitionSource,
}

/// One folder a change moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeOp {
    pub op: FolderOp,
    pub folder: String,
    pub addon: String,
}

impl ChangeOp {
    /// It put code in place: an `add` or a `replace` (a removed folder cannot throw).
    fn installs(&self) -> bool {
        matches!(self.op, FolderOp::Add | FolderOp::Replace)
    }
}

/// What the judge reads besides the appearance and the changes (design 7.2.1).
pub struct JudgeContext<'a> {
    pub now: Timestamp,
    /// The current generation of the database the error appeared in.
    pub generation: &'a DbGeneration,
    /// The marks of that database.
    pub marks: &'a [ChangeMark],
    /// How many databases the ledger holds for this client.
    pub databases: usize,
    /// Whether folder `a` declares folder `b` in its loaded TOC, as a required or an
    /// optional dependency, ignoring case; false for a missing folder. Read now.
    pub depends: &'a dyn Fn(&str, &str) -> bool,
    /// Folders some transaction added, replaced or removed after the appearance was
    /// found: what the inventory says of them now is not what ran then.
    pub changed_since: &'a BTreeSet<String>,
}

/// Folder and addon names compare ignoring ASCII case, as the game and the records do.
fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// How `change` connects to `error`: the strongest relation that holds (design 7.2.3),
/// and whether a `dependency` edge would have held but was not asserted because one of
/// its folders changed since (`cx.changed_since`): the TOC on disk is then not the one
/// that was loaded, and the caller records [`Unknown::Dependencies`].
///
/// - `own`: the change added or replaced the erroring folder, or (managed) a folder of
///   the erroring addon. With `Low` attribution, the addon is itself a guess: `in_stack`.
/// - `in_stack`: it added or replaced a folder of the error's stack.
/// - `dependency`: the erroring folder depends on a folder it added, replaced or removed,
///   or a folder it added or replaced depends on the erroring folder.
/// - `none` otherwise.
pub fn relation(
    error: &Appeared<'_>,
    change: &ChangeFact,
    cx: &JudgeContext<'_>,
) -> (Relation, bool) {
    let (relation, blocked, _) = connect(error, change, cx);
    (relation, blocked)
}

/// [`relation`], with the ops that make it: the addons a verdict's `changed` names.
fn connect<'c>(
    error: &Appeared<'_>,
    change: &'c ChangeFact,
    cx: &JudgeContext<'_>,
) -> (Relation, bool, Vec<&'c ChangeOp>) {
    let installs = || change.ops.iter().filter(|o| o.installs());
    let own: Vec<_> = installs()
        .filter(|o| {
            error.folder.is_some_and(|f| same(&o.folder, f))
                || (error.managed && same(&o.addon, error.addon))
        })
        .collect();
    if !own.is_empty() {
        let relation = if error.attribution == Confidence::Low {
            Relation::InStack
        } else {
            Relation::Own
        };
        return (relation, false, own);
    }
    let in_stack: Vec<_> = installs()
        .filter(|o| error.stack_folders.iter().any(|s| same(s, &o.folder)))
        .collect();
    if !in_stack.is_empty() {
        return (Relation::InStack, false, in_stack);
    }
    let Some(erroring) = error.folder else {
        return (Relation::None, false, change.ops.iter().collect());
    };
    let changed = |folder: &str| cx.changed_since.iter().any(|c| same(c, folder));
    let mut blocked = false;
    let mut edges = Vec::new();
    for o in &change.ops {
        let edge = (cx.depends)(erroring, &o.folder)
            || (o.installs() && (cx.depends)(&o.folder, erroring));
        if !edge {
            continue;
        }
        if changed(erroring) || changed(&o.folder) {
            blocked = true;
            continue;
        }
        edges.push(o);
    }
    if edges.is_empty() {
        (Relation::None, blocked, change.ops.iter().collect())
    } else {
        (Relation::Dependency, false, edges)
    }
}

/// The changes that may have preceded an error whose onset is `onset`, in their order:
/// at or before `onset.by`, and no more than [`SUSPECT_MAX_AGE`] before it (design 7.2.2).
pub fn candidates<'a>(changes: &'a [ChangeFact], onset: &Onset) -> Vec<&'a ChangeFact> {
    changes
        .iter()
        .filter(|c| {
            c.at <= onset.by && onset.by.duration_since(c.at).unsigned_abs() <= SUSPECT_MAX_AGE
        })
        .collect()
}

/// Whether BugGrabber itself changed between the read without the error and `onset.by`
/// (design 7.2.2): a change later than `onset.after` (else the earlier read's look) and
/// not later than `onset.by` added or replaced `appearance.provider`, the folder of the
/// BugGrabber that owns the database. A different BugGrabber may write the same error
/// differently, so no judgment is made across one.
pub fn provider_changed(appearance: &Appearance, changes: &[ChangeFact], onset: &Onset) -> bool {
    let lower = onset.after.unwrap_or(appearance.before.observed_at);
    changes.iter().any(|c| {
        c.at > lower
            && c.at <= onset.by
            && c.ops
                .iter()
                .any(|o| o.installs() && same(&o.folder, &appearance.provider))
    })
}

// ---------------------------------------------------------------------------------------
// The judge, second half (design 7.2.4 to 7.2.6): whether the change's code was running
// when the error began, the grade, and the choice among candidates. Principle 1 is
// enforced here: uncertainty may lose a link, it never creates one above `low`, and
// taking a fact away never raises a grade. Grades are compared only through [`grade`].

/// A grade's strength: `high` 3, `medium` 2, `low` 1. Never a derived ordering:
/// [`Confidence`] declares `High` first.
pub fn grade(c: Confidence) -> u8 {
    match c {
        Confidence::High => 3,
        Confidence::Medium => 2,
        Confidence::Low => 1,
    }
}

/// A link's strength, no link being 0.
fn strength(link: Option<Confidence>) -> u8 {
    link.map_or(0, grade)
}

/// One grade lower, never below `low` (and never to no link).
pub fn lower(c: Confidence) -> Confidence {
    match c {
        Confidence::High => Confidence::Medium,
        Confidence::Medium | Confidence::Low => Confidence::Low,
    }
}

/// The weaker of two links, no link the weakest.
fn weaker(a: Option<Confidence>, b: Option<Confidence>) -> Option<Confidence> {
    if strength(a) <= strength(b) { a } else { b }
}

/// At most `cap`.
fn capped(link: Option<Confidence>, cap: Confidence) -> Option<Confidence> {
    link.map(|c| if grade(c) > grade(cap) { cap } else { c })
}

/// The mark of a change (design 7.2.4): the newest mark of its run in the appearance's
/// generation. An unsettled newest mark is returned as it is, never passed over for an
/// older settled one; of two taken at the same moment, the unsettled one.
fn mark_of<'a>(
    marks: &'a [ChangeMark],
    run: Option<RunId>,
    generation: &DbGeneration,
) -> Option<&'a ChangeMark> {
    let run = run?;
    marks
        .iter()
        .filter(|m| m.run == run && &m.generation == generation)
        .max_by_key(|m| (m.at, !m.settled))
}

/// The first session that loads a change's code cleanly, as far as its mark tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundaries {
    /// The arithmetic cannot run: no settled mark of this generation (`Mark`), or one
    /// without a session or whose next sessions overflow (`Sessions`).
    Unknown(Unknown),
    /// `s`, the last session saved before the change, and `loaded`.
    One { s: u32, loaded: u32 },
    /// Whether this database's game was running is not known: `s + 1` if it was not,
    /// `s + 2` if it was (the mixed session `s + 1` loaded the old code).
    Both { s: u32, stopped: u32, running: u32 },
}

/// The load boundaries of a change from its mark (design 7.2.4), and what the mark could
/// not tell: `running` when neither look could say whether the client ran, `accounts`
/// when it ran or may have and the client has several databases, any of whose accounts
/// may have been the one playing. Every addition is checked: an overflow is unknown
/// evidence, never a wrapped or saturated session.
fn boundaries(mark: Option<&ChangeMark>, databases: usize) -> (Boundaries, Vec<Unknown>) {
    let Some(mark) = mark.filter(|m| m.settled) else {
        return (Boundaries::Unknown(Unknown::Mark), Vec::new());
    };
    let may_have_run = mark.running != Some(false);
    let several = databases > 1 && may_have_run;
    let mut unknowns = Vec::new();
    if mark.running.is_none() {
        unknowns.push(Unknown::Running);
    }
    if several {
        unknowns.push(Unknown::Accounts);
    }
    let Some(s) = mark.session else {
        return (Boundaries::Unknown(Unknown::Sessions), unknowns);
    };
    let (stopped, running) = (s.checked_add(1), s.checked_add(2));
    let boundaries = match (mark.running, several) {
        (Some(false), _) => stopped.map(|loaded| Boundaries::One { s, loaded }),
        (Some(true), false) => running.map(|loaded| Boundaries::One { s, loaded }),
        _ => stopped
            .zip(running)
            .map(|(stopped, running)| Boundaries::Both {
                s,
                stopped,
                running,
            }),
    };
    (
        boundaries.unwrap_or(Boundaries::Unknown(Unknown::Sessions)),
        unknowns,
    )
}

/// What the session numbers say of one change under one load boundary (design 7.2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sessions {
    /// The error last occurred in a session saved before the change.
    NotCandidate,
    /// The numbers are missing or do not fit together.
    Unknown,
    /// Every possible first occurrence is in the mixed session.
    SameSession,
    /// Sessions that ran the new code cleanly before the first occurrence: `clean` when
    /// the first occurrence is pinned, else the range it lies in.
    Clean {
        clean: Option<u32>,
        clean_min: u32,
        clean_max: u32,
    },
}

/// The session the error last occurred in, as the appearance knows it: the lowest among
/// the members when found, or an earlier read's when that is lower.
fn entry_session(appearance: &Appearance) -> Option<u32> {
    let found = appearance.entry_session?;
    let earliest = appearance.earliest.as_ref().and_then(|e| e.entry_session);
    Some(earliest.map_or(found, |earliest| earliest.min(found)))
}

/// The session arithmetic under one boundary (design 7.2.4): with `e` the session the
/// error last occurred in and `p` the last session known free of it, the first
/// occurrence lies in `max(p, s) + 1 ..= e`, pinned to `e` for a single occurrence.
///
/// An unknown `p` is unknown sessions (decided in RG-15, lowering the design's "counts
/// as `s`"): a known `p` that does not fit is unknown sessions, so a missing one cannot
/// be graded higher.
fn sessions(appearance: &Appearance, s: u32, loaded: u32) -> Sessions {
    let Some(e) = entry_session(appearance) else {
        return Sessions::Unknown;
    };
    if e <= s {
        return Sessions::NotCandidate;
    }
    let Some(p) = appearance.before.session else {
        return Sessions::Unknown;
    };
    let Some(mut lo) = p.max(s).checked_add(1) else {
        return Sessions::Unknown;
    };
    let hi = e;
    if lo > hi {
        return Sessions::Unknown;
    }
    if appearance.counter == 1 {
        lo = hi;
    }
    if hi < loaded {
        return Sessions::SameSession;
    }
    let Some(clean_max) = hi.checked_sub(loaded) else {
        return Sessions::Unknown;
    };
    // `lo < loaded <= hi`: it may have begun in the mixed session, so as few as none
    // ran clean. A count, not a session number.
    let clean_min = lo.saturating_sub(loaded);
    let clean = (lo >= loaded && lo == hi).then_some(clean_min);
    Sessions::Clean {
        clean,
        clean_min,
        clean_max,
    }
}

/// The table of design 7.2.5 for one boundary, with the `same_session` rule. `exact_after`:
/// one occurrence whose trusted time is after the change. `written_after`: the game saved
/// the file without the error after the change. A `none` link here still needs the gate
/// that [`judge`] applies (the newest candidate, close in time, the same game version).
fn cell(
    relation: Relation,
    sessions: Sessions,
    exact_after: bool,
    written_after: bool,
) -> Option<Confidence> {
    use Confidence::{High, Low, Medium};
    let max = SUSPECT_CLEAN_SESSIONS_MAX;
    let related = matches!(relation, Relation::InStack | Relation::Dependency);
    match sessions {
        Sessions::NotCandidate => None,
        Sessions::Unknown => (relation != Relation::None || !written_after).then_some(Low),
        Sessions::SameSession if exact_after => {
            let pinned = Sessions::Clean {
                clean: Some(0),
                clean_min: 0,
                clean_max: 0,
            };
            cell(relation, pinned, false, written_after).map(lower)
        }
        Sessions::SameSession => Some(Low),
        Sessions::Clean { clean: Some(n), .. } => match relation {
            Relation::Own if n == 0 => Some(High),
            Relation::Own if n <= max => Some(Medium),
            Relation::Own => Some(Low),
            _ if related && n == 0 => Some(Medium),
            _ if related && n <= max => Some(Low),
            Relation::None if n == 0 => Some(Low),
            _ => None,
        },
        Sessions::Clean {
            clean: None,
            clean_min,
            clean_max,
        } => match relation {
            Relation::Own if clean_max > max => Some(Low),
            Relation::Own => Some(Medium),
            Relation::None => (clean_min == 0).then_some(Low),
            _ if clean_max > max => None,
            _ => Some(Low),
        },
    }
}

/// The strongest a candidate's link could be were everything it could not establish
/// known: the cell's best for its relation (`dependency` for a blocked edge).
fn best_case(relation: Relation, blocked: bool) -> u8 {
    match relation {
        Relation::Own => grade(Confidence::High),
        Relation::InStack | Relation::Dependency => grade(Confidence::Medium),
        Relation::None if blocked => grade(Confidence::Medium),
        Relation::None => grade(Confidence::Low),
    }
}

/// Whether a later change took away the code a candidate put in place (decided in
/// RG-19): it can have caused the error only in a session that still had it loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replaced {
    /// No later change took it away while the error can have begun.
    No,
    /// It was gone before every session in which the error can have begun: no link.
    Before,
    /// It may have been gone when the error began: the link is `low` at most.
    Maybe,
}

/// Whether `change`'s code, in the folders that make its relation (`ops`), was taken
/// away by a later change before `appearance`'s errors can have begun: a change after
/// it, not later than `onset.by`, that added, replaced or removed every one of those
/// folders. The game loads code at a UI load, so that code ran in no session that
/// loaded after the later change.
///
/// - The game wrote the file without the error after the later change (`onset.after`
///   not earlier than it): every session that can have begun the error loaded after
///   it. `Before`.
/// - The later change's mark gives the last session that can have held the old code,
///   `loaded − 1` (with the game running, the mixed session). Before the first session
///   the error can have begun in: `Before`; at or after the last one: no effect; in
///   between: `Maybe`. Under two boundaries both must agree, else `Maybe`: not knowing
///   whether the game ran never favours the earlier change.
/// - No settled mark, or session numbers that do not tell (missing, or a first
///   possible session after the last occurrence's): `Maybe`.
///
/// A relation made by no folder (`none`) is timing only and is never replaced.
fn replaced(
    appearance: &Appearance,
    change: &ChangeFact,
    ops: &[&ChangeOp],
    changes: &[ChangeFact],
    cx: &JudgeContext<'_>,
    onset: &Onset,
) -> Replaced {
    let e = entry_session(appearance);
    let first = match (appearance.counter, appearance.before.session) {
        (1, _) => e,
        (_, Some(p)) => p.checked_add(1),
        (_, None) => None,
    };
    // The first session the error can have begun in, only from a whole window whose
    // numbers fit together: as in [`sessions`], missing or contradictory numbers tell
    // nothing, so they can never take a candidate away.
    let first = match (first, e) {
        (Some(first), Some(e)) if first <= e => Some(first),
        _ => None,
    };
    let mut replaced = Replaced::No;
    let later = changes
        .iter()
        .filter(|c| c.at > change.at && c.at <= onset.by);
    for c in later {
        let covers = !ops.is_empty()
            && ops
                .iter()
                .all(|o| c.ops.iter().any(|later| same(&later.folder, &o.folder)));
        if !covers {
            continue;
        }
        if onset.after.is_some_and(|after| after >= c.at) {
            return Replaced::Before;
        }
        let mark = mark_of(cx.marks, c.run, &appearance.generation);
        let loads = match boundaries(mark, cx.databases).0 {
            Boundaries::One { loaded, .. } => vec![loaded],
            Boundaries::Both {
                stopped, running, ..
            } => vec![stopped, running],
            Boundaries::Unknown(_) => Vec::new(),
        };
        let each: Vec<Replaced> = loads
            .iter()
            .map(|loaded| match (loaded.checked_sub(1), first, e) {
                (Some(last), Some(first), _) if last < first => Replaced::Before,
                (Some(last), _, Some(e)) if last >= e => Replaced::No,
                _ => Replaced::Maybe,
            })
            .collect();
        match each.as_slice() {
            [Replaced::Before, rest @ ..] if rest.iter().all(|r| *r == Replaced::Before) => {
                return Replaced::Before;
            }
            [Replaced::No, rest @ ..] if rest.iter().all(|r| *r == Replaced::No) => {}
            _ => replaced = Replaced::Maybe,
        }
    }
    replaced
}

/// One candidate, graded.
#[derive(Debug, Clone)]
struct Graded<'c> {
    change: &'c ChangeFact,
    relation: Relation,
    /// A `dependency` edge was not asserted (see [`relation`]).
    blocked: bool,
    addons: Vec<&'c str>,
    link: Option<Confidence>,
    same_session: Option<bool>,
    clean: Option<u32>,
    unknowns: BTreeSet<Unknown>,
    /// Its own facts are not all known, so its grade could be higher had they been: its
    /// mark, sessions, whether the game ran, which account played, or a dependency edge.
    /// What every candidate shares (the entry's time, the game's version) never counts.
    open: bool,
}

/// Whether `u` is one of a candidate's own facts that grading reads.
fn own_fact(u: Unknown) -> bool {
    matches!(
        u,
        Unknown::Mark
            | Unknown::Sessions
            | Unknown::Running
            | Unknown::Accounts
            | Unknown::Dependencies
    )
}

/// What the whole appearance leaves unknown, for every candidate.
struct Shared<'a> {
    onset: Onset,
    /// The game's version at both reads, when both are known.
    versions: Option<(&'a str, &'a str)>,
    unknowns: BTreeSet<Unknown>,
    written_at: Option<Timestamp>,
}

/// Grades one candidate under its one or two boundaries, before the `none` gate and the
/// game-update adjustment; `None` when the session numbers show it came after the error.
fn grade_candidate<'c>(
    error: &Appeared<'_>,
    change: &'c ChangeFact,
    changes: &[ChangeFact],
    cx: &JudgeContext<'_>,
    shared: &Shared<'_>,
) -> Option<Graded<'c>> {
    let appearance = error.appearance;
    let (relation, blocked, ops) = connect(error, change, cx);
    let replaced = if relation == Relation::None {
        Replaced::No
    } else {
        replaced(appearance, change, &ops, changes, cx, &shared.onset)
    };
    if replaced == Replaced::Before {
        return None;
    }
    let mark = mark_of(cx.marks, change.run, &appearance.generation);
    let (bounds, from_mark) = boundaries(mark, cx.databases);
    let mut unknowns: BTreeSet<Unknown> = from_mark.into_iter().collect();
    unknowns.extend(shared.unknowns.iter().copied());
    if blocked {
        unknowns.insert(Unknown::Dependencies);
    }
    if change.recorded == TransitionSource::Unavailable {
        unknowns.insert(Unknown::Transition);
    }
    let loads: Vec<u32> = match bounds {
        Boundaries::Unknown(why) => {
            unknowns.insert(why);
            Vec::new()
        }
        Boundaries::One { loaded, .. } => vec![loaded],
        Boundaries::Both {
            stopped, running, ..
        } => vec![stopped, running],
    };
    let s = match bounds {
        Boundaries::One { s, .. } | Boundaries::Both { s, .. } => Some(s),
        Boundaries::Unknown(_) => None,
    };
    let per: Vec<Sessions> = match s {
        Some(s) => loads.iter().map(|&l| sessions(appearance, s, l)).collect(),
        None => vec![Sessions::Unknown],
    };
    if per.contains(&Sessions::NotCandidate) {
        return None;
    }
    if per.contains(&Sessions::Unknown) && s.is_some() {
        unknowns.insert(Unknown::Sessions);
    }
    let exact_after = shared.onset.exact && shared.onset.by > change.at;
    let written_after = shared.written_at.is_some_and(|w| w > change.at);
    let mut link = per
        .iter()
        .map(|&x| cell(relation, x, exact_after, written_after))
        .reduce(weaker)
        .flatten();
    if per.len() > 1 {
        link = capped(link, Confidence::Medium);
    }
    if replaced == Replaced::Maybe {
        link = capped(link, Confidence::Low);
    }
    let all = |want: fn(&Sessions) -> bool| per.iter().all(want);
    let same_session = if all(|x| *x == Sessions::SameSession) {
        Some(true)
    } else if all(|x| matches!(x, Sessions::Clean { .. })) {
        Some(false)
    } else {
        None
    };
    let cleans: BTreeSet<Option<u32>> = per
        .iter()
        .map(|x| match x {
            Sessions::Clean { clean, .. } => *clean,
            _ => None,
        })
        .collect();
    let clean = match cleans.into_iter().collect::<Vec<_>>().as_slice() {
        [only] => *only,
        _ => None,
    };
    let mut addons: Vec<&str> = ops.iter().map(|o| o.addon.as_str()).collect();
    addons.dedup_by(|a, b| same(a, b));
    let open = unknowns.iter().any(|u| own_fact(*u));
    Some(Graded {
        change,
        relation,
        blocked,
        addons,
        link,
        same_session,
        clean,
        unknowns,
        open,
    })
}

/// Fewer clean sessions first, `None` last.
fn clean_key(clean: Option<u32>) -> (bool, u32) {
    (clean.is_none(), clean.unwrap_or(0))
}

/// What a verdict's `changed` names: the transitions of the addons that make the
/// relation (for `none`, every addon the change moved), at most 5 for `none`.
fn changed_of(graded: &Graded<'_>) -> Vec<ReleaseTransition> {
    let named = graded
        .change
        .transitions
        .iter()
        .filter(|t| graded.addons.iter().any(|a| same(a, &t.addon)));
    if graded.relation == Relation::None {
        named.take(5).cloned().collect()
    } else {
        named.cloned().collect()
    }
}

/// Judges one newly appeared cluster (design 7.2.2 to 7.2.6): no judgment when its
/// database's generation ended or BugGrabber itself changed in between (`Unsupported`);
/// otherwise the strongest link among the candidates, `None` when none qualifies.
///
/// - Each candidate is graded by [`cell`] under each load boundary its mark allows, the
///   weaker kept and capped at `medium` when there were two.
/// - A candidate whose code a later change took away before the error can have begun
///   has no link; one whose code may have been gone by then is `low` at most
///   ([`replaced`]).
/// - A `none` link needs the newest live candidate, within [`SUSPECT_TIMING_MAX_AGE`] of
///   `onset.by`, and the game's version known at both reads and the same.
/// - The game updated, or its version is not known at one read: one grade lower, never
///   below `low` (decided in RG-15: an unknown version is graded as the update it may
///   have been, so taking a version away cannot raise a grade).
/// - The choice: the higher grade, then fewer clean sessions, then the stronger
///   relation, then the newer change, then the higher transaction id.
/// - `unknowns`: the suspect's, and those of every candidate that might have outranked
///   it had they been known (its best case is at least the suspect's grade).
///
/// `Deferred` (history unreadable) is the caller's. Deterministic in the changes' order.
pub fn judge(error: &Appeared<'_>, changes: &[ChangeFact], cx: &JudgeContext<'_>) -> Verdict {
    let appearance = error.appearance;
    let verdict = |state, suspect, gap| Verdict {
        algorithm: SUSPECT_ALGORITHM,
        state,
        judged_at: cx.now,
        suspect,
        gap,
    };
    if &appearance.generation != cx.generation {
        let gap = Some(JudgmentGap::GenerationEnded);
        return verdict(JudgmentState::Unsupported, None, gap);
    }
    let (onset, contradicted) = onset(appearance);
    if provider_changed(appearance, changes, &onset) {
        let gap = Some(JudgmentGap::ProviderChanged);
        return verdict(JudgmentState::Unsupported, None, gap);
    }
    let mut shared = Shared {
        onset,
        versions: None,
        unknowns: BTreeSet::new(),
        written_at: appearance.before.written_at,
    };
    if contradicted || trusted_time(&appearance.time).is_none() {
        shared.unknowns.insert(Unknown::Time);
    }
    match (
        appearance.before.client_version.as_deref(),
        appearance.found.client_version.as_deref(),
    ) {
        (Some(from), Some(to)) => shared.versions = Some((from, to)),
        _ => {
            shared.unknowns.insert(Unknown::ClientVersion);
        }
    }
    let mut graded: Vec<Graded<'_>> = candidates(changes, &onset)
        .into_iter()
        .filter_map(|c| grade_candidate(error, c, changes, cx, &shared))
        .collect();
    graded.sort_by_key(|g| std::cmp::Reverse((g.change.at, g.change.tx)));
    let same_game = shared.versions.is_some_and(|(from, to)| from == to);
    let timing = SUSPECT_TIMING_MAX_AGE;
    for (i, g) in graded.iter_mut().enumerate() {
        if g.relation == Relation::None {
            let close = onset.by.duration_since(g.change.at).unsigned_abs() <= timing;
            if i > 0 || !close || !same_game {
                g.link = None;
            }
        } else if !same_game {
            g.link = g.link.map(lower);
        }
    }
    let mut linked: Vec<&Graded<'_>> = graded.iter().filter(|g| g.link.is_some()).collect();
    linked.sort_by(|a, b| {
        strength(b.link)
            .cmp(&strength(a.link))
            .then(clean_key(a.clean).cmp(&clean_key(b.clean)))
            .then(b.relation.rank().cmp(&a.relation.rank()))
            .then((b.change.at, b.change.tx).cmp(&(a.change.at, a.change.tx)))
    });
    let Some((best, rest)) = linked.split_first() else {
        return verdict(JudgmentState::Complete, None, None);
    };
    let Some(confidence) = best.link else {
        return verdict(JudgmentState::Complete, None, None);
    };
    // Another candidate that might have outranked it had its own facts been known (its
    // best case reaches this grade): which change came before the error is then not
    // known either, so the link is `low` at most (decided in RG-40). Taking a fact away
    // never names another change above `low`, so it never offers a way back from one.
    let rivalled = graded.iter().any(|g| {
        !std::ptr::eq(g, *best) && g.open && best_case(g.relation, g.blocked) >= grade(confidence)
    });
    let confidence = if rivalled {
        capped(Some(confidence), Confidence::Low).unwrap_or(Confidence::Low)
    } else {
        confidence
    };
    let bar = grade(confidence);
    let mut unknowns = best.unknowns.clone();
    for g in &graded {
        if !g.unknowns.is_empty() && best_case(g.relation, g.blocked) >= bar {
            unknowns.extend(g.unknowns.iter().copied());
        }
    }
    let client_update = shared
        .versions
        .filter(|(from, to)| from != to)
        .map(|(from, to)| ClientUpdated {
            from_version: from.to_owned(),
            to_version: to.to_owned(),
        });
    let change = best.change;
    let gap_s = onset.by.duration_since(change.at).unsigned_abs().as_secs();
    let suspect = Suspect {
        tx: change.tx,
        run: change.run,
        kind: change.kind,
        at: change.at,
        actor: change.actor,
        relation: best.relation,
        confidence,
        changed: changed_of(best),
        evidence: Evidence {
            gap_s,
            exact: onset.exact,
            same_session: best.same_session,
            clean_sessions: best.clean,
            client_update,
            other_changes: u32::try_from(rest.len()).unwrap_or(u32::MAX),
            unknowns: unknowns.into_iter().collect(),
        },
    };
    verdict(JudgmentState::Complete, Some(suspect), None)
}

#[cfg(test)]
mod judge_tests;

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::errors::ledger::{DbGeneration, PositiveSighting, ReadMark};

    fn t(seconds: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + seconds).unwrap()
    }

    fn time(at: Option<Timestamp>, meaning: TimeMeaning, tz: TzEvidence) -> TimeEvidence {
        TimeEvidence { at, meaning, tz }
    }

    fn unix(at: Timestamp) -> TimeEvidence {
        time(Some(at), TimeMeaning::Last, TzEvidence::Explicit)
    }

    fn mark(observed_at: Timestamp, written_at: Option<Timestamp>) -> ReadMark {
        ReadMark {
            stamp: "1:2:f:644:10:20:30".to_owned(),
            written_at,
            session: Some(3),
            observed_at,
            client_version: None,
        }
    }

    fn sighting(observed_at: Timestamp, written_at: Option<Timestamp>) -> PositiveSighting {
        PositiveSighting {
            database: "d:1".to_owned(),
            generation: DbGeneration("g:1".to_owned()),
            observed_at,
            written_at,
            entry_session: None,
            time: time(None, TimeMeaning::Unknown, TzEvidence::SystemAssumed),
        }
    }

    /// Written at 100 without the error (looked at 110), written at 290 with it (looked
    /// at 300), one occurrence with no time.
    fn appearance() -> Appearance {
        Appearance {
            database: "d:1".to_owned(),
            generation: DbGeneration("g:1".to_owned()),
            provider: "!BugGrabber".to_owned(),
            before: mark(t(110), Some(t(100))),
            found: mark(t(300), Some(t(290))),
            earliest: None,
            entry_session: Some(4),
            time: time(None, TimeMeaning::Unknown, TzEvidence::SystemAssumed),
            counter: 1,
        }
    }

    type Edit = fn(&mut Appearance);

    /// A case, the edit to [`appearance`], and the expected `after`, `by`, `exact` and
    /// whether the entry's time was ignored as contradictory.
    type Row = (&'static str, Edit, Option<Timestamp>, Timestamp, bool, bool);

    #[test]
    fn onset_bounds_when_an_error_began_from_the_reads_and_a_trusted_time() {
        use Timestamp as T;
        let rows: &[Row] = &[
            (
                "after is the previous proved write, by the write that held it",
                |_| {},
                Some(t(100)),
                t(290),
                false,
                false,
            ),
            (
                "after is unknown without the previous write time",
                |a| a.before.written_at = None,
                None,
                t(290),
                false,
                false,
            ),
            (
                "by is the read when the write time is unknown",
                |a| a.found.written_at = None,
                Some(t(100)),
                t(300),
                false,
                false,
            ),
            (
                "a write time later than the read is left out",
                |a| a.found.written_at = Some(t(350)),
                Some(t(100)),
                t(300),
                false,
                false,
            ),
            (
                "an earlier read that met it without proving absence tightens by",
                |a| a.earliest = Some(sighting(t(250), None)),
                Some(t(100)),
                t(250),
                false,
                false,
            ),
            (
                "and so does that read's write time",
                |a| a.earliest = Some(sighting(t(280), Some(t(240)))),
                Some(t(100)),
                t(240),
                false,
                false,
            ),
            (
                "a unix time tightens by, and one occurrence is exact",
                |a| a.time = unix(t(200)),
                Some(t(100)),
                t(200),
                true,
                false,
            ),
            (
                "twenty occurrences are never exact",
                |a| {
                    a.time = unix(t(200));
                    a.counter = 20;
                },
                Some(t(100)),
                t(200),
                false,
                false,
            ),
            (
                "a local-time string never tightens by, nor is exact",
                |a| a.time = time(Some(t(150)), TimeMeaning::Last, TzEvidence::SystemAssumed),
                Some(t(100)),
                t(290),
                false,
                false,
            ),
            (
                "a string that may be the first capture is never trusted either",
                |a| {
                    a.time = time(
                        Some(t(150)),
                        TimeMeaning::FirstOrLast,
                        TzEvidence::SystemAssumed,
                    );
                },
                Some(t(100)),
                t(290),
                false,
                false,
            ),
            (
                "a unix time that is not the last occurrence is not trusted",
                |a| a.time = time(Some(t(150)), TimeMeaning::Unknown, TzEvidence::Explicit),
                Some(t(100)),
                t(290),
                false,
                false,
            ),
            (
                "a unix time after the read is ignored and reported",
                |a| a.time = unix(t(310)),
                Some(t(100)),
                t(290),
                false,
                true,
            ),
            (
                "a unix time before the previous write is ignored and reported",
                |a| a.time = unix(t(90)),
                Some(t(100)),
                t(290),
                false,
                true,
            ),
            (
                "a unix time between the write and the read fits, but is not the bound",
                |a| a.time = unix(t(295)),
                Some(t(100)),
                t(290),
                false,
                false,
            ),
            (
                "a single occurrence timed after an earlier read met it is not exact",
                |a| {
                    a.earliest = Some(sighting(t(250), None));
                    a.time = unix(t(260));
                },
                Some(t(100)),
                t(250),
                false,
                false,
            ),
            (
                "without the previous write time, only the read bounds a unix time",
                |a| {
                    a.before.written_at = None;
                    a.time = unix(t(5));
                },
                None,
                t(5),
                true,
                false,
            ),
            (
                "equal timestamps",
                |a| {
                    a.before.written_at = Some(t(300));
                    a.found = mark(t(300), Some(t(300)));
                    a.time = unix(t(300));
                },
                Some(t(300)),
                t(300),
                true,
                false,
            ),
            (
                "the earliest representable times",
                |a| {
                    a.before = mark(T::MIN, Some(T::MIN));
                    a.found = mark(T::MIN, Some(T::MIN));
                    a.time = unix(T::MIN);
                },
                Some(T::MIN),
                T::MIN,
                true,
                false,
            ),
            (
                "the latest representable times",
                |a| {
                    a.before = mark(T::MAX, Some(T::MAX));
                    a.found = mark(T::MAX, Some(T::MAX));
                    a.time = unix(T::MAX);
                },
                Some(T::MAX),
                T::MAX,
                true,
                false,
            ),
            (
                "the whole range",
                |a| {
                    a.before = mark(T::MIN, Some(T::MIN));
                    a.found = mark(T::MAX, None);
                    a.time = unix(T::MAX);
                },
                Some(T::MIN),
                T::MAX,
                true,
                false,
            ),
        ];
        for (case, edit, after, by, exact, contradicted) in rows {
            let mut a = appearance();
            edit(&mut a);
            let expected = (
                Onset {
                    after: *after,
                    by: *by,
                    exact: *exact,
                },
                *contradicted,
            );
            assert_eq!(onset(&a), expected, "{case}");
        }
    }

    fn any_time() -> impl Strategy<Value = Timestamp> {
        (0i64..1_000).prop_map(t)
    }

    fn any_evidence() -> impl Strategy<Value = TimeEvidence> {
        (
            proptest::option::of(any_time()),
            prop_oneof![
                Just(TimeMeaning::Unknown),
                Just(TimeMeaning::FirstOrLast),
                Just(TimeMeaning::Last),
            ],
            prop_oneof![Just(TzEvidence::SystemAssumed), Just(TzEvidence::Explicit)],
        )
            .prop_map(|(at, meaning, tz)| TimeEvidence { at, meaning, tz })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Whatever the times: `after` is the previous write; `by` is never later than a
        /// look that held the error; `exact` is one occurrence at its own trusted time;
        /// and a local-time string changes nothing.
        #[test]
        fn onset_never_bounds_past_what_the_reads_show(
            before in (any_time(), proptest::option::of(any_time())),
            found in (any_time(), proptest::option::of(any_time())),
            earliest in proptest::option::of((any_time(), proptest::option::of(any_time()))),
            evidence in any_evidence(),
            counter in 0u32..3,
        ) {
            let mut a = appearance();
            a.before = mark(before.0, before.1);
            a.found = mark(found.0, found.1);
            a.earliest = earliest.map(|(observed, written)| sighting(observed, written));
            a.time = evidence;
            a.counter = counter;
            let (got, contradicted) = onset(&a);
            prop_assert_eq!(got.after, a.before.written_at);
            prop_assert!(got.by <= a.found.observed_at);
            if let Some(e) = &a.earliest {
                prop_assert!(got.by <= e.observed_at);
            }
            let trusted = evidence.tz == TzEvidence::Explicit && evidence.meaning == TimeMeaning::Last;
            if got.exact {
                prop_assert!(trusted && counter == 1 && evidence.at == Some(got.by));
            }
            if contradicted {
                prop_assert!(trusted && !got.exact);
            }
            if evidence.tz == TzEvidence::SystemAssumed {
                let mut without = a.clone();
                without.time.at = None;
                prop_assert_eq!(onset(&without), (got, false));
            }
        }
    }

    /// A change at `at` that moved `ops`, each `(op, folder, addon)`.
    fn change(at: Timestamp, ops: &[(FolderOp, &str, &str)]) -> ChangeFact {
        ChangeFact {
            tx: "01M9Q00000000000000000000A".parse().unwrap(),
            run: None,
            kind: TxKind::Update,
            at,
            actor: Actor::Human,
            ops: ops
                .iter()
                .map(|(op, folder, addon)| ChangeOp {
                    op: *op,
                    folder: (*folder).to_owned(),
                    addon: (*addon).to_owned(),
                })
                .collect(),
            transitions: Vec::new(),
            recorded: TransitionSource::Recorded,
        }
    }

    /// An error in `folder`, filed under `addon` (managed or not), with its stack's
    /// folders and attribution confidence.
    fn appeared<'a>(
        appearance: &'a Appearance,
        addon: &'a str,
        managed: bool,
        folder: Option<&'a str>,
        stack: &[&str],
        attribution: Confidence,
    ) -> Appeared<'a> {
        Appeared {
            cluster: "c:1",
            appearance,
            addon,
            managed,
            folder,
            attribution,
            stack_folders: stack.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn relation_is_the_strongest_connection_that_holds() {
        use FolderOp::{Add, Record, Remove, Replace};
        use Relation::{Dependency, InStack, None as Unrelated, Own};
        let base = appearance();
        // The TOCs now: Guidelime_Guide requires Guidelime; Guidelime requires LibStub.
        let depends = |a: &str, b: &str| {
            matches!(
                (
                    a.to_ascii_lowercase().as_str(),
                    b.to_ascii_lowercase().as_str()
                ),
                ("guidelime_guide", "guidelime") | ("guidelime", "libstub")
            )
        };
        let nothing = BTreeSet::new();
        let libstub_since: BTreeSet<String> = ["LibStub".to_owned()].into();
        let generation = DbGeneration("g:1".to_owned());
        let cx = |changed_since| JudgeContext {
            now: t(400),
            generation: &generation,
            marks: &[],
            databases: 1,
            depends: &depends,
            changed_since,
        };
        let error = |managed, folder, stack: &[&str], attribution| {
            appeared(&base, "Guidelime", managed, folder, stack, attribution)
        };
        let high = Confidence::High;
        // (case, error, change ops, changed since, relation, dependencies unknown)
        type Case<'a> = (
            &'a str,
            Appeared<'a>,
            Vec<(FolderOp, &'a str, &'a str)>,
            &'a BTreeSet<String>,
            Relation,
            bool,
        );
        let cases: Vec<Case<'_>> = vec![
            (
                "own: the erroring folder replaced",
                error(true, Some("Guidelime"), &[], high),
                vec![(Replace, "Guidelime", "Guidelime")],
                &nothing,
                Own,
                false,
            ),
            (
                "own: by folder, ignoring case",
                error(false, Some("Guidelime"), &[], high),
                vec![(Add, "GUIDELIME", "Other")],
                &nothing,
                Own,
                false,
            ),
            (
                "own: a folder of the managed addon",
                error(true, Some("Guidelime"), &[], high),
                vec![(Replace, "Guidelime_Data", "guidelime")],
                &nothing,
                Own,
                false,
            ),
            (
                "own: by folder, though the addon is named otherwise now",
                error(true, Some("Guidelime"), &[], high),
                vec![(Replace, "Guidelime", "Guidelime Classic")],
                &nothing,
                Own,
                false,
            ),
            (
                "an unmanaged erroring folder matches by folder only",
                error(false, Some("Guidelime"), &[], high),
                vec![(Replace, "Guidelime_Data", "Guidelime")],
                &nothing,
                Unrelated,
                false,
            ),
            (
                "own with low attribution is in_stack",
                error(true, Some("Guidelime"), &[], Confidence::Low),
                vec![(Replace, "Guidelime", "Guidelime")],
                &nothing,
                InStack,
                false,
            ),
            (
                "a removal of the erroring folder is not own",
                error(true, Some("Guidelime"), &[], high),
                vec![(Remove, "Guidelime", "Guidelime")],
                &nothing,
                Unrelated,
                false,
            ),
            (
                "in_stack: a folder of the stack replaced",
                error(
                    true,
                    Some("Guidelime"),
                    &["Guidelime_Guide", "Guidelime"],
                    high,
                ),
                vec![(Replace, "guidelime_guide", "GuidePack")],
                &nothing,
                InStack,
                false,
            ),
            (
                "a removal of a stack folder is not in_stack",
                error(true, Some("Guidelime"), &["Guidelime_Guide"], high),
                vec![(Remove, "Guidelime_Guide", "GuidePack")],
                &nothing,
                Unrelated,
                false,
            ),
            (
                "dependency: the erroring folder requires one replaced",
                error(true, Some("Guidelime"), &[], high),
                vec![(Replace, "LibStub", "LibStub")],
                &nothing,
                Dependency,
                false,
            ),
            (
                "dependency: the erroring folder requires one removed",
                error(true, Some("Guidelime"), &[], high),
                vec![(Remove, "LibStub", "LibStub")],
                &nothing,
                Dependency,
                false,
            ),
            (
                "dependency: a plugin of the erroring folder replaced",
                error(true, Some("Guidelime"), &[], high),
                vec![(Replace, "Guidelime_Guide", "GuidePack")],
                &nothing,
                Dependency,
                false,
            ),
            (
                "a plugin removed is no dependency",
                error(true, Some("Guidelime"), &[], high),
                vec![(Remove, "Guidelime_Guide", "GuidePack")],
                &nothing,
                Unrelated,
                false,
            ),
            (
                "a dependency whose folder changed since is not asserted",
                error(true, Some("Guidelime"), &[], high),
                vec![(Replace, "LibStub", "LibStub")],
                &libstub_since,
                Unrelated,
                true,
            ),
            (
                "a blocked edge falls to the next that holds",
                error(true, Some("Guidelime"), &[], high),
                vec![
                    (Replace, "LibStub", "LibStub"),
                    (Replace, "Guidelime_Guide", "GuidePack"),
                ],
                &libstub_since,
                Dependency,
                false,
            ),
            (
                "none: an unrelated folder",
                error(true, Some("Guidelime"), &["Guidelime"], high),
                vec![(Replace, "Bagnon", "Bagnon")],
                &nothing,
                Unrelated,
                false,
            ),
            (
                "none: records only",
                error(true, Some("Guidelime"), &[], high),
                vec![(Record, "Guidelime", "Guidelime")],
                &nothing,
                Unrelated,
                false,
            ),
            (
                "none: no erroring folder to depend on",
                error(false, None, &[], high),
                vec![(Replace, "LibStub", "LibStub")],
                &nothing,
                Unrelated,
                false,
            ),
        ];
        for (case, error, ops, changed_since, want, unknown) in cases {
            let got = relation(&error, &change(t(200), &ops), &cx(changed_since));
            assert_eq!(got, (want, unknown), "{case}");
        }
        assert_eq!(
            [Own, InStack, Dependency, Unrelated].map(Relation::rank),
            [3, 2, 1, 0]
        );
    }

    #[test]
    fn candidates_are_the_changes_within_the_window_before_the_onset() {
        use jiff::SignedDuration;
        let by = t(1_000_000);
        let onset = Onset {
            after: None,
            by,
            exact: false,
        };
        let max = SignedDuration::try_from(SUSPECT_MAX_AGE).unwrap();
        let second = SignedDuration::from_secs(1);
        let cases = [
            ("at the onset itself", by, true),
            ("a second after it", by + second, false),
            ("at the window's far edge", by - max, true),
            ("a second past it", by - max - second, false),
            ("within it", by - SignedDuration::from_hours(3), true),
        ];
        for (case, at, kept) in cases {
            let changes = [change(at, &[(FolderOp::Replace, "Foo", "Foo")])];
            assert_eq!(
                candidates(&changes, &onset).len(),
                usize::from(kept),
                "{case}"
            );
        }
    }

    #[test]
    fn no_judgment_across_a_change_to_buggrabber_itself() {
        // Written at 100 without the error, at 290 with it.
        let base = appearance();
        let (bounds, _) = onset(&base);
        let bug =
            |at: i64, folder: &str| change(t(at), &[(FolderOp::Replace, folder, "BugGrabber")]);
        let cases = [
            (
                "BugGrabber replaced between the two writes",
                vec![bug(200, "!BugGrabber")],
                true,
            ),
            (
                "and named in another case",
                vec![bug(200, "!buggrabber")],
                true,
            ),
            (
                "replaced before the write without the error",
                vec![bug(90, "!BugGrabber")],
                false,
            ),
            (
                "at that write: not after it",
                vec![bug(100, "!BugGrabber")],
                false,
            ),
            (
                "after the error had occurred",
                vec![bug(295, "!BugGrabber")],
                false,
            ),
            (
                "another addon replaced in between",
                vec![bug(200, "Foo")],
                false,
            ),
        ];
        for (case, changes, want) in cases {
            assert_eq!(provider_changed(&base, &changes, &bounds), want, "{case}");
        }
        // A pack that embeds BugGrabber owns its database: a change to the pack counts.
        let embedded = Appearance {
            provider: "bdUI".to_owned(),
            ..appearance()
        };
        assert!(provider_changed(&embedded, &[bug(200, "bdUI")], &bounds));
        assert!(!provider_changed(
            &embedded,
            &[bug(200, "!BugGrabber")],
            &bounds
        ));
        // With the earlier write's time unknown, the lower edge is the earlier look.
        let unwritten = Appearance {
            before: mark(t(110), None),
            ..appearance()
        };
        let (open, _) = onset(&unwritten);
        assert!(!provider_changed(
            &unwritten,
            &[bug(105, "!BugGrabber")],
            &open
        ));
        assert!(provider_changed(
            &unwritten,
            &[bug(115, "!BugGrabber")],
            &open
        ));
    }
}
