//! The folder inventory over synthetic clients: classes, ownership, duplicates, TOC
//! details, read-only behavior and a snapshot of a mixed inventory.

mod common;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use common::addons::{addon_folder, toc};
use common::env::TestEnv;
use common::procfs::running_as_root;
use common::trees::{BattlenetRoot, CustomRoot, tree_hash};
use omaddons::discover::DiscoverySource;
use omaddons::discover::battlenet::{BattlenetHints, discover_root};
use omaddons::discover::custom::discover_custom_root;
use omaddons::error::ErrorCode;
use omaddons::inventory::{Depth, FolderClass, Inventory, MissingFolder, inventory};
use omaddons::model::{
    AddonReason, AddonRecord, AddonState, Channel, FolderName, Target, VersionConfidence,
};
use omaddons::rules::RulesTable;
use omaddons::state::records::TargetRecord;
use support::{addon, battlenet_target, entry, record};

#[cfg(test)]
mod support {
    use super::*;

    pub fn addon(name: &str, folders: &[&str]) -> AddonRecord {
        AddonRecord {
            r#ref: "gh:WeakAuras/WeakAuras2".parse().unwrap(),
            name: name.into(),
            version: "5.19.10".into(),
            version_confidence: VersionConfidence::Release,
            release_id: None,
            channel: Channel::Stable,
            pinned: false,
            reason: AddonReason::Explicit,
            required_by: vec![],
            folders: folders
                .iter()
                .map(|f| FolderName::new(f).unwrap())
                .collect(),
            state: AddonState::Ok,
            installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
            tx_id: "01JB8Q2VA00000000000000000".parse().unwrap(),
            blob_sha256: None,
        }
    }

    /// The target of `folder` in a Battle.net root built with `product`.
    pub fn battlenet_target(root: &Path, folder: &str) -> Target {
        let (targets, _) = discover_root(
            root,
            &RulesTable::builtin(),
            &BattlenetHints::default(),
            "battlenet",
            &DiscoverySource::Scan,
        );
        targets
            .into_iter()
            .find(|t| t.target.flavor_dir.as_deref() == Some(folder))
            .unwrap()
            .target
    }

    pub fn entry<'a>(inv: &'a Inventory, name: &str) -> &'a omaddons::inventory::FolderEntry {
        inv.folders.iter().find(|e| e.name == name).unwrap()
    }

    /// A target record holding `addons`.
    pub fn record(addons: &[AddonRecord]) -> TargetRecord {
        let mut record = TargetRecord::empty("01JB8Q2V9W0000000000000000".parse().unwrap());
        for a in addons {
            record
                .addons
                .insert(TargetRecord::addon_key(&a.name), a.clone());
        }
        record
    }
}

#[test]
fn toc_summaries_carry_what_adoption_reads() {
    let env = TestEnv::new();
    let root = BattlenetRoot::at(env.root().join("wow"))
        .product("wow")
        .product("wow_classic_era")
        .product("wow_classic")
        .product("wowf")
        .build();
    // Shaped like ElvUI's: titles only with inline directives at the end of the line.
    let elvui = toc(
        "11509, 120105, 50504, 16001",
        &[
            "## Title: |cff1784d1ElvUI|r |TInterface\\Icons\\elv:16|t [AllowLoadGameType standard]",
            "## Title: |cff1784d1ElvUI|r Classic [AllowLoadGameType vanilla]",
            "## Title: |cff1784d1ElvUI|r Mists [AllowLoadGameType mists]",
            "## Title: |cff1784d1ElvUI|r Forever [AllowLoadGameType camelot][ExcludeLoadGameType standard, classic]",
            "## X-Website: https://tukui.org",
            "## X-Tukui-ProjectID: -2",
            "## X-Tukui-ProjectFolders: ElvUI, ElvUI_Libraries, ElvUI_Options",
            "## Dependencies: ElvUI_Libraries",
            "## Dependencies: ElvUI_ClassicBits [AllowLoadGameType vanilla]",
            "## OptionalDeps: Masque, BugSack",
            "## OptionalDeps: LibHealComm-4.0 [AllowLoadGameType classic]",
            "## LoadWith: Blizzard_Nothing",
        ],
    );
    let summaries: Vec<omaddons::inventory::TocSummary> =
        ["_retail_", "_classic_era_", "_classic_", "_forever_"]
            .iter()
            .map(|folder| {
                let target = battlenet_target(&root, folder);
                let addons = target.client_dir.join("Interface/AddOns");
                addon_folder(&addons, "ElvUI", &[("ElvUI.toc", &elvui)], &[]);
                let inv = inventory(&target, None, &RulesTable::builtin(), Depth::Full).unwrap();
                entry(&inv, "ElvUI").toc.clone().unwrap()
            })
            .collect();
    let [retail, era, mists, forever] = summaries.as_slice() else {
        unreachable!("{summaries:?}")
    };
    assert_eq!(
        retail.title.as_deref(),
        Some("|cff1784d1ElvUI|r |TInterface\\Icons\\elv:16|t")
    );
    let titles: Vec<Option<&str>> = summaries.iter().map(|s| s.title_plain.as_deref()).collect();
    assert_eq!(
        titles,
        [
            Some("ElvUI"),
            Some("ElvUI Classic"),
            Some("ElvUI Mists"),
            Some("ElvUI Forever")
        ]
    );
    assert_eq!(retail.website.as_deref(), Some("https://tukui.org"));
    assert_eq!(
        retail.tukui_project_folders,
        ["ElvUI", "ElvUI_Libraries", "ElvUI_Options"]
    );
    assert_eq!(retail.dependencies, ["ElvUI_Libraries"]);
    assert_eq!(era.dependencies, ["ElvUI_Libraries", "ElvUI_ClassicBits"]);
    assert_eq!(mists.dependencies, ["ElvUI_Libraries"]);
    // The last OptionalDeps line each client reads.
    assert_eq!(retail.optional_deps, ["Masque", "BugSack"]);
    assert_eq!(era.optional_deps, ["LibHealComm-4.0"]);
    assert_eq!(mists.optional_deps, ["LibHealComm-4.0"]);
    assert_eq!(forever.optional_deps, ["Masque", "BugSack"]);
}

