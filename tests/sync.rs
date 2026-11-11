//! e2e suite: sync, including a locked sync that never stages a damaged cached archive
//!
//! `omaddons sync` end to end: every client made to match `addons.toml` in one
//! `sync`-kind transaction per target under one run, a second run that changes nothing
//! and asks nothing, no updates, pins and channels that follow the file, family
//! sections that never reach a test client, failures per entry, dependencies, and undo.

use crate::common;

use std::fs;

use common::addons::toc;
use common::archives::zip_text;
use common::graph::{Folder, Node, release_graph};
use common::home::Home;
use common::netlog::net_log;
use common::trees::tree_hash;
use omaddons::model::{Channel, TxKind};
use serde_json::{Value, json};
use support::{ON_BOTH, exported, item, items, prune_home, record_of, statuses, target, warned};

#[cfg(test)]
mod support {
    use serde_json::Value;

    use super::common::graph::Folder;
    use super::common::home::Home;

    /// Loads on retail (12.1.5) and the PTR (12.1.0).
    pub const ON_BOTH: &str = "120105, 120100";

    /// Retail and the PTR, with LittleWigs (requiring BigWigs), BigWigs, Foo, Bar and Baz
    /// released, BigWigs named by the catalog.
    pub fn prune_home() -> Home {
        use super::common::graph::{Node, release_graph};
        let home = Home::new(&["wow", "wowt"]);
        let node = |base, folders| Node {
            base,
            tag: "v1",
            folders,
        };
        release_graph(
            &home.http,
            &[
                node(
                    "https://github.com/o/littlewigs",
                    &[folder("LittleWigs", &["BigWigs"])],
                ),
                node("https://github.com/o/bigwigs", &[folder("BigWigs", &[])]),
                node("https://github.com/o/foo", &[folder("Foo", &[])]),
                node("https://github.com/o/bar", &[folder("Bar", &[])]),
                node("https://github.com/o/baz", &[folder("Baz", &[])]),
            ],
        );
        let catalog_d = home.env.config().join("omaddons/catalog.d");
        std::fs::create_dir_all(&catalog_d).unwrap();
        std::fs::write(
            catalog_d.join("50-wigs.toml"),
            "schema_version = \"1.0\"\n\n[[addon]]\nname = \"BigWigs\"\nref = \"gh:o/bigwigs\"\nfolders = [\"BigWigs\"]\n",
        )
        .unwrap();
        home
    }

    /// `export`'s document on `home`, written to `<root>/<name>.toml`.
    pub fn exported(home: &Home, name: &str) -> String {
        let content = home.run(&["export"]).expect(0)["data"]["content"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_default();
        let path = home.env.root().join(format!("{name}.toml"));
        std::fs::write(&path, content).unwrap();
        path.display().to_string()
    }

    pub const fn folder<'a>(name: &'a str, deps: &'a [&'a str]) -> Folder<'a> {
        Folder {
            name,
            interface: ON_BOTH,
            deps,
            optional: &[],
        }
    }

    /// The result of target `id`.
    pub fn target<'v>(envelope: &'v Value, id: &str) -> &'v Value {
        envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["target"]["id"] == id)
            .unwrap()
    }

    /// Target `id`'s items.
    pub fn items<'v>(envelope: &'v Value, id: &str) -> &'v Vec<Value> {
        target(envelope, id)["items"].as_array().unwrap()
    }

    /// Target `id`'s item for `request`; null when there is none.
    pub fn item<'v>(envelope: &'v Value, id: &str, request: &str) -> &'v Value {
        items(envelope, id)
            .iter()
            .find(|i| i["request"] == request)
            .unwrap_or(&Value::Null)
    }

    /// How many warnings carry `code`.
    pub fn warned(envelope: &Value, code: &str) -> usize {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] == code)
            .count()
    }

    /// `(request, status)` of target `id`'s items.
    pub fn statuses(envelope: &Value, id: &str) -> Vec<(String, String)> {
        items(envelope, id)
            .iter()
            .map(|i| {
                (
                    i["request"].as_str().unwrap().to_owned(),
                    i["status"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// Target `id`'s record of the addon named `name`.
    pub fn record_of(home: &Home, id: &str, name: &str) -> Option<omaddons::model::AddonRecord> {
        let record = super::common::state::record(home, id);
        record
            .addons
            .values()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .cloned()
    }
}

fn pair(request: &str, status: &str) -> (String, String) {
    (request.to_owned(), status.to_owned())
}

/// Retail and the PTR, with gh:o/foo (Foo) and gh:o/bar (Bar) at v1.
fn two_clients() -> Home {
    let home = Home::new(&["wow", "wowt"]);
    release_graph(
        &home.http,
        &[
            Node {
                base: "https://github.com/o/foo",
                tag: "v1",
                folders: &[support::folder("Foo", &[])],
            },
            Node {
                base: "https://github.com/o/bar",
                tag: "v1",
                folders: &[support::folder("Bar", &[])],
            },
        ],
    );
    home
}

/// `release.json`-less zip assets of Foo at `tag`.
fn foo_assets(tag: &str) -> Vec<(String, String)> {
    vec![(
        "Foo/Foo.toc".to_owned(),
        toc(ON_BOTH, &[&format!("## Version: {tag}")]),
    )]
}

fn borrowed(entries: &[(String, String)]) -> Vec<(&str, &str)> {
    entries
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect()
}

/// Retail with Foo v1 installed by the binary while v2 is out.
fn foo_behind() -> Home {
    let mut home = Home::new(&["wow"]);
    let (v1, v2) = (foo_assets("v1"), foo_assets("v2"));
    let a1: [common::http::Asset<'_>; 1] = [("Foo.zip", &[], borrowed(&v1))];
    let a2: [common::http::Asset<'_>; 1] = [("Foo.zip", &[], borrowed(&v2))];
    home.installed_behind(
        "https://github.com/o/foo",
        ("v1", &a1),
        ("v2", &a2),
        &["retail"],
    );
    home
}

#[test]
fn a_sync_installs_what_is_missing_once_per_target_and_a_rerun_is_free() {
    let home = two_clients();
    let (retail, ptr) = (home.addons("_retail_"), home.addons("_ptr_"));
    let before = (tree_hash(&retail), tree_hash(&ptr));
    home.write_desired(
        "[family.mainline]\naddons = [\"gh:o/foo\"]\n\n[target.\"ptr\"]\naddons = [\"gh:o/bar\"]\n",
    );
    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("gh:o/foo", "installed")],
        "{}",
        r.context()
    );
    // The family section never reaches the test client; its own section does.
    assert_eq!(statuses(&envelope, "ptr"), [pair("gh:o/bar", "installed")]);
    assert!(retail.join("Foo/Foo.toc").is_file() && !retail.join("Bar").exists());
    assert!(ptr.join("Bar/Bar.toc").is_file() && !ptr.join("Foo").exists());
    for id in ["retail", "ptr"] {
        assert_eq!(target(&envelope, id)["kind"], "sync");
        assert_eq!(target(&envelope, id)["no_section"], false);
    }
    let run = envelope["data"]["run"].as_str().unwrap().to_owned();
    let history = common::state::history(&home);
    assert_eq!(history.len(), 2);
    for line in &history {
        assert_eq!(line.kind, TxKind::Sync);
        assert_eq!(
            line.run.map(|r| r.to_string()).as_deref(),
            Some(run.as_str())
        );
    }
    assert_eq!(envelope["commands"][0]["action"], "undo_run");

    // Again: nothing to do, nothing asked.
    let requests = net_log(&home).len();
    let r = home.run(&["sync", "-v"]);
    let again = r.expect(0);
    assert_eq!(
        net_log(&home).len(),
        requests,
        "{}",
        net_log(&home).context()
    );
    assert_eq!(
        statuses(&again, "retail"),
        [pair("gh:o/foo", "unchanged")],
        "{}",
        r.context()
    );
    assert_eq!(statuses(&again, "ptr"), [pair("gh:o/bar", "unchanged")]);
    assert_eq!(
        again["data"]["summary"],
        json!({"targets": 2, "changed": 0, "unchanged": 2, "skipped": 0, "failed": 0})
    );
    assert!(
        again["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["tx"].is_null())
    );
    assert_eq!(common::state::history(&home).len(), 2);
    let human = home.exec(&["sync".to_owned()]);
    assert!(
        human.stdout.contains("nothing to do (2 clients match")
            && human
                .stdout
                .contains("sync does not update; run `omaddons update`"),
        "{}",
        human.stdout
    );

    // One undo reverts the whole sync.
    let r = home.run(&["undo"]);
    r.expect(0);
    assert_eq!(
        (tree_hash(&retail), tree_hash(&ptr)),
        before,
        "{}",
        r.context()
    );
}

#[test]
fn a_target_without_a_section_is_untouched() {
    let home = two_clients();
    let ptr = home.addons("_ptr_");
    let before = tree_hash(&ptr);
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    let p = target(&envelope, "ptr");
    assert_eq!(
        (&p["no_section"], &p["tx"], &p["items"]),
        (&json!(true), &Value::Null, &json!([])),
        "{}",
        r.context()
    );
    assert_eq!(tree_hash(&ptr), before);
}

#[test]
fn an_installed_addon_behind_its_newest_release_is_never_updated() {
    let home = foo_behind();
    let addons = home.addons("_retail_");
    let before = tree_hash(&addons);
    let requests = net_log(&home).len();
    for text in [
        "[family.mainline]\naddons = [\"gh:o/foo\"]\n",
        // An unpinned version is a lock for --locked, never a move.
        "[[family.mainline.addon]]\nref = \"gh:o/foo\"\nversion = \"v2\"\n",
    ] {
        home.write_desired(text);
        let r = home.run(&["sync", "-v"]);
        let envelope = r.expect(0);
        assert_eq!(
            statuses(&envelope, "retail"),
            [pair("gh:o/foo", "unchanged")],
            "{text}\n{}",
            r.context()
        );
        assert_eq!(items(&envelope, "retail")[0]["to_version"], "v1");
    }
    assert_eq!(net_log(&home).len(), requests, "no request");
    assert_eq!(tree_hash(&addons), before, "folders byte-identical");
}

#[test]
fn pins_follow_the_file_and_never_update() {
    let home = foo_behind();
    let addons = home.addons("_retail_");
    // A pinned entry at another version installs that version, pinned.
    home.write_desired(
        "[[family.mainline.addon]]\nref = \"gh:o/foo\"\nversion = \"v2\"\npinned = true\n",
    );
    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("gh:o/foo", "updated")],
        "{}",
        r.context()
    );
    let foo = record_of(&home, "retail", "Foo").unwrap();
    assert_eq!((foo.version.as_str(), foo.pinned), ("v2", true));

    // The same version without its `v`, as `install --version 2` reads it: nothing to
    // do, and no request.
    let requests = net_log(&home).len();
    home.write_desired(
        "[[family.mainline.addon]]\nref = \"gh:o/foo\"\nversion = \"2\"\npinned = true\n",
    );
    let r = home.run(&["sync", "-v"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("gh:o/foo", "unchanged")],
        "{}",
        r.context()
    );
    assert_eq!(net_log(&home).len(), requests, "{}", r.context());

    // An unpinned entry against the pinned addon: a records-only unpin, no request.
    let (requests, tree) = (net_log(&home).len(), tree_hash(&addons));
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("gh:o/foo", "unpinned")],
        "{}",
        r.context()
    );
    assert_eq!(target(&envelope, "retail")["kind"], "sync");
    let foo = record_of(&home, "retail", "Foo").unwrap();
    assert_eq!((foo.version.as_str(), foo.pinned), ("v2", false));
    assert_eq!((net_log(&home).len(), tree_hash(&addons)), (requests, tree));
    assert_eq!(
        common::state::history(&home).last().unwrap().kind,
        TxKind::Sync
    );
}

#[test]
fn a_channel_change_records_the_channel_when_its_release_is_installed() {
    let home = Home::new(&["wow"]);
    let config = home.env.config().join("omaddons");
    fs::create_dir_all(config.join("MyAddon")).unwrap();
    fs::write(
        config.join("MyAddon/MyAddon.toc"),
        toc("120105", &["## Version: 1.0"]),
    )
    .unwrap();
    // A relative path, against the file's own directory.
    home.write_desired("[[family.mainline.addon]]\nref = \"./MyAddon\"\n");
    home.run(&["sync"]).expect(0);
    let addons = home.addons("_retail_");
    let tree = tree_hash(&addons);

    home.write_desired("[[family.mainline.addon]]\nref = \"./MyAddon\"\nchannel = \"beta\"\n");
    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    let item = &items(&envelope, "retail")[0];
    assert_eq!(item["status"], "edited", "{}", r.context());
    assert_eq!(
        record_of(&home, "retail", "MyAddon").unwrap().channel,
        Channel::Beta
    );
    assert_eq!(tree_hash(&addons), tree, "no folder touched");

    // Straight after: nothing to do, nothing asked.
    let requests = net_log(&home).len();
    let again = home.run(&["sync"]).expect(0);
    assert_eq!(again["data"]["summary"]["changed"], 0);
    assert_eq!(net_log(&home).len(), requests);
    assert!(
        common::state::history(&home)
            .iter()
            .all(|l| l.kind == TxKind::Sync)
    );
}

#[test]
fn a_channel_change_installs_that_channels_release_when_another_is_installed() {
    let home = Home::new(&["wow"]);
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/foo",
            tag: "v2",
            folders: &[support::folder("Foo", &[])],
        }],
    );
    // Foo 1.0 from a file, recorded on beta.
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("foo.zip");
    let text = toc(ON_BOTH, &["## Version: 1.0"]);
    fs::write(&zip, zip_text(&[("Foo/Foo.toc", text.as_str())])).unwrap();
    let zip = zip.display().to_string();
    let r = home.run(&[
        "install",
        "gh:o/foo",
        "--file",
        &zip,
        "--channel",
        "beta",
        "-t",
        "retail",
        "--offline",
    ]);
    r.expect(0);
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("gh:o/foo", "updated")],
        "{}",
        r.context()
    );
    let foo = record_of(&home, "retail", "Foo").unwrap();
    assert_eq!((foo.channel, foo.version.as_str()), (Channel::Stable, "v2"));
}

