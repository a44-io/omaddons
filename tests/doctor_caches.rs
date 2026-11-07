//! e2e suite: the blob detector and its quarantine
//!
//! F6 part 3 (omad-m4-agent-surface-doctor-c6cm.6.6.3): `fm-caches-blob-hash-mismatch`
//! through the binary. A damaged cached archive is found only by a deep diagnosis
//! (`--include-deep`, or `--only` its id), one finding per blob with its full shape; two
//! diagnoses agree and change nothing; a clean, populated cache with decoys reports
//! nothing; an unreadable blob is reported; `doctor fix` quarantines a damaged blob by
//! one `rename_noreplace` into `<cache>/quarantine/<run id>/blobs/`, keeping its bytes,
//! its mode and `index.json`, and a second run does nothing; a pinned blob is quarantined
//! too and downloaded again by the next online reinstall; and a blob put right before the
//! run is never quarantined, whether the run is bound or re-plans under the lock.
#![cfg(test)]

use crate::common;
use crate::fixture;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use common::doctor::{check_envelope, plant};
use common::home::Home;
use common::netlog::net_log;
use common::pause::Pause;
use common::procfs::running_as_root;
use common::tlog::{Cmd, Ran, TestLog, tlog};

const ID: &str = "fm-caches-blob-hash-mismatch";

fn home(log: &TestLog) -> Home {
    let home = Home::new(&["wow", "wow_classic"]);
    log.env(&home);
    home
}

/// `omaddons <args> --json`, with `--progress json` for a doctor run.
fn cmd<'a>(log: &'a TestLog, home: &Home, args: &[&str]) -> Cmd<'a> {
    let mut argv: Vec<&str> = args.to_vec();
    argv.push("--json");
    if args.first() == Some(&"doctor") {
        argv.extend(["--progress", "json"]);
    }
    log.cmd(home, &argv)
}

/// The run's envelope checked against the doctor contract (a doctor run), and its
/// output private.
fn checked(ran: Ran) -> Ran {
    if ran.run.argv.iter().any(|a| a == "doctor") && ran.run.code >= 0 {
        check_envelope(ran.stdout(), ran.stderr(), ran.run.code);
    }
    ran.assert_private();
    ran
}

fn run(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    checked(cmd(log, home, args).run())
}

