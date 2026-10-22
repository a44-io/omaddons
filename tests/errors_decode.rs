//! Every BugGrabber scenario (`tests/common/buggrabber.rs`), rendered as both client
//! writers write it, read by the SavedVariables parser and decoded into the one model.

mod common;

use common::buggrabber::{self, Writer};
use common::privacy::SENDER;
use omaddons::errors::buggrabber::{BugTime, DbShape, EntryType, Origin, TimeMeaning};
use omaddons::limits::BUGGRABBER_MAX_ENTRIES;
use support::{both, meanings, one, origins, types};

#[cfg(test)]
mod support {
    use omaddons::errors::buggrabber::{BugDb, EntryType, Origin, TimeMeaning, decode, evidence};
    use omaddons::lua::parse_saved_variables;

    use super::common::buggrabber::{SvFile, Writer};

    /// The `BugGrabberDB` of `file`, decoded.
    fn decoded(file: &SvFile) -> BugDb {
        let parsed = parse_saved_variables(&file.bytes, &["BugGrabberDB"]);
        assert!(parsed.complete && parsed.issues.is_empty(), "{}", file.rel);
        // ubs:ignore — BugGrabber's SavedVariables, not a token
        decode(parsed.globals.get("BugGrabberDB").unwrap())
    }

    /// The scenario's databases in each writer format, which must decode alike.
    pub fn both(files: impl Fn(Writer) -> Vec<SvFile>) -> Vec<BugDb> {
        let modern: Vec<BugDb> = files(Writer::Modern).iter().map(decoded).collect();
        let legacy: Vec<BugDb> = files(Writer::Legacy).iter().map(decoded).collect();
        assert_eq!(modern, legacy, "the two formats decode alike");
        modern
    }

    /// [`both`] for a scenario of one database.
    pub fn one(files: impl Fn(Writer) -> Vec<SvFile>) -> BugDb {
        let mut dbs = both(files);
        assert_eq!(dbs.len(), 1);
        dbs.remove(0)
    }

    pub fn types(db: &BugDb) -> Vec<EntryType> {
        db.entries.iter().map(|e| e.entry_type).collect()
    }

    pub fn origins(db: &BugDb) -> Vec<Origin> {
        db.entries.iter().map(|e| e.origin).collect()
    }

    pub fn meanings(db: &BugDb) -> Vec<TimeMeaning> {
        db.entries
            .iter()
            .map(|e| evidence(e.time.as_ref(), &jiff::tz::TimeZone::UTC).meaning)
            .collect()
    }
}

#[test]
fn modern_integer_times_mean_the_last_occurrence() {
    let db = one(|w| vec![buggrabber::modern_int(w).file()]);
    assert_eq!(
        (db.session, db.shape, db.dropped),
        (Some(412), DbShape::Modern, 0)
    );
    let counters: Vec<u32> = db.entries.iter().map(|e| e.counter).collect();
    assert_eq!(counters, [3, 1, 57, 1]);
    assert_eq!(meanings(&db), [TimeMeaning::Last; 4]);
    assert!(db.entries.iter().all(|e| e.stack.is_some()));
    assert_eq!(
        db.entries[0].locals.as_deref(),
        Some("self = <table> {\n}\n(*temporary) = nil")
    );
    assert_eq!(
        db.entries[0].time,
        Some(BugTime::Unix(buggrabber::NOW - 2 * 86_400))
    );
    assert!(
        db.entries[3].message.starts_with("..."),
        "the truncated location"
    );
}

#[test]
fn string_times_are_first_or_last_whichever_version_wrote_them() {
    for files in [
        buggrabber::modern_string_first_seen as fn(Writer) -> buggrabber::BugDb,
        buggrabber::modern_string_last_seen,
    ] {
        let db = one(|w| vec![files(w).file()]);
        assert_eq!(meanings(&db), [TimeMeaning::FirstOrLast; 2]);
        assert!(matches!(db.entries[0].time, Some(BugTime::Local(_))));
        assert_eq!(db.entries[1].counter, 12);
    }
}

