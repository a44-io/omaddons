//! Every JSON envelope snapshot in the repository, validated against the published schemas
//! (M4-D9). Snapshots are redacted, so each whole-value placeholder the snapshot helpers
//! write is first replaced by a typed sample that its schema admits.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::validate::validate_envelope;

/// A placeholder's stand-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sample {
    /// 26 Crockford zeros: it matches the ULID pattern ids carry.
    Ulid,
    /// An RFC 3339 instant.
    Time,
    /// The integer 0.
    Zero,
    /// The envelope's `meta`, which `support::stable` and older helpers strip.
    Meta,
    /// Kept as written: its schema is a plain string.
    Keep,
}

/// Every whole-value placeholder the snapshot helpers write. A new redaction adds its row.
const PLACEHOLDERS: &[(&str, Sample)] = &[
    ("<binary>", Sample::Keep),
    ("<bytes>", Sample::Zero),
    ("<checked_at>", Sample::Time),
    ("<expires>", Sample::Time),
    ("<key>", Sample::Ulid),
    ("<meta>", Sample::Meta),
    ("<plan>", Sample::Ulid),
    ("<precondition>", Sample::Keep),
    ("<run>", Sample::Ulid),
    ("<time>", Sample::Time),
    ("<tx>", Sample::Ulid),
    ("<ulid>", Sample::Ulid),
    ("<version>", Sample::Keep),
];

/// Renderer fixtures whose `data` is a synthetic struct (it proves declaration-order
/// rendering): only their frame, error and warnings are validated.
const FRAME_ONLY: &[&str] = &[
    "src/output/snapshots/omaddons__output__tests__envelope_check_failed.snap",
    "src/output/snapshots/omaddons__output__tests__envelope_partial_success.snap",
    "src/output/snapshots/omaddons__output__tests__envelope_success.snap",
];

/// The row name of a whole-value placeholder: `<name>`, or `<name:qualifier>` where a
/// helper keeps the client a redacted value belongs to (`<key:retail>`, `<tx:era>`).
fn placeholder_name(text: &str) -> Option<String> {
    let inner = text.strip_prefix('<')?.strip_suffix('>')?;
    let (name, qualifier) = match inner.split_once(':') {
        Some((name, qualifier)) => (name, Some(qualifier)),
        None => (inner, None),
    };
    let name_ok = !name.is_empty() && name.bytes().all(|c| c.is_ascii_lowercase() || c == b'_');
    let qualifier_ok = qualifier.is_none_or(|q| {
        !q.is_empty()
            && q.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._@-".contains(&c))
    });
    (name_ok && qualifier_ok).then(|| format!("<{name}>"))
}

fn sample(kind: Sample, original: &str) -> Value {
    match kind {
        Sample::Ulid => json!("00000000000000000000000000"),
        Sample::Time => json!("2026-01-01T00:00:00Z"),
        Sample::Zero => json!(0),
        Sample::Meta => json!({"elapsed_ms": 0, "offline": false}),
        Sample::Keep => json!(original),
    }
}

/// `value` with every whole-value placeholder, in string values and in object keys,
/// replaced by its sample. A placeholder with no row is kept and named in `unknown`
/// with its JSON path.
fn substitute(value: &Value, path: &str, unknown: &mut Vec<String>) -> Value {
    match value {
        Value::String(text) => replaced(text, path, unknown).unwrap_or_else(|| value.clone()),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| substitute(item, &format!("{path}/{index}"), unknown))
                .collect(),
        ),
        Value::Object(object) => {
            let mut out = serde_json::Map::new();
            for (key, item) in object {
                let here = format!("{path}/{key}");
                let key = match replaced(key, &here, unknown) {
                    Some(Value::String(sampled)) => sampled,
                    _ => key.clone(),
                };
                let item = substitute(item, &here, unknown);
                out.insert(key, item);
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

/// The sample for `text` when it is a whole-value placeholder; a placeholder with no row
/// is named in `unknown` with its path.
fn replaced(text: &str, path: &str, unknown: &mut Vec<String>) -> Option<Value> {
    let name = placeholder_name(text)?;
    match PLACEHOLDERS.iter().find(|(row, _)| *row == name) {
        Some((_, kind)) => Some(sample(*kind, text)),
        None => {
            unknown.push(format!(
                "{}: {text}",
                if path.is_empty() { "/" } else { path }
            ));
            None
        }
    }
}

/// Every `.snap` file under `dir`, recursively.
fn snaps(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            snaps(&path, out);
        } else if path.extension().is_some_and(|e| e == "snap") {
            out.push(path);
        }
    }
}

