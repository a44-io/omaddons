//! Multi-client archives end to end. DBM and BigWigs ship one zip for every client:
//! shared folders whose TOCs list every client's interface, and retail-only boss modules
//! gated by `## AllowLoadGameType: standard`. Each client installs the folders it loads
//! and lists the rest in `folders.skipped`; skipped folders never reach
//! `Interface/AddOns`, the files index or a snapshot of folders, and the record keeps them
//! only as what the install left out (`left_out`). Releases are replayed
//! from synthetic fixtures shaped like the real releases (layouts verified 2026-10-15:
//! DBM-Core 12.1.14, BigWigs v427.6).

use crate::common;

use std::fs;
use std::path::Path;

use common::addons::toc;
use common::archives::zip_text;
use common::home::Home;
use common::http::{Asset, fixture, fixture_bytes, gh_latest, gh_release};
use common::netlog::net_log;
use common::trees::{CustomRoot, listing, tree_hash};
use serde_json::Value;
use support::{
    by_name, folder, gated, left_out, left_out_names, mentions, on_disk, release, rest_release,
    serve_boss_mods, skipped, sorted, status_folders, strings,
};

const DBM: &str = "https://github.com/DeadlyBossMods/DeadlyBossMods";
const BIGWIGS: &str = "https://github.com/BigWigsMods/BigWigs";
const WEAKAURAS: &str = "https://github.com/WeakAuras/WeakAuras2";
const FOO: &str = "https://github.com/o/foo";

/// Every client's interface, as the shared folders list them.
const EVERY: &str = "11509, 16001, 20506, 30405, 38002, 40402, 50504, 120100, 120105";
const RETAIL: &str = "120100, 120105";

/// Products, their target ids and client dirs. Retail, which loads every folder, comes
/// last, so state written before it never names a retail-only folder.
const CLIENTS: &[(&str, &str, &str)] = &[
    ("wow_classic_era", "era", "_classic_era_"),
    ("wow_anniversary", "anniversary", "_anniversary_"),
    ("wow_classic", "classic", "_classic_"),
    ("wowf", "forever", "_forever_"),
    ("wow_classic_titan", "titan", "_classic_titan_"),
    ("wow", "retail", "_retail_"),
];

const DBM_SHARED: &[&str] = &["DBM-Core", "DBM-GUI", "DBM-StatusBarTimers", "DBM-VPVEM"];
const DBM_RETAIL: &[&str] = &[
    "DBM-Brawlers",
    "DBM-Midnight",
    "DBM-Raids-Midnight",
    "DBM-Lairs-Midnight",
];
const DBM_FLAVORS: &[(&str, u32)] = &[
    ("mainline", 120_100),
    ("mainline", 120_105),
    ("classic", 11_509),
    ("bcc", 20_506),
    ("wrath", 30_405),
    ("titan", 38_002),
    ("cata", 40_402),
    ("mists", 50_504),
    ("forever", 16_001),
];

const BW_SHARED: &[&str] = &[
    "BigWigs",
    "BigWigs_Core",
    "BigWigs_Options",
    "BigWigs_Plugins",
];
const BW_RETAIL: &[&str] = &[
    "BigWigs_TheVoidspire",
    "BigWigs_TheDreamrift",
    "BigWigs_MarchOnQuelDanas",
    "BigWigs_MidnightWorld",
    "BigWigs_Sporefall",
    "BigWigs_TheVenomousAbyss",
    "BigWigs_MidnightLairs",
    "BigWigs_TheUnbindingOfKithix",
];
const BW_FLAVORS: &[(&str, u32)] = &[
    ("mainline", 120_100),
    ("mainline", 120_105),
    ("mainline", 120_007),
    ("classic", 11_508),
    ("classic", 11_509),
    ("bcc", 20_505),
    ("bcc", 20_506),
    ("wrath", 30_405),
    ("titan", 38_002),
    ("cata", 40_402),
    ("mists", 50_503),
    ("mists", 50_504),
    ("forever", 16_001),
];

#[cfg(test)]
mod support {
    use super::*;

    /// One addon folder: its TOC (`interfaces`, then `extra` lines, then its Lua file) and
    /// the Lua file.
    pub fn folder(name: &str, interfaces: &str, extra: &[&str]) -> Vec<(String, String)> {
        let lua = format!("{name}.lua");
        let mut lines: Vec<&str> = extra.to_vec();
        lines.push(&lua);
        vec![
            (format!("{name}/{name}.toc"), toc(interfaces, &lines)),
            (format!("{name}/{lua}"), format!("-- {name}\n")),
        ]
    }

    pub fn dbm_files() -> Vec<(String, String)> {
        let mut files = folder(
            "DBM-Core",
            EVERY,
            &["## Title: DBM-Core", "## Version: 12.1.14"],
        );
        // Bundled libraries carry TOCs of their own but are not addon roots.
        files.push((
            "DBM-Core/Libs/LibStub/LibStub.toc".into(),
            toc("90005", &["LibStub.lua"]),
        ));
        files.push((
            "DBM-Core/Libs/LibStub/LibStub.lua".into(),
            "-- LibStub\n".into(),
        ));
        for shared in &DBM_SHARED[1..] {
            files.extend(folder(shared, EVERY, &["## Dependencies: DBM-Core"]));
        }
        for retail in DBM_RETAIL {
            files.extend(folder(
                retail,
                RETAIL,
                &[
                    "## AllowLoadGameType: standard",
                    "## Dependencies: DBM-Core",
                    "## LoadOnDemand: 1",
                ],
            ));
        }
        files
    }

    pub fn bigwigs_files() -> Vec<(String, String)> {
        let every = "11508, 11509, 16001, 20505, 20506, 30405, 38002, 40402, 50503, 50504, 120007, 120100, 120105";
        let mut files = folder(
            "BigWigs",
            every,
            &["## Title: BigWigs", "## Version: v427.6"],
        );
        for shared in &BW_SHARED[1..] {
            files.extend(folder(shared, every, &["## RequiredDeps: BigWigs"]));
        }
        for retail in BW_RETAIL {
            files.extend(folder(
                retail,
                "120100, 120105, 120007",
                &[
                    "## AllowLoadGameType: standard",
                    "## Dependencies: BigWigs_Core",
                    "## LoadOnDemand: 1",
                ],
            ));
        }
        files
    }

