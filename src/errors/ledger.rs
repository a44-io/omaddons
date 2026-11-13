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
//! This module holds the ledger's vocabulary and identities. Nothing here reads or
//! writes a file.

use std::collections::BTreeMap;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::errors::buggrabber::TimeEvidence;
use crate::errors::sanitize::short_digest;
use crate::errors::suspect::Verdict;
use crate::model::RunId;

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
}
