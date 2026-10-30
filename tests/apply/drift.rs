//! D7's drift scenarios for every storable command (omad-m4-agent-surface-doctor-c6cm.4.7.2):
//! a concurrent change between a preview and `apply` stales the stored plan, with the
//! part of the token that moved, and changes nothing; and once a direct run did the
//! plan's work, the typed bound run of the old token and `apply` of the old plan are both
//! refused, while a no-op preview stores no plan and its own token's run is accepted
//! (omad-m4-agent-surface-doctor-c6cm.4.7.3:) and each command's plan lives its whole
//! lifecycle (`common::plans::lifecycle`), a missing input stales its plan, and a
//! vanished working directory stales nothing.

use std::fs;
use std::path::{Path, PathBuf};

use omaddons::app::precondition::preview_argv;
use omaddons::backup::BackupKind;
use serde_json::{Value, json};

use crate::common::archives::zip_text;
use crate::common::backups::seed_backup;
use crate::common::home::Home;
use crate::common::plans::{
    LifecycleCase, Second, committed, history_txs_for, lifecycle, plan_id, read_plan,
};
use crate::common::privacy::ACCOUNT;
use crate::common::state::record;
use crate::common::tlog::{Ran, TestLog, tlog};
use crate::common::trees::tree_hash;
use crate::drift_support::{
    Built, Changed, FOO_V1, built, built_undo_played, published, run, stale, stale_with_fix,
};

/// A command's concurrent changes: each case and the `details.changed` its stale `apply`
/// must carry (`Contains` for the engine commands, whose witness may name more parts;
/// `Exactly` for M3's four, whose bindings `apply` runs unchanged).
fn cases(command: &str) -> &'static [(&'static str, Changed)] {
    use Changed::{Contains, Exactly};
    match command {
        "install" | "update" | "pin" | "unpin" => &[("commit", Contains(&["targets"]))],
        "adopt" => &[
            ("commit", Contains(&["targets"])),
            ("folder_touched", Contains(&["observed"])),
        ],
        "sync" => &[
            ("commit", Contains(&["targets"])),
            ("file_edited", Contains(&["observed"])),
        ],
        "copy" => &[
            ("destination", Contains(&["targets"])),
            ("source", Contains(&["observed"])),
        ],
        "remove" => &[("commit", Exactly(&["targets"]))],
        "undo" => &[
            ("commit", Exactly(&["targets"])),
            ("played", Exactly(&["observed"])),
        ],
        "backup restore" => &[
            ("commit", Exactly(&["targets"])),
            ("played", Exactly(&["observed"])),
            ("backup_rewritten", Exactly(&["observed"])),
        ],
        "backup prune" => &[
            ("new_backup", Exactly(&["selection", "observed"])),
            ("victim_rewritten", Exactly(&["observed"])),
        ],
        other => panic!("no cases for {other}"), // ubs:ignore — a table bug
    }
}

/// The commands that have nothing to do once a direct run did their work, and preview
/// that as a no-op (`remove` of an absent addon is `not_found`, `undo` of an undone
/// transaction is refused, and a whole restore always exchanges `WTF/`).
const NOOP_PATH: &[&str] = &[
    "install",
    "update",
    "pin",
    "unpin",
    "adopt",
    "sync",
    "copy",
    "backup prune",
];

// ---------------------------------------------------------------------------------------
// The concurrent change.

fn concurrent(log: &TestLog, command: &str) {
    let cases = cases(command);
    for (case, changed) in cases {
        if cases.len() == 1 {
            concurrent_case(log, command, case, *changed);
        } else {
            concurrent_case(&log.case(case), command, case, *changed);
        }
    }
}