#[test]
fn a_private_server_client_is_all_protected() {
    let env = TestEnv::new();
    let root = CustomRoot::at(env.home().join("Games/wotlk"))
        .blizzard_folders(23)
        .build();
    let (found, _) = discover_custom_root(
        &root,
        &RulesTable::builtin(),
        "custom",
        &DiscoverySource::Scan,
    );
    let target = found.unwrap().target;
    let before = tree_hash(&root);
    let inv = inventory(&target, None, &RulesTable::builtin(), Depth::Full).unwrap();
    assert_eq!(
        inv.counts.protected,
        23,
        "{}",
        serde_json::to_string_pretty(&inv).unwrap()
    );
    assert_eq!(inv.folders.len(), 23);
    assert!(
        inv.folders
            .iter()
            .all(|e| e.class == FolderClass::Protected && e.toc.is_some())
    );
    assert_eq!(tree_hash(&root), before, "the inventory wrote nothing");
}

#[test]
fn classes_ownership_and_duplicates() {
    let env = TestEnv::new();
    let root = BattlenetRoot::at(env.root().join("wow"))
        .product("wow")
        .build();
    let target = battlenet_target(&root, "_retail_");
    let addons = target.client_dir.join("Interface/AddOns");
    let t = toc("120105", &["## Title: X"]);
    for name in [
        "blizzard_auctionui",
        "BLIZZARD_x",
        "WeakAuras",
        "weakauras",
        "Details",
        "Shared",
        "Plater",
    ] {
        addon_folder(&addons, name, &[(&format!("{name}.toc"), &t)], &[]);
    }
    let outside = env.root().join("dev/MyAddon");
    addon_folder(&outside, "", &[], &[]);
    symlink(&outside, addons.join("Blizzard_Foo")).unwrap();
    symlink(&outside, addons.join("Linked")).unwrap();
    fs::create_dir_all(addons.join(".git")).unwrap();
    fs::create_dir_all(addons.join("bad\u{1}name")).unwrap();
    fs::write(addons.join("foo.zip"), "PK").unwrap();

    let rec = record(&[
        addon("Details", &["Details"]),
        addon("Plater", &["Plater", "Linked", "Gone"]),
        addon("SharedA", &["Shared"]),
        addon("SharedB", &["shared"]),
    ]);
    let inv = inventory(&target, Some(&rec), &RulesTable::builtin(), Depth::Classes).unwrap();
    let class = |name| entry(&inv, name).class;
    for name in ["blizzard_auctionui", "BLIZZARD_x", "Blizzard_Foo"] {
        assert_eq!(class(name), FolderClass::Protected, "{name}");
    }
    assert_eq!(class("Details"), FolderClass::Managed);
    assert_eq!(entry(&inv, "Details").owners, ["Details"]);
    assert_eq!(class("Linked"), FolderClass::Linked);
    assert_eq!(
        entry(&inv, "Linked").owners,
        ["Plater"],
        "a linked managed folder keeps its owner"
    );
    assert_eq!(class("Plater"), FolderClass::Managed);
    assert_eq!(class(".git"), FolderClass::Invalid);
    assert_eq!(class("bad\u{1}name"), FolderClass::Invalid);
    assert!(entry(&inv, "WeakAuras").case_duplicate && entry(&inv, "weakauras").case_duplicate);
    assert!(!entry(&inv, "Details").case_duplicate);
    let shared = entry(&inv, "Shared");
    assert!(shared.ownership_conflict, "{:?}", shared.owners);
    assert_eq!(shared.owners, ["SharedA", "SharedB"]);
    assert_eq!(
        inv.missing,
        [MissingFolder {
            addon: "Plater".into(),
            folder: "Gone".into()
        }]
    );
    assert_eq!(inv.stray_files, 1);
    assert_eq!(
        (
            inv.counts.protected,
            inv.counts.managed,
            inv.counts.linked,
            inv.counts.unmanaged,
            inv.counts.invalid
        ),
        (3, 3, 1, 2, 2)
    );
    let keys: Vec<&str> = inv.folders.iter().map(|e| e.key.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted, "sorted by key");
    assert!(
        inv.folders
            .iter()
            .all(|e| e.toc.is_none() && e.link_target.is_none())
    );
}

