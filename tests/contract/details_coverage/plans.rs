//! Details coverage rows: stored plans, `apply`, `undo`, `adopt`, `sync`, target and
//! argument selection, and the usage refusals.

use std::fs;

use omaddons::error::ErrorCode;
use serde_json::json;

use super::super::support::{Checked, Scene, check};
use super::Row;
use crate::common::addons::{toc, unpack};
use crate::common::home::{Home, wait};
use crate::common::pause::Pause;
use crate::common::plans::{edit_plan, plans_dir};
use crate::common::privacy::{ACCOUNT, CHARACTER};

pub const ROWS: &[Row] = &[
    MISPLACED_CLIENT,
    PLAN_LOOKUP,
    PLAN_LIFECYCLE,
    PLAN_LEASE,
    ADOPT_STALE,
    ADOPT_RIVALS,
    DESIRED_STATE,
    RUNNING_CLIENT,
    UNDO_CONFLICTS,
    UNDO_LOOKUP,
    TARGET_NOT_FOUND,
    TARGET_AMBIGUOUS,
    USAGE_REFUSALS,
];

/// A client named where the command belongs: the usage fix moves it into `-t`.
const MISPLACED_CLIENT: Row = Row {
    id: "a_client_named_where_the_command_belongs",
    keys: &[(ErrorCode::Usage, "command"), (ErrorCode::Usage, "target")],
    run: || {
        let s = Scene::new();
        vec![s.json(&["retail", "update"], 2)]
    },
};

/// `apply` with a flag its plan decides, an unknown id, a shared prefix, a damaged plan.
const PLAN_LOOKUP: Row = Row {
    id: "apply_looks_a_stored_plan_up_by_its_id_or_a_prefix",
    keys: &[
        (ErrorCode::PlanNotFound, "path"),
        (ErrorCode::PlanNotFound, "plan_id"),
        (ErrorCode::PlanNotFound, "prefix"),
        (ErrorCode::PlanNotFound, "reason"),
        (ErrorCode::RefAmbiguous, "candidates"),
        (ErrorCode::RefAmbiguous, "prefix"),
        (ErrorCode::Usage, "flag"),
    ],
    run: || {
        let s = Scene::new();
        let id = preview(&s, "gh:o/foo", "foo");
        // A damaged plan whose id shares 20 characters with the good one's.
        let dir = plans_dir(&s.home.env);
        let twin = format!("{}ZZZZZZ", &id[..20]);
        let bytes = fs::read(dir.join(format!("{id}.json"))).unwrap();
        fs::write(dir.join(format!("{twin}.json")), &bytes[..bytes.len() / 2]).unwrap();
        vec![
            s.json(&["apply", &id, "-t", "retail"], 2),
            s.json(&["apply", "01ZZZZZZZZZZZZZZZZZZZZZZZZ"], 3),
            s.json(&["apply", &id[..10]], 3),
            s.json(&["apply", &twin], 3),
        ]
    },
};

/// `apply` of another build's plan, of one whose zip is gone, and of one applied already.
const PLAN_LIFECYCLE: Row = Row {
    id: "apply_refuses_a_plan_that_can_no_longer_apply",
    keys: &[
        (ErrorCode::PlanStale, "changed"),
        (ErrorCode::PlanStale, "path"),
        (ErrorCode::PlanStale, "reason"),
        (ErrorCode::PlanStale, "txs"),
    ],
    run: || {
        let s = Scene::new();
        // One input per plan: the same dry run twice stores one plan.
        let other_build = preview(&s, "gh:o/both", "both");
        let gone = preview(&s, "gh:o/foo", "foo2");
        let applied = preview(&s, "gh:o/foo", "foo");
        edit_plan(&s.home.env, &other_build, |plan| {
            plan["build_id"] = json!("0000000000000000");
        });
        fs::remove_file(s.zip("foo2")).unwrap();
        vec![
            s.json(&["apply", &other_build], 1),
            s.json(&["apply", &gone], 1),
            s.json(&["apply", &applied], 0),
            s.json(&["apply", &applied], 1),
        ]
    },
};

