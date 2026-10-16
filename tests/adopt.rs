//! `omaddons adopt` report mode end to end: folders unpacked into AddOns with no record
//! are identified (catalog entries, TOC ids, the WoWInterface index) and, unless
//! `--no-verify`, checked against their newest release, file by file. Request counts come
//! from `net.log`; nothing but the cache, `net.log` and in-flight locks may change.

mod common;

use std::collections::BTreeMap;
use std::fs;

use common::addons::{toc, unpack};
use common::home::{Home, Run};
use common::netlog::net_log;
use common::trees::{CustomRoot, tree_hash};
use common::wowi::{FileEntry, wowi_filelist};
use serde_json::{Value, json};
use support::{FOO, catalog, foo, group, groups, release, state_files};

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::path::Path;

    use serde_json::Value;

    use super::common::home::{Home, Run};
    use super::common::http::{Asset, gh_release};

    pub const FOO: &str = "https://github.com/o/foo";

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
                if rel.starts_with("net.log") || rel.starts_with("inflight") {
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
    assert_eq!(envelope["commands"], json!([]));

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
    assert!(
        fs::read_dir(home.state().join("inflight"))
            .map(|l| l.count())
            .unwrap_or(0)
            == 0,
        "no in-flight lock left"
    );
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

#[test]
fn folders_and_apply_are_not_report_mode() {
    let home = Home::new(&["wow"]);
    for args in [
        &["adopt", "--apply"][..],
        &["adopt", "Foo"],
        &["adopt", "--ref", "gh:o/foo"],
    ] {
        let r = home.run(args);
        let envelope = r.expect(2);
        assert_eq!(envelope["error"]["code"], "usage", "{}", r.context());
    }
}
