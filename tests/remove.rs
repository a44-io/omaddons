//! `omaddons remove` end to end: `--file` installs set up records in synthetic retail
//! and classic clients; removal is checked on disk, in state, through `undo`, and
//! against a fake `/proc` for the running client.

mod common;

use std::fs;
use std::os::unix::fs::symlink;

use common::addons::{addon_folder, toc};
use common::archives::zip_text;
use common::home::{Home, Run};
use common::trees::{listing, tree_hash};
use serde_json::Value;
use support::{argv_of, install, items};

#[cfg(test)]
mod support {
    use super::*;

    /// Installs gh:o/<lowercase name> with `folders` (each with a TOC loading on both
    /// retail and classic) into `target`.
    pub fn install(home: &Home, target: &str, name: &str, folders: &[&str]) -> Run {
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        let zip = dl.join(format!("{name}-{target}.zip"));
        let text = toc("120105, 50504", &["## Version: 3.1"]);
        let tocs: Vec<(String, String)> = folders
            .iter()
            .map(|f| (format!("{f}/{f}.toc"), text.clone()))
            .collect();
        let entries: Vec<(&str, &str)> =
            tocs.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        fs::write(&zip, zip_text(&entries)).unwrap();
        let r = format!("gh:o/{}", name.to_ascii_lowercase());
        let path = zip.display().to_string();
        let run = home.run(&["install", &r, "--file", &path, "-t", target]);
        assert_eq!(run.code, 0, "{}", run.context());
        run
    }

    pub fn items(envelope: &Value) -> &Vec<Value> {
        envelope["data"]["items"].as_array().unwrap()
    }

    pub fn argv_of(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn remove_moves_folders_into_the_snapshot_and_undo_puts_them_back() {
    let home = Home::new(&["wow"]);
    install(&home, "retail", "Foo", &["Foo", "FooOptions"]);
    let addons = home.addons("_retail_");
    let before = tree_hash(&addons);
    let generation = |home: &Home| {
        let dir = home.state().join("targets");
        let record = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.to_string_lossy().ends_with(".json")
                    && !p.to_string_lossy().ends_with(".files.json")
            })
            .unwrap();
        let value: Value = serde_json::from_slice(&fs::read(record).unwrap()).unwrap();
        value["generation"].as_u64().unwrap()
    };
    let installed_generation = generation(&home);

    let r = home.run(&["remove", "Foo", "-t", "retail"]);
    let envelope = r.expect(0);
    let item = &items(&envelope)[0];
    assert_eq!(
        (
            &item["status"],
            &item["name"],
            &item["version"],
            &item["ref"]
        ),
        (
            &Value::from("removed"),
            &Value::from("Foo"),
            &Value::from("3.1"),
            &Value::from("gh:o/foo")
        ),
        "{}",
        r.context()
    );
    assert_eq!(item["folders"], serde_json::json!(["Foo", "FooOptions"]));
    let tx = envelope["data"]["tx"].as_str().unwrap().to_owned();
    assert_eq!(
        argv_of(&envelope["commands"][0]["argv"]),
        ["omaddons", "undo", tx.as_str()]
    );
    assert!(!addons.join("Foo").exists() && !addons.join("FooOptions").exists());
    let snapshot = home
        .client("_retail_")
        .join(format!("Interface/.omaddons/snapshots/{tx}/AddOns"));
    assert!(
        snapshot.join("Foo").is_dir() && snapshot.join("FooOptions").is_dir(),
        "{}",
        listing(&snapshot)
    );
    assert!(generation(&home) > installed_generation);
    let list = home.run(&["list", "-t", "retail"]).expect(0);
    assert_eq!(list["data"]["targets"][0]["addons"], serde_json::json!([]));
    let history = home.run(&["history", "--limit", "1"]).expect(0);
    assert_eq!(history["data"]["entries"][0]["kind"], "remove");

    let r = home.run(&["undo"]);
    r.expect(0);
    assert_eq!(tree_hash(&addons), before, "byte for byte");
    let list = home.run(&["list", "-t", "retail"]).expect(0);
    assert_eq!(list["data"]["targets"][0]["addons"][0]["name"], "Foo");
}

