//! `omaddons remove` end to end: `--file` installs set up records in synthetic retail
//! and classic clients; removal is checked on disk, in state, through `undo`, and
//! against a fake `/proc` for the running client.

use crate::common;

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

/// A running client sharing the target's AddOns directory sees the folder go too: its
/// loaded TOC is refused like the target's own.
#[test]
fn a_running_client_sharing_the_addons_directory_holds_its_loaded_folders() {
    let mut home = Home::new(&["wow_classic_era", "wow_anniversary"]);
    let era = home.addons("_classic_era_");
    let anniversary = home.addons("_anniversary_");
    fs::create_dir_all(&era).unwrap();
    if anniversary.exists() {
        fs::remove_dir_all(&anniversary).unwrap();
    }
    fs::create_dir_all(anniversary.parent().unwrap()).unwrap();
    symlink(&era, &anniversary).unwrap();
    let zip = home.env.root().join("Both.zip");
    let text = toc("11509, 20506", &["## Version: 1"]);
    fs::write(&zip, zip_text(&[("Both/Both.toc", text.as_str())])).unwrap();
    let path = zip.display().to_string();
    home.run(&["install", "gh:o/both", "--file", &path, "-t", "era"])
        .expect(0);

    home.client_running("_anniversary_", "WowClassic.exe");
    let r = home.run(&["remove", "Both", "-t", "era"]);
    let envelope = r.expect(4);
    assert_eq!(
        (
            &envelope["error"]["code"],
            &envelope["error"]["details"]["reason"]
        ),
        (&Value::from("game_running"), &Value::from("loaded_toc")),
        "{}",
        r.context()
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("anniversary's game client is running")),
        "{}",
        r.context()
    );
    assert!(era.join("Both").is_dir(), "nothing removed");

    // Undoing the install is the same removal, so it is refused the same way, by its
    // preview too: an undo is no way around the running policy.
    for args in [&["undo", "--dry-run"][..], &["undo"][..]] {
        let r = home.run(args);
        let envelope = r.expect(4);
        assert_eq!(
            (
                &envelope["error"]["code"],
                &envelope["error"]["details"]["reason"]
            ),
            (&Value::from("game_running"), &Value::from("loaded_toc")),
            "{}",
            r.context()
        );
        assert!(
            envelope["error"]["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("anniversary's game client is running")),
            "{}",
            r.context()
        );
        assert!(era.join("Both").is_dir(), "{args:?}: nothing removed");
    }
    // The player's word settles it, and the outcome says what the running client needs.
    let r = home.run(&["undo", "--while-running"]);
    let envelope = r.expect(0);
    assert_ne!(
        envelope["data"]["reload_required"],
        "none",
        "{}",
        r.context()
    );
    assert!(!era.join("Both").exists());
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

/// A process table omaddons cannot read is not "nothing runs": every client is treated
/// as running, `status` says it cannot tell, and removing a loaded addon is refused
/// until `--while-running`.
#[test]
fn an_unreadable_process_table_fails_closed() {
    use std::os::unix::fs::PermissionsExt;

    if common::procfs::running_as_root() {
        return; // chmod 000 hides nothing from root
    }
    let home = Home::new(&["wow"]);
    install(&home, "retail", "Foo", &["Foo"]);
    let locked = home.env.root().join("proc-locked");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let run = |args: &[&str]| {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.push("--json".to_owned());
        home.exec_with(&argv, &[("OMADDONS_PROC_ROOT", locked.to_str().unwrap())])
    };
    let unknown = |envelope: &Value| {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] == "running_unknown")
            .count()
    };

    let r = run(&["status"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["targets"][0]["running"],
        Value::Null,
        "{}",
        r.context()
    );
    assert_eq!(unknown(&envelope), 1, "{}", r.context());

    let r = run(&["remove", "Foo", "-t", "retail"]);
    let envelope = r.expect(4);
    let error = &envelope["error"];
    assert_eq!(error["code"], "game_running", "{}", r.context());
    assert_eq!(error["details"]["running"], "unknown", "{}", r.context());
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("may be running"),
        "{}",
        r.context()
    );
    assert!(home.addons("_retail_").join("Foo").is_dir());

    let r = run(&["remove", "Foo", "-t", "retail", "--while-running"]);
    let envelope = r.expect(0);
    assert_eq!(unknown(&envelope), 1, "once per command: {}", r.context());
    assert!(!home.addons("_retail_").join("Foo").exists());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Removing what no source gives again (an addon adopted as it is) says the snapshot
/// holds its only copy; removing a release whose archive is cached does not.
#[test]
fn removing_an_only_copy_says_so_and_a_cached_release_does_not() {
    use common::http::{Asset, gh_release};

    let home = Home::new(&["wow"]);
    let only = |envelope: &Value| -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] == "snapshot_only_copy")
            .map(|w| w["message"].as_str().unwrap().to_owned())
            .collect()
    };

    // Zed, unpacked by hand and adopted as it is: no release, no archive.
    let zed_toc = toc("120105", &["## Version: z1"]);
    addon_folder(
        &home.addons("_retail_"),
        "Zed",
        &[("Zed.toc", zed_toc.as_str())],
        &[],
    );
    home.run(&["adopt", "Zed", "--ref", "gh:o/zed", "--no-verify"])
        .expect(0);
    let zed = common::state::record(&home, "retail").addons["zed"].clone();
    assert_eq!((zed.release_id, zed.blob_sha256), (None, None));
    let r = home.run(&["remove", "Zed", "-t", "retail"]);
    let envelope = r.expect(0);
    let warned = only(&envelope);
    assert_eq!(warned.len(), 1, "{}", r.context());
    assert!(
        warned[0].contains("holds the only copy of Zed:"),
        "{warned:?}"
    );

    // Foo from a GitHub release, its archive in the blob cache.
    let text = toc("120105", &["## Version: 1.0"]);
    let assets: [Asset<'_>; 1] = [(
        "Foo-1.0.zip",
        &[("mainline", 120_105)],
        vec![("Foo/Foo.toc", text.as_str())],
    )];
    gh_release(&home.http, "https://github.com/o/foo", "1.0", &assets);
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let r = home.run(&["remove", "Foo", "-t", "retail"]);
    let envelope = r.expect(0);
    assert!(only(&envelope).is_empty(), "{}", r.context());
}
