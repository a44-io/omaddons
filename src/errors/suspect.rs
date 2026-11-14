//! What preceded a newly appeared Lua error (design 7.2): the verdict a watched
//! appearance is given, and the evidence it rests on. The errors ledger
//! ([`crate::errors::ledger`]) stores each verdict with the appearance it judged. Its
//! first half is here (RG-14): the onset, which changes are candidates, and how each
//! connects to the error; grading them comes next (RG-15).
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
use crate::limits::SUSPECT_MAX_AGE;
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
    let installs = || change.ops.iter().filter(|o| o.installs());
    let own = installs().any(|o| {
        error.folder.is_some_and(|f| same(&o.folder, f))
            || (error.managed && same(&o.addon, error.addon))
    });
    if own {
        let relation = if error.attribution == Confidence::Low {
            Relation::InStack
        } else {
            Relation::Own
        };
        return (relation, false);
    }
    if installs().any(|o| error.stack_folders.iter().any(|s| same(s, &o.folder))) {
        return (Relation::InStack, false);
    }
    let Some(erroring) = error.folder else {
        return (Relation::None, false);
    };
    let changed = |folder: &str| cx.changed_since.iter().any(|c| same(c, folder));
    let mut unknown = false;
    for o in &change.ops {
        let edge = (cx.depends)(erroring, &o.folder)
            || (o.installs() && (cx.depends)(&o.folder, erroring));
        if !edge {
            continue;
        }
        if changed(erroring) || changed(&o.folder) {
            unknown = true;
            continue;
        }
        return (Relation::Dependency, false);
    }
    (Relation::None, unknown)
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
