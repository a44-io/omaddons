//! e2e suite: `status` reports its seven failure modes from the doctor's registry, unchanged
//!
//! Each published failure mode planted on a synthetic home and read back through the real
//! binary: the exact `issues` JSON, with and without `--cached`, the human line under its
//! client, the `--check` exit, and every `status` run leaving every tree as it found it.
//! It is the regression net for the seven published ids while the catalog leaves rewrite
//! their detectors and F8 moves `status` onto the whole registry.

mod common;

use std::fs;
use std::path::PathBuf;

use common::addons::toc;
use common::archives::zip_text;
use common::drift::edit_managed;
use common::home::Home;
use common::http::paths;
use common::netlog::net_log;
use common::state::record;
use common::tlog::{Ran, TestLog, tlog};
use common::trees::CustomRoot;
use jiff::Timestamp;
use omaddons::doctor::registry;
use omaddons::engine::journal::{self, CommitPayload, IndexChanges, JournalLine, Prepared};
use omaddons::fsx::GlobalLock;
use omaddons::model::{Actor, TargetKey, TxId, TxKind};
use omaddons::state::history::{self, HistoryEntry, TxResult};
use omaddons::state::records::TargetRecord;
use omaddons::state::store::SchemaVersion;
use serde_json::{Value, json};

const JOURNAL: &str = "fm-concurrency_primitives-journal-pending";
const README_PROSE: &str =
    "see \"journal_pending that does not clear\" under Troubleshooting in the README";
const WAITS_PROSE: &str = "quit the game if it is running, then run any change command; it finishes or rolls back the change first";

use support::{
    Planted, assert_human_line, assert_issues, assert_registry_ids, assert_trees_unchanged,
    command, install, issue, issues, key_of, machine_issues, plant_journal, prose, record_trees,
    reinstall, status, take_over, trees,
};

#[cfg(test)]
mod support {
    use super::*;

    // ---------------------------------------------------------------------------------------
    // Building the states.

    /// Installs a one-folder addon `name` into `target` from a local zip, as `gh:o/<name>`,
    /// and returns the name its record holds.
    pub(super) fn install(
        log: &TestLog,
        home: &Home,
        target: &str,
        name: &str,
        interface: &str,
    ) -> String {
        log.step(&format!("install {name}"));
        let zip = write_zip(home, name, &[(name, interface)]);
        let reference = format!("gh:o/{}", name.to_ascii_lowercase());
        log.run(
            home,
            &[
                "install", &reference, "--file", &zip, "-t", target, "--json",
            ],
        )
        .ok();
        recorded_name(home, target, name)
    }

    /// Installs `gh:o/bar` from a zip holding `Bar/` and `taken/` with `--force`, so the
    /// addon that owned `taken` is degraded.
    pub(super) fn take_over(log: &TestLog, home: &Home, taken: &str) {
        log.step("install Bar with --force");
        let zip = write_zip(home, "Bar", &[("Bar", "120105"), (taken, "120105")]);
        log.run(
            home,
            &[
                "install", "gh:o/bar", "--file", &zip, "-t", "retail", "--force", "--json",
            ],
        )
        .ok();
    }

    /// `<root>/dl/<name>.zip` holding one folder per `(folder, interface)`.
    fn write_zip(home: &Home, name: &str, folders: &[(&str, &str)]) -> String {
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        let tocs: Vec<(String, String)> = folders
            .iter()
            .map(|(folder, interface)| (format!("{folder}/{folder}.toc"), toc(interface, &[])))
            .collect();
        let entries: Vec<(&str, &str)> =
            tocs.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        let zip = dl.join(format!("{name}.zip"));
        fs::write(&zip, zip_text(&entries)).unwrap();
        zip.display().to_string()
    }

    /// The name `target`'s record holds for the addon installed as `name` (read back, never
    /// assumed).
    fn recorded_name(home: &Home, target: &str, name: &str) -> String {
        let record = record(home, target);
        let found = record
            .addons
            .values()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .map(|a| a.name.clone());
        assert!(
            found.is_some(),
            "no {name} in {target}'s record: {record:?}"
        );
        found.unwrap()
    }

