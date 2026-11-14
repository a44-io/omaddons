//! e2e suite: `history -v`'s items, what each transaction did to each addon with its versions before and after, over every kind of change and the undo of each.
//!
//! One retail client goes through an install, an update, a reinstall, a pin and an
//! unpin, an adoption, a backup with a one-addon and a whole settings restore, a plan run
//! with `apply`, a remove and a repair; then every one of them is undone, newest first.
//! `history -v --json` must name each addon a line's ops touched, the change and both
//! versions. A line records the releases it moved (RG-02), so they outlive its snapshot.
//! A line from before that reads as it did: from the undo snapshots; with its snapshot
//! gone, from the stored plan it names; with the plan gone too, `null`. `history` without
//! `-v` has no `items`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use common::addons::toc;
use common::binding::publish;
use common::home::Home;
use common::http::paths;
use common::tlog::{Ran, TestLog, tlog};
use common::wtf::Wtf;
use serde_json::{Value, json};

/// The one build every release here ships: retail 12.1.5.
const RETAIL: &[(&str, u32)] = &[("mainline", 120_105)];

/// `omaddons <args> --json --progress json`, both streams checked for private names.
fn json(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv = args.to_vec();
    argv.extend(["--json", "--progress", "json"]);
    let ran = log.run(home, &argv);
    ran.assert_private();
    ran
}

/// Runs a change, which must succeed and append a line of `kind` last; returns that
/// line's transaction. Every command reports its transaction in its own place, while the
/// history's last line is the change's own: a backup an automatic or `before_restore`
/// checkpoint makes is appended before it.
fn change(log: &TestLog, home: &Home, label: &str, args: &[&str], kind: &str) -> String {
    log.step(label);
    json(log, home, args).ok();
    let text = fs::read_to_string(home.state().join("history.jsonl")).unwrap();
    let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    log.json(&format!("{label}: last history line"), &last);
    log.assert_eq(&format!("{label}: kind"), &last["kind"], &json!(kind));
    last["tx"].as_str().unwrap().to_owned()
}

/// One item as `history -v --json` gives it.
fn item(
    addon: Option<&str>,
    change: &str,
    folders: u64,
    from: Option<&str>,
    to: Option<&str>,
) -> Value {
    json!({
        "addon": addon,
        "change": change,
        "folders": folders,
        "from_version": from,
        "to_version": to,
        "skips_marked": [],
        "skips_cleared": [],
    })
}

/// [`item`] whose change marked (`marked`) or cleared (`cleared`) Foo's label mark `v9`.
fn skip_item(change: &str, marked: bool, cleared: bool) -> Value {
    let note = json!([{"addon": "Foo", "version": "v9", "release": null}]);
    let mut item = item(Some("Foo"), change, 1, Some("v2"), Some("v2"));
    if marked {
        item["skips_marked"] = note.clone();
    }
    if cleared {
        item["skips_cleared"] = note;
    }
    item
}

/// `history -v --json`'s items, by transaction.
fn items_by_tx(log: &TestLog, home: &Home, label: &str) -> BTreeMap<String, Value> {
    log.step(label);
    let ran = json(log, home, &["history", "-v", "--limit", "100"]);
    ran.ok();
    ran.data()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let items = e.get("items").cloned().unwrap_or_else(|| {
                panic!("{label}: -v gives every line items: {e}") // ubs:ignore — the test's own failure
            });
            (e["tx"].as_str().unwrap().to_owned(), items)
        })
        .collect()
}

/// The transactions of every kind of change, in the order they ran.
struct Changes {
    install: String,
    update: String,
    reinstall: String,
    pin: String,
    unpin: String,
    skip: String,
    unskip: String,
    adopt: String,
    restore_addon: String,
    restore_whole: String,
    apply: String,
    plan: String,
    remove: String,
    repair: String,
}

