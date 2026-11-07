//! The privacy boundary of `errors` (M3-D21) over the BugGrabber scenarios that plant
//! names where real files hide them: strict output (default and `-v`) holds no name, no
//! GUID and no chunk text whatever the message says; raw output (`--raw-details`)
//! replaces this client's names and every player GUID and link; and an error keeps its
//! fingerprint whichever writer format stored it.

use crate::common;

use common::buggrabber::{self, Writer};
use common::privacy::assert_private;
use omaddons::errors::buggrabber::BugEntry;
use omaddons::errors::sanitize::{
    MessageKind, fingerprint, normalized, raw_details, strict_frames, strict_message,
};
use omaddons::privacy::WtfPathRedactor;
use support::entries;

#[cfg(test)]
mod support {
    use omaddons::errors::buggrabber::{BugEntry, decode};
    use omaddons::lua::parse_saved_variables;

    use super::common::buggrabber::{BugDb, Writer};

    /// `db`'s entries, rendered as `writer` writes them, parsed and decoded.
    pub fn entries(db: impl Fn(Writer) -> BugDb, writer: Writer) -> Vec<BugEntry> {
        let file = db(writer).file();
        let parsed = parse_saved_variables(&file.bytes, &["BugGrabberDB"]);
        // ubs:ignore — BugGrabber's SavedVariables, not a token
        decode(parsed.globals.get("BugGrabberDB").unwrap()).entries
    }
}

/// Everything strict output shows of `entry`, verbose.
fn strict(entry: &BugEntry) -> String {
    let message = strict_message(&entry.message);
    let mut shown = vec![
        message.text.clone().unwrap_or_default(),
        message.location.clone().unwrap_or_default(),
        message.chars.to_string(),
    ];
    shown.extend(strict_frames(
        entry.stack.as_deref().unwrap_or_default(),
        true,
    ));
    shown.join("\n")
}

/// The names this client's `WTF/` holds, as the redactor learns them.
fn names() -> WtfPathRedactor {
    WtfPathRedactor::for_paths(
        ["Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/!BugGrabber.lua"].into_iter(),
    )
}

#[test]
fn custom_names_show_only_code_derived_text() {
    for writer in Writer::ALL {
        let entries = entries(buggrabber::custom_names, writer);
        let shown: Vec<String> = entries.iter().map(strict).collect();
        for text in &shown {
            assert_private(text);
            assert!(!text.contains("print("), "no chunk text: {text}");
        }
        let kinds: Vec<MessageKind> = entries
            .iter()
            .map(|e| strict_message(&e.message).kind)
            .collect();
        assert_eq!(
            kinds,
            [
                MessageKind::Custom,
                MessageKind::LuaRuntime,
                MessageKind::LuaRuntime
            ]
        );
        let custom = strict_message(&entries[0].message);
        assert_eq!(
            (custom.text, custom.chars, custom.location.as_deref()),
            (None, 28, Some("RaidAlerts/alerts.lua:30"))
        );
        let chunk = strict_message(&entries[1].message);
        assert_eq!(chunk.location.as_deref(), Some("[custom code]:1"));
        let runtime = strict_message(&entries[2].message);
        assert_eq!(
            runtime.text.as_deref(),
            Some("attempt to index a nil value (field 'x')"),
            "the runtime message intact"
        );
        // Raw output replaces the names it knows.
        for entry in &entries {
            for legacy in [false, true] {
                let raw = raw_details(entry, legacy, &names());
                assert_private(&format!("{raw:?}"));
            }
        }
    }
}

#[test]
fn personal_data_never_shows_not_even_raw() {
    for writer in Writer::ALL {
        let entries = entries(buggrabber::personal, writer);
        for entry in &entries {
            let shown = strict(entry);
            assert_private(&shown);
            assert!(!shown.contains("Player-1234"), "{shown}");
            // As a legacy entry too: its message's later lines are its raw stack.
            let raw = format!(
                "{:?} {:?}",
                raw_details(entry, false, &names()),
                raw_details(entry, true, &names())
            );
            assert_private(&raw);
            assert!(!raw.contains("Player-1234"), "{raw}");
            assert!(raw.contains("<player>") || !entry.message.contains("Player-"));
        }
        assert_eq!(
            strict_message(&entries[0].message).kind,
            MessageKind::Custom
        );
    }
}

#[test]
fn every_scenario_keeps_its_fingerprints_in_both_formats() {
    type Scenario = fn(Writer) -> buggrabber::BugDb;
    let scenarios: [(&str, Scenario); 6] = [
        ("modern_int", buggrabber::modern_int),
        ("r152_chunked", buggrabber::r152_chunked),
        ("warnings", buggrabber::warnings),
        ("forbidden", buggrabber::forbidden),
        ("custom_names", buggrabber::custom_names),
        ("mixed_times", buggrabber::mixed_times),
    ];
    for (name, db) in scenarios {
        let prints = |writer| -> Vec<String> {
            entries(db, writer)
                .iter()
                .map(|e| {
                    let location = strict_message(&e.message).location;
                    fingerprint(&normalized(&e.message), location.as_deref())
                })
                .collect()
        };
        let (modern, legacy) = (prints(Writer::Modern), prints(Writer::Legacy));
        assert_eq!(modern, legacy, "{name}");
        let mut distinct = modern.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            modern.len(),
            "{name}: distinct errors differ"
        );
    }
}
