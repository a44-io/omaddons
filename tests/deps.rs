//! Dependency rounds on install, end to end (M2-D7..D9, D33): what a TOC requires is
//! installed in the same transaction from a keyless source (here, GitHub releases named
//! by a `catalog.d` override), recorded as a dependency and undone with its requester;
//! present folders, optional dependencies, the client's own folders and the archive's
//! own folders install nothing; what cannot install warns or makes the run partial.

mod common;

use std::fs;

use common::addons::{addon_folder, toc};
use common::archives::zip_text;
use common::graph::{Node, release_graph};
use common::home::Home;
use common::netlog::net_log;
use common::trees::tree_hash;
use omaddons::model::{AddonReason, Channel};
use omaddons::state::history::HistoryEdit;
use serde_json::{Value, json};
use support::{
    RETAIL, catalog, fetches, folder, graph, items, last_line, last_tx, record_of, warnings_with,
    wigs,
};

#[cfg(test)]
mod support {
    use std::fs;

    use serde_json::Value;

    use super::common::graph::{Folder, Node, release_graph};
    use super::common::home::Home;
    use super::common::netlog::net_log;

    /// The retail client's interface: current.
    pub const RETAIL: &str = "120105";

    /// Retail; LittleWigs requires BigWigs, both GitHub releases the catalog names.
    pub fn wigs() -> Home {
        let home = Home::new(&["wow"]);
        release_graph(
            &home.http,
            &[
                Node {
                    base: "https://github.com/o/littlewigs",
                    tag: "v1",
                    folders: &[folder("LittleWigs", &["BigWigs"])],
                },
                Node {
                    base: "https://github.com/o/bigwigs",
                    tag: "v1",
                    folders: &[folder("BigWigs", &[])],
                },
            ],
        );
        catalog(
            &home,
            &[
                ("LittleWigs", "littlewigs", &["LittleWigs"]),
                ("BigWigs", "bigwigs", &["BigWigs"]),
            ],
        );
        home
    }

    /// Retail with one release per node, each named by the catalog after its first
    /// folder (folder `DepX` lives in `o/depx`).
    pub fn graph(nodes: &[Node<'_>]) -> Home {
        let home = Home::new(&["wow"]);
        release_graph(&home.http, nodes);
        let entries: Vec<(String, String, Vec<&str>)> = nodes
            .iter()
            .map(|n| {
                let repo = n.base.rsplit('/').next().unwrap().to_owned();
                let primary = n.folders[0].name;
                (
                    primary.to_owned(),
                    repo,
                    n.folders.iter().map(|f| f.name).collect(),
                )
            })
            .collect();
        let entries: Vec<(&str, &str, &[&str])> = entries
            .iter()
            .map(|(name, repo, folders)| (name.as_str(), repo.as_str(), folders.as_slice()))
            .collect();
        catalog(&home, &entries);
        home
    }

    /// A retail folder requiring `deps`.
    pub const fn folder<'a>(name: &'a str, deps: &'a [&'a str]) -> Folder<'a> {
        Folder {
            name,
            interface: RETAIL,
            deps,
            optional: &[],
        }
    }

    /// Writes `<config>/omaddons/catalog.d/50-deps.toml`: one entry per (name, repo under
    /// `o/`, folders). An entry named like a built-in one replaces it.
    pub fn catalog(home: &Home, entries: &[(&str, &str, &[&str])]) {
        let mut text = String::from("schema_version = \"1.0\"\n");
        for (name, repo, folders) in entries {
            let folders: Vec<String> = folders.iter().map(|f| format!("{f:?}")).collect();
            text.push_str(&format!(
                "\n[[addon]]\nname = {name:?}\nref = \"gh:o/{repo}\"\nfolders = [{}]\n",
                folders.join(", ")
            ));
        }
        let dir = home.env.config().join("omaddons/catalog.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("50-deps.toml"), text).unwrap();
    }

    /// `releases/latest` requests for the repository `o/<repo>`.
    pub fn fetches(home: &Home, repo: &str) -> usize {
        let path = format!("/o/{repo}/releases/latest");
        net_log(home)
            .entries
            .iter()
            .filter(|e| e.path == path)
            .count()
    }

    /// Requests of any kind under `o/<repo>`.
    pub fn requests(home: &Home, repo: &str) -> usize {
        let prefix = format!("/o/{repo}/");
        net_log(home)
            .entries
            .iter()
            .filter(|e| e.path.starts_with(&prefix))
            .count()
    }

    /// `install`'s items.
    pub fn items(envelope: &Value) -> &Vec<Value> {
        envelope["data"]["items"].as_array().unwrap()
    }

    /// `(request, status)` of the first target's items (`update`'s shape).
    pub fn update_items(envelope: &Value) -> Vec<(String, String)> {
        envelope["data"]["targets"][0]["items"]
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

    /// The warnings with `code`, as messages.
    pub fn warnings_with(envelope: &Value, code: &str) -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] == code)
            .map(|w| w["message"].as_str().unwrap().to_owned())
            .collect()
    }

    /// `install`'s envelope with its volatile values replaced, for snapshots.
    pub fn stable(envelope: &Value) -> Value {
        let mut text = envelope.to_string();
        for (value, placeholder) in [
            (&envelope["data"]["tx"], "<tx>"),
            (&envelope["data"]["target"]["key"], "<key>"),
        ] {
            if let Some(v) = value.as_str() {
                text = text.replace(v, placeholder);
            }
        }
        let mut stable: Value = serde_json::from_str(&text).unwrap();
        stable["meta"] = Value::from("<meta>");
        stable["tool_version"] = Value::from("<version>");
        stable
    }

    /// `omaddons <args>` in human form, with the last transaction's id replaced: its
    /// stdout, for snapshots.
    pub fn human(home: &Home, args: &[&str]) -> String {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let r = home.exec(&argv);
        assert_eq!(r.code, 0, "{}", r.context());
        let mut out = r.stdout.clone();
        if let Some(line) = super::common::state::history(home).pop() {
            out = out.replace(&line.tx.to_string(), "<tx>");
        }
        out
    }

    /// The last history line's kind, and the `edit` of its op on `addon`.
    pub fn last_line(
        home: &Home,
        addon: &str,
    ) -> (Value, Option<omaddons::state::history::HistoryEdit>) {
        let line = super::common::state::history(home).pop().unwrap();
        let edit = line
            .ops
            .iter()
            .find(|op| op.addon == addon)
            .and_then(|op| op.edit.clone());
        (serde_json::to_value(line.kind).unwrap(), edit)
    }

    /// The newest transaction's id.
    pub fn last_tx(home: &Home) -> String {
        super::common::state::history(home)
            .pop()
            .unwrap()
            .tx
            .to_string()
    }

    /// The retail record of the addon named `name`.
    pub fn record_of(home: &Home, name: &str) -> Option<omaddons::model::AddonRecord> {
        let record = super::common::state::record(home, "retail");
        record
            .addons
            .values()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .cloned()
    }
}

const fn node<'a>(base: &'a str, folders: &'a [common::graph::Folder<'a>]) -> Node<'a> {
    Node {
        base,
        tag: "v1",
        folders,
    }
}

/// LittleWigs and Other each require BigWigs.
fn wigs_and_other() -> Home {
    graph(&[
        node(
            "https://github.com/o/littlewigs",
            &[folder("LittleWigs", &["BigWigs"])],
        ),
        node(
            "https://github.com/o/other",
            &[folder("Other", &["BigWigs"])],
        ),
        node("https://github.com/o/bigwigs", &[folder("BigWigs", &[])]),
    ])
}

fn names(required_by: &[String]) -> Vec<&str> {
    required_by.iter().map(String::as_str).collect()
}

#[test]
fn installing_littlewigs_installs_bigwigs_with_it_and_undo_removes_both() {
    let home = wigs();
    let addons = home.addons("_retail_");
    let before = tree_hash(&addons);

    let dry = home.run(&["install", "littlewigs", "--dry-run"]);
    let planned = dry.expect(0);
    assert_eq!(
        items(&planned)[0]["status"],
        "installed",
        "{}",
        dry.context()
    );
    assert_eq!(tree_hash(&addons), before, "a dry run writes nothing");
    assert_eq!(
        fetches(&home, "bigwigs"),
        1,
        "a dry run plans the dependency too"
    );

    let r = home.run(&["install", "littlewigs"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], false, "{}", r.context());
    // The dependency is an item of its own, after its requester, named by the folder.
    let listed: Vec<(&Value, &Value, &Value, &Value)> = items(&envelope)
        .iter()
        .map(|i| (&i["request"], &i["name"], &i["reason"], &i["required_by"]))
        .collect();
    assert_eq!(
        listed,
        [
            (
                &json!("littlewigs"),
                &json!("LittleWigs"),
                &json!("explicit"),
                &json!([])
            ),
            (
                &json!("BigWigs"),
                &json!("BigWigs"),
                &json!("dependency"),
                &json!(["LittleWigs"])
            )
        ]
    );
    assert!(addons.join("LittleWigs/LittleWigs.toc").is_file());
    assert!(addons.join("BigWigs/BigWigs.toc").is_file());

    let lw = record_of(&home, "LittleWigs").unwrap();
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(
        (lw.reason, names(&lw.required_by)),
        (AddonReason::Explicit, vec![])
    );
    assert_eq!(
        (bw.reason, names(&bw.required_by), bw.pinned, bw.channel),
        (
            AddonReason::Dependency,
            vec!["LittleWigs"],
            false,
            Channel::Stable
        )
    );
    // One transaction holds both.
    let history = common::state::history(&home);
    assert_eq!(history.len(), 1);
    assert_eq!(lw.tx_id, bw.tx_id);
    // Fetched once per command, downloaded once.
    let log = net_log(&home);
    assert_eq!(fetches(&home, "bigwigs"), 2, "{}", log.context());
    let downloads = log
        .entries
        .iter()
        .filter(|e| e.path.starts_with("/o/bigwigs/") && e.purpose == "asset")
        .count();
    assert_eq!(downloads, 1, "{}", log.context());

    let undo = home.run(&["undo"]);
    undo.expect(0);
    assert_eq!(tree_hash(&addons), before, "{}", undo.context());
    assert!(common::state::record(&home, "retail").addons.is_empty());
}