fn cache(home: &Home) -> PathBuf {
    home.env.cache().join("omaddons")
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The findings of `id` in an envelope.
fn found(envelope: &Value, id: &str) -> Vec<Value> {
    envelope["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["id"] == id)
        .cloned()
        .collect()
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// Every tree a "changes nothing" claim compares, one label each: `<cache>`, `<config>`,
/// each client dir, and each top-level entry of `<state>` but `doctor/`.
fn trees(home: &Home) -> Vec<(String, PathBuf)> {
    let mut out = vec![
        ("cache".to_owned(), home.env.cache()),
        ("config".to_owned(), home.env.config()),
    ];
    for client in ["_retail_", "_classic_"] {
        out.push((client.to_owned(), home.client(client)));
    }
    for entry in fs::read_dir(home.state()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name != "doctor" {
            out.push((format!("state/{name}"), path));
        }
    }
    out.retain(|(_, p)| fs::symlink_metadata(p).is_ok());
    out
}

/// What a "changes nothing" claim compares: each directory's tree, and each file's bytes
/// (a top-level `<state>` file: history, the registry, the lock).
struct Snapshot {
    dirs: Vec<(String, PathBuf)>,
    files: Vec<(String, PathBuf, Option<Vec<u8>>)>,
}

fn record_trees(log: &TestLog, home: &Home) -> Snapshot {
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for (label, path) in trees(home) {
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            log.tree(&label, &path);
            dirs.push((label, path));
        } else {
            let bytes = fs::read(&path).ok();
            files.push((label, path, bytes));
        }
    }
    Snapshot { dirs, files }
}

fn assert_trees(log: &TestLog, snapshot: &Snapshot) {
    for (label, dir) in &snapshot.dirs {
        log.assert_unchanged(label, dir);
    }
    for (label, path, bytes) in &snapshot.files {
        log.assert_eq(&format!("{label} unchanged"), &fs::read(path).ok(), bytes);
    }
}

/// A home with the damaged blob planted: the home, the blob's name, its path, and the
/// damaged bytes.
fn damaged(log: &TestLog) -> (Home, String, PathBuf, Vec<u8>) {
    let home = home(log);
    log.step("home");
    let sha = plant::damaged_blob(&home, "retail");
    log.json("planted", &plant::take_planted());
    let path = plant::blob_path(&home, &sha);
    log.note(format!("reading {}", path.display()));
    let bytes = fs::read(&path).unwrap();
    (home, sha, path, bytes)
}

/// The finding's full shape for the blob `name` holding `bytes`.
fn assert_shape(log: &TestLog, home: &Home, f: &Value, name: &str, bytes: &[u8]) {
    log.json("finding", f);
    let path = cache(home).join("blobs").join(name);
    for (key, want) in [
        ("instance", json!(format!("{ID}@machine:blob={name}"))),
        ("label", json!(format!("machine · blob {}", &name[..12]))),
        ("severity", json!("P2")),
        ("subsystem", json!("caches")),
        ("confidence", json!("high")),
        ("target", Value::Null),
        ("addon", Value::Null),
        ("folder", Value::Null),
    ] {
        log.assert_eq(key, &f[key], &want);
    }
    log.assert_eq("evidence.path", &f["evidence"]["path"], &json!(path));
    log.assert_eq(
        "evidence.expected",
        &f["evidence"]["expected"],
        &json!(format!("sha256:{name}")),
    );
    log.assert_eq(
        "evidence.observed",
        &f["evidence"]["observed"],
        &json!(format!("sha256:{}", sha(bytes))),
    );
    let remediation = &f["remediation"];
    log.assert_eq(
        "remediation argv",
        &remediation["argv"],
        &json!(["omaddons", "doctor", "fix", "--only", ID]),
    );
    log.assert_eq("auto_fixable", &remediation["auto_fixable"], &json!(true));
    log.assert_eq(
        "estimated_actions",
        &remediation["estimated_actions"],
        &json!(1),
    );
}

// ---------------------------------------------------------------------------------------
// 1-4. Diagnosis.

#[test]
fn a_damaged_blob_is_found_only_by_a_deep_diagnosis() {
    let log = tlog!();
    let (home, name, _, bytes) = damaged(&log);
    log.step("shallow");
    for args in [vec!["doctor"], vec!["doctor", "--quick"], vec!["status"]] {
        let ran = run(&log, &home, &args);
        assert!(
            !ran.stdout().contains(ID),
            "{args:?} never runs the deep row"
        );
    }
    log.note("`doctor health` has not landed (omad-m4-agent-surface-doctor-c6cm.5.4.1)");
    log.step("deep");
    let deep = run(&log, &home, &["doctor", "--include-deep"]);
    deep.exit(1);
    log.assert_eq("state", &deep.envelope()["state"], &json!("DONE_FINDINGS"));
    let findings = found(deep.envelope(), ID);
    assert_eq!(findings.len(), 1, "one finding: {findings:?}");
    assert_shape(&log, &home, &findings[0], &name, &bytes);
    log.step("only");
    let only = run(&log, &home, &["doctor", "--only", ID]);
    only.exit(1);
    log.assert_eq("the same finding", &found(only.envelope(), ID), &findings);
}

#[test]
fn deep_diagnoses_agree_and_change_nothing() {
    let log = tlog!();
    let (home, _, _, _) = damaged(&log);
    log.step("diagnose twice");
    let trees = record_trees(&log, &home);
    let first = run(&log, &home, &["doctor", "--include-deep"]);
    let second = run(&log, &home, &["doctor", "--include-deep"]);
    first.exit(1);
    second.exit(1);
    log.assert_eq(
        "equal findings",
        &first.envelope()["findings"],
        &second.envelope()["findings"],
    );
    assert_trees(&log, &trees);
}

/// `gh:o/<addon>` at `tag`, served beside the fixture's v1 (one folder, its TOC and Lua).
fn serve_tag(home: &Home, addon: &str, tag: &str) {
    let name = plant::folder(addon);
    let (zip, toc_path, lua_path) = (
        format!("{name}.zip"),
        format!("{name}/{name}.toc"),
        format!("{name}/{name}.lua"),
    );
    let toc = format!("## Interface: 120105, 50504, 11509\n## Version: {tag}\n");
    let flavors: &[(&str, u32)] = &[
        ("mainline", 120_105),
        ("mists", 50_504),
        ("classic", 11_509),
    ];
    let assets: [common::http::Asset<'_>; 1] = [(
        &zip,
        flavors,
        vec![(&toc_path, &toc), (&lua_path, plant::LUA)],
    )];
    common::http::gh_release(
        &home.http,
        &format!("https://github.com/o/{addon}"),
        tag,
        &assets,
    );
}

#[test]
fn a_clean_populated_cache_reports_nothing() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic", "wow_classic_era"]);
    log.env(&home);
    log.step("populate");
    plant::install(&home, "foo", "retail");
    plant::install(&home, "bar", "classic");
    plant::install(&home, "baz", "era");
    serve_tag(&home, "foo", "v2");
    run(&log, &home, &["update", "foo", "-t", "retail"]).ok();
    let undo = run(&log, &home, &["undo", "-t", "classic"]);
    undo.ok();
    log.json("planted", &plant::take_planted());
    let blobs = cache(&home).join("blobs");
    let names: Vec<String> = fs::read_dir(&blobs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.len() == 64)
        .collect();
    log.json("blobs", &names);
    assert!(names.len() >= 3, "at least three blobs: {names:?}");
    log.step("decoys");
    fs::write(blobs.join(".omaddons-tmp-test"), b"partial").unwrap();
    fs::write(blobs.join(sha(b"x").to_uppercase()), b"wrong bytes").unwrap();
    fs::create_dir(blobs.join(sha(b"a directory"))).unwrap();
    let damaged_copy = home.env.root().join("a-damaged-copy");
    fs::write(&damaged_copy, b"not its bytes").unwrap();
    symlink(&damaged_copy, blobs.join(sha(b"a symlink"))).unwrap();
    log.step("diagnose");
    let ran = run(&log, &home, &["doctor", "--include-deep", "--only", ID]);
    ran.ok();
    log.assert_eq("no finding", &found(ran.envelope(), ID), &Vec::new());
    log.assert_eq(
        "complete",
        &ran.envelope()["summary"]["incomplete"],
        &json!(0),
    );
}

