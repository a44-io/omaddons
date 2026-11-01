//! E7 s6 (omad-m4-agent-surface-doctor-c6cm.5.7.6): the undo of repairs end to end, through
//! the binary, scenario by scenario. Every command runs with `--json --progress json`
//! (or as a human would), every output pair goes through `assert_private_all`, and each
//! "nothing moved" claim is a `log.assert_unchanged` over every tree a command may write.

use super::*;
use crate::common::privacy::ACCOUNT;
use crate::common::repair::{End, Kind as RepairKind, RepairScene, json_args};

// ---------------------------------------------------------------------------------------
// Homes and trees.

/// A home with a retail and a Classic Era client, both registered, each with
/// SavedVariables under private names for the privacy check.
fn two(log: &TestLog) -> Home {
    let home = Home::new(&["wow", "wow_classic_era"]);
    log.env(&home);
    for client in ["_retail_", "_classic_era_"] {
        for dir in [ACCOUNT, CHARACTER] {
            let dir = home.client(client).join(dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("Foo.lua"), "FooDB = { seen = true }\n").unwrap();
        }
    }
    register(&home, "retail");
    register(&home, "era");
    home
}

/// Every tree a command may write, by label.
fn trees(home: &Home) -> Vec<(&'static str, PathBuf)> {
    let mut trees = vec![
        ("state", home.env.state()),
        ("config", home.env.config()),
        ("cache", home.env.cache()),
        ("retail", home.client("_retail_")),
    ];
    if home.client("_classic_era_").exists() {
        trees.push(("era", home.client("_classic_era_")));
    }
    trees
}

/// Records every tree, once a reader has written the derived discovery cache every
/// command writes on its first look at the machine (no move, and no subject).
fn record(log: &TestLog, home: &Home) {
    run(log, home, &["status"]).ok();
    for (label, root) in trees(home) {
        log.tree(label, &root);
    }
}

fn unchanged(log: &TestLog, home: &Home) {
    for (label, root) in trees(home) {
        log.assert_unchanged(label, &root);
    }
}

/// `doctor fix --only <fm> <extra>`, committed: its transaction and run.
fn fixed(log: &TestLog, home: &Home, fm: &str, extra: &[&str]) -> (String, String) {
    let mut args = vec!["doctor", "fix", "--only", fm];
    args.extend(extra);
    let ran = run(log, home, &args);
    ran.ok();
    let report = ran.envelope();
    (
        text(&report["tx"]).to_owned(),
        text(&report["run_id"]).to_owned(),
    )
}

/// A refusal, logged as one record.
fn refusal(log: &TestLog, ran: &Ran) -> Value {
    let error = ran.envelope()["error"].clone();
    log.json(
        "refusal",
        &json!({
            "code": error["code"],
            "reason": error["details"]["reason"],
            "paths": error["details"]["paths"],
            "changed": error["details"]["changed_guards"],
            "fix_argv": error["fix_argv"],
        }),
    );
    error
}

/// `omaddons install gh:o/<name> --file <a zip of folder <Name>> -t <target>`.
fn install(log: &TestLog, home: &Home, name: &str, target: &str) -> Ran {
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join(format!("{name}.zip"));
    let folder = format!("{}{}", name[..1].to_ascii_uppercase(), &name[1..]);
    let entries = [(
        format!("{folder}/{folder}.toc"),
        "## Interface: 120105, 11508\n## Version: 1\n".to_owned(),
    )];
    let borrowed: Vec<(&str, &str)> = entries
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    fs::write(&zip, crate::common::archives::zip_text(&borrowed)).unwrap();
    let zip = zip.display().to_string();
    let r = format!("gh:o/{name}");
    run(log, home, &["install", &r, "--file", &zip, "-t", target])
}

/// A stuck folder journal `tx` on retail (a `prepared` line that names its target but
/// does not decode), its in-flight lock and client stage, and the request that
/// `fm-test-suspend` set it aside guarding `guarded`. `wtf` makes it a settings journal.
fn stuck(home: &Home, tx: &str, guarded: &str, wtf: bool) -> Vec<PathBuf> {
    let key = plant::target_key(home, "retail");
    let journal = home.state().join("journal").join(if wtf {
        format!("{tx}.wtf.jsonl")
    } else {
        format!("{tx}.jsonl")
    });
    let line = json!({
        "type": "prepared",
        "schema_version": "1.1",
        "target_key": key,
        "written_by": "a build this one does not read",
    });
    let lock = home.state().join(format!("inflight/{tx}.lock"));
    let stage = home
        .client("_retail_")
        .join(format!("Interface/.omaddons/stage/{tx}"));
    for (path, bytes) in [
        (journal.clone(), format!("{line}\n")),
        (lock.clone(), String::new()),
        (
            stage.join("Foo/Foo.toc"),
            "## Interface: 120105\n".to_owned(),
        ),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
    }
    let request = test_dir(home, Side::State).join(format!("{tx}.suspend"));
    fs::create_dir_all(request.parent().unwrap()).unwrap();
    fs::write(&request, format!("{key} {guarded}\n")).unwrap();
    vec![journal, lock, stage]
}

/// History's lines, as `history --json` lists them.
fn history(log: &TestLog, home: &Home) -> Vec<Value> {
    run(log, home, &["history"]).ok().data()["entries"]
        .as_array()
        .unwrap()
        .clone()
}

// ---------------------------------------------------------------------------------------
// 1. Round trips.

