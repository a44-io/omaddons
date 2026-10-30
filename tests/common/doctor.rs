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

/// The seven failure modes `status` publishes, planted on a [`Home`] whose products are
/// among `wow` (`retail`), `wow_classic` (`classic`) and `wow_classic_era` (`era`),
/// through the binary where a change command makes the state and on disk otherwise. Each helper asserts that what it ran
/// succeeded, and is deterministic. Order matters twice: `state_version_newer` after every
/// change on its client (a newer record refuses changes), and `journal_pending` last (any
/// later change command recovers its journal first).
pub mod plant {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use crate::common::archives::zip_text;
    use crate::common::home::{Home, Run};
    use crate::common::http::{Asset, gh_release};

    /// Every addon here loads on all three clients.
    const FLAVORS: &[(&str, u32)] = &[
        ("mainline", 120_105),
        ("mists", 50_504),
        ("classic", 11_509),
    ];
    const TOC: &str = "## Interface: 120105, 50504, 11509\n## Version: v1\n";
    /// Each addon's one Lua file, as its release ships it.
    pub const LUA: &str = "local loaded = true\n";

    /// A target id's client folder.
    pub fn client_folder(target: &str) -> &'static str {
        match target {
            "classic" => "_classic_",
            "era" => "_classic_era_",
            _ => "_retail_",
        }
    }

    /// `foo` → `Foo`: an addon's one folder.
    pub fn folder(addon: &str) -> String {
        let mut chars = addon.chars();
        chars
            .next()
            .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
            .unwrap_or_default()
    }

    /// `omaddons <args> --json`, which must exit 0.
    fn must(home: &Home, args: &[&str]) -> Run {
        let run = home.run(args);
        assert_eq!(run.code, 0, "planting: {args:?}\n{}", run.context());
        run
    }

    /// Serves release v1 of `gh:o/<addon>`: one folder `<Addon>` holding its TOC and
    /// [`LUA`].
    pub fn serve(home: &Home, addon: &str) {
        let name = folder(addon);
        let (zip, toc, lua) = (
            format!("{name}.zip"),
            format!("{name}/{name}.toc"),
            format!("{name}/{name}.lua"),
        );
        let assets: [Asset<'_>; 1] = [(&zip, FLAVORS, vec![(&toc, TOC), (&lua, LUA)])];
        gh_release(
            &home.http,
            &format!("https://github.com/o/{addon}"),
            "v1",
            &assets,
        );
    }

    /// `gh:o/<addon>` served and installed on `target`.
    pub fn install(home: &Home, addon: &str, target: &str) {
        serve(home, addon);
        must(home, &["install", &format!("gh:o/{addon}"), "-t", target]);
    }

    /// The Lua file of `addon` on `target`.
    pub fn lua_file(home: &Home, target: &str, addon: &str) -> PathBuf {
        let name = folder(addon);
        home.addons(client_folder(target))
            .join(&name)
            .join(format!("{name}.lua"))
    }

    /// `fm-external_artifacts-managed-files-modified` (P2, `addon=<addon>` on `target`):
    /// `addon` installed, then its Lua file edited by hand (evidence: its counts).
    pub fn managed_files_modified(home: &Home, target: &str, addon: &str) {
        install(home, addon, target);
        fs::write(lua_file(home, target, addon), "local loaded = false\n").unwrap();
    }

    /// Puts back the bytes [`managed_files_modified`] replaced: the finding is resolved.
    pub fn restore_file(home: &Home, target: &str, addon: &str) {
        fs::write(lua_file(home, target, addon), LUA).unwrap();
    }

    /// `fm-state_files-record-folder-missing` (P1, `addon=<addon>` on `target`): `addon`
    /// installed, then its folder deleted.
    pub fn record_folder_missing(home: &Home, target: &str, addon: &str) {
        install(home, addon, target);
        fs::remove_dir_all(home.addons(client_folder(target)).join(folder(addon))).unwrap();
    }

    /// `fm-external_artifacts-ownership-transferred` (P2, on `target`): `gh:o/foo`
    /// installed, then `gh:o/bar` from an archive that also ships `Foo/`, with `--force`:
    /// Foo is degraded.
    pub fn ownership_transferred(home: &Home, target: &str) {
        install(home, "foo", target);
        let zip = home.env.root().join("barfoo.zip");
        fs::write(
            &zip,
            zip_text(&[("Bar/Bar.toc", TOC), ("Foo/Foo.toc", TOC)]),
        )
        .unwrap();
        let zip = zip.display().to_string();
        must(
            home,
            &[
                "install", "gh:o/bar", "--file", &zip, "-t", target, "--force",
            ],
        );
    }

    /// `fm-userland_state-target-vanished` (P2, on `target`): `target` registered by an
    /// install, then its client directory moved away; returns its id.
    pub fn target_vanished(home: &Home, target: &str) -> String {
        install(home, "gone", target);
        fs::rename(
            home.client(client_folder(target)),
            home.env.root().join(format!("unplugged-{target}")),
        )
        .unwrap();
        target.to_owned()
    }

    /// The key `target` is registered under.
    pub fn target_key(home: &Home, target: &str) -> String {
        let registry: Value =
            serde_json::from_slice(&fs::read(home.state().join("registry.json")).unwrap()).unwrap();
        registry["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == target)
            .and_then(|entry| entry["key"].as_str())
            .unwrap()
            .to_owned()
    }

    /// `fm-schemas-state-version-newer` (P0, `path=<record>` on `target`): an addon
    /// installed, then the client's record stamped as a newer omaddons would write it (it
    /// still decodes). After every change on `target`: a newer record refuses them.
    pub fn state_version_newer(home: &Home, target: &str) {
        install(home, "newer", target);
        let record = home
            .state()
            .join("targets")
            .join(format!("{}.json", target_key(home, target)));
        let mut doc: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        doc["schema_version"] = json!("1.99");
        fs::write(&record, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    }

    /// Where a run finds Omarchy's shell: give every run `OMARCHY_PATH` set to it, so the
    /// test does not depend on the machine's own Omarchy.
    pub fn omarchy_path(home: &Home) -> PathBuf {
        home.env.root().join("omarchy")
    }

    /// `fm-plugins-widget-version-drift` (P3, about the machine): Omarchy present (with
    /// [`omarchy_path`]) and a bar widget installed without its `Build.js`, which no build
    /// renders: outdated.
    pub fn widget_version_drift(home: &Home) {
        fs::create_dir_all(omarchy_path(home).join("shell")).unwrap();
        let plugin = home.env.config().join("omarchy/plugins/omaddons");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(
            plugin.join("manifest.json"),
            r#"{"id": "a44.omaddons", "version": "0.0.9"}"#,
        )
        .unwrap();
    }

    /// `fm-concurrency_primitives-journal-pending` (P0, `tx=<tx>` on `target`): an install
    /// killed after its journal's `prepared` line (`engine.journal.prepared=abort`).
    /// Last: any later change command recovers it first.
    pub fn journal_pending(home: &Home, target: &str) {
        serve(home, "stuck");
        let args = ["install", "gh:o/stuck", "-t", target, "--json"].map(str::to_owned);
        let crashed = home.exec_with(
            &args,
            &[("OMADDONS_FAILPOINTS", "engine.journal.prepared=abort")],
        );
        assert_eq!(
            crashed.code,
            -1,
            "planting a journal\n{}",
            crashed.context()
        );
    }

    /// The published run directories' names, sorted; never a stage (`.<id>.tmp`).
    pub fn runs(home: &Home) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(home.state().join("doctor/runs"))
            .map(|listing| {
                listing
                    .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                    .filter(|name| !name.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// The run `latest` names, when it is there.
    pub fn latest(home: &Home) -> Option<String> {
        let target = fs::read_link(home.state().join("doctor/latest")).ok()?;
        target.file_name()?.to_str().map(str::to_owned)
    }
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