#[test]
fn an_unreadable_blob_is_reported() {
    let log = tlog!();
    if running_as_root() {
        log.note("skipped: root reads a mode-000 file");
        return;
    }
    let home = home(&log);
    log.step("home");
    plant::install(&home, "foo", "retail");
    let record = common::state::record(&home, "retail");
    let name = record.addons["foo"].blob_sha256.clone().unwrap();
    let path = plant::blob_path(&home, &name);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    log.step("deep");
    let ran = run(&log, &home, &["doctor", "--include-deep"]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    ran.exit(1);
    let findings = found(ran.envelope(), ID);
    assert_eq!(findings.len(), 1, "{findings:?}");
    let f = &findings[0];
    log.json("finding", f);
    log.assert_eq("reason", &f["evidence"]["reason"], &json!("unreadable"));
    log.assert_eq(
        "no observed digest",
        &f["evidence"]["observed"],
        &Value::Null,
    );
    assert!(
        text(&f["title"]).ends_with("cannot be read"),
        "{}",
        f["title"]
    );
}

// ---------------------------------------------------------------------------------------
// 5-7. The repair.

/// The paths `actions` (or `actions_planned`) names, as `(op, path, to)`.
fn ops(actions: &Value) -> Vec<(String, String, String)> {
    actions
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            (
                text(&a["op"]).to_owned(),
                text(&a["path"]).to_owned(),
                text(&a["to"]).to_owned(),
            )
        })
        .collect()
}

/// Whether nothing lies under `<cache>/quarantine/`.
fn nothing_quarantined(home: &Home) -> bool {
    let root = cache(home).join("quarantine");
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                return false;
            }
        }
    }
    true
}