#[test]
fn a_present_unmanaged_dependency_installs_nothing_and_hints_adopting_it() {
    let home = wigs();
    let addons = home.addons("_retail_");
    addon_folder(
        &addons,
        "BigWigs",
        &[("BigWigs.toc", &toc(RETAIL, &[]))],
        &[],
    );
    let r = home.run(&["install", "littlewigs"]);
    let envelope = r.expect(0);
    assert!(record_of(&home, "BigWigs").is_none(), "{}", r.context());
    assert!(record_of(&home, "LittleWigs").is_some());
    assert_eq!(support::requests(&home, "bigwigs"), 0);
    let hint = envelope["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["action"] == "adopt")
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        hint["argv"],
        json!(["omaddons", "adopt", "BigWigs", "-t", "retail"]),
        "{}",
        r.context()
    );
    assert!(warnings_with(&envelope, "missing_dependency").is_empty());

    // The hint runs: BigWigs is managed, as it is.
    let tree = tree_hash(&addons.join("BigWigs"));
    let r = home.run(&["adopt", "BigWigs", "-t", "retail"]);
    r.expect(0);
    assert!(record_of(&home, "BigWigs").is_some(), "{}", r.context());
    assert_eq!(tree_hash(&addons.join("BigWigs")), tree);
}

#[test]
fn a_managed_dependency_folder_gone_from_disk_is_fixed_by_reinstalling_its_owner() {
    let home = wigs();
    home.run(&["install", "bigwigs"]).expect(0);
    let bigwigs = home.addons("_retail_").join("BigWigs");
    fs::remove_dir_all(&bigwigs).unwrap();
    let r = home.run(&["install", "littlewigs"]);
    let envelope = r.expect(0);
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "missing_dependency")
        .cloned();
    assert!(warning.is_some(), "{}", r.context());
    let fix: Vec<String> = serde_json::from_value(warning.unwrap()["fix_argv"].clone()).unwrap();
    assert_eq!(
        fix,
        [
            "omaddons",
            "install",
            "BigWigs",
            "--reinstall",
            "-t",
            "retail"
        ]
    );
    let args: Vec<&str> = fix[1..].iter().map(String::as_str).collect();
    home.run(&args).expect(0);
    assert!(bigwigs.join("BigWigs.toc").is_file(), "its folder is back");
}

#[test]
fn a_managed_explicit_dependency_gains_the_requester_in_the_same_transaction() {
    let home = wigs();
    home.run(&["install", "bigwigs"]).expect(0);
    let r = home.run(&["install", "littlewigs"]);
    r.expect(0);
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(
        (bw.reason, names(&bw.required_by)),
        (AddonReason::Explicit, vec!["LittleWigs"]),
        "{}",
        r.context()
    );
    let history = common::state::history(&home);
    assert_eq!(history.len(), 2);
    let last = history.last().unwrap();
    let edit = last
        .ops
        .iter()
        .find(|op| op.addon == "BigWigs")
        .and_then(|op| op.edit.clone());
    assert_eq!(edit, Some(HistoryEdit::RequiredByAdd("LittleWigs".into())));
    assert_eq!(fetches(&home, "bigwigs"), 1, "not fetched again");

    // Undo takes the requester back out, in place.
    home.run(&["undo"]).expect(0);
    let bw = record_of(&home, "BigWigs").unwrap();
    assert!(bw.required_by.is_empty());
    assert!(record_of(&home, "LittleWigs").is_none());
}

#[test]
fn updating_a_requester_its_dependency_already_lists_touches_only_the_requester() {
    let home = wigs();
    home.run(&["install", "littlewigs"]).expect(0);
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/littlewigs",
            tag: "v2",
            folders: &[folder("LittleWigs", &["BigWigs"])],
        }],
    );
    let r = home.run(&["update", "--refresh"]);
    r.expect(0);
    let lw = record_of(&home, "LittleWigs").unwrap();
    assert_eq!(lw.version, "v2", "{}", r.context());
    let history = common::state::history(&home);
    assert_eq!(history.len(), 2);
    let touched: Vec<&str> = history[1].ops.iter().map(|op| op.addon.as_str()).collect();
    assert!(
        touched.iter().all(|a| *a == "LittleWigs"),
        "{touched:?}\n{}",
        r.context()
    );
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(names(&bw.required_by), ["LittleWigs"]);
}

#[test]
fn optional_dependencies_are_never_installed_or_fetched() {
    let home = Home::new(&["wow"]);
    let mut lw = folder("LittleWigs", &[]);
    lw.optional = &["BigWigs"];
    release_graph(
        &home.http,
        &[
            node("https://github.com/o/littlewigs", &[lw]),
            node("https://github.com/o/bigwigs", &[folder("BigWigs", &[])]),
        ],
    );
    catalog(
        &home,
        &[
            ("LittleWigs", "littlewigs", &["LittleWigs"]),
            ("BigWigs", "bigwigs", &["BigWigs"]),
        ],
    );
    let r = home.run(&["install", "littlewigs"]);
    let envelope = r.expect(0);
    assert!(record_of(&home, "BigWigs").is_none(), "{}", r.context());
    assert_eq!(support::requests(&home, "bigwigs"), 0);
    assert!(warnings_with(&envelope, "missing_dependency").is_empty());
}

#[test]
fn a_chain_installs_every_link_and_a_cycle_installs_each_once() {
    let chain = graph(&[
        node("https://github.com/o/depa", &[folder("DepA", &["DepB"])]),
        node("https://github.com/o/depb", &[folder("DepB", &["DepC"])]),
        node("https://github.com/o/depc", &[folder("DepC", &[])]),
    ]);
    let r = chain.run(&["install", "depa"]);
    r.expect(0);
    let b = record_of(&chain, "DepB").unwrap();
    let c = record_of(&chain, "DepC").unwrap();
    assert_eq!(names(&b.required_by), ["DepA"], "{}", r.context());
    assert_eq!(names(&c.required_by), ["DepB"]);
    assert_eq!(
        (b.reason, c.reason),
        (AddonReason::Dependency, AddonReason::Dependency)
    );
    assert_eq!(common::state::history(&chain).len(), 1);

    let cycle = graph(&[
        node("https://github.com/o/depa", &[folder("DepA", &["DepB"])]),
        node("https://github.com/o/depb", &[folder("DepB", &["DepA"])]),
    ]);
    let r = cycle.run(&["install", "depa"]);
    r.expect(0);
    let a = record_of(&cycle, "DepA").unwrap();
    let b = record_of(&cycle, "DepB").unwrap();
    assert_eq!(
        (a.reason, names(&a.required_by)),
        (AddonReason::Explicit, vec!["DepB"]),
        "{}",
        r.context()
    );
    assert_eq!(
        (b.reason, names(&b.required_by)),
        (AddonReason::Dependency, vec!["DepA"])
    );
    assert_eq!((fetches(&cycle, "depa"), fetches(&cycle, "depb")), (1, 1));
}

#[test]
fn a_dependency_without_a_compatible_release_leaves_the_requester_installed_and_the_run_partial() {
    let home = graph(&[
        node("https://github.com/o/depa", &[folder("DepA", &["DepB"])]),
        node(
            "https://github.com/o/depb",
            &[common::graph::Folder {
                name: "DepB",
                interface: "11509",
                deps: &[],
                optional: &[],
            }],
        ),
    ]);
    let r = home.run(&["install", "depa"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    assert!(record_of(&home, "DepA").is_some());
    assert!(record_of(&home, "DepB").is_none());
    let named: Vec<&Value> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| {
            let m = w["message"].as_str().unwrap();
            m.contains("gh:o/depb") && m.contains("required by DepA")
        })
        .collect();
    assert_eq!(named.len(), 1, "{}", r.context());
    // A failed item, named by the folder DepA required, right after DepA.
    let failed = &items(&envelope)[1];
    assert_eq!(
        (
            &failed["request"],
            &failed["ref"],
            &failed["status"],
            &failed["reason"],
            &failed["required_by"],
            &failed["error"]["details"]["required_by"]
        ),
        (
            &json!("DepB"),
            &json!("gh:o/depb"),
            &json!("failed"),
            &json!("dependency"),
            &json!(["DepA"]),
            &json!(["DepA"])
        ),
        "{}",
        r.context()
    );

    // --strict makes it a check failure.
    home.run(&["undo"]).expect(0);
    home.run(&["install", "depa", "--strict"]).expect(6);
}

#[test]
fn conditional_dependencies_count_only_where_they_apply() {
    let home = Home::new(&["wow"]);
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let foo = toc(
        "120105",
        &[
            "## Dependencies: ClassicOnly [AllowLoadGameType vanilla]",
            "## Dependencies [AllowLoadTextLocale enUS]: FutureLib",
        ],
    );
    let zip = dl.join("Foo.zip");
    fs::write(&zip, zip_text(&[("Foo/Foo.toc", &foo)])).unwrap();
    let path = zip.display().to_string();
    let r = home.run(&["install", "gh:o/foo", "--file", &path, "-t", "retail"]);
    let envelope = r.expect(0);
    let missing = warnings_with(&envelope, "missing_dependency");
    assert_eq!(missing.len(), 1, "{}", r.context());
    assert!(
        missing[0].contains("FutureLib")
            && missing[0].contains("under a condition omaddons cannot decide offline"),
        "{}",
        missing[0]
    );
    assert!(
        !missing[0].contains("ClassicOnly"),
        "a vanilla-only dependency does not apply on retail"
    );
    assert!(!home.addons("_retail_").join("FutureLib").exists());
}

/// Repeated dependency lines add up, as the client reads them; one that applies only on
/// Classic is not required on retail.
#[test]
fn repeated_dependency_lines_add_up() {
    let home = Home::new(&["wow"]);
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let foo = toc(
        "120105",
        &[
            "## Dependencies: Alpha",
            "## Dependencies: Beta",
            "## Dependencies: Ghost [AllowLoadGameType classic]",
        ],
    );
    let zip = dl.join("Foo.zip");
    fs::write(&zip, zip_text(&[("Foo/Foo.toc", &foo)])).unwrap();
    let path = zip.display().to_string();
    let r = home.run(&[
        "install",
        "gh:o/foo",
        "--file",
        &path,
        "-t",
        "retail",
        "--offline",
        "--dry-run",
    ]);
    let envelope = r.expect(0);
    let missing = warnings_with(&envelope, "missing_dependency").join("\n");
    for (name, required) in [("Alpha", true), ("Beta", true), ("Ghost", false)] {
        assert_eq!(
            missing.contains(&format!("requires {name}")),
            required,
            "{name}\n{}",
            r.context()
        );
    }
}

#[test]
fn no_deps_installs_only_the_requester_and_warns() {
    let home = wigs();
    let r = home.run(&["install", "littlewigs", "--no-deps"]);
    let envelope = r.expect(0);
    assert!(record_of(&home, "LittleWigs").is_some(), "{}", r.context());
    assert!(record_of(&home, "BigWigs").is_none());
    assert_eq!(support::requests(&home, "bigwigs"), 0);
    let missing = warnings_with(&envelope, "missing_dependency");
    assert_eq!(missing.len(), 1, "{}", r.context());
    assert!(missing[0].contains("BigWigs") && missing[0].contains("--no-deps"));
    // Its fix is the same install with its dependencies.
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "missing_dependency")
        .unwrap();
    assert_eq!(
        warning["fix_argv"],
        json!(["omaddons", "install", "littlewigs"]),
        "{}",
        r.context()
    );
}