/// A pinned addon whose pinned entry names another channel switches to it (omad-4h0d.17):
/// a records-only change that keeps its release and its pin, with no `addon_pinned`
/// warning, and a second sync has nothing to do. It was the engine's pinned no-op, run
/// after run, while sync said the client matched the file.
#[test]
fn a_pinned_addon_switches_channel_when_its_pinned_entry_names_another() {
    let home = Home::new(&["wow"]);
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/foo",
            tag: "v1",
            folders: &[support::folder("Foo", &[])],
        }],
    );
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    home.run(&["pin", "Foo", "-t", "retail"]).expect(0);
    let addons = home.addons("_retail_");
    let tree = tree_hash(&addons);
    home.write_desired(
        "[[family.mainline.addon]]\nref = \"gh:o/foo\"\nchannel = \"beta\"\npinned = true\n",
    );

    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    let item = &items(&envelope, "retail")[0];
    assert_eq!(
        (&item["status"], &envelope["data"]["summary"]["changed"]),
        (&Value::from("edited"), &Value::from(1)),
        "{}",
        r.context()
    );
    assert_eq!(warned(&envelope, "addon_pinned"), 0, "{}", r.context());
    let foo = record_of(&home, "retail", "Foo").unwrap();
    assert_eq!(
        (foo.channel, foo.version.as_str(), foo.pinned),
        (Channel::Beta, "v1", true)
    );
    assert_eq!(tree_hash(&addons), tree, "no folder touched");

    let again = home.run(&["sync"]);
    let envelope = again.expect(0);
    assert_eq!(
        envelope["data"]["summary"]["changed"],
        0,
        "{}",
        again.context()
    );
    assert_eq!(warned(&envelope, "addon_pinned"), 0, "{}", again.context());
    assert_eq!(
        record_of(&home, "retail", "Foo").unwrap().channel,
        Channel::Beta
    );
}

#[test]
fn a_target_entry_overrides_the_family_entry_for_the_same_addon() {
    let home = foo_behind();
    let catalog_d = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&catalog_d).unwrap();
    fs::write(
        catalog_d.join("50-foo.toml"),
        "schema_version = \"1.0\"\n\n[[addon]]\nname = \"Foo\"\nref = \"gh:o/foo\"\nfolders = [\"Foo\"]\n",
    )
    .unwrap();
    home.write_desired(
        "[family.mainline]\naddons = [\"Foo\"]\n\n[[target.\"retail\".addon]]\nref = \"gh:o/foo\"\npinned = true\n",
    );
    let r = home.run(&["sync", "-v"]);
    let envelope = r.expect(0);
    // One item, the target's: a records-only pin at the installed release.
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("gh:o/foo", "pinned")],
        "{}",
        r.context()
    );
    let foo = record_of(&home, "retail", "Foo").unwrap();
    assert_eq!((foo.version.as_str(), foo.pinned), ("v1", true));
}

