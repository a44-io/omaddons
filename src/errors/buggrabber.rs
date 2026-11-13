//! `BugGrabberDB`, decoded: one typed model for every BugGrabber era and both writer
//! formats, though the data has no version field and its fields changed meaning.
//!
//! - `time` is a unix integer of the last occurrence (v12.0.11+), or a local-time
//!   string of the first capture (≤ v12.0.2, r152) or of the last occurrence
//!   (v12.0.3–v12.0.9), written in the zone of whatever machine wrote it. A string
//!   cannot say which, so it is only ever a lower bound on the last occurrence
//!   ([`TimeMeaning::FirstOrLast`]).
//! - r152 (3.3.5a) splits a long `message` into chunks, has a `type`, and embeds the
//!   stack (the message's lines after the first) and the locals (after
//!   `"\nLocals:|r\n"`) in the message, which ends `"\n  ---"`.
//! - BugSack stores other players' errors in the same array with a `source` key and the
//!   receiving session: they decode as [`Origin::Received`], and the sender is never
//!   kept (M3-D21).
//!
//! Decoding never fails: unknown keys are ignored, and what cannot be read is skipped
//! and counted in [`BugDb::dropped`].

use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::limits::BUGGRABBER_MAX_ENTRIES;
use crate::lua::{Table, Value};

/// One BugGrabber database: one file of one account, or one addon's file that embeds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BugDb {
    /// The current session (+1 per UI load).
    pub session: Option<u32>,
    pub shape: DbShape,
    /// Oldest to newest, at most [`BUGGRABBER_MAX_ENTRIES`].
    pub entries: Vec<BugEntry>,
    /// Entries skipped: malformed, or the oldest past the cap.
    pub dropped: usize,
    /// The value is a database: a table whose `errors` is a table (an empty one is a
    /// database with no errors). A missing global, a value of another type, or an
    /// `errors` that is missing or not a table decodes as empty and is not recognized:
    /// "empty" and "not a database" are never the same thing (regression guard, 7.1.3).
    pub recognized: bool,
}

/// Which BugGrabber wrote a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DbShape {
    Modern,
    /// r152 (3.3.5a): legacy keys, typed and chunked entries.
    Legacy,
}

/// One error as BugGrabber stored it (repeats bump its counter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BugEntry {
    /// Lossy UTF-8; raw control bytes kept (the renderers strip them).
    pub message: String,
    pub stack: Option<String>,
    pub locals: Option<String>,
    pub session: Option<u32>,
    pub time: Option<BugTime>,
    /// At least 1.
    pub counter: u32,
    pub entry_type: EntryType,
    pub origin: Origin,
}

/// What kind of error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    Error,
    /// r152's `event` (a blocked or forbidden action).
    Event,
    /// A `LUA_WARNING`.
    Warning,
}

/// Where an error happened. A received one's sender is never kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Local,
    /// Sent by another player through BugSack.
    Received,
}

/// An entry's `time`, as written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BugTime {
    /// A unix integer: the last occurrence.
    Unix(i64),
    /// A local-time string: the first capture or the last occurrence, in the zone of
    /// the machine that wrote it.
    Local(DateTime),
}

/// What a time says about the last occurrence, weakest first.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TimeMeaning {
    Unknown,
    /// A string time: the first capture or the last occurrence, so only a lower bound
    /// on the last occurrence.
    FirstOrLast,
    /// The last occurrence.
    Last,
}

/// How sure the instant's zone is, weakest first.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TzEvidence {
    /// A local string read in this machine's zone, which may not be the zone that wrote
    /// it (an imported `WTF/`).
    SystemAssumed,
    /// A unix time.
    Explicit,
}

/// An instant with what it means and how sure its zone is: the wire's `last_seen`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimeEvidence {
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub at: Option<Timestamp>,
    pub meaning: TimeMeaning,
    pub tz: TzEvidence,
}

/// Yes, no, or cannot tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tri {
    Yes,
    No,
    Uncertain,
}