/// A retail client with Foo's SavedVariables, taken through every kind of change.
fn every_change(log: &TestLog) -> (Home, Changes) {
    log.step("home");
    let home = Home::new(&["wow"]);
    let client = home.client("_retail_");
    let wtf = Wtf::at(&client).saved_variables("Foo", 200).build();
    let foo_sv = wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua");
    publish(&home, "foo", "Foo", "v1", RETAIL);

    let install = change(
        log,
        &home,
        "install",
        &["install", "gh:o/foo", "-t", "retail"],
        "install",
    );
    publish(&home, "foo", "Foo", "v2", RETAIL);
    let update = change(
        log,
        &home,
        "update",
        &["update", "Foo", "-t", "retail", "--refresh"],
        "update",
    );
    let reinstall = change(
        log,
        &home,
        "reinstall",
        &["install", "gh:o/foo", "-t", "retail", "--reinstall"],
        "update",
    );
    let pin = change(log, &home, "pin", &["pin", "Foo", "-t", "retail"], "pin");
    let unpin = change(
        log,
        &home,
        "unpin",
        &["unpin", "Foo", "-t", "retail"],
        "pin",
    );
    let skip = change(
        log,
        &home,
        "skip",
        &["skip", "Foo", "--version", "v9", "-t", "retail"],
        "skip",
    );
    let unskip = change(
        log,
        &home,
        "unskip",
        &["unskip", "Foo", "-t", "retail"],
        "skip",
    );

    log.step("an unmanaged folder");
    let unmanaged = home.addons("_retail_").join("Baz");
    fs::create_dir_all(&unmanaged).unwrap();
    fs::write(unmanaged.join("Baz.toc"), toc("120105", &["## Version: 3"])).unwrap();
    let adopt = change(
        log,
        &home,
        "adopt",
        &[
            "adopt",
            "Baz",
            "--ref",
            "gh:o/baz",
            "-t",
            "retail",
            "--no-verify",
        ],
        "adopt",
    );

    log.step("backup");
    let created = json(log, &home, &["backup", "create", "-t", "retail"]);
    created.ok();
    let backup = created.data()["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::write(&foo_sv, "FooDB = { played = 1 }\n").unwrap();
    let restore_addon = change(
        log,
        &home,
        "restore Foo's settings",
        &[
            "backup", "restore", &backup, "-t", "retail", "--addon", "Foo",
        ],
        "restore",
    );
    fs::write(&foo_sv, "FooDB = { played = 2 }\n").unwrap();
    let restore_whole = change(
        log,
        &home,
        "restore the whole WTF/",
        &["backup", "restore", &backup, "-t", "retail"],
        "restore",
    );

    log.step("preview Bar's install");
    publish(&home, "bar", "Bar", "v1", RETAIL);
    let preview = json(
        log,
        &home,
        &["install", "gh:o/bar", "-t", "retail", "--dry-run"],
    );
    preview.ok();
    let plan = preview.data()["plan_id"].as_str().unwrap().to_owned();
    let apply = change(log, &home, "apply", &["apply", &plan], "install");

    let remove = change(
        log,
        &home,
        "remove",
        &["remove", "Foo", "-t", "retail"],
        "remove",
    );

    log.step("a credential others can read");
    let credential = paths(&home.env).credential_file("github");
    fs::create_dir_all(credential.parent().unwrap()).unwrap();
    fs::write(&credential, "ghp_not_a_real_token_000000000000000000\n").unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o644)).unwrap();
    let repair = change(
        log,
        &home,
        "repair",
        &["doctor", "fix", "--only", "fm-secrets-credential-file-mode"],
        "repair",
    );
    let changes = Changes {
        install,
        update,
        reinstall,
        pin,
        unpin,
        skip,
        unskip,
        adopt,
        restore_addon,
        restore_whole,
        apply,
        plan,
        remove,
        repair,
    };
    (home, changes)
}