#[test]
fn unresolvable_and_duplicate_entries_fail_their_items() {
    let home = two_clients();
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\",\n  \"nosuchaddon\"]\n");
    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("gh:o/foo", "installed"), pair("nosuchaddon", "failed")]
    );
    let error = &items(&envelope, "retail")[1]["error"];
    assert_eq!(error["code"], "not_found");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("addons.toml line 3"),
        "{error}"
    );
    home.run(&["sync", "--strict"]).expect(6);

    // Two entries, written differently, naming one addon: neither syncs.
    let home = two_clients();
    home.write_desired(
        "[family.mainline]\naddons = [\"gh:o/foo\", \"gh:o/bar\"]\n\n[[family.mainline.addon]]\nname = \"Foo\"\nref = \"https://github.com/o/foo\"\npinned = true\n",
    );
    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    let got = statuses(&envelope, "retail");
    assert_eq!(
        got,
        [
            pair("gh:o/foo", "failed"),
            pair("gh:o/bar", "installed"),
            pair("Foo", "failed")
        ],
        "{}",
        r.context()
    );
    for i in [0, 2] {
        let error = &items(&envelope, "retail")[i]["error"];
        assert_eq!(error["code"], "desired_state_invalid");
        assert!(
            error["fix"]
                .as_str()
                .unwrap()
                .starts_with("keep one of the entries on lines 2 and 4"),
            "{error}"
        );
        // Its details, as the error table promises: where, and why.
        assert_eq!(error["details"]["reason"], "duplicate", "{error}");
        assert!(error["details"]["line"].is_u64(), "{error}");
    }
    assert!(record_of(&home, "retail", "Foo").is_none());
}

#[test]
fn a_missing_or_invalid_file_changes_nothing() {
    let home = two_clients();
    let r = home.run(&["sync"]);
    let envelope = r.expect(3);
    assert_eq!(envelope["error"]["code"], "not_found", "{}", r.context());
    assert!(
        envelope["error"]["fix"]
            .as_str()
            .unwrap()
            .starts_with("pass the file")
    );
    assert!(envelope["error"]["fix_argv"].is_null());
    let r = home.run(&["sync", "elsewhere.toml"]);
    let envelope = r.expect(3);
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("elsewhere.toml")
    );

    let retail = home.addons("_retail_");
    let before = tree_hash(&retail);
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n\n[family.bogus]\n");
    let r = home.run(&["sync"]);
    let envelope = r.expect(2);
    assert_eq!(
        (
            &envelope["error"]["code"],
            &envelope["error"]["details"]["reason"],
            &envelope["error"]["details"]["line"]
        ),
        (
            &json!("desired_state_invalid"),
            &json!("unknown_family"),
            &json!(4)
        ),
        "{}",
        r.context()
    );
    assert_eq!(tree_hash(&retail), before);
    assert!(common::state::history(&home).is_empty());
    assert!(net_log(&home).is_empty());

    // An unknown target section warns; the rest syncs.
    home.write_desired(
        "[family.mainline]\naddons = [\"gh:o/foo\"]\n\n[target.elsewhere]\naddons = [\"x\"]\n",
    );
    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "desired_unknown_target"),
        "{}",
        r.context()
    );
    assert!(retail.join("Foo").is_dir());
}

#[test]
fn dependencies_install_with_their_requester() {
    let home = Home::new(&["wow"]);
    release_graph(
        &home.http,
        &[
            Node {
                base: "https://github.com/o/littlewigs",
                tag: "v1",
                folders: &[support::folder("LittleWigs", &["BigWigs"])],
            },
            Node {
                base: "https://github.com/o/bigwigs",
                tag: "v1",
                folders: &[support::folder("BigWigs", &[])],
            },
        ],
    );
    let catalog_d = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&catalog_d).unwrap();
    fs::write(
        catalog_d.join("50-wigs.toml"),
        "schema_version = \"1.0\"\n\n[[addon]]\nname = \"LittleWigs\"\nref = \"gh:o/littlewigs\"\nfolders = [\"LittleWigs\"]\n\n[[addon]]\nname = \"BigWigs\"\nref = \"gh:o/bigwigs\"\nfolders = [\"BigWigs\"]\n",
    )
    .unwrap();
    home.write_desired("[family.mainline]\naddons = [\"littlewigs\"]\n");

    let r = home.run(&["sync", "--no-deps"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("littlewigs", "installed")],
        "{}",
        r.context()
    );
    assert!(record_of(&home, "retail", "BigWigs").is_none());
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "missing_dependency")
    );
    home.run(&["undo"]).expect(0);

    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [
            pair("littlewigs", "installed"),
            pair("BigWigs", "installed")
        ],
        "{}",
        r.context()
    );
    let bw = record_of(&home, "retail", "BigWigs").unwrap();
    assert_eq!(bw.required_by, ["LittleWigs"]);
    assert_eq!(
        common::state::history(&home).len(),
        3,
        "one transaction each way"
    );
}

#[test]
fn an_ownership_conflict_says_how_to_force_it_with_install() {
    let home = two_clients();
    // Other ships Foo too, which Foo (gh:o/foo) owns.
    let shared = [
        Folder {
            name: "Other",
            interface: ON_BOTH,
            deps: &[],
            optional: &[],
        },
        support::folder("Foo", &[]),
    ];
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/other",
            tag: "v1",
            folders: &shared,
        }],
    );
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    home.write_desired("[family.mainline]\naddons = [\"gh:o/other\", \"gh:o/bar\"]\n");
    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("gh:o/other", "failed"), pair("gh:o/bar", "installed")],
        "{}",
        r.context()
    );
    let error = &items(&envelope, "retail")[0]["error"];
    assert_eq!(error["code"], "ownership_conflict");
    assert_eq!(
        error["fix_argv"],
        json!([
            "omaddons",
            "install",
            "gh:o/other",
            "--force",
            "-t",
            "retail"
        ])
    );
}

#[test]
fn a_dry_run_writes_only_the_cache_and_the_net_log() {
    let home = two_clients();
    let path = home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    let state = home.state();
    let listing = || {
        let mut files: Vec<String> = Vec::new();
        for root in [&state, &home.env.config(), &home.root] {
            for entry in walk(root) {
                let shown = entry.display().to_string();
                if !shown.ends_with("net.log") {
                    files.push(shown);
                }
            }
        }
        files.sort();
        files
    };
    let before = listing();
    let r = home.run(&["sync", "--dry-run"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("gh:o/foo", "installed")],
        "{}",
        r.context()
    );
    assert_eq!(listing(), before);
    assert!(path.is_file());
}

#[test]
fn a_relative_desired_file_is_stored_as_the_absolute_file_the_preview_read() {
    let home = two_clients();
    let cwd = home.env.root().join("work");
    fs::create_dir_all(&cwd).unwrap();
    let file = cwd.join("desired.toml");
    fs::write(&file, "[family.mainline]\naddons = [\"gh:o/foo\"]\n").unwrap();
    let argv = [
        "sync",
        "./desired.toml",
        "-t",
        "retail",
        "--dry-run",
        "--json",
    ]
    .map(str::to_owned)
    .to_vec();
    let output = home
        .std_command(&argv, &[("OMADDONS_NOW", "2026-10-27T12:00:00Z")])
        .current_dir(&cwd)
        .output()
        .unwrap();
    let run = common::home::Run {
        argv,
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    };
    let envelope = run.expect(0);
    let plans = common::plans::plan_files(&home.env);
    assert_eq!(plans.len(), 1, "{}", run.context());
    let stored: Value = serde_json::from_slice(&fs::read(&plans[0]).unwrap()).unwrap();
    assert_eq!(stored["intent"]["command"], "sync", "{stored}");
    assert_eq!(stored["intent"]["file"], file.to_str().unwrap(), "{stored}");
    assert_eq!(stored["intent"]["default_file"], false, "{stored}");
    assert_eq!(
        stored["precondition"]["token"], envelope["data"]["precondition"],
        "{stored}"
    );
    assert_eq!(stored["approval"], envelope["data"]["approval"], "{stored}");
    assert_eq!(stored["plan_id"], envelope["data"]["plan_id"], "{stored}");
    assert!(file.is_file());
    assert!(!home.addons("_retail_").join("Foo").exists());
}

// ---- `sync -`: the desired state from standard input (1n2p.8)

/// `omaddons sync - <args> --json` with `text` piped to its standard input.
fn sync_stdin(home: &Home, args: &[&str], text: &str) -> common::home::Run {
    let mut argv = vec!["sync".to_owned(), "-".to_owned()];
    argv.extend(args.iter().map(|a| (*a).to_owned()));
    argv.push("--json".to_owned());
    home.exec_input(&argv, &[], text.as_bytes())
}

#[test]
fn a_desired_state_on_stdin_previews_exactly_what_its_file_does() {
    let home = two_clients();
    let text = "[family.mainline]\naddons = [\"gh:o/foo\", \"gh:o/bar\"]\n";
    home.write_desired(text);
    let from_file = home.run(&["sync", "--dry-run"]);
    let file = from_file.expect(0);
    let piped = sync_stdin(&home, &["--dry-run"], text);
    let stdin = piped.expect(0);
    assert_eq!(
        stdin["data"]["approval"],
        file["data"]["approval"],
        "{}",
        piped.context()
    );
    for id in ["retail", "ptr"] {
        assert_eq!(
            statuses(&stdin, id),
            statuses(&file, id),
            "{id}: {}",
            piped.context()
        );
    }
    assert_eq!(
        statuses(&stdin, "retail"),
        [pair("gh:o/foo", "installed"), pair("gh:o/bar", "installed")]
    );
    assert!(!home.addons("_retail_").join("Foo").exists(), "a dry run");
}

