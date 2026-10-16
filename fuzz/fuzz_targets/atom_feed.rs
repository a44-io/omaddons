//! An Atom feed as GitHub might send it (`releases.atom`, `commits/<rev>.atom`): any
//! text parses, the same way twice, into no more entries than it has `entry` tags; each
//! entry's sha is a full one or none; and its `<content>` becomes bounded plain text, as
//! a changelog takes it.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::limits::CHANGELOG_MAX_CHARS;
use omaddons::sources::atom::parse;
use omaddons::sources::git_github::{commit_sha, is_sha};
use omaddons::text::{has_markup, html_to_text};

fuzz_target!(|data: &[u8]| {
    let xml = String::from_utf8_lossy(data);
    let entries = parse(&xml);
    assert_eq!(entries, parse(&xml));
    assert!(entries.len() <= xml.matches("entry").count());
    for e in &entries {
        assert!(commit_sha(&e.id).is_none_or(|sha| is_sha(&sha)));
        if let Some(content) = &e.content {
            let text = html_to_text(content, CHANGELOG_MAX_CHARS);
            assert!(text.chars().count() <= CHANGELOG_MAX_CHARS + 1);
            assert!(!has_markup(&text), "markup in {text:?}");
        }
    }
});