    /// The key of the client `id`, from `status --json`.
    pub(super) fn key_of(log: &TestLog, home: &Home, id: &str) -> TargetKey {
        let ran = status(log, home, &[]);
        let key = target(ran.envelope(), id)["key"]
            .as_str()
            .map(str::to_owned);
        assert!(key.is_some(), "{id} has no key: {}", ran.envelope());
        key.unwrap().parse().unwrap()
    }

    /// What a pending journal holds.
    #[derive(Clone, Copy)]
    pub(super) enum Planted {
        /// A valid `prepared` line: recovery finishes or rolls it back.
        Recoverable,
        /// A `prepared` line a newer major version started (`journal_too_new`).
        TooNew,
        /// A valid `prepared` line, then a complete line that does not decode
        /// (`journal_unreadable`).
        Unreadable,
        /// Only a line that does not decode: no `prepared`, so no target.
        Unattributed,
    }

    /// Writes a pending journal into the state directory, and returns its transaction.
    pub(super) fn plant_journal(
        log: &TestLog,
        home: &Home,
        key: TargetKey,
        planted: Planted,
    ) -> TxId {
        let paths = paths(&home.env);
        let tx = TxId::new();
        let schema_version = match planted {
            Planted::TooNew => SchemaVersion::new(2, 0),
            _ => journal::CURRENT,
        };
        let prepared = Prepared {
            schema_version,
            tx,
            target_key: key,
            client_dir: home.client("_retail_"),
            kind: TxKind::Install,
            started_at: Timestamp::now(),
            creates_dirs: Vec::new(),
            writes_wtf: false,
            staged_stamp: None,
            staged_files: None,
            source_backup: None,
            client: None,
            commit: CommitPayload {
                record: TargetRecord::empty(key),
                index: IndexChanges::default(),
                history: HistoryEntry {
                    schema_version: history::CURRENT,
                    tx,
                    kind: TxKind::Install,
                    at: Timestamp::now(),
                    target: Some(key),
                    targets: Vec::new(),
                    actor: Actor::Agent,
                    reason: None,
                    argv: vec!["omaddons".into()],
                    ops: Vec::new(),
                    result: TxResult::Committed,
                    reverts: None,
                    sv_deferred: false,
                    sv_restored: None,
                    created_addons_dir: false,
                    run: None,
                    backup: None,
                    plan: None,
                    doctor_run_id: None,
                    repairs: Vec::new(),
                },
                stage_dir: home.client("_retail_").join("Interface/.omaddons/stage/x"),
                sv_restore: None,
            },
        };
        let prepared = serde_json::to_string(&JournalLine::Prepared(Box::new(prepared))).unwrap();
        let bogus = r#"{"type":"bogus"}"#;
        let text = match planted {
            Planted::Recoverable | Planted::TooNew => format!("{prepared}\n"),
            Planted::Unreadable => format!("{prepared}\n{bogus}\n"),
            Planted::Unattributed => format!("{bogus}\n"),
        };
        fs::create_dir_all(paths.journal_dir()).unwrap();
        fs::write(paths.journal(&tx), text).unwrap();
        log.note(format!("planted journal {tx}"));
        tx
    }

    // ---------------------------------------------------------------------------------------
    // Reading `status`.

    pub(super) fn status(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
        let mut argv = vec!["status"];
        argv.extend_from_slice(args);
        argv.push("--json");
        log.run(home, &argv)
    }

