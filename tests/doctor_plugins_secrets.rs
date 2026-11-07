//! e2e suite: the widget and credential failure modes, and the chmod repair
//!
//! F7 part 4 (omad-m4-agent-surface-doctor-c6cm.6.7.4): `fm-plugins-widget-version-drift`,
//! `fm-plugins-widget-invalid` and `fm-secrets-credential-file-mode` through the binary,
//! on their fixtures. Each widget row is found with its evidence, never quotes the
//! manifest, gives `omaddons omarchy install` (or prose for a widget serving another
//! state) and clears after that install on the stub desk; neither fires on a clean or a
//! current home. The credential row is found from a stat, repaired by one chmod on the
//! same inode with the same bytes, recorded as one `repair` history line, verified, and
//! a second run does nothing; its preview binds across processes; credentials outside
//! `<config>`, symlinked or another user's are reported with prose and never touched.
//! Detection is repeatable and pure, the fake token appears nowhere, and `status` reports
//! the three rows as the doctor finds them (F8).
#![cfg(test)]

use crate::common;
use crate::fixture;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use common::doctor::{check_envelope, plant};
use common::home::Home;
use common::netlog::net_log;
use common::omarchy::stub_desk;
use common::procfs::running_as_root;
use common::tlog::{Cmd, Ran, TestLog, tlog};

const DRIFT: &str = "fm-plugins-widget-version-drift";
const INVALID: &str = "fm-plugins-widget-invalid";
// ubs:ignore — a failure-mode id, not a secret
const MODE: &str = "fm-secrets-credential-file-mode";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn home(log: &TestLog) -> Home {
    let home = Home::new(&["wow"]);
    log.env(&home);
    home
}

/// `omaddons <args>` as given (`--json` among them), with `OMARCHY_PATH` set to
/// `plant::omarchy_path`, as every doctor suite's runs carry it.
fn cmd<'a>(log: &'a TestLog, home: &Home, args: &[&str]) -> Cmd<'a> {
    log.cmd(home, args)
        .env("OMARCHY_PATH", plant::omarchy_path(home))
}

/// The run's envelope checked against the doctor contract (a doctor run), and its output
/// private.
fn checked(ran: Ran) -> Ran {
    // ubs:ignore — an argv word, not a secret
    if ran.run.argv.first().is_some_and(|a| a == "doctor") && ran.run.code >= 0 {
        check_envelope(ran.stdout(), ran.stderr(), ran.run.code);
    }
    ran.assert_private();
    ran
}

fn run(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    checked(cmd(log, home, args).run())
}

/// `omaddons omarchy install --json` on the stub desk: `PATH` holding the stubs alone.
fn install_widget(log: &TestLog, home: &Home) -> Ran {
    let desk = stub_desk(&home.env);
    let mut install = cmd(log, home, &["omarchy", "install", "--json"]);
    for (key, value) in desk.env() {
        install = install.env(key, value);
    }
    let ran = checked(install.run());
    ran.ok();
    ran
}

/// The findings of `id` in an envelope.
fn found(envelope: &Value, id: &str) -> Vec<Value> {
    envelope["findings"]
        .as_array()
        .into_iter()
        .flatten()
        // ubs:ignore — a failure-mode id, not a secret
        .filter(|f| f["id"] == id)
        .cloned()
        .collect()
}

fn argv(words: &[&str]) -> Value {
    json!(words)
}

fn plugin(home: &Home) -> PathBuf {
    home.env.config().join("omarchy/plugins/omaddons")
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

/// What a test compares of a credential file: its mode, inode and bytes (as their
/// SHA-256, never the fake token itself).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct FileFacts {
    mode: String,
    ino: u64,
    sha256: String,
}

fn facts(log: &TestLog, label: &str, path: &Path) -> FileFacts {
    let meta = fs::metadata(path).unwrap();
    let facts = FileFacts {
        mode: format!("{:04o}", meta.permissions().mode() & 0o7777),
        ino: meta.ino(),
        sha256: sha(&fs::read(path).unwrap()),
    };
    log.json(label, &facts);
    facts
}

/// Every tree a "changes nothing" claim compares, one label each: `<config>`, `<cache>`,
/// the client dir, and each top-level directory of `<state>/omaddons` but `doctor/` (its
/// top-level files are compared by [`state_files`]).
fn trees(home: &Home) -> Vec<(String, PathBuf)> {
    let mut out = vec![
        ("config".to_owned(), home.env.config()),
        ("cache".to_owned(), home.env.cache()),
        ("_retail_".to_owned(), home.client("_retail_")),
    ];
    for name in state_entries(home) {
        let path = home.state().join(&name);
        if name != "doctor" && path.is_dir() {
            out.push((format!("state/{name}"), path));
        }
    }
    out
}

/// Each top-level file of `<state>/omaddons`: its mode and SHA-256.
fn state_files(home: &Home) -> Vec<(String, String)> {
    state_entries(home)
        .into_iter()
        .filter_map(|name| {
            let path = home.state().join(&name);
            path.is_file().then(|| {
                let digest = format!("{:04o} {}", mode_of(&path), sha(&fs::read(&path).unwrap()));
                (name, digest)
            })
        })
        .collect()
}

/// The top-level entries of `<state>/omaddons`, sorted (none when it is absent).
fn state_entries(home: &Home) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(home.state())
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort_unstable();
    names
}

