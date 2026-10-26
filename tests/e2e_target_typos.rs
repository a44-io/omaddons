//! e2e suite: a mistyped client selector (M4-D14) gets the closest word that selects a client here, in the user's own command, and the fix runs; nothing changes for the mistyped invocation.
//!
//! `-t retial` was `target_not_found` with the listing as its only fix. Now the fix is
//! the same command with `retail` in place of the typo, further close words follow as
//! alternatives, and `omaddons targets` comes last. Nothing is corrected on its own: the
//! command typed did nothing, and the scenarios hash `<state>`, `<config>` and every
//! client directory around it to prove so.

mod common;

use common::home::Home;
use common::tlog::{Ran, tlog};
use serde_json::Value;
use support::{
    assert_not_found, client_dirs, foo_zip, json, record_trees, targets_of, trees_unchanged,
    typo_home,
};

#[cfg(test)]
mod support {
    use std::path::PathBuf;

    use super::*;
    use common::tlog::TestLog;
    use common::trees::CustomRoot;

    /// `args` and `--json`.
    pub fn json(args: &[&str]) -> Vec<String> {
        args.iter()
            .copied()
            .chain(["--json"])
            .map(str::to_owned)
            .collect()
    }

    /// `retail` and `era` (Battle.net) and the 3.3.5a custom client `wotlk`.
    pub fn typo_home(log: &TestLog) -> Home {
        let home = Home::new(&["wow", "wow_classic_era"]);
        log.env(&home);
        CustomRoot::at(home.env.home().join("Games/wotlk"))
            .version(3, 3, 5, 12_340)
            .build();
        let ran = log.run(&home, &json(&["targets"]));
        let mut ids = targets_of(ran.ok());
        ids.sort();
        assert_eq!(ids, ["era", "retail", "wotlk"], "{}", ran.run.context());
        home
    }

    /// The client directories of [`typo_home`] that exist on `home`.
    pub fn client_dirs(home: &Home) -> Vec<(&'static str, PathBuf)> {
        [
            ("retail", home.client("_retail_")),
            ("era", home.client("_classic_era_")),
            ("wotlk", home.env.home().join("Games/wotlk")),
        ]
        .into_iter()
        .filter(|(_, dir)| dir.exists())
        .collect()
    }

    /// Records `<state>`, `<config>` and every client directory; `<cache>` too, for the
    /// reader only (any command that loads the world may refresh it).
    pub fn record_trees(log: &TestLog, home: &Home) {
        log.tree("state", &home.env.state());
        log.tree("config", &home.env.config());
        for (label, dir) in client_dirs(home) {
            log.tree(label, &dir);
        }
        log.tree("cache, for the reader", &home.env.cache());
    }

    #[track_caller]
    pub fn trees_unchanged(log: &TestLog, home: &Home) {
        log.assert_unchanged("state", &home.env.state());
        log.assert_unchanged("config", &home.env.config());
        for (label, dir) in client_dirs(home) {
            log.assert_unchanged(label, &dir);
        }
    }

    /// The ids of `data.targets`.
    pub fn targets_of(ran: &Ran) -> Vec<String> {
        ran.data()["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// A zip of `Foo` loading on retail, under the home's `dl/`; its path.
    pub fn foo_zip(home: &Home) -> String {
        let dl = home.env.root().join("dl");
        std::fs::create_dir_all(&dl).unwrap();
        let zip = dl.join("Foo.zip");
        let toc = "## Interface: 120105\n## Version: 1\n";
        std::fs::write(&zip, common::archives::zip_text(&[("Foo/Foo.toc", toc)])).unwrap();
        zip.display().to_string()
    }

    /// `target_not_found` (exit 3) with `fix` (after `omaddons`) and `alternatives` (each
    /// after `omaddons`), `details.selector` the typed value.
    #[track_caller]
    pub fn assert_not_found(ran: &Ran, typed: &str, fix: &[&str], alternatives: &[&[&str]]) {
        ran.fails(3, "target_not_found").assert_private();
        let ctx = ran.run.context();
        let error = &ran.envelope()["error"];
        let argv = |words: &[&str]| -> Value {
            std::iter::once("omaddons")
                .chain(words.iter().copied())
                .collect::<Vec<_>>()
                .into()
        };
        assert_eq!(error["fix_argv"], argv(fix), "{ctx}");
        let got: Vec<&Value> = error["alternatives"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| &a["argv"])
            .collect();
        let want: Vec<Value> = alternatives.iter().map(|a| argv(a)).collect();
        assert_eq!(got, want.iter().collect::<Vec<_>>(), "{ctx}");
        assert_eq!(error["details"]["selector"], typed, "{ctx}");
    }
}

/// (wrong, typed, fix, fix exit, fix code, the ids the fix selects).
type TypoRow<'a> = (
    &'a [&'a str],
    &'a str,
    &'a [&'a str],
    i32,
    Option<&'a str>,
    &'a [&'a str],
);