fn concurrent_case(log: &TestLog, command: &str, case: &str, changed: Changed) {
    let built = if command == "undo" && case == "played" {
        built_undo_played(log)
    } else {
        built(log, command)
    };

    log.step("preview");
    let previewed = run(log, &built, &built.preview_with(&["--dry-run"]));
    previewed.ok();
    let id = plan_id(&previewed);
    let plan = read_plan(&built.home().env, &id);
    log.json("plan file", &plan);

    log.step("change");
    let put_back = change(log, &built, command, case, &previewed);
    built.record_trees(log);
    let plans = built.plan_files();
    log.json("plan files", &plans);
    let lines = built.history_lines();
    log.json("history lines", &lines);

    log.step("apply the stale plan");
    let ran = run(log, &built, &["apply", &id]);
    stale(log, &ran, &plan, changed);
    built.assert_trees_unchanged(log);
    log.assert_eq("plan files unchanged", &built.plan_files(), &plans);
    log.assert_eq("history unchanged", &built.history_lines(), &lines);

    log.step("preview again (the fix)");
    let put_back = match put_back {
        Some((path, bytes)) => {
            log.note("Foo/core.lua written back to its preview bytes");
            fs::write(path, bytes).unwrap();
            true
        }
        None => false,
    };
    let fix: Vec<String> =
        serde_json::from_value(ran.envelope()["error"]["fix_argv"].clone()).unwrap();
    let again = run(log, &built, &fix[1..]);
    again.ok();
    let fresh = plan_id(&again);
    if put_back {
        // The folder is as the preview saw it again: the same intent and token reuse the
        // plan, and the refusal was not sticky.
        log.assert_eq("the same plan", &fresh, &id);
    } else {
        assert_ne!(fresh, id, "the fix stores a new plan");
    }

    log.step("apply the fresh plan");
    let applied = run(log, &built, &["apply", &fresh]);
    applied.ok();
    log.json("outcomes", &applied.data()["outcomes"]);
    let statuses: Vec<&str> = applied.data()["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| o["status"].as_str().unwrap_or_default())
        .collect();
    assert!(!statuses.is_empty(), "no outcome: {}", applied.data());
    log.assert_eq(
        "every target committed",
        &statuses,
        &vec!["committed"; statuses.len()],
    );
    let commits = committed(&applied);
    let history = history_txs_for(log, built.home(), &fresh);
    if command == "backup prune" {
        log.assert_eq("a prune writes no transaction", &history, &Vec::new());
    } else {
        log.assert_eq("history's lines carrying the plan", &history, &commits);
    }
}

/// Makes the case's change between the preview and `apply`; returns a file and the
/// bytes to write back before the fix's preview, when the case edited one.
fn change(
    log: &TestLog,
    built: &Built,
    command: &str,
    case: &str,
    previewed: &Ran,
) -> Option<(PathBuf, Vec<u8>)> {
    let bar = built.bar();
    let install_bar = |target: &str| {
        run(
            log,
            built,
            &[
                "install",
                "gh:o/bar",
                "--file",
                &bar,
                "-t",
                target,
                "--offline",
            ],
        )
        .ok();
    };
    let client = built.home().client("_retail_");
    let backups = client.join("Interface/.omaddons/backups");
    let rewrite = |path: PathBuf, from: &str, to: &str| {
        let text = fs::read_to_string(&path).unwrap();
        let edited = text.replace(from, to);
        assert_ne!(edited, text, "{} holds {from}", path.display());
        fs::write(path, edited).unwrap();
    };
    match (command, case) {
        (_, "commit") => install_bar("retail"),
        ("copy", "destination") => install_bar("classic"),
        ("copy", "source") => {
            run(log, built, &["pin", "foo", "-t", "retail"]).ok();
        }
        ("adopt", "folder_touched") => {
            let path = built.addons("_retail_").join("Foo/core.lua");
            let bytes = fs::read(&path).unwrap();
            log.note("appended `-- touched` to <retail AddOns>/Foo/core.lua");
            let mut touched = bytes.clone();
            touched.extend_from_slice(b"-- touched\n");
            fs::write(&path, touched).unwrap();
            return Some((path, bytes));
        }
        ("sync", "file_edited") => {
            let path = built.home().env.config().join("omaddons/addons.toml");
            log.note("appended `# edited` to <config>/omaddons/addons.toml");
            let mut text = fs::read_to_string(&path).unwrap();
            text.push_str("# edited\n");
            fs::write(path, text).unwrap();
        }
        ("undo", "played") => {
            log.note("a play session rewrote Foo's SavedVariables");
            built.on.scene().play(&["Foo"]);
        }
        ("backup restore", "played") => {
            log.note("a play session rewrote the account's Foo.lua");
            fs::write(
                client.join(ACCOUNT).join("Foo.lua"),
                "Foo = { played = true }",
            )
            .unwrap();
        }
        ("backup restore", "backup_rewritten") => {
            log.note("the backup's backup.json gained a reason");
            let id = &built.preview[2];
            rewrite(
                backups.join(id).join("backup.json"),
                "\"actor\"",
                "\"reason\": \"checked\", \"actor\"",
            );
        }
        ("backup prune", "new_backup") => {
            log.note("an automatic backup seeded at 2026-10-04");
            let at = "2026-10-04T08:00:00Z".parse().unwrap();
            seed_backup(&client, BackupKind::Auto, at, 1);
        }
        ("backup prune", "victim_rewritten") => {
            let victim = previewed.data()["targets"][0]["pruned"][0]["id"]
                .as_str()
                .unwrap()
                .to_owned();
            log.note("the victim's backup.json says verified, as a verify records");
            rewrite(
                backups.join(victim).join("backup.json"),
                "metadata_only",
                "verified",
            );
        }
        other => panic!("no change for {other:?}"), // ubs:ignore — a table bug
    }
    None
}

// ---------------------------------------------------------------------------------------
// The stale bound no-op.

fn noop(log: &TestLog, command: &str) {
    let built = built(log, command);

    log.step("preview");
    let previewed = run(log, &built, &built.preview_with(&["--dry-run"]));
    previewed.ok();
    let id = plan_id(&previewed);
    let token = previewed.data()["precondition"]
        .as_str()
        .unwrap()
        .to_owned();
    let plan = read_plan(&built.home().env, &id);
    log.json("plan file", &plan);

    log.step("the same change, made directly");
    run(log, &built, &built.preview).ok();
    built.record_trees(log);
    let plans = built.plan_files();
    log.json("plan files", &plans);
    let lines = built.history_lines();

    log.step("typed run of the stale token");
    let typed = run(log, &built, &built.preview_with(&["--if-match", &token]));
    let mut own = vec!["omaddons".to_owned()];
    own.extend(built.preview.iter().cloned());
    refused(log, &typed, command, &preview_argv(&own));
    built.assert_trees_unchanged(log);

    log.step("apply the stale plan");
    let applied = run(log, &built, &["apply", &id]);
    let display: Vec<String> = serde_json::from_value(plan["display_argv"].clone()).unwrap();
    refused(log, &applied, command, &preview_argv(&display));
    let reason = applied.envelope()["error"]["details"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        !matches!(reason.as_str(), "already_applied"),
        "the direct run's line carries no plan, so apply is never already_applied"
    );
    built.assert_trees_unchanged(log);
    log.assert_eq("plan files unchanged", &built.plan_files(), &plans);
    log.assert_eq("history unchanged", &built.history_lines(), &lines);

    if !NOOP_PATH.contains(&command) {
        return;
    }
    log.step("a no-op preview");
    let quiet = run(log, &built, &built.preview_with(&["--dry-run"]));
    quiet.ok();
    log.assert_eq("no plan", &quiet.data()["plan_id"], &Value::Null);
    log.assert_eq("no expiry", &quiet.data()["plan_expires_at"], &Value::Null);
    let hints: Vec<String> = quiet.commands().into_iter().map(|c| c.action).collect();
    log.assert_eq(
        "no apply hint",
        &hints.contains(&"apply".to_owned()),
        &false,
    );
    log.assert_eq("no plan stored", &built.plan_files(), &plans);
    let fresh = quiet.data()["precondition"].as_str().unwrap().to_owned();
    log.json("the no-op's token", &json!(fresh));
    let bound = run(log, &built, &built.preview_with(&["--if-match", &fresh]));
    bound.exit(0);
    built.assert_trees_unchanged(log);
}

/// A stale run after the direct one: `plan_stale` with `fix` (its preview), or, for
/// `remove`, `not_managed` (exit 3): it resolves its names before its binding, and a name
/// no managed addon answers to is `not_managed`.
#[track_caller]
fn refused(log: &TestLog, ran: &Ran, command: &str, fix: &[String]) {
    if command == "remove" {
        ran.fails(3, "not_managed");
    } else {
        stale_with_fix(log, ran, fix, Changed::Contains(&[]));
    }
}

// ---------------------------------------------------------------------------------------
// The lifecycle.

/// The addon named `name` on `target`, as its record has it.
fn foo_record(built_home: &Home, target: &str) -> Option<omaddons::model::AddonRecord> {
    record(built_home, target)
        .addons
        .into_values()
        .find(|addon| addon.name == "Foo")
}

/// Each backup of retail and when it was captured.
fn backups_captured(client: &Path) -> Vec<(String, String)> {
    let dir = client.join("Interface/.omaddons/backups");
    let mut backups: Vec<(String, String)> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("backup.json").is_file())
        .map(|path| {
            let meta: Value =
                serde_json::from_slice(&fs::read(path.join("backup.json")).unwrap()).unwrap();
            let id = path.file_name().unwrap().to_string_lossy().into_owned();
            (
                id,
                meta["captured_at"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    backups.sort();
    backups
}

fn lifecycle_row(log: &TestLog, command: &str) {
    let built = built(log, command);
    let home = built.home();
    let client = home.client("_retail_");
    let before = if command == "backup prune" {
        backups_captured(&client)
    } else {
        Vec::new()
    };
    let case = LifecycleCase {
        label: command,
        preview: built.preview.clone(),
        roots: built.roots.clone(),
        env: built.env.clone(),
        progress_json: command == "backup restore",
        second: if command == "backup prune" {
            Second::StaleSelection
        } else {
            Second::AlreadyApplied
        },
    };
    let done = lifecycle(log, &built.on, &case);
    log.json("lifecycle", &json!({"plan": done.plan_id, "txs": done.txs}));

    log.step(&format!("{command}: its effect"));
    let version = |target: &str| foo_record(home, target).map(|foo| foo.version);
    let pinned = || foo_record(home, "retail").map(|foo| foo.pinned);
    match command {
        "install" | "sync" => {
            log.assert_eq("Foo v1 on retail", &version("retail"), &Some("v1".into()))
        }
        "update" => log.assert_eq("Foo v2 on retail", &version("retail"), &Some("v2".into())),
        "copy" => log.assert_eq("Foo v1 on classic", &version("classic"), &Some("v1".into())),
        "pin" => log.assert_eq("Foo pinned", &pinned(), &Some(true)),
        "unpin" => log.assert_eq("Foo unpinned", &pinned(), &Some(false)),
        "adopt" => log.assert_eq(
            "Foo managed on retail",
            &foo_record(home, "retail").is_some(),
            &true,
        ),
        "remove" | "undo" => {
            log.assert_eq(
                "no Foo folder",
                &built.addons("_retail_").join("Foo").exists(),
                &false,
            );
            log.assert_eq(
                "no Foo record",
                &foo_record(home, "retail").is_none(),
                &true,
            );
        }
        "backup restore" => {
            let id = &built.preview[2];
            let source = client
                .join("Interface/.omaddons/backups")
                .join(id)
                .join("WTF");
            log.assert_eq(
                "WTF/ is the backup's",
                &tree_hash(&client.join("WTF")),
                &tree_hash(&source),
            );
        }
        "backup prune" => {
            let oldest: Vec<(String, String)> = before
                .iter()
                .filter(|(_, at)| at.starts_with("2026-10-01"))
                .cloned()
                .collect();
            log.assert_eq("one backup of 2026-10-01", &oldest.len(), &1);
            let expected: Vec<(String, String)> = before
                .iter()
                .filter(|backup| !oldest.contains(backup))
                .cloned()
                .collect();
            log.assert_eq(
                "the oldest automatic backup gone, the other three kept",
                &backups_captured(&client),
                &expected,
            );
        }
        other => panic!("no effect for {other}"), // ubs:ignore — a table bug
    }
}

/// `Foo`'s files written as an unpacked folder at `dir` (`Foo.toc`, `core.lua`).
fn foo_folder(dir: &Path) {
    for (rel, text) in FOO_V1 {
        let path = dir.join(rel.trim_start_matches("Foo/"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

/// `apply` of a plan whose local input is gone: `input_missing`, naming the input, with
/// nothing changed.
#[test]
fn drift_lifecycle_input_missing() {
    let log = tlog!();
    for name in ["file_dir", "file_ref"] {
        let log = log.case(name);
        log.step("seed home");
        let home = published(&log, &["wow"]);
        let inputs = home.env.root().join("inputs");
        let (input, preview) = if name == "file_dir" {
            let dir = inputs.join("Foo");
            foo_folder(&dir);
            let dir_arg = dir.display().to_string();
            (
                dir,
                vec![
                    "install".to_owned(),
                    "gh:o/foo".into(),
                    "--file".into(),
                    dir_arg,
                ],
            )
        } else {
            let zip = inputs.join("foo.zip");
            fs::create_dir_all(&inputs).unwrap();
            fs::write(&zip, zip_text(FOO_V1)).unwrap();
            let file_ref = format!("file:{}", zip.display());
            (zip, vec!["install".to_owned(), file_ref])
        };
        let mut argv = preview;
        argv.extend(["-t", "retail", "--dry-run", "--json"].map(str::to_owned));

        log.step("preview");
        let previewed = log.run(&home, &argv);
        previewed.ok();
        let id = plan_id(&previewed);
        log.json("plan file", &read_plan(&home.env, &id));
        let absolute = input.canonicalize().unwrap().display().to_string();
        let roots = [
            ("retail AddOns", home.addons("_retail_")),
            ("state targets", home.state().join("targets")),
        ];
        for (label, root) in &roots {
            log.tree(label, root);
        }
        let lines = history_lines(&home);

        log.step("delete the input");
        if input.is_dir() {
            fs::remove_dir_all(&input).unwrap();
        } else {
            fs::remove_file(&input).unwrap();
        }
        log.note(format!("deleted {absolute}"));

        log.step("apply");
        let ran = log.run(&home, &["apply", &id, "--json"]);
        ran.fails(1, "plan_stale").assert_private();
        let details = &ran.envelope()["error"]["details"];
        log.json("details", details);
        log.assert_eq("input missing", &details["reason"], &json!("input_missing"));
        log.assert_eq("the input's path", &details["path"], &json!(absolute));
        for (label, root) in &roots {
            log.assert_unchanged(label, root);
        }
        log.assert_eq("history unchanged", &history_lines(&home), &lines);
    }
}

fn history_lines(home: &Home) -> usize {
    fs::read_to_string(home.state().join("history.jsonl"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// Every string in `value`, recursively.
fn strings_in(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => vec![text.clone()],
        Value::Array(items) => items.iter().flat_map(strings_in).collect(),
        Value::Object(map) => map.values().flat_map(strings_in).collect(),
        _ => Vec::new(),
    }
}

/// A plan previewed from a working directory that is deleted before `apply` runs from
/// another: its intent holds absolute paths, so nothing goes stale. The inputs live
/// beside the working directory and are named relative to it.
#[test]
fn drift_lifecycle_cwd_gone() {
    let log = tlog!();
    for name in ["sync_file", "file_dir", "client_path"] {
        let log = log.case(name);
        log.step("seed home");
        let home = published(&log, &["wow"]);
        let root = home.env.root();
        let (work, elsewhere, inputs) = (
            root.join("work"),
            root.join("elsewhere"),
            root.join("inputs"),
        );
        for dir in [&work, &elsewhere, &inputs] {
            fs::create_dir_all(dir).unwrap();
        }
        let (args, input): (Vec<String>, PathBuf) = match name {
            "sync_file" => {
                let file = inputs.join("addons.toml");
                fs::write(
                    &file,
                    "schema_version = \"1.0\"\n[family.mainline]\naddons = [\"gh:o/foo\"]\n",
                )
                .unwrap();
                (vec!["sync".into(), "../inputs/addons.toml".into()], file)
            }
            "file_dir" => {
                let dir = inputs.join("Foo");
                foo_folder(&dir);
                let args = [
                    "install",
                    "gh:o/foo",
                    "--file",
                    "../inputs/Foo",
                    "-t",
                    "retail",
                ];
                (args.map(str::to_owned).to_vec(), dir)
            }
            _ => {
                let zip = inputs.join("foo.zip");
                fs::write(&zip, zip_text(FOO_V1)).unwrap();
                let client = home.client("_retail_");
                let relative = format!("../{}", client.strip_prefix(root).unwrap().display());
                let zip_arg = zip.display().to_string();
                let args = ["install", "gh:o/foo", "--file", &zip_arg, "-t", &relative];
                (args.map(str::to_owned).to_vec(), client)
            }
        };

        log.step("preview from the working directory");
        let mut argv = args.clone();
        argv.extend(["--dry-run", "--json"].map(str::to_owned));
        let previewed = log.cmd(&home, &argv).cwd(&work).run();
        previewed.ok();
        let id = plan_id(&previewed);
        let intent = read_plan(&home.env, &id)["intent"].clone();
        log.json("intent", &intent);
        let texts = strings_in(&intent);
        if name == "client_path" {
            // `install` stores the client its relative path resolved to, not the path.
            log.assert_eq("the client it named", &intent["target"], &json!("retail"));
        } else {
            let absolute = input.canonicalize().unwrap().display().to_string();
            log.assert_eq(
                "the input, absolute, in the intent",
                &texts.contains(&absolute),
                &true,
            );
        }
        log.assert_eq(
            "no relative path in the intent",
            &texts
                .iter()
                .any(|text| text.starts_with("./") || text.starts_with("../")),
            &false,
        );

        log.step("delete the working directory");
        fs::remove_dir_all(&work).unwrap();
        log.note("deleted <root>/work");

        log.step("apply from another directory");
        let ran = log
            .cmd(&home, &["apply", &id, "--json"])
            .cwd(&elsewhere)
            .run();
        ran.ok().assert_private();
        log.json("outcomes", &ran.data()["outcomes"]);
        log.assert_eq("committed", &committed(&ran).len(), &1);
        log.assert_eq(
            "Foo installed on retail",
            &foo_record(&home, "retail").is_some(),
            &true,
        );
    }
}

// ---------------------------------------------------------------------------------------
// One test per command and scenario, named by the command.

macro_rules! rows {
    ($scenario:ident: $($test:ident => $command:literal),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                let log = tlog!();
                $scenario(&log, $command);
            }
        )*
    };
}

rows!(concurrent:
    drift_concurrent_install => "install",
    drift_concurrent_update => "update",
    drift_concurrent_pin => "pin",
    drift_concurrent_unpin => "unpin",
    drift_concurrent_adopt => "adopt",
    drift_concurrent_sync => "sync",
    drift_concurrent_copy => "copy",
    drift_concurrent_remove => "remove",
    drift_concurrent_undo => "undo",
    drift_concurrent_backup_restore => "backup restore",
    drift_concurrent_backup_prune => "backup prune",
);

rows!(noop:
    drift_noop_install => "install",
    drift_noop_update => "update",
    drift_noop_pin => "pin",
    drift_noop_unpin => "unpin",
    drift_noop_adopt => "adopt",
    drift_noop_sync => "sync",
    drift_noop_copy => "copy",
    drift_noop_remove => "remove",
    drift_noop_undo => "undo",
    drift_noop_backup_restore => "backup restore",
    drift_noop_backup_prune => "backup prune",
);

rows!(lifecycle_row:
    drift_lifecycle_install => "install",
    drift_lifecycle_update => "update",
    drift_lifecycle_pin => "pin",
    drift_lifecycle_unpin => "unpin",
    drift_lifecycle_adopt => "adopt",
    drift_lifecycle_sync => "sync",
    drift_lifecycle_copy => "copy",
    drift_lifecycle_remove => "remove",
    drift_lifecycle_undo => "undo",
    drift_lifecycle_backup_restore => "backup restore",
    drift_lifecycle_backup_prune => "backup prune",
);