/// What a "changes nothing" claim compares: the recorded trees, and `<state>/omaddons`'s
/// top-level entries and files.
struct Recorded {
    trees: Vec<(String, PathBuf)>,
    entries: Vec<String>,
    files: Vec<(String, String)>,
}

fn record_trees(log: &TestLog, home: &Home) -> Recorded {
    let trees = trees(home);
    for (label, root) in &trees {
        log.tree(label, root);
    }
    let files = state_files(home);
    log.json("state files", &files);
    Recorded {
        trees,
        entries: state_entries(home),
        files,
    }
}

/// Every recorded tree and state file is unchanged, and `<state>/omaddons` gained nothing
/// but `doctor/`.
fn assert_trees_unchanged(log: &TestLog, home: &Home, recorded: &Recorded) {
    for (label, root) in &recorded.trees {
        log.assert_unchanged(label, root);
    }
    log.assert_eq("state files", &state_files(home), &recorded.files);
    let extra: Vec<String> = state_entries(home)
        .into_iter()
        .filter(|name| name != "doctor" && !recorded.entries.contains(name))
        .collect();
    assert_eq!(extra, Vec::<String>::new(), "new state entries");
}

// ---------------------------------------------------------------------------------------
// The widget rows.

/// The evidence a widget fixture's finding carries.
fn widget_evidence(id: &str, home: &Home) -> Value {
    let path = plugin(home).display().to_string();
    if id == DRIFT {
        json!({"path": path, "observed": VERSION, "expected": VERSION, "reason": "code_differs"})
    } else {
        json!({"path": path, "count": 2, "rules": ["default_section", "symlink"]})
    }
}

const SKILL_DRIFT: &str = "fm-plugins-skill-version-drift";

/// The skill drift row (M4-D64) on its fixture, through the binary: an older guide in
/// Claude Code's default skill folder is one P3 finding about that file, outdated and not
/// edited, remediated by `robot-docs install-skill` (no `--dir`: the run's own default),
/// never auto-fixable; running that remedy clears it.
#[test]
fn the_skill_drift_fixture_is_found_then_cleared_by_install_skill() {
    let log = tlog!();
    let fx = fixture::fixtures::all()
        .into_iter()
        .find(|f| f.id() == SKILL_DRIFT)
        .expect("the skill drift fixture is registered");
    let home = home(&log);
    let file = home.env.home().join(".claude/skills/omaddons/SKILL.md");
    log.step("plant");
    fx.corrupt(&home);
    log.step("diagnose");
    let ran = run(&log, &home, &["doctor", "--json"]);
    ran.exit(1);
    let envelope = ran.envelope();
    let ours = found(envelope, SKILL_DRIFT);
    assert_eq!(ours.len(), 1, "{envelope:#}");
    let f = &ours[0];
    log.json("finding", f);
    assert_eq!(f["severity"], "P3");
    assert_eq!(f["target"], Value::Null);
    let instance = f["instance"].as_str().unwrap();
    assert!(
        instance.starts_with(&format!("{SKILL_DRIFT}@machine:"))
            && instance.ends_with(&file.display().to_string()),
        "{instance}"
    );
    let evidence = &f["evidence"];
    assert_eq!(evidence["path"], json!(file.display().to_string()));
    assert_eq!(evidence["reason"], "outdated");
    assert_eq!(evidence["edited"], false);
    assert_eq!(
        evidence["expected"],
        json!(format!(
            "sha256:{}",
            omaddons::contract::guide::EXPECTED_DIGEST
        ))
    );
    assert_ne!(evidence["observed"], evidence["expected"], "{evidence}");
    let remediation = &f["remediation"];
    assert_eq!(
        remediation["argv"],
        argv(&["omaddons", "robot-docs", "install-skill"])
    );
    assert_eq!(remediation["auto_fixable"], false);
    assert_eq!(remediation["destructive"], false);
    log.step("remedy");
    fx.remedy(&home);
    log.step("verify");
    fx.assert_healthy(&home);
}