#[test]
fn a_mistyped_client_gets_the_closest_and_its_fix_runs() {
    let log = tlog!();
    log.step("home");
    let home = typo_home(&log);
    log.step("trees before");
    record_trees(&log, &home);

    let rows: &[TypoRow<'_>] = &[
        (
            &["status", "-t", "retial"],
            "retial",
            &["status", "-t", "retail"],
            0,
            None,
            &["retail"],
        ),
        (
            &["list", "-t", "wtolk"],
            "wtolk",
            &["list", "-t", "wotlk"],
            0,
            None,
            &["wotlk"],
        ),
        (
            &["history", "-t", "retial"],
            "retial",
            &["history", "-t", "retail"],
            0,
            None,
            &[],
        ),
        (
            &["backup", "list", "-t", "retial"],
            "retial",
            &["backup", "list", "-t", "retail"],
            0,
            None,
            &[],
        ),
        (
            &["status", "-t", "vanila"],
            "vanila",
            &["status", "-t", "vanilla"],
            0,
            None,
            &["era"],
        ),
        (
            &["status", "-t", "RETIAL"],
            "RETIAL",
            &["status", "-t", "retail"],
            0,
            None,
            &["retail"],
        ),
        // Nothing to undo on a fresh home.
        (
            &["undo", "-t", "retial"],
            "retial",
            &["undo", "-t", "retail"],
            3,
            Some("not_found"),
            &[],
        ),
    ];
    for (wrong, typed, fix, exit, code, selects) in rows {
        log.step(&format!("wrong: {}", wrong.join(" ")));
        let ran = log.run(&home, &json(wrong));
        assert_not_found(&ran, typed, fix, &[&["targets"]]);
        trees_unchanged(&log, &home);

        log.step(&format!("fix: {}", fix.join(" ")));
        let ran = log.run(&home, &json(fix));
        match code {
            Some(code) => {
                ran.fails(*exit, code);
            }
            None => {
                ran.exit(*exit);
            }
        }
        if *exit == 0 && matches!(fix[0], "status" | "list") {
            let want: Vec<String> = selects.iter().map(|id| (*id).to_owned()).collect();
            log.assert_eq("selected", &targets_of(&ran), &want);
        }
    }

    // Without --json and without a terminal, stderr carries NDJSON events: the error
    // event has the same fix. (Its human form, `fix:` then `  or:` per alternative, needs
    // a terminal; output::stderr's `human_lines_read_naturally` renders it.)
    log.step("human");
    let ran = log.run(&home, &["status", "-t", "retial"]);
    ran.exit(3);
    log.note("stderr is not a terminal here: the error is an NDJSON event");
    let events = ran.events();
    let error = events.iter().find(|e| e["event"] == "error");
    let fix = error.map(|e| (&e["fix"], &e["fix_argv"]));
    let want = (
        Value::from("omaddons status -t retail"),
        Value::from(vec!["omaddons", "status", "-t", "retail"]),
    );
    assert_eq!(fix, Some((&want.0, &want.1)), "{}", ran.run.context());
}

