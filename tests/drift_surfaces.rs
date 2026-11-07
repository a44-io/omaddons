//! Drift where users look: `status` (stat-first; `--cached` presence only, `--deep`
//! every file), `list -v` and `--deep`, `info`, and the `local_changes_replaced`
//! warning when an install or update replaces files changed by hand.

use crate::common;

use std::fs;

use common::drift::{delete_managed, edit_managed, touch_identical};
use common::http::{Asset, gh_release};
use serde_json::{Value, json};
use support::{argv, drift, foo_assets, installed};

#[cfg(test)]
mod support {
    use serde_json::Value;

    use super::common::addons::toc;
    use super::common::home::Home;
    use super::common::http::{Asset, gh_release};

    pub const FOO: &str = "https://github.com/o/foo";

    /// gh:o/foo `tag`: `Foo` (three files) and `FooOptions`, for retail.
    pub fn foo_assets(tag: &str) -> Vec<(String, String)> {
        let toc_text = toc("120105", &["## Title: Foo", &format!("## Version: {tag}")]);
        vec![
            ("Foo/Foo.toc".into(), toc_text.clone()),
            ("Foo/core.lua".into(), format!("-- core {tag}\n")),
            ("Foo/libs/lib.lua".into(), format!("-- lib {tag}\n")),
            ("FooOptions/FooOptions.toc".into(), toc_text),
        ]
    }