#[test]
fn widget_fixtures_are_found_and_cleared() {
    let log = tlog!();
    let fixtures: Vec<_> = fixture::fixtures::all()
        .into_iter()
        .filter(|f| [INVALID, DRIFT].contains(&f.id()))
        .collect();
    let ids: Vec<&str> = fixtures.iter().map(|f| f.id()).collect();
    log.assert_eq("registered", &ids, &vec![INVALID, DRIFT]);
    for fx in fixtures {
        let id = fx.id();
        let log = log.case(id);
        let home = home(&log);
        log.step("plant");
        fx.corrupt(&home);
        log.step("diagnose");
        let ran = run(&log, &home, &["doctor", "--json"]);
        ran.exit(1);
        let envelope = ran.envelope();
        assert_eq!(envelope["state"], "DONE_FINDINGS");
        let ours = found(envelope, id);
        assert_eq!(ours.len(), 1, "{id}: {envelope:#}");
        let f = &ours[0];
        log.json("finding", f);
        let (severity, title, other) = if id == DRIFT {
            (
                "P3",
                "The bar widget is from another omaddons build",
                INVALID,
            )
        } else {
            (
                "P2",
                "The bar widget fails Omarchy's plugin validation (2 problems)",
                DRIFT,
            )
        };
        assert_eq!(f["severity"], severity, "{id}");
        assert_eq!(f["title"], title, "{id}");
        assert_eq!(f["instance"], format!("{id}@machine"), "{id}");
        assert_eq!(f["target"], Value::Null, "{id}");
        log.assert_eq("evidence", &f["evidence"], &widget_evidence(id, &home));
        let remediation = &f["remediation"];
        assert_eq!(
            remediation["argv"],
            argv(&["omaddons", "omarchy", "install"])
        );
        assert_eq!(remediation["auto_fixable"], false, "{id}");
        assert_eq!(remediation["destructive"], false, "{id}");
        assert_eq!(
            found(envelope, other),
            Vec::<Value>::new(),
            "{id}: no {other}"
        );
        // Never a validator message, which quotes the manifest's values.
        assert!(!ran.stdout().contains("\"top\""), "{id}: {}", ran.stdout());
        assert!(!ran.stdout().contains("must be left"), "{id}");
        log.step("remedy");
        install_widget(&log, &home);
        log.step("verify");
        fx.assert_healthy(&home);
        let only = format!("{INVALID},{DRIFT}");
        let ran = run(&log, &home, &["doctor", "--json", "--only", &only]);
        ran.ok();
        assert_eq!(ran.envelope()["findings"], json!([]), "{id}");
    }

    // A widget without its `Build.js`: drift that cannot say which state it serves, and
    // a bare manifest that fails validation too.
    let log = log.case("no_build_js");
    let home = home(&log);
    log.step("plant");
    plant::widget_version_drift(&home);
    log.step("diagnose");
    let ran = run(&log, &home, &["doctor", "--json"]);
    ran.exit(1);
    let envelope = ran.envelope();
    let drift = found(envelope, DRIFT);
    assert_eq!(drift.len(), 1, "{envelope:#}");
    log.assert_eq(
        "drift evidence",
        &drift[0]["evidence"],
        &json!({"path": plugin(&home).display().to_string(), "observed": "0.0.9", "expected": VERSION, "reason": "context_unknown"}),
    );
    let invalid = found(envelope, INVALID);
    assert_eq!(invalid.len(), 1, "{envelope:#}");
    log.assert_eq(
        "invalid rules",
        &invalid[0]["evidence"]["rules"],
        &json!([
            "entry_points_not_object",
            "field_missing",
            "kinds_empty",
            "schema_version"
        ]),
    );
    assert_eq!(
        invalid[0]["evidence"]["count"], 6,
        "three fields and three rules"
    );
    log.step("remedy");
    install_widget(&log, &home);
    log.step("verify");
    let only = format!("{INVALID},{DRIFT}");
    run(&log, &home, &["doctor", "--json", "--only", &only]).ok();
}

#[test]
fn a_widget_serving_another_state_gets_prose() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    let desk = plant::widget_invalid(&home);
    plant::serve_another_state(&desk.plugin());
    log.step("diagnose");
    let ran = run(&log, &home, &["doctor", "--json"]);
    ran.exit(1);
    let envelope = ran.envelope();
    let ours = found(envelope, INVALID);
    assert_eq!(ours.len(), 1, "{envelope:#}");
    let f = &ours[0];
    log.json("finding", f);
    log.assert_eq(
        "evidence",
        &f["evidence"],
        &json!({"path": plugin(&home).display().to_string(), "count": 2, "rules": ["default_section", "symlink"], "reason": "other_context"}),
    );
    assert_eq!(f["remediation"]["argv"], Value::Null);
    assert_eq!(
        f["remediation"]["command"],
        "reinstall the widget with the omaddons binary and the HOME and XDG directories it was installed under"
    );
    assert_eq!(f["remediation"]["auto_fixable"], false);
    assert_eq!(
        found(envelope, DRIFT),
        Vec::<Value>::new(),
        "M3-D7: no drift"
    );
}

#[test]
fn no_widget_finding_on_a_clean_or_current_home() {
    let log = tlog!();
    let dir = tempfile::tempdir().unwrap();
    log.step("realistic home");
    let realistic = common::realistic::realistic_home(&dir.path().join("home"));
    let home = &realistic.home;
    log.env(home);
    let only = format!("{DRIFT},{INVALID}");
    log.step("no omarchy");
    assert!(!home.env.config().join("omarchy").exists());
    let ran = run(&log, home, &["doctor", "--json", "--only", &only]);
    ran.ok();
    assert_eq!(ran.envelope()["findings"], json!([]));
    log.step("a current widget");
    plant::widget_current(home);
    let ran = run(&log, home, &["doctor", "--json", "--only", &only]);
    ran.ok();
    assert_eq!(ran.envelope()["findings"], json!([]));
    assert_eq!(ran.envelope()["skipped"], json!([]));
    // The credential row has no false positive there either: a token stored as
    // `auth github set` leaves it (0600) is no finding.
    log.step("a private stored token");
    plant::stored_token(home, plant::MODE_TOKEN);
    let ran = run(&log, home, &["doctor", "--json", "--only", MODE]);
    ran.ok();
    assert_eq!(ran.envelope()["findings"], json!([]));
}

// ---------------------------------------------------------------------------------------
// The credential row.