#[test]
fn a_diamond_fetches_each_ref_once() {
    let home = graph(&[
        node(
            "https://github.com/o/depa",
            &[folder("DepA", &["DepB", "DepC"])],
        ),
        node("https://github.com/o/depb", &[folder("DepB", &["DepD"])]),
        node("https://github.com/o/depc", &[folder("DepC", &["DepD"])]),
        node("https://github.com/o/depd", &[folder("DepD", &[])]),
    ]);
    let r = home.run(&["install", "depa"]);
    r.expect(0);
    let d = record_of(&home, "DepD").unwrap();
    assert_eq!(names(&d.required_by), ["DepB", "DepC"], "{}", r.context());
    for repo in ["depa", "depb", "depc", "depd"] {
        assert_eq!(
            fetches(&home, repo),
            1,
            "{repo}\n{}",
            net_log(&home).context()
        );
    }
}

#[test]
fn a_chain_past_the_depth_limit_stops_with_a_warning_naming_it() {
    let home = graph(&[
        node("https://github.com/o/depa", &[folder("DepA", &["DepB"])]),
        node("https://github.com/o/depb", &[folder("DepB", &["DepC"])]),
        node("https://github.com/o/depc", &[folder("DepC", &["DepD"])]),
        node("https://github.com/o/depd", &[folder("DepD", &["DepE"])]),
        node("https://github.com/o/depe", &[folder("DepE", &["DepF"])]),
        node("https://github.com/o/depf", &[folder("DepF", &[])]),
    ]);
    let r = home.run(&["install", "depa"]);
    let envelope = r.expect(0);
    for name in ["DepA", "DepB", "DepC", "DepD", "DepE"] {
        assert!(record_of(&home, name).is_some(), "{name}\n{}", r.context());
    }
    assert!(record_of(&home, "DepF").is_none());
    assert_eq!(fetches(&home, "depf"), 0);
    let missing = warnings_with(&envelope, "missing_dependency");
    assert_eq!(missing.len(), 1, "{}", r.context());
    assert!(missing[0].contains("DepF") && missing[0].contains("DEPENDENCY_MAX_DEPTH"));
    assert_eq!(envelope["partial"], false);
}

#[test]
fn the_clients_folders_and_the_archives_own_install_nothing() {
    let home = graph(&[node(
        "https://github.com/o/depa",
        &[
            folder("DepA", &["Blizzard_Foo", "DepA_Lib"]),
            folder("DepA_Lib", &[]),
        ],
    )]);
    let r = home.run(&["install", "depa"]);
    let envelope = r.expect(0);
    let a = record_of(&home, "DepA").unwrap();
    assert_eq!(a.folders.len(), 2, "{}", r.context());
    assert_eq!(common::state::record(&home, "retail").addons.len(), 1);
    assert!(warnings_with(&envelope, "missing_dependency").is_empty());
}

#[test]
fn an_out_of_date_dependency_is_not_installed_and_warns() {
    let home = graph(&[
        node("https://github.com/o/depa", &[folder("DepA", &["DepB"])]),
        node(
            "https://github.com/o/depb",
            &[common::graph::Folder {
                name: "DepB",
                interface: "120000",
                deps: &[],
                optional: &[],
            }],
        ),
    ]);
    let r = home.run(&["install", "depa"]);
    let envelope = r.expect(0);
    assert!(record_of(&home, "DepA").is_some(), "{}", r.context());
    assert!(record_of(&home, "DepB").is_none());
    assert!(!home.addons("_retail_").join("DepB").exists());
    let missing = warnings_with(&envelope, "missing_dependency");
    assert_eq!(missing.len(), 1, "{}", r.context());
    assert!(missing[0].contains("out of date") && missing[0].contains("gh:o/depb"));
    assert_eq!(envelope["partial"], false);
}

#[test]
fn an_unresolvable_dependency_warns_with_a_search_for_it() {
    let home = graph(&[node(
        "https://github.com/o/depa",
        &[folder("DepA", &["NowhereToBeFound"])],
    )]);
    let r = home.run(&["install", "depa"]);
    let envelope = r.expect(0);
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "missing_dependency");
    assert!(warning.is_some(), "no missing_dependency\n{}", r.context());
    let warning = warning.unwrap().clone();
    let message = warning["message"].as_str().unwrap();
    assert!(
        message.contains("NowhereToBeFound") && message.contains("CurseForge or Wago"),
        "{message}"
    );
    assert_eq!(
        warning["fix_argv"],
        json!(["omaddons", "search", "NowhereToBeFound"])
    );
    // The fix runs (offline here: no GitHub fixture is needed).
    let r = home.run(&["search", "NowhereToBeFound", "--offline"]);
    assert_eq!(r.expect(0)["data"]["query"], "NowhereToBeFound");
}

#[test]
fn a_beta_install_takes_its_dependencies_from_the_default_channel() {
    let home = wigs();
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("littlewigs.zip");
    let lw = toc(
        RETAIL,
        &["## Title: LittleWigs", "## Dependencies: BigWigs"],
    );
    fs::write(
        &zip,
        zip_text(&[("LittleWigs/LittleWigs.toc", lw.as_str())]),
    )
    .unwrap();
    let zip = zip.display().to_string();
    let r = home.run(&[
        "install",
        "gh:o/littlewigs",
        "--file",
        &zip,
        "--channel",
        "beta",
        "-t",
        "retail",
    ]);
    r.expect(0);
    let lw = record_of(&home, "LittleWigs").unwrap();
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(
        (lw.channel, bw.channel),
        (Channel::Beta, Channel::Stable),
        "{}",
        r.context()
    );
}

#[test]
fn a_transaction_installs_at_most_the_addon_limit_of_dependencies() {
    let deps: Vec<String> = (0..33).map(|i| format!("Dep{i:02}")).collect();
    let required: Vec<&str> = deps.iter().map(String::as_str).collect();
    let bases: Vec<String> = deps
        .iter()
        .map(|d| format!("https://github.com/o/{}", d.to_lowercase()))
        .collect();
    let folders: Vec<[common::graph::Folder<'_>; 1]> =
        deps.iter().map(|d| [folder(d, &[])]).collect();
    let mut nodes = vec![node("https://github.com/o/depa", &[])];
    let requester = [folder("DepA", &required)];
    nodes[0].folders = &requester;
    for (base, f) in bases.iter().zip(&folders) {
        nodes.push(node(base, f));
    }
    let home = graph(&nodes);
    let r = home.run(&["install", "depa"]);
    let envelope = r.expect(0);
    let installed = common::state::record(&home, "retail").addons.len();
    assert_eq!(installed, 1 + 32, "{}", r.context());
    assert!(record_of(&home, "Dep32").is_none());
    let missing = warnings_with(&envelope, "missing_dependency");
    assert_eq!(missing.len(), 1, "{}", r.context());
    assert!(missing[0].contains("Dep32") && missing[0].contains("DEPENDENCY_MAX_ADDONS"));
}

/// Retail with LittleWigs v1 (requiring nothing) installed, BigWigs v1 out, both named
/// by the catalog; then `extra` releases of LittleWigs at v2.
fn wigs_v2(requires: &[&str], extra: &[Node<'_>]) -> Home {
    let home = Home::new(&["wow"]);
    release_graph(
        &home.http,
        &[
            node(
                "https://github.com/o/littlewigs",
                &[folder("LittleWigs", &[])],
            ),
            node("https://github.com/o/bigwigs", &[folder("BigWigs", &[])]),
        ],
    );
    catalog(
        &home,
        &[
            ("LittleWigs", "littlewigs", &["LittleWigs"]),
            ("BigWigs", "bigwigs", &["BigWigs"]),
        ],
    );
    home.run(&["install", "littlewigs"]).expect(0);
    assert!(record_of(&home, "BigWigs").is_none());
    let v2 = [folder("LittleWigs", requires)];
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/littlewigs",
            tag: "v2",
            folders: &v2,
        }],
    );
    release_graph(&home.http, extra);
    home
}

/// `(request, status)` of the update's items on retail.
fn update_items(envelope: &Value) -> Vec<(String, String)> {
    support::update_items(envelope)
}

fn pair(request: &str, status: &str) -> (String, String) {
    (request.to_owned(), status.to_owned())
}

