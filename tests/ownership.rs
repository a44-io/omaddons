//! The M1 checkpoint: on a 3.3.5a client shipping 23 `Blizzard_*` folders, an archive
//! holding `blizzard_auctionui/` is refused before anything is written, with or
//! without `--force`.

use crate::common;

use std::io::Cursor;

use common::archives::zip_text;
use common::env::TestEnv;
use common::trees::{CustomRoot, tree_hash};
use omaddons::discover::DiscoverySource;
use omaddons::discover::custom::discover_custom_root;
use omaddons::engine::inspect::{ArchiveLimits, inspect_zip};
use omaddons::engine::ownership::{AdoptFix, FolderChange, check};
use omaddons::error::ErrorCode;
use omaddons::inventory::{Depth, inventory};
use omaddons::model::Ref;
use omaddons::rules::RulesTable;

#[test]
fn blizzard_folders_are_never_planned_for_mutation() {
    let env = TestEnv::new();
    let root = CustomRoot::at(env.home().join("Games/wow-335"))
        .version(3, 3, 5, 12_340)
        .blizzard_folders(23)
        .build();
    let table = RulesTable::builtin();
    let (found, _) = discover_custom_root(&root, &table, "custom", &DiscoverySource::Scan);
    let target = found.unwrap().target;
    let addons = target.addons_dir();
    assert!(addons.join("Blizzard_AuctionUI").is_dir());
    let before = tree_hash(&addons);

    let zip = zip_text(&[
        (
            "blizzard_auctionui/blizzard_auctionui.toc",
            "## Interface: 30300\n",
        ),
        ("MyAddon/MyAddon.toc", "## Interface: 30300\n"),
    ]);
    let inspected = inspect_zip(Cursor::new(zip), &ArchiveLimits::default(), &table).unwrap();
    let changes: Vec<FolderChange> = inspected
        .folders
        .iter()
        .map(|f| FolderChange::Install(f.name.clone()))
        .collect();
    let live = inventory(&target, None, &table, Depth::Classes).unwrap();
    let argv: Vec<String> = ["omaddons", "install", "./pack.zip"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    // Never adopted either, for an addon new to the client.
    let r: Ref = "gh:o/pack".parse().unwrap();
    let adopt = AdoptFix {
        target: "custom",
        r#ref: &r,
    };
    for (force, adopt) in [(false, None), (true, None), (false, Some(adopt))] {
        let err = check(&changes, &live, "pack", force, &argv, adopt).unwrap_err();
        assert_eq!(
            (err.code, err.exit().code()),
            (ErrorCode::ProtectedFolder, 4),
            "force={force}"
        );
        assert_eq!(err.details["folders"][0]["folder"], "blizzard_auctionui");
        assert_eq!(err.fix.argv(), None, "no override exists");
        assert!(err.alternatives.is_empty());
    }
    assert_eq!(tree_hash(&addons), before, "nothing written");
}