#[test]
fn credential_fixture_is_found_fixed_and_verified() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    let file = plant::credential_file_mode(&home);
    let planted = facts(&log, "credential", &file);
    assert_eq!(planted.mode, "0644");
    let trees = record_trees(&log, &home);

    log.step("diagnose");
    let ran = run(&log, &home, &["doctor", "--json"]);
    ran.exit(1);
    let ours = found(ran.envelope(), MODE);
    assert_eq!(ours.len(), 1, "{:#}", ran.envelope());
    let f = &ours[0];
    log.json("finding", f);
    let path = file.display().to_string();
    assert_eq!(f["instance"], format!("{MODE}@machine:path={path}"));
    assert_eq!(
        f["title"],
        "The GitHub token file is readable by other users"
    );
    assert_eq!(f["severity"], "P2");
    log.assert_eq(
        "evidence",
        &f["evidence"],
        &json!({"path": path, "expected": "0600", "mode": "0644", "origin": "file", "source": "github"}),
    );
    let remediation = &f["remediation"];
    assert_eq!(
        remediation["argv"],
        argv(&["omaddons", "doctor", "fix", "--only", MODE])
    );
    assert_eq!(remediation["auto_fixable"], true);
    assert_eq!(remediation["estimated_actions"], 1);
    assert_trees_unchanged(&log, &home, &trees);

    log.step("dry run");
    let ran = run(
        &log,
        &home,
        &["doctor", "fix", "--only", MODE, "--dry-run", "--json"],
    );
    ran.exit(1);
    let envelope = ran.envelope();
    let planned = envelope["actions_planned"].as_array().unwrap();
    log.json("actions_planned", planned);
    assert_eq!(planned.len(), 1, "{envelope:#}");
    let action = &planned[0];
    assert_eq!(
        (
            &action["op"],
            &action["path"],
            &action["mode"],
            &action["bytes"]
        ),
        (&json!("chmod"), &json!(path), &json!("0600"), &json!(0))
    );
    assert_eq!(action["pre"]["kind"], "file");
    assert_eq!(action["pre"]["mode"], "0644");
    assert_eq!(action["post"]["mode"], "0600");
    assert_eq!(
        action["pre"]["identity"], action["post"]["identity"],
        "in place"
    );
    assert_eq!(
        action["pre"]["identity"]["ino"],
        json!(planted.ino.to_string())
    );
    assert!(
        envelope["precondition"]
            .as_str()
            .is_some_and(|t| t.starts_with("pc1:")),
        "{envelope:#}"
    );
    assert_eq!(
        facts(&log, "credential", &file),
        planted,
        "the dry run chmods nothing"
    );
    assert_trees_unchanged(&log, &home, &trees);

    log.step("fix");
    let ran = run(&log, &home, &["doctor", "fix", "--only", MODE, "--json"]);
    ran.ok();
    let envelope = ran.envelope();
    assert_eq!(envelope["state"], "DONE_OK");
    let summary = &envelope["summary"];
    log.json("summary", summary);
    assert_eq!(
        (
            &summary["actions_taken"],
            &summary["bytes_backed_up"],
            &summary["findings_after"]
        ),
        (&json!(1), &json!(0), &json!(0))
    );
    let tx = envelope["tx"].as_str().expect("a transaction").to_owned();
    let run_id = envelope["run_id"].as_str().unwrap().to_owned();
    let fixed = facts(&log, "credential", &file);
    assert_eq!(
        fixed,
        FileFacts {
            mode: "0600".to_owned(),
            ..planted.clone()
        },
        "0600, the same inode and bytes"
    );
    log.assert_unchanged("_retail_", &home.client("_retail_"));
    let diff = log.diff("config", &home.env.config());
    assert_eq!(
        (
            diff.added.clone(),
            diff.removed.clone(),
            diff.changed.clone()
        ),
        (
            Vec::new(),
            Vec::new(),
            vec![("omaddons/credentials/github".to_owned(), "mode".to_owned())]
        ),
        "only the token's mode changed"
    );

    log.step("history");
    let ran = run(&log, &home, &["history", "--json"]);
    ran.ok();
    let line = &ran.data()["entries"][0];
    log.json("history line", line);
    assert_eq!(line["kind"], "repair");
    assert_eq!(line["tx"], tx);
    assert_eq!(line["target"], Value::Null);
    assert_eq!(line["targets"], json!([]));
    assert_eq!(line["doctor_run_id"], run_id);
    let repairs = line["repairs"].as_array().unwrap();
    assert_eq!(repairs.len(), 1, "{line:#}");
    assert_eq!(
        (
            &repairs[0]["op"],
            &repairs[0]["fm"],
            &repairs[0]["pre"]["mode"],
            &repairs[0]["post"]["mode"]
        ),
        (
            &json!("chmod"),
            &json!(MODE),
            &json!("0644"),
            &json!("0600")
        )
    );

    log.step("verify");
    let ran = run(&log, &home, &["doctor", "--json", "--only", MODE]);
    ran.ok();
    assert_eq!(ran.envelope()["findings"], json!([]));

    log.step("again");
    let ran = run(&log, &home, &["doctor", "fix", "--only", MODE, "--json"]);
    ran.ok();
    assert_eq!(ran.envelope()["summary"]["actions_taken"], 0);
    assert_eq!(facts(&log, "credential", &file), fixed);
}

