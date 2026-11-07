//! e2e suite: target-less repair lines (M4-D31) through `history`, `status` and `undo` on a two-client home.
//!
//! No command writes a repair line before E5, so these scenarios plant synthetic ones
//! (`common::history`) on a home with a retail and a Mists Classic client, then prove
//! every reader through the real binary: `history` and `history -t`, `status` plain and
//! `--cached`, `undo` (its refusal, a later repair's folder conflict, and rule 5 for
//! damaged lines), and privacy on every stream.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};

use common::addons::toc;
use common::archives::zip_text;
use common::history::{chmod, doctor_run_id, on_client, plant, plant_raw, rename, repair_line};
use common::home::Home;
use common::http::paths;
use common::privacy::{ACCOUNT, CHARACTER};
use common::state::key_of;
use common::tlog::{Ran, TestLog, tlog};
use omaddons::model::TargetKey;
use serde_json::{Value, json};

/// The interfaces the two synthetic clients load.
const RETAIL: &str = "120105";
const CLASSIC: &str = "50504";

/// A retail and a Mists Classic client, with SavedVariables under fake account, realm
/// and character names on retail.
fn new_home(log: &TestLog) -> Home {
    log.step("home");
    let home = Home::new(&["wow", "wow_classic"]);
    for (dir, text) in [(ACCOUNT, "-- account"), (CHARACTER, "-- character")] {
        let dir = home.client("_retail_").join(dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Foo.lua"), text).unwrap();
    }
    home
}

/// `omaddons <args> --json --progress json`, both streams checked for private names.
fn json(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv = args.to_vec();
    argv.extend(["--json", "--progress", "json"]);
    let ran = log.run(home, &argv);
    ran.assert_private();
    ran
}

/// Installs `<name>` (one folder) from a `--file` zip on `client` (`retail` or
/// `classic`), as `gh:o/<name>`; returns the transaction.
fn install(log: &TestLog, home: &Home, name: &str, client: &str) -> String {
    let interface = if client == "retail" { RETAIL } else { CLASSIC };
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join(format!("{name}.zip"));
    let text = toc(interface, &["## Version: 1"]);
    let (toc_path, lua_path) = (format!("{name}/{name}.toc"), format!("{name}/core.lua"));
    fs::write(&zip, zip_text(&[(&toc_path, &text), (&lua_path, "1")])).unwrap();
    let r = format!("gh:o/{}", name.to_ascii_lowercase());
    let zip = zip.display().to_string();
    let ran = json(log, home, &["install", &r, "--file", &zip, "-t", client]);
    ran.ok();
    ran.data()["tx"].as_str().unwrap().to_owned()
}

/// The home every scenario starts from: Foo on retail, Bar on classic, and the
/// transactions that installed them.
struct Clients {
    home: Home,
    retail: TargetKey,
    classic: TargetKey,
    foo_tx: String,
    bar_tx: String,
}

fn clients(log: &TestLog) -> Clients {
    let home = new_home(log);
    let foo_tx = install(log, &home, "Foo", "retail");
    let bar_tx = install(log, &home, "Bar", "classic");
    Clients {
        retail: key_of(&home, "retail"),
        classic: key_of(&home, "classic"),
        home,
        foo_tx,
        bar_tx,
    }
}

impl Clients {
    fn credential(&self) -> PathBuf {
        paths(&self.home.env).credential_file("github")
    }

    fn addons(&self, client: &str) -> PathBuf {
        self.home.addons(&format!("_{client}_"))
    }

    fn meta(&self, client: &str) -> PathBuf {
        self.home
            .client(&format!("_{client}_"))
            .join("Interface/.omaddons")
    }

    /// Records the trees no read or refused command may change.
    fn trees(&self, log: &TestLog) -> Vec<(&'static str, PathBuf)> {
        log.step("trees");
        let roots = vec![
            ("state", self.home.env.state()),
            ("retail", self.home.client("_retail_")),
            ("classic", self.home.client("_classic_")),
        ];
        for (label, root) in &roots {
            log.tree(label, root);
        }
        roots
    }

    fn unchanged(&self, log: &TestLog, roots: &[(&'static str, PathBuf)]) {
        for (label, root) in roots {
            log.assert_unchanged(label, root);
        }
    }
}

/// The transactions of `history`'s entries.
fn txs(ran: &Ran) -> Vec<String> {
    ran.data()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["tx"].as_str().unwrap().to_owned())
        .collect()
}

fn tx_of(line: &Value) -> String {
    line["tx"].as_str().unwrap().to_owned()
}

fn run_of(line: &Value) -> String {
    line["run"].as_str().unwrap().to_owned()
}

#[test]
fn a_repair_line_is_listed_with_its_clients_paths_and_doctor_run() {
    let log = tlog!();
    let c = clients(&log);
    log.step("plant R1");
    let run_dir = doctor_run_id();
    let credential = c.credential();
    let r1 = repair_line(
        "repair",
        &[c.retail],
        &run_dir,
        vec![chmod(1, &credential, "0644", "0600")],
    );
    plant(&c.home, &r1);
    log.json("planted R1", &r1);
    let roots = c.trees(&log);

    log.step("history --json");
    let ran = json(&log, &c.home, &["history"]);
    ran.ok();
    let entries = ran.data()["entries"].as_array().unwrap();
    let repair = &entries[0];
    log.json("the repair entry", repair);
    log.assert_eq("kind", &repair["kind"], &json!("repair"));
    log.assert_eq("target", &repair["target"], &Value::Null);
    log.assert_eq(
        "targets",
        &repair["targets"],
        &json!([{"id": "retail", "key": c.retail}]),
    );
    log.assert_eq("doctor_run_id", &repair["doctor_run_id"], &json!(run_dir));
    log.assert_eq("repairs", &repair["repairs"], &r1["repairs"]);
    log.assert_eq(
        "changes",
        &repair["changes"],
        &json!({"added": 0, "replaced": 0, "removed": 0, "recorded": 0, "restored": 0}),
    );
    assert!(
        !ran.warnings().contains(&"history_lines_skipped".to_owned()),
        "{:?}",
        ran.warnings()
    );
    for install in &entries[1..] {
        for absent in ["targets", "doctor_run_id", "repairs"] {
            assert!(install.get(absent).is_none(), "{absent} in {install}");
        }
    }

    log.step("history (human)");
    let ran = log.run(&c.home, &["history"]);
    ran.ok().assert_private();
    for expected in [
        "repair".to_owned(),
        "retail".to_owned(),
        format!("chmod {}", credential.display()),
        format!("doctor run {run_dir}"),
    ] {
        assert!(
            ran.stdout().contains(&expected),
            "{expected:?} missing: {}",
            ran.stdout()
        );
    }

    log.step("trees unchanged");
    c.unchanged(&log, &roots);
}

#[test]
fn history_t_shows_a_repair_under_each_client_it_touches() {
    let log = tlog!();
    let c = clients(&log);
    log.step("plant R1, R2, R3");
    let credential = c.credential();
    let r1 = repair_line(
        "repair",
        &[c.retail],
        &doctor_run_id(),
        vec![chmod(1, &credential, "0644", "0600")],
    );
    let r2 = repair_line(
        "repair",
        &[],
        &doctor_run_id(),
        vec![chmod(1, &credential, "0644", "0600")],
    );
    let quarantine = c.meta("retail").join("quarantine/x/Foo");
    let r3 = repair_line(
        "repair",
        &[],
        &doctor_run_id(),
        vec![on_client(
            rename(1, &c.addons("retail").join("Foo"), &quarantine),
            None,
            Some("Foo"),
        )],
    );
    for (name, line) in [("R1", &r1), ("R2", &r2), ("R3", &r3)] {
        plant(&c.home, line);
        log.json(&format!("planted {name}"), line);
    }
    let roots = c.trees(&log);

    let (r1, r2, r3) = (tx_of(&r1), tx_of(&r2), tx_of(&r3));
    log.step("history -t retail");
    let on_retail = txs(json(&log, &c.home, &["history", "-t", "retail"]).ok());
    log.json("on retail", &on_retail);
    for tx in [&r1, &r3, &c.foo_tx] {
        assert!(on_retail.contains(tx), "{tx} missing from {on_retail:?}");
    }
    for tx in [&r2, &c.bar_tx] {
        assert!(!on_retail.contains(tx), "{tx} in {on_retail:?}");
    }

    log.step("history -t classic");
    let mut on_classic = txs(json(&log, &c.home, &["history", "-t", "classic"]).ok());
    on_classic.sort();
    let mut expected = vec![r3.clone(), c.bar_tx.clone()];
    expected.sort();
    log.assert_eq("on classic", &on_classic, &expected);

    log.step("history");
    let all = txs(json(&log, &c.home, &["history"]).ok());
    for tx in [&r1, &r2, &r3, &c.foo_tx, &c.bar_tx] {
        assert!(all.contains(tx), "{tx} missing from {all:?}");
    }
    c.unchanged(&log, &roots);
}

#[test]
fn status_names_a_repair_as_the_last_change_from_history_and_from_the_projection() {
    let log = tlog!();
    let c = clients(&log);
    log.step("plant R1");
    let r1 = repair_line(
        "repair",
        &[c.retail],
        &doctor_run_id(),
        vec![chmod(1, &c.credential(), "0644", "0600")],
    );
    let planted = plant(&c.home, &r1);
    log.json("planted R1", &r1);

    log.step("status");
    let ran = json(&log, &c.home, &["status"]);
    let change = &ran.ok().data()["last_change"];
    log.json("last_change", change);
    log.assert_eq("kind", &change["kind"], &json!("repair"));
    log.assert_eq("targets", &change["targets"], &json!(["retail"]));
    log.assert_eq(
        "changes",
        &change["changes"],
        &json!({"added": 0, "replaced": 0, "removed": 0, "recorded": 0, "restored": 0}),
    );
    log.assert_eq(
        "undo argv",
        &change["undo"]["argv"],
        &json!(["omaddons", "undo", "--run", run_of(&r1)]),
    );
    log.assert_eq("state", &ran.data()["last_change_state"], &json!("ok"));

    log.step("status --cached, before a rebuild");
    let ran = json(&log, &c.home, &["status", "--cached"]);
    log.assert_eq(
        "state",
        &ran.ok().data()["last_change_state"],
        &json!("unknown"),
    );

    log.step("a settings change rebuilds the projection");
    json(&log, &c.home, &["config", "set", "default_channel", "beta"]).ok();
    let ran = json(&log, &c.home, &["status", "--cached"]);
    let data = ran.ok().data();
    log.assert_eq("state", &data["last_change_state"], &json!("ok"));
    log.assert_eq("kind", &data["last_change"]["kind"], &json!("repair"));
    log.assert_eq(
        "targets",
        &data["last_change"]["targets"],
        &json!(["retail"]),
    );
    let tail: Value =
        serde_json::from_slice(&fs::read(paths(&c.home.env).history_tail()).unwrap()).unwrap();
    log.json("history_tail", &tail);
    let held = tail["lines"].as_array().unwrap();
    // ubs:ignore — comparing history lines, not secrets
    let verbatim = held.iter().any(|l| l.as_str() == Some(planted.trim_end()));
    assert!(verbatim, "the projection holds R1 verbatim");

    log.step("a home-side repair is the newest");
    let r2 = repair_line(
        "repair",
        &[],
        &doctor_run_id(),
        vec![chmod(1, &c.credential(), "0644", "0600")],
    );
    plant(&c.home, &r2);
    log.json("planted R2", &r2);
    let ran = json(&log, &c.home, &["status"]);
    log.assert_eq(
        "targets",
        &ran.ok().data()["last_change"]["targets"],
        &json!([]),
    );
    let ran = log.run(&c.home, &["status"]);
    ran.ok().assert_private();
    assert!(
        ran.stdout()
            .lines()
            .any(|l| l.starts_with("last change: repair by human (")),
        "{}",
        ran.stdout()
    );
}

/// Every selector reaches R1, a repair, as the newest change (M4-D46), and the undo of
/// repairs checks it strictly: the credential it chmodded is not there, so each refuses
/// (`undo_conflict`, `changed_since_repair`) before anything moves.
#[test]
fn undo_reaches_a_repair_and_refuses_one_whose_paths_changed() {
    let log = tlog!();
    let c = clients(&log);
    log.step("plant R1");
    let r1 = repair_line(
        "repair",
        &[c.retail],
        &doctor_run_id(),
        vec![chmod(1, &c.credential(), "0644", "0600")],
    );
    plant(&c.home, &r1);
    log.json("planted R1", &r1);
    let roots = c.trees(&log);
    let addons_after_install = log.tree("retail addons", &c.addons("retail"));

    let (tx, run) = (tx_of(&r1), run_of(&r1));
    let selectors: [Vec<&str>; 6] = [
        vec!["undo"],
        vec!["undo", &tx],
        vec!["undo", "--run", &run],
        vec!["undo", "-t", "retail"],
        vec!["undo", "--last-by", "human"],
        vec!["undo", "--dry-run"],
    ];
    for args in selectors {
        log.step(&args.join(" "));
        let ran = json(&log, &c.home, &args);
        ran.fails(4, "undo_conflict");
        let error = &ran.envelope()["error"];
        log.assert_eq("details.tx", &error["details"]["tx"], &json!(tx));
        log.assert_eq(
            "details.reason",
            &error["details"]["reason"],
            &json!("changed_since_repair"),
        );
        log.assert_eq(
            "details.paths",
            &error["details"]["paths"],
            &json!([c.credential()]),
        );
        c.unchanged(&log, &roots);
        let journals = fs::read_dir(paths(&c.home.env).journal_dir()).map_or(0, |d| d.count());
        log.assert_eq("journals", &journals, &0);
    }

    log.step("undo -t classic --dry-run");
    let ran = json(&log, &c.home, &["undo", "-t", "classic", "--dry-run"]);
    log.assert_eq(
        "reverted",
        &ran.ok().data()["reverted"]["tx"],
        &json!(c.bar_tx),
    );

    log.step("undo the install of Foo");
    json(&log, &c.home, &["undo", &c.foo_tx]).ok();
    assert!(
        !c.addons("retail").join("Foo").exists(),
        "Foo's folder is gone"
    );
    log.step("undo that undo");
    json(&log, &c.home, &["undo"]).ok();
    assert!(addons_after_install.is_some());
    log.assert_unchanged("retail addons", &c.addons("retail"));
}

#[test]
fn undo_of_an_older_install_conflicts_with_a_later_repair_of_its_folder() {
    let log = tlog!();
    // (case, the repair line on a home, whether undoing Foo's install conflicts with it)
    type Case = (&'static str, fn(&Clients) -> Value, bool);
    let cases: [Case; 5] = [
        (
            "folder_on_retail",
            |c| {
                let addons = c.addons("retail");
                let primitive = rename(1, &addons.join("Foo/Foo"), &addons.join("FooFlat"));
                repair_line(
                    "repair",
                    &[c.retail],
                    &doctor_run_id(),
                    vec![on_client(primitive, Some(c.retail), Some("Foo"))],
                )
            },
            true,
        ),
        (
            "folder_by_path_only",
            |c| {
                let addons = c.addons("retail");
                let primitive = rename(1, &addons.join("Foo/Foo"), &addons.join("FooFlat"));
                repair_line("repair", &[c.retail], &doctor_run_id(), vec![primitive])
            },
            true,
        ),
        (
            "every_client",
            |c| {
                let addons = c.addons("retail");
                let primitive = rename(1, &addons.join("Foo/Foo"), &addons.join("FooFlat"));
                repair_line(
                    "repair",
                    &[],
                    &doctor_run_id(),
                    vec![on_client(primitive, None, Some("Foo"))],
                )
            },
            true,
        ),
        (
            "other_folder",
            |c| {
                let addons = c.addons("retail");
                let primitive = rename(1, &addons.join("Bar/Bar"), &addons.join("BarFlat"));
                repair_line(
                    "repair",
                    &[c.retail],
                    &doctor_run_id(),
                    vec![on_client(primitive, Some(c.retail), Some("Bar"))],
                )
            },
            false,
        ),
        (
            "other_client",
            |c| {
                let addons = c.addons("classic");
                let primitive = rename(1, &addons.join("Foo/Foo"), &addons.join("FooFlat"));
                repair_line(
                    "repair",
                    &[c.classic],
                    &doctor_run_id(),
                    vec![on_client(primitive, Some(c.classic), Some("Foo"))],
                )
            },
            false,
        ),
    ];
    for (name, line_of, conflicts) in cases {
        let case = log.case(name);
        let c = clients(&case);
        case.step("plant the repair");
        let line = line_of(&c);
        plant(&c.home, &line);
        case.json("planted", &line);
        let before = case.tree("retail addons", &c.addons("retail"));
        check_undo_of_foo(&case, &c, &line, conflicts);
        assert!(before.is_some(), "{name}");
    }

    // The repair predates Foo's install: another addon gives retail its key first.
    let case = log.case("older_repair");
    let home = new_home(&case);
    install(&case, &home, "Baz", "retail");
    let retail = key_of(&home, "retail");
    case.step("plant the repair, then install Foo");
    let addons = home.addons("_retail_");
    let line = repair_line(
        "repair",
        &[retail],
        &doctor_run_id(),
        vec![on_client(
            rename(1, &addons.join("Foo/Foo"), &addons.join("FooFlat")),
            Some(retail),
            Some("Foo"),
        )],
    );
    plant(&home, &line);
    case.json("planted", &line);
    let foo_tx = install(&case, &home, "Foo", "retail");
    case.step("undo Foo's install");
    json(&case, &home, &["undo", &foo_tx]).ok();
}

/// `undo <Foo's install>`: refused by the repair `line` (its dry run too, nothing
/// moving), or done, as `conflicts` says.
fn check_undo_of_foo(log: &TestLog, c: &Clients, line: &Value, conflicts: bool) {
    log.step("undo Foo's install");
    if !conflicts {
        json(log, &c.home, &["undo", &c.foo_tx]).ok();
        return;
    }
    for args in [
        vec!["undo", c.foo_tx.as_str()],
        vec!["undo", c.foo_tx.as_str(), "--dry-run"],
    ] {
        let ran = json(log, &c.home, &args);
        ran.fails(4, "undo_conflict");
        let error = &ran.envelope()["error"];
        log.assert_eq("details.tx", &error["details"]["tx"], &json!(c.foo_tx));
        log.assert_eq(
            "details.conflicting",
            &error["details"]["conflicting"],
            &json!([{"tx": tx_of(line), "kind": "repair", "folders": ["Foo"]}]),
        );
        log.assert_eq(
            "fix_argv",
            &error["fix_argv"],
            &json!(["omaddons", "undo", tx_of(line)]),
        );
        log.assert_unchanged("retail addons", &c.addons("retail"));
    }
}

#[test]
fn damaged_target_less_lines_stay_unreadable_and_undo_fails_closed() {
    let log = tlog!();
    let add_foo = json!([{"op": "add", "folder": "Foo", "addon": "Foo"}]);
    /// How a case damages a good repair line (the second argument: Foo's add op).
    type Damage = fn(Value, &Value) -> Value;
    let damaged: [(&str, Damage); 5] = [
        ("install_without_target", |mut line, ops| {
            line["kind"] = json!("install");
            line["ops"] = ops.clone();
            line["repairs"] = json!([]);
            line
        }),
        ("repair_with_folder_ops", |mut line, ops| {
            line["ops"] = ops.clone();
            line
        }),
        ("undo_without_primitives", |mut line, _| {
            line["kind"] = json!("undo");
            line["repairs"] = json!([]);
            line
        }),
        ("repair_with_bad_folder", |mut line, _| {
            line["repairs"][0]["folder"] = json!("../Escape");
            line
        }),
        ("repair_with_unknown_op", |mut line, _| {
            line["repairs"][0]["op"] = json!("symlink");
            line
        }),
    ];
    for (name, damage) in damaged {
        let case = log.case(name);
        let c = clients(&case);
        case.step("plant the damaged line");
        let good = repair_line(
            "repair",
            &[c.retail],
            &doctor_run_id(),
            vec![chmod(1, &c.credential(), "0644", "0600")],
        );
        let line = damage(good, &add_foo);
        plant_raw(&c.home, &line.to_string());
        case.json("planted", &line);

        case.step("history skips it");
        let ran = json(&case, &c.home, &["history"]);
        ran.ok();
        assert!(
            ran.warnings().contains(&"history_lines_skipped".to_owned()),
            "{name}: {:?}",
            ran.warnings()
        );
        assert!(!txs(&ran).contains(&tx_of(&line)), "{name}: listed");

        case.step("undo fails closed (rule 5)");
        case.tree("retail addons", &c.addons("retail"));
        let ran = json(&case, &c.home, &["undo", &c.foo_tx]);
        ran.fails(4, "undo_conflict");
        case.assert_eq(
            "details.reason",
            &ran.envelope()["error"]["details"]["reason"],
            &json!("history_unreadable"),
        );
        case.assert_unchanged("retail addons", &c.addons("retail"));
    }
}

#[test]
fn lines_with_a_target_read_and_print_exactly_as_before() {
    let log = tlog!();
    let c = clients(&log);
    log.step("history.jsonl as written");
    let text = fs::read_to_string(paths(&c.home.env).history_file()).unwrap();
    for line in text.lines() {
        assert!(
            line.starts_with(r#"{"schema_version":"1.0","tx":"#),
            "{line}"
        );
        // No line here was applied from a stored plan, so none carries `plan` (M4-D25).
        for absent in [
            "\"targets\"",
            "\"repairs\"",
            "\"doctor_run_id\"",
            "\"plan\"",
        ] {
            assert!(!line.contains(absent), "{absent} in {line}");
        }
    }
    log.step("history --json");
    let ran = json(&log, &c.home, &["history"]);
    // `plan` is always listed, `null` on a line no plan committed.
    let expected: Vec<&str> = vec![
        "actor", "argv", "at", "changes", "kind", "plan", "reason", "result", "run", "target", "tx",
    ];
    for entry in ran.ok().data()["entries"].as_array().unwrap() {
        let mut keys: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        log.assert_eq("keys", &keys, &expected);
        log.assert_eq("plan", &entry["plan"], &serde_json::Value::Null);
    }
}

#[test]
fn a_repair_path_under_wtf_is_never_printed() {
    let log = tlog!();
    let c = clients(&log);
    log.step("plant a hand-made line under WTF/");
    let client = c.home.client("_retail_");
    let saved = client.join("WTF/Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/Foo.lua");
    let to = client.join("WTF/Account/FAKEACCT/Foo.lua");
    let line = repair_line(
        "repair",
        &[c.retail],
        &doctor_run_id(),
        vec![rename(1, &saved, &to)],
    );
    let planted = plant(&c.home, &line);
    log.note("planted a line naming the fake account, realm and character");

    log.step("history --json");
    let ran = json(&log, &c.home, &["history"]);
    let repair = &ran.ok().data()["entries"][0]["repairs"][0];
    for key in ["path", "to"] {
        let shown = repair[key].as_str().unwrap();
        assert!(Path::new(shown).ends_with("WTF/…"), "{key}: {shown}");
    }
    log.step("history (human)");
    log.run(&c.home, &["history", "--progress", "json"])
        .ok()
        .assert_private();

    log.step("the line on disk is unchanged");
    let text = fs::read_to_string(paths(&c.home.env).history_file()).unwrap();
    assert!(text.ends_with(&planted), "history.jsonl is never rewritten");
}