#[test]
fn an_update_whose_new_release_requires_a_dependency_installs_it_and_undo_reverts_both() {
    let home = wigs_v2(&["BigWigs"], &[]);
    let addons = home.addons("_retail_");
    let before = tree_hash(&addons);
    let latest_before = fetches(&home, "bigwigs");
    let r = home.run(&["update", "--refresh"]);
    let envelope = r.expect(0);
    // Items by name, as `update` lists them, each dependency right after its requester.
    assert_eq!(
        update_items(&envelope),
        [pair("LittleWigs", "updated"), pair("BigWigs", "installed")],
        "{}",
        r.context()
    );
    let lw = record_of(&home, "LittleWigs").unwrap();
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(lw.version, "v2");
    assert_eq!(
        (bw.reason, names(&bw.required_by), bw.tx_id),
        (AddonReason::Dependency, vec!["LittleWigs"], lw.tx_id)
    );
    assert_eq!(
        common::state::history(&home).len(),
        2,
        "one line for the update"
    );
    assert_eq!(fetches(&home, "bigwigs") - latest_before, 1, "fetched once");

    let r = home.run(&["undo"]);
    r.expect(0);
    assert_eq!(tree_hash(&addons), before, "{}", r.context());
    assert!(record_of(&home, "BigWigs").is_none());
    assert_eq!(record_of(&home, "LittleWigs").unwrap().version, "v1");
}

#[test]
fn a_removed_dependency_comes_back_with_its_requesters_update() {
    let home = wigs();
    home.run(&["install", "littlewigs"]).expect(0);
    home.run(&["remove", "bigwigs", "-t", "retail"]).expect(0);
    assert!(record_of(&home, "BigWigs").is_none());
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/littlewigs",
            tag: "v2",
            folders: &[folder("LittleWigs", &["BigWigs"])],
        }],
    );
    let r = home.run(&["update", "--refresh"]);
    r.expect(0);
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(
        (bw.reason, names(&bw.required_by)),
        (AddonReason::Dependency, vec!["LittleWigs"]),
        "{}",
        r.context()
    );
}

#[test]
fn update_no_deps_updates_only_the_requester_and_warns() {
    let home = wigs_v2(&["BigWigs"], &[]);
    let r = home.run(&["update", "--refresh", "--no-deps"]);
    let envelope = r.expect(0);
    assert_eq!(
        update_items(&envelope),
        [pair("LittleWigs", "updated")],
        "{}",
        r.context()
    );
    assert!(record_of(&home, "BigWigs").is_none());
    let missing = warnings_with(&envelope, "missing_dependency");
    assert_eq!(missing.len(), 1, "{}", r.context());
    assert!(missing[0].contains("BigWigs"));
}

#[test]
fn a_dependency_without_a_compatible_release_during_update_is_a_failed_item() {
    let vanilla = [common::graph::Folder {
        name: "DepB",
        interface: "11509",
        deps: &[],
        optional: &[],
    }];
    let home = wigs_v2(&["DepB"], &[node("https://github.com/o/depb", &vanilla)]);
    catalog(
        &home,
        &[
            ("LittleWigs", "littlewigs", &["LittleWigs"]),
            ("DepB", "depb", &["DepB"]),
        ],
    );
    let r = home.run(&["update", "--refresh"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    // The dependency right after its requester.
    assert_eq!(
        update_items(&envelope),
        [pair("LittleWigs", "updated"), pair("DepB", "failed")],
        "{}",
        r.context()
    );
    let failed = &envelope["data"]["targets"][0]["items"][1];
    assert_eq!(failed["ref"], "gh:o/depb");
    assert!(failed["skip_reason"].is_null(), "never skipped/blocked");
    assert_eq!(
        (&failed["reason"], &failed["required_by"]),
        (&json!("dependency"), &json!(["LittleWigs"]))
    );
    assert_eq!(
        failed["error"]["details"]["required_by"],
        json!(["LittleWigs"])
    );
    assert_eq!(record_of(&home, "LittleWigs").unwrap().version, "v2");
    home.run(&["undo"]).expect(0);
    home.run(&["update", "--refresh", "--strict"]).expect(6);
}

#[test]
fn a_beta_update_takes_its_dependencies_from_the_default_channel() {
    let home = wigs_v2(&[], &[]);
    // The API lists a newer prerelease of LittleWigs that requires BigWigs.
    let beta = [folder("LittleWigs", &["BigWigs"])];
    let toc = common::graph::folder_toc(&beta[0], "v3-beta");
    let assets: [common::http::Asset<'_>; 1] = [(
        "littlewigs.zip",
        &[],
        vec![("LittleWigs/LittleWigs.toc", toc.as_str())],
    )];
    let base = "https://github.com/o/littlewigs";
    common::http::gh_release_v(&home.http, base, "v3-beta", &assets);
    let list = json!([
        {"tag_name": "v3-beta", "prerelease": true, "draft": false,
         "published_at": "2026-10-15T00:00:00Z",
         "assets": [{"name": "littlewigs.zip",
                     "browser_download_url": format!("{base}/releases/download/v3-beta/littlewigs.zip")}]},
        {"tag_name": "v2", "prerelease": false, "draft": false,
         "published_at": "2026-10-01T00:00:00Z", "assets": []}
    ])
    .to_string();
    common::http::fixture(
        &home.http,
        "api-releases-littlewigs",
        "https://api.github.com/repos/o/littlewigs/releases?per_page=20",
        200,
        &[("content-type", "application/json")],
        Some(&list),
    );
    let r = home.run(&["update", "littlewigs", "--channel", "beta"]);
    r.expect(0);
    let lw = record_of(&home, "LittleWigs").unwrap();
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(
        (lw.channel, lw.version.as_str(), bw.channel),
        (Channel::Beta, "v3-beta", Channel::Stable),
        "{}",
        r.context()
    );
}

/// The `(action, argv)` of an envelope's commands.
fn hints(envelope: &Value) -> Vec<(String, Value)> {
    envelope["commands"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            (
                c["action"].as_str().unwrap_or_default().to_owned(),
                c["argv"].clone(),
            )
        })
        .collect()
}

#[test]
fn an_install_with_one_dependency_in_json_and_text() {
    let home = wigs();
    let r = home.run(&["install", "littlewigs"]);
    insta::assert_json_snapshot!("install_with_dependency", support::stable(&r.expect(0)));
    let text = support::human(&wigs(), &["install", "littlewigs"]);
    assert!(
        text.contains("BigWigs v1 (1 folder) (dependency of LittleWigs)"),
        "{text}"
    );
    insta::assert_snapshot!("install_with_dependency_human", text);
}

#[test]
fn a_missing_dependency_with_candidates_hints_at_most_three_installs() {
    let home = Home::new(&["wow"]);
    let aces = ["acea", "aceb", "acec", "aced"];
    let bases: Vec<String> = aces
        .iter()
        .map(|a| format!("https://github.com/o/{a}"))
        .collect();
    let ace = [folder("Ace", &[])];
    let little = [folder("LittleWigs", &["Ace"])];
    let mut nodes = vec![node("https://github.com/o/littlewigs", &little)];
    nodes.extend(bases.iter().map(|b| node(b, &ace)));
    release_graph(&home.http, &nodes);
    let mut entries: Vec<(&str, &str, &[&str])> =
        vec![("LittleWigs", "littlewigs", &["LittleWigs"])];
    let names = ["AceA", "AceB", "AceC", "AceD"];
    entries.extend(names.iter().zip(aces).map(|(n, r)| (*n, r, &["Ace"][..])));
    catalog(&home, &entries);

    let r = home.run(&["install", "littlewigs"]);
    let envelope = r.expect(0);
    let unresolved = &items(&envelope)[0]["unresolved"];
    assert_eq!(
        (&unresolved[0]["folder"], &unresolved[0]["reason"]),
        (&json!("Ace"), &json!("several_catalog_entries")),
        "{}",
        r.context()
    );
    assert_eq!(
        unresolved[0]["candidates"].as_array().map(Vec::len),
        Some(3)
    );
    let top = unresolved[0]["candidates"][0].as_str().unwrap().to_owned();
    assert_eq!(
        warning(&envelope, "missing_dependency")["fix_argv"],
        json!(["omaddons", "install", top, "-t", "retail"])
    );
    let actions: Vec<String> = hints(&envelope).into_iter().map(|(a, _)| a).collect();
    assert_eq!(actions, ["undo", "install", "install", "install"]);
    insta::assert_json_snapshot!(
        "install_missing_with_candidates",
        support::stable(&envelope)
    );
    home.run(&["undo"]).expect(0);
    insta::assert_snapshot!(
        "install_missing_with_candidates_human",
        support::human(&home, &["install", "littlewigs"])
    );
    // A hint runs.
    let argv: Vec<String> = hints(&envelope)[1].1.as_array().unwrap()[1..]
        .iter()
        .map(|a| a.as_str().unwrap().to_owned())
        .collect();
    let r = home.exec(&[&argv[..], &["--json".to_owned()]].concat());
    r.expect(0);
}

#[test]
fn a_missing_dependency_without_candidates_gets_the_no_source_fix_and_no_hint() {
    let home = Home::new(&["wow"]);
    release_graph(
        &home.http,
        &[node(
            "https://github.com/o/littlewigs",
            &[folder("LittleWigs", &["Nowhere"])],
        )],
    );
    catalog(&home, &[("LittleWigs", "littlewigs", &["LittleWigs"])]);
    // Tukui and WoWInterface are consulted, and neither ships it.
    common::tukui::tukui_list(&home.http, &[common::tukui::elvui()]);
    common::wowi::wowi_filelist(
        &home.http,
        &[common::wowi::FileEntry::new(5995, "BugSack", &["BugSack"])],
    );
    let r = home.run(&["install", "littlewigs"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope)[0]["unresolved"],
        json!([{"folder": "Nowhere", "reason": "no_source", "candidates": []}]),
        "{}",
        r.context()
    );
    let w = warning(&envelope, "missing_dependency");
    assert_eq!(w["fix_argv"], json!(["omaddons", "search", "Nowhere"]));
    let actions: Vec<String> = hints(&envelope).into_iter().map(|(a, _)| a).collect();
    assert_eq!(
        actions,
        ["undo"],
        "a search is the warning's fix, not an install hint"
    );
    insta::assert_json_snapshot!("install_missing_no_source", support::stable(&envelope));
    home.run(&["undo"]).expect(0);
    insta::assert_snapshot!(
        "install_missing_no_source_human",
        support::human(&home, &["install", "littlewigs"])
    );
}