#[test]
fn a_mistyped_client_on_a_command_that_changes_addons_changes_nothing() {
    let log = tlog!();
    log.step("setup");
    let home = typo_home(&log);
    let zip = foo_zip(&home);
    log.run(
        &home,
        &json(&["install", "gh:o/foo", "--file", &zip, "-t", "retail"]),
    )
    .ok();
    log.step("trees before");
    record_trees(&log, &home);

    for (wrong, fix) in [
        (
            &["remove", "foo", "-t", "retial"][..],
            &["remove", "foo", "-t", "retail"][..],
        ),
        (
            &["update", "foo", "-t", "retial", "--offline"],
            &["update", "foo", "-t", "retail", "--offline"],
        ),
        (
            &["pin", "foo", "-t", "retial"],
            &["pin", "foo", "-t", "retail"],
        ),
    ] {
        log.step(&format!("wrong: {}", wrong.join(" ")));
        let ran = log.run(&home, &json(wrong));
        assert_not_found(&ran, "retial", fix, &[&["targets"]]);
        trees_unchanged(&log, &home);
    }

    log.step("fix: remove foo -t retail");
    log.run(&home, &json(&["remove", "foo", "-t", "retail"]))
        .ok()
        .assert_private();
    let ran = log.run(&home, &json(&["list", "-t", "retail"]));
    let addons: Vec<String> = ran.ok().data()["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|t| t["addons"].as_array().into_iter().flatten())
        .map(|a| a["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(!addons.iter().any(|a| a == "Foo"), "{addons:?}");
    for (label, dir) in client_dirs(&home) {
        if label != "retail" {
            log.assert_unchanged(label, &dir);
        }
    }
}

#[test]
fn ids_on_this_machine_win_ties_and_the_listing_stays_last() {
    let log = tlog!();
    log.step("setup");
    let home = typo_home(&log);
    log.run(&home, &json(&["targets", "rename", "wotlk", "sad"]))
        .ok();

    log.step("wrong");
    let ran = log.run(&home, &json(&["status", "-t", "sxd"]));
    assert_not_found(
        &ran,
        "sxd",
        &["status", "-t", "sad"],
        &[&["status", "-t", "sod"], &["targets"]],
    );

    log.step("fix");
    let ran = log.run(&home, &json(&["status", "-t", "sad"]));
    log.assert_eq("selected", &targets_of(ran.ok()), &vec!["sad".to_owned()]);
    log.step("alternative");
    let ran = log.run(&home, &json(&["status", "-t", "sod"]));
    log.assert_eq("selected", &targets_of(ran.ok()), &vec!["era".to_owned()]);
}

#[test]
fn no_guess_without_a_close_client_here() {
    let log = tlog!();
    log.step("home");
    let home = Home::new(&["wow"]);
    log.env(&home);

    for typed in ["sodd", "clasic", "nosuch"] {
        log.step(&format!("status -t {typed}"));
        let ran = log.run(&home, &json(&["status", "-t", typed]));
        assert_not_found(&ran, typed, &["targets"], &[]);
        let message = ran.envelope()["error"]["message"]
            .as_str()
            .unwrap_or_default();
        assert!(!message.contains("closest"), "{message}");
    }

    log.step("status -t sod: the mode's client is missing");
    let ran = log.run(&home, &json(&["status", "-t", "sod"]));
    assert_not_found(&ran, "sod", &["targets", "--rescan"], &[]);
    let message = ran.envelope()["error"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        message.contains("which was not found on this machine"),
        "{message}"
    );

    log.step("control: status -t retial");
    let ran = log.run(&home, &json(&["status", "-t", "retial"]));
    assert_not_found(&ran, "retial", &["status", "-t", "retail"], &[&["targets"]]);
}

#[test]
fn a_mistyped_default_target_in_config_keeps_the_listing() {
    let log = tlog!();
    log.step("setup");
    let home = typo_home(&log);
    log.run(&home, &json(&["config", "set", "default_target", "retial"]))
        .ok();
    let zip = foo_zip(&home);

    log.step("wrong");
    let ran = log.run(
        &home,
        &json(&["install", "gh:o/foo", "--file", &zip, "--offline"]),
    );
    assert_not_found(&ran, "retial", &["targets"], &[]);
    let message = &ran.envelope()["error"]["message"];
    log.assert_eq(
        "message",
        message,
        &Value::from("no target matches \"retial\"; the closest selector is \"retail\""),
    );
}