/// A second `apply` of a plan a paused `apply` holds: it waits out the 30 s lease.
const PLAN_LEASE: Row = Row {
    id: "a_second_apply_waits_out_the_lease_and_is_locked",
    keys: &[
        (ErrorCode::Locked, "holder"),
        (ErrorCode::Locked, "holder.age_s"),
        (ErrorCode::Locked, "holder.command"),
        (ErrorCode::Locked, "holder.pid"),
        (ErrorCode::Locked, "holder.started_at"),
        (ErrorCode::Locked, "lease"),
    ],
    run: || {
        let s = Scene::new();
        let id = preview(&s, "gh:o/foo", "foo");
        let argv = ["apply", id.as_str()];
        // The holder stops inside its commit, the plan's lease held, until resumed.
        let pause = Pause::new(s.home.env.root());
        let holder = s.home.spawn(
            &argv,
            &[("OMADDONS_FAILPOINTS", "engine.swapped=pause"), pause.env()],
        );
        pause.reached("engine.swapped");
        let second = s.home.run(&argv);
        // Resumed before any check, so a failure never leaves the holder paused.
        pause.resume("engine.swapped");
        let first = wait(holder, &argv);
        vec![check(second, 5), check(first, 0)]
    },
};

/// `adopt --apply` while a folder it planned is edited before the commit lock.
const ADOPT_STALE: Row = Row {
    id: "a_folder_edited_between_planning_and_the_lock_is_plan_stale",
    keys: &[
        (ErrorCode::PlanStale, "cause"),
        (ErrorCode::PlanStale, "cause_message"),
        (ErrorCode::PlanStale, "changed"),
    ],
    run: || {
        let home = Home::new(&["wow"]);
        catalog(&home, "Foo", "gh:o/foo", &["Foo", "Foo_Options"]);
        let addons = home.addons("_retail_");
        let foo = toc("120105", &["## Title: Foo", "## Version: v1"]);
        let options = toc("120105", &["## Dependencies: Foo"]);
        unpack(
            &addons,
            &[
                ("Foo/Foo.toc", &foo),
                ("Foo/core.lua", "-- foo v1\n"),
                ("Foo_Options/Foo_Options.toc", &options),
            ],
        );
        let mut out = vec![check(home.run(&["status"]), 0)];
        let argv = ["adopt", "--apply", "--no-verify"];
        let pause = Pause::new(home.env.root());
        let child = home.spawn(
            &argv,
            &[
                ("OMADDONS_FAILPOINTS", "engine.plan.done=pause"),
                pause.env(),
            ],
        );
        // Planned, so the folder is hashed; the edit lands before the lock.
        pause.reached("engine.plan.done");
        fs::write(addons.join("Foo/core.lua"), "-- edited after indexing\n").unwrap();
        pause.resume("engine.plan.done");
        out.push(check(wait(child, &argv), 1));
        out
    },
};

/// `adopt` naming a folder two WoWInterface projects claim: a guess with a rival.
const ADOPT_RIVALS: Row = Row {
    id: "a_named_folder_two_projects_claim_is_ref_ambiguous",
    keys: &[
        (ErrorCode::RefAmbiguous, "folder"),
        (ErrorCode::RefAmbiguous, "refs"),
    ],
    run: || {
        use crate::common::wowi::{FileEntry, wowi_filelist};
        let home = Home::new(&["wow"]);
        wowi_filelist(
            &home.http,
            &[
                FileEntry::new(100, "Bagnon Bags", &["Bagnon"]),
                FileEntry::new(200, "Bagnon Classic", &["Bagnon"]),
            ],
        );
        let other = toc("120105", &["## Title: Other", "## Author: Nobody"]);
        unpack(&home.addons("_retail_"), &[("Bagnon/Bagnon.toc", &other)]);
        vec![check(home.run(&["adopt", "Bagnon", "--no-verify"]), 3)]
    },
};