/// Retail; Foo ships `Foo` and the bridge `Foo_Masque`, which works with Masque (in the
/// catalog).
fn masque_bridge() -> Home {
    let home = Home::new(&["wow"]);
    release_graph(
        &home.http,
        &[
            node(
                "https://github.com/o/foo",
                &[folder("Foo", &[]), folder("Foo_Masque", &["Masque"])],
            ),
            node("https://github.com/o/masque", &[folder("Masque", &[])]),
        ],
    );
    catalog(&home, &[("Masque", "masque", &["Masque"])]);
    home
}

#[test]
fn a_bridge_dependency_warns_on_the_first_install_only() {
    let home = masque_bridge();
    let r = home.run(&["install", "gh:o/foo"]);
    let envelope = r.expect(0);
    let bridge = json!([{"folder": "Foo_Masque", "requires": "Masque"}]);
    assert_eq!(
        items(&envelope)[0]["bridge_dependencies"],
        bridge,
        "{}",
        r.context()
    );
    let fix = json!(["omaddons", "install", "Masque", "-t", "retail"]);
    let w = warning(&envelope, "bridge_dependency");
    assert_eq!(w["fix_argv"], fix, "{}", r.context());
    assert_eq!(
        w["message"],
        "Foo_Masque works with Masque, which is not installed on retail"
    );
    assert!(hints(&envelope).contains(&("install".to_owned(), fix.clone())));
    assert!(
        record_of(&home, "Masque").is_none(),
        "reported, never installed"
    );
    insta::assert_json_snapshot!("install_bridge_dependency", support::stable(&envelope));
    home.run(&["undo"]).expect(0);
    insta::assert_snapshot!(
        "install_bridge_dependency_human",
        support::human(&home, &["install", "gh:o/foo"])
    );

    // An update keeps the field and says nothing more.
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/foo",
            tag: "v2",
            folders: &[folder("Foo", &[]), folder("Foo_Masque", &["Masque"])],
        }],
    );
    let r = home.run(&["update", "--refresh"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["targets"][0]["items"][0];
    assert_eq!(
        (
            &item["status"],
            &item["bridge_dependencies"],
            &item["unresolved"]
        ),
        (&json!("updated"), &bridge, &json!([])),
        "{}",
        r.context()
    );
    assert_eq!(warning(&envelope, "bridge_dependency"), Value::Null);

    // The hint runs, and then nothing is open.
    let r = home.run(&["install", "Masque", "-t", "retail"]);
    r.expect(0);
    assert!(record_of(&home, "Masque").is_some(), "{}", r.context());
}

#[test]
fn optional_dependencies_show_under_verbose_only() {
    let home = Home::new(&["wow"]);
    let with_optional = [common::graph::Folder {
        name: "LittleWigs",
        interface: RETAIL,
        deps: &[],
        optional: &["Opt"],
    }];
    release_graph(
        &home.http,
        &[node("https://github.com/o/littlewigs", &with_optional)],
    );
    catalog(&home, &[("LittleWigs", "littlewigs", &["LittleWigs"])]);
    let r = home.run(&["install", "littlewigs", "--dry-run"]);
    assert!(
        items(&r.expect(0))[0]
            .get("optional_dependencies")
            .is_none()
    );
    let r = home.run(&["install", "littlewigs", "-v"]);
    assert_eq!(
        items(&r.expect(0))[0]["optional_dependencies"],
        json!(["Opt"]),
        "{}",
        r.context()
    );
}

/// The first warning with `code`, `null` when there is none.
fn warning(envelope: &Value, code: &str) -> Value {
    envelope["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|w| w["code"] == code)
        .cloned()
        .unwrap_or(Value::Null)
}

/// The retail `status` (`args` added), and its `remove_orphans` hint (`null` if none).
fn orphan_status(home: &Home, args: &[&str]) -> (Value, Value) {
    let mut argv = vec!["status"];
    argv.extend(args);
    let envelope = home.run(&argv).expect(0);
    let hint = envelope["commands"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["action"] == "remove_orphans")
        .cloned()
        .unwrap_or(Value::Null);
    (envelope["data"]["targets"][0].clone(), hint)
}

#[test]
fn removing_a_dependency_still_required_goes_ahead_and_warns_with_undo() {
    let home = wigs();
    home.run(&["install", "littlewigs"]).expect(0);

    // Planned only: the reminder, as text.
    let r = home.run(&["remove", "bigwigs", "-t", "retail", "--dry-run"]);
    let w = warning(&r.expect(0), "still_required");
    assert!(
        w["message"]
            .as_str()
            .is_some_and(|m| m.contains("LittleWigs")),
        "{}",
        r.context()
    );
    assert_eq!(w["fix_argv"], Value::Null);
    assert!(record_of(&home, "BigWigs").is_some());

    let r = home.run(&["remove", "bigwigs", "-t", "retail"]);
    let envelope = r.expect(0);
    let tx = common::state::history(&home).pop().unwrap().tx.to_string();
    let w = warning(&envelope, "still_required");
    assert_eq!(
        w["fix_argv"],
        json!(["omaddons", "undo", tx]),
        "{}",
        r.context()
    );
    assert!(w["message"].as_str().unwrap().contains("LittleWigs"));
    assert_eq!(warning(&envelope, "orphaned_dependency"), Value::Null);
    assert!(record_of(&home, "BigWigs").is_none());
    let (kind, edit) = last_line(&home, "LittleWigs");
    assert_eq!(
        (kind, edit),
        (json!("remove"), None),
        "the requester untouched"
    );

    home.run(&["undo", &tx]).expect(0);
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(
        (bw.reason, names(&bw.required_by)),
        (AddonReason::Dependency, vec!["LittleWigs"])
    );
}

#[test]
fn removing_the_requester_orphans_its_dependency_until_the_recommendation_runs() {
    let home = wigs();
    home.run(&["install", "littlewigs"]).expect(0);
    let r = home.run(&["remove", "littlewigs", "-t", "retail"]);
    let envelope = r.expect(0);
    let fix = json!(["omaddons", "remove", "BigWigs", "-t", "retail"]);
    assert_eq!(
        warning(&envelope, "orphaned_dependency")["fix_argv"],
        fix,
        "{}",
        r.context()
    );
    assert_eq!(warning(&envelope, "still_required"), Value::Null);
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(
        (bw.reason, names(&bw.required_by)),
        (AddonReason::Dependency, vec![])
    );
    assert_eq!(
        last_line(&home, "BigWigs"),
        (
            json!("remove"),
            Some(HistoryEdit::RequiredByRemove("LittleWigs".into()))
        )
    );

    for args in [&[][..], &["--cached"]] {
        let (target, hint) = orphan_status(&home, args);
        assert_eq!(target["orphans"], 1, "{args:?}: {target}");
        assert_eq!(
            (&hint["argv"], &hint["destructive"]),
            (&fix, &json!(true)),
            "{args:?}"
        );
    }
    let list = home.run(&["list"]).expect(0);
    let listed = list["data"]["targets"][0]["addons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "BigWigs")
        .cloned()
        .unwrap_or(Value::Null);
    let info = home.run(&["info", "bigwigs"]).expect(0);
    for shown in [&listed, &info["data"]["installed"][0]] {
        assert_eq!(
            (&shown["reason"], &shown["required_by"], &shown["orphan"]),
            (&json!("dependency"), &json!([]), &json!(true)),
            "{shown}"
        );
    }

    // Never removed by itself; the recommendation removes it.
    let argv: Vec<&str> = fix.as_array().unwrap()[1..]
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    home.run(&argv).expect(0);
    assert!(record_of(&home, "BigWigs").is_none());
    let (target, hint) = orphan_status(&home, &[]);
    assert_eq!((&target["orphans"], &hint), (&json!(0), &Value::Null));
}

#[test]
fn undoing_the_remove_restores_required_by_even_after_the_dependency_updated() {
    let home = wigs();
    home.run(&["install", "littlewigs"]).expect(0);
    home.run(&["remove", "littlewigs", "-t", "retail"])
        .expect(0);
    let remove = common::state::history(&home).pop().unwrap().tx.to_string();
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/bigwigs",
            tag: "v2",
            folders: &[folder("BigWigs", &[])],
        }],
    );
    home.run(&["update", "bigwigs", "--refresh"]).expect(0);
    assert_eq!(record_of(&home, "BigWigs").unwrap().version, "v2");

    let r = home.run(&["undo", &remove]);
    let envelope = r.expect(0);
    assert_eq!(
        warning(&envelope, "undo_conflict"),
        Value::Null,
        "{}",
        r.context()
    );
    assert!(record_of(&home, "LittleWigs").is_some());
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(
        (bw.version.as_str(), names(&bw.required_by)),
        ("v2", vec!["LittleWigs"]),
        "the dependency keeps its update and gets its requester back"
    );
}

#[test]
fn removing_a_requester_with_its_dependency_warns_nothing() {
    let home = wigs();
    home.run(&["install", "littlewigs"]).expect(0);
    let r = home.run(&["remove", "littlewigs", "bigwigs", "-t", "retail"]);
    let envelope = r.expect(0);
    for code in ["still_required", "orphaned_dependency"] {
        assert_eq!(warning(&envelope, code), Value::Null, "{}", r.context());
    }
    assert!(record_of(&home, "LittleWigs").is_none() && record_of(&home, "BigWigs").is_none());
    let line = common::state::history(&home).pop().unwrap();
    assert!(
        line.ops.iter().all(|op| op.edit.is_none()),
        "{:?}",
        line.ops
    );
}

