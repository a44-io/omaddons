//! e2e suite: history lines record what each change did to each addon's installed release (`transitions`, RG-02), and no later command rewrites them.
//!
//! A retail and a Mists Classic client go through an install, an update, a pin and an
//! unpin, a skip and an unskip, a backup, a sync, a copy, a remove and its undo. Each
//! line that moved a release records it, both sides with their release key and archive
//! hash; a pin, a skip and a backup record none, and their lines carry no `transitions`
//! key at all, as before. Every command only appends to `history.jsonl`: the bytes of
//! the lines before it never change.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::fs;

use common::binding::{publish, two_clients_with_foo};
use common::home::Home;
use common::tlog::{TestLog, tlog};
use common::wtf::Wtf;
use serde_json::{Value, json};

const MAINLINE_AND_MISTS: &[(&str, u32)] = common::binding::MAINLINE_AND_MISTS;

/// Runs a change, which must succeed, and returns the history lines it appended, after
/// checking that every line before them kept its bytes.
fn change(log: &TestLog, home: &Home, label: &str, args: &[&str]) -> Vec<Value> {
    let history = home.state().join("history.jsonl");
    let before = fs::read(&history).unwrap_or_default();
    log.step(label);
    let mut argv = args.to_vec();
    argv.push("--json");
    let ran = log.run(home, &argv);
    ran.ok();
    let after = fs::read(&history).unwrap();
    assert!(
        after.starts_with(&before),
        "{label}: history.jsonl is append-only: the {} bytes before it changed",
        before.len()
    );
    let appended: Vec<Value> = String::from_utf8(after[before.len()..].to_vec())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    log.json(&format!("{label}: lines appended"), &json!(appended));
    assert!(!appended.is_empty(), "{label}: appended no line");
    // An automatic checkpoint before the change, where one is made, moved no release.
    for line in appended.iter().filter(|l| l["kind"] == "backup") {
        assert_none_recorded(&format!("{label}: its backup"), line);
    }
    appended
}

/// The one line of `kind` among `lines`.
fn line_of<'a>(label: &str, lines: &'a [Value], kind: &str) -> &'a Value {
    let mut of_kind = lines.iter().filter(|l| l["kind"] == kind);
    let line = of_kind
        .next()
        .unwrap_or_else(|| panic!("{label}: no {kind} line in {lines:?}")); // ubs:ignore — the test's own failure
    assert!(of_kind.next().is_none(), "{label}: two {kind} lines");
    line
}