/// `sync` with no `addons.toml`, a mistyped value, a list entry twice, one addon twice.
const DESIRED_STATE: Row = Row {
    id: "sync_names_the_lines_of_addons_toml_it_refuses",
    keys: &[
        (ErrorCode::DesiredStateInvalid, "first_line"),
        (ErrorCode::DesiredStateInvalid, "line"),
        (ErrorCode::DesiredStateInvalid, "other_line"),
        (ErrorCode::DesiredStateInvalid, "path"),
        (ErrorCode::DesiredStateInvalid, "reason"),
        (ErrorCode::DesiredStateInvalid, "toml_key"),
        (ErrorCode::NotFound, "path"),
    ],
    run: || {
        let s = Scene::new();
        let mut out = vec![s.json(&["sync"], 3)];
        s.home
            .write_desired("[family.mainline]\naddons = \"weakauras\"\n");
        out.push(s.json(&["sync"], 2));
        // The parser refuses one list naming an entry twice: the file fails.
        s.home
            .write_desired("[family.mainline]\naddons = [\"gh:o/foo\",\n  \"gh:o/foo\"]\n");
        out.push(s.json(&["sync"], 2));
        // Two entries written differently, naming one addon: each item fails, so the run.
        s.home.write_desired(concat!(
            "[family.mainline]\naddons = [\"gh:o/foo\"]\n\n",
            "[[family.mainline.addon]]\nname = \"Foo\"\nref = \"https://github.com/o/foo\"\n",
        ));
        out.push(s.json(&["sync", "-t", "retail"], 2));
        out
    },
};

/// A running client: a removal, an undo whose settings wait, its retries, no process table.
const RUNNING_CLIENT: Row = Row {
    id: "a_running_client_refuses_what_it_would_see_change",
    keys: &[
        // The removal's one item failed: the items, each error with its kept details.
        (ErrorCode::GameRunning, "items"),
        (ErrorCode::GameRunning, "items[].error"),
        (ErrorCode::GameRunning, "items[].error.code"),
        (ErrorCode::GameRunning, "items[].error.details"),
        (ErrorCode::GameRunning, "items[].error.fix"),
        (ErrorCode::GameRunning, "items[].error.fix_argv"),
        (ErrorCode::GameRunning, "items[].error.message"),
        (ErrorCode::GameRunning, "items[].request"),
        (ErrorCode::GameRunning, "items[].target"),
        (ErrorCode::GameRunning, "ops"),
        (ErrorCode::GameRunning, "ops[].files"),
        (ErrorCode::GameRunning, "ops[].folder"),
        (ErrorCode::GameRunning, "ops[].op"),
        (ErrorCode::GameRunning, "reason"),
        (ErrorCode::GameRunning, "running"),
        (ErrorCode::GameRunning, "target"),
        (ErrorCode::UndoConflict, "tx"),
        (ErrorCode::UndoConflict, "undone_by"),
    ],
    run: || {
        let mut s = Scene::new();
        let client = s.home.client("_retail_");
        for (dir, text) in [(ACCOUNT, "-- before"), (CHARACTER, "-- char before")] {
            fs::create_dir_all(client.join(dir)).unwrap();
            fs::write(client.join(dir).join("Foo.lua"), text).unwrap();
        }
        let foo = s.install_foo();
        let both = s.zip("both");
        s.json(
            &["install", "gh:o/both", "--file", &both, "-t", "retail"],
            0,
        );
        // Played: the game rewrote Foo's settings, and still runs.
        fs::write(client.join(ACCOUNT).join("Foo.lua"), "-- after").unwrap();
        s.home.retail_running();
        let mut out = vec![
            s.json(&["remove", "gh:o/both", "-t", "retail"], 4),
            // Folders revert; the SavedVariables wait for the client to exit.
            s.json(&["undo", &foo, "--while-running"], 0),
            s.json(&["undo", &foo], 4),
            s.json(&["undo", &foo, "--saved-variables", "restore"], 4),
        ];
        // A process table that cannot be listed: a file where its directory should be.
        let unlistable = s.home.env.root().join("proc-file");
        fs::write(&unlistable, "").unwrap();
        s.home.proc = Some(unlistable);
        out.push(s.json(&["remove", "gh:o/both", "-t", "retail"], 4));
        out
    },
};