    /// The client `id` of a `status` envelope.
    fn target<'a>(envelope: &'a Value, id: &str) -> &'a Value {
        let found = envelope["data"]["targets"]
            .as_array()
            .and_then(|targets| targets.iter().find(|t| t["id"] == id));
        assert!(found.is_some(), "no client {id}: {envelope}");
        found.unwrap()
    }

    /// The issues of the client `id`.
    pub(super) fn issues(envelope: &Value, id: &str) -> Value {
        target(envelope, id)["issues"].clone()
    }

    /// The issues of no client.
    pub(super) fn machine_issues(envelope: &Value) -> Value {
        envelope["data"]["issues"].clone()
    }

    pub(super) fn issue(
        id: &str,
        severity: &str,
        title: &str,
        addon: Option<&str>,
        remediation: Value,
    ) -> Value {
        json!({"id": id, "severity": severity, "title": title, "addon": addon, "remediation": remediation})
    }

    pub(super) fn prose(text: &str) -> Value {
        json!({"command": text, "argv": null, "destructive": false})
    }

    pub(super) fn command(argv: &[&str], destructive: bool) -> Value {
        json!({"command": argv.join(" "), "argv": argv, "destructive": destructive})
    }

    /// `omaddons install <addon> -t <target> --reinstall`, the repair of a drifted addon.
    pub(super) fn reinstall(addon: &str, target: &str) -> Value {
        command(
            &["omaddons", "install", addon, "-t", target, "--reinstall"],
            false,
        )
    }

    /// `actual` is exactly `expected`, logged first.
    #[track_caller]
    pub(super) fn assert_issues(log: &TestLog, label: &str, actual: &Value, expected: &Value) {
        log.json(&format!("expected issues: {label}"), expected);
        assert_eq!(actual, expected, "{label}");
    }

    /// Every issue of the envelope, on any client or none, is a registry row of the same
    /// severity: the ids come from the registry.
    #[track_caller]
    pub(super) fn assert_registry_ids(log: &TestLog, envelope: &Value) {
        let on_targets = envelope["data"]["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|t| t["issues"].as_array().into_iter().flatten());
        let on_machine = envelope["data"]["issues"].as_array().into_iter().flatten();
        for issue in on_targets.chain(on_machine) {
            let id = issue["id"].as_str().unwrap_or_default();
            let row = registry::row(id);
            assert!(row.is_some(), "{id} is no registry row");
            assert_eq!(
                serde_json::to_value(row.unwrap().severity).unwrap(),
                issue["severity"],
                "{id}"
            );
            log.note(format!("registry row {id}: {}", issue["severity"]));
        }
    }

    /// The human `status`, which must print `line`.
    #[track_caller]
    pub(super) fn assert_human_line(log: &TestLog, home: &Home, line: &str) {
        log.step("status (human)");
        let ran = log.run(home, &["status"]);
        ran.exit(0);
        assert!(
            ran.stdout().lines().any(|l| l == line),
            "no line {line:?} in:\n{}",
            ran.stdout()
        );
    }

    /// The trees `status` must leave as they are: state, cache, config and every client.
    pub(super) fn trees(home: &Home, clients: &[PathBuf]) -> Vec<(String, PathBuf)> {
        let env = &home.env;
        let mut out = vec![
            ("state".to_owned(), env.state().join("omaddons")),
            ("cache".to_owned(), env.cache().join("omaddons")),
            ("config".to_owned(), env.config().join("omaddons")),
            ("client retail".to_owned(), home.client("_retail_")),
        ];
        out.extend(
            clients
                .iter()
                .map(|dir| (format!("client {}", dir.display()), dir.clone())),
        );
        out
    }

    pub(super) fn record_trees(log: &TestLog, trees: &[(String, PathBuf)]) {
        for (label, root) in trees {
            log.tree(label, root);
        }
    }

    pub(super) fn assert_trees_unchanged(log: &TestLog, trees: &[(String, PathBuf)]) {
        for (label, root) in trees {
            log.assert_unchanged(label, root);
        }
    }
}

// ---------------------------------------------------------------------------------------
// The scenarios.