#[test]
fn an_update_whose_new_release_drops_a_dependency_unrequires_it() {
    for extra in [&[][..], &["--no-deps"]] {
        let home = wigs();
        home.run(&["install", "littlewigs"]).expect(0);
        release_graph(
            &home.http,
            &[Node {
                base: "https://github.com/o/littlewigs",
                tag: "v2",
                folders: &[folder("LittleWigs", &[])],
            }],
        );
        let mut argv = vec!["update", "--refresh"];
        argv.extend(extra);
        let r = home.run(&argv);
        let envelope = r.expect(0);
        assert_eq!(
            warning(&envelope, "orphaned_dependency")["fix_argv"],
            json!(["omaddons", "remove", "BigWigs", "-t", "retail"]),
            "{extra:?}: {}",
            r.context()
        );
        assert_eq!(record_of(&home, "LittleWigs").unwrap().version, "v2");
        assert!(record_of(&home, "BigWigs").unwrap().required_by.is_empty());
        assert_eq!(
            last_line(&home, "BigWigs"),
            (
                json!("update"),
                Some(HistoryEdit::RequiredByRemove("LittleWigs".into()))
            ),
            "{extra:?}"
        );
    }
}

#[test]
fn an_explicit_or_promoted_addon_never_becomes_an_orphan() {
    let home = wigs();
    home.run(&["install", "littlewigs"]).expect(0);
    // Installing a dependency by name promotes it.
    home.run(&["install", "bigwigs"]).expect(0);
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(
        (bw.reason, names(&bw.required_by)),
        (AddonReason::Explicit, vec!["LittleWigs"])
    );
    let r = home.run(&["remove", "littlewigs", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(
        warning(&envelope, "orphaned_dependency"),
        Value::Null,
        "{}",
        r.context()
    );
    assert!(record_of(&home, "BigWigs").unwrap().required_by.is_empty());
    let (target, hint) = orphan_status(&home, &[]);
    assert_eq!((&target["orphans"], &hint), (&json!(0), &Value::Null));
}

/// The engine API: dependency locks and the failure's `details.required_by`.
#[cfg(test)]
mod engine_api {
    use std::collections::{BTreeMap, BTreeSet};

    use omaddons::discover::DiscoverySource;
    use omaddons::discover::battlenet::{BattlenetHints, discover_root};
    use omaddons::discover::registry::reconcile;
    use omaddons::engine::inspect::ArchiveLimits;
    use omaddons::engine::plan::{PinIntent, RequestedChange};
    use omaddons::engine::retention::RetentionLimits;
    use omaddons::engine::{EngineCtx, TxOutcomes, TxRequest, TxTarget};
    use omaddons::model::{Actor, AddonReason, Channel, Ref, RunId, TxKind};
    use omaddons::output::ProgressSink;
    use omaddons::resolve::catalog::Catalog;
    use omaddons::rules::RulesTable;
    use omaddons::sources::{SourceConfigView, SourceRegistry};
    use omaddons::state::config::RunningPolicy;
    use omaddons::state::registry::load_registry;
    use serde_json::json;

    use omaddons::resolve::deps::BridgeDependency;

    use super::common::graph::{Node, release_graph};
    use super::common::home::Home;
    use super::common::http::{paths, replay};
    use super::common::netlog::net_log;
    use super::support::{catalog, folder, record_of};

    /// A user's plain install of `r`.
    fn installing(r: &str) -> RequestedChange {
        RequestedChange::Install {
            r#ref: r.parse().unwrap(),
            channel: Channel::Stable,
            channel_explicit: false,
            version: None,
            file: None,
            name: None,
            pin: PinIntent::Keep,
            reason: AddonReason::Explicit,
            reinstall: false,
            blob_sha256: None,
            asset: None,
        }
    }

    /// Installs `r` on retail through the engine with `locks`.
    fn install(home: &Home, r: &str, locks: BTreeMap<Ref, String>) -> TxOutcomes {
        run(home, vec![installing(r)], locks)
    }

    /// Runs `changes` on retail through the engine with `locks`.
    fn run(home: &Home, changes: Vec<RequestedChange>, locks: BTreeMap<Ref, String>) -> TxOutcomes {
        let paths = paths(&home.env);
        let table = RulesTable::builtin();
        let (found, _) = discover_root(
            &home.root,
            &table,
            &BattlenetHints::default(),
            "battlenet",
            &DiscoverySource::Scan,
        );
        let registry = load_registry(&paths).unwrap().value;
        let retail = reconcile(&registry, &found)
            .targets
            .into_iter()
            .find(|d| d.target.id == "retail")
            .unwrap();
        let req = TxRequest {
            targets: vec![TxTarget {
                discovered: retail,
                changes,
                default_kind: TxKind::Install,
                dependency_locks: locks,
            }],
            force: false,
            while_running: false,
            dry_run: false,
            reason: None,
            kind: None,
            run: RunId::new(),
            no_deps: false,
        };
        let http = replay(&home.env, &home.http);
        let sources = SourceRegistry::builtin(&SourceConfigView::all_enabled());
        let argv: Vec<String> = vec!["omaddons".into(), "install".into()];
        let nothing_runs = BTreeSet::new();
        let (catalog, warnings) = Catalog::load(&paths);
        assert!(warnings.is_empty(), "{warnings:?}");
        let ctx = EngineCtx {
            paths: &paths,
            http: http.as_ref(),
            registry: &sources,
            table: &table,
            policy: RunningPolicy::default(),
            limits: ArchiveLimits::default(),
            retention: RetentionLimits::default(),
            actor: Actor::Agent,
            argv: &argv,
            command: "install",
            running: Some(&nothing_runs),
            known_targets: std::slice::from_ref(&req.targets[0].discovered.target),
            cache: None,
            progress: &ProgressSink::none(),
            catalog: Some(&catalog),
            default_channel: Channel::Stable,
            downloads: None,
            indexes: None,
            notes: None,
            binding: None,
        };
        omaddons::engine::run(&req, &ctx).unwrap()
    }

    #[test]
    fn a_dependency_lock_fetches_that_version_unpinned() {
        let home = super::support::wigs();
        // v2 is BigWigs' latest; the lock asks for v1.
        release_graph(
            &home.http,
            &[Node {
                base: "https://github.com/o/bigwigs",
                tag: "v2",
                folders: &[folder("BigWigs", &[])],
            }],
        );
        let locks = BTreeMap::from([("gh:o/bigwigs".parse().unwrap(), "v1".to_owned())]);
        let done = install(&home, "gh:o/littlewigs", locks);
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let bw = record_of(&home, "BigWigs").unwrap();
        assert_eq!((bw.version.as_str(), bw.pinned), ("v1", false));

        // Without a lock: the latest.
        let fresh = super::support::wigs();
        release_graph(
            &fresh.http,
            &[Node {
                base: "https://github.com/o/bigwigs",
                tag: "v2",
                folders: &[folder("BigWigs", &[])],
            }],
        );
        install(&fresh, "gh:o/littlewigs", BTreeMap::new());
        assert_eq!(record_of(&fresh, "BigWigs").unwrap().version, "v2");
    }

    #[test]
    fn a_failed_dependency_names_its_requesters() {
        let home = Home::new(&["wow"]);
        release_graph(
            &home.http,
            &[
                Node {
                    base: "https://github.com/o/depa",
                    tag: "v1",
                    folders: &[folder("DepA", &["DepB"])],
                },
                Node {
                    base: "https://github.com/o/depb",
                    tag: "v1",
                    folders: &[super::common::graph::Folder {
                        name: "DepB",
                        interface: "11509",
                        deps: &[],
                        optional: &[],
                    }],
                },
            ],
        );
        catalog(&home, &[("DepB", "depb", &["DepB"])]);
        let done = install(&home, "gh:o/depa", BTreeMap::new());
        assert_eq!(done.failures.len(), 1, "{:?}", done.failures);
        let failure = &done.failures[0];
        assert_eq!(failure.r#ref, Some("gh:o/depb".parse().unwrap()));
        assert_eq!(failure.error.details["required_by"], json!(["DepA"]));
        assert!(done.outcomes[0].tx.is_some(), "the requester still commits");
        assert!(record_of(&home, "DepA").is_some());
        let report = &done.plan.targets[0].dependencies["DepA"];
        assert_eq!(report.failed.len(), 1);
    }

    /// Foo (primary) ships `Foo_Masque`, which requires Masque; the catalog knows
    /// Masque, so following the bridge would fetch it.
    fn bridged() -> Home {
        let home = Home::new(&["wow"]);
        release_graph(
            &home.http,
            &[
                Node {
                    base: "https://github.com/o/foo",
                    tag: "v1",
                    folders: &[folder("Foo", &[]), folder("Foo_Masque", &["Masque"])],
                },
                Node {
                    base: "https://github.com/o/masque",
                    tag: "v1",
                    folders: &[folder("Masque", &[])],
                },
            ],
        );
        catalog(&home, &[("Masque", "masque", &["Masque"])]);
        home
    }

    fn bridges(done: &TxOutcomes) -> Option<(Vec<BridgeDependency>, bool)> {
        let report = done.plan.targets[0].dependencies.get("Foo")?;
        Some((report.bridge_dependencies.clone(), report.first_install))
    }

    fn masque_requests(home: &Home) -> usize {
        net_log(home)
            .entries
            .iter()
            .filter(|e| e.path.starts_with("/o/masque/"))
            .count()
    }

    #[test]
    fn a_bridge_requirement_is_reported_never_installed() {
        let home = bridged();
        let done = install(&home, "gh:o/foo", BTreeMap::new());
        let want = vec![BridgeDependency {
            folder: omaddons::model::FolderName::try_from("Foo_Masque".to_owned()).unwrap(),
            requires: omaddons::model::FolderName::try_from("Masque".to_owned()).unwrap(),
        }];
        assert_eq!(
            bridges(&done),
            Some((want.clone(), true)),
            "{:?}",
            done.failures
        );
        assert_eq!(masque_requests(&home), 0, "never fetched");
        assert!(record_of(&home, "Masque").is_none());

        // An update reports it again, not as a first install.
        release_graph(
            &home.http,
            &[Node {
                base: "https://github.com/o/foo",
                tag: "v2",
                folders: &[folder("Foo", &[]), folder("Foo_Masque", &["Masque"])],
            }],
        );
        let update = RequestedChange::Update {
            addon: "Foo".into(),
            installed_primary_interface: None,
        };
        let done = run(&home, vec![update], BTreeMap::new());
        assert_eq!(record_of(&home, "Foo").unwrap().version, "v2");
        assert_eq!(bridges(&done), Some((want, false)));
        assert_eq!(masque_requests(&home), 0);
    }

    #[test]
    fn a_bridge_requirement_met_on_disk_or_in_the_transaction_is_not_reported() {
        // On disk, unmanaged.
        let home = bridged();
        super::addon_folder(
            &home.addons("_retail_"),
            "Masque",
            &[("Masque.toc", &super::toc(super::RETAIL, &[]))],
            &[],
        );
        let done = install(&home, "gh:o/foo", BTreeMap::new());
        assert!(
            bridges(&done).is_none_or(|(b, _)| b.is_empty()),
            "{:?}",
            bridges(&done)
        );

        // Installed by the same transaction, after Foo.
        let home = bridged();
        let done = run(
            &home,
            vec![installing("gh:o/foo"), installing("gh:o/masque")],
            BTreeMap::new(),
        );
        assert!(record_of(&home, "Masque").is_some());
        assert!(
            bridges(&done).is_none_or(|(b, _)| b.is_empty()),
            "{:?}",
            bridges(&done)
        );
    }
}

/// After the catalog: Tukui's list, then WoWInterface's index, each loaded once per
/// command and only when a lookup reaches it; offline from what is stored; skipped, and
/// said so, when its source is off.
#[cfg(test)]
mod via_indexes {
    use super::common::tukui::{elvui, etag, tukui_addon, tukui_download, tukui_list};
    use super::common::wowi::{FileEntry, details, wowi_details, wowi_file, wowi_filelist};
    use super::*;

    /// A plugin requiring `requires`, from `gh:o/plug`, named by the catalog.
    fn plugin(requires: &[&'static str]) -> Home {
        let deps: &'static [&'static str] = Box::leak(requires.to_vec().into_boxed_slice());
        let folders: &'static [common::graph::Folder<'static>] =
            Box::leak(vec![folder("Plug", deps)].into_boxed_slice());
        graph(&[node("https://github.com/o/plug", folders)])
    }

    /// Serves `wowi:<uid>` (one per details batch) with one retail folder each.
    fn wowi_files(home: &Home, projects: &[(u64, &str)]) {
        let mut all = Vec::new();
        for (uid, folder_name) in projects {
            let toc = toc(RETAIL, &[&format!("## Title: {folder_name}")]);
            let zip = zip_text(&[(&format!("{folder_name}/{folder_name}.toc"), &toc)]);
            let md5 = wowi_file(&home.http, *uid, &zip);
            all.push(details(*uid, folder_name, "1.0", &md5));
        }
        let ids: Vec<u64> = projects.iter().map(|(uid, _)| *uid).collect();
        wowi_details(&home.http, &ids, &all);
    }

    fn purpose(home: &Home, purpose: &str) -> usize {
        net_log(home).count_purpose(purpose)
    }

    #[test]
    fn a_folder_only_tukui_ships_installs_from_tukui() {
        let home = plugin(&["ElvUI"]);
        // ElvUI's own catalog entry would resolve it first.
        fs::write(
            home.env
                .config()
                .join("omaddons/catalog.d/60-no-elvui.toml"),
            "schema_version = \"1.0\"\n\n[[addon]]\nname = \"ElvUI\"\nremove = true\n",
        )
        .unwrap();
        let entry = elvui();
        tukui_list(&home.http, std::slice::from_ref(&entry));
        tukui_addon(&home.http, &entry, &["T1", "T2"]);
        let toc = toc(RETAIL, &["## Title: ElvUI"]);
        let zip = zip_text(&[
            ("ElvUI/ElvUI.toc", &toc),
            ("ElvUI_Libraries/ElvUI_Libraries.toc", &toc),
            ("ElvUI_Options/ElvUI_Options.toc", &toc),
        ]);
        for token in ["T1", "T2"] {
            tukui_download(&home.http, "elvui", token, &zip, Some(&etag(&zip)));
        }
        let r = home.run(&["install", "gh:o/plug"]);
        let envelope = r.expect(0);
        let elv = record_of(&home, "ElvUI");
        assert!(elv.is_some(), "{}", r.context());
        let elv = elv.unwrap();
        assert_eq!(
            (elv.r#ref.to_string(), elv.reason, elv.required_by.clone()),
            (
                "tukui:elvui".to_owned(),
                AddonReason::Dependency,
                vec!["Plug".to_owned()]
            )
        );
        assert_eq!(envelope["partial"], false);
        // Tukui answered: WoWInterface's index was never needed.
        assert_eq!(purpose(&home, "wowi-filelist"), 0);
    }

    #[test]
    fn a_folder_only_wowinterface_ships_installs_from_it_verified() {
        let home = plugin(&["LibOnly"]);
        tukui_list(&home.http, &[elvui()]);
        wowi_filelist(
            &home.http,
            &[
                FileEntry::new(4001, "LibOnly", &["LibOnly"]),
                FileEntry::new(4002, "Bundle", &["LibOnly", "Other"]),
            ],
        );
        wowi_files(&home, &[(4001, "LibOnly")]);
        let r = home.run(&["install", "gh:o/plug"]);
        let envelope = r.expect(0);
        let lib = record_of(&home, "LibOnly");
        assert!(lib.is_some(), "{}", r.context());
        let lib = lib.unwrap();
        assert_eq!(
            (lib.r#ref.to_string(), lib.reason),
            ("wowi:4001".to_owned(), AddonReason::Dependency)
        );
        assert!(
            lib.release_id
                .as_deref()
                .is_some_and(|id| id.starts_with("wowi:4001@")),
            "the content identity is the MD5: {lib:?}"
        );
        assert_eq!(envelope["partial"], false);
        assert_eq!(purpose(&home, "wowi-filelist"), 1);
    }

    #[test]
    fn an_abandoned_project_is_a_candidate_never_an_install() {
        let home = plugin(&["OldLib"]);
        tukui_list(&home.http, &[elvui()]);
        let mut old = FileEntry::new(4003, "OldLib", &["OldLib"]);
        old.date_ms = 1_500_000_000_000;
        wowi_filelist(&home.http, &[old]);
        let r = home.run(&["install", "gh:o/plug"]);
        let envelope = r.expect(0);
        assert!(record_of(&home, "OldLib").is_none());
        assert_eq!(envelope["partial"], false, "{}", r.context());
        let missing = warnings_with(&envelope, "missing_dependency");
        assert_eq!(missing.len(), 1, "{}", r.context());
        assert!(missing[0].contains("wowi:4003") && missing[0].contains("two years"));
    }

    #[test]
    fn the_catalog_first_and_each_index_once_per_command() {
        // A catalog dependency asks neither index.
        let home = wigs();
        home.run(&["install", "littlewigs"]).expect(0);
        assert_eq!(
            (
                purpose(&home, "tukui-list"),
                purpose(&home, "wowi-filelist")
            ),
            (0, 0)
        );

        // Two WoWInterface-only dependencies: one file list.
        let home = plugin(&["LibA", "LibB"]);
        tukui_list(&home.http, &[elvui()]);
        wowi_filelist(
            &home.http,
            &[
                FileEntry::new(4011, "LibA", &["LibA"]),
                FileEntry::new(4012, "LibB", &["LibB"]),
            ],
        );
        wowi_files(&home, &[(4011, "LibA"), (4012, "LibB")]);
        let r = home.run(&["install", "gh:o/plug"]);
        r.expect(0);
        assert!(
            record_of(&home, "LibA").is_some() && record_of(&home, "LibB").is_some(),
            "{}",
            r.context()
        );
        assert_eq!(
            (
                purpose(&home, "tukui-list"),
                purpose(&home, "wowi-filelist")
            ),
            (1, 1)
        );
    }

    #[test]
    fn offline_reads_the_stored_index_and_never_downloads_it() {
        use omaddons::sources::wowi::index::StoredIndex;
        use omaddons::state::store;

        // Online once: the index is stored.
        let home = plugin(&["LibOnly"]);
        tukui_list(&home.http, &[elvui()]);
        wowi_filelist(
            &home.http,
            &[
                FileEntry::new(4001, "LibOnly", &["LibOnly"]),
                FileEntry::new(4021, "LibLater", &["LibLater"]),
            ],
        );
        wowi_files(&home, &[(4001, "LibOnly")]);
        home.run(&["install", "gh:o/plug"]).expect(0);
        // A day later, offline: a requester installed from a file needs LibLater.
        let path = common::http::paths(&home.env).wowi_index();
        let mut stored = store::load::<StoredIndex>(&path).unwrap().unwrap().value;
        stored.fetched_at -= jiff::SignedDuration::from_hours(24);
        fs::write(&path, store::encode(&stored).unwrap()).unwrap();
        let zip = home.env.root().join("later.zip");
        let later_toc = toc(RETAIL, &["## Dependencies: LibLater"]);
        fs::write(&zip, zip_text(&[("Later/Later.toc", &later_toc)])).unwrap();
        let before = net_log(&home).len();
        let r = home.run(&[
            "install",
            "gh:o/later",
            "--file",
            &zip.display().to_string(),
            "--offline",
        ]);
        let envelope = r.expect(0);
        assert_eq!(net_log(&home).len(), before, "no request offline");
        // The index says it is old once (the release memo says so for itself).
        let stale: Vec<String> = warnings_with(&envelope, "stale_check")
            .into_iter()
            .filter(|w| w.contains("WoWInterface index"))
            .collect();
        assert_eq!(stale.len(), 1, "{}", r.context());
        // Resolved from the stored index; installing it is the offline rules' call: with
        // nothing remembered for it, an `offline` failure.
        let dependency = envelope["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["ref"] == "wowi:4021");
        assert_eq!(
            dependency.map(|d| (&d["request"], &d["status"], &d["error"]["code"])),
            Some((&json!("LibLater"), &json!("failed"), &json!("offline"))),
            "{}",
            r.context()
        );

        // With no stored index: nothing asked, and the warning says why.
        fs::remove_file(&path).unwrap();
        let before = net_log(&home).len();
        let r = home.run(&[
            "install",
            "gh:o/later",
            "--file",
            &zip.display().to_string(),
            "--offline",
            "--reinstall",
        ]);
        let envelope = r.expect(0);
        assert_eq!(net_log(&home).len(), before);
        let missing = warnings_with(&envelope, "missing_dependency");
        assert!(
            missing
                .iter()
                .any(|m| m.contains("WoWInterface (offline, and no index is stored)")),
            "{}",
            r.context()
        );
    }

    #[test]
    fn a_disabled_source_is_skipped_and_named() {
        let home = plugin(&["LibOnly"]);
        tukui_list(&home.http, &[elvui()]);
        wowi_filelist(&home.http, &[FileEntry::new(4001, "LibOnly", &["LibOnly"])]);
        for source in ["wowi", "tukui"] {
            home.run(&[
                "config",
                "set",
                &format!("sources.{source}.enabled"),
                "false",
            ])
            .expect(0);
        }
        let r = home.run(&["install", "gh:o/plug"]);
        let envelope = r.expect(0);
        assert!(record_of(&home, "LibOnly").is_none());
        assert_eq!(
            (
                purpose(&home, "tukui-list"),
                purpose(&home, "wowi-filelist")
            ),
            (0, 0)
        );
        let missing = warnings_with(&envelope, "missing_dependency");
        assert!(
            missing
                .iter()
                .any(|m| m.contains("not consulted: Tukui (disabled); WoWInterface (disabled)")),
            "{}",
            r.context()
        );
    }
}

/// Undoing the install of LittleWigs and BigWigs after Other came to require BigWigs:
/// the undo goes ahead as `remove` would, warning `still_required` for Other (planned
/// and committed), and undoing it brings both back as they were.
#[test]
fn undo_of_an_older_dependency_install_warns_still_required() {
    let home = wigs_and_other();
    home.run(&["install", "littlewigs"]).expect(0);
    let tx1 = last_tx(&home);
    home.run(&["install", "other"]).expect(0);
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(names(&bw.required_by), ["LittleWigs", "Other"]);
    let expected = "On retail, BigWigs is required by Other, which will not load without it";

    let r = home.run(&["undo", &tx1, "--dry-run"]);
    let planned = r.expect(0);
    assert_eq!(
        warnings_with(&planned, "still_required"),
        [expected],
        "{}",
        r.context()
    );

    let r = home.run(&["undo", &tx1]);
    let envelope = r.expect(0);
    assert_eq!(
        warnings_with(&envelope, "still_required"),
        [expected],
        "{}",
        r.context()
    );
    let undo = last_tx(&home);
    let fix = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "still_required")
        .map(|w| w["fix_argv"].clone())
        .unwrap();
    assert_eq!(fix, json!(["omaddons", "undo", undo]));
    let addons = home.addons("_retail_");
    assert!(!addons.join("BigWigs").exists() && !addons.join("LittleWigs").exists());
    assert!(addons.join("Other").is_dir());
    assert!(record_of(&home, "BigWigs").is_none());

    // The fix: BigWigs and LittleWigs come back, BigWigs required by both again.
    home.run(&["undo", &undo]).expect(0);
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(names(&bw.required_by), ["LittleWigs", "Other"]);
    assert!(addons.join("BigWigs").is_dir() && addons.join("LittleWigs").is_dir());
}

/// One `update` run installs BigWigs as LittleWigs v2's new dependency on retail and
/// classic; Other then comes to require it on both. `undo --run` reports
/// `still_required` once per transaction, each naming its target and its own undo.
#[test]
fn undo_of_a_run_warns_still_required_per_transaction() {
    use common::graph::Folder;
    const BOTH: &str = "120105, 50504";
    let on_both = |name: &'static str, deps: &'static [&'static str]| Folder {
        name,
        interface: BOTH,
        deps,
        optional: &[],
    };
    let home = Home::new(&["wow", "wow_classic"]);
    let (lw1, other, bw) = (
        [on_both("LittleWigs", &[])],
        [on_both("Other", &["BigWigs"])],
        [on_both("BigWigs", &[])],
    );
    release_graph(
        &home.http,
        &[
            node("https://github.com/o/littlewigs", &lw1),
            node("https://github.com/o/other", &other),
            node("https://github.com/o/bigwigs", &bw),
        ],
    );
    catalog(
        &home,
        &[
            ("LittleWigs", "littlewigs", &["LittleWigs"]),
            ("Other", "other", &["Other"]),
            ("BigWigs", "bigwigs", &["BigWigs"]),
        ],
    );
    for target in ["retail", "classic"] {
        home.run(&["install", "littlewigs", "-t", target]).expect(0);
    }
    let lw2 = [on_both("LittleWigs", &["BigWigs"])];
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/littlewigs",
            tag: "v2",
            folders: &lw2,
        }],
    );
    let r = home.run(&["update", "--refresh"]);
    let updated = r.expect(0);
    let run = updated["data"]["run"].as_str().unwrap().to_owned();
    for client in ["_retail_", "_classic_"] {
        assert!(
            home.addons(client).join("BigWigs").is_dir(),
            "{}",
            r.context()
        );
    }
    for target in ["retail", "classic"] {
        home.run(&["install", "other", "-t", target]).expect(0);
    }

    let r = home.run(&["undo", "--run", &run]);
    let envelope = r.expect(0);
    let mut warned = warnings_with(&envelope, "still_required");
    warned.sort();
    assert_eq!(
        warned,
        [
            "On classic, BigWigs is required by Other, which will not load without it",
            "On retail, BigWigs is required by Other, which will not load without it",
        ],
        "{}",
        r.context()
    );
    let undos: Vec<Value> = envelope["data"]["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tx"].clone())
        .collect();
    let fixes: Vec<Value> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "still_required")
        .map(|w| w["fix_argv"][2].clone())
        .collect();
    assert_eq!(fixes.len(), 2);
    assert!(
        fixes.iter().all(|f| undos.contains(f)),
        "{fixes:?} {undos:?}"
    );
    assert_ne!(fixes[0], fixes[1], "each names its own undo");
}