    /// Serves `base`'s release `tag` without a `release.json`, so the REST fallback lists
    /// its zips (`assets`: file name, files), whose names then carry flavor hints.
    pub fn rest_release(
        scenario: &Path,
        base: &str,
        tag: &str,
        assets: &[(&str, Vec<(String, String)>)],
    ) {
        let repo = base.trim_start_matches("https://github.com/");
        gh_latest(scenario, base, tag);
        fixture(
            scenario,
            &format!("manifest-{tag}"),
            &format!("{base}/releases/download/{tag}/release.json"),
            404,
            &[],
            Some("Not Found"),
        );
        let listed: Vec<Value> = assets
            .iter()
            .map(|(file, _)| {
                serde_json::json!({
                    "name": file,
                    "browser_download_url": format!("{base}/releases/download/{tag}/{file}"),
                })
            })
            .collect();
        fixture(
            scenario,
            &format!("rest-{tag}"),
            &format!("https://api.github.com/repos/{repo}/releases/tags/{tag}"),
            200,
            &[("x-ratelimit-remaining", "59")],
            Some(&serde_json::json!({ "tag_name": tag, "assets": listed }).to_string()),
        );
        for (file, files) in assets {
            let entries: Vec<(&str, &str)> = files
                .iter()
                .map(|(p, t)| (p.as_str(), t.as_str()))
                .collect();
            fixture_bytes(
                scenario,
                &format!("asset-{tag}-{file}"),
                &format!("{base}/releases/download/{tag}/{file}"),
                200,
                &[],
                Some(&zip_text(&entries)),
            );
        }
    }

    /// The record of `r#ref` on `id`, as `info` shows it: the folders it left out.
    pub fn left_out_names(home: &Home, r#ref: &str, id: &str) -> Vec<String> {
        let r = home.run(&["info", r#ref, "-t", id, "--offline"]);
        let envelope = r.expect(0);
        let left_out = &envelope["data"]["installed"][0]["left_out"];
        assert!(left_out.is_array(), "{}", r.context());
        left_out
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Sorted by folder name, to compare lists whose order differs.
    pub fn by_name(list: &Value) -> Value {
        let mut items = list.as_array().unwrap().clone();
        items.sort_by_key(|f| f["name"].as_str().unwrap().to_ascii_lowercase());
        Value::from(items)
    }

    /// The `folder_left_out` warnings of an envelope.
    pub fn left_out(envelope: &Value) -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] == "folder_left_out")
            .map(|w| w["message"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Serves `files` as the only asset of `base`'s release `tag`.
    pub fn release(
        scenario: &Path,
        base: &str,
        tag: &str,
        asset: &str,
        flavors: &[(&str, u32)],
        files: &[(String, String)],
    ) {
        let entries: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let assets: [Asset<'_>; 1] = [(asset, flavors, entries)];
        gh_release(scenario, base, tag, &assets);
    }

    pub fn serve_boss_mods(scenario: &Path) {
        release(
            scenario,
            DBM,
            "12.1.14",
            "DBM-Core-12.1.14.zip",
            DBM_FLAVORS,
            &dbm_files(),
        );
        release(
            scenario,
            BIGWIGS,
            "v427.6",
            "BigWigs-v427.6.zip",
            BW_FLAVORS,
            &bigwigs_files(),
        );
    }

    /// Folder names sorted by key, as the output lists them.
    pub fn sorted(names: &[&str]) -> Vec<String> {
        let mut names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        names.sort_by_key(|n| n.to_ascii_lowercase());
        names
    }

    pub fn strings(v: &Value) -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_owned())
            .collect()
    }

    /// `name=reason` per skipped folder.
    pub fn skipped(folders: &Value) -> Vec<String> {
        folders["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                format!(
                    "{}={}",
                    s["name"].as_str().unwrap(),
                    s["reason"].as_str().unwrap()
                )
            })
            .collect()
    }

    pub fn gated(names: &[&str]) -> Vec<String> {
        sorted(names)
            .into_iter()
            .map(|n| format!("{n}=game_type_excluded"))
            .collect()
    }

    /// The top-level entries of a client's AddOns folder.
    pub fn on_disk(home: &Home, dir: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(home.addons(dir))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort_by_key(|n| n.to_ascii_lowercase());
        names
    }

    /// Files under `dir` (names or contents) that mention `needle`; `net.log` is skipped.
    pub fn mentions(dir: &Path, needle: &str) -> Vec<String> {
        listing(dir)
            .lines()
            .filter(|p| !p.is_empty() && !p.starts_with("net.log"))
            .filter(|p| {
                let path = dir.join(p);
                p.contains(needle)
                    || (path.is_file()
                        && String::from_utf8_lossy(&fs::read(&path).unwrap()).contains(needle))
            })
            .map(str::to_owned)
            .collect()
    }

    pub fn status_folders(home: &Home, id: &str) -> Value {
        let envelope = home.run(&["status"]).expect(0);
        envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()["folders"]
            .clone()
    }
}