impl TimeEvidence {
    /// What two times prove together: the weaker meaning and zone evidence, and the
    /// later instant (or whichever exists). It can only understate what is known.
    pub fn weakest(a: &Self, b: &Self) -> Self {
        Self {
            at: a.at.max(b.at),
            meaning: a.meaning.min(b.meaning),
            tz: a.tz.min(b.tz),
        }
    }

    /// Whether the entry occurred at or after `cutoff`: yes when its instant is not
    /// before it (even a first capture after the cutoff proves an occurrence after it),
    /// no only for a last occurrence before it, and uncertain otherwise.
    pub fn at_or_after(&self, cutoff: Timestamp) -> Tri {
        match self.at {
            Some(at) if at >= cutoff => Tri::Yes,
            // ubs:ignore — comparing what a time means, not a secret
            Some(_) if self.meaning == TimeMeaning::Last => Tri::No,
            _ => Tri::Uncertain,
        }
    }
}

/// The evidence an entry's `time` gives, a local string read in `tz` (the app passes
/// [`TimeZone::system`]). A string a DST change made ambiguous or skipped takes the
/// earlier instant.
pub fn evidence(time: Option<&BugTime>, tz: &TimeZone) -> TimeEvidence {
    let unknown = TimeEvidence {
        at: None,
        meaning: TimeMeaning::Unknown,
        tz: TzEvidence::SystemAssumed,
    };
    match time {
        Some(BugTime::Unix(at)) => match Timestamp::from_second(*at) {
            Ok(at) => TimeEvidence {
                at: Some(at),
                meaning: TimeMeaning::Last,
                tz: TzEvidence::Explicit,
            },
            Err(_) => unknown,
        },
        Some(BugTime::Local(civil)) => TimeEvidence {
            at: tz.to_ambiguous_timestamp(*civil).earlier().ok(),
            meaning: TimeMeaning::FirstOrLast,
            tz: TzEvidence::SystemAssumed,
        },
        None => unknown,
    }
}

/// The `BugGrabberDB` global's value, decoded. Anything that is not a table decodes as
/// an empty database.
// ubs:ignore — BugGrabber's SavedVariables, not a token
pub fn decode(value: &Value) -> BugDb {
    let mut db = BugDb {
        session: None,
        shape: DbShape::Modern,
        entries: Vec::new(),
        dropped: 0,
        recognized: false,
    };
    let Value::Table(t) = value else {
        return db;
    };
    db.session = t.get("session").and_then(as_u32);
    let Some(Value::Table(errors)) = t.get("errors") else {
        return db;
    };
    db.recognized = true;
    // Newest first, up to the cap: the oldest go first (repeats move to the end).
    let all: Vec<&Value> = errors.seq().collect();
    let mut newest_first = Vec::new();
    let mut seen = 0;
    let mut legacy_entries = false;
    for value in all.iter().rev() {
        if newest_first.len() == BUGGRABBER_MAX_ENTRIES {
            break;
        }
        seen += 1;
        match entry(value) {
            Some((entry, legacy)) => {
                legacy_entries |= legacy;
                newest_first.push(entry);
            }
            None => db.dropped += 1,
        }
    }
    db.dropped += all.len() - seen;
    newest_first.reverse();
    db.entries = newest_first;
    let legacy_keys = ["limit", "save", "throttle"]
        .iter()
        .any(|k| t.get(k).is_some());
    if legacy_keys && legacy_entries {
        db.shape = DbShape::Legacy;
    }
    db
}