#[test]
fn a_plan_from_stdin_holds_the_text_and_applies_without_stdin() {
    let home = two_clients();
    let text = "[family.mainline]\naddons = [\"gh:o/foo\"]\n";
    let piped = sync_stdin(&home, &["-t", "retail", "--dry-run"], text);
    let preview = piped.expect(0);
    let plans = common::plans::plan_files(&home.env);
    assert_eq!(plans.len(), 1, "{}", piped.context());
    let stored: Value = serde_json::from_slice(&fs::read(&plans[0]).unwrap()).unwrap();
    assert_eq!(stored["intent"]["file"], "/dev/stdin", "{stored}");
    assert_eq!(stored["intent"]["stdin"], text, "{stored}");
    assert_eq!(stored["intent"]["default_file"], false, "{stored}");
    assert_eq!(
        stored["display_argv"],
        json!(["omaddons", "sync", "-", "-t", "retail"]),
        "{stored}"
    );
    // No file to find again: `plans show` lists no input.
    let id = preview["data"]["plan_id"].as_str().unwrap().to_owned();
    let shown = home.run(&["plans", "show", &id]);
    assert_eq!(
        shown.expect(0)["data"]["inputs"],
        json!([]),
        "{}",
        shown.context()
    );
    // `apply` reads no stdin (a run's stdin is closed): the plan's own text.
    let applied = home.run(&["apply", &id]);
    let envelope = applied.expect(0);
    assert_eq!(
        envelope["data"]["applied"]["display_argv"],
        json!(["omaddons", "sync", "-", "-t", "retail"]),
        "{}",
        applied.context()
    );
    assert!(
        record_of(&home, "retail", "Foo").is_some(),
        "{}",
        applied.context()
    );
    assert!(
        !home.addons("_ptr_").join("Foo").exists(),
        "only the client it previewed"
    );
}

#[test]
fn a_relative_path_on_stdin_is_read_against_the_working_directory() {
    use std::io::Write as _;
    use std::process::Stdio;
    let home = Home::new(&["wow"]);
    let work = home.env.root().join("work");
    fs::create_dir_all(work.join("MyAddon")).unwrap();
    fs::write(
        work.join("MyAddon/MyAddon.toc"),
        toc("120105", &["## Version: 1.0"]),
    )
    .unwrap();
    let argv: Vec<String> = ["sync", "-", "--json"].map(str::to_owned).to_vec();
    let mut child = home
        .std_command(&argv, &[])
        .current_dir(&work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Written, then closed by the drop.
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"[[family.mainline.addon]]\nref = \"./MyAddon\"\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let run = common::home::Run {
        argv,
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    };
    run.expect(0);
    assert!(
        record_of(&home, "retail", "MyAddon").is_some(),
        "{}",
        run.context()
    );
}

#[test]
fn prune_previews_from_stdin_as_from_its_file() {
    let home = prune_home();
    for r in ["gh:o/foo", "gh:o/bar"] {
        home.run(&["install", r, "-t", "retail"]).expect(0);
    }
    let text = "[family.mainline]\naddons = [\"gh:o/foo\"]\n";
    let piped = sync_stdin(&home, &["--prune", "--dry-run"], text);
    let envelope = piped.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("Bar", "removed")],
        "{}",
        piped.context()
    );
    assert!(record_of(&home, "retail", "Bar").is_some(), "a dry run");
}

#[test]
fn stdin_past_its_cap_or_malformed_is_refused_as_a_file_would_be() {
    let home = two_clients();
    // Past the cap: a usage error that says to use a file, before anything is read.
    let cap = omaddons::limits::SYNC_STDIN_MAX_BYTES as usize;
    let long = format!(
        "[family.mainline]\naddons = [\"gh:o/foo\"]\n{}",
        "#".repeat(cap)
    );
    let r = sync_stdin(&home, &["--dry-run"], &long);
    let envelope = r.expect(2);
    assert_eq!(envelope["error"]["code"], "usage", "{}", r.context());
    assert_eq!(
        envelope["error"]["details"]["limit"],
        "SYNC_STDIN_MAX_BYTES"
    );
    let fix = envelope["error"]["fix"].to_string();
    assert!(fix.contains("omaddons sync <path>"), "{fix}");
    assert!(common::plans::plan_files(&home.env).is_empty());
    // Malformed: `desired_state_invalid`, as a malformed file is, naming `<stdin>`.
    let r = sync_stdin(
        &home,
        &[],
        "[family.mainline]\naddons = [\"gh:o/foo\"]\n\n[family.bogus]\n",
    );
    let envelope = r.expect(2);
    assert_eq!(
        (
            &envelope["error"]["code"],
            &envelope["error"]["details"]["reason"],
            &envelope["error"]["details"]["line"],
            &envelope["error"]["details"]["path"],
        ),
        (
            &json!("desired_state_invalid"),
            &json!("unknown_family"),
            &json!(4),
            &json!("<stdin>"),
        ),
        "{}",
        r.context()
    );
    assert!(common::state::history(&home).is_empty());
}

/// Every file and directory under `root`.
fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path.clone());
            }
            out.push(path);
        }
    }
    out
}

#[test]
fn offline_an_unpin_works_and_an_uncached_install_fails() {
    let home = foo_behind();
    home.run(&["pin", "foo"]).expect(0);
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\", \"gh:o/bar\"]\n");
    let r = home.run(&["sync", "--offline"]);
    let envelope = r.expect(0);
    let got = statuses(&envelope, "retail");
    assert_eq!(
        got,
        [pair("gh:o/foo", "unpinned"), pair("gh:o/bar", "failed")],
        "{}",
        r.context()
    );
    assert_eq!(items(&envelope, "retail")[1]["error"]["code"], "offline");
}

