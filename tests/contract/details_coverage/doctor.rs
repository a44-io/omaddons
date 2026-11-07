//! Details coverage rows: the doctor (`doctor`, `doctor fix`, `doctor undo`, the
//! test-only fixers), stuck journals and the fences they hold (E14), recovery that fails,
//! state files a build cannot read, config parse errors, `robot-docs install-skill`
//! refusals and the bar widget's validation.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

use omaddons::error::ErrorCode;

use super::super::support::{Checked, Scene, check};
use super::Row;
use crate::common::doctor::plant;
use crate::common::doctor::test_fixers::{
    ENV, FLATTEN, Kind, QUARANTINE, Side, WRITE, plant_flatten, plant_quarantine, plant_write,
    register, test_dir,
};
use crate::common::home::Home;
use crate::common::repairs::{
    Child, FixSpec, doctor_run, journal_dir, plant_newer_major, plant_undecodable_prepared,
    run_child,
};
use crate::common::tlog::TestLog;

pub const ROWS: &[Row] = &[
    OVER_BUDGET,
    ONLINE_ONLY,
    OVERLAPPING_FIXES,
    FLATTEN_WHILE_RUNNING,
    TOO_MANY_ACTIONS,
    REPAIR_CHANGED_SINCE,
    REACTIVATION_REFUSED,
    CONFIG_OUT_OF_RANGE,
    STATE_TOO_NEW,
    CLOSED_FENCES,
    BEHIND_MARKER_MISMATCH,
    BEHIND_GAME_WAIT,
    BEHIND_STEP_UNRECOGNIZED,
    RECOVERY_FAILED,
    INSTALL_SKILL_REFUSED,
    WIDGET_INVALID,
];

/// `omaddons <args> --json` on `home` with `env`, and this home's own Omarchy
/// (`OMARCHY_PATH`), so a doctor run never reads the machine's.
fn doctor(home: &Home, args: &[&str], env: &[(&str, &str)], code: i32) -> Checked {
    let omarchy = plant::omarchy_path(home).display().to_string();
    let mut env = env.to_vec();
    env.push(("OMARCHY_PATH", &omarchy));
    check(home.exec_with(&json_argv(args), &env), code)
}

