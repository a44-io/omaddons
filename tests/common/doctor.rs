//! The doctor's envelope contract (M4-D34, M4-D40), as the contract harness checks every
//! `doctor` command's output: one document whose report keys sit at the top level between
//! `exit_code` and the envelope's trailing keys, an exit from the doctor's dictionary, an
//! `error` exactly on the exits that are failures, and one stderr `error` event for it.

use omaddons::app::{Dictionary, dictionary_of};
use omaddons::doctor::exit::{Exit as DoctorExit, exit_for};
use omaddons::error::ErrorCode;
use serde::Deserialize;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde_json::Value;

/// The doctor exits that are failures and carry an `error`; findings, a partial fix and a
/// rolled-back fix are reports.
pub const ERROR_EXITS: &[i32] = &[4, 5, 6, 64, 66, 73, 74];

/// The envelope's keys before the report, in wire order (`kind` only in a run's record).
const HEAD: &[&str] = &["ok", "schema_version", "tool_version", "command"];
/// The envelope's keys after the report, in wire order.
const TAIL: &[&str] = &["error", "warnings", "commands", "meta"];

/// Every key of the envelope itself; the others are the report's.
pub fn envelope_key(key: &str) -> bool {
    HEAD.contains(&key) || TAIL.contains(&key) || key == "kind" || key == "exit_code"
}

/// Whether the envelope `command` exits in the doctor's dictionary.
pub fn is_doctor(command: &str) -> bool {
    dictionary_of(command) == Dictionary::Doctor
}

/// The top-level keys of a JSON object, in document order (a `Value` sorts them).
struct Keys(Vec<String>);

impl<'de> Deserialize<'de> for Keys {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Keys;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Keys, A::Error> {
                let mut keys = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    keys.push(key);
                }
                Ok(Keys(keys))
            }
        }
        d.deserialize_map(V)
    }
}

/// Asserts the doctor envelope contract on one run's stdout, stderr and exit `code`, and
/// returns the document. Each failure names its rule, the stdout and the stderr.
pub fn check_envelope(stdout: &str, stderr: &str, code: i32) -> Value {
    let ctx = format!("stdout: {stdout}\nstderr: {stderr}");
    let docs: Result<Vec<Value>, _> = serde_json::Deserializer::from_str(stdout)
        .into_iter::<Value>()
        .collect();
    assert!(docs.is_ok(), "doctor: stdout is not JSON\n{ctx}");
    let docs = docs.unwrap();
    assert_eq!(docs.len(), 1, "doctor: exactly one document\n{ctx}");
    let envelope = docs.into_iter().next().unwrap();

    // The key order: the head, an optional `kind`, `exit_code`, the report, the tail.
    let keys: Keys = serde_json::from_str(stdout).unwrap();
    let keys: Vec<&str> = keys.0.iter().map(String::as_str).collect();
    for key in ["data", "partial"] {
        assert!(
            !keys.contains(&key),
            "doctor: a doctor envelope has no `{key}`\n{ctx}"
        );
    }
    assert!(
        keys.starts_with(HEAD),
        "doctor: envelope key order: {keys:?} does not start with {HEAD:?}\n{ctx}"
    );
    let mut at = HEAD.len();
    if keys.get(at) == Some(&"kind") {
        at += 1;
    }
    assert_eq!(
        keys.get(at),
        Some(&"exit_code"),
        "doctor: envelope key order: `exit_code` after the head and `kind`: {keys:?}\n{ctx}"
    );
    at += 1;
    let report_end = keys[at..]
        .iter()
        .position(|key| TAIL.contains(key))
        .map_or(keys.len(), |end| at + end);
    let tail = &keys[report_end..];
    let expected: Vec<&str> = TAIL.iter().copied().filter(|k| tail.contains(k)).collect();
    assert_eq!(
        tail, expected,
        "doctor: envelope key order: a report key after the envelope's trailing keys, or those out of order: {keys:?}\n{ctx}"
    );
    for key in ["warnings", "commands"] {
        assert!(tail.contains(&key), "doctor: no `{key}`\n{ctx}");
    }
    assert!(
        keys.contains(&"kind") != keys.contains(&"meta"),
        "doctor: `meta` on stdout, `kind` in a record: exactly one of them\n{ctx}"
    );

    // The exit, `ok` and `error` agree.
    assert!(
        DoctorExit::ALL.iter().any(|e| i32::from(e.code()) == code),
        "doctor: exit {code} is not in the doctor's dictionary\n{ctx}"
    );
    assert_eq!(envelope["exit_code"], code, "doctor: `exit_code`\n{ctx}");
    assert_eq!(envelope["ok"], code == 0, "doctor: `ok` is exit 0\n{ctx}");
    assert_eq!(
        envelope["schema_version"], "1.0",
        "doctor: schema_version\n{ctx}"
    );
    let error = envelope.get("error");
    assert_eq!(
        error.is_some(),
        ERROR_EXITS.contains(&code),
        "doctor: an `error` exactly on exits {ERROR_EXITS:?}, not on exit {code}\n{ctx}"
    );
    if let Some(error) = error {
        let wire = error["code"].as_str().unwrap_or_default();
        let registered = serde_json::from_value::<ErrorCode>(Value::from(wire));
        assert!(
            registered.is_ok(),
            "doctor: {wire:?} is not a registered code\n{ctx}"
        );
        if code == 73 {
            assert_eq!(
                wire, "io",
                "doctor: exit 73 is the run directory's io error\n{ctx}"
            );
        } else {
            let mapped = exit_for(registered.unwrap()).code();
            assert_eq!(
                i32::from(mapped),
                code,
                "doctor: the doctor exit of {wire} is {mapped}\n{ctx}"
            );
        }
    }

    // stderr: NDJSON, with one `error` event exactly when there is an `error`.
    let mut events = 0;
    for line in stderr.lines() {
        let event = serde_json::from_str::<Value>(line);
        assert!(
            event.is_ok(),
            "doctor: stderr line is not JSON: {line}\n{ctx}"
        );
        events += usize::from(event.unwrap()["event"] == "error");
    }
    assert_eq!(
        events,
        usize::from(error.is_some()),
        "doctor: stderr error events\n{ctx}"
    );

    // `next_steps` mirrors `commands[]`.
    if let Some(next) = envelope.get("next_steps") {
        let commands: Vec<&Value> = envelope["commands"]
            .as_array()
            .map(|c| c.iter().map(|hint| &hint["command"]).collect())
            .unwrap_or_default();
        let next: Vec<&Value> = next
            .as_array()
            .map(|n| n.iter().collect())
            .unwrap_or_default();
        assert_eq!(
            next, commands,
            "doctor: `next_steps` mirrors `commands[].command`\n{ctx}"
        );
    }
    envelope
}

/// The doctor envelope in the general shape, its report under `data` (when it has one),
/// so the general validator checks each part against its schema.
pub fn as_general(envelope: &Value) -> Value {
    let mut general = serde_json::Map::new();
    let mut report = serde_json::Map::new();
    for (key, value) in envelope.as_object().into_iter().flatten() {
        let side = if envelope_key(key) {
            &mut general
        } else {
            &mut report
        };
        side.insert(key.clone(), value.clone());
    }
    if !report.is_empty() {
        general.insert("data".to_owned(), Value::Object(report));
    }
    Value::Object(general)
}