/// gh:o/foo at v1 and v2 (latest), for a home that never installed it.
fn foo_v1_and_v2(home: &Home) {
    let (v1, v2) = (foo_assets("v1"), foo_assets("v2"));
    let a1: [common::http::Asset<'_>; 1] = [("Foo.zip", &[], borrowed(&v1))];
    let a2: [common::http::Asset<'_>; 1] = [("Foo.zip", &[], borrowed(&v2))];
    common::http::gh_release_v(&home.http, "https://github.com/o/foo", "v1", &a1);
    common::http::gh_release(&home.http, "https://github.com/o/foo", "v2", &a2);
}

#[test]
fn export_then_locked_sync_reproduces_versions_on_a_fresh_machine() {
    // Old machine: Foo v1, while v2 is out.
    let old = foo_behind();
    let file = exported(&old, "dotfiles");
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("version = \"v1\""), "{text}");

    // New machine, both releases served: locked reproduces v1, plain sync takes v2.
    let new = Home::new(&["wow"]);
    foo_v1_and_v2(&new);
    let r = new.run(&["sync", &file, "--locked"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("Foo", "installed")],
        "{}",
        r.context()
    );
    let foo = record_of(&new, "retail", "Foo").unwrap();
    assert_eq!(
        (
            foo.r#ref.to_string(),
            foo.version.as_str(),
            foo.release_id.as_deref(),
            foo.pinned
        ),
        ("gh:o/foo".to_owned(), "v1", Some("gh:o/foo@v1"), false)
    );
    // Again: nothing to do, no request.
    let before = net_log(&new).len();
    let r = new.run(&["sync", &file, "--locked"]);
    assert_eq!(
        r.expect(0)["data"]["summary"]["changed"],
        0,
        "{}",
        r.context()
    );
    assert_eq!(net_log(&new).len(), before);

    let plain = Home::new(&["wow"]);
    foo_v1_and_v2(&plain);
    plain.run(&["sync", &file]).expect(0);
    assert_eq!(record_of(&plain, "retail", "Foo").unwrap().version, "v2");
}

#[test]
fn a_locked_restore_on_the_same_machine_needs_no_network() {
    let home = foo_behind();
    let file = exported(&home, "dotfiles");
    home.run(&["remove", "Foo", "-t", "retail"]).expect(0);
    let before = net_log(&home).len();
    let r = home.run(&["sync", &file, "--locked", "--offline"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("Foo", "installed")],
        "{}",
        r.context()
    );
    assert_eq!(net_log(&home).len(), before, "the blob and its fact answer");
    assert_eq!(record_of(&home, "retail", "Foo").unwrap().version, "v1");
}

#[test]
fn a_cached_blob_without_a_fact_costs_the_metadata_and_a_bad_one_a_download() {
    let home = foo_behind();
    let file = exported(&home, "dotfiles");
    // The memo forgets what the download taught.
    let paths = common::http::paths(&home.env);
    let (mut memo, _) = omaddons::state::memo::load(&paths);
    memo.candidates.clear();
    home.seed_memo(&memo);
    home.run(&["remove", "Foo", "-t", "retail"]).expect(0);
    let (lines, assets) = (net_log(&home).len(), net_log(&home).count_purpose("asset"));
    let r = home.run(&["sync", &file, "--locked"]);
    r.expect(0);
    assert_eq!(
        net_log(&home).len(),
        lines + 1,
        "the exact release's metadata"
    );
    assert_eq!(
        net_log(&home).count_purpose("asset"),
        assets,
        "{}",
        r.context()
    );

    // A blob whose bytes no longer hash to the entry's sha256 is not used.
    let sha = record_of(&home, "retail", "Foo")
        .unwrap()
        .blob_sha256
        .unwrap();
    fs::write(paths.blob(&sha), b"not the archive").unwrap();
    home.run(&["remove", "Foo", "-t", "retail"]).expect(0);
    let assets = net_log(&home).count_purpose("asset");
    let r = home.run(&["sync", &file, "--locked"]);
    r.expect(0);
    assert_eq!(
        net_log(&home).count_purpose("asset"),
        assets + 1,
        "downloaded again: {}",
        r.context()
    );
    assert_eq!(record_of(&home, "retail", "Foo").unwrap().version, "v1");
}

/// F6 part 4: the archive a lock names, damaged in the cache, is never staged. Offline the
/// locked sync fails `hash_mismatch` and changes nothing; online it downloads the archive
/// again into the same place.
#[test]
fn a_locked_sync_never_stages_a_damaged_blob() {
    use common::tlog::tlog;
    let log = tlog!();
    log.step("home");
    let home = foo_behind();
    log.env(&home);
    let file = exported(&home, "dotfiles");
    home.run(&["remove", "Foo", "-t", "retail"]).expect(0);
    let sha = lock_file::locked_sha(&file);
    let blob = common::http::paths(&home.env).blob(&sha);
    log.step("damage");
    let mut bytes = fs::read(&blob).unwrap();
    let offset = bytes.len() / 2;
    bytes[offset] ^= 0xff;
    fs::write(&blob, &bytes).unwrap();
    log.note(format!("damage: byte {offset} of {}", blob.display()));
    let state = home.state();
    log.tree("blobs", blob.parent().unwrap());
    log.tree("retail", &home.client("_retail_"));
    log.tree("targets", &state.join("targets"));
    let history = fs::read(state.join("history.jsonl")).ok();
    log.step("offline");
    let offline = log.run(&home, &["sync", &file, "--locked", "--offline", "--json"]);
    offline.assert_private();
    offline.fails(1, "hash_mismatch");
    let error = offline.envelope()["error"].clone();
    log.json("error", &error);
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&format!("{}…", &sha[..12])),
        "the message names the damaged archive (its details.blob is omad-r86s's): {error}"
    );
    log.assert_eq(
        "its fix",
        &error["fix_argv"],
        &serde_json::json!(["omaddons", "doctor", "--include-deep"]),
    );
    log.assert_eq(
        "Foo still absent",
        &record_of(&home, "retail", "Foo").is_none(),
        &true,
    );
    log.assert_unchanged("blobs", blob.parent().unwrap());
    log.assert_unchanged("retail", &home.client("_retail_"));
    log.assert_unchanged("targets", &state.join("targets"));
    log.assert_eq(
        "history unchanged",
        &fs::read(state.join("history.jsonl")).ok(),
        &history,
    );
    log.step("online");
    let assets = net_log(&home).count_purpose("asset");
    let online = log.run(&home, &["sync", &file, "--locked", "--json"]);
    online.assert_private();
    online.ok();
    log.assert_eq(
        "v1 installed",
        &record_of(&home, "retail", "Foo").unwrap().version,
        &"v1".to_owned(),
    );
    log.assert_eq(
        "downloaded once more",
        &net_log(&home).count_purpose("asset"),
        &(assets + 1),
    );
    let healed: String = {
        use sha2::{Digest, Sha256};
        Sha256::digest(fs::read(&blob).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    log.assert_eq("healed: it hashes to its name", &healed, &sha);
}

/// gh:o/foo at v1, its `Foo.zip` re-uploaded under the same tag with other bytes (an
/// author's packaging fix, or a compromised account), and v2 (latest); gh:o/bar at v1.
fn foo_reuploaded(home: &Home) {
    let mut v1 = foo_assets("v1");
    v1.push(("Foo/Changes.txt".to_owned(), "re-uploaded\n".to_owned()));
    let v2 = foo_assets("v2");
    let a1: [common::http::Asset<'_>; 1] = [("Foo.zip", &[], borrowed(&v1))];
    let a2: [common::http::Asset<'_>; 1] = [("Foo.zip", &[], borrowed(&v2))];
    common::http::gh_release_v(&home.http, "https://github.com/o/foo", "v1", &a1);
    common::http::gh_release(&home.http, "https://github.com/o/foo", "v2", &a2);
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/bar",
            tag: "v1",
            folders: &[support::folder("Bar", &[])],
        }],
    );
}

/// omad-t8m3: `--locked` pins the archive's bytes, not only its version. On a fresh
/// machine whose source serves v1's `Foo.zip` with other bytes, the locked entry fails
/// `hash_mismatch` (`details.reason: "lock_mismatch"`), naming both digests, and nothing
/// of Foo is installed; an entry locked by version alone installs as before, and
/// `--strict` fails the partial run. The fix, the run without `--locked`, accepts what
/// the source serves, and `export` then locks that.
#[test]
fn a_locked_sync_refuses_an_archive_other_than_the_locked_one() {
    use common::tlog::tlog;
    let log = tlog!();
    log.step("lock on the old machine");
    let old = foo_behind();
    let file = exported(&old, "dotfiles");
    let lock = lock_file::locked_sha(&file);
    // Bar locked by its version alone: no archive hash.
    let mut text = fs::read_to_string(&file).unwrap();
    text.push_str(
        "\n[[family.mainline.addon]]\nname = \"Bar\"\nref = \"gh:o/bar\"\nversion = \"v1\"\n",
    );
    fs::write(&file, &text).unwrap();
    log.note(&text);

    log.step("a fresh machine, v1 re-uploaded upstream");
    let new = Home::new(&["wow"]);
    log.env(&new);
    foo_reuploaded(&new);
    new.run(&["status"]).expect(0);
    let state = new.state();
    log.tree("retail", &new.client("_retail_"));
    let history = fs::read(state.join("history.jsonl")).ok();

    log.step("locked sync");
    let run = log.run(&new, &["sync", &file, "--locked", "--json"]);
    run.assert_private();
    run.ok();
    let envelope = run.envelope().clone();
    assert_eq!(envelope["partial"], true, "{envelope}");
    let foo = item(&envelope, "retail", "Foo").clone();
    log.json("Foo", &foo);
    let error = &foo["error"];
    log.assert_eq("code", &error["code"], &json!("hash_mismatch"));
    log.assert_eq(
        "reason",
        &error["details"]["reason"],
        &json!("lock_mismatch"),
    );
    log.assert_eq("the lock", &error["details"]["expected"], &json!(lock));
    let actual = error["details"]["actual"].as_str().unwrap_or_default();
    assert!(
        actual.len() == 64 && actual != lock,
        "the served archive's own digest: {error}"
    );
    let message = error["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("gh:o/foo")
            && message.contains(&lock[..12])
            && message.contains(&actual[..12]),
        "the message names the addon and both digests: {message}"
    );
    log.assert_eq(
        "its fix: the run without --locked",
        &error["fix_argv"],
        &json!(["omaddons", "sync", file]),
    );
    log.assert_eq(
        "Foo not installed",
        &(record_of(&new, "retail", "Foo").is_none()
            && !new.addons("_retail_").join("Foo").exists()),
        &true,
    );
    log.assert_eq(
        "Bar, locked by its version alone, installed as before",
        &record_of(&new, "retail", "Bar").map(|b| b.version),
        &Some("v1".to_owned()),
    );
    assert_ne!(
        fs::read(state.join("history.jsonl")).ok(),
        history,
        "Bar's transaction"
    );

    log.step("--strict");
    new.run(&["remove", "Bar", "-t", "retail"]).expect(0);
    let strict = log.run(&new, &["sync", &file, "--locked", "--strict", "--json"]);
    strict.exit(6);
    log.assert_eq(
        "Foo still refused",
        &item(strict.envelope(), "retail", "Foo")["error"]["details"]["reason"],
        &json!("lock_mismatch"),
    );

    log.step("the fix, then export");
    let fixed = log.run(&new, &["sync", &file, "--json"]);
    fixed.ok();
    log.assert_eq(
        "Foo installed from what the source serves (the newest: unpinned, unlocked)",
        &record_of(&new, "retail", "Foo").map(|f| f.version),
        &Some("v2".to_owned()),
    );
    let relocked = exported(&new, "relocked");
    let again = log.run(&new, &["sync", &relocked, "--locked", "--json"]);
    again.ok();
    log.assert_eq(
        "the new lock holds: nothing to do",
        &again.envelope()["data"]["summary"]["changed"],
        &json!(0),
    );
}