/// `args` with `--json`.
fn json_argv(args: &[&str]) -> Vec<String> {
    args.iter()
        .copied()
        .chain(["--json"])
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------------------
// The doctor's own refusals.

/// A doctor run whose estimate exceeds `--budget` is refused before it runs.
const OVER_BUDGET: Row = Row {
    id: "a_doctor_run_over_its_budget_is_refused",
    keys: &[
        (ErrorCode::DoctorRefused, "budget_ms"),
        (ErrorCode::DoctorRefused, "estimated_ms"),
        (ErrorCode::DoctorRefused, "reason"),
    ],
    run: || {
        let s = Scene::new();
        vec![doctor(&s.home, &["doctor", "--budget", "1ms"], &[], 4)]
    },
};

/// `doctor --only` naming only an online check, without `--online`.
const ONLINE_ONLY: Row = Row {
    id: "doctor_only_online_checks_need_online",
    keys: &[(ErrorCode::OnlineRequired, "fms")],
    run: || {
        let s = Scene::new();
        let only = ["doctor", "--only", "fm-network-source-unreachable"];
        vec![doctor(&s.home, &only, &[], 6)]
    },
};

/// A directory and its child, planned by two test fixers: refused together.
const OVERLAPPING_FIXES: Row = Row {
    id: "overlapping_fixes_are_refused_together",
    keys: &[
        (ErrorCode::DoctorRefused, "fixes"),
        (ErrorCode::DoctorRefused, "fixes[].fm"),
        (ErrorCode::DoctorRefused, "fixes[].instance"),
        (ErrorCode::DoctorRefused, "reason"),
        (ErrorCode::DoctorRefused, "resources"),
    ],
    run: || {
        let home = Home::new(&["wow"]);
        plant_quarantine(&home, Side::State, "box.quarantine", Kind::Dir);
        plant_write(&home, "box.quarantine/item.write", b"broken\n");
        let only = format!("{QUARANTINE},{WRITE}");
        let fix = ["doctor", "fix", "--only", &only, "--force", "--yes"];
        vec![doctor(&home, &fix, &[ENV], 4)]
    },
};

/// The test flatten fixer, refused while its client runs: the refusal names the
/// failure mode.
const FLATTEN_WHILE_RUNNING: Row = Row {
    id: "a_flatten_is_refused_while_its_client_runs",
    keys: &[
        (ErrorCode::DoctorRefused, "fm"),
        (ErrorCode::DoctorRefused, "reason"),
    ],
    run: || {
        let mut home = Home::new(&["wow"]);
        register(&home, "retail");
        plant_flatten(&home, "_retail_", "TestWrapA", "Inner");
        home.retail_running();
        vec![doctor(
            &home,
            &["doctor", "fix", "--only", FLATTEN],
            &[ENV],
            4,
        )]
    },
};

/// One more `fm-test-write` subject than `DOCTOR_FIX_MAX_ACTIONS` (one write each): the
/// repair is refused before anything moves, comparing the two counts.
const TOO_MANY_ACTIONS: Row = Row {
    id: "a_repair_of_more_operations_than_one_run_may_make_is_refused",
    keys: &[
        (ErrorCode::DoctorRefused, "actions"),
        (ErrorCode::DoctorRefused, "fm"),
        (ErrorCode::DoctorRefused, "max_actions"),
        (ErrorCode::DoctorRefused, "reason"),
    ],
    run: || {
        let home = Home::new(&["wow"]);
        let dir = test_dir(&home, Side::State);
        fs::create_dir_all(&dir).unwrap();
        for n in 0..=omaddons::limits::DOCTOR_FIX_MAX_ACTIONS {
            fs::write(dir.join(format!("{n}.write")), b"broken\n").unwrap();
        }
        vec![doctor(
            &home,
            &["doctor", "fix", "--only", WRITE],
            &[ENV],
            4,
        )]
    },
};

// ---------------------------------------------------------------------------------------
// Undoing repairs.

/// The undo of a test repair whose file was edited since: refused, naming the path.
const REPAIR_CHANGED_SINCE: Row = Row {
    id: "the_undo_of_a_repair_whose_file_changed_since_is_refused",
    keys: &[
        (ErrorCode::UndoConflict, "paths"),
        (ErrorCode::UndoConflict, "reason"),
        (ErrorCode::UndoConflict, "tx"),
    ],
    run: || {
        let s = Scene::new();
        let file = plant_write(&s.home, "a.write", b"broken\n");
        let fixed = doctor(&s.home, &["doctor", "fix", "--only", WRITE], &[ENV], 0);
        let tx = fixed.envelope["tx"].as_str().unwrap().to_owned();
        fs::write(&file, b"edited\n").unwrap();
        let refused = s.json(&["undo", &tx], 4);
        vec![fixed, refused]
    },
};

/// A journal stuck at an unrecognized step on retail, set aside, then the folder its step
/// renames into `AddOns/` made by hand: its reactivation (`doctor undo <run>`) is
/// refused, naming the guard that changed and the target.
const REACTIVATION_REFUSED: Row = Row {
    id: "a_set_aside_is_not_reactivated_after_a_folder_it_guards_changed",
    keys: &[
        (ErrorCode::UndoConflict, "changed_guards"),
        (ErrorCode::UndoConflict, "reason"),
        (ErrorCode::UndoConflict, "target"),
        (ErrorCode::UndoConflict, "tx"),
    ],
    run: || {
        let home = Home::new(&["wow", "wow_classic"]);
        plant::journal_step_unrecognized(&home, "retail");
        let fm = "fm-concurrency_primitives-journal-step-unrecognized";
        let set_aside = doctor(
            &home,
            &[
                "doctor", "fix", "--only", fm, "-t", "retail", "--force", "--yes",
            ],
            &[],
            0,
        );
        let run = set_aside.envelope["run_id"].as_str().unwrap().to_owned();
        let folder = home.addons("_retail_").join("Unrecognized");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("Unrecognized.lua"), "-- played with\n").unwrap();
        let refused = doctor(&home, &["doctor", "undo", &run], &[], 4);
        vec![set_aside, refused]
    },
};

// ---------------------------------------------------------------------------------------
// State a build cannot read.

