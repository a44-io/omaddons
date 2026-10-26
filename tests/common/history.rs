//! Synthetic repair lines (M4-D31) for the history readers' tests, before any command
//! writes one (E5): a `doctor fix` run is one transaction of kind `repair` whose line
//! names no single client (`target: null`), lists the clients it touched in `targets`,
//! its filesystem operations in `repairs`, and its doctor run directory in
//! `doctor_run_id`. E5, E7 and E9 reuse these builders.
//!
//! The builders return JSON, so a test can damage a line before planting it; [`plant`]
//! writes a line that parses as a history entry in that type's own key order, as another
//! omaddons would, and [`plant_raw`] writes text as it is.

use std::io::Write;
use std::path::Path;

use omaddons::model::{RunId, TargetKey, TxId};
use omaddons::state::history::HistoryEntry;
use serde_json::{Value, json};

use super::home::Home;
use super::http::paths;

/// The synthetic identity every primitive's endpoints carry.
fn identity() -> Value {
    json!({"ino": "4242", "born_ns": "1790000000000000000"})
}

/// A doctor run directory name: `<UTC time>Z__<ULID>`.
pub fn doctor_run_id() -> String {
    format!("2026-10-30T18-03-40Z__{}", RunId::new())
}

/// A `chmod` of `path` from mode `from` to `to` (octal digits, `"0644"`), home-side
/// unless [`on_client`] says otherwise.
pub fn chmod(seq: u32, path: &Path, from: &str, to: &str) -> Value {
    json!({
        "seq": seq,
        "fix": 1,
        "fm": "fm-secrets-credential-file-mode",
        "op": "chmod",
        "path": path,
        "pre": {"kind": "file", "identity": identity(), "mode": from},
        "post": {"kind": "file", "identity": identity(), "mode": to},
    })
}

/// A `rename_noreplace` of the directory `path` to `to`.
pub fn rename(seq: u32, path: &Path, to: &Path) -> Value {
    json!({
        "seq": seq,
        "fix": 1,
        "fm": "fm-external_artifacts-nested-addon-folder",
        "op": "rename_noreplace",
        "path": path,
        "to": to,
        "pre": {
            "path": {"kind": "dir", "identity": identity(), "stamp": "stamp:sha256:00"},
            "to": {"kind": "absent"},
        },
        "post": {
            "path": {"kind": "absent"},
            "to": {"kind": "dir", "identity": identity()},
        },
    })
}

/// `primitive` naming the client it touches and the AddOns folder its path is in.
pub fn on_client(mut primitive: Value, target: Option<TargetKey>, folder: Option<&str>) -> Value {
    if let Some(target) = target {
        primitive["target"] = json!(target);
    }
    if let Some(folder) = folder {
        primitive["folder"] = json!(folder);
    }
    primitive
}

/// A target-less line of `kind` (`repair`, or `undo` once the caller sets `reverts`) on
/// `targets`: a fresh tx and run, made now by a human running `doctor fix`.
pub fn repair_line(
    kind: &str,
    targets: &[TargetKey],
    doctor_run_id: &str,
    primitives: Vec<Value>,
) -> Value {
    json!({
        "schema_version": "1.0",
        "tx": TxId::new(),
        "kind": kind,
        "at": jiff::Timestamp::now(),
        "target": null,
        "targets": targets,
        "actor": "human",
        "argv": ["omaddons", "doctor", "fix"],
        "ops": [],
        "result": "committed",
        "reverts": null,
        "run": RunId::new(),
        "doctor_run_id": doctor_run_id,
        "repairs": primitives,
    })
}

/// Appends `line` and a newline to `<state>/history.jsonl`, as another omaddons would:
/// in the history entry's own key order when it parses as one, else as it is. Returns
/// the text written, newline included.
pub fn plant(home: &Home, line: &Value) -> String {
    let text = match serde_json::from_value::<HistoryEntry>(line.clone()) {
        Ok(entry) => serde_json::to_string(&entry).unwrap(),
        Err(_) => line.to_string(),
    };
    plant_raw(home, &text)
}

/// Appends `line` (a damaged or hand-made line) and a newline to `<state>/history.jsonl`.
/// Returns the text written.
pub fn plant_raw(home: &Home, line: &str) -> String {
    let path = paths(&home.env).history_file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text = format!("{line}\n");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
    text
}
