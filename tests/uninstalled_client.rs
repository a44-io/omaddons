//! A client Battle.net uninstalled (omad-launch-week-fixes-bmcx.1): Battle.net drops its
//! `.build.info` row and deletes its files except `Interface/` (omaddons' own data and
//! the AddOns), leaving fonts, a loader DLL and Proton's shader caches, as it did on the
//! development machine on 2026-11-04 and 2026-11-06. Such a folder is no client: an
//! unregistered one is no target at all, and a registered one is a missing target that
//! nothing changes, until the client is forgotten or installed again.

use crate::common;

use std::fs;

use common::addons::toc;
use common::home::Home;
use common::http::Asset;
use serde_json::{Value, json};
use support::{actions, ids, reinstall, rows_of, target, uninstall, warning_codes};

#[cfg(test)]
mod support {
    use super::*;

    /// What Battle.net does to `folder` when it uninstalls `product`; returns the
    /// `.build.info` text from before, for a reinstall.
    pub fn uninstall(home: &Home, folder: &str, product: &str) -> String {
        let build_info = home.root.join(".build.info");
        let before = fs::read_to_string(&build_info).unwrap();
        let row_end = format!("|{product}");
        let kept: String = before
            .lines()
            .filter(|line| !line.ends_with(&row_end))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_ne!(kept, before, "{product} had a .build.info row");
        fs::write(&build_info, kept).unwrap();
        let dir = home.client(folder);
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|n| n == "Interface") {
                continue;
            }
            if path.is_dir() {
                fs::remove_dir_all(&path).unwrap();
            } else {
                fs::remove_file(&path).unwrap();
            }
        }
        fs::create_dir_all(dir.join("Fonts")).unwrap();
        fs::write(dir.join("libxell.dll"), "MZ").unwrap();
        fs::write(dir.join("WowClassic_loader.dll"), "MZ").unwrap();
        fs::write(dir.join("vkd3d-proton.cache"), "").unwrap();
        before
    }

    /// Battle.net installing `product` into `folder` again: its row, its `.flavor.info`
    /// and its executable.
    pub fn reinstall(home: &Home, folder: &str, product: &str, exe: &str, build_info: &str) {
        fs::write(home.root.join(".build.info"), build_info).unwrap();
        let dir = home.client(folder);
        fs::write(
            dir.join(".flavor.info"),
            format!("Product Flavor!STRING:0\n{product}\n"),
        )
        .unwrap();
        fs::write(dir.join(exe), "MZ").unwrap();
    }

    pub fn ids(envelope: &Value) -> Vec<String> {
        envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_owned())
            .collect()
    }

    pub fn target<'a>(envelope: &'a Value, id: &str) -> &'a Value {
        envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap_or_else(|| panic!("no target {id}: {envelope}")) // ubs:ignore — test
    }

    pub fn warning_codes(envelope: &Value) -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap().to_owned())
            .collect()
    }

    pub fn actions(envelope: &Value) -> Vec<String> {
        envelope["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["action"].as_str().unwrap().to_owned())
            .collect()
    }

    /// A human run's stdout lines that start with `id` as a word (a row of that client).
    pub fn rows_of(home: &Home, args: &[&str], id: &str) -> Vec<String> {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let ran = home.exec(&argv);
        assert_eq!(ran.code, 0, "{}", ran.context());
        ran.stdout
            .lines()
            .filter(|line| {
                let line = line.trim_start();
                line.strip_prefix(id)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t', ':']))
            })
            .map(str::to_owned)
            .collect()
    }
}

/// Never registered: the uninstalled folder is no target in `status`, `list` or
/// `targets`, in JSON or for a person, and no `unknown_flavor` points at a family
/// override for it.
#[test]
fn an_unregistered_uninstalled_client_is_no_target() {
    let home = Home::new(&["wow", "wow_classic"]);
    uninstall(&home, "_classic_", "wow_classic");
    for (args, cached) in [
        (&["status"][..], false),
        (&["status", "--cached"][..], true),
    ] {
        let envelope = home.run(args).expect(0);
        assert_eq!(
            ids(&envelope),
            ["retail"],
            "{args:?} (cached {cached}): {envelope}"
        );
        assert!(
            !warning_codes(&envelope).contains(&"unknown_flavor".to_owned()),
            "{args:?}: {envelope}"
        );
    }
    for args in [&["list"][..], &["targets"][..]] {
        let envelope = home.run(args).expect(0);
        assert_eq!(ids(&envelope), ["retail"], "{args:?}: {envelope}");
        assert!(
            !warning_codes(&envelope).contains(&"unknown_flavor".to_owned()),
            "{args:?}: {envelope}"
        );
    }
    for args in [&["status"][..], &["list"][..], &["targets"][..]] {
        assert_eq!(
            rows_of(&home, args, "classic"),
            Vec::<String>::new(),
            "{args:?}"
        );
    }
}

