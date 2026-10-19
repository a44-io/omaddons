//! `omaddons adopt` end to end. Report mode: folders unpacked into AddOns with no record
//! are identified (catalog entries, TOC ids, the WoWInterface index) and, unless
//! `--no-verify`, checked against their newest release, file by file; request counts
//! come from `net.log`, and nothing but the cache, `net.log` and in-flight locks may
//! change. `--apply`, named folders and `--ref`: records-only `adopt` transactions that
//! leave every folder as it was, undone by `undo`.

mod common;

use std::collections::BTreeMap;
use std::fs;

use common::addons::{toc, unpack};
use common::home::{Home, Run};
use common::netlog::net_log;
use common::state::{files_index, history, record};
use common::trees::{CustomRoot, tree_hash};
use common::wowi::{FileEntry, wowi_filelist};
use omaddons::model::{TxKind, VersionConfidence};
use omaddons::state::history::FolderOp;
use serde_json::{Value, json};
use support::{BAR, FOO, bar, catalog, foo, group, groups, items, release, state_files};

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::path::Path;

    use serde_json::Value;

    use super::common::home::{Home, Run};
    use super::common::http::{Asset, gh_release};

    pub const FOO: &str = "https://github.com/o/foo";
    pub const BAR: &str = "https://github.com/o/bar";

    /// Bar `tag`: one folder, its TOC naming the version, for retail.
    pub fn bar(tag: &str) -> Vec<(String, String)> {
        vec![
            (
                "Bar/Bar.toc".into(),
                super::toc("120105", &["## Title: Bar", &format!("## Version: {tag}")]),
            ),
            ("Bar/bar.lua".into(), format!("-- bar {tag}\n")),
        ]
    }

    /// `(target, request, status)` of every item, sorted.
    pub fn items(envelope: &Value) -> Vec<(String, String, String)> {
        let mut out: Vec<(String, String, String)> = envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|t| {
                t["items"].as_array().unwrap().iter().map(move |i| {
                    (
                        t["target"]["id"].as_str().unwrap().to_owned(),
                        i["request"].as_str().unwrap().to_owned(),
                        i["status"].as_str().unwrap().to_owned(),
                    )
                })
            })
            .collect();
        out.sort();
        out
    }

    /// Foo `tag`: `Foo` (its TOC naming the version) and `Foo_Options`, for retail.
    pub fn foo(tag: &str) -> Vec<(String, String)> {
        vec![
            (
                "Foo/Foo.toc".into(),
                super::toc("120105", &["## Title: Foo", &format!("## Version: {tag}")]),
            ),
            ("Foo/core.lua".into(), format!("-- foo {tag}\n")),
            (
                "Foo_Options/Foo_Options.toc".into(),
                super::toc("120105", &["## Dependencies: Foo"]),
            ),
        ]
    }

    /// Serves `files` as release `tag` of `base`, one zip with `flavors`.
    pub fn release(
        scenario: &Path,
        base: &str,
        tag: &str,
        files: &[(String, String)],
        flavors: &[(&str, u32)],
    ) {
        let entries: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();
        let assets: [Asset<'_>; 1] = [("Foo.zip", flavors, entries)];
        gh_release(scenario, base, tag, &assets);
    }

    /// A user catalog file: `(name, ref, folders)`, every family.
    pub fn catalog(home: &Home, entries: &[(&str, &str, &[&str])]) {
        let mut text = String::from("schema_version = \"1.0\"\n");
        for (name, r#ref, folders) in entries {
            let folders: Vec<String> = folders.iter().map(|f| format!("{f:?}")).collect();
            text.push_str(&format!(
                "\n[[addon]]\nname = {name:?}\nref = {ref:?}\nfolders = [{}]\n",
                folders.join(", ")
            ));
        }
        let dir = home.env.config().join("omaddons/catalog.d");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("50-adopt.toml"), text).unwrap();
    }

    /// The first target's groups.
    pub fn groups(envelope: &Value) -> Vec<Value> {
        envelope["data"]["targets"][0]["groups"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// The first target's group named `name`.
    pub fn group(envelope: &Value, name: &str) -> Value {
        let found = groups(envelope).into_iter().find(|g| g["name"] == name);
        assert!(found.is_some(), "no group {name}: {envelope:#}");
        found.unwrap()
    }

    /// Every file under `<state>/omaddons` but `net.log` and in-flight locks.
    pub fn state_files(home: &Home) -> BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            let Ok(listing) = std::fs::read_dir(dir) else {
                return;
            };
            for item in listing {
                let path = item.unwrap().path();
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                if rel.starts_with("net.log") {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    out.insert(rel, std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&home.state(), &home.state(), &mut out);
        out
    }

    /// `(phase, current)` of a run's progress events.
    pub fn progress(r: &Run) -> Vec<(String, u64)> {
        r.stderr
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| e["event"] == "progress")
            .map(|e| {
                (
                    e["phase"].as_str().unwrap_or_default().to_owned(),
                    e["current"].as_u64().unwrap_or_default(),
                )
            })
            .collect()
    }
}

fn borrowed(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect()
}

const RETAIL: &[(&str, u32)] = &[("mainline", 120_105)];

/// Retail with Foo v2 released and listed in the catalog.
fn foo_home() -> Home {
    let home = Home::new(&["wow"]);
    catalog(&home, &[("Foo", "gh:o/foo", &["Foo", "Foo_Options"])]);
    release(&home.http, FOO, "v2", &foo("v2"), RETAIL);
    home
}

fn asset_requests(home: &Home) -> usize {
    net_log(home).count_purpose("asset")
}

#[test]
fn the_release_exactly_is_exact_and_an_older_build_is_identified() {
    let home = foo_home();
    unpack(&home.addons("_retail_"), &borrowed(&foo("v2")));
    let r = home.run(&["adopt"]);
    let envelope = r.expect(0);
    let g = group(&envelope, "Foo");
    assert_eq!(
        (
            &g["tier"],
            &g["ref"],
            &g["version"],
            &g["version_confidence"],
            &g["reason"]
        ),
        (
            &json!("exact"),
            &json!("gh:o/foo"),
            &json!("v2"),
            &json!("release"),
            &Value::Null
        ),
        "{}",
        r.context()
    );
    assert_eq!(g["release_id"], "gh:o/foo@v2");
    assert_eq!(g["folders"], json!(["Foo", "Foo_Options"]));
    assert_eq!(envelope["data"]["targets"][0]["unmanaged"], 2);
    assert_eq!(envelope["data"]["verify"]["groups"], 1);
    assert_eq!(
        envelope["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (&c["action"], &c["argv"]))
            .collect::<Vec<_>>(),
        [(
            &json!("adopt"),
            &json!(["omaddons", "adopt", "--apply", "-t", "retail"])
        )]
    );

    let human = home.exec(&["adopt".to_owned()]);
    assert!(
        human
            .stdout
            .contains("Apply exact + identified: omaddons adopt --apply -t retail"),
        "{}",
        human.context()
    );

    // One version behind: the TOC's version, and the files differ.
    let home = foo_home();
    unpack(&home.addons("_retail_"), &borrowed(&foo("v1")));
    let envelope = home.run(&["adopt"]).expect(0);
    let g = group(&envelope, "Foo");
    assert_eq!(
        (
            &g["tier"],
            &g["version"],
            &g["version_confidence"],
            &g["reason"],
            &g["release_id"]
        ),
        (
            &json!("identified"),
            &json!("v1"),
            &json!("toc"),
            &json!("files_differ"),
            &Value::Null
        )
    );
}

#[test]
fn bigwigs_goes_to_its_catalog_ref_with_its_wowi_id_as_alternative() {
    let home = Home::new(&["wow"]);
    let t = |extra: &[&str]| toc("120105", extra);
    unpack(
        &home.addons("_retail_"),
        &[
            (
                "BigWigs/BigWigs.toc",
                &t(&["## Title: BigWigs", "## X-WoWI-ID: 5086"]),
            ),
            (
                "BigWigs_Core/BigWigs_Core.toc",
                &t(&["## Dependencies: BigWigs"]),
            ),
            (
                "BigWigs_Options/BigWigs_Options.toc",
                &t(&["## Dependencies: BigWigs"]),
            ),
            (
                "BigWigs_Plugins/BigWigs_Plugins.toc",
                &t(&["## Dependencies: BigWigs"]),
            ),
        ],
    );
    let r = home.run(&["adopt", "--no-verify"]);
    let envelope = r.expect(0);
    let g = group(&envelope, "BigWigs");
    assert_eq!(
        (
            &g["ref"],
            &g["tier"],
            &g["strength"],
            &g["evidence"],
            &g["alternatives"]
        ),
        (
            &json!("gh:BigWigsMods/BigWigs"),
            &json!("identified"),
            &json!("catalog"),
            &json!(["catalog_folder", "toc_wowi_id"]),
            &json!(["wowi:5086"])
        ),
        "{}",
        r.context()
    );
    assert_eq!(g["reason"], "not_verified");
    assert_eq!(envelope["data"]["verify"], Value::Null);
    assert_eq!(asset_requests(&home), 0);

    // Applied, it is recorded under the catalog's ref, never the TOC's WoWInterface id.
    let r = home.run(&["adopt", "--apply", "--no-verify"]);
    r.expect(0);
    let bigwigs = &record(&home, "retail").addons["bigwigs"];
    assert_eq!(
        bigwigs.r#ref.to_string(),
        "gh:BigWigsMods/BigWigs",
        "{}",
        r.context()
    );
    let folders: Vec<&str> = bigwigs.folders.iter().map(|f| f.as_str()).collect();
    assert_eq!(
        folders,
        [
            "BigWigs",
            "BigWigs_Core",
            "BigWigs_Options",
            "BigWigs_Plugins"
        ]
    );
}

#[test]
fn an_uncorroborated_uidir_match_is_a_guess_and_a_curse_id_alone_cf_only() {
    let home = Home::new(&["wow"]);
    wowi_filelist(
        &home.http,
        &[FileEntry::new(100, "Bagnon Bags", &["Bagnon", "__MACOSX"])],
    );
    unpack(
        &home.addons("_retail_"),
        &[
            (
                "Bagnon/Bagnon.toc",
                &toc("120105", &["## Title: Other", "## Author: Nobody"]),
            ),
            (
                "TSM/TSM.toc",
                &toc("120105", &["## X-Curse-Project-ID: 12345"]),
            ),
        ],
    );
    let r = home.run(&["adopt", "--no-verify"]);
    let envelope = r.expect(0);
    let bagnon = group(&envelope, "Bagnon Bags");
    assert_eq!(
        (&bagnon["tier"], &bagnon["ref"], &bagnon["reason"]),
        (&json!("guess"), &json!("wowi:100"), &Value::Null),
        "{}",
        r.context()
    );
    let tsm = group(&envelope, "TSM");
    assert_eq!(
        (&tsm["tier"], &tsm["ref"], &tsm["evidence"]),
        (&json!("cf_only"), &Value::Null, &json!(["toc_curse_id"]))
    );
    let human = home.exec(&["adopt".to_owned(), "--no-verify".to_owned()]);
    assert!(
        human
            .stdout
            .contains("— CurseForge arrives in M5; pass --ref"),
        "{}",
        human.context()
    );
    // Only a guess: nothing for --apply, so no hint to run it.
    assert!(!human.stdout.contains("Apply exact + identified"));
}

#[test]
fn a_retail_only_module_on_classic_is_unloaded_and_still_exact() {
    let home = Home::new(&["wow_classic"]);
    catalog(&home, &[("Foo", "gh:o/foo", &["Foo", "Foo_Retail"])]);
    let files = vec![
        (
            "Foo/Foo.toc".to_owned(),
            toc("50504, 120105", &["## Title: Foo", "## Version: v3"]),
        ),
        ("Foo/core.lua".to_owned(), "-- foo\n".to_owned()),
        (
            "Foo_Retail/Foo_Retail.toc".to_owned(),
            toc("120105", &["## Dependencies: Foo"]),
        ),
    ];
    release(
        &home.http,
        FOO,
        "v3",
        &files,
        &[("mists", 50_504), ("mainline", 120_105)],
    );
    unpack(&home.addons("_classic_"), &borrowed(&files));
    let r = home.run(&["adopt"]);
    let envelope = r.expect(0);
    let g = group(&envelope, "Foo");
    assert_eq!(
        (&g["tier"], &g["unloaded_folders"], &g["folders"]),
        (
            &json!("exact"),
            &json!(["Foo_Retail"]),
            &json!(["Foo", "Foo_Retail"])
        ),
        "{}",
        r.context()
    );
}

#[test]
fn an_inner_symlink_fails_its_folder_and_a_managed_refs_leftover_is_already_managed() {
    let home = foo_home();
    let addons = home.addons("_retail_");
    unpack(&addons, &borrowed(&foo("v2")));
    std::os::unix::fs::symlink("/etc/hostname", addons.join("Foo/link.lua")).unwrap();
    let r = home.run(&["adopt"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["targets"][0]["failed"],
        json!([{"folder": "Foo", "reason": "unindexable"}]),
        "{}",
        r.context()
    );
    assert_eq!(group(&envelope, "Foo")["reason"], "not_verified");
    assert_eq!(asset_requests(&home), 0, "nothing to check");

    // Foo installed by omaddons; an old folder its catalog entry lists is a leftover.
    let home = Home::new(&["wow"]);
    catalog(
        &home,
        &[("Foo", "gh:o/foo", &["Foo", "Foo_Options", "Foo_Old"])],
    );
    release(&home.http, FOO, "v2", &foo("v2"), RETAIL);
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    unpack(
        &home.addons("_retail_"),
        &[(
            "Foo_Old/Foo_Old.toc",
            &toc("120105", &["## Dependencies: Foo"]),
        )],
    );
    let envelope = home.run(&["adopt", "--no-verify"]).expect(0);
    assert_eq!(
        envelope["data"]["targets"][0]["failed"],
        json!([{"folder": "Foo_Old", "reason": "already_managed"}])
    );
    assert_eq!(groups(&envelope), Vec::<Value>::new());
}

#[test]
fn no_verify_downloads_nothing_and_a_second_run_downloads_nothing_either() {
    let home = foo_home();
    unpack(&home.addons("_retail_"), &borrowed(&foo("v2")));
    let envelope = home.run(&["adopt", "--no-verify"]).expect(0);
    assert_eq!(envelope["data"]["verify"], Value::Null);
    assert_eq!(group(&envelope, "Foo")["reason"], "not_verified");
    assert_eq!(asset_requests(&home), 0);

    home.run(&["adopt"]).expect(0);
    assert_eq!(asset_requests(&home), 1);
    let releases = net_log(&home).count_purpose("release-latest");
    let r = home.run(&["adopt"]);
    let envelope = r.expect(0);
    assert_eq!(group(&envelope, "Foo")["tier"], "exact", "{}", r.context());
    assert_eq!(asset_requests(&home), 1, "the archive is cached");
    assert_eq!(
        net_log(&home).count_purpose("release-latest"),
        releases,
        "the release is remembered"
    );
    assert_eq!(envelope["data"]["verify"]["downloads"], 0);
}

#[test]
fn offline_is_exact_only_from_the_memo_and_the_blob_cache() {
    // Nothing remembered: not checked, no request.
    let home = foo_home();
    unpack(&home.addons("_retail_"), &borrowed(&foo("v2")));
    let envelope = home.run(&["adopt", "--offline"]).expect(0);
    assert_eq!(group(&envelope, "Foo")["reason"], "not_verified_offline");
    assert!(net_log(&home).entries.iter().all(|e| e.purpose != "asset"));

    // Checked once online: offline, the memo and the cached archive answer.
    home.run(&["adopt"]).expect(0);
    let before = net_log(&home).len();
    let r = home.run(&["adopt", "--offline"]);
    let envelope = r.expect(0);
    assert_eq!(group(&envelope, "Foo")["tier"], "exact", "{}", r.context());
    assert_eq!(net_log(&home).len(), before);
}

#[test]
fn every_vanilla_client_is_reported_and_a_disabled_source_is_skipped() {
    let home = Home::new(&["wow", "wow_classic_era"]);
    CustomRoot::at(home.env.home().join("Games/era-custom"))
        .version(1, 15, 9, 60_003)
        .build();
    home.run(&["targets", "rename", "vanilla", "custom"])
        .expect(0);
    home.run(&["config", "set", "sources.wowi.enabled", "false"])
        .expect(0);
    let r = home.run(&["adopt", "-t", "vanilla", "--no-verify"]);
    let envelope = r.expect(0);
    let ids: Vec<&str> = envelope["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["target"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["custom", "era"], "{}", r.context());
    assert!(
        envelope["data"]["sources_skipped"]
            .as_array()
            .unwrap()
            .contains(&json!({"source": "wowi", "reason": "disabled"})),
        "{}",
        r.context()
    );
}

#[test]
fn adopt_writes_nothing_but_the_cache_and_repeats_identically() {
    let home = foo_home();
    let addons = home.addons("_retail_");
    unpack(&addons, &borrowed(&foo("v1")));
    home.run(&["status"]).expect(0);
    let (state, tree) = (state_files(&home), tree_hash(&home.client("_retail_")));

    let first = home.run(&["adopt", "--progress", "json"]);
    first.expect(0);
    assert_eq!(state_files(&home), state, "no state written");
    assert_eq!(
        tree_hash(&home.client("_retail_")),
        tree,
        "AddOns unchanged"
    );
    for dir in [home.state().join("inflight"), home.planning_locks()] {
        assert!(
            fs::read_dir(&dir).map(|l| l.count()).unwrap_or(0) == 0,
            "no in-flight lock left in {}",
            dir.display()
        );
    }
    // Progress never goes back within a phase, and the download is announced first.
    let mut seen: BTreeMap<String, u64> = BTreeMap::new();
    for (phase, current) in support::progress(&first) {
        let last = seen.entry(phase.clone()).or_insert(0);
        assert!(current >= *last, "{phase} went back: {}", first.context());
        *last = current;
    }
    assert!(
        support::progress(&first)
            .iter()
            .any(|(p, c)| p == "download" && *c == 0)
    );

    let strip = |r: &Run| {
        let mut v = r.envelope();
        v["meta"] = Value::Null;
        v
    };
    let (a, b) = (home.run(&["adopt"]), home.run(&["adopt"]));
    assert_eq!(strip(&a), strip(&b), "repeat runs are identical");
}

/// The folder names of `addon`'s record on retail.
fn recorded_folders(home: &Home, addon: &str) -> Vec<String> {
    record(home, "retail").addons[addon]
        .folders
        .iter()
        .map(|f| f.as_str().to_owned())
        .collect()
}

fn row(target: &str, request: &str, status: &str) -> (String, String, String) {
    (target.to_owned(), request.to_owned(), status.to_owned())
}

#[test]
fn apply_records_exact_and_identified_groups_and_changes_no_folder() {
    let home = foo_home();
    catalog(
        &home,
        &[
            ("Foo", "gh:o/foo", &["Foo", "Foo_Options"]),
            ("Bar", "gh:o/bar", &["Bar"]),
        ],
    );
    release(&home.http, BAR, "v2", &bar("v2"), RETAIL);
    wowi_filelist(
        &home.http,
        &[FileEntry::new(100, "Bagnon Bags", &["Bagnon"])],
    );
    let addons = home.addons("_retail_");
    unpack(&addons, &borrowed(&foo("v2")));
    unpack(&addons, &borrowed(&bar("v1")));
    unpack(
        &addons,
        &[(
            "Bagnon/Bagnon.toc",
            &toc("120105", &["## Title: Other", "## Author: Nobody"]),
        )],
    );
    let tree = tree_hash(&addons);

    let r = home.run(&["adopt", "--apply"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [
            row("retail", "Bar", "adopted"),
            row("retail", "Foo", "adopted")
        ],
        "{}",
        r.context()
    );
    let t = &envelope["data"]["targets"][0];
    assert_eq!(
        (&t["kind"], &t["reload_required"]),
        (&json!("adopt"), &json!("none"))
    );
    assert!(t["tx"].is_string());
    let rec = record(&home, "retail");
    let foo = &rec.addons["foo"];
    assert_eq!(
        (
            foo.version.as_str(),
            foo.version_confidence,
            foo.release_id.as_deref()
        ),
        ("v2", VersionConfidence::Release, Some("gh:o/foo@v2"))
    );
    assert!(foo.blob_sha256.is_some());
    assert_eq!(recorded_folders(&home, "foo"), ["Foo", "Foo_Options"]);
    let bar = &rec.addons["bar"];
    assert_eq!(
        (
            bar.version.as_str(),
            bar.version_confidence,
            bar.release_id.as_deref(),
            bar.blob_sha256.as_deref()
        ),
        ("v1", VersionConfidence::Toc, None, None),
        "identified: the TOC's version"
    );
    assert_eq!(rec.addons.len(), 2, "a guess is never applied");
    assert_eq!(tree_hash(&addons), tree, "no folder changed");
    let entry = history(&home).pop().unwrap();
    assert_eq!(entry.kind, TxKind::Adopt);
    assert!(entry.ops.iter().all(|o| o.op == FolderOp::Record));
    let mut folders: Vec<&str> = entry.ops.iter().map(|o| o.folder.as_str()).collect();
    folders.sort_unstable();
    assert_eq!(folders, ["Bar", "Foo", "Foo_Options"]);
    let indexed: Vec<String> = files_index(&home, "retail").folders.into_keys().collect();
    assert_eq!(indexed, ["bar", "foo", "foo_options"]);

    // Undo forgets the records and their index entries; the folders stay as they are.
    home.run(&["undo"]).expect(0);
    assert!(record(&home, "retail").addons.is_empty());
    assert!(files_index(&home, "retail").folders.is_empty());
    assert_eq!(tree_hash(&addons), tree);
}

#[test]
fn one_bare_undo_reverts_every_targets_adoption() {
    let home = Home::new(&["wow", "wow_classic", "wow_classic_era"]);
    catalog(&home, &[("Foo", "gh:o/foo", &["Foo"])]);
    let toc_text = toc("120105, 50504, 11507", &["## Version: v1"]);
    let clients = ["_retail_", "_classic_", "_classic_era_"];
    for client in clients {
        unpack(
            &home.addons(client),
            &[("Foo/Foo.toc", &toc_text), ("Foo/core.lua", "-- foo\n")],
        );
    }
    let trees: Vec<_> = clients.iter().map(|c| tree_hash(&home.addons(c))).collect();

    let r = home.run(&["adopt", "--apply", "--no-verify"]);
    let envelope = r.expect(0);
    let targets = envelope["data"]["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 3, "{}", r.context());
    assert!(targets.iter().all(|t| t["tx"].is_string()));
    assert_eq!(envelope["commands"][0]["action"], "undo_run");
    let ids: Vec<String> = targets
        .iter()
        .map(|t| t["target"]["id"].as_str().unwrap().to_owned())
        .collect();

    home.run(&["undo"]).expect(0);
    for id in &ids {
        assert!(record(&home, id).addons.is_empty(), "{id} is forgotten");
    }
    let after: Vec<_> = clients.iter().map(|c| tree_hash(&home.addons(c))).collect();
    assert_eq!(after, trees);
}

#[test]
fn an_adoption_commits_while_a_legacy_client_runs() {
    let mut home = Home::new(&["wow"]);
    let legacy =
        home.with_custom(CustomRoot::at(home.env.root().join("wow-335")).version(3, 3, 5, 12_340));
    let targets = home.run(&["targets"]).expect(0);
    let wotlk = targets["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["family"] != "mainline")
        .and_then(|t| t["id"].as_str())
        .unwrap()
        .to_owned();
    catalog(&home, &[("Leg", "gh:o/leg", &["Leg"])]);
    unpack(
        &legacy.join("Interface/AddOns"),
        &[("Leg/Leg.toc", &toc("30300", &["## Version: 1.0"]))],
    );
    let exe = legacy.join("Wow.exe").display().to_string();
    let proc = common::procfs::FakeProc::new(home.env.root().join("proc")).process(
        9,
        true,
        "Wow.exe",
        &["/usr/bin/wine", &exe],
        &[],
    );
    home.proc = Some(proc.root().to_path_buf());

    let r = home.run(&["adopt", "--apply", "--no-verify", "-t", &wotlk]);
    let envelope = r.expect(0);
    let t = &envelope["data"]["targets"][0];
    assert!(t["tx"].is_string(), "{}", r.context());
    assert_eq!(t["reload_required"], "none");
    // The client does run: changing a folder is refused.
    let r = home.run(&["remove", "Leg", "-t", &wotlk]);
    assert_eq!(r.expect(4)["error"]["code"], "game_running");
}

#[test]
fn an_adopted_older_build_updates_to_the_newest_release() {
    let home = foo_home();
    release(&home.http, FOO, "v1", &foo("v1"), RETAIL);
    release(&home.http, FOO, "v2", &foo("v2"), RETAIL);
    unpack(&home.addons("_retail_"), &borrowed(&foo("v1")));
    home.run(&["adopt", "--apply"]).expect(0);
    assert_eq!(
        record(&home, "retail").addons["foo"].version_confidence,
        VersionConfidence::Toc
    );

    let r = home.run(&["outdated"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["targets"][0]["items"][0];
    assert_eq!(
        (&item["status"], &item["latest"]["version"]),
        (&json!("update_available"), &json!("v2")),
        "{}",
        r.context()
    );
    let r = home.run(&["update"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["targets"][0]["items"][0]["status"],
        "updated",
        "{}",
        r.context()
    );
    let foo = &record(&home, "retail").addons["foo"];
    assert_eq!(
        (foo.version.as_str(), foo.version_confidence),
        ("v2", VersionConfidence::Release)
    );
}

#[test]
fn an_adopted_build_newer_than_the_release_is_blocked_not_replaced() {
    // v2 is newer by tag than the v1 build on disk, but built for an older interface.
    let build = |interface: &str, version: &str| {
        vec![
            (
                "Foo/Foo.toc".to_owned(),
                toc(interface, &[&format!("## Version: {version}")]),
            ),
            ("Foo/core.lua".to_owned(), format!("-- foo {version}\n")),
        ]
    };
    let setup = || {
        let home = Home::new(&["wow"]);
        catalog(&home, &[("Foo", "gh:o/foo", &["Foo"])]);
        release(&home.http, FOO, "v2", &build("110207", "v2"), &[]);
        unpack(&home.addons("_retail_"), &borrowed(&build("120105", "v1")));
        home
    };

    // Checked while adopting: outdated knows it is older, and update leaves it alone.
    let home = setup();
    let tree = tree_hash(&home.addons("_retail_"));
    home.run(&["adopt", "--apply"]).expect(0);
    let r = home.run(&["outdated"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["targets"][0]["items"][0];
    assert_eq!(
        (&item["status"], &item["blocked_reason"]),
        (&json!("blocked"), &json!("older_than_installed")),
        "{}",
        r.context()
    );
    let r = home.run(&["update"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["targets"][0]["items"][0];
    assert_eq!(
        (&item["status"], &item["skip_reason"]),
        (&json!("skipped"), &json!("blocked")),
        "{}",
        r.context()
    );
    assert_eq!(tree_hash(&home.addons("_retail_")), tree);

    // Not checked: update downloads it, its guard refuses it, and the fact is kept.
    let home = setup();
    home.run(&["adopt", "--apply", "--no-verify"]).expect(0);
    let r = home.run(&["update"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["targets"][0]["items"][0];
    assert_eq!(
        (&item["status"], &item["skip_reason"]),
        (&json!("skipped"), &json!("blocked")),
        "{}",
        r.context()
    );
    assert_eq!(tree_hash(&home.addons("_retail_")), tree);
    let (memo, _) = omaddons::state::memo::load(&common::http::paths(&home.env));
    assert!(
        memo.candidates
            .iter()
            .any(|f| f.release_id == "gh:o/foo@v2")
    );
}

#[test]
fn a_named_guess_is_adopted_unless_it_has_rivals_and_each_report_command_runs() {
    let home = Home::new(&["wow"]);
    wowi_filelist(
        &home.http,
        &[
            FileEntry::new(100, "Bagnon Bags", &["Bagnon"]),
            FileEntry::new(200, "Bagnon Classic", &["Bagnon"]),
            FileEntry::new(300, "Gizmo Things", &["Gizmo"]),
        ],
    );
    let other = toc("120105", &["## Title: Other", "## Author: Nobody"]);
    unpack(
        &home.addons("_retail_"),
        &[("Bagnon/Bagnon.toc", &other), ("Gizmo/Gizmo.toc", &other)],
    );
    home.run(&["status"]).expect(0);
    let state = state_files(&home);
    let r = home.run(&["adopt", "--no-verify"]);
    let report = r.expect(0);
    let commands: Vec<(String, Vec<String>)> = report["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["action"].as_str().unwrap().to_owned(),
                serde_json::from_value(c["argv"].clone()).unwrap(),
            )
        })
        .collect();
    let actions: Vec<&str> = commands.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(
        actions,
        ["adopt_with_ref", "adopt_with_ref", "adopt_folder"],
        "{}",
        r.context()
    );
    assert_eq!(
        commands[2].1,
        ["omaddons", "adopt", "Gizmo", "-t", "retail"]
    );
    let human = home.exec(&["adopt".to_owned(), "--no-verify".to_owned()]);
    assert!(
        human
            .stdout
            .contains("— not applied; choose with --ref (or wowi:"),
        "{}",
        human.context()
    );
    let refs: Vec<&str> = commands[..2]
        .iter()
        .map(|(_, argv)| argv[4].as_str())
        .collect();
    let mut sorted = refs.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, ["wowi:100", "wowi:200"]);

    // --apply never takes a guess.
    let envelope = home.run(&["adopt", "--apply", "--no-verify"]).expect(0);
    assert_eq!(envelope["data"]["summary"]["changed"], 0);
    assert_eq!(state_files(&home), state, "nothing recorded");

    // Named with rivals: ref_ambiguous, each rival a fix.
    let r = home.run(&["adopt", "Bagnon", "--no-verify"]);
    let envelope = r.expect(3);
    let error = &envelope["error"];
    assert_eq!(error["code"], "ref_ambiguous", "{}", r.context());
    assert_eq!(error["fix_argv"], json!(commands[0].1));
    assert_eq!(error["alternatives"][0]["argv"], json!(commands[1].1));
    assert_eq!(state_files(&home), state, "nothing recorded");

    // Each report command runs: the first adopts Bagnon, the second re-points it, the
    // third adopts Gizmo.
    let mut statuses = Vec::new();
    for (_, argv) in &commands {
        let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        let r = home.run(&args);
        let envelope = r.expect(0);
        statuses.push(envelope["data"]["targets"][0]["items"][0]["status"].clone());
    }
    assert_eq!(
        statuses,
        [json!("adopted"), json!("edited"), json!("adopted")]
    );
    let rec = record(&home, "retail");
    assert_eq!(rec.addons.len(), 2);
    assert_eq!(rec.addons["gizmo things"].r#ref.to_string(), "wowi:300");
}

#[test]
fn ref_reads_catalog_names_and_re_points_a_managed_addon() {
    let home = Home::new(&["wow"]);
    catalog(&home, &[("Foo", "gh:o/foo", &["Foo", "Foo_Options"])]);
    unpack(
        &home.addons("_retail_"),
        &[
            ("MyFoo/MyFoo.toc", &toc("120105", &["## Version: m1"])),
            ("Zed/Zed.toc", &toc("120105", &["## Version: z1"])),
        ],
    );
    // A catalog name, read as install reads it.
    let argv: Vec<String> = ["adopt", "MyFoo", "--ref", "foo", "--no-verify"]
        .map(str::to_owned)
        .to_vec();
    let human = home.exec(&argv);
    assert_eq!(human.code, 0, "{}", human.context());
    let first = human.stdout.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("retail: adopted 1 addon (1 folder), tx ")
            && first.ends_with("… — no folder was changed"),
        "{}",
        human.context()
    );
    let foo = &record(&home, "retail").addons["foo"];
    assert_eq!(
        (
            foo.r#ref.to_string(),
            foo.version.as_str(),
            foo.version_confidence
        ),
        ("gh:o/foo".to_owned(), "m1", VersionConfidence::Toc)
    );
    assert_eq!(recorded_folders(&home, "foo"), ["MyFoo"]);

    // Any ref for a folder nothing identifies: identified, the TOC's version.
    let r = home.run(&["adopt", "Zed", "--ref", "gh:o/x", "--no-verify"]);
    r.expect(0);
    let zed = &record(&home, "retail").addons["zed"];
    assert_eq!(
        (zed.r#ref.to_string(), zed.version.as_str()),
        ("gh:o/x".to_owned(), "z1")
    );

    // On a managed folder, the record is re-pointed: an adopt transaction, undoable.
    let r = home.run(&["adopt", "Zed", "--ref", "gh:o/zed"]);
    let envelope = r.expect(0);
    let t = &envelope["data"]["targets"][0];
    assert_eq!(
        (&t["kind"], &t["items"][0]["status"]),
        (&json!("adopt"), &json!("edited")),
        "{}",
        r.context()
    );
    let zed = &record(&home, "retail").addons["zed"];
    assert_eq!(
        (
            zed.r#ref.to_string(),
            zed.release_id.as_deref(),
            zed.version_confidence
        ),
        ("gh:o/zed".to_owned(), None, VersionConfidence::Toc)
    );
    // The same ref again, or the folder alone, changes nothing.
    for args in [
        &["adopt", "Zed", "--ref", "gh:o/zed", "-v"][..],
        &["adopt", "Zed", "-v"],
    ] {
        let envelope = home.run(args).expect(0);
        assert_eq!(items(&envelope), [row("retail", "Zed", "unchanged")]);
    }
    // A ref another addon records: one ref, one addon.
    let r = home.run(&["adopt", "Zed", "--ref", "gh:o/foo"]);
    let envelope = r.expect(4);
    assert_eq!(
        (
            &envelope["error"]["code"],
            &envelope["error"]["details"]["owner"]
        ),
        (&json!("ownership_conflict"), &json!("Foo")),
        "{}",
        r.context()
    );
    home.run(&["undo"]).expect(0);
    assert_eq!(
        record(&home, "retail").addons["zed"].r#ref.to_string(),
        "gh:o/x",
        "undo restores the ref"
    );
}

#[test]
fn folders_that_cannot_be_adopted_fail_and_write_nothing() {
    let home = Home::new(&["wow"]);
    unpack(
        &home.addons("_retail_"),
        &[
            (
                "TSM/TSM.toc",
                &toc("120105", &["## X-Curse-Project-ID: 12345"]),
            ),
            ("Mystery/Mystery.toc", &toc("120105", &[])),
            ("Blizzard_Thing/Blizzard_Thing.toc", &toc("120105", &[])),
        ],
    );
    home.run(&["status"]).expect(0);
    let state = state_files(&home);

    let r = home.run(&["adopt", "TSM", "--no-verify"]);
    let envelope = r.expect(1);
    assert_eq!(
        (
            &envelope["error"]["code"],
            &envelope["error"]["fix"],
            &envelope["error"]["fix_argv"]
        ),
        (
            &json!("source_disabled"),
            &json!("omaddons adopt --ref <ref> TSM -t retail"),
            &Value::Null
        ),
        "{}",
        r.context()
    );
    let envelope = home.run(&["adopt", "Mystery", "--no-verify"]).expect(3);
    assert_eq!(
        (&envelope["error"]["code"], &envelope["error"]["fix"]),
        (
            &json!("not_found"),
            &json!("omaddons adopt --ref <ref> Mystery -t retail")
        )
    );
    let envelope = home.run(&["adopt", "Blizzard_Thing"]).expect(4);
    assert_eq!(
        (&envelope["error"]["code"], &envelope["error"]["fix_argv"]),
        (
            &json!("protected_folder"),
            &json!(["omaddons", "list", "-t", "retail", "--protected"])
        )
    );
    let envelope = home.run(&["adopt", "Nowhere"]).expect(3);
    assert_eq!(
        (&envelope["error"]["code"], &envelope["error"]["fix_argv"]),
        (
            &json!("not_found"),
            &json!(["omaddons", "list", "-t", "retail", "--unmanaged"])
        )
    );
    // One of several fails as an item: the run is partial.
    let r = home.run(&["adopt", "Mystery", "Blizzard_Thing", "--no-verify"]);
    assert_eq!(
        r.expect(3)["error"]["details"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(state_files(&home), state, "nothing written");
}

#[test]
fn a_taken_name_is_recorded_with_its_folder_and_a_warning() {
    let home = foo_home();
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    // Another project the catalog also calls Foo.
    catalog(
        &home,
        &[
            ("Foo", "gh:o/foo", &["Foo", "Foo_Options"]),
            ("Foo", "gh:o/foo-two", &["FooTwo"]),
        ],
    );
    unpack(
        &home.addons("_retail_"),
        &[("FooTwo/FooTwo.toc", &toc("120105", &["## Version: 2"]))],
    );
    let r = home.run(&["adopt", "FooTwo", "--no-verify"]);
    let envelope = r.expect(0);
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "adopt_name_changed")
        .cloned();
    assert!(warning.is_some(), "{}", r.context());
    assert_eq!(
        warning.unwrap()["fix_argv"],
        json!(["omaddons", "list", "-t", "retail"])
    );
    let rec = record(&home, "retail");
    assert_eq!(rec.addons["foo (footwo)"].name, "Foo (FooTwo)");
    assert_eq!(rec.addons["foo (footwo)"].r#ref.to_string(), "gh:o/foo-two");
}

#[test]
fn a_file_changed_between_planning_and_the_lock_is_plan_stale() {
    let home = foo_home();
    let addons = home.addons("_retail_");
    unpack(&addons, &borrowed(&foo("v1")));
    home.run(&["status"]).expect(0);
    let argv = ["adopt", "--apply", "--no-verify"];
    let pause = common::pause::Pause::new(home.env.root());
    let child = home.spawn(
        &argv,
        &[
            ("OMADDONS_FAILPOINTS", "engine.plan.done=pause"),
            pause.env(),
        ],
    );
    // Planned, so the folder is hashed; the edit lands before the lock.
    pause.reached("engine.plan.done");
    fs::write(
        addons.join("Foo/core.lua"),
        "-- edited after indexing, longer\n",
    )
    .unwrap();
    pause.resume("engine.plan.done");
    let r = common::home::wait(child, &argv);
    let envelope = r.expect(1);
    assert_eq!(
        (&envelope["error"]["code"], &envelope["error"]["retryable"]),
        (&json!("plan_stale"), &json!(true)),
        "{}",
        r.context()
    );
    assert!(
        record(&home, "retail").addons.is_empty(),
        "nothing recorded"
    );
}

#[test]
fn a_dry_run_shows_the_plan_and_commits_nothing() {
    let home = foo_home();
    unpack(&home.addons("_retail_"), &borrowed(&foo("v2")));
    home.run(&["status"]).expect(0);
    let state = state_files(&home);
    let r = home.run(&["adopt", "--apply", "--dry-run"]);
    let envelope = r.expect(0);
    let t = &envelope["data"]["targets"][0];
    assert_eq!(
        (
            &envelope["data"]["dry_run"],
            &t["tx"],
            &t["items"][0]["status"]
        ),
        (&json!(true), &Value::Null, &json!("adopted")),
        "{}",
        r.context()
    );
    assert_eq!(state_files(&home), state, "nothing committed");
}

#[test]
fn apply_with_folders_and_a_ref_without_one_folder_are_usage_errors() {
    let home = Home::new(&["wow", "wow_classic"]);
    let foo_toc = toc("120105, 50504", &["## Version: 1"]);
    for client in ["_retail_", "_classic_"] {
        unpack(&home.addons(client), &[("Foo/Foo.toc", &foo_toc)]);
    }
    let r = home.run(&["adopt", "--apply", "Foo"]);
    let envelope = r.expect(2);
    assert_eq!(
        (&envelope["error"]["code"], &envelope["error"]["fix_argv"]),
        (&json!("usage"), &json!(["omaddons", "adopt", "Foo"])),
        "{}",
        r.context()
    );
    for args in [
        &["adopt", "--ref", "gh:o/foo"][..],
        &["adopt", "Foo", "Bar", "--ref", "gh:o/foo"],
    ] {
        let envelope = home.run(args).expect(2);
        assert_eq!(envelope["error"]["code"], "usage");
    }
    // --ref lands on one client: the folder is on two.
    let r = home.run(&["adopt", "Foo", "--ref", "gh:o/foo", "--no-verify"]);
    let envelope = r.expect(3);
    let error = &envelope["error"];
    assert_eq!(error["code"], "target_ambiguous", "{}", r.context());
    let argv: Vec<String> = serde_json::from_value(error["fix_argv"].clone()).unwrap();
    assert_eq!(argv[argv.len() - 2..], ["-t", "classic"]);
    assert_eq!(error["alternatives"].as_array().unwrap().len(), 1);
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    home.run(&args).expect(0);
    assert_eq!(
        record(&home, "classic").addons["foo"].r#ref.to_string(),
        "gh:o/foo"
    );
}
