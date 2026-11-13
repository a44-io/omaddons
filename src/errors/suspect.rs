//! What preceded a newly appeared Lua error (design 7.2): the verdict a watched
//! appearance is given, and the evidence it rests on. The errors ledger
//! ([`crate::errors::ledger`]) stores each verdict with the appearance it judged. These
//! are the judge's types; the judge itself comes later.
//!
//! A verdict is evidence, not proof: the wire says `after_change`, never "caused by".

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::errors::attribute::Confidence;
use crate::errors::buggrabber::{TimeEvidence, TimeMeaning, TzEvidence};
use crate::errors::ledger::Appearance;
use crate::model::{Actor, ReleaseTransition, RunId, TxId, TxKind};

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
}