#[test]
fn explain_warns_that_undo_restores_the_exposure() {
    let log = tlog!();
    let home = home(&log);
    log.tree("state", &home.env.state());
    log.step("explain");
    let ran = run(&log, &home, &["doctor", "explain", MODE, "--json"]);
    ran.ok();
    let row = &ran.envelope()["failure_mode"];
    log.json("failure_mode", row);
    assert!(
        row["why"]
            .as_str()
            .unwrap()
            .contains("undoing that repair restores the old mode, and with it the exposure"),
        "{row:#}"
    );
    log.assert_eq(
        "fixer",
        &row["fixer"],
        &json!({"ops": ["chmod"], "scopes": ["config"], "enabled": true, "needs_force": false}),
    );
    log.assert_unchanged("state", &home.env.state());
}

/// `doctor fix --only <MODE> --dry-run --json`'s token.
fn preview(log: &TestLog, home: &Home) -> String {
    let ran = run(
        log,
        home,
        &["doctor", "fix", "--only", MODE, "--dry-run", "--json"],
    );
    ran.exit(1);
    ran.envelope()["precondition"]
        .as_str()
        .expect("a precondition")
        .to_owned()
}

fn bound(log: &TestLog, home: &Home, token: &str) -> Ran {
    run(
        log,
        home,
        &[
            "doctor",
            "fix",
            "--only",
            MODE,
            "--if-match",
            token,
            "--json",
        ],
    )
}

#[test]
fn the_chmod_preview_binds_across_processes() {
    let log = tlog!();
    for case in ["unchanged", "mode_changed", "replaced", "two_dry_runs"] {
        let log = log.case(case);
        let home = home(&log);
        log.step("plant");
        let file = plant::credential_file_mode(&home);
        let planted = facts(&log, "credential", &file);
        log.step("dry run");
        let token = preview(&log, &home);
        match case {
            "unchanged" => {
                log.step("bound run");
                let ran = bound(&log, &home, &token);
                ran.ok();
                assert_eq!(ran.envelope()["summary"]["actions_taken"], 1);
                assert_eq!(mode_of(&file), 0o600);
            }
            "mode_changed" => {
                log.step("chmod 0640 by hand");
                fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
                log.step("bound run");
                let ran = bound(&log, &home, &token);
                ran.fails(4, "plan_stale");
                let changed = &ran.envelope()["error"]["details"]["changed"];
                assert!(
                    changed.as_array().unwrap().contains(&json!("observed")),
                    "{changed}"
                );
                assert_eq!(mode_of(&file), 0o640, "nothing chmodded");
            }
            "replaced" => {
                log.step("replace the file");
                let new = file.with_extension("new");
                fs::write(&new, fs::read(&file).unwrap()).unwrap();
                fs::set_permissions(&new, fs::Permissions::from_mode(0o644)).unwrap();
                fs::rename(&new, &file).unwrap();
                let replaced = facts(&log, "credential", &file);
                assert_ne!(replaced.ino, planted.ino, "another inode");
                assert_eq!(replaced.sha256, planted.sha256, "the same bytes");
                log.step("bound run");
                let ran = bound(&log, &home, &token);
                ran.fails(4, "plan_stale");
                assert_eq!(
                    facts(&log, "credential", &file),
                    replaced,
                    "nothing chmodded"
                );
            }
            _ => {
                log.step("second dry run");
                let again = preview(&log, &home);
                log.assert_eq("token", &again, &token);
                assert_eq!(facts(&log, "credential", &file), planted);
            }
        }
    }
}

/// `doctor --json`, `doctor fix --json` and `doctor fix --only <MODE> --json` over one
/// credential the doctor cannot repair (`reason`, prose `command`), and nothing touched.
fn left_alone(log: &TestLog, home: &Home, reason: &str, command: &str, watched: &[&Path]) {
    for path in watched {
        facts(log, &path.display().to_string(), path);
    }
    log.step("diagnose");
    let ran = run(log, home, &["doctor", "--json"]);
    ran.exit(1);
    let ours = found(ran.envelope(), MODE);
    assert_eq!(ours.len(), 1, "{:#}", ran.envelope());
    let f = &ours[0];
    log.json("finding", f);
    assert_eq!(f["evidence"]["reason"], reason);
    assert_eq!(f["remediation"]["auto_fixable"], false);
    assert_eq!(f["remediation"]["argv"], Value::Null);
    assert_eq!(f["remediation"]["command"], command);
    log.step("fix");
    let ran = run(log, home, &["doctor", "fix", "--json"]);
    ran.exit(1);
    let summary = &ran.envelope()["summary"];
    assert_eq!(summary["actions_taken"], 0, "{summary}");
    assert_eq!(summary["manual"], 1, "listed as manual: {summary}");
    log.step("fix --only");
    let ran = run(log, home, &["doctor", "fix", "--only", MODE, "--json"]);
    ran.exit(1);
    assert_eq!(ran.envelope()["state"], "DONE_FINDINGS");
    assert_eq!(ran.envelope()["summary"]["actions_taken"], 0);
    log.step("untouched");
    for path in watched {
        let label = path.display().to_string();
        let before = facts(log, &label, path);
        assert_eq!(before.mode, format!("{:04o}", mode_of(path)));
    }
}