#[test]
fn dbm_and_bigwigs_install_what_each_client_loads() {
    let products: Vec<&str> = CLIENTS.iter().map(|c| c.0).collect();
    let home = Home::new(&products);
    serve_boss_mods(&home.http);

    for (_, id, dir) in CLIENTS {
        let retail = *id == "retail";
        let (dbm_added, dbm_skipped) = if retail {
            ([DBM_SHARED, DBM_RETAIL].concat(), Vec::new())
        } else {
            (DBM_SHARED.to_vec(), gated(DBM_RETAIL))
        };
        let (bw_added, bw_skipped) = if retail {
            ([BW_SHARED, BW_RETAIL].concat(), Vec::new())
        } else {
            (BW_SHARED.to_vec(), gated(BW_RETAIL))
        };
        let check = |envelope: &Value, context: &str| {
            let items = envelope["data"]["items"].as_array().unwrap();
            for (item, added, skips) in [
                (&items[0], &dbm_added, &dbm_skipped),
                (&items[1], &bw_added, &bw_skipped),
            ] {
                assert_eq!(item["status"], "installed", "{id}\n{context}");
                assert_eq!(
                    strings(&item["folders"]["added"]),
                    sorted(added),
                    "{id}\n{context}"
                );
                assert_eq!(skipped(&item["folders"]), *skips, "{id}\n{context}");
            }
        };

        let before = tree_hash(&home.addons(dir));
        let r = home.run(&["install", "dbm", "bigwigs", "-t", id, "--dry-run"]);
        check(&r.expect(0), &r.context());
        assert_eq!(tree_hash(&home.addons(dir)), before, "{id}: dry run wrote");

        let r = home.run(&["install", "dbm", "bigwigs", "-t", id]);
        let envelope = r.expect(0);
        check(&envelope, &r.context());
        let installed = [dbm_added.clone(), bw_added.clone()].concat();
        assert_eq!(on_disk(&home, dir), sorted(&installed), "{id}");

        let folders = status_folders(&home, id);
        assert_eq!(
            (
                &folders["managed"],
                &folders["unmanaged"],
                &folders["invalid"]
            ),
            (
                &Value::from(installed.len()),
                &Value::from(0),
                &Value::from(0)
            ),
            "{id}: {folders}"
        );

        // Skipped folders are nowhere in the client, the files index, history or
        // journals; only target records (and undo's copies of them) name them, as what
        // the install left out.
        let is_record = |path: &str| {
            let text = fs::read_to_string(home.state().join(path)).unwrap_or_default();
            serde_json::from_str::<Value>(&text).is_ok_and(|v| v["addons"].is_object())
        };
        for needle in ["DBM-Midnight", "BigWigs_TheVoidspire"]
            .iter()
            .filter(|_| !retail)
        {
            assert_eq!(mentions(&home.client(dir), needle), Vec::<String>::new());
            let state = mentions(&home.state(), needle);
            assert!(
                state.iter().any(|p| p.starts_with("targets/"))
                    && state
                        .iter()
                        .all(|p| !p.ends_with(".files.json") && is_record(p)),
                "{id}: {needle} in {state:?}"
            );
        }

        let r = home.run(&["undo"]);
        r.expect(0);
        assert_eq!(
            tree_hash(&home.addons(dir)),
            before,
            "{id}: undo restores the AddOns folder\n{}",
            r.context()
        );
    }
}

#[test]
fn the_human_output_lists_skipped_folders() {
    let home = Home::new(&["wow_classic_era"]);
    serve_boss_mods(&home.http);
    let argv: Vec<String> = ["install", "dbm", "-t", "era", "--dry-run"]
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
    let r = home.exec(&argv);
    assert_eq!(r.code, 0, "{}", r.context());
    assert!(
        r.stdout.contains(
            "  would skip 4 folders this client does not load: DBM-Brawlers, DBM-Lairs-Midnight, DBM-Midnight, DBM-Raids-Midnight"
        ),
        "{}",
        r.context()
    );
}

/// Without release.json metadata the archive's own TOCs must prove the skips.
#[test]
fn without_metadata_only_gated_folders_are_skipped() {
    let mut home = Home::new(&["wow", "wow_classic_era"]);
    let library = |extra: &[&str]| {
        let mut files = folder("Foo", RETAIL, extra);
        files.extend(folder("LibFoo", EVERY, &[]));
        files
    };

    // The addon itself is retail-only; only a bundled library loads on Classic Era.
    release(&home.http, FOO, "v1", "Foo.zip", &[], &library(&[]));
    let r = home.run(&["install", "gh:o/foo", "-t", "era", "--dry-run"]);
    let envelope = r.expect(3);
    assert_eq!(
        envelope["error"]["code"],
        "no_compatible_release",
        "{}",
        r.context()
    );
    let reason = envelope["error"]["details"]["tried"][0]["reason"].to_string();
    assert!(reason.contains("interface_not_in_family"), "{reason}");
    // The same holds when the addon's own folder is gated. Each scenario is a release of
    // its own: a tag's archive is remembered by its hash.
    let scenario = home.scenario("gated-addon");
    release(
        &scenario,
        FOO,
        "v2",
        "Foo.zip",
        &[],
        &library(&["## AllowLoadGameType: standard"]),
    );
    let r = home.run(&["install", "gh:o/foo", "-t", "era", "--dry-run"]);
    let error = &r.expect(3)["error"];
    assert_eq!(error["code"], "no_compatible_release");
    // What the addon supports is a read-only look away.
    assert_eq!(
        error["alternatives"][0]["argv"],
        serde_json::json!(["omaddons", "info", "gh:o/foo", "-t", "era"])
    );

    // A sibling that merely lists retail's interfaces is built for other clients: the
    // addon installs, and the sibling is left out and said so.
    let scenario = home.scenario("ungated-sibling");
    let mut files = folder("Foo", EVERY, &[]);
    files.extend(folder("FooRetail", RETAIL, &[]));
    release(&scenario, FOO, "v3", "Foo.zip", &[], &files);
    let r = home.run(&["install", "gh:o/foo", "-t", "era", "--dry-run"]);
    let envelope = r.expect(0);
    let folders = &envelope["data"]["items"][0]["folders"];
    assert_eq!(strings(&folders["added"]), ["Foo"], "{}", r.context());
    assert_eq!(
        folders["skipped"][0]["reason"],
        "interface_not_in_family",
        "{}",
        r.context()
    );
    let left_out: Vec<&Value> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "folder_left_out")
        .collect();
    assert_eq!(left_out.len(), 1, "{}", r.context());
    assert!(
        left_out[0]["message"]
            .as_str()
            .unwrap()
            .contains("FooRetail lists only interfaces 120100, 120105 (mainline)"),
        "{}",
        r.context()
    );

    // Gated siblings, and what requires them, are skipped.
    let scenario = home.scenario("gated-sibling");
    let mut files = folder("Foo", EVERY, &[]);
    files.extend(folder(
        "FooRetail",
        RETAIL,
        &["## AllowLoadGameType: standard"],
    ));
    files.extend(folder(
        "FooExtra",
        EVERY,
        &["## Dependencies: Foo, FooRetail"],
    ));
    release(&scenario, FOO, "v4", "Foo.zip", &[], &files);
    let r = home.run(&["install", "gh:o/foo", "-t", "era"]);
    let envelope = r.expect(0);
    let folders = &envelope["data"]["items"][0]["folders"];
    assert_eq!(strings(&folders["added"]), ["Foo"], "{}", r.context());
    assert_eq!(
        folders["skipped"],
        serde_json::json!([
            {"name": "FooExtra", "reason": "dependency_skipped", "folder": "FooRetail"},
            {"name": "FooRetail", "reason": "game_type_excluded"},
        ]),
        "{}",
        r.context()
    );
    assert_eq!(on_disk(&home, "_classic_era_"), ["Foo"]);
    // Retail loads all three.
    let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
    let envelope = r.expect(0);
    let folders = &envelope["data"]["items"][0]["folders"];
    assert_eq!(
        strings(&folders["added"]),
        ["Foo", "FooExtra", "FooRetail"],
        "{}",
        r.context()
    );
    assert_eq!(folders["skipped"], serde_json::json!([]));
}