/// Undoing BigWigs' update after Other came to require it: BigWigs is back at v1 and
/// still required by both, so removing LittleWigs orphans nothing and removing BigWigs
/// warns for Other.
#[test]
fn undo_of_an_older_dependency_update_keeps_a_later_requester() {
    let home = wigs_and_other();
    home.run(&["install", "littlewigs"]).expect(0);
    release_graph(
        &home.http,
        &[Node {
            base: "https://github.com/o/bigwigs",
            tag: "v2",
            folders: &[folder("BigWigs", &[])],
        }],
    );
    let r = home.run(&["update", "bigwigs", "--refresh"]);
    r.expect(0);
    assert_eq!(
        record_of(&home, "BigWigs").unwrap().version,
        "v2",
        "{}",
        r.context()
    );
    let tx1 = last_tx(&home);
    home.run(&["install", "other"]).expect(0);

    let r = home.run(&["undo", &tx1]);
    let envelope = r.expect(0);
    assert!(
        warnings_with(&envelope, "still_required").is_empty(),
        "{}",
        r.context()
    );
    let bw = record_of(&home, "BigWigs").unwrap();
    assert_eq!(
        (bw.version.as_str(), names(&bw.required_by)),
        ("v1", vec!["LittleWigs", "Other"])
    );

    let r = home.run(&["remove", "littlewigs"]);
    let envelope = r.expect(0);
    assert!(
        warnings_with(&envelope, "orphaned_dependency").is_empty(),
        "{}",
        r.context()
    );
    let r = home.run(&["remove", "bigwigs"]);
    let envelope = r.expect(0);
    assert_eq!(
        warnings_with(&envelope, "still_required"),
        ["On retail, BigWigs is required by Other, which will not load without it"],
        "{}",
        r.context()
    );
}