#[test]
fn full_depth_reads_tocs_and_judges_them() {
    let env = TestEnv::new();
    let root = BattlenetRoot::at(env.root().join("wow"))
        .product("wowf")
        .build();
    let target = battlenet_target(&root, "_forever_");
    let addons = target.client_dir.join("Interface/AddOns");
    addon_folder(
        &addons,
        "RetailOnly",
        &[(
            "RetailOnly_Mainline.toc",
            &toc("120105", &["## Title: Retail Only", "## Version: 1.0"]),
        )],
        &[],
    );
    addon_folder(
        &addons,
        "Both",
        &[
            ("Both_Mainline.toc", &toc("120105", &[])),
            (
                "Both-Forever.toc",
                &toc(
                    "16001",
                    &[
                        "## Title: Both",
                        "## Author: Someone",
                        "## X-Wago-ID: abc123",
                        "## X-Website: https://example.org",
                    ],
                ),
            ),
        ],
        &[],
    );
    let inv = inventory(&target, None, &RulesTable::builtin(), Depth::Full).unwrap();
    let retail = entry(&inv, "RetailOnly");
    let toc = retail.toc.as_ref().unwrap();
    assert_eq!(
        (
            toc.file.as_str(),
            toc.title.as_deref(),
            toc.version.as_deref()
        ),
        ("RetailOnly_Mainline.toc", Some("Retail Only"), Some("1.0"))
    );
    assert_eq!(toc.interfaces, [120_105]);
    let verdict = serde_json::to_value(&retail.verdict.as_ref().unwrap().verdict).unwrap();
    assert_eq!(verdict["verdict"], "wrong_flavor");
    let both = entry(&inv, "Both");
    let toc = both.toc.as_ref().unwrap();
    assert_eq!(toc.file, "Both-Forever.toc");
    assert_eq!(toc.x_ids.keys().collect::<Vec<_>>(), ["X-Wago-ID"]);
    assert_eq!(
        serde_json::to_value(&both.verdict.as_ref().unwrap().verdict).unwrap()["verdict"],
        "compatible"
    );
}