#[test]
fn credentials_outside_the_scope_are_left_alone() {
    let log = tlog!();
    for case in ["outside", "symlink", "other_owner"] {
        let log = log.case(case);
        if case == "other_owner" && !running_as_root() {
            log.note("other_owner runs only as root: a test cannot create another user's file (s2's unit test covers the rule with a synthetic uid)");
            continue;
        }
        let home = home(&log);
        log.step("plant");
        let elsewhere = home.env.root().join("elsewhere/gh");
        match case {
            "outside" => {
                plant::configured_credential(&home, &elsewhere, 0o644);
                let before = facts(&log, "outside", &elsewhere);
                left_alone(
                    &log,
                    &home,
                    "outside_config",
                    &format!("chmod 600 {}", elsewhere.display()),
                    &[&elsewhere],
                );
                assert_eq!(facts(&log, "outside", &elsewhere), before);
            }
            "symlink" => {
                fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
                fs::write(&elsewhere, "f7-fake-linked-key\n").unwrap();
                fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o644)).unwrap();
                let link = plant::token_file(&home);
                fs::create_dir_all(link.parent().unwrap()).unwrap();
                symlink(&elsewhere, &link).unwrap();
                let before = facts(&log, "target", &elsewhere);
                left_alone(
                    &log,
                    &home,
                    "symlink",
                    &format!(
                        "{} is a symlink: run chmod 600 on the file it points to",
                        link.display()
                    ),
                    &[&elsewhere],
                );
                assert_eq!(fs::read_link(&link).unwrap(), elsewhere, "the link stays");
                assert_eq!(facts(&log, "target", &elsewhere), before);
            }
            _ => {
                let file = plant::credential_file_mode(&home);
                std::os::unix::fs::chown(&file, Some(65_534), Some(65_534)).unwrap();
                let before = facts(&log, "credential", &file);
                let ran = run(&log, &home, &["doctor", "--json"]);
                let command = found(ran.envelope(), MODE)[0]["remediation"]["command"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                left_alone(&log, &home, "other_owner", &command, &[&file]);
                assert_eq!(facts(&log, "credential", &file), before);
            }
        }
    }
}

#[test]
fn inside_and_outside_in_one_run() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    let stored = plant::credential_file_mode(&home);
    let elsewhere = home.env.root().join("elsewhere/gh");
    plant::configured_credential(&home, &elsewhere, 0o644);
    let outside = facts(&log, "outside", &elsewhere);
    log.step("fix");
    let ran = run(&log, &home, &["doctor", "fix", "--only", MODE, "--json"]);
    let envelope = ran.envelope();
    log.json("summary", &envelope["summary"]);
    assert_eq!(envelope["summary"]["actions_taken"], 1, "{envelope:#}");
    assert_eq!(envelope["summary"]["findings_before"], 2);
    // The outside instance is left as manual, never a residual of the repair
    // (DONE_PARTIAL, exit 2), and with it left the run is no healthy one: exit 1.
    ran.exit(1);
    assert_eq!(envelope["state"], "DONE_FINDINGS", "{envelope:#}");
    let left = found(envelope, MODE);
    assert_eq!(left.len(), 1, "{envelope:#}");
    assert_eq!(left[0]["evidence"]["path"], elsewhere.display().to_string());
    assert_eq!(left[0]["evidence"]["reason"], "outside_config");
    assert_eq!(mode_of(&stored), 0o600, "the stored token is repaired");
    assert_eq!(
        facts(&log, "outside", &elsewhere),
        outside,
        "the outside key is not"
    );
}

#[test]
fn credential_detection_cases() {
    let log = tlog!();
    // (case, what the row reports: None for nothing, else (title, origin, auto_fixable))
    type Expected = Option<(&'static str, &'static str, bool)>;
    let cases: [(&str, Expected); 5] = [
        ("zero_byte", None),
        ("private", None),
        (
            "open_to_others",
            Some(("The GitHub token file is open to other users", "file", true)),
        ),
        (
            "configured_inside",
            Some((
                "The credential file config.toml names for github is readable by other users",
                "config",
                true,
            )),
        ),
        (
            "stored_also_configured",
            Some((
                "The GitHub token file is readable by other users",
                "file",
                true,
            )),
        ),
    ];
    for (case, expected) in cases {
        let log = log.case(case);
        let home = home(&log);
        log.step("plant");
        match case {
            "zero_byte" => {
                let file = plant::credential_file_mode(&home);
                fs::write(&file, b"").unwrap();
                assert_eq!(mode_of(&file), 0o644);
            }
            "private" => {
                plant::stored_token(&home, plant::MODE_TOKEN);
            }
            "open_to_others" => {
                let file = plant::stored_token(&home, plant::MODE_TOKEN);
                fs::set_permissions(&file, fs::Permissions::from_mode(0o602)).unwrap();
            }
            "configured_inside" => {
                let key = home.env.config().join("omaddons/keys/gh");
                plant::configured_credential(&home, &key, 0o644);
            }
            _ => {
                let file = plant::credential_file_mode(&home);
                let config = plant::config_toml(&home);
                fs::write(
                    &config,
                    format!(
                        "[sources.github]\ncredential = {{ file = {} }}\n",
                        json!(file.display().to_string())
                    ),
                )
                .unwrap();
            }
        }
        log.step("diagnose");
        let ran = run(&log, &home, &["doctor", "--json", "--only", MODE]);
        let ours = found(ran.envelope(), MODE);
        log.json("found", &ours);
        let Some((title, origin, fixable)) = expected else {
            ran.ok();
            assert_eq!(ours, Vec::<Value>::new(), "{case}");
            continue;
        };
        ran.exit(1);
        assert_eq!(ours.len(), 1, "{case}: one finding per file");
        assert_eq!(ours[0]["title"], title, "{case}");
        assert_eq!(ours[0]["evidence"]["origin"], origin, "{case}");
        assert_eq!(ours[0]["remediation"]["auto_fixable"], fixable, "{case}");
        if case == "open_to_others" {
            assert_eq!(ours[0]["evidence"]["mode"], "0602");
        }
    }

    // A config.toml that does not read: the row is skipped, never a clean result.
    let log = log.case("broken_config");
    let home = home(&log);
    log.step("plant");
    plant::credential_file_mode(&home);
    fs::write(plant::config_toml(&home), "x = [\n").unwrap();
    log.step("diagnose");
    let ran = run(&log, &home, &["doctor", "--json", "--only", MODE]);
    ran.exit(1);
    assert_eq!(found(ran.envelope(), MODE), Vec::<Value>::new());
    log.assert_eq(
        "skipped",
        &ran.envelope()["skipped"],
        &json!([{"id": MODE, "reason": "config_unavailable", "target": null}]),
    );
}