#[test]
fn journal_pending_is_one_issue_per_client_and_titled_by_its_journals() {
    let log = tlog!();
    log.step("build the home");
    let home = Home::new(&["wow"]);
    install(&log, &home, "retail", "Foo", "120105");
    let key = key_of(&log, &home, "retail");

    log.step("plant a recoverable journal");
    plant_journal(&log, &home, key, Planted::Recoverable);
    let trees = trees(&home, &[]);
    record_trees(&log, &trees);
    log.step("status --json");
    let ran = status(&log, &home, &[]);
    ran.ok();
    let waits = json!([issue(
        JOURNAL,
        "P0",
        "An interrupted change waits to be finished",
        None,
        prose(WAITS_PROSE)
    )]);
    assert_issues(
        &log,
        "one recoverable journal",
        &issues(ran.envelope(), "retail"),
        &waits,
    );
    let summary = &ran.data()["summary"];
    assert_eq!(
        (&summary["issues"]["p0"], &summary["state"]),
        (&json!(1), &json!("attention"))
    );
    assert_registry_ids(&log, ran.envelope());
    assert_trees_unchanged(&log, &trees);

    log.step("plant an unreadable journal");
    plant_journal(&log, &home, key, Planted::Unreadable);
    record_trees(&log, &trees);
    let manual = json!([issue(
        JOURNAL,
        "P0",
        "An interrupted change needs manual repair",
        None,
        prose(README_PROSE)
    )]);
    log.step("status --json");
    let ran = status(&log, &home, &[]);
    ran.ok();
    assert_issues(
        &log,
        "two journals, one unreadable",
        &issues(ran.envelope(), "retail"),
        &manual,
    );
    assert_eq!(ran.data()["journal"]["pending"], 2);
    assert_registry_ids(&log, ran.envelope());
    log.step("status --cached --json");
    let ran = status(&log, &home, &["--cached"]);
    ran.ok();
    assert_issues(
        &log,
        "two journals, --cached",
        &issues(ran.envelope(), "retail"),
        &manual,
    );
    assert_human_line(
        &log,
        &home,
        &format!("  P0 An interrupted change needs manual repair → {README_PROSE}"),
    );
    log.step("status --check");
    status(&log, &home, &["--check"]).fails(6, "check_failed");
    assert_trees_unchanged(&log, &trees);

    log.step("hold the lock");
    let held = GlobalLock::acquire(&paths(&home.env), "another process").unwrap();
    // Taking the lock writes its holder into the lock file: record after it.
    record_trees(&log, &trees);
    log.step("status --json");
    let ran = status(&log, &home, &[]);
    ran.ok();
    assert_issues(
        &log,
        "the lock held",
        &issues(ran.envelope(), "retail"),
        &json!([]),
    );
    assert_eq!(ran.data()["journal"]["in_progress"], true);
    assert_trees_unchanged(&log, &trees);
    log.step("release the lock");
    drop(held);
}

#[test]
fn an_unattributed_journal_is_the_machines_issue() {
    let log = tlog!();
    log.step("build the home");
    let home = Home::new(&["wow"]);
    install(&log, &home, "retail", "Foo", "120105");
    let key = key_of(&log, &home, "retail");
    log.step("plant a journal that names no client");
    plant_journal(&log, &home, key, Planted::Unattributed);
    let trees = trees(&home, &[]);
    record_trees(&log, &trees);

    log.step("status --json");
    let ran = status(&log, &home, &[]);
    ran.ok();
    let expected = json!([issue(
        JOURNAL,
        "P0",
        "An interrupted change needs manual repair",
        None,
        prose(README_PROSE)
    )]);
    assert_issues(
        &log,
        "the machine's",
        &machine_issues(ran.envelope()),
        &expected,
    );
    assert_issues(
        &log,
        "no client carries it",
        &issues(ran.envelope(), "retail"),
        &json!([]),
    );
    assert_eq!(ran.data()["needs_attention"], true, "{}", ran.envelope());
    assert_registry_ids(&log, ran.envelope());
    log.step("status --cached --json");
    let ran = status(&log, &home, &["--cached"]);
    ran.ok();
    assert_issues(
        &log,
        "the machine's, --cached",
        &machine_issues(ran.envelope()),
        &expected,
    );
    log.step("status --check");
    status(&log, &home, &["--check"]).fails(6, "check_failed");
    assert_trees_unchanged(&log, &trees);
}

#[test]
fn a_newer_record_is_state_version_newer() {
    let log = tlog!();
    log.step("build the home");
    let home = Home::new(&["wow"]);
    install(&log, &home, "retail", "Foo", "120105");
    let key = key_of(&log, &home, "retail");
    log.step("plant a record a newer omaddons wrote");
    let path = paths(&home.env).target_record(&key);
    let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value["schema_version"] = json!("9.0");
    fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
    let trees = trees(&home, &[]);
    record_trees(&log, &trees);

    let newer = issue(
        "fm-schemas-state-version-newer",
        "P0",
        "State was written by a newer omaddons",
        None,
        prose("upgrade omaddons"),
    );
    for (step, args) in [
        ("status --json", &[][..]),
        ("status --cached --json", &["--cached"][..]),
    ] {
        log.step(step);
        let ran = status(&log, &home, args);
        ran.ok();
        let found = issues(ran.envelope(), "retail");
        log.json(&format!("expected issues: {step} includes"), &newer);
        assert!(
            found.as_array().is_some_and(|all| all.contains(&newer)),
            "{step}: {found}"
        );
        assert_registry_ids(&log, ran.envelope());
    }
    log.step("status --check");
    status(&log, &home, &["--check"]).fails(6, "check_failed");
    assert_trees_unchanged(&log, &trees);
}