/// Without release.json the addon's own folder must load, and a module family beside the
/// core folder (`DBM-Test`, extended by `DBM-Test-WarWithin`) is not the addon's own
/// folder: WoWInterface's DBM 11.2.15 was refused on every classic client for it
/// (keyless-coverage study, 2026-10-20).
#[test]
fn a_module_family_beside_the_core_folder_is_not_the_addons_own_folder() {
    let home = Home::new(&["wow", "wow_classic_era"]);
    let mut files = folder("DBM-Core", EVERY, &[]);
    files.extend(folder("DBM-GUI", EVERY, &["## Dependencies: DBM-Core"]));
    for module in ["DBM-Test", "DBM-Test-WarWithin"] {
        let lua = format!("{module}.lua");
        files.push((
            format!("{module}/{module}_Mainline.toc"),
            toc(RETAIL, &["## Dependencies: DBM-Core", &lua]),
        ));
        files.push((format!("{module}/{lua}"), format!("-- {module}\n")));
    }
    release(&home.http, FOO, "v1", "DBM-Core-11.2.15.zip", &[], &files);
    let r = home.run(&["install", "gh:o/foo", "-t", "era"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["items"][0];
    assert_eq!(item["name"], "DBM-Core", "{}", r.context());
    assert_eq!(strings(&item["folders"]["added"]), ["DBM-Core", "DBM-GUI"]);
    assert_eq!(
        skipped(&item["folders"]),
        [
            "DBM-Test=no_loadable_toc",
            "DBM-Test-WarWithin=no_loadable_toc"
        ],
        "{}",
        r.context()
    );
    assert_eq!(on_disk(&home, "_classic_era_"), ["DBM-Core", "DBM-GUI"]);
}

/// WeakAuras' release.json declares no retail or Forever build, and its zip loads
/// nowhere else: it is still refused there.
#[test]
fn weakauras_is_still_refused_where_it_is_not_declared() {
    let home = Home::new(&["wow", "wowf", "wow_classic_era", "wow_classic_titan"]);
    let mut files = Vec::new();
    for name in ["WeakAuras", "WeakAurasOptions"] {
        for (suffix, interface) in [
            ("Vanilla", "11509"),
            ("TBC", "20506"),
            ("Cata", "40402"),
            ("Mists", "50504"),
        ] {
            files.push((
                format!("{name}/{name}_{suffix}.toc"),
                toc(interface, &["init.lua"]),
            ));
        }
        // Titan's build, as WeakAuras ships it.
        files.push((
            format!("{name}/{name}_Wrath.toc"),
            toc("38002", &["## AllowLoadGameType: wrath, titan", "init.lua"]),
        ));
        files.push((format!("{name}/init.lua"), "-- init\n".to_owned()));
    }
    let flavors: &[(&str, u32)] = &[
        ("classic", 11_509),
        ("bcc", 20_506),
        ("cata", 40_402),
        ("mists", 50_504),
        ("titan", 38_002),
    ];
    release(
        &home.http,
        WEAKAURAS,
        "5.22.1",
        "WeakAuras-5.22.1.zip",
        flavors,
        &files,
    );

    for id in ["retail", "forever"] {
        let r = home.run(&["install", "weakauras", "-t", id, "--dry-run"]);
        assert_eq!(
            r.expect(3)["error"]["code"],
            "no_compatible_release",
            "{id}\n{}",
            r.context()
        );
    }
    for id in ["era", "titan"] {
        let r = home.run(&["install", "weakauras", "-t", id]);
        let envelope = r.expect(0);
        let folders = &envelope["data"]["items"][0]["folders"];
        assert_eq!(
            strings(&folders["added"]),
            ["WeakAuras", "WeakAurasOptions"],
            "{id}\n{}",
            r.context()
        );
        assert_eq!(folders["skipped"], serde_json::json!([]));
    }
}

/// A folder an earlier release installed that the next one gates to retail is removed
/// by the update, and undo brings it back.
#[test]
fn an_update_that_newly_skips_a_folder_removes_it_and_undo_restores_it() {
    let mut home = Home::new(&["wow_classic_era"]);
    let flavors: &[(&str, u32)] = &[("mainline", 120_105), ("classic", 11_509)];
    let mut v1 = folder("Foo", EVERY, &["## Version: 1"]);
    v1.extend(folder("FooModule", EVERY, &["## Dependencies: Foo"]));
    release(&home.http, FOO, "v1", "Foo.zip", flavors, &v1);
    let r = home.run(&["install", "gh:o/foo", "-t", "era"]);
    let envelope = r.expect(0);
    assert_eq!(
        strings(&envelope["data"]["items"][0]["folders"]["added"]),
        ["Foo", "FooModule"],
        "{}",
        r.context()
    );
    let after_v1 = tree_hash(&home.addons("_classic_era_"));

    let scenario = home.scenario("v2");
    let mut v2 = folder("Foo", EVERY, &["## Version: 2"]);
    v2.extend(folder(
        "FooModule",
        RETAIL,
        &["## AllowLoadGameType: standard", "## Dependencies: Foo"],
    ));
    release(&scenario, FOO, "v2", "Foo.zip", flavors, &v2);
    let r = home.run(&["install", "gh:o/foo", "-t", "era"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["items"][0];
    assert_eq!(item["status"], "updated", "{}", r.context());
    assert_eq!(
        (
            strings(&item["folders"]["replaced"]),
            strings(&item["folders"]["removed"]),
            skipped(&item["folders"]),
        ),
        (
            vec!["Foo".to_owned()],
            vec!["FooModule".to_owned()],
            vec!["FooModule=game_type_excluded".to_owned()]
        ),
        "{}",
        r.context()
    );
    assert_eq!(on_disk(&home, "_classic_era_"), ["Foo"]);
    let info = home.run(&["list", "-t", "era"]).expect(0);
    assert_eq!(
        info["data"]["targets"][0]["addons"][0]["folders"], 1,
        "the record holds only the installed folder: {info}"
    );

    let r = home.run(&["undo"]);
    r.expect(0);
    assert_eq!(
        tree_hash(&home.addons("_classic_era_")),
        after_v1,
        "{}",
        r.context()
    );
}

// ---- Choosing a download by evidence (omad-mixed-archive-single-file-skk1) ---------------
// Shapes of real releases, with the TOC lines that decide kept verbatim: a folder built
// for other clients only is left out and said so, a clean candidate beats one that leaves
// folders out, and a file its publisher labels for other clients only is refused.

/// ArkInventory 3.12.17 (`wowi:6488`): four folders with plain (retail), `_Mists`,
/// `_Vanilla`, `_BCC` and `_Camelot` TOCs, and a disabled example module whose only TOC
/// lists 120105.
fn ark_files() -> Vec<(String, String)> {
    let mut files = Vec::new();
    for name in ARK {
        files.push((
            format!("{name}/{name}.toc"),
            toc("120105", &[&format!("## Title: {name}")]),
        ));
        for (suffix, interface) in [
            ("Mists", "50504"),
            ("Vanilla", "11509"),
            ("BCC", "20506"),
            ("Camelot", "16001"),
        ] {
            files.push((
                format!("{name}/{name}_{suffix}.toc"),
                toc(interface, &[&format!("## Title: {name}")]),
            ));
        }
        files.push((format!("{name}/{name}.lua"), format!("-- {name}\n")));
    }
    files.extend(folder(
        "ArkInventoryRules_Example",
        "120105",
        &[
            "## DefaultState: Disabled",
            "## RequiredDeps: ArkInventoryRules",
        ],
    ));
    files
}

const ARK: [&str; 4] = [
    "ArkInventory",
    "ArkInventoryConfig",
    "ArkInventoryRules",
    "ArkInventorySearch",
];

#[test]
fn arkinventory_leaves_out_its_retail_only_example_on_classic_clients() {
    const BASE: &str = "https://github.com/o/arkinventory";
    let home = Home::new(&["wow", "wow_classic_era", "wow_classic", "wowf"]);
    release(
        &home.http,
        BASE,
        "3.12.17",
        "ArkInventory-3.12.17.zip",
        &[],
        &ark_files(),
    );
    for (id, dir) in [
        ("classic", "_classic_"),
        ("era", "_classic_era_"),
        ("forever", "_forever_"),
    ] {
        let r = home.run(&["install", "gh:o/arkinventory", "-t", id]);
        let envelope = r.expect(0);
        let folders = &envelope["data"]["items"][0]["folders"];
        assert_eq!(strings(&folders["added"]), ARK, "{id}: {}", r.context());
        assert_eq!(
            skipped(folders),
            ["ArkInventoryRules_Example=interface_not_in_family"],
            "{id}: {}",
            r.context()
        );
        let warnings = left_out(&envelope);
        assert_eq!(warnings.len(), 1, "{id}: {}", r.context());
        assert!(
            warnings[0]
                .contains("ArkInventoryRules_Example lists only interface 120105 (mainline)"),
            "{id}: {warnings:?}"
        );
        assert_eq!(on_disk(&home, dir), ARK, "{id}");
    }
    let r = home.run(&["install", "gh:o/arkinventory", "-t", "retail"]);
    let envelope = r.expect(0);
    let folders = &envelope["data"]["items"][0]["folders"];
    let mut every = ARK.to_vec();
    every.insert(3, "ArkInventoryRules_Example");
    assert_eq!(strings(&folders["added"]), every, "{}", r.context());
    assert!(left_out(&envelope).is_empty(), "{}", r.context());
}

/// Titan Panel 9.3.5 (`wowi:8092`): TitanAmmo, verbatim, lists 205056 (a typo for 2.5.x)
/// under `AllowLoadGameType: camelot, tbc, vanilla`. On Anniversary it is the author's
/// build for this client, out of date: every folder installs, nothing is left out.
#[test]
fn titan_panel_installs_whole_on_anniversary_with_titan_ammo_out_of_date() {
    const BASE: &str = "https://github.com/o/titan";
    let home = Home::new(&["wow_anniversary"]);
    let every = "11509, 16001, 20506, 50504, 120105";
    let mut files = folder("Titan", every, &["## Title: Titan Panel"]);
    files.extend(folder("TitanBag", every, &["## Dependencies: Titan"]));
    files.push((
        "TitanAmmo/TitanAmmo.toc".to_owned(),
        "## Interface: 11509, 16001, 205056\n## AllowLoadGameType: camelot, tbc, vanilla\n## Notes: Adds an ammo monitor to Titan Panel - Classic versions only\n## Dependencies: Titan\nTitanAmmo.lua\n".to_owned(),
    ));
    files.push((
        "TitanAmmo/TitanAmmo.lua".to_owned(),
        "-- TitanAmmo\n".to_owned(),
    ));
    release(&home.http, BASE, "9.3.5", "Titan-9.3.5.zip", &[], &files);
    let r = home.run(&["install", "gh:o/titan", "-t", "anniversary"]);
    let envelope = r.expect(0);
    let folders = &envelope["data"]["items"][0]["folders"];
    assert_eq!(
        strings(&folders["added"]),
        ["Titan", "TitanAmmo", "TitanBag"],
        "{}",
        r.context()
    );
    assert!(skipped(folders).is_empty(), "{}", r.context());
    assert!(left_out(&envelope).is_empty(), "{}", r.context());
    let out_of_date: Vec<&Value> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "out_of_date")
        .collect();
    assert_eq!(out_of_date.len(), 1, "{}", r.context());
    assert!(
        out_of_date[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("TitanAmmo:"),
        "{}",
        r.context()
    );
}

/// Carbonite All-in-One on retail (`Carbonite.Info` lists only classic and Forever
/// interfaces) and BlizzMove 3.8.5 on Forever (`BlizzMove_Debug` omits 16001): the stray
/// folder is left out, the rest installs.
#[test]
fn carbonite_and_blizzmove_leave_out_their_stray_folder() {
    let home = Home::new(&["wow", "wowf"]);
    let mut carbonite = folder("Carbonite", "11509, 16001, 120105", &[]);
    carbonite.extend(folder("Carbonite.Info", "11509, 16001", &[]));
    release(
        &home.http,
        "https://github.com/o/carbonite",
        "v1",
        "Carbonite.zip",
        &[],
        &carbonite,
    );
    let mut blizzmove = folder("BlizzMove", "11509, 16001, 20506, 50504, 120105", &[]);
    blizzmove.extend(folder(
        "BlizzMove_Debug",
        "11509, 20506, 50504, 120105",
        &[],
    ));
    release(
        &home.http,
        "https://github.com/o/blizzmove",
        "3.8.5",
        "BlizzMove-3.8.5.zip",
        &[],
        &blizzmove,
    );
    for (r#ref, id, dir, kept, stray) in [
        (
            "gh:o/carbonite",
            "retail",
            "_retail_",
            "Carbonite",
            "Carbonite.Info",
        ),
        (
            "gh:o/blizzmove",
            "forever",
            "_forever_",
            "BlizzMove",
            "BlizzMove_Debug",
        ),
    ] {
        let r = home.run(&["install", r#ref, "-t", id]);
        let envelope = r.expect(0);
        let folders = &envelope["data"]["items"][0]["folders"];
        assert_eq!(strings(&folders["added"]), [kept], "{}", r.context());
        assert_eq!(
            skipped(folders),
            [format!("{stray}=interface_not_in_family")],
            "{}",
            r.context()
        );
        assert_eq!(left_out(&envelope).len(), 1, "{}", r.context());
        assert!(on_disk(&home, dir).contains(&kept.to_owned()));
        assert!(!on_disk(&home, dir).contains(&stray.to_owned()));
    }
}

/// A clean candidate beats one that leaves folders out, whatever their rank: on Classic
/// Era `Foo-classic.zip` ranks first by its name but carries a retail-only folder, and
/// the unannotated `Foo.zip` installs.
#[test]
fn a_clean_candidate_beats_one_that_leaves_folders_out_whatever_the_rank() {
    let home = Home::new(&["wow_classic_era"]);
    let mut classic = folder("Foo", "11509, 120105", &[]);
    classic.extend(folder("FooRetail", "120105", &[]));
    let plain = folder("Foo", "11509, 120105", &[]);
    rest_release(
        &home.http,
        FOO,
        "v1",
        &[("Foo-classic.zip", classic), ("Foo.zip", plain)],
    );
    let r = home.run(&["install", "gh:o/foo", "-t", "era", "-v"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["items"][0];
    assert!(
        item["asset_url"].as_str().unwrap().ends_with("/Foo.zip"),
        "{}",
        r.context()
    );
    assert!(skipped(&item["folders"]).is_empty(), "{}", r.context());
    assert!(left_out(&envelope).is_empty(), "{}", r.context());
}

/// A file its publisher labels for other clients only is refused, even with clean TOCs,
/// and the message says by what: its `release.json` entry, or its file name.
#[test]
fn a_file_labelled_for_other_clients_only_is_refused_naming_the_evidence() {
    let mut home = Home::new(&["wow_classic", "wow_classic_era"]);
    release(
        &home.http,
        FOO,
        "v1",
        "Foo.zip",
        &[("mainline", 120_105)],
        &folder("Foo", "120105, 50504", &[]),
    );
    let said = "release.json lists Foo.zip for mainline only";
    let r = home.run(&["install", "gh:o/foo", "-t", "classic"]);
    let error = &r.expect(3)["error"];
    assert_eq!(error["code"], "no_compatible_release", "{}", r.context());
    assert!(
        error["message"].as_str().unwrap().contains(said),
        "{}",
        r.context()
    );
    assert!(
        error["details"]["tried"][0]["reason"]
            .as_str()
            .unwrap()
            .contains(said),
        "{}",
        r.context()
    );
    let human = home.exec(&[
        "install".into(),
        "gh:o/foo".into(),
        "-t".into(),
        "classic".into(),
    ]);
    assert!(human.stderr.contains(said), "{}", human.stderr);
    assert!(!home.addons("_classic_").join("Foo").exists());

    let scenario = home.scenario("named-for-retail");
    rest_release(
        &scenario,
        FOO,
        "v2",
        &[("Foo-1.0-retail.zip", folder("Foo", "11509, 120105", &[]))],
    );
    let said = "its file name Foo-1.0-retail.zip names mainline only";
    let r = home.run(&["install", "gh:o/foo", "-t", "era"]);
    let error = &r.expect(3)["error"];
    assert_eq!(error["code"], "no_compatible_release", "{}", r.context());
    assert!(
        error["message"].as_str().unwrap().contains(said),
        "{}",
        r.context()
    );
    assert!(!home.addons("_classic_era_").join("Foo").exists());
}

/// The addon's own folder carrying a TOC named for this client, at an in-range
/// interface, is the author building for it: metadata naming other clients only is no
/// evidence against it. Raider.IO's shape (2026-10-24): `release.json` lists mainline
/// only, and `RaiderIO/` ships `_Classic` (50504) and `_Vanilla` (11509) TOCs. A
/// client whose own TOC is out of range (Anniversary reads `_Classic` at 50504) is
/// still refused.
#[test]
fn the_addons_own_toc_for_this_client_outweighs_metadata_for_others() {
    let home = Home::new(&["wow", "wow_classic", "wow_classic_era", "wow_anniversary"]);
    let files = vec![
        (
            "RaiderIO/RaiderIO.toc".to_owned(),
            toc("120100, 120105", &[]),
        ),
        (
            "RaiderIO/RaiderIO_Classic.toc".to_owned(),
            toc("50504", &[]),
        ),
        (
            "RaiderIO/RaiderIO_Vanilla.toc".to_owned(),
            toc("11509", &[]),
        ),
        ("RaiderIO/core.lua".to_owned(), "-- RaiderIO\n".to_owned()),
    ];
    release(
        &home.http,
        FOO,
        "v1",
        "RaiderIO.zip",
        &[("mainline", 120_100), ("mainline", 120_105)],
        &files,
    );
    for (id, dir) in [
        ("retail", "_retail_"),
        ("classic", "_classic_"),
        ("era", "_classic_era_"),
    ] {
        let r = home.run(&["install", "gh:o/foo", "-t", id]);
        r.expect(0);
        assert!(
            home.addons(dir).join("RaiderIO/RaiderIO.toc").is_file(),
            "{id}: {}",
            r.context()
        );
    }
    let r = home.run(&["install", "gh:o/foo", "-t", "anniversary"]);
    let error = &r.expect(3)["error"];
    assert_eq!(error["code"], "no_compatible_release", "{}", r.context());
    assert!(!home.addons("_anniversary_").join("RaiderIO").exists());
}

/// On a legacy client a `release.json` cannot name the client, so its entries say
/// nothing: the TOCs decide, as before.
#[test]
fn a_legacy_client_ignores_release_json() {
    let mut home = Home::new(&["wow"]);
    let client =
        home.with_custom(CustomRoot::at(home.env.root().join("wow-335")).version(3, 3, 5, 12_340));
    release(
        &home.http,
        FOO,
        "v1",
        "Foo.zip",
        &[("mainline", 120_105)],
        &folder("Foo", "30300", &[]),
    );
    let targets = home.run(&["targets"]).expect(0);
    let id = targets["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| {
            t["client_dir"]
                .as_str()
                .is_some_and(|d| Path::new(d) == client)
        })
        .and_then(|t| t["id"].as_str())
        .unwrap()
        .to_owned();
    let r = home.run(&["install", "gh:o/foo", "-t", &id]);
    r.expect(0);
    assert!(
        client.join("Interface/AddOns/Foo").is_dir(),
        "{}",
        r.context()
    );
}

/// The update check chooses as install does: an update whose only winning build leaves
/// a folder out is an update, verified, and `info` says it loads.
#[test]
fn the_update_check_counts_a_build_that_leaves_folders_out() {
    let mut home = Home::new(&["wow_classic_era"]);
    let v1 = folder("Foo", "11509", &["## Version: v1"]);
    let mut v2 = folder("Foo", "11509", &["## Version: v2"]);
    v2.extend(folder("FooRetail", "120105", &[]));
    fn entries(files: &[(String, String)]) -> Vec<(&str, &str)> {
        files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect()
    }
    let unannotated: &[(&str, u32)] = &[];
    let old: [Asset<'_>; 1] = [("Foo.zip", unannotated, entries(&v1))];
    let new: [Asset<'_>; 1] = [("Foo.zip", unannotated, entries(&v2))];
    home.installed_behind(FOO, ("v1", &old), ("v2", &new), &["era"]);
    let r = home.run(&["outdated", "-t", "era", "--verify"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["targets"][0]["items"][0];
    assert_eq!(
        (&item["status"], &item["confidence"]),
        (&Value::from("update_available"), &Value::from("verified")),
        "{}",
        r.context()
    );
    let r = home.run(&["info", "gh:o/foo", "-t", "era"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["latest"]["compat"]["era"],
        "verified",
        "{}",
        r.context()
    );
}

/// A reinstall's cached archive carries no rank, so one that leaves a folder out never
/// wins by itself: the source is asked for the release, and the same folders install.
#[test]
fn a_reinstall_of_a_build_that_leaves_folders_out_asks_the_source() {
    let home = Home::new(&["wow_classic_era"]);
    let mut files = folder("Foo", "11509", &[]);
    files.extend(folder("FooRetail", "120105", &[]));
    release(&home.http, FOO, "v1", "Foo.zip", &[], &files);
    home.run(&["install", "gh:o/foo", "-t", "era"]).expect(0);
    let before = net_log(&home).len();
    let r = home.run(&["install", "gh:o/foo", "-t", "era", "--reinstall"]);
    let envelope = r.expect(0);
    assert!(
        net_log(&home).len() > before,
        "{}",
        net_log(&home).context()
    );
    let folders = &envelope["data"]["items"][0]["folders"];
    assert_eq!(strings(&folders["replaced"]), ["Foo"], "{}", r.context());
    assert_eq!(on_disk(&home, "_classic_era_"), ["Foo"]);
}

/// A human refusal names the folder that decided it and what it lists.
#[test]
fn a_human_refusal_names_the_folder_and_what_it_lists() {
    let home = Home::new(&["wow_classic_era"]);
    release(
        &home.http,
        FOO,
        "v1",
        "Foo.zip",
        &[],
        &folder("Foo", "120105", &[]),
    );
    let human = home.exec(&[
        "install".into(),
        "gh:o/foo".into(),
        "-t".into(),
        "era".into(),
    ]);
    assert_eq!(human.code, 3, "{}", human.stderr);
    assert!(
        human.stderr.contains(
            "no candidate loads on this client: Foo.zip: Foo lists only interface 120105 (mainline)"
        ),
        "{}",
        human.stderr
    );
}

// ---- What an install left out, on the record (omad-author-built-loading-5po6.3) ----------

/// After a DBM install on Classic Era the record keeps the retail-only modules it left
/// out, with the reasons `folders.skipped` gave, and `info` and `list -v` show them.
#[test]
fn the_record_keeps_what_an_install_left_out_and_info_and_list_show_it() {
    const REF: &str = "gh:DeadlyBossMods/DeadlyBossMods";
    let home = Home::new(&["wow_classic_era"]);
    serve_boss_mods(&home.http);
    let r = home.run(&["install", REF, "-t", "era"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["items"][0];
    let name = item["name"].as_str().unwrap().to_owned();
    let skipped_json = by_name(&item["folders"]["skipped"]);
    assert_eq!(
        skipped(&item["folders"]),
        gated(DBM_RETAIL),
        "{}",
        r.context()
    );

    let r = home.run(&["info", REF, "-t", "era", "--offline"]);
    let info = r.expect(0);
    assert_eq!(
        by_name(&info["data"]["installed"][0]["left_out"]),
        skipped_json,
        "{}",
        r.context()
    );
    let human = home.exec(&[
        "info".into(),
        REF.into(),
        "-t".into(),
        "era".into(),
        "--offline".into(),
    ]);
    assert!(
        human
            .stdout
            .contains("left out: DBM-Brawlers (gated to other game types)"),
        "{}",
        human.stdout
    );

    let r = home.run(&["list", "-t", "era", "-v"]);
    let list = r.expect(0);
    let addon = &list["data"]["targets"][0]["addons"][0];
    assert_eq!(by_name(&addon["left_out"]), skipped_json, "{}", r.context());
    let plain = home.run(&["list", "-t", "era"]).expect(0);
    assert!(plain["data"]["targets"][0]["addons"][0]["left_out"].is_null());
    let human = home.exec(&["list".into(), "-t".into(), "era".into(), "-v".into()]);
    assert!(
        human.stdout.contains(&format!(
            "{name} left out: DBM-Brawlers (gated to other game types)"
        )),
        "{}",
        human.stdout
    );

    // `export` writes requests, not what a client left out.
    let exported = home.run(&["export"]).expect(0);
    let content = exported["data"]["content"].as_str().unwrap();
    assert!(
        !content.contains("left_out") && !content.contains("DBM-Brawlers"),
        "{content}"
    );
}

/// An update whose release loads a folder left out before installs it and drops it from
/// the record; undoing the update brings the record back with it.
#[test]
fn an_update_that_loads_a_left_out_folder_records_it_and_undo_restores_the_record() {
    let mut home = Home::new(&["wow_classic_era"]);
    let mut v1 = folder("Foo", EVERY, &["## Version: 1"]);
    v1.extend(folder(
        "FooModule",
        RETAIL,
        &["## AllowLoadGameType: standard", "## Dependencies: Foo"],
    ));
    release(&home.http, FOO, "v1", "Foo.zip", &[], &v1);
    home.run(&["install", "gh:o/foo", "-t", "era"]).expect(0);
    assert_eq!(left_out_names(&home, "gh:o/foo", "era"), ["FooModule"]);

    let scenario = home.scenario("v2");
    let mut v2 = folder("Foo", EVERY, &["## Version: 2"]);
    v2.extend(folder("FooModule", EVERY, &["## Dependencies: Foo"]));
    release(&scenario, FOO, "v2", "Foo.zip", &[], &v2);
    let r = home.run(&["install", "gh:o/foo", "-t", "era"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["items"][0];
    assert_eq!(item["status"], "updated", "{}", r.context());
    assert_eq!(
        strings(&item["folders"]["added"]),
        ["FooModule"],
        "{}",
        r.context()
    );
    assert!(left_out_names(&home, "gh:o/foo", "era").is_empty());

    home.run(&["undo"]).expect(0);
    assert_eq!(left_out_names(&home, "gh:o/foo", "era"), ["FooModule"]);
}

/// `copy` installs each addon from its ref on the other client, judged there: a folder
/// Classic Era left out, which Anniversary loads, is installed on Anniversary and never
/// recorded as left out there.
#[test]
fn copy_judges_the_other_client_and_carries_no_left_out() {
    let home = Home::new(&["wow_classic_era", "wow_anniversary"]);
    let mut files = folder("Foo", EVERY, &[]);
    files.extend(folder(
        "FooTbc",
        EVERY,
        &["## AllowLoadGameType: tbc", "## Dependencies: Foo"],
    ));
    release(&home.http, FOO, "v1", "Foo.zip", &[], &files);
    home.run(&["install", "gh:o/foo", "-t", "era"]).expect(0);
    assert_eq!(left_out_names(&home, "gh:o/foo", "era"), ["FooTbc"]);
    let r = home.run(&["copy", "--from", "era", "--to", "anniversary"]);
    r.expect(0);
    assert!(
        left_out_names(&home, "gh:o/foo", "anniversary").is_empty(),
        "{}",
        r.context()
    );
    assert!(on_disk(&home, "_anniversary_").contains(&"FooTbc".to_owned()));
}
