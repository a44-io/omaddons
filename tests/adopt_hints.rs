//! Hints and fixes that point at `adopt` (E5.3), end to end: an unmanaged folder named
//! by `update`, `pin` or `remove`, installed over by `install` or `sync`, or found by
//! `outdated` and `update` on a client that manages nothing yet, is adopted where it is.
//! Every argv given runs.

mod common;

use common::addons::{addon_folder, toc};
use common::graph::{Folder, Node, release_graph};
use common::home::Home;
use common::trees::tree_hash;
use serde_json::{Value, json};
use support::{argv, commands_of, run_argv, unmanaged_foo};

#[cfg(test)]
mod support {
    use std::fs;

    use serde_json::Value;

    use super::common::addons::{addon_folder, toc};
    use super::common::graph::{Folder, Node, release_graph};
    use super::common::home::{Home, Run};

    /// A retail folder requiring nothing.
    pub const fn folder(name: &str) -> Folder<'_> {
        Folder {
            name,
            interface: "120105",
            deps: &[],
            optional: &[],
        }
    }

    /// Retail with Foo (in `catalog.d` as `gh:o/foo`), Bar and Baz released, and `Foo`
    /// in AddOns, put there by hand.
    pub fn unmanaged_foo() -> Home {
        let home = Home::new(&["wow"]);
        let node = |base, folders| Node {
            base,
            tag: "v1",
            folders,
        };
        release_graph(
            &home.http,
            &[
                node("https://github.com/o/foo", &[folder("Foo")]),
                node("https://github.com/o/bar", &[folder("Bar")]),
                node("https://github.com/o/baz", &[folder("Baz")]),
            ],
        );
        let catalog_d = home.env.config().join("omaddons/catalog.d");
        fs::create_dir_all(&catalog_d).unwrap();
        fs::write(
            catalog_d.join("50-foo.toml"),
            "schema_version = \"1.0\"\n\n[[addon]]\nname = \"Foo\"\nref = \"gh:o/foo\"\nfolders = [\"Foo\"]\n",
        )
        .unwrap();
        addon_folder(
            &home.addons("_retail_"),
            "Foo",
            &[("Foo.toc", &toc("120105", &[]))],
            &[("mine.lua", b"x")],
        );
        home
    }

    /// The strings of a JSON argv.
    pub fn argv(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_owned())
            .collect()
    }

    /// Runs `argv` (`omaddons …`, as a hint gives it) with `--json`.
    pub fn run_argv(home: &Home, argv: &[String]) -> Run {
        let mut args = argv[1..].to_vec();
        args.push("--json".to_owned());
        home.exec(&args)
    }

    /// `(action, argv)` of an envelope's commands.
    pub fn commands_of(envelope: &Value) -> Vec<(String, Vec<String>)> {
        envelope["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["action"].as_str().unwrap().to_owned(), argv(&c["argv"])))
            .collect()
    }
}

/// The retail record of `name`, as `(ref, folders)`.
fn recorded(home: &Home, name: &str) -> Option<(String, Vec<String>)> {
    let record = common::state::record(home, "retail");
    record
        .addons
        .values()
        .find(|a| a.name.eq_ignore_ascii_case(name))
        .map(|a| {
            (
                a.r#ref.to_string(),
                a.folders.iter().map(|f| f.as_str().to_owned()).collect(),
            )
        })
}

fn adopt_foo() -> Vec<String> {
    [
        "omaddons", "adopt", "Foo", "--ref", "gh:o/foo", "-t", "retail",
    ]
    .map(str::to_owned)
    .to_vec()
}

#[test]
fn naming_an_unmanaged_folder_offers_adopt_with_the_catalog_ref_and_it_runs() {
    for command in [
        &["update", "Foo"][..],
        &["pin", "Foo"],
        &["unpin", "Foo"],
        &["remove", "Foo", "-t", "retail"],
    ] {
        let home = unmanaged_foo();
        let tree = tree_hash(&home.addons("_retail_"));
        let r = home.run(command);
        let envelope = r.envelope();
        let error = &envelope["error"];
        assert_eq!(
            (&error["code"], argv(&error["fix_argv"])),
            (&json!("not_managed"), adopt_foo()),
            "{command:?}: {}",
            r.context()
        );
        let r = run_argv(&home, &adopt_foo());
        r.expect(0);
        assert_eq!(
            recorded(&home, "Foo"),
            Some(("gh:o/foo".to_owned(), vec!["Foo".to_owned()])),
            "{command:?}: {}",
            r.context()
        );
        assert_eq!(
            tree_hash(&home.addons("_retail_")),
            tree,
            "adopted as it is"
        );
        // Now the command itself works.
        assert_eq!(home.run(command).code, 0, "{command:?}");
    }
}