#[test]
fn a_missing_folder_is_record_folder_missing_in_both_modes() {
    let log = tlog!();
    log.step("build the home");
    let home = Home::new(&["wow"]);
    let foo = install(&log, &home, "retail", "Foo", "120105");
    log.step("plant a missing folder");
    fs::remove_dir_all(home.addons("_retail_").join("Foo")).unwrap();
    let trees = trees(&home, &[]);
    record_trees(&log, &trees);

    let expected = json!([issue(
        "fm-state_files-record-folder-missing",
        "P1",
        &format!("{foo}'s folder is gone"),
        Some(&foo),
        reinstall(&foo, "retail"),
    )]);
    for (step, args) in [
        ("status --json", &[][..]),
        ("status --cached --json", &["--cached"][..]),
    ] {
        log.step(step);
        let ran = status(&log, &home, args);
        ran.ok();
        assert_issues(&log, step, &issues(ran.envelope(), "retail"), &expected);
        assert_registry_ids(&log, ran.envelope());
    }
    assert_human_line(
        &log,
        &home,
        &format!("  P1 {foo}'s folder is gone → omaddons install {foo} -t retail --reinstall"),
    );
    log.step("status --check");
    status(&log, &home, &["--check"]).fails(6, "check_failed");
    assert_trees_unchanged(&log, &trees);
}

#[test]
fn modified_files_show_in_plain_status_only() {
    let log = tlog!();
    log.step("build the home");
    let home = Home::new(&["wow"]);
    let foo = install(&log, &home, "retail", "Foo", "120105");
    log.step("plant an edited file");
    edit_managed(
        &home.addons("_retail_").join("Foo/Foo.toc"),
        b"## Interface: 120105\n## Title: edited\n",
    );
    let trees = trees(&home, &[]);
    record_trees(&log, &trees);

    log.step("status --json");
    let ran = status(&log, &home, &[]);
    ran.ok();
    let modified = json!([issue(
        "fm-external_artifacts-managed-files-modified",
        "P2",
        &format!("{foo}'s files changed on disk (1 file)"),
        Some(&foo),
        reinstall(&foo, "retail"),
    )]);
    assert_issues(
        &log,
        "plain status",
        &issues(ran.envelope(), "retail"),
        &modified,
    );
    assert_registry_ids(&log, ran.envelope());
    log.step("status --cached --json");
    let ran = status(&log, &home, &["--cached"]);
    ran.ok();
    assert_issues(
        &log,
        "--cached checks presence only",
        &issues(ran.envelope(), "retail"),
        &json!([]),
    );
    assert_human_line(
        &log,
        &home,
        &format!(
            "  P2 {foo}'s files changed on disk (1 file) → omaddons install {foo} -t retail --reinstall"
        ),
    );
    assert_trees_unchanged(&log, &trees);
}

#[test]
fn a_forced_takeover_is_ownership_transferred() {
    let log = tlog!();
    log.step("build the home");
    let home = Home::new(&["wow"]);
    install(&log, &home, "retail", "Foo", "120105");
    take_over(&log, &home, "Foo");
    let trees = trees(&home, &[]);
    record_trees(&log, &trees);

    let transferred = issue(
        "fm-external_artifacts-ownership-transferred",
        "P2",
        "1 addon lost a folder to another addon",
        None,
        command(&["omaddons", "list", "-t", "retail", "-v"], false),
    );
    for (step, args) in [
        ("status --json", &[][..]),
        ("status --cached --json", &["--cached"][..]),
    ] {
        log.step(step);
        let ran = status(&log, &home, args);
        ran.ok();
        let found = issues(ran.envelope(), "retail");
        log.json(&format!("expected issues: {step} includes"), &transferred);
        assert!(
            found
                .as_array()
                .is_some_and(|all| all.contains(&transferred)),
            "{step}: {found}"
        );
        assert_registry_ids(&log, ran.envelope());
    }
    assert_trees_unchanged(&log, &trees);
}

