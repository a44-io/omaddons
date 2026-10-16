//! Several addons in one transaction, judged against one working view (M2-D6): two
//! releases that ship the same folder conflict (no `--force` can settle it), a folder
//! one addon's update drops and another's install ships is handed over in either
//! argument order and undone byte for byte, a ref asked for twice installs once, and a
//! dry run is deterministic.

mod common;

use common::home::Home;
use common::http::gh_release;
use common::trees::tree_hash;
use serde_json::{Value, json};
use support::{A, B, Rel, items};

#[cfg(test)]
mod support {
    use serde_json::Value;

    use super::common::addons::toc;
    use super::common::http::Asset;

    pub const A: &str = "https://github.com/o/a";
    pub const B: &str = "https://github.com/o/b";

    /// A release's one asset `<file>.zip`, one folder per name, each with a TOC.
    pub struct Rel {
        file: String,
        entries: Vec<(String, String)>,
    }

    impl Rel {
        pub fn new(file: &str, tag: &str, folders: &[&str]) -> Self {
            let entries = folders
                .iter()
                .map(|f| {
                    (
                        format!("{f}/{f}.toc"),
                        toc("120105", &[&format!("## Version: {tag}")]),
                    )
                })
                .collect();
            Self {
                file: format!("{file}.zip"),
                entries,
            }
        }

        pub fn assets(&self) -> [Asset<'_>; 1] {
            const MAINLINE: &[(&str, u32)] = &[("mainline", 120_105)];
            [(
                self.file.as_str(),
                MAINLINE,
                self.entries
                    .iter()
                    .map(|(p, t)| (p.as_str(), t.as_str()))
                    .collect(),
            )]
        }
    }

