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
use crate::model::{Actor, ReleaseTransition, RunId, TxId, TxKind};

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