/// Every fix kind (E5's synthetic repairs, so each subject is known exactly, modes and
/// symlinks included) round-trips: `doctor undo <fix run>` gives `O`, the undo of that
/// undo `R`, and `doctor undo` of the second undo's run `O` again.
#[test]
fn undo_round_trips_every_fix_kind() {
    let log = tlog!();
    for kind in RepairKind::ALL.into_iter().chain([RepairKind::Suspend]) {
        let log = log.case(kind.label());
        let r = RepairScene::repaired(&log, kind);

        log.step("undo");
        let undone = log.run(&r.s, &json_args(&["doctor", "undo", &r.run_id]));
        let undone = checked(undone);
        undone.ok();
        let report = undone.envelope().clone();
        log.json("report", &report);
        log.assert_eq("state", &report["state"], &json!("DONE_OK"));
        log.assert_eq(
            "actions_taken",
            &report["actions_taken"],
            &json!(kind.undo_inverses()),
        );
        log.assert_eq("reverted_run", &report["reverted_run"], &json!(r.run_id));
        let recorded: Value = serde_json::from_slice(
            &fs::read(PathBuf::from(text(&report["run_dir"])).join("report.json")).unwrap(),
        )
        .unwrap();
        log.assert_eq("its run's kind", &recorded["kind"], &json!("undo"));
        assert_eq!(r.accounted(End::Before), Vec::<String>::new(), "O");
        assert_eq!(
            r.undo_accounted(),
            Vec::<String>::new(),
            "the undo's originals"
        );

        log.step("again");
        r.redo(&log, "").ok().assert_private();
        assert_eq!(r.accounted(End::After), Vec::<String>::new(), "R again");
        let redo_run = r.redo_history()[0]["doctor_run_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let undone = checked(log.run(&r.s, &json_args(&["doctor", "undo", &redo_run])));
        undone.ok();
        assert_eq!(
            r.accounted(End::Before),
            Vec::<String>::new(),
            "O once more"
        );
    }
}

/// `omaddons undo <tx>` and `doctor undo <run>` are the same undo: two homes built alike
/// end alike, with the same history line (paths below the home and identities aside),
/// and the human output of each names the repair.
#[test]
fn undo_through_omaddons_undo_and_doctor_undo_are_the_same() {
    let log = tlog!();
    let mut ends = Vec::new();
    for by in ["omaddons undo", "doctor undo"] {
        let log = log.case(by);
        let r = RepairScene::repaired(&log, RepairKind::FlattenTwo);
        let argv: Vec<String> = match by {
            "omaddons undo" => vec!["undo".into(), r.tx().unwrap()],
            _ => vec!["doctor".into(), "undo".into(), r.run_id.clone()],
        };
        let human = log.run(&r.s, &argv);
        human.ok().assert_private();
        assert!(
            human.stdout().contains("repair"),
            "{by} names the repair: {}",
            human.stdout()
        );
        assert_eq!(r.accounted(End::Before), Vec::<String>::new(), "{by}: O");
        let line = r.undo_history().pop().unwrap();
        let root = r.s.home.env.root().display().to_string();
        let neutral = |line: &Value| {
            let mut repairs: Vec<Value> = line["repairs"].as_array().unwrap().clone();
            for entry in &mut repairs {
                for side in ["pre", "post"] {
                    for end in ["", "path", "to"] {
                        let at = if end.is_empty() {
                            &mut entry[side]
                        } else {
                            &mut entry[side][end]
                        };
                        // Identities and directory stamps (which hold times) differ
                        // between two homes built alike.
                        if let Some(object) = at.as_object_mut() {
                            object.remove("identity");
                            object.remove("stamp");
                        }
                    }
                }
            }
            let text = serde_json::to_string(&repairs)
                .unwrap()
                .replace(&root, "<root>");
            json!({
                "kind": line["kind"],
                "reverts_named": line["reverts"].is_string(),
                "target": line["target"],
                "targets": line["targets"].as_array().map(Vec::len),
                "repairs": redact_txs(&text),
            })
        };
        ends.push((by, neutral(&line), r.subject()));
    }
    let (first, rest) = ends.split_first().unwrap();
    for other in rest {
        assert_eq!(
            other.1, first.1,
            "{} and {} wrote one line",
            first.0, other.0
        );
        assert_eq!(
            other.2.keys().collect::<Vec<_>>(),
            first.2.keys().collect::<Vec<_>>(),
            "the same subject"
        );
    }
}

/// `text` with every 26-character ULID written `<ulid>`.
fn redact_txs(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let crockford = |c: char| c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c));
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let run = chars[i..].iter().take_while(|c| crockford(**c)).count();
        if run == 26 {
            out.push_str("<ulid>");
            i += 26;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// §4.6: an agent repairs, then undoes through the general undo, never naming the doctor.
#[test]
fn an_agent_repairs_and_undoes_with_last_by() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    plant_write(&home, "a.write", b"broken\n");
    let subject = test_dir(&home, Side::State);
    log.tree("O", &subject);
    log.step("repair");
    let ran = run(
        &log,
        &home,
        &[
            "doctor",
            "fix",
            "--only",
            WRITE,
            "--actor",
            "agent",
            "--reason",
            "cleanup the doctor recommended",
        ],
    );
    ran.ok();
    let tx = text(&ran.envelope()["tx"]).to_owned();
    log.step("undo");
    let undone = run(&log, &home, &["undo", "--last-by", "agent"]);
    undone.ok();
    let data = undone.data();
    log.assert_eq("kind", &data["kind"], &json!("repair"));
    log.assert_eq("reverted", &data["reverted"]["tx"], &json!(tx));
    assert!(data["doctor_run_id"].is_string(), "{data}");
    log.assert_unchanged("O", &subject);
    log.step("history");
    let kinds: Vec<Value> = history(&log, &home)
        .iter()
        .map(|e| e["kind"].clone())
        .collect();
    // Newest first: the undo, then the repair it reverted.
    assert!(
        kinds.starts_with(&[json!("undo"), json!("repair")]),
        "{kinds:?}"
    );
}

