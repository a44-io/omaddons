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

/// The seven failure modes `status` publishes, and the ten journal and lock states,
/// planted on a [`Home`] whose products are among `wow` (`retail`), `wow_classic`
/// (`classic`) and `wow_classic_era` (`era`), through the binary where a change command
/// makes the state and on disk otherwise. Each helper asserts that what it ran succeeded
/// (or, for a crash, was killed), and is deterministic. Order matters twice:
/// `state_version_newer` after every change on its client (a newer record refuses
/// changes), and a journal last on its client (any later change command recovers it
/// first, and a too-new or unreadable one refuses it).
pub mod plant {
    use std::cell::RefCell;
    use std::fs;
    use std::path::{Path, PathBuf};

    use jiff::Timestamp;
    use omaddons::fsx::{GlobalLock, GlobalLockGuard};
    use omaddons::model::TxId;
    use serde_json::{Value, json};

    use crate::common::archives::zip_text;
    use crate::common::home::{Home, Run};
    use crate::common::http::{Asset, gh_release, paths};
    use crate::common::procfs::FakeProc;
    use crate::common::wtf::Wtf;

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

    /// `omaddons <args> --json`, which must exit 0; recorded for [`take_planted`].
    fn must(home: &Home, args: &[&str]) -> Run {
        let run = exec(home, &json_argv(args), &[]);
        assert_eq!(run.code, 0, "planting: {args:?}\n{}", run.context());
        run
    }

    fn json_argv(args: &[&str]) -> Vec<String> {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.push("--json".to_owned());
        argv
    }