/// One entry, and whether it has r152's shape (a `type`, or a chunked message); `None`
/// when it is not an entry.
fn entry(value: &Value) -> Option<(BugEntry, bool)> {
    let Value::Table(t) = value else {
        return None;
    };
    let (message, chunked) = match t.get("message")? {
        Value::Str(bytes) => (String::from_utf8_lossy(bytes).into_owned(), false),
        Value::Table(chunks) => (joined(chunks)?, true),
        _ => return None,
    };
    let kind = t.get("type").and_then(Value::as_str_lossy);
    let r152 = kind.is_some() || chunked;
    let (message, stack, locals) = if r152 {
        // The stack stays in the message (its lines after the first); the locals and
        // the closing line come off.
        let message = message.strip_suffix("\n  ---").unwrap_or(&message);
        match message.split_once("\nLocals:|r\n") {
            Some((message, locals)) => (message.to_owned(), None, Some(locals.to_owned())),
            None => (message.to_owned(), None, None),
        }
    } else {
        let text = |key| {
            t.get(key)
                .and_then(Value::as_str_lossy)
                .map(|s| s.into_owned())
        };
        (message, text("stack"), text("locals"))
    };
    let entry_type = match kind.as_deref() {
        Some("event") => EntryType::Event,
        Some(_) => EntryType::Error,
        None if message.starts_with("LUA_WARNING: ") => EntryType::Warning,
        None => EntryType::Error,
    };
    let entry = BugEntry {
        message,
        stack,
        locals,
        session: t.get("session").and_then(as_u32),
        time: t.get("time").and_then(time),
        counter: t.get("counter").and_then(as_u32).unwrap_or(1).max(1),
        entry_type,
        origin: if t.get("source").is_some() {
            Origin::Received
        } else {
            Origin::Local
        },
    };
    Some((entry, r152))
}