/// Bare `undo`, `undo --run` and `undo -t` select a repair like any change.
#[test]
fn bare_undo_and_undo_run_select_a_repair() {
    let log = tlog!();
    {
        let log = log.case("bare");
        let home = home(&log);
        plant_write(&home, "a.write", b"broken\n");
        let (tx, _) = fixed(&log, &home, WRITE, &[]);
        let undone = run(&log, &home, &["undo"]);
        log.assert_eq(
            "reverted",
            &undone.ok().data()["reverted"]["tx"],
            &json!(tx),
        );
    }
    {
        let log = log.case("--run");
        let home = home(&log);
        plant_write(&home, "a.write", b"broken\n");
        let (tx, _) = fixed(&log, &home, WRITE, &[]);
        let line = history(&log, &home)
            .into_iter()
            // ubs:ignore — comparing transaction ids, not secrets
            .find(|e| e["tx"] == json!(tx))
            .unwrap();
        let undone = run(&log, &home, &["undo", "--run", text(&line["run"])]);
        log.assert_eq(
            "reverted",
            &undone.ok().data()["reverted"]["tx"],
            &json!(tx),
        );
    }
    {
        let log = log.case("-t");
        let home = two(&log);
        plant_flatten(&home, "_retail_", "TestWrapA", "Inner");
        let (tx, _) = fixed(&log, &home, FLATTEN, &[]);
        run(&log, &home, &["undo", "-t", "era"]).fails(3, "not_found");
        let undone = run(&log, &home, &["undo", "-t", "retail"]);
        log.assert_eq(
            "reverted",
            &undone.ok().data()["reverted"]["tx"],
            &json!(tx),
        );
    }
}

// ---------------------------------------------------------------------------------------
// 2. Strict refusals: nothing moves.

