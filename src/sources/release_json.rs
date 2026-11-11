//! The BigWigs packager's `release.json` (vision §4.5): one entry per zip, naming the
//! file, whether it is a no-lib build, and the flavors and interfaces it targets.
//!
//! Only `filename`, `nolib` and `metadata` are reliable (Questie omits `name` and
//! `version` and adds its own keys), so decoding is tolerant: unknown keys are ignored,
//! entries without a usable file name are skipped and counted, and flavor names are
//! lowercased and kept even when unknown (a future flavor simply matches nothing). The
//! metadata is a hint: a manifest can omit a flavor the zip supports, so the zip's TOCs
//! are what decides.

use serde_json::Value;

use crate::limits::SOURCE_TEXT_MAX_CHARS;
use crate::model::{AssetFlavor, FlavorSource};
use crate::text::{Entities, source_line};

/// One zip described by a `release.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub filename: String,
    pub nolib: bool,
    pub flavors: Vec<AssetFlavor>,
    pub name: Option<String>,
    pub version: Option<String>,
}

/// A decoded manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub entries: Vec<Entry>,
    /// Entries dropped for a missing or unsafe file name.
    pub skipped: usize,
}

/// Reads a manifest; `Err` (a reason) when it is not JSON, has no `releases` array, or
/// has no usable entry.
pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
    let doc: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("release.json is not JSON: {e}"))?;
    let releases = doc
        .get("releases")
        .and_then(Value::as_array)
        .ok_or("release.json has no releases array")?;
    let mut manifest = Manifest {
        entries: Vec::new(),
        skipped: 0,
    };
    for raw in releases {
        match entry(raw) {
            Some(e) => manifest.entries.push(e),
            None => manifest.skipped += 1,
        }
    }
    if manifest.entries.is_empty() {
        return Err(format!(
            "release.json lists no usable file ({} entries skipped)",
            manifest.skipped
        ));
    }
    Ok(manifest)
}

fn entry(raw: &Value) -> Option<Entry> {
    let filename = raw.get("filename")?.as_str()?.trim();
    if !safe_filename(filename) {
        return None;
    }
    // A name and a version are shown as they are (`info`, `outdated`, records), so each
    // is one safe line, as every source's text is: no control character (a terminal
    // escape sequence in a publisher's file would reach the terminal), nothing that
    // hides or reorders text, cut at the source-text cap.
    let string = |key: &str| {
        raw.get(key)
            .and_then(Value::as_str)
            .map(|s| source_line(s, SOURCE_TEXT_MAX_CHARS, Entities::Keep))
            .filter(|s| !s.is_empty())
    };
    let flavors = raw
        .get("metadata")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(flavor).collect())
        .unwrap_or_default();
    Some(Entry {
        filename: filename.to_owned(),
        nolib: raw.get("nolib").and_then(Value::as_bool).unwrap_or(false),
        flavors,
        name: string("name"),
        version: string("version"),
    })
}

fn flavor(item: &Value) -> Option<AssetFlavor> {
    let flavor = item.get("flavor")?.as_str()?.trim().to_ascii_lowercase();
    if flavor.is_empty() {
        return None;
    }
    let interface = item.get("interface").and_then(|v| match v {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    });
    Some(AssetFlavor {
        flavor,
        interface,
        source: FlavorSource::ReleaseJson,
    })
}