    /// Retail with gh:o/foo v1 installed.
    pub fn installed() -> Home {
        let home = Home::new(&["wow"]);
        let files = foo_assets("v1");
        let entries: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let assets: [Asset<'_>; 1] = [("Foo.zip", &[("mainline", 120_105)], entries)];
        gh_release(&home.http, FOO, "v1", &assets);
        home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
        home
    }

    /// The first target's `drift`.
    pub fn drift(envelope: &Value) -> Value {
        envelope["data"]["targets"][0]["drift"].clone()
    }

    /// A JSON argv as strings, without the leading `omaddons`.
    pub fn argv(value: &Value) -> Vec<String> {
        let all: Vec<String> = serde_json::from_value(value.clone()).unwrap();
        all[1..].to_vec()
    }
}

#[test]
fn status_reports_modified_files_and_the_reinstall_clears_them() {
    let home = installed();
    let foo = home.addons("_retail_").join("Foo");
    edit_managed(&foo.join("core.lua"), b"-- edited by hand\n");

    let r = home.run(&["status"]);
    let envelope = r.expect(0);
    let t = &envelope["data"]["targets"][0];
    assert_eq!(t["attention"], json!(["addon_modified"]), "{}", r.context());
    assert_eq!(
        drift(&envelope),
        json!([{"addon": "Foo", "state": "modified", "files": 1}]),
        "counts only without -v"
    );
    let hint = envelope["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["action"] == "reinstall")
        .cloned()
        .unwrap();
    assert_eq!(
        hint["argv"],
        json!(["omaddons", "install", "Foo", "-t", "retail", "--reinstall"])
    );
    assert_eq!(envelope["commands"][0]["action"], "reinstall", "first");

    // -v names the paths.
    let verbose = home.run(&["status", "-v"]).expect(0);
    assert_eq!(
        drift(&verbose)[0]["folders"][0]["modified"],
        json!(["core.lua"])
    );
    // Humans see the issue, the line and what to run next.
    let human = home.exec(&["status".to_owned()]);
    assert!(
        human.stdout.contains(
            "  P2 Foo's files changed on disk (1 file) → omaddons install Foo -t retail --reinstall"
        ) && human
            .stdout
            .contains("Foo on retail: 1 file changed (Foo/core.lua)")
            && human
                .stdout
                .contains("Next: omaddons install Foo -t retail --reinstall"),
        "{}",
        human.context()
    );

    // The hint runs, and the finding is gone.
    let args = argv(&hint["argv"]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    home.run(&args).expect(0);
    let after = home.run(&["status"]).expect(0);
    assert_eq!(drift(&after), json!([]));
    assert_eq!(after["data"]["targets"][0]["attention"], json!([]));
}

#[test]
fn a_touched_file_raises_nothing() {
    let home = installed();
    touch_identical(&home.addons("_retail_").join("Foo/core.lua"));
    let envelope = home.run(&["status"]).expect(0);
    assert_eq!(drift(&envelope), json!([]));
    assert_eq!(envelope["data"]["needs_attention"], false);
}

#[test]
fn cached_status_checks_presence_only() {
    let home = installed();
    let addons = home.addons("_retail_");
    // An edit is not seen without a scan.
    edit_managed(&addons.join("Foo/core.lua"), b"-- edited\n");
    let envelope = home.run(&["status", "--cached"]).expect(0);
    assert_eq!(drift(&envelope), json!([]));
    // A folder gone is.
    delete_managed(&addons.join("FooOptions"));
    let r = home.run(&["status", "--cached"]);
    let envelope = r.expect(0);
    assert_eq!(
        drift(&envelope),
        json!([{"addon": "Foo", "state": "missing", "files": 0}]),
        "{}",
        r.context()
    );
    assert_eq!(
        envelope["data"]["targets"][0]["attention"],
        json!(["addon_folder_missing"])
    );
}

#[test]
fn deep_status_rehashes_what_stat_trusts_and_conflicts_with_cached() {
    let home = installed();
    let file = home.addons("_retail_").join("Foo/core.lua");
    // Same size, mtime put back: outside the racy window, only a hash can tell.
    let before = fs::metadata(&file).unwrap().modified().unwrap();
    fs::write(&file, b"-- CORE v1\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(before)
        .unwrap();
    let stat = home.run(&["status"]).expect(0);
    assert_eq!(drift(&stat), json!([]), "stat trusts it");
    let deep = home.run(&["status", "--deep"]).expect(0);
    assert_eq!(deep["data"]["targets"][0]["drift"][0]["state"], "modified");
    let r = home.run(&["status", "--deep", "--cached"]);
    assert_eq!(r.expect(2)["error"]["code"], "usage");
}

#[test]
fn check_exits_6_on_drift_with_the_reinstall_as_its_fix() {
    let home = installed();
    edit_managed(
        &home.addons("_retail_").join("Foo/core.lua"),
        b"-- edited\n",
    );
    let r = home.run(&["status", "--check"]);
    let envelope = r.expect(6);
    assert_eq!(
        envelope["error"]["fix_argv"],
        json!(["omaddons", "install", "Foo", "-t", "retail", "--reinstall"]),
        "{}",
        r.context()
    );
    assert!(
        envelope["data"]["targets"][0]["drift"].is_array(),
        "data kept"
    );
}

#[test]
fn list_and_info_show_drift_on_request() {
    let home = installed();
    edit_managed(
        &home.addons("_retail_").join("Foo/core.lua"),
        b"-- edited\n",
    );
    let plain = home.run(&["list"]).expect(0);
    let addon = &plain["data"]["targets"][0]["addons"][0];
    assert!(addon.get("drift").is_none(), "plain list is unchanged");
    let verbose = home.run(&["list", "-v"]).expect(0);
    let addon = &verbose["data"]["targets"][0]["addons"][0];
    assert_eq!(
        (&addon["drift"]["state"], &addon["drift"]["files"]),
        (&json!("modified"), &json!(1))
    );
    assert_eq!(
        addon["drift"]["folders"][0]["modified"],
        json!(["core.lua"])
    );
    let deep = home.run(&["list", "--deep"]).expect(0);
    let addon = &deep["data"]["targets"][0]["addons"][0];
    assert_eq!(addon["drift"]["state"], "modified");
    assert!(addon["drift"].get("folders").is_none(), "paths need -v");

    let info = home.run(&["info", "gh:o/foo", "--offline"]).expect(0);
    assert_eq!(
        info["data"]["installed"][0]["drift"],
        json!({"state": "modified", "files": 1})
    );
}

#[test]
fn an_update_over_changed_files_warns_and_undo_brings_them_back() {
    let mut home = installed();
    let v2 = foo_assets("v2");
    let entries: Vec<(&str, &str)> = v2.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let assets: [Asset<'_>; 1] = [("Foo.zip", &[("mainline", 120_105)], entries)];
    home.scenario("v2");
    gh_release(&home.http, support::FOO, "v2", &assets);
    let file = home.addons("_retail_").join("Foo/core.lua");
    edit_managed(&file, b"-- edited by hand\n");

    // A dry run says so, with the snapshot as its fix.
    let r = home.run(&["update", "--dry-run", "--refresh"]);
    let envelope = r.expect(0);
    let warning = local_changes(&envelope);
    assert!(warning.is_some(), "{}", r.context());
    let warning = warning.unwrap();
    assert_eq!(warning["fix_argv"], Value::Null);
    assert!(
        warning["fix"]
            .as_str()
            .unwrap()
            .starts_with("the replaced files are kept in the transaction snapshot")
    );

    // The update replaces them all the same, and its undo is the fix.
    let r = home.run(&["update", "--refresh"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["targets"][0]["items"][0]["status"],
        "updated",
        "{}",
        r.context()
    );
    let warning = local_changes(&envelope).unwrap();
    assert!(
        warning["message"]
            .as_str()
            .unwrap()
            .contains("replaced 1 locally changed file (Foo/core.lua)"),
        "{warning}"
    );
    let tx = envelope["data"]["targets"][0]["tx"].as_str().unwrap();
    assert_eq!(warning["fix_argv"], json!(["omaddons", "undo", tx]));
    assert_eq!(fs::read_to_string(&file).unwrap(), "-- core v2\n");
    let undo = argv(&warning["fix_argv"]);
    let undo: Vec<&str> = undo.iter().map(String::as_str).collect();
    home.run(&undo).expect(0);
    assert_eq!(fs::read_to_string(&file).unwrap(), "-- edited by hand\n");
}

/// The `local_changes_replaced` warning of an envelope.
fn local_changes(envelope: &Value) -> Option<Value> {
    envelope["warnings"]
        .as_array()?
        .iter()
        .find(|w| w["code"] == "local_changes_replaced")
        .cloned()
}