// ---------------------------------------------------------------------------------------
// All three together.

/// All three rows planted on `home`: an invalid widget from another build (both widget
/// rows) and the exposed stored token.
fn plant_all(home: &Home) -> PathBuf {
    let desk = plant::widget_invalid(home);
    plant::other_build(&desk.plugin());
    plant::credential_file_mode(home)
}

#[test]
fn detection_is_repeatable_and_pure() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    plant_all(&home);
    let trees = record_trees(&log, &home);
    let lines = net_log(&home).len();
    log.step("diagnose twice");
    let first = run(&log, &home, &["doctor", "--json"]);
    first.exit(1);
    let second = run(&log, &home, &["doctor", "--json"]);
    second.exit(1);
    let ids: Vec<&str> = first.envelope()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["id"].as_str())
        .collect();
    log.json("ids", &ids);
    for id in [INVALID, MODE, DRIFT] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    log.assert_eq(
        "findings",
        &second.envelope()["findings"],
        &first.envelope()["findings"],
    );
    assert_trees_unchanged(&log, &home, &trees);
    assert_eq!(net_log(&home).len(), lines, "no request");
}

/// Every file under `root`, recursively (symlinks not followed).
fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                out.push(path);
            }
        }
    }
    out
}

#[test]
fn the_fake_token_never_appears() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    plant::credential_file_mode(&home);
    let mut runs: Vec<Ran> = Vec::new();
    log.step("diagnose, preview, bound fix, history");
    runs.push(run(&log, &home, &["doctor", "--json"]));
    let token = preview(&log, &home);
    runs.push(bound(&log, &home, &token));
    runs.push(run(&log, &home, &["history", "--json"]));
    log.step("diagnose, fix, verify again");
    let file = plant::token_file(&home);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    runs.push(run(
        &log,
        &home,
        &["doctor", "fix", "--only", MODE, "--dry-run", "--json"],
    ));
    runs.push(run(
        &log,
        &home,
        &["doctor", "fix", "--only", MODE, "--json"],
    ));
    runs.push(run(&log, &home, &["doctor", "--json", "--only", MODE]));
    log.step("search");
    for ran in &runs {
        assert!(
            !ran.stdout().contains(plant::MODE_TOKEN),
            "{:?} stdout",
            ran.run.argv
        );
        assert!(
            !ran.stderr().contains(plant::MODE_TOKEN),
            "{:?} stderr",
            ran.run.argv
        );
    }
    let state = files_under(&home.env.state());
    let logs = files_under(log.steps_path().parent().unwrap());
    log.json(
        "searched",
        &json!({"state_files": state.len(), "log_files": logs.len()}),
    );
    assert!(
        state.iter().any(|p| p.ends_with("history.jsonl")),
        "the state was written: {state:?}"
    );
    for path in state.iter().chain(&logs) {
        let bytes = fs::read(path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(plant::MODE_TOKEN),
            "the token appears in {}",
            path.display()
        );
    }
}

/// `status` reports the three rows as the doctor finds them (F8): each the machine's,
/// `widget-invalid` (a `quick` check) without `--cached` only, ranked by severity then id,
/// with the doctor's remediation.
#[test]
fn status_reports_these_rows_as_the_doctor_does() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    plant_all(&home);
    // The machine's: no `-t` on its explain command.
    let issue = |id: &str, severity: &str, title: &str, argv: &[&str], auto_fixable: bool| {
        json!({
            "id": id,
            "severity": severity,
            "title": title,
            "addon": null,
            "remediation": {
                "command": argv.join(" "),
                "argv": argv,
                "destructive": false
            },
            "auto_fixable": auto_fixable,
            "explain": {
                "command": format!("omaddons doctor explain {id}"),
                "argv": ["omaddons", "doctor", "explain", id]
            }
        })
    };
    let invalid = issue(
        INVALID,
        "P2",
        "The bar widget fails Omarchy's plugin validation (2 problems)",
        &["omaddons", "omarchy", "install"],
        false,
    );
    let mode = issue(
        MODE,
        "P2",
        "The GitHub token file is readable by other users",
        &["omaddons", "doctor", "fix", "--only", MODE],
        true,
    );
    let drift = issue(
        DRIFT,
        "P3",
        "The bar widget is from another omaddons build",
        &["omaddons", "omarchy", "install"],
        false,
    );
    for (args, expected) in [
        (
            vec!["status", "--json"],
            json!([invalid, mode.clone(), drift.clone()]),
        ),
        (vec!["status", "--cached", "--json"], json!([mode, drift])),
    ] {
        log.step(&args.join(" "));
        let ran = run(&log, &home, &args);
        ran.ok();
        let data = ran.data();
        log.json("issues", &data["issues"]);
        assert_eq!(data["issues"], expected, "{args:?}");
        for target in data["targets"].as_array().unwrap() {
            for issue in target["issues"].as_array().into_iter().flatten() {
                assert!(
                    ![INVALID, MODE].contains(&issue["id"].as_str().unwrap_or_default()),
                    "{args:?}: {issue}"
                );
            }
        }
    }
}