#[test]
fn every_kind_of_change_and_its_undo_names_the_addon_the_change_and_both_versions() {
    let log = tlog!();
    let (home, c) = every_change(&log);

    // Undone newest first, each by its own transaction.
    let mut undos = BTreeMap::new();
    for (label, tx) in [
        ("repair", &c.repair),
        ("remove", &c.remove),
        ("apply", &c.apply),
        ("restore_whole", &c.restore_whole),
        ("restore_addon", &c.restore_addon),
        ("adopt", &c.adopt),
        ("unskip", &c.unskip),
        ("skip", &c.skip),
        ("unpin", &c.unpin),
        ("pin", &c.pin),
        ("reinstall", &c.reinstall),
        ("update", &c.update),
        ("install", &c.install),
    ] {
        let undo = change(&log, &home, &format!("undo {label}"), &["undo", tx], "undo");
        undos.insert(label, undo);
    }

    let items = items_by_tx(&log, &home, "history -v --json");
    let foo = Some("Foo");
    let (v1, v2) = (Some("v1"), Some("v2"));
    let expected: Vec<(&str, &String, Value)> = vec![
        (
            "install",
            &c.install,
            json!([item(foo, "installed", 1, None, v1)]),
        ),
        (
            "update",
            &c.update,
            json!([item(foo, "updated", 1, v1, v2)]),
        ),
        (
            "reinstall",
            &c.reinstall,
            json!([item(foo, "reinstalled", 1, v2, v2)]),
        ),
        ("pin", &c.pin, json!([item(foo, "pinned", 1, v2, v2)])),
        ("unpin", &c.unpin, json!([item(foo, "unpinned", 1, v2, v2)])),
        (
            "skip: the label mark it made",
            &c.skip,
            json!([skip_item("skip_marked", true, false)]),
        ),
        (
            "unskip: the mark it cleared",
            &c.unskip,
            json!([skip_item("skip_cleared", false, true)]),
        ),
        (
            "adopt",
            &c.adopt,
            json!([item(Some("Baz"), "adopted", 1, None, Some("3"))]),
        ),
        (
            "restore_addon",
            &c.restore_addon,
            json!([item(foo, "settings_restored", 1, v2, v2)]),
        ),
        (
            "restore_whole",
            &c.restore_whole,
            json!([item(None, "settings_restored", 1, None, None)]),
        ),
        (
            "apply",
            &c.apply,
            json!([item(Some("Bar"), "installed", 1, None, v1)]),
        ),
        (
            "remove",
            &c.remove,
            json!([item(foo, "removed", 1, v2, None)]),
        ),
        ("repair: no addon", &c.repair, json!([])),
        ("undo repair: no addon", &undos["repair"], json!([])),
        (
            "undo remove",
            &undos["remove"],
            json!([item(foo, "reverted", 1, None, v2)]),
        ),
        (
            "undo apply",
            &undos["apply"],
            json!([item(Some("Bar"), "reverted", 1, v1, None)]),
        ),
        (
            "undo restore_whole",
            &undos["restore_whole"],
            json!([item(None, "reverted", 1, None, None)]),
        ),
        (
            "undo restore_addon",
            &undos["restore_addon"],
            json!([item(foo, "reverted", 1, v2, v2)]),
        ),
        (
            "undo adopt",
            &undos["adopt"],
            json!([item(Some("Baz"), "reverted", 1, Some("3"), None)]),
        ),
        (
            "undo unskip: the mark is back",
            &undos["unskip"],
            json!([skip_item("reverted", true, false)]),
        ),
        (
            "undo skip: the mark is gone",
            &undos["skip"],
            json!([skip_item("reverted", false, true)]),
        ),
        (
            "undo unpin",
            &undos["unpin"],
            json!([item(foo, "reverted", 1, v2, v2)]),
        ),
        (
            "undo pin",
            &undos["pin"],
            json!([item(foo, "reverted", 1, v2, v2)]),
        ),
        (
            "undo reinstall",
            &undos["reinstall"],
            json!([item(foo, "reverted", 1, v2, v2)]),
        ),
        (
            // The undo of an update marks the release it leaves (RG-32).
            "undo update",
            &undos["update"],
            {
                let mut reverted = item(foo, "reverted", 1, v2, v1);
                reverted["skips_marked"] = json!([
                    {"addon": "Foo", "version": "v2", "release": {"source": "gh:o/foo", "tag": "v2"}}
                ]);
                json!([reverted])
            },
        ),
        (
            "undo install",
            &undos["install"],
            json!([item(foo, "reverted", 1, v1, None)]),
        ),
    ];
    for (label, tx, want) in &expected {
        let got = items.get(*tx).unwrap_or_else(|| {
            panic!("{label}: {tx} is not listed") // ubs:ignore — the test's own failure
        });
        log.assert_eq(label, got, want);
    }
    // Every other line (the backups) names no addon.
    let named: Vec<&String> = expected.iter().map(|(_, tx, _)| *tx).collect();
    for (tx, got) in items.iter().filter(|(tx, _)| !named.contains(tx)) {
        log.assert_eq(&format!("{tx}: a line without ops"), got, &json!([]));
    }

    log.step("history without -v has no items");
    let plain = json(&log, &home, &["history", "--limit", "100"]);
    plain.ok();
    for entry in plain.data()["entries"].as_array().unwrap() {
        assert!(entry.get("items").is_none(), "{entry}");
    }

    log.step("history -v (human)");
    let human = log.run(&home, &["history", "-v", "--limit", "100"]);
    human.ok().assert_private();
    for expected in [
        "    Foo: installed v1 (1 folder)",
        "    Foo: updated v1 → v2 (1 folder)",
        "    Foo: reinstalled v2 (1 folder)",
        "    Foo: pinned v2 (1 folder)",
        "    Foo: skip marked v9 (1 folder)",
        "    Foo: skip cleared v9 (1 folder)",
        "    Baz: adopted 3 (1 folder)",
        "    WTF/: settings restored (1 folder)",
        "    Foo: removed v2 (1 folder)",
        "    Foo: reverted v2 → v1 (1 folder)",
    ] {
        assert!(
            human.stdout().contains(expected),
            "{expected:?} missing: {}",
            human.stdout()
        );
    }
}