/// Undo over a later update, a removed folder put back by hand, an unreadable newer line.
const UNDO_CONFLICTS: Row = Row {
    id: "undo_refuses_to_overwrite_what_came_after",
    keys: &[
        (ErrorCode::UndoConflict, "conflicting"),
        (ErrorCode::UndoConflict, "conflicting[].folders"),
        (ErrorCode::UndoConflict, "conflicting[].kind"),
        (ErrorCode::UndoConflict, "conflicting[].tx"),
        (ErrorCode::UndoConflict, "folder"),
        (ErrorCode::UndoConflict, "lines"),
        (ErrorCode::UndoConflict, "reason"),
        (ErrorCode::UndoConflict, "tx"),
    ],
    run: || {
        let s = Scene::new();
        let first = s.install_foo();
        let foo2 = s.zip("foo2");
        let update = ["install", "gh:o/foo", "--file", &foo2, "-t", "retail"];
        let update = tx_of(&s.json(&update, 0));
        let mut out = vec![s.json(&["undo", &first], 4)];
        let both = s.zip("both");
        s.json(
            &["install", "gh:o/both", "--file", &both, "-t", "retail"],
            0,
        );
        let removed = tx_of(&s.json(&["remove", "gh:o/both", "-t", "retail"], 0));
        let addons = s.home.addons("_retail_");
        unpack(&addons, &[("Both/Both.toc", "## Interface: 120105\n")]);
        out.push(s.json(&["undo", &removed], 4));
        // A newer line this build cannot read (an unknown op), which may have changed Foo.
        let key = crate::common::state::key_of(&s.home, "retail").to_string();
        let line = json!({
            "schema_version": "1.0", "tx": "01M5ZZZZZZZZZZZZZZZZZZZZZZ", "kind": "restore",
            "at": "2026-10-21T12:00:00Z", "target": key, "actor": "human",
            "reason": null, "argv": ["omaddons"], "result": "committed",
            "ops": [{"op": "merge", "folder": "Foo", "addon": "Foo"}],
        });
        append_history(&s.home, &line.to_string());
        out.push(s.json(&["undo", &update], 4));
        out
    },
};

/// Undo of a transaction whose snapshot is gone, and of a prefix two transactions share.
const UNDO_LOOKUP: Row = Row {
    id: "undo_names_what_it_cannot_find",
    keys: &[
        (ErrorCode::NotFound, "candidates"),
        (ErrorCode::NotFound, "reason"),
        (ErrorCode::NotFound, "tx"),
    ],
    run: || {
        let s = Scene::new();
        let tx = s.install_foo();
        fs::remove_dir_all(s.home.state().join("snapshots").join(&tx)).unwrap();
        let mut out = vec![s.json(&["undo", &tx], 3)];
        // The install's line again, under an id that shares its first 20 characters.
        let history = fs::read_to_string(s.home.state().join("history.jsonl")).unwrap();
        let line = history.lines().find(|l| l.contains(&tx)).unwrap();
        let twin = format!("{}ZZZZZZ", &tx[..20]);
        append_history(&s.home, &line.replace(&tx, &twin));
        out.push(s.json(&["undo", &tx[..10]], 3));
        out
    },
};

/// Selectors naming no client here: a missing mode's, an empty directory, a vanished one.
const TARGET_NOT_FOUND: Row = Row {
    id: "a_selector_that_names_no_client_here",
    keys: &[
        (ErrorCode::TargetNotFound, "game_mode"),
        (ErrorCode::TargetNotFound, "key"),
        (ErrorCode::TargetNotFound, "missing"),
        (ErrorCode::TargetNotFound, "path"),
        (ErrorCode::TargetNotFound, "product"),
        (ErrorCode::TargetNotFound, "selector"),
    ],
    run: || {
        let s = Scene::new();
        let empty = s.home.env.root().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let mut out = vec![
            s.json(&["status", "-t", "sod"], 3),
            s.json(&["targets", "add", &empty.display().to_string()], 3),
        ];
        // Registered by its first change, then moved away.
        s.install_foo();
        let retail = s.home.client("_retail_");
        fs::rename(&retail, retail.with_extension("gone")).unwrap();
        out.push(s.json(&["status", "-t", "retail"], 3));
        out
    },
};