/// On Classic Era, a dependency the catalog declares only for mainline (as RaiderIO's
/// entry does) is tried after Tukui and WoWInterface: its TOCs decide. Loading there it
/// installs; not loading, the requester warns `not_shipped` and the run is not partial.
#[test]
fn an_entry_not_declaring_the_client_is_tried_last_and_its_tocs_decide() {
    for (rio_interface, loads) in [("11509", true), ("120105", false)] {
        let home = Home::new(&["wow_classic_era"]);
        let needs = [common::graph::Folder {
            name: "NeedsRio",
            interface: "11509",
            deps: &["RaiderIO"],
            optional: &[],
        }];
        let rio = [common::graph::Folder {
            name: "RaiderIO",
            interface: rio_interface,
            deps: &[],
            optional: &[],
        }];
        release_graph(
            &home.http,
            &[
                Node {
                    base: "https://github.com/o/needsrio",
                    tag: "v1",
                    folders: &needs,
                },
                Node {
                    base: "https://github.com/o/rio",
                    tag: "v1",
                    folders: &rio,
                },
            ],
        );
        let dir = home.env.config().join("omaddons/catalog.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("50-deps.toml"),
            "schema_version = \"1.0\"\n\n[[addon]]\nname = \"RaiderIO\"\nref = \"gh:o/rio\"\nfamilies = [\"mainline\"]\nfolders = [\"RaiderIO\"]\n",
        )
        .unwrap();
        let r = home.run(&["install", "gh:o/needsrio", "-t", "era"]);
        let envelope = r.expect(0);
        let on_era = |name: &str| {
            common::state::record(&home, "era")
                .addons
                .values()
                .any(|a| a.name.eq_ignore_ascii_case(name))
        };
        assert_eq!(envelope["partial"], false, "{}", r.context());
        assert!(on_era("NeedsRio"), "{}", r.context());
        assert_eq!(on_era("RaiderIO"), loads, "{}", r.context());
        let missing = warnings_with(&envelope, "missing_dependency");
        if loads {
            assert!(missing.is_empty(), "{}", r.context());
        } else {
            let item = &items(&envelope)[0];
            assert_eq!(
                (
                    &item["unresolved"][0]["folder"],
                    &item["unresolved"][0]["reason"],
                    &item["unresolved"][0]["candidates"]
                ),
                (
                    &json!("RaiderIO"),
                    &json!("not_shipped"),
                    &json!(["gh:o/rio"])
                ),
                "{}",
                r.context()
            );
            assert_eq!(missing.len(), 1, "{}", r.context());
        }
    }
}