/// A line's transitions as `<addon> <change> <from> -> <to>`, `-` for no release.
fn moves(line: &Value) -> Vec<String> {
    let side = |r: &Value| match r["version"].as_str() {
        Some(version) => version.to_owned(),
        None => "-".to_owned(),
    };
    line["transitions"]
        .as_array()
        .map(|all| {
            all.iter()
                .map(|t| {
                    format!(
                        "{} {} {} -> {}",
                        t["addon"].as_str().unwrap(),
                        t["change"].as_str().unwrap(),
                        side(&t["from"]),
                        side(&t["to"])
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every side of `line`'s transitions names its release (`gh:o/<repo>` at a tag) and
/// the archive it was installed from.
fn assert_identified(log: &TestLog, label: &str, line: &Value) {
    for t in line["transitions"].as_array().unwrap() {
        for side in [&t["from"], &t["to"]] {
            if side.is_null() {
                continue;
            }
            let repo = t["addon"].as_str().unwrap().to_ascii_lowercase();
            log.assert_eq(
                &format!("{label}: {} release", t["addon"]),
                &side["release"],
                &json!({"source": format!("gh:o/{repo}"), "tag": side["version"]}),
            );
            let hash = side["witness"]["blob_sha256"].as_str().unwrap_or("");
            assert!(
                hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "{label}: {} is witnessed by its archive's hash: {side}",
                t["addon"]
            );
        }
    }
}

/// A line that moved no release carries no `transitions` key, as lines did before it.
fn assert_none_recorded(label: &str, line: &Value) {
    assert!(
        line.get("transitions").is_none(),
        "{label}: no transitions key: {line}"
    );
}

#[test]
fn each_change_records_the_releases_it_moved_and_a_pin_a_skip_and_a_backup_none() {
    let log = tlog!();
    log.step("home: retail and Mists Classic, gh:o/foo v1 and gh:o/bar v1");
    let home = two_clients_with_foo("v1");
    let retail = home.client("_retail_");
    Wtf::at(&retail).saved_variables("Foo", 200).build();

    let lines = change(
        &log,
        &home,
        "install",
        &["install", "gh:o/foo", "-t", "retail"],
    );
    let install = line_of("install", &lines, "install");
    log.assert_eq(
        "install",
        &json!(moves(install)),
        &json!(["Foo installed - -> v1"]),
    );
    assert_identified(&log, "install", install);

    publish(&home, "foo", "Foo", "v2", MAINLINE_AND_MISTS);
    let lines = change(
        &log,
        &home,
        "update",
        &["update", "Foo", "-t", "retail", "--refresh"],
    );
    let update = line_of("update", &lines, "update");
    log.assert_eq(
        "update",
        &json!(moves(update)),
        &json!(["Foo updated v1 -> v2"]),
    );
    assert_identified(&log, "update", update);

    for (label, args, kind) in [
        ("pin", vec!["pin", "Foo", "-t", "retail"], "pin"),
        ("unpin", vec!["unpin", "Foo", "-t", "retail"], "pin"),
        (
            "skip",
            vec!["skip", "Foo", "--version", "v9", "-t", "retail"],
            "skip",
        ),
        ("unskip", vec!["unskip", "Foo", "-t", "retail"], "skip"),
        ("backup", vec!["backup", "create", "-t", "retail"], "backup"),
    ] {
        let lines = change(&log, &home, label, &args);
        assert_none_recorded(label, line_of(label, &lines, kind));
    }

    log.step("a desired state naming Bar on retail");
    let wanted = home.env.root().join("wanted.toml");
    fs::write(
        &wanted,
        "schema_version = \"1.0\"\n\n[target.\"retail\"]\naddons = [\"gh:o/foo\", \"gh:o/bar\"]\n",
    )
    .unwrap();
    let lines = change(
        &log,
        &home,
        "sync",
        &["sync", wanted.to_str().unwrap(), "-t", "retail"],
    );
    let sync = line_of("sync", &lines, "sync");
    log.assert_eq(
        "sync",
        &json!(moves(sync)),
        &json!(["Bar installed - -> v1"]),
    );
    assert_identified(&log, "sync", sync);

    let lines = change(
        &log,
        &home,
        "copy",
        &["copy", "--from", "retail", "--to", "classic"],
    );
    let copy = line_of("copy", &lines, "install");
    log.assert_eq(
        "copy: in name order",
        &json!(moves(copy)),
        &json!(["Bar installed - -> v1", "Foo installed - -> v2"]),
    );
    assert_identified(&log, "copy", copy);

    let lines = change(&log, &home, "remove", &["remove", "Foo", "-t", "retail"]);
    let remove = line_of("remove", &lines, "remove");
    log.assert_eq(
        "remove",
        &json!(moves(remove)),
        &json!(["Foo removed v2 -> -"]),
    );
    assert_identified(&log, "remove", remove);

    let lines = change(&log, &home, "undo the remove", &["undo"]);
    let undo = line_of("undo", &lines, "undo");
    log.assert_eq(
        "undo",
        &json!(moves(undo)),
        &json!(["Foo reverted - -> v2"]),
    );
    assert_identified(&log, "undo", undo);

    log.step("history -v reads every line");
    let ran = log.run(&home, &["history", "-v", "--limit", "100", "--json"]);
    ran.ok();
    let codes = ran.warnings();
    assert!(
        !codes.iter().any(|c| c == "history_lines_skipped"),
        "no line skipped: {codes:?}"
    );
    let entries = ran.data()["entries"].as_array().unwrap();
    let written = fs::read_to_string(home.state().join("history.jsonl")).unwrap();
    log.assert_eq(
        "every line listed",
        &json!(entries.len()),
        &json!(written.lines().count()),
    );
}