/// The issue `status` reports of a token file other users can read (F8 part 2, M4-D64):
/// auto-fixable, its explain argv runs, and its remediation, the doctor's own `doctor fix
/// --only`, repairs the file, after which `status` no longer reports it.
#[test]
fn status_runs_its_credential_issues_explain_and_fix() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    let file = plant::credential_file_mode(&home);
    let mode_issue = |ran: &Ran| -> Option<Value> {
        ran.data()["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|issue| issue["id"] == MODE)
            .cloned()
    };
    let words = |argv: &Value| -> Vec<String> {
        argv.as_array()
            .into_iter()
            .flatten()
            .map(|word| word.as_str().unwrap_or_default().to_owned())
            .collect()
    };
    // `omaddons <argv after omaddons> --json`, which exits `code`.
    let run_argv = |step: &str, argv: &[String], code: i32| {
        log.step(step);
        assert_eq!(
            argv.first().map(String::as_str),
            Some("omaddons"),
            "{argv:?}"
        );
        let mut args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        args.push("--json");
        run(&log, &home, &args).exit(code);
    };

    log.step("status --json");
    let ran = run(&log, &home, &["status", "--json"]);
    ran.ok();
    let issue = mode_issue(&ran);
    assert!(issue.is_some(), "no {MODE} issue: {}", ran.envelope());
    let issue = issue.unwrap_or_default();
    log.json("issue", &issue);
    assert_eq!(issue["auto_fixable"], true, "{issue}");
    let explain = words(&issue["explain"]["argv"]);
    let fix = words(&issue["remediation"]["argv"]);
    assert_eq!(explain, ["omaddons", "doctor", "explain", MODE], "{issue}");
    assert_eq!(
        fix,
        ["omaddons", "doctor", "fix", "--only", MODE],
        "{issue}"
    );

    run_argv("the issue's explain argv", &explain, 0);
    assert_eq!(mode_of(&file), 0o644, "explaining repairs nothing");
    run_argv("the issue's remediation", &fix, 0);
    assert_eq!(mode_of(&file), 0o600, "the doctor's fix repaired the file");

    log.step("status --json after the fix");
    let ran = run(&log, &home, &["status", "--json"]);
    ran.ok();
    assert_eq!(mode_issue(&ran), None, "{}", ran.envelope());
}

// ---------------------------------------------------------------------------------------
// The fixtures themselves.

/// The three fixtures are registered in id order; each one's `corrupt` produces its
/// finding, `assert_healthy` fails on it, and its remedy (the widget's `omarchy install`,
/// or the credential's repair) makes `assert_healthy` hold.
#[test]
fn the_fixtures_are_registered_and_round_trip() {
    let log = tlog!();
    let all = fixture::fixtures::all();
    let ids: Vec<&str> = all.iter().map(|f| f.id()).collect();
    // ubs:ignore — failure-mode ids, not secrets
    let at = |id: &str| ids.iter().position(|x| *x == id).expect("registered");
    assert!(
        at(INVALID) < at(DRIFT) && at(DRIFT) < at(MODE),
        "id order: {ids:?}"
    );
    for fx in all
        .iter()
        .filter(|f| [INVALID, DRIFT, MODE].contains(&f.id()))
    {
        let id = fx.id();
        // ubs:ignore — a failure-mode id, not a secret
        let credential = id == MODE;
        let log = log.case(id);
        let home = home(&log);
        assert_eq!(fx.fixer(), credential, "{id}");
        assert_eq!(fx.run_args(), Vec::<String>::new(), "{id}");
        let subject = if credential {
            vec![plant::token_file(&home)]
        } else {
            Vec::new()
        };
        assert_eq!(fx.subject(&home), subject, "{id}");
        log.step("corrupt");
        fx.corrupt(&home);
        let found = fixture::fixtures::found_ids(&home, &[]);
        assert!(found.contains(&id.to_owned()), "{id} in {found:?}");
        let healthy = catch_unwind(AssertUnwindSafe(|| fx.assert_healthy(&home)));
        assert!(
            healthy.is_err(),
            "{id}: assert_healthy fails on the planted state"
        );
        log.step("remedy");
        if credential {
            run(&log, &home, &["doctor", "fix", "--only", MODE, "--json"]).ok();
        } else {
            fx.remedy(&home);
        }
        log.step("healthy");
        fx.assert_healthy(&home);
    }
}