#[test]
fn classes_depth_opens_no_toc() {
    if running_as_root() {
        return;
    }
    let env = TestEnv::new();
    let root = BattlenetRoot::at(env.root().join("wow"))
        .product("wow")
        .build();
    let target = battlenet_target(&root, "_retail_");
    let addons = target.client_dir.join("Interface/AddOns");
    let dir = addon_folder(
        &addons,
        "Locked",
        &[("Locked.toc", &toc("120105", &[]))],
        &[],
    );
    fs::set_permissions(dir.join("Locked.toc"), fs::Permissions::from_mode(0o000)).unwrap();
    let classes = inventory(&target, None, &RulesTable::builtin(), Depth::Classes).unwrap();
    assert!(
        !entry(&classes, "Locked").unreadable,
        "Classes never opens the TOC"
    );
    let full = inventory(&target, None, &RulesTable::builtin(), Depth::Full).unwrap();
    assert!(entry(&full, "Locked").unreadable, "Full tried and failed");
    fs::set_permissions(dir.join("Locked.toc"), fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn missing_and_unlistable_addons_dirs() {
    let env = TestEnv::new();
    let root = BattlenetRoot::at(env.root().join("wow"))
        .product("wow")
        .partial("_retail_")
        .build();
    let target = battlenet_target(&root, "_retail_");
    let inv = inventory(&target, None, &RulesTable::builtin(), Depth::Full).unwrap();
    assert!(inv.addons_dir_missing && inv.folders.is_empty());
    assert!(
        !target.client_dir.join("Interface").exists(),
        "never created"
    );

    if !running_as_root() {
        let addons = target.client_dir.join("Interface/AddOns");
        fs::create_dir_all(&addons).unwrap();
        fs::set_permissions(&addons, fs::Permissions::from_mode(0o000)).unwrap();
        let err = inventory(&target, None, &RulesTable::builtin(), Depth::Classes).unwrap_err();
        fs::set_permissions(&addons, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!((err.code, err.exit().code()), (ErrorCode::Io, 1));
    }
}

#[test]
fn mixed_inventory_snapshot() {
    let env = TestEnv::new();
    let root = BattlenetRoot::at(env.root().join("wow"))
        .product("wow")
        .build();
    let target = battlenet_target(&root, "_retail_");
    let addons = target.client_dir.join("Interface/AddOns");
    addon_folder(
        &addons,
        "Blizzard_Console",
        &[(
            "Blizzard_Console.toc",
            &toc("120105", &["## Title: Console"]),
        )],
        &[],
    );
    addon_folder(
        &addons,
        "Details",
        &[(
            "Details.toc",
            &toc(
                "120105, 50504",
                &[
                    "## Title: Details!",
                    "## Version: 2.3",
                    "## X-Curse-Project-ID: 61284",
                ],
            ),
        )],
        &[],
    );
    addon_folder(
        &addons,
        "OldThing",
        &[("OldThing.toc", &toc("110200", &["## Title: Old"]))],
        &[],
    );
    addon_folder(&addons, "NoToc", &[], &[("readme.txt", b"hi")]);
    symlink("../../../../dev/WorkingCopy", addons.join("WorkingCopy")).unwrap();
    let rec = record(&[addon("Details", &["Details", "Details_DataStorage"])]);
    let inv = inventory(&target, Some(&rec), &RulesTable::builtin(), Depth::Full).unwrap();
    assert_eq!(
        inventory(&target, Some(&rec), &RulesTable::builtin(), Depth::Full).unwrap(),
        inv,
        "deterministic"
    );
    insta::assert_json_snapshot!("mixed_inventory", inv);
}

/// Classes depth counts 500 folders, the same on a second run (its 5 ms budget is a
/// release-build check in `tests/perf.rs`).
#[test]
fn classes_depth_counts_500_folders() {
    let env = TestEnv::new();
    let root = BattlenetRoot::at(env.root().join("wow"))
        .product("wow")
        .build();
    let target = battlenet_target(&root, "_retail_");
    let addons = target.client_dir.join("Interface/AddOns");
    for i in 0..500 {
        fs::create_dir_all(addons.join(format!("Addon{i:03}"))).unwrap();
    }
    let table = RulesTable::builtin();
    let first = inventory(&target, None, &table, Depth::Classes).unwrap();
    let inv = inventory(&target, None, &table, Depth::Classes).unwrap();
    assert_eq!(inv.counts.unmanaged, 500);
    assert_eq!(inv, first, "deterministic");
}

/// A folder only a client sharing this AddOns directory records is `shared`, naming
/// that client; one recorded on both stays `managed` and lists the other too.
#[test]
fn folders_another_sharing_client_records_are_shared() {
    let env = TestEnv::new();
    let root = BattlenetRoot::at(env.root().join("wow"))
        .product("wow")
        .build();
    let target = battlenet_target(&root, "_retail_");
    let addons = target.client_dir.join("Interface/AddOns");
    for name in ["Mine", "Theirs", "Both", "Loose"] {
        addon_folder(
            &addons,
            name,
            &[(&format!("{name}.toc"), &toc("120105", &[]))],
            &[],
        );
    }
    let mine = record(&[addon("Mine", &["Mine"]), addon("Both", &["Both"])]);
    let era = record(&[addon("Theirs", &["Theirs"]), addon("BothThere", &["Both"])]);
    let inv = omaddons::inventory::inventory_shared(
        &target,
        Some(&mine),
        &[("era".to_owned(), era)],
        &RulesTable::builtin(),
        Depth::Classes,
    )
    .unwrap();
    let class = |name: &str| inv.folders.iter().find(|e| e.name == name).unwrap();
    assert_eq!(class("Mine").class, FolderClass::Managed);
    assert!(class("Mine").owned_on.is_empty());
    assert_eq!(class("Theirs").class, FolderClass::Shared);
    assert_eq!(
        (
            class("Theirs").owned_on[0].target.as_str(),
            class("Theirs").owned_on[0].addon.as_str()
        ),
        ("era", "Theirs")
    );
    assert_eq!(class("Both").class, FolderClass::Managed);
    assert_eq!(class("Both").owned_on[0].addon, "BothThere");
    assert_eq!(class("Loose").class, FolderClass::Unmanaged);
    assert_eq!(
        (inv.counts.managed, inv.counts.shared, inv.counts.unmanaged),
        (2, 1, 1)
    );
}