/// omad-tz8v: a `url:` release is its URL and has no version (the TOC's stands in), so
/// `export` records its archive's `sha256` alone, and `--locked` holds to it. On a fresh
/// machine the URL's same bytes install, with no `not_locked` warning; other bytes fail
/// the entry `hash_mismatch` (`details.reason: "lock_mismatch"`), nothing of it installed,
/// and its fix is the run without `--locked`. An entry without `sha256` (exported while
/// its archive was not cached) installs what the URL serves now, with `not_locked`, as
/// before.
#[test]
fn a_locked_url_entry_installs_only_its_recorded_bytes() {
    use common::tlog::tlog;
    let log = tlog!();
    let url = "https://cdn.wowinterface.com/downloads/foo.zip";
    // Each archive built once, so every machine that serves it serves exactly its bytes.
    let zip = |lua: &str| {
        zip_text(&[
            ("Foo/Foo.toc", &toc(ON_BOTH, &["## Version: 1.0"])),
            ("Foo/core.lua", lua),
        ])
    };
    let (one, two) = (zip("-- one"), zip("-- two"));
    let serving = |bytes: &[u8]| {
        let home = Home::new(&["wow"]);
        common::http::fixture_bytes(&home.http, "foo", url, 200, &[], Some(bytes));
        log.env(&home);
        home
    };

    log.step("lock on the old machine");
    let old = serving(&one);
    log.run(
        &old,
        &["install", &format!("url:{url}"), "-t", "retail", "--json"],
    )
    .ok();
    let file = exported(&old, "dotfiles");
    let text = fs::read_to_string(&file).unwrap();
    log.note(&text);
    assert!(
        text.contains("\nsha256 = ") && !text.contains("\nversion = "),
        "{text}"
    );
    let lock = lock_file::locked_sha(&file);

    log.step("a fresh machine, the URL serving the same bytes");
    let same = serving(&one);
    let run = log.run(&same, &["sync", &file, "--locked", "--json"]);
    run.ok();
    log.assert_eq("partial", &run.envelope()["partial"], &json!(false));
    log.assert_eq(
        "locked: no not_locked",
        &warned(run.envelope(), "not_locked"),
        &0,
    );
    let lua = fs::read_to_string(same.addons("_retail_").join("Foo/core.lua")).unwrap();
    log.assert_eq("the locked bytes", &lua, &"-- one".to_owned());

    log.step("a fresh machine, the URL serving other bytes");
    let moved = serving(&two);
    let run = log.run(&moved, &["sync", &file, "--locked", "--json"]);
    // The file's only entry failed, so the run fails with its error, the entry's own
    // under `details.items`.
    run.exit(1);
    let envelope = run.envelope().clone();
    let foo = &envelope["error"]["details"]["items"][0];
    log.json("Foo", foo);
    log.assert_eq("the entry", &foo["request"], &json!("Foo"));
    let error = &foo["error"];
    log.assert_eq("code", &error["code"], &json!("hash_mismatch"));
    log.assert_eq(
        "the run's error is the entry's",
        &envelope["error"]["details"]["reason"],
        &json!("lock_mismatch"),
    );
    log.assert_eq(
        "reason",
        &error["details"]["reason"],
        &json!("lock_mismatch"),
    );
    log.assert_eq("the lock", &error["details"]["expected"], &json!(lock));
    log.assert_eq(
        "its fix: the run without --locked",
        &error["fix_argv"],
        &json!(["omaddons", "sync", file]),
    );
    log.assert_eq(
        "locked: no not_locked",
        &warned(&envelope, "not_locked"),
        &0,
    );
    log.assert_eq(
        "nothing of Foo installed, nothing committed",
        &(!moved.addons("_retail_").join("Foo").exists()
            && !moved.state().join("history.jsonl").exists()),
        &true,
    );

    log.step("an entry without sha256 installs what the URL serves, as before");
    let unlocked: String = text
        .lines()
        .filter(|l| !l.starts_with("sha256 = "))
        .map(|l| format!("{l}\n"))
        .collect();
    let unlocked_file = moved.env.root().join("unlocked.toml");
    fs::write(&unlocked_file, &unlocked).unwrap();
    let unlocked_file = unlocked_file.display().to_string();
    let run = log.run(&moved, &["sync", &unlocked_file, "--locked", "--json"]);
    run.ok();
    log.assert_eq("not_locked", &warned(run.envelope(), "not_locked"), &1);
    let lua = fs::read_to_string(moved.addons("_retail_").join("Foo/core.lua")).unwrap();
    log.assert_eq("what the URL serves now", &lua, &"-- two".to_owned());
}

#[cfg(test)]
mod lock_file {
    use std::fs;

    /// The archive hash the lock file `file` records for Foo.
    pub fn locked_sha(file: &str) -> String {
        let text = fs::read_to_string(file).unwrap();
        let parsed: toml::Table = text.parse().unwrap();
        find_sha(&toml::Value::Table(parsed)).expect("the lock records Foo's sha256")
    }

    fn find_sha(value: &toml::Value) -> Option<String> {
        match value {
            toml::Value::Table(table) => table
                .get("sha256")
                .and_then(|v| v.as_str().map(str::to_owned))
                .or_else(|| table.values().find_map(find_sha)),
            toml::Value::Array(items) => items.iter().find_map(find_sha),
            _ => None,
        }
    }
}

#[test]
fn a_version_no_longer_served_fails_and_an_unversioned_entry_warns() {
    let home = Home::new(&["wow"]);
    foo_v1_and_v2(&home);
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/bar",
            tag: "v1",
            folders: &[support::folder("Bar", &[])],
        }],
    );
    for (name, url) in [
        (
            "v9-json",
            "https://github.com/o/foo/releases/download/v9/release.json",
        ),
        (
            "v9-api",
            "https://api.github.com/repos/o/foo/releases/tags/v9",
        ),
    ] {
        common::http::fixture(&home.http, name, url, 404, &[], None);
    }
    let file = home.write_desired(
        "[family.mainline]\naddons = [\"gh:o/bar\"]\n\n[[family.mainline.addon]]\nname = \"Foo\"\nref = \"gh:o/foo\"\nversion = \"v9\"\n",
    );
    let file = file.display().to_string();
    let r = home.run(&["sync", &file, "--locked"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    let foo = items(&envelope, "retail")
        .iter()
        .find(|i| i["request"] == "Foo")
        .cloned()
        .unwrap();
    assert_eq!(foo["error"]["code"], "not_found");
    assert_eq!(foo["error"]["fix_argv"], json!(["omaddons", "sync", file]));
    assert!(record_of(&home, "retail", "Bar").is_some());
    let not_locked: Vec<&Value> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "not_locked")
        .collect();
    assert_eq!(not_locked.len(), 1, "{}", r.context());
    assert!(
        not_locked[0]["message"]
            .as_str()
            .unwrap()
            .contains("gh:o/bar")
    );
    assert_eq!(not_locked[0]["fix_argv"], Value::Null);
}

#[test]
fn locked_moves_an_installed_unpinned_addon_to_its_version() {
    let home = foo_behind();
    home.write_desired(
        "[[family.mainline.addon]]\nname = \"Foo\"\nref = \"gh:o/foo\"\nversion = \"v2\"\n",
    );
    let r = home.run(&["sync"]);
    let summary = &r.expect(0)["data"]["summary"];
    assert_eq!(
        (&summary["changed"], &summary["unchanged"]),
        (&json!(0), &json!(1)),
        "{}",
        r.context()
    );
    let r = home.run(&["sync", "--locked"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("Foo", "updated")],
        "{}",
        r.context()
    );
    let foo = record_of(&home, "retail", "Foo").unwrap();
    assert_eq!((foo.version.as_str(), foo.pinned), ("v2", false));
}