#[test]
fn a_vanished_client_is_target_vanished() {
    let log = tlog!();
    log.step("build the home");
    let home = Home::new(&["wow"]);
    let legacy = CustomRoot::at(home.env.home().join("Games/wow-335"))
        .version(3, 3, 5, 12_340)
        .blizzard_folders(23)
        .build();
    let ran = status(&log, &home, &[]);
    ran.ok();
    let id = ran.data()["targets"]
        .as_array()
        .and_then(|targets| targets.iter().find(|t| t["family"] == "wrath-legacy"))
        .and_then(|t| t["id"].as_str())
        .map(str::to_owned);
    assert!(id.is_some(), "no 3.3.5a client: {}", ran.envelope());
    let id = id.unwrap();
    install(&log, &home, &id, "Foo", "30300");

    log.step("plant a vanished client");
    let unplugged = home.env.root().join("unplugged");
    fs::rename(&legacy, &unplugged).unwrap();
    // The first scan after the move rewrites the discovery cache, derived data that is
    // not `status`'s to keep: settle it before recording the trees.
    log.note("one status run settles the discovery cache after the move");
    status(&log, &home, &[]).ok();
    let trees = trees(&home, &[unplugged]);
    record_trees(&log, &trees);

    let expected = json!([issue(
        "fm-userland_state-target-vanished",
        "P2",
        "The client's directory is gone",
        None,
        command(&["omaddons", "targets", "rm", &id], true),
    )]);
    for (step, args) in [
        ("status --json", &[][..]),
        ("status --cached --json", &["--cached"][..]),
    ] {
        log.step(step);
        let ran = status(&log, &home, args);
        ran.ok();
        assert_issues(&log, step, &issues(ran.envelope(), &id), &expected);
        assert_registry_ids(&log, ran.envelope());
    }
    assert_human_line(
        &log,
        &home,
        &format!("  P2 The client's directory is gone → omaddons targets rm {id}"),
    );
    assert_trees_unchanged(&log, &trees);
}

#[test]
fn an_outdated_widget_is_widget_version_drift() {
    let log = tlog!();
    log.step("build the home");
    let home = Home::new(&["wow"]);
    let root = home.env.root().to_path_buf();
    for dir in [root.join("omarchy/shell"), root.join("no-tools")] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::create_dir_all(home.env.config().join("omarchy")).unwrap();
    let tools = root.join("no-tools");
    let omarchy = root.join("omarchy");
    // The first scan writes the discovery cache, derived data: settle it before the trees
    // are recorded, as the other scenarios' installs do.
    log.note("one status run writes the discovery cache");
    status(&log, &home, &[]).ok();
    let run = |args: &[&str]| -> Ran {
        log.cmd(&home, args)
            .env("PATH", &tools)
            .env("OMARCHY_PATH", &omarchy)
            .run()
    };
    log.step("install the widget");
    run(&["omarchy", "install", "--json"]).ok();
    log.step("plant another build's widget");
    let plugin = home.env.config().join("omarchy/plugins/omaddons");
    let code = fs::read_dir(&plugin)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.is_dir())
        .unwrap();
    let build_js = code.join("Build.js");
    let stamped: Vec<String> = fs::read_to_string(&build_js)
        .unwrap()
        .lines()
        .map(|line| {
            if line.starts_with("var CONTENT_SHA256 = ") {
                format!("var CONTENT_SHA256 = \"{}\";", "0".repeat(64))
            } else {
                line.to_owned()
            }
        })
        .collect();
    fs::write(&build_js, stamped.join("\n") + "\n").unwrap();
    let mut trees = trees(&home, &[]);
    trees.push((
        "omarchy config".to_owned(),
        home.env.config().join("omarchy"),
    ));
    record_trees(&log, &trees);

    let expected = json!([issue(
        "fm-plugins-widget-version-drift",
        "P3",
        "The bar widget is from another omaddons build",
        None,
        command(&["omaddons", "omarchy", "install"], false),
    )]);
    for (step, args) in [
        ("status --json", &[][..]),
        ("status --cached --json", &["--cached"][..]),
    ] {
        log.step(step);
        let mut argv = vec!["status"];
        argv.extend_from_slice(args);
        argv.push("--json");
        let ran = run(&argv);
        ran.ok();
        assert_issues(&log, step, &machine_issues(ran.envelope()), &expected);
        let summary = &ran.data()["summary"];
        assert_eq!(
            (&summary["issues"]["p3"], &summary["widget_outdated"]),
            (&json!(1), &json!(true)),
            "{step}"
        );
        assert_registry_ids(&log, ran.envelope());
    }
    assert_trees_unchanged(&log, &trees);
}