/// A plain file name: no path separators, control characters, or `.`/`..`.
fn safe_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn flavors(e: &Entry) -> Vec<(&str, Option<u32>)> {
        e.flavors
            .iter()
            .map(|f| (f.flavor.as_str(), f.interface))
            .collect()
    }

    #[test]
    fn the_documented_example() {
        let json = br#"{"releases":[{"name":"WeakAuras","version":"5.22.1","filename":"WeakAuras-5.22.1.zip","nolib":false,"metadata":[{"flavor":"mists","interface":50504},{"flavor":"classic","interface":11509}]}]}"#;
        let m = parse(json).unwrap();
        assert_eq!((m.entries.len(), m.skipped), (1, 0));
        let e = &m.entries[0];
        assert_eq!(
            (e.filename.as_str(), e.nolib),
            ("WeakAuras-5.22.1.zip", false)
        );
        assert_eq!(
            (e.name.as_deref(), e.version.as_deref()),
            (Some("WeakAuras"), Some("5.22.1"))
        );
        assert_eq!(
            flavors(e),
            [("mists", Some(50_504)), ("classic", Some(11_509))]
        );
        assert!(
            !flavors(e).iter().any(|(f, _)| *f == "mainline"),
            "no invented mainline"
        );
    }

    /// A name and a version are shown, so each is one safe line: a terminal escape
    /// sequence, a line break or a character that reorders text in a publisher's file
    /// never reaches output; plain text is kept as written, an entity undecoded.
    #[test]
    fn a_name_and_a_version_are_one_safe_line() {
        let json = "{\"releases\":[{\"name\":\"Foo\\u001b]0;owned\\u0007\\nBar\",\"version\":\" 1.0\\u001b[2J\\u202e &amp; \",\"filename\":\"Foo.zip\"},{\"name\":\"\\u001b\",\"version\":\"\\t\",\"filename\":\"Bar.zip\"}]}";
        let m = parse(json.as_bytes()).unwrap();
        assert_eq!(m.entries.len(), 2);
        assert_eq!(
            (
                m.entries[0].name.as_deref(),
                m.entries[0].version.as_deref()
            ),
            (Some("Foo]0;owned Bar"), Some("1.0[2J &amp;"))
        );
        for e in &m.entries {
            for text in [&e.name, &e.version].into_iter().flatten() {
                assert!(!text.chars().any(char::is_control), "{text:?}");
            }
        }
        // Nothing but control characters is no name and no version.
        assert_eq!((&m.entries[1].name, &m.entries[1].version), (&None, &None));
    }

    #[test]
    fn questie_style_and_odd_entries() {
        let json = br#"{
            "releases": [
                {"filename": "Questie-v12.1.0.zip", "nolib": false, "extra": [1, 2],
                 "metadata": [{"flavor": "CLASSIC", "interface": 11508}, {"flavor": "forever"}, {"flavor": "titan", "interface": "38002"},
                              {"interface": 1}, {"flavor": "", "interface": 2}, {"flavor": "x", "interface": -1}, "junk"]},
                {"filename": "Questie-v12.1.0-nolib.zip", "nolib": true},
                {"filename": "Weird.zip", "nolib": "yes", "metadata": "nope", "version": "  "},
                {"name": "no file"},
                {"filename": "../escape.zip"},
                {"filename": "a/b.zip"},
                {"filename": ""},
                {"filename": 7},
                "not an object"
            ],
            "questie": {"version": "12.1.0", "changelog": []}
        }"#;
        let m = parse(json).unwrap();
        assert_eq!((m.entries.len(), m.skipped), (3, 6));
        let (full, nolib, weird) = (&m.entries[0], &m.entries[1], &m.entries[2]);
        assert_eq!(
            (full.name.as_deref(), full.version.as_deref()),
            (None, None)
        );
        assert_eq!(
            flavors(full),
            [
                ("classic", Some(11_508)),
                ("forever", None),
                ("titan", Some(38_002)),
                ("x", None)
            ]
        );
        assert!(nolib.nolib && nolib.flavors.is_empty());
        assert!(!weird.nolib && weird.flavors.is_empty() && weird.version.is_none());
    }

    #[test]
    fn malformed_manifests() {
        for bad in [
            &b"{"[..],
            b"[]",
            b"{}",
            br#"{"releases": {}}"#,
            br#"{"releases": []}"#,
            br#"{"releases": [{"name": "x"}]}"#,
        ] {
            let err = parse(bad).unwrap_err();
            assert!(err.starts_with("release.json"), "{err}");
        }
    }

    proptest! {
        #[test]
        fn never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            let _ = parse(&bytes);
        }
    }
}
