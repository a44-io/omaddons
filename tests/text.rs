//! Release notes as plain text on real samples: a WeakAuras release's Atom HTML,
//! BugSack's WoWInterface changelog and description, and hand-made cases. The fixtures
//! keep the sources' bytes (CRLF included) with contributor names trimmed.

use std::path::{Path, PathBuf};

use omaddons::limits::CHANGELOG_MAX_CHARS;
use omaddons::text::{bbcode_to_text, html_to_text};
use serde_json::Value;

use support::json;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/text")
        .join(name)
}

#[cfg(test)]
mod support {
    use super::{Value, fixture};

    /// The JSON fixture `name`.
    pub fn json(name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(fixture(name)).unwrap()).unwrap()
    }
}

#[test]
fn a_weakauras_release_reads_as_text() {
    let html = std::fs::read_to_string(fixture("weakauras_release.html")).unwrap();
    let text = html_to_text(&html, CHANGELOG_MAX_CHARS);
    insta::assert_snapshot!("weakauras_release", text);
}

#[test]
fn bugsack_changelog_and_description_read_as_text() {
    let details = json("bugsack_filedetails.json");
    let changelog = details["UIChangeLog"].as_str().unwrap();
    assert!(changelog.contains("\r\n"), "the sample keeps its CRLF");
    insta::assert_snapshot!(
        "bugsack_changelog",
        bbcode_to_text(changelog, CHANGELOG_MAX_CHARS)
    );
    let description = details["UIDescription"].as_str().unwrap();
    insta::assert_snapshot!(
        "bugsack_description",
        bbcode_to_text(description, CHANGELOG_MAX_CHARS)
    );
}

#[test]
fn hand_made_cases_read_as_text() {
    let html = std::fs::read_to_string(fixture("handmade.html")).unwrap();
    insta::assert_snapshot!("handmade_html", html_to_text(&html, CHANGELOG_MAX_CHARS));
    let cases = json("handmade.json");
    let bbcode = cases["bbcode"].as_str().unwrap();
    insta::assert_snapshot!(
        "handmade_bbcode",
        bbcode_to_text(bbcode, CHANGELOG_MAX_CHARS)
    );
}

#[test]
fn a_long_changelog_is_cut_with_an_ellipsis() {
    let html = std::fs::read_to_string(fixture("weakauras_release.html")).unwrap();
    let text = html_to_text(&html, 200);
    // At most 200 characters (trailing space trimmed) and the ellipsis.
    assert!(text.chars().count() <= 201, "{text}");
    assert!(text.ends_with('…'), "{text}");
    let full = html_to_text(&html, CHANGELOG_MAX_CHARS);
    assert!(full.starts_with(text.trim_end_matches('…')));
}