#[test]
fn removal_by_owned_folder_and_by_ref() {
    let home = Home::new(&["wow"]);
    install(&home, "retail", "DBM", &["DBM-Core", "DBM-GUI"]);
    install(&home, "retail", "Bar", &["Bar"]);
    // The addon is named after its core folder; any folder it owns names it.
    let r = home.run(&["remove", "dbm-gui", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(items(&envelope)[0]["name"], "DBM-Core", "{}", r.context());
    assert_eq!(
        items(&envelope)[0]["folders"],
        serde_json::json!(["DBM-Core", "DBM-GUI"])
    );
    let r = home.run(&["remove", "gh:o/bar", "-t", "retail"]);
    assert_eq!(items(&r.expect(0))[0]["name"], "Bar", "{}", r.context());
    assert!(
        fs::read_dir(home.addons("_retail_"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn unmanaged_protected_and_linked_folders_are_never_removed() {
    let home = Home::new(&["wow"]);
    install(&home, "retail", "Foo", &["Foo"]);
    let addons = home.addons("_retail_");
    let t = toc("120105", &[]);
    addon_folder(
        &addons,
        "Blizzard_AuctionUI",
        &[("Blizzard_AuctionUI.toc", &t)],
        &[],
    );
    addon_folder(&addons, "Loose", &[("Loose.toc", &t)], &[]);
    let dev = home.env.root().join("dev/Linked");
    addon_folder(&dev, "", &[("Linked.toc", &t)], &[]);
    symlink(&dev, addons.join("Linked")).unwrap();
    let before = tree_hash(&addons);
    for (name, says) in [
        ("Nope", "not managed by omaddons in retail"),
        ("blizzard_auctionui", "Blizzard folder"),
        ("Loose", "not installed by omaddons"),
        ("Linked", "symlink"),
    ] {
        let r = home.run(&["remove", name, "-t", "retail"]);
        let envelope = r.expect(3);
        let error = &envelope["error"];
        assert_eq!(error["code"], "not_managed", "{}", r.context());
        assert!(
            error["message"].as_str().unwrap().contains(says),
            "{name}: {}",
            r.context()
        );
        assert!(
            error["fix"].as_str().is_some_and(|f| !f.is_empty()),
            "{name}"
        );
    }
    assert_eq!(tree_hash(&addons), before, "nothing touched");
}

#[test]
fn without_t_a_name_in_two_targets_is_ambiguous() {
    let home = Home::new(&["wow", "wow_classic"]);
    install(&home, "retail", "Foo", &["Foo"]);
    install(&home, "classic", "Foo", &["Foo"]);
    let r = home.run(&["remove", "Foo"]);
    let envelope = r.expect(3);
    assert_eq!(
        envelope["error"]["code"],
        "target_ambiguous",
        "{}",
        r.context()
    );
    assert_eq!(
        argv_of(&envelope["error"]["fix_argv"]),
        ["omaddons", "remove", "Foo", "-t", "classic"]
    );
    assert_eq!(
        argv_of(&envelope["error"]["alternatives"][0]["argv"]),
        ["omaddons", "remove", "Foo", "-t", "retail"]
    );
    // Managed in only one target: no -t needed.
    install(&home, "classic", "Bar", &["Bar"]);
    let r = home.run(&["remove", "Bar"]);
    assert_eq!(
        r.expect(0)["data"]["target"]["id"],
        "classic",
        "{}",
        r.context()
    );
}

#[test]
fn partial_and_all_failing() {
    let home = Home::new(&["wow"]);
    install(&home, "retail", "Foo", &["Foo"]);
    let r = home.run(&["remove", "Foo", "Nope", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    assert_eq!(
        (
            &items(&envelope)[0]["status"],
            &items(&envelope)[1]["error"]["code"]
        ),
        (&Value::from("removed"), &Value::from("not_managed"))
    );
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "not_managed"),
        "{}",
        r.context()
    );
    let r = home.run(&["remove", "Nope", "Nada", "-t", "retail"]);
    let envelope = r.expect(3);
    assert_eq!(envelope["error"]["code"], "not_managed");
    assert_eq!(
        envelope["error"]["details"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

/// A target whose AddOns cannot be read fails the removal with the reason.
#[test]
fn an_unreadable_addons_folder_fails_the_removal() {
    let home = Home::new(&["wow"]);
    install(&home, "retail", "Foo", &["Foo"]);
    let addons = home.addons("_retail_");
    fs::rename(&addons, home.client("_retail_").join("AddOns.moved")).unwrap();
    fs::write(&addons, "not a folder").unwrap();
    let r = home.run(&["remove", "Foo", "-t", "retail"]);
    let envelope = r.expect(1);
    assert_eq!(
        (&envelope["error"]["code"], &envelope["data"]),
        (&Value::from("io"), &Value::Null),
        "{}",
        r.context()
    );
}

#[test]
fn a_running_client_and_dry_runs() {
    let mut home = Home::new(&["wow"]);
    install(&home, "retail", "Foo", &["Foo"]);
    let client = tree_hash(&home.client("_retail_"));

    let r = home.run(&["remove", "Foo", "-t", "retail", "--dry-run"]);
    let envelope = r.expect(0);
    assert_eq!(
        (
            &envelope["data"]["dry_run"],
            &envelope["data"]["tx"],
            &items(&envelope)[0]["status"]
        ),
        (&Value::Bool(true), &Value::Null, &Value::from("removed")),
        "{}",
        r.context()
    );
    assert_eq!(
        tree_hash(&home.client("_retail_")),
        client,
        "the dry run wrote nothing"
    );

    home.retail_running();
    let r = home.run(&["remove", "Foo", "-t", "retail"]);
    let envelope = r.expect(4);
    assert_eq!(envelope["error"]["code"], "game_running", "{}", r.context());
    assert_eq!(
        argv_of(&envelope["error"]["fix_argv"]).last().unwrap(),
        "--while-running"
    );
    assert_eq!(tree_hash(&home.client("_retail_")), client);

    let r = home.run(&["remove", "Foo", "-t", "retail", "--while-running"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["reload_required"],
        "restart",
        "{}",
        r.context()
    );
    assert!(!home.addons("_retail_").join("Foo").exists());
}
