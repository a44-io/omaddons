//! `omaddons sync` end to end: every client made to match `addons.toml` in one
//! `sync`-kind transaction per target under one run, a second run that changes nothing
//! and asks nothing, no updates, pins and channels that follow the file, family
//! sections that never reach a test client, failures per entry, dependencies, and undo.

mod common;

use std::fs;

use common::addons::toc;
use common::archives::zip_text;
use common::graph::{Folder, Node, release_graph};
use common::home::Home;
use common::netlog::net_log;
use common::trees::tree_hash;
use omaddons::model::{Channel, TxKind};
use serde_json::{Value, json};
use support::{ON_BOTH, exported, items, prune_home, record_of, statuses, target};

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
fn a_dry_run_writes_only_the_cache_the_net_log_and_inflight() {
    let home = two_clients();
    let path = home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    let state = home.state();
    let listing = || {
        let mut files: Vec<String> = Vec::new();
        for root in [&state, &home.env.config(), &home.root] {
            for entry in walk(root) {
                let shown = entry.display().to_string();
                // The in-flight lock, and the trash its cleanup leaves (omad-gfoq).
                let bookkeeping = ["/inflight", "/trash"]
                    .iter()
                    .any(|d| shown.contains(&format!("{}{d}", state.display())));
                if !bookkeeping && !shown.ends_with("net.log") {
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
                &error["details"]["key"]
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