    /// Waits until `ready` holds (10 s at most).
    pub fn poll(what: &str, ready: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// `(request, status)` per item of an install envelope.
    pub fn items(envelope: &Value) -> Vec<(String, String)> {
        envelope["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["request"].as_str().unwrap().to_owned(),
                    i["status"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
}

fn pair(request: &str, status: &str) -> (String, String) {
    (request.to_owned(), status.to_owned())
}

#[test]
fn two_releases_shipping_one_folder_conflict_within_the_transaction() {
    for force in [false, true] {
        let home = Home::new(&["wow"]);
        let (s1, s2) = (
            Rel::new("S1", "v1", &["S1", "Shared"]),
            Rel::new("S2", "v1", &["S2", "Shared"]),
        );
        gh_release(&home.http, "https://github.com/o/s1", "v1", &s1.assets());
        gh_release(&home.http, "https://github.com/o/s2", "v1", &s2.assets());
        let mut args = vec!["install", "gh:o/s1", "gh:o/s2", "-t", "retail"];
        if force {
            args.push("--force");
        }
        let r = home.run(&args);
        let envelope = r.expect(0);
        assert_eq!(
            items(&envelope),
            [pair("gh:o/s1", "installed"), pair("gh:o/s2", "failed")],
            "{}",
            r.context()
        );
        assert_eq!(envelope["partial"], true);
        let error = &envelope["data"]["items"][1]["error"];
        assert_eq!(error["code"], "ownership_conflict");
        assert_eq!(error["details"]["within_transaction"], true);
        let message = error["message"].as_str().unwrap();
        assert!(
            message.contains("S1") && message.contains("S2"),
            "{message}"
        );
        assert_eq!(error["fix_argv"], Value::Null, "no command settles it");
        assert!(home.addons("_retail_").join("Shared").is_dir());
        assert!(!home.addons("_retail_").join("S2").exists());
    }
}

/// A at v1 (folders A and Old) while v2 ships only A, and B (B and Old) published.
fn handover_home() -> Home {
    let mut home = Home::new(&["wow"]);
    let (a1, a2) = (
        Rel::new("A", "v1", &["A", "Old"]),
        Rel::new("A", "v2", &["A"]),
    );
    home.installed_behind(A, ("v1", &a1.assets()), ("v2", &a2.assets()), &["retail"]);
    let b = Rel::new("B", "v1", &["B", "Old"]);
    gh_release(&home.http, B, "v1", &b.assets());
    home
}

#[test]
fn a_dropped_folder_is_handed_over_in_either_order_and_undone_byte_for_byte() {
    for order in [["gh:o/a", "gh:o/b"], ["gh:o/b", "gh:o/a"]] {
        let home = handover_home();
        let before = tree_hash(&home.addons("_retail_"));
        let r = home.run(&["install", order[0], order[1], "-t", "retail"]);
        let envelope = r.expect(0);
        let mut got = items(&envelope);
        got.sort();
        assert_eq!(
            got,
            [pair("gh:o/a", "updated"), pair("gh:o/b", "installed")],
            "{order:?}\n{}",
            r.context()
        );
        assert_eq!(envelope["partial"], false);
        let codes: Vec<&str> = envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w["code"].as_str())
            .collect();
        assert!(!codes.contains(&"ownership_transferred"), "{codes:?}");

        let line = common::state::history(&home).pop().unwrap();
        let old: Vec<_> = line
            .ops
            .iter()
            .filter(|o| o.folder.as_str() == "Old")
            .collect();
        assert_eq!(old.len(), 1, "{:?}", line.ops);
        assert_eq!(
            (old[0].op, old[0].addon.as_str()),
            (omaddons::state::history::FolderOp::Replace, "B")
        );
        let record = common::state::record(&home, "retail");
        let (a, b) = (&record.addons["a"], &record.addons["b"]);
        assert_eq!(a.state, omaddons::model::AddonState::Ok);
        assert_eq!(
            a.folders.iter().map(|f| f.as_str()).collect::<Vec<_>>(),
            ["A"]
        );
        assert!(b.folders.iter().any(|f| f.as_str() == "Old"));

        home.run(&["undo"]).expect(0);
        assert_eq!(tree_hash(&home.addons("_retail_")), before, "{order:?}");
        let record = common::state::record(&home, "retail");
        assert!(!record.addons.contains_key("b"));
        assert!(
            record.addons["a"]
                .folders
                .iter()
                .any(|f| f.as_str() == "Old")
        );
    }
}

#[test]
fn a_ref_asked_for_twice_installs_once() {
    let home = Home::new(&["wow"]);
    let a = Rel::new("A", "v1", &["A"]);
    gh_release(&home.http, A, "v1", &a.assets());
    let r = home.run(&["install", "gh:o/a", "gh:O/A", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [pair("gh:o/a", "installed"), pair("gh:O/A", "skipped")],
        "{}",
        r.context()
    );
    let duplicate = &envelope["data"]["items"][1];
    assert_eq!(
        (
            &duplicate["skip_reason"],
            &duplicate["duplicate_of"],
            &duplicate["error"]
        ),
        (&json!("duplicate"), &json!("gh:o/a"), &Value::Null)
    );
    assert_eq!(envelope["partial"], false);
    assert_eq!(common::state::history(&home).len(), 1);
}

/// Starts the handover install paused after planning, lets `meanwhile` run to
/// completion, and returns the paused command's result.
fn while_planned(home: &Home, meanwhile: &[&str]) -> common::home::Run {
    let argv = ["install", "gh:o/a", "gh:o/b", "-t", "retail"];
    let child = home.spawn(
        &argv,
        &[("OMADDONS_FAILPOINTS", "engine.plan.done=sleep:2000")],
    );
    let inflight = home.state().join("inflight");
    support::poll("the install to plan", || {
        std::fs::read_dir(&inflight).is_ok_and(|mut d| d.next().is_some())
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    home.run(meanwhile).expect(0);
    common::home::wait(child, &argv)
}

#[test]
fn a_transaction_landing_between_plan_and_lock_is_replayed_against() {
    // Unrelated: both commit, and the handover is still one replace.
    let home = handover_home();
    let d = Rel::new("D", "v1", &["D"]);
    gh_release(&home.http, "https://github.com/o/d", "v1", &d.assets());
    let r = while_planned(&home, &["install", "gh:o/d", "-t", "retail"]);
    let envelope = r.expect(0);
    assert!(envelope["data"]["tx"].is_string(), "{}", r.context());
    let line = common::state::history(&home).pop().unwrap();
    assert_eq!(
        line.ops
            .iter()
            .filter(|o| o.folder.as_str() == "Old")
            .count(),
        1
    );
    assert!(
        common::state::record(&home, "retail")
            .addons
            .contains_key("d")
    );

    // One that takes a folder B claims: the replay refuses, plan_stale.
    let home = handover_home();
    let c = Rel::new("C", "v1", &["C", "B"]);
    gh_release(&home.http, "https://github.com/o/c", "v1", &c.assets());
    let before = tree_hash(&home.addons("_retail_"));
    let r = while_planned(&home, &["install", "gh:o/c", "-t", "retail"]);
    let text = r.stdout.clone();
    assert!(text.contains("\"plan_stale\""), "{}", r.context());
    assert!(
        text.contains("ownership_conflict"),
        "the cause\n{}",
        r.context()
    );
    assert_ne!(tree_hash(&home.addons("_retail_")), before, "C committed");
    assert!(
        !common::state::record(&home, "retail")
            .addons
            .contains_key("b")
    );
}

#[test]
fn a_dry_run_is_deterministic() {
    let home = handover_home();
    let run = || {
        let r = home.run(&["install", "gh:o/b", "gh:o/a", "-t", "retail", "--dry-run"]);
        let mut envelope = r.expect(0);
        envelope["meta"] = Value::Null;
        envelope.to_string()
    };
    let first = run();
    assert_eq!(run(), first);
    assert!(first.contains("\"replaced\":[\"Old\"]"), "{first}");
}
