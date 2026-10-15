//! Installing into a client that has never been launched. A WoW client creates
//! `Interface/AddOns` on its first launch, so until then the folder is missing (and on a
//! Battle.net flavor folder often `Interface/` too). The first install creates it inside
//! its transaction, says so in `creates_addons_dir` (dry run and real run alike), and
//! undo leaves the empty folder. A case variant (`interface/addons`) is the client's own
//! folder and is used; two variants are refused, since the game reads only one.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::addons::toc;
use common::archives::zip_text;
use common::home::Home;
use common::http::{Asset, gh_release};
use common::trees::{CustomRoot, listing, tree_hash};
use serde_json::Value;
use support::{entries, first_install_into, foo_release};

#[cfg(test)]
mod support {
    use super::*;

    /// gh:o/foo v1.0.0 for retail: `Foo` and `FooOptions`.
    pub fn foo_release(scenario: &Path) {
        let foo = toc("120105", &["## Title: Foo", "## Version: 1.0.0"]);
        let options = toc("120105", &["## Title: Foo Options"]);
        let assets: [Asset<'_>; 1] = [(
            "Foo.zip",
            &[("mainline", 120_105)],
            vec![
                ("Foo/Foo.toc", foo.as_str()),
                ("Foo/core.lua", "-- foo"),
                ("FooOptions/FooOptions.toc", options.as_str()),
            ],
        )];
        gh_release(scenario, "https://github.com/o/foo", "v1.0.0", &assets);
    }

    /// The entries of `dir`, sorted; empty when it does not exist.
    pub fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .map(|l| {
                l.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// history.jsonl, parsed.
    pub fn history_lines(home: &Home) -> Vec<Value> {
        fs::read_to_string(home.state().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// The items' outcomes without volatile values, for comparing a dry run with the run.
    fn summary(envelope: &Value) -> Vec<(Value, Value, Value)> {
        envelope["data"]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| (i["name"].clone(), i["status"].clone(), i["folders"].clone()))
            .collect()
    }

    /// `missing` is removed from the retail client before anything runs.
    pub fn first_install_into(missing: &str) {
        let home = Home::new(&["wow"]);
        foo_release(&home.http);
        let client = home.client("_retail_");
        fs::remove_dir_all(client.join(missing)).unwrap();
        assert!(!home.addons("_retail_").exists());
        let before = tree_hash(&client);

        // The dry run says the folder would be created, and writes nothing.
        let r = home.run(&["install", "gh:o/foo", "-t", "retail", "--dry-run"]);
        let dry = r.expect(0);
        assert_eq!(dry["data"]["creates_addons_dir"], true, "{}", r.context());
        assert_eq!(dry["data"]["items"][0]["status"], "installed");
        assert_eq!(tree_hash(&client), before, "a dry run writes nothing");
        let args: Vec<String> = ["install", "gh:o/foo", "-t", "retail", "--dry-run"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        let human = home.exec(&args);
        assert_eq!(
            human.stdout.lines().next(),
            Some("retail: would create Interface/AddOns (this client has not been launched yet)"),
            "{}",
            human.context()
        );

        // The run creates it and installs, as the dry run said.
        let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
        let envelope = r.expect(0);
        assert_eq!(
            envelope["data"]["creates_addons_dir"],
            true,
            "{}",
            r.context()
        );
        assert_eq!(summary(&envelope), summary(&dry), "{}", r.context());
        assert_eq!(entries(&home.addons("_retail_")), ["Foo", "FooOptions"]);
        let tx = envelope["data"]["tx"].as_str().unwrap().to_owned();
        let line = history_lines(&home).pop().unwrap();
        assert_eq!(
            (&line["tx"], &line["created_addons_dir"]),
            (&Value::from(tx.as_str()), &Value::Bool(true))
        );
        let status = home.run(&["status"]).expect(0);
        assert_eq!(
            status["data"]["targets"][0]["folders"]["managed"], 2,
            "{status}"
        );

        // A second install finds the folder there.
        let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
        assert_eq!(r.expect(0)["data"]["creates_addons_dir"], false);

        // Undo removes the addon and leaves the empty folder the client would create.
        let r = home.run(&["undo"]);
        let undone = r.expect(0);
        assert_eq!(
            undone["data"]["reverted"]["tx"],
            tx.as_str(),
            "{}",
            r.context()
        );
        assert!(home.addons("_retail_").is_dir(), "{}", r.context());
        assert_eq!(entries(&home.addons("_retail_")), Vec::<String>::new());
        let kinds: Vec<Value> = history_lines(&home)
            .iter()
            .map(|l| l["kind"].clone())
            .collect();
        assert_eq!(kinds, [Value::from("install"), Value::from("undo")]);
    }
}

#[test]
fn a_flavor_folder_without_addons_gets_one() {
    first_install_into("Interface/AddOns");
}

#[test]
fn a_flavor_folder_without_interface_gets_one() {
    first_install_into("Interface");
}

#[test]
fn a_custom_client_without_addons_gets_one() {
    let home = Home::new(&["wow"]);
    let legacy = CustomRoot::at(home.env.home().join("Games/wow-335"))
        .version(3, 3, 5, 12_340)
        .build();
    if legacy.join("Interface/AddOns").exists() {
        fs::remove_dir_all(legacy.join("Interface/AddOns")).unwrap();
    }
    fs::create_dir_all(legacy.join("Interface")).unwrap();
    let path = home.env.root().join("Legacy.zip");
    fs::write(
        &path,
        zip_text(&[("Legacy/Legacy.toc", &toc("30300", &[]))]),
    )
    .unwrap();
    let path = path.display().to_string();
    let added = home.run(&[
        "targets",
        "add",
        &legacy.display().to_string(),
        "--name",
        "wotlk",
    ]);
    added.expect(0);
    let r = home.run(&[
        "install",
        "gh:o/legacy",
        "--file",
        &path,
        "-t",
        "wotlk",
        "--dry-run",
    ]);
    assert_eq!(r.expect(0)["data"]["creates_addons_dir"], true);
    let r = home.run(&["install", "gh:o/legacy", "--file", &path, "-t", "wotlk"]);
    let envelope = r.expect(0);
    assert_eq!(
        (
            &envelope["data"]["creates_addons_dir"],
            &envelope["data"]["items"][0]["status"]
        ),
        (&Value::Bool(true), &Value::from("installed")),
        "{}",
        r.context()
    );
    assert_eq!(entries(&legacy.join("Interface/AddOns")), ["Legacy"]);
}

/// A lowercase `interface/addons` is the client's own folder: it is used, and no second
/// spelling appears.
#[test]
fn a_case_variant_of_the_addons_folder_is_used() {
    let home = Home::new(&["wow"]);
    foo_release(&home.http);
    let client = home.client("_retail_");
    fs::remove_dir_all(client.join("Interface")).unwrap();
    fs::create_dir_all(client.join("interface/addons")).unwrap();

    let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["creates_addons_dir"],
        false,
        "{}",
        r.context()
    );
    let interfaces: Vec<String> = entries(&client)
        .into_iter()
        .filter(|n| n.eq_ignore_ascii_case("interface"))
        .collect();
    assert_eq!(interfaces, ["interface"], "{}", listing(&client));
    assert_eq!(
        entries(&client.join("interface")),
        [".omaddons", "addons"],
        "{}",
        listing(&client)
    );
    assert_eq!(
        entries(&client.join("interface/addons")),
        ["Foo", "FooOptions"]
    );
    let status = home.run(&["status"]).expect(0);
    assert_eq!(status["data"]["targets"][0]["folders"]["managed"], 2);

    // Undo works through the same spelling.
    home.run(&["undo"]).expect(0);
    assert_eq!(
        entries(&client.join("interface/addons")),
        Vec::<String>::new()
    );
}

/// Two spellings that differ only in case: the game reads one of them, so neither is
/// written, and the refusal names both.
#[test]
fn two_case_variants_are_refused() {
    for (a, b) in [
        ("Interface/AddOns", "interface/addons"),
        ("Interface/AddOns", "Interface/addons"),
    ] {
        let home = Home::new(&["wow"]);
        foo_release(&home.http);
        let client = home.client("_retail_");
        fs::remove_dir_all(client.join("Interface")).unwrap();
        for dir in [a, b] {
            fs::create_dir_all(client.join(dir)).unwrap();
        }
        let before = tree_hash(&client);
        let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
        let envelope = r.expect(4);
        let error = &envelope["error"];
        assert_eq!(
            (&error["code"], &error["retryable"]),
            (&Value::from("case_collision"), &Value::Bool(false)),
            "{}",
            r.context()
        );
        let paths: Vec<PathBuf> = error["details"]["paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| PathBuf::from(p.as_str().unwrap()))
            .collect();
        let named = |dir: &str| {
            let first = dir.split('/').next().unwrap();
            let wanted = if a.split('/').next() == b.split('/').next() {
                client.join(dir)
            } else {
                client.join(first)
            };
            paths.contains(&wanted)
        };
        assert!(named(a) && named(b), "{paths:?}\n{}", r.context());
        assert_eq!(tree_hash(&client), before, "nothing written");
    }
}