/// Registered, managing an addon whose update is known: once Battle.net uninstalled
/// it, it is a missing target (`status`, `--cached` and the doctor say so in words
/// true for a folder that is still there), its update no longer counts or is
/// recommended, a command naming it is refused, a reinstall finds it again under its
/// key, and `targets rm` forgets it for good.
#[test]
fn a_registered_uninstalled_client_is_a_missing_target() {
    let mut home = Home::new(&["wow", "wow_classic"]);
    let (t1, t2) = (
        toc("50504", &["## Version: v1"]),
        toc("50504", &["## Version: v2"]),
    );
    let a1: [Asset<'_>; 1] = [("Foo.zip", &[], vec![("Foo/Foo.toc", t1.as_str())])];
    let a2: [Asset<'_>; 1] = [("Foo.zip", &[], vec![("Foo/Foo.toc", t2.as_str())])];
    home.installed_behind(
        "https://github.com/o/foo",
        ("v1", &a1),
        ("v2", &a2),
        &["classic"],
    );
    home.run(&["outdated"]).expect(0);
    let live = home.run(&["status"]).expect(0);
    let key = target(&live, "classic")["key"].clone();
    assert!(key.is_string(), "registered by its install: {live}");
    assert_eq!(
        (
            &target(&live, "classic")["updates"]["available"],
            &live["data"]["summary"]["updates"]["targets"]
        ),
        (&json!(1), &json!(["classic"])),
        "the control: Foo's update counts while the client is installed: {live}"
    );

    let build_info = uninstall(&home, "_classic_", "wow_classic");
    for args in [&["status"][..], &["status", "--cached"][..]] {
        let envelope = home.run(args).expect(0);
        let classic = target(&envelope, "classic");
        assert_eq!(
            (&classic["missing"], &classic["key"]),
            (&json!(true), &key),
            "{args:?}: {envelope}"
        );
        assert_eq!(
            classic["attention"],
            json!(["target_missing"]),
            "{args:?}: {envelope}"
        );
        let titles: Vec<(&Value, &Value)> = classic["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| (&i["id"], &i["title"]))
            .collect();
        assert_eq!(
            titles,
            [(
                &json!("fm-userland_state-target-vanished"),
                &json!("The client is gone or no longer installed")
            )],
            "{args:?}: {envelope}"
        );
        assert!(
            !warning_codes(&envelope).contains(&"unknown_flavor".to_owned()),
            "{args:?}: {envelope}"
        );
        assert_eq!(
            (
                &envelope["data"]["summary"]["updates"]["available"],
                &envelope["data"]["summary"]["updates"]["targets"]
            ),
            (&json!(0), &json!([])),
            "{args:?}: a missing client's update counts nowhere: {envelope}"
        );
        assert!(
            !actions(&envelope).contains(&"update".to_owned()),
            "{args:?}: no update is recommended for it: {envelope}"
        );
    }

    let (code, doctor) = {
        let ran = home.run(&["doctor", "--only", "fm-userland_state-target-vanished"]);
        (ran.code, ran.envelope())
    };
    assert_eq!(code, 1, "findings present: {doctor}");
    let findings: Vec<(&Value, &Value, &Value)> = doctor["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (&f["id"], &f["title"], &f["target"]["key"]))
        .collect();
    assert_eq!(
        findings,
        [(
            &json!("fm-userland_state-target-vanished"),
            &json!("The client is gone or no longer installed"),
            &key
        )],
        "{doctor}"
    );

    // Nothing reaches the folder Battle.net left: a command naming the client is
    // refused, and a bare update changes no client.
    let tree = common::trees::tree_hash(&home.client("_classic_"));
    let refused = home.run(&["update", "-t", "classic"]).expect(3);
    assert_eq!(
        (
            &refused["error"]["code"],
            &refused["error"]["details"]["missing"]
        ),
        (&json!("target_not_found"), &json!(true)),
        "{refused}"
    );
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains("gone or no longer installed"), "{message}");
    let updated = home.run(&["update"]).expect(0);
    let touched: Vec<&Value> = updated["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| &t["target"]["id"])
        .collect();
    assert_eq!(touched, [&json!("retail")], "{updated}");
    assert_eq!(updated["data"]["summary"]["changed"], 0, "{updated}");
    assert_eq!(
        common::trees::tree_hash(&home.client("_classic_")),
        tree,
        "the leftover folder is as Battle.net left it"
    );

    // Installed again: found under its old key, as the client it was.
    reinstall(
        &home,
        "_classic_",
        "wow_classic",
        "WoWClassic.exe",
        &build_info,
    );
    let again = home.run(&["status"]).expect(0);
    let classic = target(&again, "classic");
    assert_eq!(
        (&classic["missing"], &classic["key"], &classic["family"]),
        (&json!(false), &key, &json!("mists")),
        "{again}"
    );

    // Uninstalled again, then forgotten: listed nowhere, with or without a scan.
    uninstall(&home, "_classic_", "wow_classic");
    home.run(&["targets", "rm", "classic"]).expect(0);
    for args in [
        &["status"][..],
        &["status", "--cached"][..],
        &["targets"][..],
    ] {
        let envelope = home.run(args).expect(0);
        assert_eq!(ids(&envelope), ["retail"], "{args:?}: {envelope}");
        assert!(
            !warning_codes(&envelope).contains(&"unknown_flavor".to_owned()),
            "{args:?}: {envelope}"
        );
    }
}