/// The JSON envelope an insta snapshot holds: the text after its header's closing `---`,
/// when it is an object with the envelope's `ok`, `schema_version` and `command`.
fn envelope_body(text: &str) -> Option<Value> {
    let mut dashes = 0;
    let mut body = String::new();
    for line in text.lines() {
        if dashes < 2 {
            dashes += usize::from(line == "---");
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    let value: Value = serde_json::from_str(&body).ok()?;
    ["ok", "schema_version", "command"]
        .iter()
        .all(|key| value.get(key).is_some())
        .then_some(value)
}

#[test]
fn every_envelope_snapshot_validates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["tests", "src"] {
        snaps(&root.join(dir), &mut files);
    }
    files.sort();
    let (mut validated, mut frame_only) = (0usize, 0usize);
    let mut failures = Vec::new();
    for file in &files {
        let relative = file.strip_prefix(root).unwrap().display().to_string();
        let Some(body) = envelope_body(&std::fs::read_to_string(file).unwrap()) else {
            continue;
        };
        let mut unknown = Vec::new();
        let mut envelope = substitute(&body, "", &mut unknown);
        if !unknown.is_empty() {
            failures.push(format!(
                "{relative}: placeholders with no sample (add a row to PLACEHOLDERS): {}",
                unknown.join(", ")
            ));
            continue;
        }
        if let Some(object) = envelope.as_object_mut() {
            object
                .entry("meta")
                .or_insert_with(|| sample(Sample::Meta, ""));
            if FRAME_ONLY.contains(&relative.as_str()) {
                object.remove("data");
                frame_only += 1;
            }
        }
        validated += 1;
        if let Err(message) = validate_envelope(&envelope, "") {
            failures.push(format!(
                "{relative}:\n{message}\nas validated: {envelope:#}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "envelope snapshots that do not validate:\n\n{}",
        failures.join("\n\n")
    );
    assert_eq!(
        frame_only,
        FRAME_ONLY.len(),
        "every frame-only fixture was found"
    );
    assert!(
        validated >= 100,
        "only {validated} envelope snapshots found under tests/ and src/: a wrong root?"
    );
    println!(
        "validated {validated} envelope snapshots ({frame_only} frame-only) of {} .snap files",
        files.len()
    );
}

#[test]
fn placeholder_table_cases() {
    let ulid = json!("00000000000000000000000000");
    let time = json!("2026-01-01T00:00:00Z");
    let cases: Vec<(&str, Value, Value, Vec<&str>)> = vec![
        (
            "ulid_value",
            json!({"id": "<ulid>"}),
            json!({"id": ulid}),
            vec![],
        ),
        (
            "tx_value",
            json!({"tx": "<tx>"}),
            json!({"tx": ulid}),
            vec![],
        ),
        (
            "key_value",
            json!({"key": "<key>"}),
            json!({"key": ulid}),
            vec![],
        ),
        (
            "run_value",
            json!({"run": "<run>"}),
            json!({"run": ulid}),
            vec![],
        ),
        (
            "time_value",
            json!({"at": "<time>"}),
            json!({"at": time}),
            vec![],
        ),
        (
            "checked_at_value",
            json!({"checked_at": "<checked_at>"}),
            json!({"checked_at": time}),
            vec![],
        ),
        (
            "bytes_value_becomes_integer",
            json!({"compressed_bytes": "<bytes>"}),
            json!({"compressed_bytes": 0}),
            vec![],
        ),
        (
            "ulid_as_map_key",
            json!({"targets": {"<key>": {"n": 1}}}),
            json!({"targets": {"00000000000000000000000000": {"n": 1}}}),
            vec![],
        ),
        (
            "qualified_by_client",
            json!({"key": "<key:retail>", "tx": "<tx:retail@steam>"}),
            json!({"key": ulid, "tx": ulid}),
            vec![],
        ),
        (
            "unknown_qualified_placeholder_fails",
            json!({"id": "<addon:retail>"}),
            json!({"id": "<addon:retail>"}),
            vec!["/id: <addon:retail>"],
        ),
        (
            "placeholder_inside_a_string_stays",
            json!({"path": "<root>/dl/foo.zip"}),
            json!({"path": "<root>/dl/foo.zip"}),
            vec![],
        ),
        (
            "version_stays",
            json!({"tool_version": "<version>"}),
            json!({"tool_version": "<version>"}),
            vec![],
        ),
        (
            "meta_becomes_an_object",
            json!({"meta": "<meta>"}),
            json!({"meta": {"elapsed_ms": 0, "offline": false}}),
            vec![],
        ),
        (
            "unknown_placeholder_fails",
            json!({"items": [{"name": "<addon_name>"}]}),
            json!({"items": [{"name": "<addon_name>"}]}),
            vec!["/items/0/name: <addon_name>"],
        ),
    ];
    for (name, input, expected, expected_unknown) in cases {
        let mut unknown = Vec::new();
        let got = substitute(&input, "", &mut unknown);
        assert_eq!(got, expected, "case {name}: {input}");
        assert_eq!(unknown, expected_unknown, "case {name}: {input}");
    }
    assert_eq!(
        envelope_body(
            "---\nsource: x\n---\n{\"ok\": true, \"schema_version\": \"1.0\", \"command\": \"status\"}\n"
        ),
        Some(json!({"ok": true, "schema_version": "1.0", "command": "status"})),
        "an insta body after its header"
    );
    assert_eq!(
        envelope_body("---\nsource: x\n---\n\"a string\"\n"),
        None,
        "not an envelope"
    );
}