/// Both clients where one is needed: `install -t all`, `copy --from all`, `adopt --ref`.
const TARGET_AMBIGUOUS: Row = Row {
    id: "a_selector_that_names_two_clients_where_one_is_needed",
    keys: &[
        (ErrorCode::TargetAmbiguous, "candidates"),
        (ErrorCode::TargetAmbiguous, "flag"),
        (ErrorCode::TargetAmbiguous, "folder"),
        (ErrorCode::TargetAmbiguous, "selector"),
    ],
    run: || {
        let s = Scene::new();
        // An unmanaged Foo on both clients: `--ref` lands on one.
        let foo = toc("120105, 50504", &["## Version: 1"]);
        for client in ["_retail_", "_classic_"] {
            unpack(&s.home.addons(client), &[("Foo/Foo.toc", &foo)]);
        }
        let both = s.zip("both");
        vec![
            s.json(&["install", "gh:o/both", "--file", &both, "-t", "all"], 3),
            s.json(&["copy", "--from", "all", "--to", "classic"], 3),
            s.json(&["adopt", "Foo", "--ref", "gh:o/foo", "--no-verify"], 3),
        ]
    },
};

/// Usage refusals naming their subject: pruning the newest checkpoint, a taken target id.
const USAGE_REFUSALS: Row = Row {
    id: "usage_names_the_backup_or_target_it_refuses",
    keys: &[(ErrorCode::Usage, "holder"), (ErrorCode::Usage, "id")],
    run: || {
        use crate::common::backups::seed_backup;
        use omaddons::backup::BackupKind;
        let s = Scene::new();
        let at = "2026-10-05T10:00:00Z".parse().unwrap();
        let manual = seed_backup(&s.home.client("_retail_"), BackupKind::Manual, at, 1);
        let manual = manual.to_string();
        let now = [("OMADDONS_NOW", "2026-10-15T18:30:00Z")];
        vec![
            s.json_env(&["backup", "prune", "-t", "retail", &manual], &now, 2),
            // `mine` is no family or product name, so only its holder refuses it.
            s.json(&["targets", "rename", "classic", "mine"], 0),
            s.json(&["targets", "rename", "retail", "mine"], 2),
        ]
    },
};

/// A dry run of installing the `zip` fixture as `r#ref` on retail: its stored plan's id.
fn preview(s: &Scene, r#ref: &str, zip: &str) -> String {
    let zip = s.zip(zip);
    let args = [
        "install",
        r#ref,
        "--file",
        &zip,
        "-t",
        "retail",
        "--dry-run",
    ];
    let c = s.json(&args, 0);
    c.envelope["data"]["plan_id"].as_str().unwrap().to_owned()
}

/// The transaction a change committed, as `data.tx` or its one target's `tx`.
fn tx_of(c: &Checked) -> String {
    let data = &c.envelope["data"];
    data["tx"]
        .as_str()
        .or_else(|| data["targets"][0]["tx"].as_str())
        .unwrap()
        .to_owned()
}

/// Appends `line` to `history.jsonl`.
fn append_history(home: &Home, line: &str) {
    let path = home.state().join("history.jsonl");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str(line);
    text.push('\n');
    fs::write(&path, text).unwrap();
}

/// A user catalog file naming one addon, for every family.
fn catalog(home: &Home, name: &str, r#ref: &str, folders: &[&str]) {
    let folders: Vec<String> = folders.iter().map(|f| format!("{f:?}")).collect();
    let text = format!(
        "schema_version = \"1.0\"\n\n[[addon]]\nname = {name:?}\nref = {ref:?}\nfolders = [{}]\n",
        folders.join(", ")
    );
    let dir = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("50-adopt.toml"), text).unwrap();
}