    /// `omaddons <argv>` with `env`, recorded for [`take_planted`]: its argv, environment
    /// and exit (-1 for a process a failpoint killed).
    fn exec(home: &Home, argv: &[String], env: &[(&str, &str)]) -> Run {
        let run = home.exec_with(argv, env);
        let vars: serde_json::Map<String, Value> = env
            .iter()
            .map(|(key, value)| ((*key).to_owned(), json!(value)))
            .collect();
        planted(json!({"argv": argv, "env": vars, "exit": run.code}));
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

    /// `<cache>/omaddons/blobs/<sha>`.
    pub fn blob_path(home: &Home, sha: &str) -> PathBuf {
        home.env.cache().join("omaddons/blobs").join(sha)
    }

    /// `fm-caches-blob-hash-mismatch` (P2, `deep`, `machine:blob=<sha>`): `gh:o/foo` v1
    /// served and installed on `target`, so its archive is cached as `<cache>/blobs/<sha>`
    /// and its record names it, then the blob's middle byte flipped (`^ 0xff`) by a plain
    /// write, keeping its size, inode and mode (its ctime moves). The blob's name. The
    /// finding's evidence: `{path, expected: "sha256:<sha>", observed: "sha256:<the
    /// damaged bytes' digest>"}`.
    pub fn damaged_blob(home: &Home, target: &str) -> String {
        install(home, "foo", target);
        let record = crate::common::state::record(home, target);
        let sha = record.addons["foo"]
            .blob_sha256
            .clone()
            .expect("an archive downloaded through the cache");
        let path = blob_path(home, &sha);
        let mut bytes = fs::read(&path).unwrap();
        let offset = bytes.len() / 2;
        let old = bytes[offset];
        bytes[offset] ^= 0xff;
        fs::write(&path, &bytes).unwrap();
        planted(json!({
            "blob": {"sha": sha, "size": bytes.len(), "offset": offset, "old_byte": old, "new_byte": bytes[offset]},
        }));
        sha
    }

    /// The bytes [`damaged_blob`] replaced: the blob's, its middle byte flipped back.
    pub fn undamaged(home: &Home, sha: &str) -> Vec<u8> {
        let mut bytes = fs::read(blob_path(home, sha)).unwrap();
        let offset = bytes.len() / 2;
        bytes[offset] ^= 0xff;
        bytes
    }

    /// Puts `original` back as the blob `sha`, as the cache's own store does: written to a
    /// temp file beside it and renamed over its name (a new inode).
    pub fn restore_blob(home: &Home, sha: &str, original: &[u8]) {
        let path = blob_path(home, sha);
        let temp = path.with_file_name(format!(".omaddons-restore-{sha}"));
        fs::write(&temp, original).unwrap();
        fs::rename(&temp, &path).unwrap();
        planted(json!({"restored_blob": sha}));
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
    /// of `gh:o/stuck` killed after its journal's `prepared` line
    /// (`engine.journal.prepared=abort`), before any step. E11's verdict:
    /// `Recoverable { Prepared, Back }`; evidence `phase: prepared`, `kind: install`.
    /// Last: any later change command recovers it first ([`recover`]).
    pub fn journal_pending(home: &Home, target: &str) {
        journal_pending_tx(home, target);
    }

    /// [`journal_pending`], returning the transaction.
    pub fn journal_pending_tx(home: &Home, target: &str) -> String {
        serve(home, "stuck");
        crash(
            home,
            &["install", "gh:o/stuck", "-t", target],
            "engine.journal.prepared",
        )
    }

    // ---------------------------------------------------------------------------------
    // The journal and lock states (Appendix B rows 1-10). Each journal helper plants
    // exactly one verdict of E11's `simulate::diagnose`: a change killed at an engine
    // failpoint, then plain edits of the journal it left (each line parsed as JSON, one
    // key changed, written back with its newline). A client holds at most one journal
    // planted by a crash: the next change there recovers a recoverable journal first, and
    // an unrecoverable one refuses it. [`copy_journal`] makes more.

    thread_local! {
        /// What the helpers ran and edited on this thread, for [`take_planted`].
        static PLANTED: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    }

    fn planted(entry: Value) {
        PLANTED.with(|log| log.borrow_mut().push(entry));
    }

    /// Everything the helpers ran (argv, environment, exit) and every journal edit they
    /// made (the line before and after, any path below `WTF/` cut at it) on this thread
    /// since the last call, in order, so a suite logs how each state was planted.
    pub fn take_planted() -> Vec<Value> {
        PLANTED.with(|log| std::mem::take(&mut *log.borrow_mut()))
    }

    /// `text` with every path below a `WTF/` cut at it (`…/WTF/…`), so no account, realm
    /// or character name reaches a log.
    fn cut_wtf(text: &str) -> String {
        const WTF: &str = "/WTF/";
        let mut cut = String::new();
        let mut rest = text;
        while let Some(at) = rest.find(WTF) {
            cut.push_str(&rest[..at + WTF.len()]);
            cut.push('…');
            rest = &rest[at + WTF.len()..];
            rest = &rest[rest.find('"').unwrap_or(rest.len())..];
        }
        cut.push_str(rest);
        cut
    }

    fn file_name(path: &Path) -> String {
        path.file_name().unwrap().to_string_lossy().into_owned()
    }

    /// `<root>/aside/`: outside every directory discovery scans or omaddons keeps.
    pub fn aside(home: &Home) -> PathBuf {
        let dir = home.env.root().join("aside");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The transaction ids and files in `<state>/journal/`, sorted.
    pub fn journals(home: &Home) -> Vec<(String, PathBuf)> {
        let mut found: Vec<(String, PathBuf)> = fs::read_dir(home.state().join("journal"))
            .map(|listing| {
                listing
                    .filter_map(|entry| {
                        let path = entry.ok()?.path();
                        let name = path.file_name()?.to_str()?.strip_suffix(".jsonl")?;
                        let tx = name.split('.').next()?.to_owned();
                        Some((tx, path))
                    })
                    .collect()
            })
            .unwrap_or_default();
        found.sort();
        found
    }

    /// `tx`'s journal file (`<tx>.jsonl`, or `<tx>.wtf.jsonl` for one that puts settings
    /// back).
    pub fn journal_file(home: &Home, tx: &str) -> PathBuf {
        journals(home)
            .into_iter()
            .find(|(t, _)| t == tx)
            .map(|(_, path)| path)
            .unwrap_or_else(|| panic!("no journal of {tx}")) // ubs:ignore — the test's own failure
    }

    /// The first line of `tx`'s journal that `pick` selects, as JSON.
    pub fn journal_line(home: &Home, tx: &str, pick: impl Fn(&Value) -> bool) -> Value {
        let text = fs::read_to_string(journal_file(home, tx)).unwrap();
        text.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|line| pick(line))
            .unwrap_or_else(|| panic!("no such line in {tx}'s journal:\n{}", cut_wtf(&text))) // ubs:ignore — the test's own failure
    }

    /// A `prepared` line.
    pub fn is_prepared(line: &Value) -> bool {
        line["type"] == "prepared"
    }

    /// The `step` line of step `n`.
    pub fn is_step(n: u64) -> impl Fn(&Value) -> bool {
        move |line| line["type"] == "step" && line["n"] == n
    }

    /// A journal file as it was before a helper edited it.
    pub struct JournalEdit {
        pub path: PathBuf,
        original: Vec<u8>,
    }

    impl JournalEdit {
        fn of(path: PathBuf) -> Self {
            let original = fs::read(&path).unwrap();
            Self { path, original }
        }

        /// Writes the journal back byte for byte, as the crash left it.
        pub fn undo(&self) {
            fs::write(&self.path, &self.original).unwrap();
            planted(json!({"journal": file_name(&self.path), "restored": true}));
        }
    }

    /// Rewrites the first line of `tx`'s journal that `pick` selects, with `edit` applied
    /// to its JSON; the line keeps its newline (and comes out with its keys sorted).
    pub fn edit_line(
        home: &Home,
        tx: &str,
        pick: impl Fn(&Value) -> bool,
        edit: impl FnOnce(&mut Value),
    ) -> JournalEdit {
        let undo = JournalEdit::of(journal_file(home, tx));
        let text = String::from_utf8(undo.original.clone()).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let at = lines
            .iter()
            .position(|line| serde_json::from_str::<Value>(line).is_ok_and(|line| pick(&line)))
            .unwrap_or_else(|| panic!("no such line in {tx}'s journal:\n{}", cut_wtf(&text))); // ubs:ignore — the test's own failure
        let mut line: Value = serde_json::from_str(&lines[at]).unwrap();
        edit(&mut line);
        let after = line.to_string();
        planted(json!({
            "journal": file_name(&undo.path),
            "line": at + 1,
            "before": cut_wtf(&lines[at]),
            "after": cut_wtf(&after),
        }));
        lines[at] = after;
        fs::write(&undo.path, lines.join("\n") + "\n").unwrap();
        undo
    }

    /// Appends the complete line `line` to `tx`'s journal.
    pub fn append_line(home: &Home, tx: &str, line: &Value) -> JournalEdit {
        let undo = JournalEdit::of(journal_file(home, tx));
        let mut bytes = undo.original.clone();
        bytes.extend_from_slice(line.to_string().as_bytes());
        bytes.push(b'\n');
        fs::write(&undo.path, bytes).unwrap();
        planted(json!({"journal": file_name(&undo.path), "appended": line}));
        undo
    }

    /// A copy of `tx`'s journal under a fresh transaction id, its `prepared` line's `tx`
    /// and `commit.history.tx` set to that id, which this returns. Nothing else is copied
    /// (stage, snapshots, in-flight lock): the verdicts of a too-new, unreadable or
    /// mismatched journal do not depend on them, so a scenario that needs several such
    /// journals on one client plants one by a crash and derives the others.
    pub fn copy_journal(home: &Home, tx: &str) -> String {
        let from = journal_file(home, tx);
        let copy = TxId::new().to_string();
        let text = fs::read_to_string(&from).unwrap();
        let mut copied = String::new();
        for line in text.lines() {
            match serde_json::from_str::<Value>(line) {
                Ok(mut value) if is_prepared(&value) => {
                    value["tx"] = json!(copy);
                    if value["commit"]["history"].is_object() {
                        value["commit"]["history"]["tx"] = json!(copy);
                    }
                    copied.push_str(&value.to_string());
                }
                _ => copied.push_str(line),
            }
            copied.push('\n');
        }
        let name = file_name(&from).replacen(tx, &copy, 1);
        fs::write(from.with_file_name(&name), copied).unwrap();
        planted(json!({"journal": name, "copied_from": file_name(&from)}));
        copy
    }

    /// Runs `omaddons <args> --json` with `failpoint` armed as an abort, which must kill
    /// it; returns the transaction of the one journal it left.
    fn crash(home: &Home, args: &[&str], failpoint: &str) -> String {
        let before = journals(home);
        let spec = format!("{failpoint}=abort");
        let run = exec(home, &json_argv(args), &[("OMADDONS_FAILPOINTS", &spec)]);
        assert_eq!(
            run.code,
            -1,
            "planting: {spec} must kill {args:?}\n{}",
            run.context()
        );
        let new: Vec<String> = journals(home)
            .into_iter()
            .filter(|entry| !before.contains(entry))
            .map(|(tx, _)| tx)
            .collect();
        assert_eq!(
            new.len(),
            1,
            "planting {args:?}: one new journal, not {new:?}"
        );
        new.into_iter().next().unwrap()
    }

    /// Settles every folder journal: `config set check_interval_hours 6`, a command that
    /// takes the global lock and recovers first. It runs no discovery, so it never moves
    /// a `WTF/`: a journal that puts settings back needs [`recover_wtf`].
    pub fn recover(home: &Home) {
        must(home, &["config", "set", "check_interval_hours", "6"]);
    }

    /// Settles every journal, `WTF/` ones included: `gh:o/recover` installed on `target`
    /// from a local archive (`--file`, `--offline`), a change command that runs discovery
    /// and recovers first.
    pub fn recover_wtf(home: &Home, target: &str) {
        let run = exec(home, &recover_argv(home, target), &[]);
        assert_eq!(run.code, 0, "recovering\n{}", run.context());
    }

    /// [`recover_wtf`] with `failpoint` armed as an abort, which must kill it.
    pub fn recover_wtf_killed_at(home: &Home, target: &str, failpoint: &str) {
        let spec = format!("{failpoint}=abort");
        let run = exec(
            home,
            &recover_argv(home, target),
            &[("OMADDONS_FAILPOINTS", &spec)],
        );
        assert_eq!(
            run.code,
            -1,
            "recovering: {spec} must kill it\n{}",
            run.context()
        );
    }

    fn recover_argv(home: &Home, target: &str) -> Vec<String> {
        let zip = home.env.root().join("dl/Recover.zip");
        if !zip.exists() {
            fs::create_dir_all(zip.parent().unwrap()).unwrap();
            fs::write(&zip, zip_text(&[("Recover/Recover.toc", TOC)])).unwrap();
        }
        let zip = zip.display().to_string();
        json_argv(&[
            "install",
            "gh:o/recover",
            "--file",
            &zip,
            "-t",
            target,
            "--offline",
        ])
    }

    /// A target id's executable.
    pub fn exe(target: &str) -> &'static str {
        match target {
            "classic" | "era" => "WoWClassic.exe",
            _ => "Wow.exe",
        }
    }

    /// A fake `/proc` at `<root>/proc-<target>` in which `target`'s client runs (pid
    /// 4242, in the home's prefix) and nothing else, as `Home::client_running` builds
    /// one. Give a run its root as `OMADDONS_PROC_ROOT`.
    pub fn running(home: &Home, target: &str) -> FakeProc {
        let prefix = home.prefix.display().to_string();
        let path = format!(
            "C:\\Program Files (x86)\\World of Warcraft\\{}\\{}",
            client_folder(target),
            exe(target)
        );
        FakeProc::new(home.env.root().join(format!("proc-{target}"))).process(
            4242,
            true,
            exe(target),
            &[&path],
            &[("WINEPREFIX", &prefix)],
        )
    }

    /// A whole `backup restore` on `target` killed right after its exchange
    /// (`engine.restore.exchanged=abort`): a `writes_wtf` journal (`<tx>.wtf.jsonl`) whose
    /// step 1 swapped the backup's copy in for the live `WTF/`. First the client gets a
    /// `WTF/` holding `Foo`'s SavedVariables (fake names only), a manual backup of it, and
    /// an edit after that backup, so the restore changes something. Returns the
    /// transaction.
    pub fn crashed_restore(home: &Home, target: &str) -> String {
        let client = home.client(client_folder(target));
        let wtf = Wtf::at(&client).saved_variables("Foo", 64).build();
        let made = must(home, &["backup", "create", "-t", target]).envelope();
        let id = made["data"]["targets"][0]["backup"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        fs::write(wtf.join("Config.wtf"), "SET edited \"after the backup\"\n").unwrap();
        crash(
            home,
            &["backup", "restore", &id, "-t", target],
            "engine.restore.exchanged",
        )
    }

    /// `fm-concurrency_primitives-journal-waits-for-game` (P2, `tx=<tx>` on `target`):
    /// [`crashed_restore`], and [`running`] for `target`. E11's verdict with that fake
    /// `/proc`: `WaitsForGame`; evidence `reason: saved_variables`, `kind: restore`.
    /// Returns the transaction and the fake. Without it the game has exited, and
    /// [`recover_wtf`] settles the journal.
    pub fn journal_waits_for_game(home: &Home, target: &str) -> (String, FakeProc) {
        let tx = crashed_restore(home, target);
        (tx, running(home, target))
    }

    /// `fm-concurrency_primitives-journal-too-new` (P0, `tx=<tx>` on `target`):
    /// [`journal_pending`], then its `prepared` line's `schema_version` set to `"9.0"`, a
    /// newer major this build never recovers. Verdict `TooNew`; evidence `reason:
    /// journal_too_new`, `format: "9.0"`. Refuses every later change on `target`; undo
    /// the edit, then [`recover`].
    pub fn journal_too_new(home: &Home, target: &str) -> (String, JournalEdit) {
        let tx = journal_pending_tx(home, target);
        let edit = edit_line(home, &tx, is_prepared, |line| {
            line["schema_version"] = json!("9.0");
        });
        (tx, edit)
    }

    /// `fm-concurrency_primitives-journal-unreadable` (P0, `tx=<tx>` on `target`):
    /// [`journal_pending`], then the complete line `{"type":"from_a_later_build"}`
    /// appended. Verdict `Unreadable`; evidence `reason: journal_unreadable`. Refuses
    /// every later change on `target`; undo the edit, then [`recover`].
    pub fn journal_unreadable(home: &Home, target: &str) -> (String, JournalEdit) {
        let tx = journal_pending_tx(home, target);
        let edit = append_line(home, &tx, &json!({"type": "from_a_later_build"}));
        (tx, edit)
    }

    /// What a helper moved into [`aside`]: put back with [`Moved::undo`].
    pub struct Moved {
        /// Each path, and where it went.
        pub moves: Vec<(PathBuf, PathBuf)>,
        /// The empty directory made in a moved path's place, if any.
        pub made: Option<PathBuf>,
    }

    impl Moved {
        /// Removes the empty directory, and moves each path back.
        pub fn undo(&self) {
            if let Some(made) = &self.made {
                fs::remove_dir(made).unwrap();
            }
            for (path, away) in &self.moves {
                fs::rename(away, path).unwrap();
            }
            planted(json!({"moved_back": self.moves.len()}));
        }
    }

    /// `fm-concurrency_primitives-journal-unexpected-state` (P0, `tx=<tx>` on `target`):
    /// [`crashed_restore`], then both sides of its step 1 replaced: the staged and the
    /// live `WTF/` moved into [`aside`], and an empty `WTF/` made at the live path.
    /// Verdict `UnexpectedState { step: 1 }`; evidence `reason: unexpected_state`, `step:
    /// 1`. Undo the moves, then [`recover_wtf`].
    pub fn journal_unexpected_state(home: &Home, target: &str) -> (String, Moved) {
        let tx = crashed_restore(home, target);
        let step = journal_line(home, &tx, is_step(1));
        let (from, to) = (
            PathBuf::from(step["from"].as_str().unwrap()),
            PathBuf::from(step["to"].as_str().unwrap()),
        );
        let aside = aside(home);
        let moves = vec![
            (from, aside.join(format!("{tx}.staged-WTF"))),
            (to.clone(), aside.join(format!("{tx}.live-WTF"))),
        ];
        for (path, away) in &moves {
            fs::rename(path, away).unwrap();
        }
        fs::create_dir(&to).unwrap();
        planted(json!({"moved_aside": ["staged WTF/", "live WTF/"], "made": "an empty live WTF/"}));
        (
            tx,
            Moved {
                moves,
                made: Some(to),
            },
        )
    }

    /// `fm-concurrency_primitives-journal-step-unrecognized` (P0, `tx=<tx>` on
    /// `target`): an install of `gh:o/unrecognized`, not installed yet, killed after its
    /// step 1 was journaled and before that rename ran (`engine.swap.before_rename=abort`);
    /// then that `step` line's `from_id.ino` increased by 1,000,000, so the staged folder
    /// matches neither the step's before nor its after state. Verdict `StepUnrecognized {
    /// step: 1 }`; evidence `reason: step_unrecognized`, `step: 1`. Undo the edit, then
    /// [`recover`].
    pub fn journal_step_unrecognized(home: &Home, target: &str) -> (String, JournalEdit) {
        serve(home, "unrecognized");
        let tx = crash(
            home,
            &["install", "gh:o/unrecognized", "-t", target],
            "engine.swap.before_rename",
        );
        let edit = edit_line(home, &tx, is_step(1), |line| {
            let ino = line["from_id"]["ino"].as_u64().unwrap();
            line["from_id"]["ino"] = json!(ino + 1_000_000);
        });
        (tx, edit)
    }

    /// `fm-concurrency_primitives-journal-marker-mismatch` (P0, `tx=<tx>` on `target`):
    /// `other` registered (an addon installed there, which writes its marker), then
    /// [`journal_pending`] on `target`, its `prepared` line's `client_dir` set to
    /// `other`'s client dir. Verdict `MarkerMismatch`; evidence `reason:
    /// marker_mismatch`, `path` the other client dir, `expected` `target`'s key,
    /// `observed` `other`'s. Undo the edit, then [`recover`].
    pub fn journal_marker_mismatch(
        home: &Home,
        target: &str,
        other: &str,
    ) -> (String, JournalEdit) {
        install(home, "elsewhere", other);
        let tx = journal_pending_tx(home, target);
        let dir = home.client(client_folder(other)).display().to_string();
        let edit = edit_line(home, &tx, is_prepared, |line| {
            line["client_dir"] = json!(dir);
        });
        (tx, edit)
    }

    /// `fm-concurrency_primitives-journal-unattributed` (P0, about the machine,
    /// `tx=<tx>`): `<state>/journal/<a fresh id>.jsonl` holding the one complete line
    /// `{"type":"prepared","schema_version":"1.1"}`, which names no target and does not
    /// decode. Verdict `Unattributed { cause: Unreadable }`; evidence `cause: unreadable`.
    /// Refuses every change on every client; move the file into [`aside`] to clear it.
    /// Returns the transaction.
    pub fn journal_unattributed(home: &Home) -> String {
        let tx = TxId::new().to_string();
        let dir = home.state().join("journal");
        fs::create_dir_all(&dir).unwrap();
        let line = r#"{"type":"prepared","schema_version":"1.1"}"#;
        fs::write(dir.join(format!("{tx}.jsonl")), format!("{line}\n")).unwrap();
        planted(json!({"journal": format!("{tx}.jsonl"), "written": line}));
        tx
    }

    /// Moves `tx`'s journal into [`aside`].
    pub fn set_journal_aside(home: &Home, tx: &str) {
        let journal = journal_file(home, tx);
        fs::rename(&journal, aside(home).join(file_name(&journal))).unwrap();
        planted(json!({"journal": file_name(&journal), "moved_aside": true}));
    }

    /// `fm-concurrency_primitives-journal-preservation-incomplete` (P1, `tx=<tx>` on
    /// `target`): [`crashed_restore`]; a play session on the restored `WTF/` (one
    /// SavedVariables file rewritten), so rolling back must keep that tree as a
    /// `recovered` backup; then [`recover_wtf`] killed once that intent is journaled and
    /// before it is done (`engine.preserve.intent=abort`). Verdict
    /// `PreservationIncomplete { stops: false }`; evidence `resumable: true`, `count` the
    /// trees to keep. [`recover_wtf`] again resumes and completes it.
    pub fn journal_preservation_incomplete(home: &Home, target: &str) -> String {
        let tx = crashed_restore(home, target);
        let played = home
            .client(client_folder(target))
            .join("WTF/Account/FAKEACCT/SavedVariables/Foo.lua");
        fs::write(played, "FooDB = { played = true }\n").unwrap();
        planted(json!({"played_on": "the restored WTF/"}));
        recover_wtf_killed_at(home, target, "engine.preserve.intent");
        tx
    }

    /// [`journal_preservation_incomplete`], then the first item its pending intent names
    /// (the played-on tree, where recovery found it) moved into [`aside`]: recovery can
    /// place it neither there nor in the assembly, so it stops. Verdict
    /// `PreservationIncomplete { stops: true }`; evidence `resumable: false`. Undo the move,
    /// then [`recover_wtf`] completes it.
    pub fn journal_preservation_stops(home: &Home, target: &str) -> (String, Moved) {
        let tx = journal_preservation_incomplete(home, target);
        let intent = journal_line(home, &tx, |line| line["type"] == "preserve");
        let item = PathBuf::from(intent["items"][0]["path"].as_str().unwrap());
        let away = aside(home).join(format!("{tx}.displaced"));
        fs::rename(&item, &away).unwrap();
        planted(json!({"moved_aside": "the displaced tree the intent names"}));
        (
            tx,
            Moved {
                moves: vec![(item, away)],
                made: None,
            },
        )
    }

    /// The `recovered` backups on `target`, by id.
    pub fn recovered_backups(home: &Home, target: &str) -> Vec<String> {
        let dir = home
            .client(client_folder(target))
            .join("Interface/.omaddons/backups");
        let mut ids: Vec<String> = fs::read_dir(dir)
            .map(|listing| {
                listing
                    .filter_map(|entry| {
                        let path = entry.ok()?.path();
                        let meta: Value =
                            serde_json::from_slice(&fs::read(path.join("backup.json")).ok()?)
                                .ok()?;
                        (meta["kind"] == "recovered").then(|| file_name(&path))
                    })
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        ids
    }

    /// `fm-concurrency_primitives-lock-held-long` (P2, about the machine): the global lock
    /// taken by this test process through the library, as `command` (the holder record
    /// names this process's pid). Returns the guard, which holds it until dropped, and the
    /// time right after it was taken: a diagnosing run whose `OMADDONS_NOW` is more than
    /// `LOCK_HELD_LONG` past it finds the holder.
    pub fn hold_lock(home: &Home, command: &str) -> (GlobalLockGuard, Timestamp) {
        let guard = GlobalLock::acquire(&paths(&home.env), command).unwrap();
        let taken = Timestamp::now();
        planted(json!({"lock": command, "pid": std::process::id()}));
        (guard, taken)
    }

    /// The journal set of `tx`, those of its paths that exist: its journal, its in-flight
    /// lock, its state snapshots, and on `target`'s client (none for a journal no client
    /// carries) its stage and snapshots, each under its bare and its `.wtf` name. What a
    /// set-aside moves (F2's subject rule).
    pub fn journal_set(home: &Home, tx: &str, target: Option<&str>) -> Vec<PathBuf> {
        let state = home.state();
        let mut set: Vec<PathBuf> = journals(home)
            .into_iter()
            .filter(|(t, _)| t == tx)
            .map(|(_, path)| path)
            .collect();
        set.push(state.join("inflight").join(format!("{tx}.lock")));
        let names = [tx.to_owned(), format!("{tx}.wtf")];
        for name in &names {
            set.push(state.join("snapshots").join(name));
        }
        if let Some(target) = target {
            let meta = home
                .client(client_folder(target))
                .join("Interface/.omaddons");
            for side in ["stage", "snapshots"] {
                for name in &names {
                    set.push(meta.join(side).join(name));
                }
            }
        }
        set.retain(|path| fs::symlink_metadata(path).is_ok());
        set
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

/// Subjects for the doctor's three test-only fixers (s2 of `doctor fix`): registered only
/// in debug builds run with `OMADDONS_TEST_FIXERS=1`, so every run that should see them
/// passes [`test_fixers::ENV`].
pub mod test_fixers {
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::common::home::Home;

    /// The switch that registers the test fixers.
    pub const ENV: (&str, &str) = ("OMADDONS_TEST_FIXERS", "1");
    pub const WRITE: &str = "fm-test-write";
    pub const QUARANTINE: &str = "fm-test-quarantine";
    pub const FLATTEN: &str = "fm-test-flatten";
    /// The directory each side's subjects live in.
    pub const TEST_DIR: &str = "doctor-test";
    /// What `fm-test-write`'s fixer writes.
    pub const REPAIRED: &[u8] = b"repaired\n";

    /// Where a subject lives: below omaddons' state, config or cache directory, or below
    /// a client's `<meta>` (`Interface/.omaddons`), by client folder.
    #[derive(Debug, Clone, Copy)]
    pub enum Side<'a> {
        State,
        Config,
        Cache,
        Client(&'a str),
    }

    /// A quarantine subject's shape.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Kind {
        File,
        Dir,
    }

    /// `doctor-test/` on `side`.
    pub fn test_dir(home: &Home, side: Side<'_>) -> PathBuf {
        match side {
            Side::State => home.state().join(TEST_DIR),
            Side::Config => home.env.config().join("omaddons").join(TEST_DIR),
            Side::Cache => home.env.cache().join("omaddons").join(TEST_DIR),
            Side::Client(folder) => home
                .client(folder)
                .join("Interface/.omaddons")
                .join(TEST_DIR),
        }
    }

    fn put(path: &Path, bytes: &[u8]) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        path.to_path_buf()
    }

    /// `<state>/doctor-test/<rel>` (a `*.write`) holding `bytes`: an `fm-test-write`
    /// subject unless `bytes` is [`REPAIRED`]. A `*.residual.write` stays broken after
    /// its repair.
    pub fn plant_write(home: &Home, rel: &str, bytes: &[u8]) -> PathBuf {
        put(&test_dir(home, Side::State).join(rel), bytes)
    }

    /// The marker `<rel>.missing` with no `<rel>` (a `*.write`): the fixer creates `<rel>`,
    /// whose path this returns.
    pub fn plant_missing_write(home: &Home, rel: &str) -> PathBuf {
        let target = test_dir(home, Side::State).join(rel);
        let mut marker = target.clone().into_os_string();
        marker.push(".missing");
        put(Path::new(&marker), b"");
        target
    }

    /// `doctor-test/<rel>` (a `*.quarantine`) on `side`: a file, or a directory holding
    /// one file. An `fm-test-quarantine` subject.
    pub fn plant_quarantine(home: &Home, side: Side<'_>, rel: &str, kind: Kind) -> PathBuf {
        let path = test_dir(home, side).join(rel);
        match kind {
            Kind::File => put(&path, b"set aside\n"),
            Kind::Dir => {
                put(&path.join("inside.txt"), b"kept\n");
                path
            }
        }
    }

    /// An unmanaged `AddOns/<wrapper>/<inner>/` on the client in `folder`, the inner
    /// folder holding its TOC: an `fm-test-flatten` subject (two renames, or three when
    /// `inner` is `wrapper`). The client must be registered ([`register`]).
    pub fn plant_flatten(home: &Home, folder: &str, wrapper: &str, inner: &str) -> PathBuf {
        let path = home.addons(folder).join(wrapper);
        put(
            &path.join(inner).join(format!("{inner}.toc")),
            b"## Interface: 120105\n## Title: Test\n",
        );
        path
    }

    /// Registers `target` (`targets default <target>`), so it has a key and the
    /// client-side test fixers see it.
    pub fn register(home: &Home, target: &str) {
        let run = home.run(&["targets", "default", target]);
        assert_eq!(run.code, 0, "registering {target}\n{}", run.context());
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
