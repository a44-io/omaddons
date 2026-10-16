//! Multi-client archives end to end. DBM and BigWigs ship one zip for every client:
//! shared folders whose TOCs list every client's interface, and retail-only boss modules
//! gated by `## AllowLoadGameType: standard`. Each client installs the folders it loads
//! and lists the rest in `folders.skipped`; skipped folders never reach
//! `Interface/AddOns`, the record, the files index or a snapshot. Releases are replayed
//! from synthetic fixtures shaped like the real releases (layouts verified 2026-10-15:
//! DBM-Core 12.1.14, BigWigs v427.6).

mod common;

use std::fs;
use std::path::Path;

use common::addons::toc;
use common::home::Home;
use common::http::{Asset, gh_release};
use common::trees::{listing, tree_hash};
use serde_json::Value;
use support::{
    folder, gated, mentions, on_disk, release, serve_boss_mods, skipped, sorted, status_folders,
    strings,
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

        // Skipped folders are nowhere: not in the client, the records, the files index,
        // history or snapshots.
        for needle in ["DBM-Midnight", "BigWigs_TheVoidspire"]
            .iter()
            .filter(|_| !retail)
        {
            assert_eq!(mentions(&home.client(dir), needle), Vec::<String>::new());
            assert_eq!(mentions(&home.state(), needle), Vec::<String>::new());
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
    assert_eq!(r.expect(3)["error"]["code"], "no_compatible_release");

    // A sibling that merely lists retail's interfaces marks a retail build.
    let scenario = home.scenario("ungated-sibling");
    let mut files = folder("Foo", EVERY, &[]);
    files.extend(folder("FooRetail", RETAIL, &[]));
    release(&scenario, FOO, "v3", "Foo.zip", &[], &files);
    let r = home.run(&["install", "gh:o/foo", "-t", "era", "--dry-run"]);
    assert_eq!(r.expect(3)["error"]["code"], "no_compatible_release");

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