#[test]
fn several_states_rank_by_severity_id_and_addon() {
    let log = tlog!();
    log.step("build the home");
    let home = Home::new(&["wow"]);
    let aaa = install(&log, &home, "retail", "Aaa", "120105");
    let foo = install(&log, &home, "retail", "Foo", "120105");
    install(&log, &home, "retail", "Baz", "120105");
    take_over(&log, &home, "Baz");
    let key = key_of(&log, &home, "retail");

    log.step("plant a missing folder, an edited file and a too-new journal");
    fs::remove_dir_all(home.addons("_retail_").join("Aaa")).unwrap();
    edit_managed(
        &home.addons("_retail_").join("Foo/Foo.toc"),
        b"## Interface: 120105\n## Title: edited\n",
    );
    // Last: a too-new journal blocks every later change on the client.
    plant_journal(&log, &home, key, Planted::TooNew);
    let trees = trees(&home, &[]);
    record_trees(&log, &trees);

    log.step("status --json");
    let ran = status(&log, &home, &[]);
    ran.ok();
    let expected = json!([
        issue(
            JOURNAL,
            "P0",
            "An interrupted change needs manual repair",
            None,
            prose(README_PROSE)
        ),
        issue(
            "fm-state_files-record-folder-missing",
            "P1",
            &format!("{aaa}'s folder is gone"),
            Some(&aaa),
            reinstall(&aaa, "retail"),
        ),
        issue(
            "fm-external_artifacts-managed-files-modified",
            "P2",
            &format!("{foo}'s files changed on disk (1 file)"),
            Some(&foo),
            reinstall(&foo, "retail"),
        ),
        issue(
            "fm-external_artifacts-ownership-transferred",
            "P2",
            "1 addon lost a folder to another addon",
            None,
            command(&["omaddons", "list", "-t", "retail", "-v"], false),
        ),
    ]);
    assert_issues(&log, "ranked", &issues(ran.envelope(), "retail"), &expected);
    let summary = &ran.data()["summary"];
    assert_eq!(
        summary["issues"],
        json!({"p0": 1, "p1": 1, "p2": 2, "p3": 0})
    );
    assert_eq!(summary["state"], "attention");
    assert_registry_ids(&log, ran.envelope());

    log.step("status (human)");
    let human = log.run(&home, &["status"]);
    human.exit(0);
    let lines = [
        format!("  P0 An interrupted change needs manual repair → {README_PROSE}"),
        format!("  P1 {aaa}'s folder is gone → omaddons install {aaa} -t retail --reinstall"),
        format!(
            "  P2 {foo}'s files changed on disk (1 file) → omaddons install {foo} -t retail --reinstall"
        ),
        "  P2 1 addon lost a folder to another addon → omaddons list -t retail -v".to_owned(),
    ];
    let positions: Vec<Option<usize>> = lines
        .iter()
        .map(|line| human.stdout().lines().position(|l| l == line))
        .collect();
    assert!(
        positions.iter().all(Option::is_some) && positions.windows(2).all(|p| p[0] < p[1]),
        "{lines:#?} in order in:\n{}",
        human.stdout()
    );
    assert_trees_unchanged(&log, &trees);
    let requests = net_log(&home).len();
    assert_eq!(
        requests, 0,
        "every install was local, and status asks no source"
    );
}