/// A `config.toml` value out of range: the refusal names its file, line and key.
const CONFIG_OUT_OF_RANGE: Row = Row {
    id: "a_config_value_out_of_range_names_its_toml_key",
    keys: &[
        (ErrorCode::StateCorrupt, "column"),
        (ErrorCode::StateCorrupt, "line"),
        (ErrorCode::StateCorrupt, "path"),
        (ErrorCode::StateCorrupt, "toml_key"),
    ],
    run: || {
        let s = Scene::new();
        let config = plant::config_toml(&s.home);
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "[limits]\ndownload_max_bytes = 0\n").unwrap();
        vec![s.json(&["status"], 1)]
    },
};

/// A target record of a newer schema, on retail, and a journal a newer omaddons started,
/// on Mists Classic: each refuses its own client's change.
const STATE_TOO_NEW: Row = Row {
    id: "a_record_and_a_journal_of_a_newer_omaddons_refuse_their_clients_changes",
    keys: &[
        (ErrorCode::StateTooNew, "path"),
        (ErrorCode::StateTooNew, "reason"),
        (ErrorCode::StateTooNew, "schema_version"),
        (ErrorCode::StateTooNew, "target"),
    ],
    run: || {
        let s = Scene::new();
        s.install_foo();
        let key = crate::common::state::key_of(&s.home, "retail");
        let record = s.home.state().join(format!("targets/{key}.json"));
        let mut doc: serde_json::Value =
            serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        doc["schema_version"] = serde_json::json!("1.99");
        fs::write(&record, serde_json::to_vec(&doc).unwrap()).unwrap();
        let foo2 = s.zip("foo2");
        let record_too_new = s.json(&["install", "gh:o/foo", "--file", &foo2, "-t", "retail"], 4);
        plant::journal_too_new(&s.home, "classic");
        let both = s.zip("both");
        let journal_too_new = s.json(
            &["install", "gh:o/both", "--file", &both, "-t", "classic"],
            4,
        );
        vec![record_too_new, journal_too_new]
    },
};

// ---------------------------------------------------------------------------------------
// Fences (E14).

/// A repair journal whose resources cannot be known closes every change, so `config set`
/// is refused with that journal's own stop (`resource` `journal:<tx>`, `target` null):
/// its `prepared` line of a newer major (`state_too_new`), one that does not decode
/// (`state_corrupt`), and the file unreadable (`io`, `unattributed`).
const CLOSED_FENCES: Row = Row {
    id: "a_repair_journal_whose_resources_cannot_be_known_refuses_every_change",
    keys: &[
        (ErrorCode::Io, "journal_pending"),
        (ErrorCode::Io, "reason"),
        (ErrorCode::Io, "resource"),
        (ErrorCode::Io, "target"),
        (ErrorCode::Io, "tx"),
        (ErrorCode::StateCorrupt, "journal_pending"),
        (ErrorCode::StateCorrupt, "reason"),
        (ErrorCode::StateCorrupt, "resource"),
        (ErrorCode::StateCorrupt, "target"),
        (ErrorCode::StateCorrupt, "tx"),
        (ErrorCode::StateTooNew, "journal_pending"),
        (ErrorCode::StateTooNew, "reason"),
        (ErrorCode::StateTooNew, "resource"),
        (ErrorCode::StateTooNew, "target"),
        (ErrorCode::StateTooNew, "tx"),
    ],
    run: || {
        let config_set = ["config", "set", "check_interval_hours", "6"];
        let newer = Scene::new();
        plant_newer_major(&newer.home);
        let undecodable = Scene::new();
        plant_undecodable_prepared(&undecodable.home);
        let sealed = Scene::new();
        let tx = plant_undecodable_prepared(&sealed.home);
        let journal = journal_dir(&sealed.home).join(format!("{tx}.repair.jsonl"));
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o000)).unwrap();
        vec![
            newer.json(&config_set, 4),
            undecodable.json(&config_set, 1),
            sealed.json(&config_set, 1),
        ]
    },
};

/// A fake token stored with `auth github set`: the fenced subject of [`fenced_behind`].
// ubs:ignore — a canary, not a credential
const TOKEN: &[u8] = b"ghp_COVERAGEc4n4ry0123456789\n";

/// `auth github set --json` with [`TOKEN`] on standard input.
fn set_token(home: &Home) -> crate::common::home::Run {
    home.exec_input(&json_argv(&["auth", "github", "set"]), &[], TOKEN)
}