/// r152's message chunks, in order; `None` when one is not a string.
fn joined(chunks: &Table) -> Option<String> {
    let mut bytes = Vec::new();
    for chunk in chunks.seq() {
        let Value::Str(chunk) = chunk else {
            return None;
        };
        bytes.extend_from_slice(chunk);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// A `time`: an integer is unix time; a string must read exactly
/// `YYYY/MM/DD HH:MM:SS`.
fn time(value: &Value) -> Option<BugTime> {
    match value {
        Value::Int(at) => Some(BugTime::Unix(*at)),
        Value::Str(bytes) => {
            let text = std::str::from_utf8(bytes).ok()?;
            let civil = DateTime::strptime("%Y/%m/%d %H:%M:%S", text).ok()?;
            Some(BugTime::Local(civil))
        }
        _ => None,
    }
}

fn as_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Int(n) => u32::try_from(*n).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::lua::Key;

    /// [`decode`], called from one place the bug scanner can be told about.
    fn read(value: &Value) -> BugDb {
        // ubs:ignore — BugGrabber's SavedVariables, not a token
        decode(value)
    }

    fn s(text: &str) -> Value {
        Value::Str(text.as_bytes().to_vec())
    }

    /// A table of `(key, value)` fields.
    fn t(fields: Vec<(&str, Value)>) -> Value {
        Value::Table(Table {
            array: Vec::new(),
            hash: fields
                .into_iter()
                .map(|(k, v)| (Key::Str(k.as_bytes().to_vec()), v))
                .collect(),
        })
    }

    fn list(values: Vec<Value>) -> Value {
        Value::Table(Table {
            array: values,
            hash: Vec::new(),
        })
    }

    /// A database of `entries`, with the legacy keys when `legacy`.
    fn db(entries: Vec<Value>, legacy: bool) -> Value {
        let mut fields = vec![("session", Value::Int(9)), ("errors", list(entries))];
        if legacy {
            fields.push(("limit", Value::Int(50)));
            fields.push(("save", Value::Bool(true)));
        }
        t(fields)
    }

    /// "Empty" and "not a database" are never the same thing: only a table whose
    /// `errors` is a table is recognized, an empty one included.
    #[test]
    fn only_a_table_with_an_errors_table_is_a_recognized_database() {
        // (case, the global's value, recognized)
        let cases = [
            ("no global (nil)", Value::Nil, false),
            ("a string", s("errors"), false),
            (
                "a table without errors",
                t(vec![("session", Value::Int(9))]),
                false,
            ),
            (
                "errors that is a number",
                t(vec![("session", Value::Int(9)), ("errors", Value::Int(5))]),
                false,
            ),
            ("errors that is an empty table", db(Vec::new(), false), true),
            (
                "errors with a malformed entry",
                db(vec![s("not an entry")], false),
                true,
            ),
        ];
        for (case, value, recognized) in cases {
            let decoded = read(&value);
            assert_eq!(decoded.recognized, recognized, "{case}");
            assert!(decoded.entries.is_empty(), "{case}");
        }
        assert_eq!(
            read(&db(vec![s("not an entry")], false)).dropped,
            1,
            "recognized, and the skipped entry counted"
        );
    }

    #[test]
    fn r152_chunks_join_exactly_and_lose_their_locals_and_tail() {
        let body = format!(
            "{}\nLocals:|r\nself = <table>\n  ---",
            "a".repeat(1_960 - 2)
        );
        let chunks: Vec<Value> = body
            .as_bytes()
            .chunks(980)
            .map(|c| Value::Str(c.to_vec()))
            .collect();
        assert_eq!(chunks.len(), 3, "a boundary at 980 bytes");
        let full: Vec<Value> = (0..7).map(|_| Value::Str(vec![b'x'; 980])).collect();
        let decoded = read(&db(
            vec![
                t(vec![
                    ("message", list(chunks)),
                    ("type", s("error")),
                    ("time", s("2026/10/15 18:30:00")),
                ]),
                t(vec![("message", list(full)), ("type", s("error"))]),
                t(vec![
                    (
                        "message",
                        s("[ADDON_ACTION_BLOCKED] x\nFoo-1.2\\a.lua:5:\nLocals:|r\n\n  ---"),
                    ),
                    ("type", s("event")),
                ]),
            ],
            true,
        ));
        assert_eq!(decoded.shape, DbShape::Legacy);
        let [first, truncated, event] = &decoded.entries[..] else {
            unreachable!("{decoded:?}");
        };
        assert_eq!(first.message, "a".repeat(1_958));
        assert_eq!(first.locals.as_deref(), Some("self = <table>"));
        assert_eq!(
            (first.stack.as_ref(), first.entry_type),
            (None, EntryType::Error)
        );
        assert_eq!(
            truncated.message.len(),
            7 * 980,
            "a full message stays as joined"
        );
        assert_eq!(event.entry_type, EntryType::Event);
        assert_eq!(event.message, "[ADDON_ACTION_BLOCKED] x\nFoo-1.2\\a.lua:5:");
        assert_eq!(event.locals.as_deref(), Some(""));
    }

    #[test]
    fn modern_entries_keep_their_stack_and_locals_and_type_warnings() {
        let decoded = read(&db(
            vec![
                t(vec![
                    ("message", s("x.lua:1: boom")),
                    ("stack", s("frame")),
                    ("locals", s("self = nil")),
                    ("counter", Value::Int(0)),
                    ("session", Value::Int(8)),
                    ("future_key", Value::Bool(true)),
                ]),
                t(vec![("message", s("LUA_WARNING: x.xml:5: odd"))]),
            ],
            true,
        ));
        assert_eq!(decoded.shape, DbShape::Modern, "leftover legacy keys alone");
        assert_eq!(decoded.session, Some(9));
        let [error, warning] = &decoded.entries[..] else {
            unreachable!("{decoded:?}");
        };
        assert_eq!(
            (error.stack.as_deref(), error.locals.as_deref()),
            (Some("frame"), Some("self = nil"))
        );
        assert_eq!((error.counter, error.session), (1, Some(8)));
        assert_eq!(warning.entry_type, EntryType::Warning);
        assert_eq!((warning.counter, warning.time), (1, None));
    }

    #[test]
    fn a_source_makes_an_entry_received_and_is_never_kept() {
        let decoded = read(&db(
            vec![
                t(vec![("message", s("a")), ("source", s("SomePlayer"))]),
                t(vec![("message", s("b")), ("source", Value::Bool(false))]),
                t(vec![("message", s("c"))]),
            ],
            false,
        ));
        let origins: Vec<Origin> = decoded.entries.iter().map(|e| e.origin).collect();
        assert_eq!(origins, [Origin::Received, Origin::Received, Origin::Local]);
        assert!(!format!("{decoded:?}").contains("SomePlayer"));
    }

    #[test]
    fn what_is_not_an_entry_is_skipped_and_counted() {
        let decoded = read(&db(
            vec![
                s("not an entry"),
                t(vec![("message", Value::Int(42))]),
                t(vec![("stack", s("no message"))]),
                t(vec![("message", list(vec![s("a"), Value::Int(1)]))]),
                t(vec![
                    ("message", s("ok")),
                    ("time", s("2026-10-15 18:30:00")),
                ]),
                t(vec![("message", s("ok")), ("time", Value::Bool(true))]),
            ],
            false,
        ));
        assert_eq!((decoded.entries.len(), decoded.dropped), (2, 4));
        assert!(
            decoded.entries.iter().all(|e| e.time.is_none()),
            "strict times"
        );
        for not_a_db in [s("x"), t(vec![("errors", s("x"))]), t(vec![])] {
            let empty = read(&not_a_db);
            assert!(empty.entries.is_empty() && empty.dropped == 0);
        }
    }

    #[test]
    fn the_newest_entries_are_kept_up_to_the_cap() {
        let entries: Vec<Value> = (0..=BUGGRABBER_MAX_ENTRIES)
            .map(|i| t(vec![("message", s(&format!("e{i}")))]))
            .chain([s("tail")])
            .collect();
        let decoded = read(&db(entries, false));
        assert_eq!(decoded.entries.len(), BUGGRABBER_MAX_ENTRIES);
        assert_eq!(decoded.dropped, 2, "the oldest one and the tail");
        assert_eq!(decoded.entries[0].message, "e1");
    }

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn ev(meaning: TimeMeaning, when: Option<&str>) -> TimeEvidence {
        TimeEvidence {
            at: when.map(at),
            meaning,
            tz: TzEvidence::Explicit,
        }
    }

    #[test]
    fn at_or_after_proves_only_what_the_meaning_allows() {
        let cutoff = at("2026-10-15T12:00:00Z");
        let (before, same, after) = (
            Some("2026-10-15T11:00:00Z"),
            Some("2026-10-15T12:00:00Z"),
            Some("2026-10-15T13:00:00Z"),
        );
        use TimeMeaning::*;
        use Tri::*;
        for (meaning, when, expected) in [
            (Last, before, No),
            (Last, same, Yes),
            (Last, after, Yes),
            (FirstOrLast, before, Uncertain),
            (FirstOrLast, same, Yes),
            (FirstOrLast, after, Yes),
            (Unknown, before, Uncertain),
            (Unknown, same, Yes),
            (Unknown, after, Yes),
            (Unknown, None, Uncertain),
            (Last, None, Uncertain),
        ] {
            assert_eq!(
                ev(meaning, when).at_or_after(cutoff),
                expected,
                "{meaning:?} {when:?}"
            );
        }
    }

    #[test]
    fn weakest_keeps_the_weaker_meaning_and_zone_and_the_later_instant() {
        use TimeMeaning::*;
        let meanings = [Unknown, FirstOrLast, Last];
        let zones = [TzEvidence::SystemAssumed, TzEvidence::Explicit];
        for a_meaning in meanings {
            for b_meaning in meanings {
                for a_tz in zones {
                    for b_tz in zones {
                        let a = TimeEvidence {
                            at: Some(at("2026-10-15T10:00:00Z")),
                            meaning: a_meaning,
                            tz: a_tz,
                        };
                        let b = TimeEvidence {
                            at: None,
                            meaning: b_meaning,
                            tz: b_tz,
                        };
                        let w = TimeEvidence::weakest(&a, &b);
                        assert_eq!(w.meaning, a_meaning.min(b_meaning));
                        assert_eq!(w.tz, a_tz.min(b_tz));
                        assert_eq!(w.at, a.at, "whichever exists");
                        assert_eq!(TimeEvidence::weakest(&b, &a), w, "symmetric");
                    }
                }
            }
        }
        let later = TimeEvidence::weakest(
            &ev(Last, Some("2026-10-15T10:00:00Z")),
            &ev(Last, Some("2026-10-15T11:00:00Z")),
        );
        assert_eq!(later.at, Some(at("2026-10-15T11:00:00Z")));
    }

    #[test]
    fn a_local_string_is_read_in_the_zone_given_and_says_so() {
        let tokyo = TimeZone::get("Asia/Tokyo").unwrap();
        let local = time(&s("2026/10/15 18:30:00")).unwrap();
        let read = evidence(Some(&local), &tokyo);
        assert_eq!(read.at, Some(at("2026-10-15T09:30:00Z")));
        assert_eq!(
            (read.meaning, read.tz),
            (TimeMeaning::FirstOrLast, TzEvidence::SystemAssumed)
        );
        let unix = evidence(Some(&BugTime::Unix(1_792_089_000)), &tokyo);
        assert_eq!(
            unix.at,
            Some(at("2026-10-15T18:30:00Z")),
            "a unix time ignores the zone"
        );
        assert_eq!(
            (unix.meaning, unix.tz),
            (TimeMeaning::Last, TzEvidence::Explicit)
        );
        let none = evidence(None, &tokyo);
        assert_eq!((none.at, none.meaning), (None, TimeMeaning::Unknown));
    }

    #[test]
    fn a_dst_change_takes_the_earlier_instant() {
        let berlin = TimeZone::get("Europe/Berlin").unwrap();
        // 02:30 happens twice on 2026-10-25 (CEST, then CET).
        let twice = time(&s("2026/10/25 02:30:00")).unwrap();
        assert_eq!(
            evidence(Some(&twice), &berlin).at,
            Some(at("2026-10-25T00:30:00Z"))
        );
        // 02:30 never happens on 2026-03-29: of its two readings (+01:00 and +02:00),
        // the earlier instant.
        let never = time(&s("2026/03/29 02:30:00")).unwrap();
        assert_eq!(
            evidence(Some(&never), &berlin).at,
            Some(at("2026-03-29T00:30:00Z"))
        );
    }

    fn lua_value() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Nil),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::Int),
            any::<f64>().prop_map(Value::Num),
            prop_oneof![
                Just("message"),
                Just("errors"),
                Just("time"),
                Just("2026/10/15 18:30:00"),
                Just("\nLocals:|r\n"),
                Just("LUA_WARNING: x"),
                Just("event"),
            ]
            .prop_map(s),
            proptest::collection::vec(any::<u8>(), 0..8).prop_map(Value::Str),
        ];
        leaf.prop_recursive(4, 48, 6, |inner| {
            let key = prop_oneof![
                prop_oneof![
                    Just("message"),
                    Just("errors"),
                    Just("time"),
                    Just("type"),
                    Just("source"),
                    Just("session"),
                    Just("counter"),
                    Just("limit")
                ]
                .prop_map(|k| Key::Str(k.as_bytes().to_vec())),
                (-2_i64..4).prop_map(Key::Int),
            ];
            (
                proptest::collection::vec(inner.clone(), 0..5),
                proptest::collection::vec((key, inner), 0..5),
            )
                .prop_map(|(array, hash)| Value::Table(Table { array, hash }))
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Decoding never fails, whatever the tree.
        #[test]
        fn decode_never_panics(value in lua_value()) {
            let decoded = read(&value);
            prop_assert!(decoded.entries.len() <= BUGGRABBER_MAX_ENTRIES);
            for entry in &decoded.entries {
                prop_assert!(entry.counter >= 1);
                let _ = evidence(entry.time.as_ref(), &TimeZone::UTC);
            }
        }
    }
}