#[test]
fn doctor_fix_quarantines_a_damaged_blob_and_a_second_run_does_nothing() {
    let log = tlog!();
    let (home, name, path, bytes) = damaged(&log);
    let blob = path.display().to_string();
    let index = cache(&home).join("blobs/index.json");
    let index_bytes = fs::read(&index).unwrap();
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;

    log.step("default tiers");
    let plain = run(&log, &home, &["doctor", "fix", "--dry-run"]);
    assert!(
        !ops(&plain.envelope()["actions_planned"])
            .iter()
            .any(|(_, p, _)| *p == blob),
        "a plain fix never selects the deep row"
    );
    log.step("include deep");
    let deep = run(
        &log,
        &home,
        &["doctor", "fix", "--include-deep", "--dry-run"],
    );
    assert!(
        ops(&deep.envelope()["actions_planned"])
            .iter()
            .any(|(_, p, _)| *p == blob),
        "--include-deep plans it: {}",
        deep.envelope()["actions_planned"]
    );

    log.step("preview");
    let trees = record_trees(&log, &home);
    let preview = run(&log, &home, &["doctor", "fix", "--only", ID, "--dry-run"]);
    preview.exit(1);
    let planned = ops(&preview.envelope()["actions_planned"]);
    log.json("actions_planned", &preview.envelope()["actions_planned"]);
    assert_eq!(planned.len(), 1, "{planned:?}");
    let (op, from, to) = &planned[0];
    assert_eq!(
        (op.as_str(), from.as_str()),
        ("rename_noreplace", blob.as_str())
    );
    assert!(
        to.starts_with(&cache(&home).join("quarantine").display().to_string())
            && to.ends_with(&format!("/blobs/{name}")),
        "into the cache quarantine: {to}"
    );
    let token = text(&preview.envelope()["precondition"]).to_owned();
    assert!(token.starts_with("pc1:"), "{token}");
    assert_trees(&log, &trees);

    log.step("bound run");
    let fixed = run(
        &log,
        &home,
        &["doctor", "fix", "--only", ID, "--if-match", &token],
    );
    fixed.ok();
    let report = fixed.envelope();
    log.json("actions", &report["actions"]);
    log.assert_eq("state", &report["state"], &json!("DONE_OK"));
    log.assert_eq("done", &report["actions"][0]["ok"], &json!(true));
    let run_id = text(&report["run_id"]).to_owned();
    let quarantined = cache(&home)
        .join("quarantine")
        .join(&run_id)
        .join("blobs")
        .join(&name);
    log.json(
        "quarantine",
        &fs::read_dir(quarantined.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    );
    assert!(fs::symlink_metadata(&path).is_err(), "the blob is moved");
    log.note(format!("reading {}", quarantined.display()));
    log.assert_eq("its bytes, kept", &fs::read(&quarantined).unwrap(), &bytes);
    log.assert_eq(
        "its mode, kept",
        &(fs::metadata(&quarantined).unwrap().permissions().mode() & 0o7777),
        &mode,
    );
    log.assert_eq(
        "index.json untouched",
        &fs::read(&index).unwrap(),
        &index_bytes,
    );
    let history = common::state::history(&home);
    let repair = history.last().unwrap();
    let line = serde_json::to_value(repair).unwrap();
    log.json("history line", &line);
    log.assert_eq("kind", &line["kind"], &json!("repair"));
    log.assert_eq("no target", &line["target"], &Value::Null);
    let repairs: Vec<String> = line["repairs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| text(&r["op"]).to_owned())
        .collect();
    log.assert_eq("one rename", &repairs, &vec!["rename_noreplace".to_owned()]);

    log.step("after");
    let after = run(&log, &home, &["doctor", "--include-deep"]);
    log.assert_eq("no finding", &found(after.envelope(), ID), &Vec::new());
    let again = run(&log, &home, &["doctor", "fix", "--only", ID]);
    again.ok();
    log.assert_eq(
        "a second run: nothing",
        &again.envelope()["summary"]["actions_taken"],
        &json!(0),
    );
}

#[test]
fn a_pinned_blob_is_quarantined_and_the_next_reinstall_downloads_it() {
    let log = tlog!();
    let (home, name, path, bytes) = damaged(&log);
    log.step("fix");
    let fixed = run(&log, &home, &["doctor", "fix", "--only", ID]);
    fixed.ok();
    let run_id = text(&fixed.envelope()["run_id"]).to_owned();
    let quarantined = cache(&home)
        .join("quarantine")
        .join(&run_id)
        .join("blobs")
        .join(&name);
    log.step("offline");
    let offline = run(
        &log,
        &home,
        &[
            "install",
            "gh:o/foo",
            "-t",
            "retail",
            "--reinstall",
            "--offline",
        ],
    );
    offline.exit(1);
    log.assert_eq(
        "code",
        &offline.envelope()["error"]["code"],
        &json!("offline"),
    );
    log.assert_eq(
        "not cached",
        &offline.envelope()["error"]["details"]["reason"],
        &json!("not_cached"),
    );
    log.step("online");
    let before = net_log(&home).count_purpose("asset");
    let online = run(
        &log,
        &home,
        &["install", "gh:o/foo", "-t", "retail", "--reinstall"],
    );
    online.ok();
    let after = net_log(&home).count_purpose("asset");
    log.json("asset requests", &json!({"before": before, "after": after}));
    log.assert_eq("downloaded again", &after, &(before + 1));
    log.note(format!("reading {}", path.display()));
    log.assert_eq(
        "back, hashing to its name",
        &sha(&fs::read(&path).unwrap()),
        &name,
    );
    log.assert_eq(
        "the quarantined copy, unchanged",
        &fs::read(&quarantined).unwrap(),
        &bytes,
    );
}

#[test]
fn a_blob_put_right_before_the_run_is_never_quarantined() {
    let log = tlog!();
    {
        let log = log.case("bound");
        let (home, name, path, _) = damaged(&log);
        let original = plant::undamaged(&home, &name);
        log.step("preview");
        let preview = run(&log, &home, &["doctor", "fix", "--only", ID, "--dry-run"]);
        let token = text(&preview.envelope()["precondition"]).to_owned();
        log.step("put right");
        plant::restore_blob(&home, &name, &original);
        log.step("bound run");
        let bound = run(
            &log,
            &home,
            &["doctor", "fix", "--only", ID, "--if-match", &token],
        );
        bound.exit(4);
        log.assert_eq(
            "plan_stale",
            &bound.envelope()["error"]["code"],
            &json!("plan_stale"),
        );
        log.assert_eq(
            "the good blob in place",
            &sha(&fs::read(&path).unwrap()),
            &name,
        );
        assert!(nothing_quarantined(&home), "nothing quarantined");
    }
    {
        let log = log.case("paused");
        let (home, name, path, _) = damaged(&log);
        let original = plant::undamaged(&home, &name);
        let pause = Pause::new(home.env.root());
        log.step("plan, then pause");
        let running = cmd(&log, &home, &["doctor", "fix", "--only", ID])
            .failpoints("doctor.fix.planned=pause")
            .pause(&pause)
            .spawn();
        log.reached(&pause, "doctor.fix.planned");
        plant::restore_blob(&home, &name, &original);
        log.note(format!(
            "restored through a rename: inode {}",
            std::os::unix::fs::MetadataExt::ino(&fs::metadata(&path).unwrap())
        ));
        log.resume(&pause, "doctor.fix.planned");
        let ran = checked(running.wait());
        ran.ok();
        log.assert_eq(
            "re-planned under the lock: nothing",
            &ran.envelope()["summary"]["actions_taken"],
            &json!(0),
        );
        log.assert_eq(
            "the good blob in place",
            &sha(&fs::read(&path).unwrap()),
            &name,
        );
        assert!(nothing_quarantined(&home), "nothing quarantined");
    }
}

// ---------------------------------------------------------------------------------------
// 8. The fixture.

#[test]
fn the_caches_fixture_is_registered() {
    let log = tlog!();
    let fixtures = fixture::fixtures::all();
    let fx = fixtures
        .iter()
        .find(|f| f.id() == ID)
        .expect("registered in all()");
    assert!(fx.fixer(), "it declares its fixer");
    log.assert_eq(
        "run_args",
        &fx.run_args(),
        &vec!["--include-deep".to_owned()],
    );
    let home = home(&log);
    log.step("corrupt");
    fx.corrupt(&home);
    log.json("planted", &plant::take_planted());
    log.assert_eq(
        "subject",
        &fx.subject(&home),
        &vec![cache(&home).join("blobs")],
    );
    log.step("found, and not healthy");
    let ids = fixture::fixtures::found_ids(&home, &fx.run_args());
    assert!(
        ids.iter().any(|id| id == ID),
        "its corrupt produces its finding: {ids:?}"
    );
    let healthy = catch_unwind(AssertUnwindSafe(|| fx.assert_healthy(&home)));
    assert!(healthy.is_err(), "assert_healthy fails on the damaged blob");
    log.step("put right");
    let record = common::state::record(&home, "retail");
    let name = record.addons["foo"].blob_sha256.clone().unwrap();
    let original = plant::undamaged(&home, &name);
    plant::restore_blob(&home, &name, &original);
    fx.assert_healthy(&home);
    log.step("a clean home");
    let clean = Home::new(&["wow"]);
    log.env(&clean);
    let ids = fixture::fixtures::found_ids(&clean, &fx.run_args());
    assert!(
        !ids.iter().any(|id| id == ID),
        "the clean home does not: {ids:?}"
    );
}