/// The scene of [`fenced_behind`]: a log for its child repair, a home with a retail and
/// a Mists Classic client and [`TOKEN`] stored, then the stuck folder journal `plant`
/// leaves (its transaction).
fn behind_scene(id: &str, plant: fn(&Home) -> String) -> (TestLog, Home, String) {
    // Both the table's test and the dev test run every row: each run logs on its own.
    static RUN: AtomicUsize = AtomicUsize::new(0);
    let n = RUN.fetch_add(1, Ordering::SeqCst);
    let log = TestLog::new("contract", &format!("details_coverage.doctor.{id}.{n}"));
    let home = Home::new(&["wow", "wow_classic"]);
    let stored = set_token(&home);
    assert_eq!(stored.code, 0, "{}", stored.context());
    let tx = plant(&home);
    (log, home, tx)
}

/// A repair that would chmod the in-flight lock of the folder journal `tx` and the stored
/// token, crashed right after its `prepared` line (a child of this binary): what it
/// writes overlaps what `tx` protects, so recovery holds it behind `tx`, and the token is
/// fenced with `tx`'s own stop. `auth github set`, which runs no discovery (the game may
/// run, for all it knows), is then refused with that stop, exiting `code`: `tx`,
/// `journal_pending`, `target` and `reason` its, and the token its `resource`.
fn fenced_behind(log: &TestLog, home: &Home, tx: &str, code: i32) -> Checked {
    let credential = fs::canonicalize(plant::token_file(home)).unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o644)).unwrap();
    let lock = fs::canonicalize(home.state().join(format!("inflight/{tx}.lock"))).unwrap();
    let mode = fs::metadata(&lock).unwrap().permissions().mode() & 0o7777;
    let fixes = [
        FixSpec::Chmod {
            path: lock,
            mode: if mode == 0o600 { 0o640 } else { 0o600 },
        },
        FixSpec::Chmod {
            path: credential,
            mode: 0o600,
        },
    ];
    let run = doctor_run(home);
    let ended = run_child(home, log, &run, &[], &fixes, "doctor.repair.prepared=abort");
    assert!(matches!(ended, Child::Crashed), "{ended:?}");
    check(set_token(home), code)
}

/// A folder journal on retail whose `prepared` names Mists Classic's client dir (marker
/// mismatch): retail's own install is refused with it, and a repair held behind it is
/// fenced with its `scope_denied`.
const BEHIND_MARKER_MISMATCH: Row = Row {
    id: "a_repair_behind_a_journal_naming_another_clients_dir_is_fenced_with_its_stop",
    keys: &[
        (ErrorCode::ScopeDenied, "journal_pending"),
        (ErrorCode::ScopeDenied, "reason"),
        (ErrorCode::ScopeDenied, "resource"),
        (ErrorCode::ScopeDenied, "target"),
        (ErrorCode::ScopeDenied, "tx"),
    ],
    run: || {
        let (log, home, tx) = behind_scene(BEHIND_MARKER_MISMATCH.id, |home| {
            plant::journal_marker_mismatch(home, "retail", "classic").0
        });
        let zip = plant::local_zip(&home, "Probe");
        let install = [
            "install",
            "gh:o/probe",
            "--file",
            &zip,
            "-t",
            "retail",
            "--offline",
        ];
        let own = check(home.exec_with(&json_argv(&install), &[]), 4);
        vec![own, fenced_behind(&log, &home, &tx, 4)]
    },
};

/// A settings restore on retail killed after its exchange: a journal that waits for the
/// game, as a command that runs no discovery sees it. A repair held behind it is fenced
/// with its `game_running`.
const BEHIND_GAME_WAIT: Row = Row {
    id: "a_repair_behind_a_journal_waiting_for_the_game_is_fenced_with_its_stop",
    keys: &[
        (ErrorCode::GameRunning, "journal_pending"),
        (ErrorCode::GameRunning, "reason"),
        (ErrorCode::GameRunning, "resource"),
        (ErrorCode::GameRunning, "target"),
        (ErrorCode::GameRunning, "tx"),
    ],
    run: || {
        let (log, home, tx) = behind_scene(BEHIND_GAME_WAIT.id, |home| {
            plant::crashed_restore(home, "retail")
        });
        vec![fenced_behind(&log, &home, &tx, 4)]
    },
};

