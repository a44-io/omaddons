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
use support::{RETAIL, catalog, fetches, folder, graph, items, record_of, warnings_with, wigs};

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
    let listed: Vec<&Value> = items(&envelope).iter().map(|i| &i["name"]).collect();
    assert_eq!(
        listed,
        [&json!("LittleWigs")],
        "positionals only (C5 lists dependencies)"
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
fn a_present_unmanaged_dependency_installs_nothing_and_hints_a_look() {
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
        .find(|c| c["action"] == "list_unmanaged");
    assert!(hint.is_some(), "no list_unmanaged hint\n{}", r.context());
    let hint = hint.unwrap();
    assert_eq!(
        hint["argv"],
        json!(["omaddons", "list", "-t", "retail", "--unmanaged"])
    );
    assert!(warnings_with(&envelope, "missing_dependency").is_empty());
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
            "## Dependencies [AllowLoadGameType vanilla]: ClassicOnly",
            "## Dependencies [SomeFutureCondition x]: FutureLib",
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
            && missing[0].contains("under a condition omaddons does not read"),
        "{}",
        missing[0]
    );
    assert!(
        !missing[0].contains("ClassicOnly"),
        "a vanilla-only dependency does not apply on retail"
    );
    assert!(!home.addons("_retail_").join("FutureLib").exists());
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
fn an_unresolvable_dependency_warns_with_a_text_fix() {
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
    assert!(
        warning["message"]
            .as_str()
            .unwrap()
            .contains("NowhereToBeFound")
    );
    assert_eq!(
        warning["fix"],
        "no keyless source ships NowhereToBeFound; it may only be on CurseForge or Wago, which arrive in M5"
    );
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
    // Items by name, as `update` lists them.
    assert_eq!(
        update_items(&envelope),
        [pair("BigWigs", "installed"), pair("LittleWigs", "updated")],
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
    assert_eq!(
        update_items(&envelope),
        [pair("DepB", "failed"), pair("LittleWigs", "updated")],
        "{}",
        r.context()
    );
    let failed = &envelope["data"]["targets"][0]["items"][0];
    assert_eq!(failed["ref"], "gh:o/depb");
    assert!(failed["skip_reason"].is_null(), "never skipped/blocked");
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
            cache: None,
            progress: &ProgressSink::none(),
            catalog: Some(&catalog),
            default_channel: Channel::Stable,
            downloads: None,
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