#[test]
fn a_dependency_lock_fixes_the_version_it_installs() {
    let home = Home::new(&["wow"]);
    release_graph(
        &home.http,
        &[
            Node {
                base: "https://github.com/o/littlewigs",
                tag: "v1",
                folders: &[support::folder("LittleWigs", &["BigWigs"])],
            },
            Node {
                base: "https://github.com/o/bigwigs",
                tag: "v1",
                folders: &[support::folder("BigWigs", &[])],
            },
            Node {
                base: "https://github.com/o/bigwigs",
                tag: "v2",
                folders: &[support::folder("BigWigs", &[])],
            },
        ],
    );
    let catalog_d = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&catalog_d).unwrap();
    fs::write(
        catalog_d.join("50-wigs.toml"),
        "schema_version = \"1.0\"\n\n[[addon]]\nname = \"LittleWigs\"\nref = \"gh:o/littlewigs\"\nfolders = [\"LittleWigs\"]\n\n[[addon]]\nname = \"BigWigs\"\nref = \"gh:o/bigwigs\"\nfolders = [\"BigWigs\"]\n",
    )
    .unwrap();
    home.write_desired(
        "[family.mainline]\naddons = [\"littlewigs\"]\n\n[[family.mainline.addon]]\nname = \"BigWigs\"\nref = \"gh:o/bigwigs\"\ndependency = true\nversion = \"v1\"\n\n[[family.mainline.addon]]\nname = \"Nothing\"\nref = \"gh:o/nothing\"\ndependency = true\nversion = \"v1\"\n",
    );
    let r = home.run(&["sync", "--locked"]);
    r.expect(0);
    let bw = record_of(&home, "retail", "BigWigs");
    assert!(bw.is_some(), "{}", r.context());
    let bw = bw.unwrap();
    assert_eq!(
        (bw.version.as_str(), bw.reason, bw.pinned),
        ("v1", omaddons::model::AddonReason::Dependency, false)
    );
    assert!(
        record_of(&home, "retail", "Nothing").is_none(),
        "an unmatched lock is ignored"
    );
}

#[test]
fn prune_removes_only_unlisted_managed_addons() {
    let home = prune_home();
    for (r, t) in [
        ("gh:o/littlewigs", "retail"),
        ("gh:o/foo", "retail"),
        ("gh:o/bar", "retail"),
        ("gh:o/foo", "ptr"),
    ] {
        home.run(&["install", r, "-t", t]).expect(0);
    }
    assert!(
        record_of(&home, "retail", "BigWigs").is_some(),
        "the dependency"
    );
    let addons = home.addons("_retail_");
    common::addons::unpack(&addons, &[("Mine/Mine.toc", "## Interface: 120105\n")]);
    let mine = tree_hash(&addons.join("Mine"));
    home.write_desired("[family.mainline]\naddons = [\"gh:o/littlewigs\", \"gh:o/foo\"]\n");

    let r = home.run(&["sync", "--prune"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("Bar", "removed")],
        "{}",
        r.context()
    );
    for kept in ["LittleWigs", "BigWigs", "Foo"] {
        assert!(record_of(&home, "retail", kept).is_some(), "{kept}");
    }
    assert!(record_of(&home, "retail", "Bar").is_none());
    assert_eq!(
        tree_hash(&addons.join("Mine")),
        mine,
        "unmanaged: untouched"
    );
    assert_eq!(target(&envelope, "ptr")["no_section"], true);
    assert!(
        record_of(&home, "ptr", "Foo").is_some(),
        "no section: untouched"
    );

    // Nothing left to prune: no request, nothing committed.
    let (before, txs) = (net_log(&home).len(), common::state::history(&home).len());
    let r = home.run(&["sync", "--prune"]);
    assert_eq!(r.expect(0)["data"]["summary"]["changed"], 0);
    assert_eq!(net_log(&home).len(), before);
    assert_eq!(common::state::history(&home).len(), txs);
}

/// `--prune` keeps every addon a kept one requires, also when the kept one is new: a
/// listed addon installed by this very run has no record yet to read its requirements
/// from, so they come from its archive. BigWigs, installed by hand and not listed, stays
/// as it is (never removed, never installed again) once the listed LittleWigs requires
/// it; Bar, which nothing requires, goes.
#[test]
fn prune_keeps_what_an_addon_installed_by_the_same_run_requires() {
    let home = prune_home();
    for r in ["gh:o/bigwigs", "gh:o/bar"] {
        home.run(&["install", r, "-t", "retail"]).expect(0);
    }
    let before = record_of(&home, "retail", "BigWigs").unwrap();
    assert!(before.required_by.is_empty());
    home.write_desired("[family.mainline]\naddons = [\"gh:o/littlewigs\"]\n");

    let r = home.run(&["sync", "--prune"]);
    let envelope = r.expect(0);
    let statuses = statuses(&envelope, "retail");
    assert!(
        statuses.contains(&pair("gh:o/littlewigs", "installed"))
            && statuses.contains(&pair("Bar", "removed")),
        "{statuses:?}"
    );
    assert!(
        !statuses
            .iter()
            .any(|(request, status)| request == "BigWigs" && status == "removed"),
        "{statuses:?}"
    );
    let after = record_of(&home, "retail", "BigWigs").expect("BigWigs stays installed");
    assert_eq!(
        (&after.version, after.installed_at, after.reason),
        (&before.version, before.installed_at, before.reason),
        "as it was: not removed and installed again"
    );
    assert_eq!(after.required_by, ["LittleWigs"]);
    assert!(home.addons("_retail_").join("BigWigs").is_dir());
    assert!(record_of(&home, "retail", "Bar").is_none());

    // Idempotent: the record now says what LittleWigs requires.
    let r = home.run(&["sync", "--prune"]);
    assert_eq!(r.expect(0)["data"]["summary"]["changed"], 0);
}

/// `--prune` never removes what an entry names, also when the entry fails: a local
/// archive that is no longer where the file says fails its entry (`not_found`), and the
/// addon installed from it stays.
#[test]
fn prune_keeps_the_addon_of_an_entry_whose_local_archive_is_gone() {
    let home = prune_home();
    let zip = home.env.config().join("omaddons/Alpha-1.0.zip");
    fs::create_dir_all(zip.parent().unwrap()).unwrap();
    fs::write(
        &zip,
        zip_text(&[("Alpha/Alpha.toc", &toc("120105", &["## Version: 1.0"]))]),
    )
    .unwrap();
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\", \"./Alpha-1.0.zip\"]\n");
    home.run(&["sync", "-t", "retail"]).expect(0);
    assert!(record_of(&home, "retail", "Alpha").is_some());

    fs::remove_file(&zip).unwrap();
    for args in [
        &["sync", "--prune", "-t", "retail", "--dry-run"][..],
        &["sync", "--prune", "-t", "retail"][..],
    ] {
        let r = home.run(args);
        let envelope = r.expect(0);
        let statuses = statuses(&envelope, "retail");
        assert!(
            statuses.contains(&pair("./Alpha-1.0.zip", "failed"))
                && statuses.iter().all(|(_, status)| status != "removed"),
            "{}",
            r.context()
        );
        assert_eq!(
            (
                &envelope["partial"],
                &envelope["data"]["summary"]["changed"]
            ),
            (&json!(true), &json!(0)),
            "{}",
            r.context()
        );
        assert_eq!(
            item(&envelope, "retail", "./Alpha-1.0.zip")["error"]["code"],
            "not_found"
        );
    }
    assert!(record_of(&home, "retail", "Alpha").is_some());
    assert!(home.addons("_retail_").join("Alpha").is_dir());
}

#[test]
fn prune_refuses_a_file_with_a_section_for_no_known_client() {
    let home = prune_home();
    home.run(&["install", "gh:o/bar", "-t", "retail"]).expect(0);
    home.write_desired(
        "[family.mainline]\naddons = [\"gh:o/foo\"]\n\n[target.\"retial\"]\naddons = [\"gh:o/baz\"]\n",
    );
    let tree = tree_hash(&home.addons("_retail_"));
    for args in [&["sync", "--prune"][..], &["sync", "--prune", "-t", "ptr"]] {
        let r = home.run(args);
        let envelope = r.expect(2);
        let error = &envelope["error"];
        assert_eq!(
            (
                &error["code"],
                &error["details"]["reason"],
                &error["details"]["toml_key"]
            ),
            (
                &json!("desired_state_invalid"),
                &json!("unknown_target_section"),
                &json!("target.retial")
            ),
            "{}",
            r.context()
        );
        assert_eq!(error["fix_argv"], json!(["omaddons", "targets"]));
    }
    assert_eq!(tree_hash(&home.addons("_retail_")), tree, "nothing changed");
    assert!(record_of(&home, "retail", "Bar").is_some());
    // Without --prune the same file syncs, with a warning.
    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "desired_unknown_target"),
        "{}",
        r.context()
    );
}