#[test]
fn r152_entries_are_joined_typed_and_split() {
    let db = one(|w| vec![buggrabber::r152_chunked(w).file()]);
    assert_eq!((db.session, db.shape), (Some(18), DbShape::Legacy));
    assert_eq!(types(&db), [EntryType::Error, EntryType::Event]);
    let error = &db.entries[0];
    let source = &buggrabber::r152_chunked(Writer::Modern).entries[0].message;
    let (message, locals) = source
        .strip_suffix("\n  ---")
        .unwrap()
        .split_once("\nLocals:|r\n")
        .unwrap();
    assert_eq!(error.message, message, "the chunks joined exactly");
    assert_eq!(error.locals.as_deref(), Some(locals));
    assert!(
        error
            .message
            .contains("\nFoo-1.2\\core.lua:12: in function `Refresh'")
    );
    assert_eq!(error.stack, None, "r152 frames stay in the message");
    assert_eq!(meanings(&db), [TimeMeaning::FirstOrLast; 2]);
}

#[test]
fn a_modern_db_with_leftover_legacy_keys_stays_modern() {
    let db = one(|w| vec![buggrabber::modern_int(w).legacy_keys().file()]);
    assert_eq!(db.shape, DbShape::Modern);
}

#[test]
fn an_embedded_db_decodes_like_its_own_file() {
    let db = one(buggrabber::embedded_bdui);
    assert_eq!((db.session, db.entries.len()), (Some(88), 1));
    assert!(
        db.entries[0]
            .message
            .contains("Interface/AddOns/bdUI/core/frames.lua")
    );
}

#[test]
fn warnings_forbidden_and_custom_names() {
    let warnings = one(|w| vec![buggrabber::warnings(w).file()]);
    assert_eq!(types(&warnings), [EntryType::Warning, EntryType::Error]);
    assert_eq!(warnings.entries[0].stack, None);
    assert!(warnings.entries[0].message.contains("|cffffd200Stack:|r"));
    let forbidden = one(|w| vec![buggrabber::forbidden(w).file()]);
    assert_eq!(types(&forbidden), [EntryType::Error]);
    assert_eq!(forbidden.entries[0].counter, 2);
    assert!(
        forbidden.entries[0]
            .stack
            .as_deref()
            .unwrap()
            .starts_with("[Interface/AddOns/!BugGrabber/")
    );
    let custom = one(|w| vec![buggrabber::custom_names(w).file()]);
    assert_eq!(origins(&custom), [Origin::Local; 3]);
}

#[test]
fn received_reports_decode_apart_without_their_sender() {
    let db = one(|w| vec![buggrabber::received(w).file()]);
    assert_eq!(
        origins(&db),
        [Origin::Local, Origin::Received, Origin::Received]
    );
    let sessions: Vec<Option<u32>> = db.entries.iter().map(|e| e.session).collect();
    assert_eq!(
        sessions,
        [Some(56), Some(57), Some(57)],
        "the receiving session"
    );
    assert!(
        !format!("{db:?}").contains(SENDER),
        "the sender is never kept"
    );

    let personal = one(|w| vec![buggrabber::personal(w).file()]);
    assert_eq!(origins(&personal), [Origin::Local, Origin::Received]);
    // Only --raw-details renders locals, through the redactor.
    assert!(
        personal.entries[0]
            .locals
            .as_deref()
            .unwrap()
            .contains("FakeChar")
    );
}

#[test]
fn two_accounts_are_two_databases() {
    let dbs = both(buggrabber::two_accounts);
    let shape: Vec<(Option<u32>, usize)> = dbs
        .iter()
        .map(|db| (db.session, db.entries.len()))
        .collect();
    assert_eq!(shape, [(Some(120), 2), (Some(7), 1)]);
}

#[test]
fn mixed_times_keep_their_own_meanings() {
    let db = one(|w| vec![buggrabber::mixed_times(w).file()]);
    assert_eq!(
        meanings(&db),
        [
            TimeMeaning::FirstOrLast,
            TimeMeaning::FirstOrLast,
            TimeMeaning::Last
        ]
    );
}

#[test]
fn a_flood_keeps_the_newest_entries_and_counts_the_rest() {
    let db = one(|w| vec![buggrabber::flood(w, BUGGRABBER_MAX_ENTRIES + 1).file()]);
    assert_eq!(db.entries.len(), BUGGRABBER_MAX_ENTRIES);
    assert_eq!(db.dropped, 1 + 2, "the oldest entry and the malformed tail");
    assert!(
        db.entries[0].message.contains("(local 'frame1')"),
        "frame0 went"
    );
    let small = one(|w| vec![buggrabber::flood(w, 10).file()]);
    assert_eq!((small.entries.len(), small.dropped), (10, 2));
}
