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
use support::{ON_BOTH, items, record_of, statuses, target};

#[cfg(test)]
mod support {
    use serde_json::Value;

    use super::common::graph::Folder;
    use super::common::home::Home;

    /// Loads on retail (12.1.5) and the PTR (12.1.0).
    pub const ON_BOTH: &str = "120105, 120100";

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