/// A path changed after the repair refuses both undos, `changed_since_repair` naming
/// exactly that path, before anything moves.
#[test]
fn undo_refuses_a_path_changed_after_the_repair() {
    let log = tlog!();
    // (case, what plants and repairs, then changes: the repair's tx and run, and the path
    // the refusal must name)
    type Change = fn(&TestLog, &Home) -> (String, String, PathBuf);
    let cases: [(&str, Change); 7] = [
        ("identity_replaced", |log, home| {
            let path = chmod_subject(home);
            let (tx, run) = fixed(log, home, "fm-test-chmod", &[]);
            let copy = path.with_extension("copy");
            fs::copy(&path, &copy).unwrap();
            fs::set_permissions(&copy, fs::Permissions::from_mode(0o600)).unwrap();
            fs::rename(&copy, &path).unwrap();
            (tx, run, path)
        }),
        ("same_size_content_edit", |log, home| {
            let path = plant_write(home, "a.write", b"broken\n");
            let (tx, run) = fixed(log, home, WRITE, &[]);
            fs::write(&path, b"REPAIRED\n").unwrap();
            (tx, run, path)
        }),
        ("mode_changed", |log, home| {
            let path = chmod_subject(home);
            let (tx, run) = fixed(log, home, "fm-test-chmod", &[]);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            (tx, run, path)
        }),
        ("destination_appeared", |log, home| {
            let path = plant_quarantine(home, Side::State, "x.quarantine", Kind::File);
            let (tx, run) = fixed(log, home, QUARANTINE, &["--force", "--yes"]);
            fs::write(&path, b"a newcomer\n").unwrap();
            (tx, run, path)
        }),
        ("source_vanished", |log, home| {
            plant_quarantine(home, Side::State, "x.quarantine", Kind::File);
            let (tx, run) = fixed(log, home, QUARANTINE, &["--force", "--yes"]);
            let parked = home
                .state()
                .join("doctor/runs")
                .join(&run)
                .join("quarantine/state/doctor-test/x.quarantine");
            fs::remove_file(&parked).unwrap();
            (tx, run, parked)
        }),
        ("backup_edited", |log, home| {
            plant_write(home, "a.write", b"broken\n");
            let (tx, run) = fixed(log, home, WRITE, &[]);
            let backup = home
                .state()
                .join("doctor/runs")
                .join(&run)
                .join("backups/1");
            let mut bytes = fs::read(&backup).unwrap();
            bytes[0] ^= 1;
            fs::set_permissions(&backup, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(&backup, bytes).unwrap();
            (tx, run, backup)
        }),
        ("backup_missing", |log, home| {
            plant_write(home, "a.write", b"broken\n");
            let (tx, run) = fixed(log, home, WRITE, &[]);
            let backup = home
                .state()
                .join("doctor/runs")
                .join(&run)
                .join("backups/1");
            fs::remove_file(&backup).unwrap();
            (tx, run, backup)
        }),
    ];
    for (case, change) in cases {
        let log = log.case(case);
        log.step("repair, then change");
        let home = home(&log);
        let (tx, fix_run, changed) = change(&log, &home);
        record(&log, &home);
        let runs_before = plant::runs(&home);
        log.step("undo");
        for args in [
            vec!["undo", tx.as_str()],
            vec!["doctor", "undo", fix_run.as_str()],
        ] {
            let ran = run(&log, &home, &args);
            ran.exit(4);
            let error = refusal(&log, &ran);
            log.assert_eq("code", &error["code"], &json!("undo_conflict"));
            log.assert_eq(
                "reason",
                &error["details"]["reason"],
                &json!("changed_since_repair"),
            );
            log.assert_eq("paths", &error["details"]["paths"], &json!([changed]));
        }
        unchanged(&log, &home);
        log.assert_eq("no run directory", &plant::runs(&home), &runs_before);
    }
}

/// `fm-test-chmod`'s subject: a 0644 file below `<config>/doctor-test/`.
fn chmod_subject(home: &Home) -> PathBuf {
    let path = test_dir(home, Side::Config).join("a.chmod");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"secret\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    path
}

/// A set-aside's guards refuse its undo once what they watch changed: the client's
/// generation, a guarded folder's epoch, the marker's bytes, the `WTF/` epoch; the set
/// stays in quarantine, and the fix (`doctor explain`) runs.
#[test]
fn undo_refuses_a_changed_guard() {
    let log = tlog!();
    type Change = fn(&TestLog, &Home);
    let cases: [(&str, &str, bool, Change, &str); 4] = [
        (
            "generation",
            "DBM-Core",
            false,
            |log, home| {
                install(log, home, "bar", "retail").ok();
            },
            "generation",
        ),
        (
            "a guarded folder edited",
            "DBM-Core",
            false,
            |_, home| {
                fs::write(
                    home.addons("_retail_").join("DBM-Core/edited.lua"),
                    "-- x\n",
                )
                .unwrap();
            },
            "epochs",
        ),
        (
            "the marker rewritten",
            "DBM-Core",
            false,
            |_, home| {
                let marker = home
                    .client("_retail_")
                    .join("Interface/.omaddons/target.json");
                let value: Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
                fs::write(&marker, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            },
            "marker",
        ),
        (
            "WTF/ touched",
            "WTF",
            true,
            |_, home| {
                let sv = home.client("_retail_").join(ACCOUNT).join("Foo.lua");
                fs::write(sv, "FooDB = { seen = false }\n").unwrap();
            },
            "epochs",
        ),
    ];
    for (case, guarded, wtf, change, changed) in cases {
        let log = log.case(case);
        let home = two(&log);
        fs::create_dir_all(home.addons("_retail_").join("DBM-Core")).unwrap();
        fs::write(
            home.addons("_retail_").join("DBM-Core/DBM-Core.toc"),
            "## Interface: 120105\n",
        )
        .unwrap();
        stuck(&home, "01JB8Q2VA00000000000000009", guarded, wtf);
        log.step("set aside");
        let (_, fix_run) = fixed(&log, &home, "fm-test-suspend", &["--force", "--yes"]);
        let quarantine = home
            .state()
            .join("doctor/runs")
            .join(&fix_run)
            .join("quarantine");
        log.tree("the set", &quarantine);
        log.step("change");
        change(&log, &home);
        record(&log, &home);
        log.step("undo");
        let ran = run(&log, &home, &["doctor", "undo", &fix_run]);
        ran.exit(4);
        let error = refusal(&log, &ran);
        log.assert_eq(
            "reason",
            &error["details"]["reason"],
            &json!("changed_since_suspension"),
        );
        log.assert_eq(
            "changed",
            &error["details"]["changed_guards"],
            &json!([changed]),
        );
        log.assert_eq(
            "target",
            &error["details"]["target"],
            &json!(plant::target_key(&home, "retail")),
        );
        let text = error.to_string();
        for private in [ACCOUNT, CHARACTER] {
            for part in private.split('/').filter(|p| p.starts_with("Fake")) {
                assert!(!text.contains(part), "{case}: {part} named: {text}");
            }
        }
        unchanged(&log, &home);
        log.assert_unchanged("the set", &quarantine);
        let fix: Vec<String> = error["fix_argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap().to_owned())
            .collect();
        let fix: Vec<&str> = fix.iter().skip(1).map(String::as_str).collect();
        run(&log, &home, &fix).exit(0);
    }
}

/// An unchanged set-aside comes back byte for byte, and the next change fails exactly as
/// it did before the set-aside.
#[test]
fn undo_reactivates_a_set_aside_byte_identical() {
    let log = tlog!();
    let home = two(&log);
    let set = stuck(&home, "01JB8Q2VA00000000000000009", "", false);
    log.step("before");
    let before = install(&log, &home, "bar", "retail");
    let error = before.envelope()["error"].clone();
    log.json("refusal", &error);
    assert!(
        error["code"].is_string(),
        "the stuck journal blocks retail: {error}"
    );
    let status_before = run(&log, &home, &["status"]).ok().warnings();
    for (i, path) in set.iter().enumerate() {
        log.tree(&format!("artifact {i}"), path.parent().unwrap());
    }
    log.step("set aside");
    let (_, fix_run) = fixed(&log, &home, "fm-test-suspend", &["--force", "--yes"]);
    log.step("undo");
    run(&log, &home, &["doctor", "undo", &fix_run]).ok();
    for (i, path) in set.iter().enumerate() {
        log.assert_unchanged(&format!("artifact {i}"), path.parent().unwrap());
    }
    log.step("again");
    let after = install(&log, &home, "bar", "retail");
    let again = after.envelope()["error"].clone();
    log.assert_eq("the same code", &again["code"], &error["code"]);
    log.assert_eq(
        "the same reason",
        &again["details"]["reason"],
        &error["details"]["reason"],
    );
    log.assert_eq(
        "status as before",
        &run(&log, &home, &["status"]).ok().warnings(),
        &status_before,
    );
}

/// A client-side repair's undo refuses while its client runs, or may, whatever
/// `--while-running` says; a home-side one does not.
#[test]
fn undo_of_a_repair_refuses_while_its_client_runs() {
    let log = tlog!();
    let mut home = two(&log);
    plant_flatten(&home, "_retail_", "TestWrapA", "Inner");
    let (flatten_tx, _) = fixed(&log, &home, FLATTEN, &[]);
    let chmod_file = chmod_subject(&home);
    let (chmod_tx, _) = fixed(&log, &home, "fm-test-chmod", &[]);
    let exe = home
        .client("_retail_")
        .join("Wow.exe")
        .display()
        .to_string();
    for (case, own) in [("running", true), ("may be running", false)] {
        let log = log.case(case);
        if !own && crate::common::procfs::running_as_root() {
            log.note("as root, an unreadable process table is readable: skipped");
            continue;
        }
        let root = if own {
            let proc = crate::common::procfs::FakeProc::new(home.env.root().join("proc-running"))
                .process(9, true, "Wow.exe", &["/usr/bin/wine", &exe], &[]);
            proc.root().to_path_buf()
        } else {
            // A process table this user cannot read: running is unknown, so it may run.
            let locked = home.env.root().join("proc-locked");
            fs::create_dir_all(&locked).unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
            locked
        };
        home.proc = Some(root);
        record(&log, &home);
        for extra in [&[][..], &["--while-running"][..]] {
            let mut args = vec!["undo", flatten_tx.as_str()];
            args.extend(extra);
            let ran = run(&log, &home, &args);
            ran.exit(4);
            let error = refusal(&log, &ran);
            log.assert_eq("code", &error["code"], &json!("game_running"));
            log.assert_eq("reason", &error["details"]["reason"], &json!("repair"));
        }
        unchanged(&log, &home);
    }
    log.step("a home-side repair's undo while retail runs");
    let proc = crate::common::procfs::FakeProc::new(home.env.root().join("proc-home-side"))
        .process(9, true, "Wow.exe", &["/usr/bin/wine", &exe], &[]);
    home.proc = Some(proc.root().to_path_buf());
    run(&log, &home, &["undo", &chmod_tx]).ok();
    log.assert_eq("undone", &mode(&chmod_file), &0o644);
}

// ---------------------------------------------------------------------------------------
// 3. Conflicts by overlap.

/// An older install's undo after a later repair of its folder is `undo_conflict` naming
/// the repair; once the repair is undone, the install's undo proceeds. (The repair is
/// E5's synthetic quarantine of the folder: `fm-test-flatten` takes only unmanaged
/// `TestWrap` folders.)
#[test]
fn an_older_install_undo_conflicts_with_a_later_flatten() {
    use crate::common::repairs::{Child, FixSpec, Root, doctor_run, run_child};
    let log = tlog!();
    let home = two(&log);
    let installed = install(&log, &home, "foo", "retail");
    installed.ok();
    let install_tx = installed.data()["tx"].as_str().unwrap().to_owned();
    log.step("a later repair of its folder");
    let retail = fs::canonicalize(home.client("_retail_")).unwrap();
    let run_id = doctor_run(&home);
    let child = run_child(
        &home,
        &log,
        &run_id,
        std::slice::from_ref(&retail),
        &[FixSpec::Quarantine {
            path: retail.join("Interface/AddOns/Foo"),
            root: Root::Client(retail.clone()),
        }],
        "",
    );
    let Child::Ended(outcome) = child else {
        panic!("the repair crashed"); // ubs:ignore — fails the test
    };
    let repair_tx = outcome["tx"].as_str().unwrap().to_owned();
    record(&log, &home);
    log.step("undo the install");
    let ran = run(&log, &home, &["undo", &install_tx]);
    ran.exit(4);
    let error = refusal(&log, &ran);
    let conflicting = &error["details"]["conflicting"][0];
    log.assert_eq("the repair", &conflicting["tx"], &json!(repair_tx));
    log.assert_eq("its kind", &conflicting["kind"], &json!("repair"));
    unchanged(&log, &home);
    log.step("undo the repair, then the install");
    run(&log, &home, &["undo", &repair_tx]).ok();
    run(&log, &home, &["undo", &install_tx]).ok();
    assert!(
        !home.addons("_retail_").join("Foo").exists(),
        "Foo's install undone"
    );
}

/// A repair's undo after a later adopt of the folder it lifted is `undo_conflict` naming
/// the adopt; once the adopt is undone, the repair's undo proceeds.
#[test]
fn undoing_a_repair_after_later_work_on_its_resources_conflicts() {
    let log = tlog!();
    let home = two(&log);
    plant_flatten(&home, "_retail_", "TestWrapA", "Inner");
    let (repair_tx, _) = fixed(&log, &home, FLATTEN, &[]);
    log.step("adopt the lifted folder");
    let adopted = run(
        &log,
        &home,
        &[
            "adopt",
            "Inner",
            "--ref",
            "gh:o/inner",
            "-t",
            "retail",
            "--no-verify",
        ],
    );
    adopted.ok();
    let adopt_tx = adopted.data()["targets"][0]["tx"]
        .as_str()
        .or_else(|| adopted.data()["tx"].as_str())
        .unwrap()
        .to_owned();
    record(&log, &home);
    log.step("undo the repair");
    let ran = run(&log, &home, &["undo", &repair_tx]);
    ran.exit(4);
    let error = refusal(&log, &ran);
    log.assert_eq(
        "reason",
        &error["details"]["reason"],
        &json!("changed_since_repair"),
    );
    log.assert_eq(
        "the adopt",
        &error["details"]["conflicting"][0]["kind"],
        &json!("adopt"),
    );
    log.assert_eq(
        "fix",
        &error["fix_argv"],
        &json!(["omaddons", "undo", adopt_tx]),
    );
    unchanged(&log, &home);
    log.step("undo the adopt, then the repair");
    run(&log, &home, &["undo", &adopt_tx]).ok();
    run(&log, &home, &["undo", &repair_tx]).ok();
    assert!(
        home.addons("_retail_").join("TestWrapA/Inner").is_dir(),
        "the flatten undone"
    );
}

// ---------------------------------------------------------------------------------------
// 4. `doctor undo`: selection, idempotence, usage.

/// A home with one committed `fm-test-write` repair: its file, tx and run.
fn written(log: &TestLog) -> (Home, PathBuf, String, String) {
    let home = home(log);
    log.step("repair");
    let file = plant_write(&home, "a.write", b"broken\n");
    let (tx, run) = fixed(log, &home, WRITE, &[]);
    (home, file, tx, run)
}

#[test]
fn bare_doctor_undo_twice() {
    let log = tlog!();
    let (home, file, _, fix_run) = written(&log);
    log.step("undo");
    let first = run(&log, &home, &["doctor", "undo"]);
    first.ok();
    log.assert_eq(
        "reverted_run",
        &first.envelope()["reverted_run"],
        &json!(fix_run),
    );
    log.assert_eq("undone", &fs::read(&file).unwrap(), &b"broken\n".to_vec());
    record(&log, &home);
    log.step("again");
    let again = run(&log, &home, &["doctor", "undo"]);
    again.ok();
    log.assert_eq(
        "nothing selected",
        &again.envelope()["reverted_run"],
        &Value::Null,
    );
    let human = log.cmd(&home, &["doctor", "undo"]).env(ENV.0, ENV.1).run();
    human.ok().assert_private();
    assert!(
        human.stdout().contains("nothing to undo"),
        "{}",
        human.stdout()
    );
    unchanged(&log, &home);
}

#[test]
fn doctor_undo_run_twice() {
    let log = tlog!();
    let (home, _, _, fix_run) = written(&log);
    let first = run(&log, &home, &["doctor", "undo", &fix_run]);
    first.ok();
    let undo_tx = first.envelope()["undo_tx"].clone();
    record(&log, &home);
    let runs_before = plant::runs(&home);
    log.step("again");
    let again = run(&log, &home, &["doctor", "undo", &fix_run]);
    again.ok();
    let report = again.envelope();
    log.json("report", report);
    log.assert_eq("already_undone", &report["already_undone"], &json!(true));
    log.assert_eq("the first undo", &report["undo_tx"], &undo_tx);
    log.assert_eq("no run", &report["run_id"], &Value::Null);
    log.assert_eq("no run directory", &plant::runs(&home), &runs_before);
    unchanged(&log, &home);
}

#[test]
fn doctor_undo_after_a_diagnose_selects_the_fix_run() {
    let log = tlog!();
    let (home, _, _, fix_run) = written(&log);
    log.step("diagnose");
    run(&log, &home, &["doctor"]).exit(0);
    log.step("undo");
    let undone = run(&log, &home, &["doctor", "undo"]);
    undone.ok();
    log.assert_eq(
        "the fix run",
        &undone.envelope()["reverted_run"],
        &json!(fix_run),
    );
}

/// `argv` without its leading `omaddons`, as the run helpers take it.
fn words(argv: &Value) -> Vec<String> {
    argv.as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|w| w.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn doctor_undo_latest_is_usage() {
    let log = tlog!();
    let (home, file, _, _) = written(&log);
    let ran = run(&log, &home, &["doctor", "undo", "latest"]);
    ran.fails(64, "usage");
    let error = refusal(&log, &ran);
    log.assert_eq(
        "fix",
        &error["fix_argv"],
        &json!(["omaddons", "doctor", "undo"]),
    );
    log.step("the fix");
    let fix = words(&error["fix_argv"]);
    let fix: Vec<&str> = fix.iter().map(String::as_str).collect();
    run(&log, &home, &fix).ok();
    log.assert_eq("undone", &fs::read(&file).unwrap(), &b"broken\n".to_vec());
}

#[test]
fn undo_run_with_a_doctor_run_id_is_usage() {
    let log = tlog!();
    let (home, file, _, fix_run) = written(&log);
    record(&log, &home);
    let ran = run(&log, &home, &["undo", "--run", &fix_run]);
    ran.fails(2, "usage");
    let error = refusal(&log, &ran);
    log.assert_eq(
        "fix",
        &error["fix_argv"],
        &json!(["omaddons", "doctor", "undo", fix_run]),
    );
    unchanged(&log, &home);
    log.step("the fix");
    let fix = words(&error["fix_argv"]);
    let fix: Vec<&str> = fix.iter().map(String::as_str).collect();
    run(&log, &home, &fix).ok();
    log.assert_eq("undone", &fs::read(&file).unwrap(), &b"broken\n".to_vec());
}

#[test]
fn doctor_undo_of_a_failed_or_diagnose_run_changes_nothing() {
    let log = tlog!();
    let home = home(&log);
    log.step("a diagnose");
    let diagnose = run(&log, &home, &["doctor"]);
    let diagnose_run = text(&diagnose.envelope()["run_id"]).to_owned();
    log.step("a fix that rolled back");
    plant_write(&home, "a.write", b"broken\n");
    let failed = cmd(&log, &home, &["doctor", "fix", "--only", WRITE])
        .failpoints("fsx.mutate.executed=error@1")
        .run();
    let failed = checked(failed);
    failed.exit(3);
    let failed_run = text(&failed.envelope()["run_id"]).to_owned();
    for (case, id) in [
        ("a diagnose", &diagnose_run),
        ("a rolled-back fix", &failed_run),
    ] {
        log.step(case);
        record(&log, &home);
        let undone = run(&log, &home, &["doctor", "undo", id]);
        undone.ok();
        log.assert_eq("no undo", &undone.envelope()["undo_tx"], &Value::Null);
        log.assert_eq("no run", &undone.envelope()["run_id"], &Value::Null);
        let human = log
            .cmd(&home, &["doctor", "undo", id])
            .env(ENV.0, ENV.1)
            .run();
        human.ok().assert_private();
        assert!(
            human.stdout().contains("changed nothing"),
            "{case}: {}",
            human.stdout()
        );
        unchanged(&log, &home);
    }
}

// ---------------------------------------------------------------------------------------
// 5. Dry runs, previews and binding.

/// Every dry run of an undo of a repair writes nothing: `doctor undo`'s none at all, and
/// `omaddons undo`'s only the plan it stores (in `<cache>/omaddons/plans`).
#[test]
fn undo_dry_runs_write_nothing() {
    let log = tlog!();
    let (home, _, tx, fix_run) = written(&log);
    record(&log, &home);
    let runs_before = plant::runs(&home);
    log.step("doctor undo --dry-run");
    let planned = run(&log, &home, &["doctor", "undo", "--dry-run"]);
    planned.ok();
    let report = planned.envelope().clone();
    log.json("report", &report);
    log.assert_eq(
        "one action planned",
        &report["actions_planned"].as_array().map(Vec::len),
        &Some(1),
    );
    log.assert_eq(
        "the bindable preview",
        &report["commands"][0]["argv"],
        &json!(["omaddons", "undo", tx, "--dry-run"]),
    );
    run(&log, &home, &["doctor", "undo", &fix_run, "--dry-run"]).ok();
    unchanged(&log, &home);
    log.assert_eq("no run directory", &plant::runs(&home), &runs_before);
    log.step("the preview it offers");
    let preview = words(&report["commands"][0]["argv"]);
    let preview: Vec<&str> = preview.iter().map(String::as_str).collect();
    run(&log, &home, &preview).ok();
    for (label, root) in trees(&home) {
        if label != "cache" {
            log.assert_unchanged(label, &root);
        }
    }
    log.assert_eq("no run directory", &plant::runs(&home), &runs_before);
}

/// The bound undo of a repair runs exactly its preview, in another process, or refuses
/// stale: the repaired file edited (`observed`), a commit on the repair's client
/// (`targets`).
#[test]
fn a_bound_undo_of_a_repair_is_stale_after_a_change() {
    let log = tlog!();
    let token_of = |log: &TestLog, home: &Home, tx: &str| -> String {
        run(log, home, &["undo", tx, "--dry-run"]).ok().data()["precondition"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    {
        let log = log.case("unchanged");
        let (home, file, tx, _) = written(&log);
        let token = token_of(&log, &home, &tx);
        run(&log, &home, &["undo", &tx, "--if-match", &token]).ok();
        log.assert_eq("undone", &fs::read(&file).unwrap(), &b"broken\n".to_vec());
    }
    {
        let log = log.case("the file edited");
        let (home, file, tx, _) = written(&log);
        let token = token_of(&log, &home, &tx);
        fs::write(&file, b"edited\n").unwrap();
        record(&log, &home);
        let ran = run(&log, &home, &["undo", &tx, "--if-match", &token]);
        ran.fails(1, "plan_stale");
        let error = refusal(&log, &ran);
        // The run's re-plan is refused (the path changed), and a refused undo's token
        // part names its verdict and its transaction alone, as a folder undo's does: so
        // `selection` moves with `observed`.
        assert!(
            error["details"]["changed"]
                .as_array()
                .unwrap()
                .contains(&json!("observed")),
            "{error}"
        );
        log.assert_eq("retryable", &error["retryable"], &json!(false));
        log.assert_eq(
            "fix",
            &error["fix_argv"],
            &json!(["omaddons", "undo", tx, "--dry-run"]),
        );
        unchanged(&log, &home);
    }
    {
        let log = log.case("a commit on its client");
        let home = two(&log);
        plant_flatten(&home, "_retail_", "TestWrapA", "Inner");
        let (tx, _) = fixed(&log, &home, FLATTEN, &[]);
        let token = token_of(&log, &home, &tx);
        install(&log, &home, "bar", "retail").ok();
        record(&log, &home);
        let ran = run(&log, &home, &["undo", &tx, "--if-match", &token]);
        ran.fails(1, "plan_stale");
        let error = refusal(&log, &ran);
        assert!(
            error["details"]["changed"]
                .as_array()
                .unwrap()
                .contains(&json!("targets")),
            "{error}"
        );
        unchanged(&log, &home);
    }
}

// ---------------------------------------------------------------------------------------
// 6. Concurrency and the lock.

/// Two `doctor undo <run>` planned at once serialize: one undoes the repair, the other
/// finds it already undone and exits 0.
#[test]
fn two_concurrent_doctor_undos_serialize() {
    let log = tlog!();
    let (home, file, _, fix_run) = written(&log);
    let (a, b) = (
        Pause::new(&home.env.root().join("pause-a")),
        Pause::new(&home.env.root().join("pause-b")),
    );
    let args = ["doctor", "undo", fix_run.as_str()];
    log.step("both plan");
    let first = cmd(&log, &home, &args)
        .failpoints("engine.snapshot.prestaged=pause")
        .pause(&a)
        .spawn();
    let second = cmd(&log, &home, &args)
        .failpoints("engine.snapshot.prestaged=pause")
        .pause(&b)
        .spawn();
    log.reached(&a, "engine.snapshot.prestaged");
    log.reached(&b, "engine.snapshot.prestaged");
    log.step("the first undoes");
    log.resume(&a, "engine.snapshot.prestaged");
    let first = checked(first.wait());
    first.ok();
    log.assert_eq(
        "undid it",
        &first.envelope()["already_undone"],
        &json!(false),
    );
    log.assert_eq("its actions", &first.envelope()["actions_taken"], &json!(1));
    log.step("the second finds it undone");
    log.resume(&b, "engine.snapshot.prestaged");
    let second = checked(second.wait());
    second.ok();
    log.assert_eq(
        "already undone",
        &second.envelope()["already_undone"],
        &json!(true),
    );
    log.assert_eq(
        "undone once",
        &fs::read(&file).unwrap(),
        &b"broken\n".to_vec(),
    );
    let logged: Vec<usize> = plant::runs(&home)
        .iter()
        .map(|r| actions_log(&home.state().join("doctor/runs").join(r)).len())
        .filter(|n| *n > 0)
        .collect();
    log.assert_eq(
        "the fix's run and one undo's hold actions",
        &logged.len(),
        &2,
    );
}

/// The same with `omaddons undo <tx>`: one undoes, the other is `undo_conflict`, already
/// undone, and moves nothing.
#[test]
fn two_concurrent_omaddons_undos() {
    let log = tlog!();
    let (home, file, tx, _) = written(&log);
    let (a, b) = (
        Pause::new(&home.env.root().join("pause-a")),
        Pause::new(&home.env.root().join("pause-b")),
    );
    let args = ["undo", tx.as_str()];
    let first = cmd(&log, &home, &args)
        .failpoints("engine.snapshot.prestaged=pause")
        .pause(&a)
        .spawn();
    let second = cmd(&log, &home, &args)
        .failpoints("engine.snapshot.prestaged=pause")
        .pause(&b)
        .spawn();
    log.reached(&a, "engine.snapshot.prestaged");
    log.reached(&b, "engine.snapshot.prestaged");
    log.resume(&a, "engine.snapshot.prestaged");
    checked(first.wait()).ok();
    log.resume(&b, "engine.snapshot.prestaged");
    let second = checked(second.wait());
    second.fails(4, "undo_conflict");
    assert!(
        second.envelope()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already undone"),
        "{}",
        second.envelope()
    );
    log.assert_eq(
        "undone once",
        &fs::read(&file).unwrap(),
        &b"broken\n".to_vec(),
    );
}

/// An undo that cannot take the lock within its timeout gives up: exit 5 (`locked`), its
/// run's report `LOCK_LOST`, the repair standing; once the lock is free, it undoes.
#[test]
fn an_undo_waits_for_the_lock_and_then_gives_up() {
    let log = tlog!();
    let (home, file, _, fix_run) = written(&log);
    log.step("another process holds the lock");
    let held = hold_lock(&home);
    let ran = run(&log, &home, &["doctor", "undo", &fix_run]);
    ran.exit(5);
    assert!(ran.elapsed_ms >= 5_000, "{} ms", ran.elapsed_ms);
    let error = refusal(&log, &ran);
    log.assert_eq("code", &error["code"], &json!("locked"));
    log.assert_eq("state", &ran.envelope()["state"], &json!("LOCK_LOST"));
    log.assert_eq(
        "still repaired",
        &fs::read(&file).unwrap(),
        &REPAIRED.to_vec(),
    );
    assert!(
        files_in(&home.state().join("journal"))
            .iter()
            .all(|name| !name.ends_with(".repair.jsonl")),
        "no journal"
    );
    drop(held);
    log.step("the lock free");
    run(&log, &home, &["doctor", "undo", &fix_run]).ok();
    log.assert_eq("undone", &fs::read(&file).unwrap(), &b"broken\n".to_vec());
}

// ---------------------------------------------------------------------------------------
// 7. `status.last_change`.

/// After a repair, `status` offers its undo as the last change's, and that undo (its
/// preview first, as the widget's Undo last does) reverts it.
#[test]
fn status_last_change_offers_the_repair_undo() {
    let log = tlog!();
    let (home, file, _, _) = written(&log);
    let status = run(&log, &home, &["status"]);
    let last = status.ok().data()["last_change"].clone();
    log.json("last change", &last);
    log.assert_eq("kind", &last["kind"], &json!("repair"));
    let argv = words(&last["undo"]["argv"]);
    log.assert_eq(
        "its argv",
        &argv[..2].to_vec(),
        &vec!["undo".to_owned(), "--run".to_owned()],
    );
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let mut preview = argv.clone();
    preview.push("--dry-run");
    run(&log, &home, &preview).ok();
    run(&log, &home, &argv).ok();
    log.assert_eq("undone", &fs::read(&file).unwrap(), &b"broken\n".to_vec());
    let after = run(&log, &home, &["status"]);
    log.assert_eq(
        "the undo is the last change",
        &after.ok().data()["last_change"]["kind"],
        &json!("undo"),
    );
}