#[test]
fn a_line_whose_snapshot_is_gone_keeps_the_versions_it_recorded() {
    let log = tlog!();
    let (home, c) = every_change(&log);
    let state = home.state();

    log.step("the update's undo snapshot pruned");
    fs::remove_dir_all(state.join("snapshots").join(&c.update)).unwrap();
    let items = items_by_tx(&log, &home, "history -v after the prune");
    // Each line recorded the releases it moved (RG-02): no snapshot is needed for them.
    log.assert_eq(
        "update",
        &items[&c.update],
        &json!([item(Some("Foo"), "updated", 1, Some("v1"), Some("v2"))]),
    );
    log.assert_eq(
        "install",
        &items[&c.install],
        &json!([item(Some("Foo"), "installed", 1, None, Some("v1"))]),
    );

    log.step("the applied install's undo snapshot pruned, and its plan swept");
    fs::remove_dir_all(state.join("snapshots").join(&c.apply)).unwrap();
    fs::remove_file(paths(&home.env).plan(&c.plan.parse().unwrap())).unwrap();
    let items = items_by_tx(&log, &home, "history -v: nothing but the line");
    log.assert_eq(
        "apply: its own record of the install",
        &items[&c.apply],
        &json!([item(Some("Bar"), "installed", 1, None, Some("v1"))]),
    );
}

/// Rewrites `history.jsonl` as an omaddons from before RG-02 would have written it: every
/// line without its `transitions`, the last key a line carries.
fn without_transitions(log: &TestLog, home: &Home) {
    log.step("history.jsonl as written before transitions were recorded");
    let path = home.state().join("history.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let mut old = String::new();
    for line in text.lines() {
        match line.find(",\"transitions\":") {
            Some(at) => {
                assert!(line.ends_with("]}"), "transitions is the last key: {line}");
                old.push_str(&line[..at]);
                old.push('}');
            }
            None => old.push_str(line),
        }
        old.push('\n');
    }
    assert_ne!(old, text, "some line recorded transitions");
    fs::write(&path, old).unwrap();
}

#[test]
fn a_line_from_before_transitions_reads_as_it_did() {
    let log = tlog!();
    let (home, _) = every_change(&log);
    let recorded = items_by_tx(&log, &home, "history -v with transitions");
    without_transitions(&log, &home);
    log.assert_eq(
        "with every snapshot there, the items are the same",
        &json!(items_by_tx(&log, &home, "history -v without them")),
        &json!(recorded),
    );
}

#[test]
fn a_line_from_before_transitions_whose_snapshot_is_gone_takes_its_versions_from_its_plan_else_null()
 {
    let log = tlog!();
    let (home, c) = every_change(&log);
    without_transitions(&log, &home);
    let state = home.state();

    log.step("the update's undo snapshot pruned");
    fs::remove_dir_all(state.join("snapshots").join(&c.update)).unwrap();
    let items = items_by_tx(&log, &home, "history -v after the prune");
    // The update found nothing to read, so neither side is believed: its ops still name
    // the change. The install before it reads the record it left from the update's
    // snapshot, which is gone too.
    log.assert_eq(
        "update",
        &items[&c.update],
        &json!([item(Some("Foo"), "updated", 1, None, None)]),
    );
    log.assert_eq(
        "install",
        &items[&c.install],
        &json!([item(Some("Foo"), "installed", 1, None, None)]),
    );
    log.assert_eq(
        "reinstall: its own snapshot and the next one's still read",
        &items[&c.reinstall],
        &json!([item(Some("Foo"), "reinstalled", 1, Some("v2"), Some("v2"))]),
    );

    log.step("the applied install's undo snapshot pruned");
    fs::remove_dir_all(state.join("snapshots").join(&c.apply)).unwrap();
    let items = items_by_tx(&log, &home, "history -v: from the plan");
    log.assert_eq(
        "apply: both versions from the stored plan",
        &items[&c.apply],
        &json!([item(Some("Bar"), "installed", 1, None, Some("v1"))]),
    );

    log.step("its plan swept from the cache");
    let plan = paths(&home.env).plan(&c.plan.parse().unwrap());
    fs::remove_file(&plan).unwrap();
    let items = items_by_tx(&log, &home, "history -v: nothing left");
    log.assert_eq(
        "apply: null, the change from its ops",
        &items[&c.apply],
        &json!([item(Some("Bar"), "installed", 1, None, None)]),
    );
}