/// An install on retail killed before its first rename, its step then matching neither
/// state: a repair held behind it is fenced with its `internal` stop.
const BEHIND_STEP_UNRECOGNIZED: Row = Row {
    id: "a_repair_behind_a_journal_at_an_unrecognized_step_is_fenced_with_its_stop",
    keys: &[
        (ErrorCode::Internal, "journal_pending"),
        (ErrorCode::Internal, "reason"),
        (ErrorCode::Internal, "resource"),
        (ErrorCode::Internal, "target"),
        (ErrorCode::Internal, "tx"),
    ],
    run: || {
        let (log, home, tx) = behind_scene(BEHIND_STEP_UNRECOGNIZED.id, |home| {
            plant::journal_step_unrecognized(home, "retail").0
        });
        vec![fenced_behind(&log, &home, &tx, 1)]
    },
};

// ---------------------------------------------------------------------------------------
// Recovery that fails.

/// Foo 2 (with a new folder, Bar) installed over Foo 1, its commit failing after its
/// exchange and its rollback failing too: the error names the kept journal and the
/// recovery's own error.
const RECOVERY_FAILED: Row = Row {
    id: "a_commit_whose_recovery_fails_too_names_both_errors",
    keys: &[
        (ErrorCode::Io, "journal_pending"),
        (ErrorCode::Io, "path"),
        (ErrorCode::Io, "recovery_error"),
        (ErrorCode::Io, "recovery_error.code"),
        (ErrorCode::Io, "recovery_error.message"),
    ],
    run: || {
        let s = Scene::new();
        s.install_foo();
        let v2bar = s.home.env.root().join("dl/v2bar.zip");
        let entries = [
            ("Foo/Foo.toc", "## Interface: 120105\n## Version: 2\n"),
            ("Foo/core.lua", "-- v2\n"),
            ("Bar/Bar.toc", "## Interface: 120105\n"),
        ];
        fs::write(&v2bar, crate::common::archives::zip_text(&entries)).unwrap();
        let v2bar = v2bar.display().to_string();
        let faults = "engine.swap.after_exchange=error,fsx.rename_with.before=error@3";
        vec![s.json_env(
            &["install", "gh:o/foo", "--file", &v2bar, "-t", "retail"],
            &[("OMADDONS_FAILPOINTS", faults)],
            1,
        )]
    },
};

// ---------------------------------------------------------------------------------------
// The skill and the bar widget.

/// `robot-docs install-skill` into a folder inside omaddons' state, then over a
/// symlinked `SKILL.md`.
const INSTALL_SKILL_REFUSED: Row = Row {
    id: "install_skill_refuses_omaddons_state_and_a_symlinked_skill",
    keys: &[
        (ErrorCode::ScopeDenied, "link_target"),
        (ErrorCode::ScopeDenied, "path"),
        (ErrorCode::ScopeDenied, "reason"),
    ],
    run: || {
        let s = Scene::new();
        let state = s.home.env.state().join("omaddons");
        fs::create_dir_all(&state).unwrap();
        let dir = state.join("skill").display().to_string();
        let inside = s.json(&["robot-docs", "install-skill", "--dir", &dir], 4);
        let home = s.home.env.home();
        let skill = home.join(".claude/skills/omaddons");
        fs::create_dir_all(&skill).unwrap();
        fs::create_dir_all(home.join("dotfiles")).unwrap();
        fs::write(home.join("dotfiles/SKILL.md"), b"my dotfiles copy\n").unwrap();
        std::os::unix::fs::symlink(home.join("dotfiles/SKILL.md"), skill.join("SKILL.md")).unwrap();
        let linked = s.json(&["robot-docs", "install-skill"], 4);
        vec![inside, linked]
    },
};

/// `omarchy install` over stub Omarchy tools whose `omarchy-plugin-validate` refuses the
/// rendered widget: a build bug, `internal`, with the validator's problems.
const WIDGET_INVALID: Row = Row {
    id: "a_widget_omarchy_refuses_is_internal_with_its_problems",
    keys: &[
        (ErrorCode::Internal, "problems"),
        (ErrorCode::Internal, "reason"),
    ],
    run: || {
        let home = Home::new(&["wow"]);
        let desk = crate::common::omarchy::stub_desk(&home.env);
        fs::write(home.env.root().join("validate-mode"), "bad\n").unwrap();
        let env: Vec<(&str, String)> = desk
            .env()
            .iter()
            .map(|(key, value)| (*key, value.display().to_string()))
            .collect();
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let install = home.exec_with(&json_argv(&["omarchy", "install"]), &env);
        vec![check(install, 1)]
    },
};