#[test]
fn a_folder_the_catalog_does_not_know_is_adopted_by_name_alone() {
    let home = unmanaged_foo();
    addon_folder(
        &home.addons("_retail_"),
        "Mine",
        &[("Mine.toc", &toc("120105", &["## Title: Mine"]))],
        &[],
    );
    let r = home.run(&["pin", "Mine"]);
    let envelope = r.envelope();
    assert!(envelope["error"]["fix_argv"].is_array(), "{}", r.context());
    assert_eq!(
        argv(&envelope["error"]["fix_argv"]),
        ["omaddons", "adopt", "Mine", "-t", "retail"],
        "{}",
        r.context()
    );
    // adopt identifies it, or fails saying how to name its ref.
    let r = run_argv(&home, &argv(&envelope["error"]["fix_argv"]));
    let done = r.envelope();
    assert_eq!(
        (&done["error"]["code"], &done["error"]["fix"]),
        (
            &json!("not_found"),
            &json!("omaddons adopt --ref <ref> Mine -t retail")
        ),
        "{}",
        r.context()
    );
}

#[test]
fn installing_or_syncing_onto_an_unmanaged_folder_offers_adopt_under_that_ref() {
    // sync passes install's adopt fix through.
    let home = unmanaged_foo();
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    let r = home.run(&["sync"]);
    let envelope = r.envelope();
    // Its only item failed, so the item's error is the command's.
    let error = &envelope["error"];
    assert!(error["fix_argv"].is_array(), "{}", r.context());
    assert_eq!(
        (&error["code"], argv(&error["fix_argv"])),
        (&json!("unmanaged_folder"), adopt_foo()),
        "{}",
        r.context()
    );
    run_argv(&home, &adopt_foo()).expect(0);
    let r = home.run(&["sync"]);
    assert_eq!(
        r.expect(0)["data"]["summary"]["failed"],
        0,
        "{}",
        r.context()
    );
    assert!(recorded(&home, "Foo").is_some());
}

#[test]
fn an_update_shipping_an_unmanaged_folder_keeps_the_install_force_fix() {
    let home = unmanaged_foo();
    home.run(&["adopt", "Foo", "--ref", "gh:o/foo", "-t", "retail"])
        .expect(0);
    addon_folder(
        &home.addons("_retail_"),
        "FooExtra",
        &[("FooExtra.toc", &toc("120105", &[]))],
        &[],
    );
    let v2 = [support::folder("Foo"), support::folder("FooExtra")];
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/foo",
            tag: "v2",
            folders: &v2,
        }],
    );
    let r = home.run(&["update", "foo", "--refresh"]);
    let envelope = r.envelope();
    assert_eq!(
        (&envelope["error"]["code"], &envelope["error"]["fix_argv"]),
        (
            &json!("unmanaged_folder"),
            &json!(["omaddons", "install", "Foo", "--force", "-t", "retail"])
        ),
        "a managed addon cannot adopt another copy of itself\n{}",
        r.context()
    );
}

#[test]
fn outdated_and_update_hint_adopt_only_where_nothing_is_managed_yet() {
    let home = unmanaged_foo();
    let hint = (
        "adopt".to_owned(),
        ["omaddons", "adopt", "-t", "retail"]
            .map(str::to_owned)
            .to_vec(),
    );
    for command in [&["outdated"][..], &["update"]] {
        let r = home.run(command);
        let envelope = r.expect(0);
        assert!(
            commands_of(&envelope).contains(&hint),
            "{command:?}: {}",
            r.context()
        );
    }
    // The hint runs: a report.
    run_argv(&home, &hint.1).expect(0);

    // Once the client manages something, `status` says it instead.
    home.run(&["install", "gh:o/bar", "-t", "retail"]).expect(0);
    for command in [&["outdated"][..], &["update"]] {
        let envelope = home.run(command).expect(0);
        assert!(
            !commands_of(&envelope).iter().any(|(a, _)| a == "adopt"),
            "{command:?}: {envelope}"
        );
    }
    let status = home.run(&["status"]).expect(0);
    assert!(commands_of(&status).contains(&hint), "{status}");
    assert!(
        !commands_of(&status)
            .iter()
            .any(|(a, _)| a == "list_unmanaged"),
        "{status}"
    );
}

#[test]
fn unknown_unmanaged_folders_alone_never_hint_adopt() {
    let home = Home::new(&["wow"]);
    let node = [Folder {
        name: "Bar",
        interface: "120105",
        deps: &[],
        optional: &[],
    }];
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/bar",
            tag: "v1",
            folders: &node,
        }],
    );
    addon_folder(
        &home.addons("_retail_"),
        "Random",
        &[("Random.toc", &toc("120105", &[]))],
        &[],
    );
    for command in [&["outdated"][..], &["update"], &["status"]] {
        let envelope = home.run(command).expect(0);
        let actions: Vec<String> = commands_of(&envelope).into_iter().map(|(a, _)| a).collect();
        assert!(
            !actions.contains(&"adopt".to_owned()),
            "{command:?}: {envelope}"
        );
    }
    let status: Value = home.run(&["status"]).expect(0);
    assert!(
        commands_of(&status)
            .iter()
            .any(|(a, _)| a == "list_unmanaged"),
        "{status}"
    );
}