#[test]
fn one_sync_installs_removes_and_unpins_and_undo_restores_it_all() {
    let home = prune_home();
    for r in ["gh:o/foo", "gh:o/bar"] {
        home.run(&["install", r, "-t", "retail"]).expect(0);
    }
    home.run(&["pin", "Foo"]).expect(0);
    let addons = home.addons("_retail_");
    let (tree, before) = (tree_hash(&addons), common::state::record(&home, "retail"));
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\", \"gh:o/baz\"]\n");

    // A dry run lists it and changes nothing.
    let r = home.run(&["sync", "--prune", "--dry-run"]);
    assert!(
        statuses(&r.expect(0), "retail").contains(&pair("Bar", "removed")),
        "{}",
        r.context()
    );
    assert_eq!(tree_hash(&addons), tree);

    let r = home.run(&["sync", "--prune"]);
    let envelope = r.expect(0);
    let t = target(&envelope, "retail");
    assert_eq!(t["kind"], "sync", "{}", r.context());
    let mut got = statuses(&envelope, "retail");
    got.sort();
    assert_eq!(
        got,
        [
            pair("Bar", "removed"),
            pair("gh:o/baz", "installed"),
            pair("gh:o/foo", "unpinned")
        ]
    );
    let line = common::state::history(&home).pop().unwrap();
    let ops: std::collections::BTreeSet<String> =
        line.ops.iter().map(|o| format!("{:?}", o.op)).collect();
    assert_eq!(
        ops.into_iter().collect::<Vec<_>>(),
        ["Add", "Record", "Remove"]
    );
    home.run(&["undo"]).expect(0);
    assert_eq!(tree_hash(&addons), tree, "the folders as they were");
    let after = common::state::record(&home, "retail");
    assert_eq!(
        after.addons.keys().collect::<Vec<_>>(),
        before.addons.keys().collect::<Vec<_>>()
    );
    assert!(after.addons["foo"].pinned, "the pin is back");
}

#[test]
fn pruning_a_record_whose_folder_is_gone_forgets_it_undoably() {
    let home = prune_home();
    for r in ["gh:o/foo", "gh:o/bar"] {
        home.run(&["install", r, "-t", "retail"]).expect(0);
    }
    common::drift::delete_managed(&home.addons("_retail_").join("Bar"));
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    let r = home.run(&["sync", "--prune"]);
    assert_eq!(
        statuses(&r.expect(0), "retail"),
        [pair("Bar", "removed")],
        "{}",
        r.context()
    );
    let line = common::state::history(&home).pop().unwrap();
    assert!(
        line.ops
            .iter()
            .all(|o| o.op == omaddons::state::history::FolderOp::Record),
        "{:?}",
        line.ops
    );
    assert!(record_of(&home, "retail", "Bar").is_none());
    home.run(&["undo"]).expect(0);
    assert!(
        record_of(&home, "retail", "Bar").is_some(),
        "the record is back"
    );
}

/// Under the safe policy, a prune that removes a folder the running client has loaded
/// fails that item and leaves the folder; `--while-running` removes it and asks for a
/// restart, and once the client exits undo brings it back.
#[test]
fn sync_prune_of_a_loaded_addon_is_refused_while_the_client_runs() {
    let mut home = prune_home();
    for r in ["gh:o/foo", "gh:o/bar"] {
        home.run(&["install", r, "-t", "retail"]).expect(0);
    }
    let addons = home.addons("_retail_");
    let (tree, txs) = (tree_hash(&addons), common::state::history(&home).len());
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    home.retail_running();

    let r = home.run(&["sync", "--prune"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    let bar = item(&envelope, "retail", "Bar");
    assert_eq!(
        (
            &bar["status"],
            &bar["error"]["code"],
            &bar["error"]["details"]
        ),
        (
            &json!("failed"),
            &json!("game_running"),
            &json!({ "reason": "loaded_toc" })
        ),
        "{}",
        r.context()
    );
    assert_eq!(
        bar["error"]["fix_argv"],
        json!(["omaddons", "sync", "--prune", "--while-running"])
    );
    assert_eq!(
        target(&envelope, "retail")["tx"],
        Value::Null,
        "nothing else changed"
    );
    assert_eq!(
        warned(&envelope, "game_running"),
        1,
        "the failure, mirrored"
    );
    assert_eq!(tree_hash(&addons), tree);
    assert_eq!(common::state::history(&home).len(), txs);

    let r = home.run(&["sync", "--prune", "--while-running"]);
    let envelope = r.expect(0);
    assert_eq!(
        statuses(&envelope, "retail"),
        [pair("Bar", "removed")],
        "{}",
        r.context()
    );
    assert_eq!(target(&envelope, "retail")["reload_required"], "restart");
    assert_eq!(warned(&envelope, "reload_required"), 1, "{}", r.context());
    assert!(!addons.join("Bar").exists());

    home.nothing_running();
    home.run(&["undo"]).expect(0);
    assert_eq!(tree_hash(&addons), tree);
}

/// A new addon installs while the client runs, and the client must restart: no client
/// was ever observed loading a folder added while it ran on `/reload` (bmcx.5).
#[test]
fn sync_install_of_a_new_addon_while_running_needs_restart() {
    let mut home = prune_home();
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\", \"gh:o/baz\"]\n");
    home.retail_running();

    let r = home.run(&["sync"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], false, "{}", r.context());
    assert_eq!(item(&envelope, "retail", "gh:o/baz")["status"], "installed");
    let t = target(&envelope, "retail");
    assert!(t["tx"].is_string(), "{t}");
    assert_eq!(t["reload_required"], "restart");
    assert_eq!(warned(&envelope, "reload_required"), 1, "{}", r.context());
    assert!(home.addons("_retail_").join("Baz").is_dir());
}

/// A process table omaddons cannot read counts every client as running: the add goes
/// through with a reload, removing the loaded folder fails with `running: "unknown"`,
/// and `running_unknown` is warned once.
#[test]
fn sync_fails_closed_when_the_process_table_cannot_be_read() {
    use std::os::unix::fs::PermissionsExt;

    if common::procfs::running_as_root() {
        return; // chmod 000 hides nothing from root
    }
    let home = prune_home();
    for r in ["gh:o/foo", "gh:o/bar"] {
        home.run(&["install", r, "-t", "retail"]).expect(0);
    }
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\", \"gh:o/baz\"]\n");
    let locked = home.env.root().join("proc-locked");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let argv = ["sync", "--prune", "--json"].map(str::to_owned);
    let r = home.exec_with(&argv, &[("OMADDONS_PROC_ROOT", locked.to_str().unwrap())]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    assert_eq!(item(&envelope, "retail", "gh:o/baz")["status"], "installed");
    let bar = &item(&envelope, "retail", "Bar")["error"];
    assert_eq!(
        (&bar["code"], &bar["details"]),
        (
            &json!("game_running"),
            &json!({ "reason": "loaded_toc", "running": "unknown" })
        ),
        "{}",
        r.context()
    );
    let t = target(&envelope, "retail");
    assert!(t["tx"].is_string(), "the add commits: {t}");
    // A new folder on a running client asks for a restart (bmcx.5: never seen on /reload).
    assert_eq!(t["reload_required"], "restart");
    assert_eq!(warned(&envelope, "running_unknown"), 1, "{}", r.context());
    assert!(home.addons("_retail_").join("Bar").is_dir());
}

/// On a legacy client every change waits for it to exit, an add too (`legacy_client`):
/// with nothing else in the run, that is the run's error, each item in
/// `details.items`. `--while-running` installs it and asks for a restart.
#[test]
fn a_legacy_client_refuses_every_change_while_it_runs() {
    let mut home = prune_home();
    let legacy = home.with_custom(common::trees::CustomRoot::at(
        home.env.root().join("wow-335"),
    ));
    let targets = home.run(&["targets"]).expect(0);
    let wotlk = targets["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["family"] != "mainline")
        .and_then(|t| t["id"].as_str())
        .unwrap()
        .to_owned();
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/leg",
            tag: "v1",
            folders: &[Folder {
                name: "Leg",
                interface: "30300",
                deps: &[],
                optional: &[],
            }],
        }],
    );
    home.write_desired(&format!("[target.{wotlk:?}]\naddons = [\"gh:o/leg\"]\n"));
    let exe = legacy.join("Wow.exe").display().to_string();
    let proc = common::procfs::FakeProc::new(home.env.root().join("proc")).process(
        9,
        true,
        "Wow.exe",
        &["/usr/bin/wine", &exe],
        &[],
    );
    home.proc = Some(proc.root().to_path_buf());
    let interface = legacy.join("Interface");
    let (tree, txs) = (tree_hash(&interface), common::state::history(&home).len());

    let r = home.run(&["sync"]);
    let error = &r.expect(4)["error"];
    assert_eq!(
        (&error["code"], &error["details"]["reason"]),
        (&json!("game_running"), &json!("legacy_client")),
        "{}",
        r.context()
    );
    assert_eq!(
        (
            &error["details"]["items"][0]["target"],
            &error["details"]["items"][0]["request"]
        ),
        (&json!(wotlk), &json!("gh:o/leg"))
    );
    assert_eq!(
        error["fix_argv"],
        json!(["omaddons", "sync", "--while-running"])
    );
    assert_eq!(tree_hash(&interface), tree);
    assert_eq!(common::state::history(&home).len(), txs);

    let r = home.run(&["sync", "--while-running"]);
    let envelope = r.expect(0);
    assert_eq!(
        item(&envelope, &wotlk, "gh:o/leg")["status"],
        "installed",
        "{}",
        r.context()
    );
    assert_eq!(target(&envelope, &wotlk)["reload_required"], "restart");
    assert!(interface.join("AddOns/Leg").is_dir());
}
