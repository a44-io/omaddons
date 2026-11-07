//! Custom and legacy client discovery over synthetic roots with real PE stubs.

use crate::common;

use std::fs;
use std::path::Path;

use common::env::TestEnv;
use common::pe::{pe_stub, pe_stub_product_only, pe_stub_with};
use common::trees::{BattlenetRoot, CustomRoot, LEGACY_VERSIONS, tree_hash, tree_stat};
use omaddons::discover::custom::is_custom_root;
use omaddons::discover::pe::exe_version;
use omaddons::error::WarningCode;
use omaddons::rules::{ClientVersion, Family, InstallKind, TargetKind};
use support::{discover, found};

#[cfg(test)]
mod support {
    use std::path::Path;

    use omaddons::discover::custom::discover_custom_root;
    use omaddons::discover::{DiscoveredTarget, DiscoverySource};
    use omaddons::error::Warning;
    use omaddons::rules::RulesTable;

    use crate::common::trees::listing;

    pub fn discover(root: &Path) -> (Option<DiscoveredTarget>, Vec<Warning>) {
        discover_custom_root(
            root,
            &RulesTable::builtin(),
            "custom",
            &DiscoverySource::Scan,
        )
    }

    /// The target of `root`, which must be found without warnings; logs the target,
    /// the warnings and the tree otherwise.
    pub fn found(root: &Path) -> DiscoveredTarget {
        let (target, warnings) = discover(root);
        let context = format!(
            "{}\n{}\n{}",
            serde_json::to_string_pretty(&target).unwrap(),
            serde_json::to_string_pretty(&warnings).unwrap(),
            listing(root)
        );
        assert!(warnings.is_empty(), "{context}");
        assert!(target.is_some(), "no target\n{context}");
        target.unwrap()
    }
}

#[test]
fn every_legacy_version() {
    let env = TestEnv::new();
    let expected = [
        (Family::VanillaLegacy, 11_200, "vanilla"),
        (Family::TbcLegacy, 20_400, "tbc"),
        (Family::WrathLegacy, 30_300, "wotlk"),
        (Family::CataLegacy, 40_300, "cata"),
        (Family::MistsLegacy, 50_400, "mists"),
    ];
    for (&(major, minor, patch, build), (family, interface, id)) in
        LEGACY_VERSIONS.iter().zip(expected)
    {
        let root = CustomRoot::at(env.home().join(format!("Games/{id}")))
            .version(major, minor, patch, build)
            .build();
        let before = tree_hash(&root);
        let d = found(&root);
        let t = &d.target;
        assert_eq!(
            (t.family, t.interface, t.id.as_str()),
            (family, interface, id),
            "{major}.{minor}.{patch}"
        );
        assert_eq!(d.default_id, id);
        assert_eq!(
            t.version,
            ClientVersion {
                major: major.into(),
                minor: minor.into(),
                patch: patch.into(),
                build: Some(build.into())
            }
        );
        assert_eq!(
            (
                t.install_kind,
                t.kind,
                t.product.as_deref(),
                t.flavor_dir.as_deref()
            ),
            (InstallKind::Custom, TargetKind::Live, None, None)
        );
        assert_eq!(
            (t.install_root.as_path(), t.client_dir.as_path()),
            (root.as_path(), root.as_path())
        );
        assert_eq!(t.exe_name.as_deref(), Some("Wow.exe"));
        assert!(t.rules.legacy && !t.rules.interface_includes_patch, "{id}");
        assert_eq!(t.label.as_deref(), Some("localhost"));
        assert_eq!(tree_hash(&root), before, "discovery wrote nothing");
    }
}

#[test]
fn labels_come_from_the_realmlist() {
    let env = TestEnv::new();
    let local = CustomRoot::at(env.home().join("Games/wow-335"))
        .blizzard_folders(23)
        .build();
    assert_eq!(found(&local).target.label.as_deref(), Some("localhost"));
    let remote = CustomRoot::at(env.home().join("Games/remote"))
        .realmlist("logon.example.org")
        .build();
    assert_eq!(
        found(&remote).target.label.as_deref(),
        Some("logon.example.org")
    );
    let none = CustomRoot::at(env.home().join("Games/My Server"))
        .no_realmlist()
        .build();
    assert_eq!(found(&none).target.label.as_deref(), Some("My Server"));
    fs::write(
        none.join("Data/enUS/realmlist.wtf"),
        "SET REALMLIST \"quoted.example.org\"\r\n",
    )
    .unwrap();
    assert_eq!(
        found(&none).target.label.as_deref(),
        Some("quoted.example.org")
    );
}

#[test]
fn last_addon_version_overrides_the_legacy_interface() {
    let env = TestEnv::new();
    let same = CustomRoot::at(env.home().join("same"))
        .last_addon_version(30_300)
        .build();
    assert_eq!(found(&same).target.interface, 30_300);
    let other = CustomRoot::at(env.home().join("other"))
        .last_addon_version(30_000)
        .build();
    assert_eq!(
        found(&other).target.interface,
        30_000,
        "the client's own value wins"
    );
}

/// Turtle WoW's client is 1.12 with its own version stamp (1.17.x, 1.18.1 build 7272):
/// `vanilla-legacy` and its rules, at interface 11200, whose addons declare; its own
/// `lastAddonVersion` still wins.
#[test]
fn builds_of_the_1_12_client_that_stamp_their_own_version_are_vanilla_legacy() {
    let env = TestEnv::new();
    for (major, minor, patch, build) in [(1, 18, 1, 7272), (1, 17, 2, 7234)] {
        let root = CustomRoot::at(env.home().join(format!("Games/turtle-{minor}")))
            .version(major, minor, patch, build)
            .build();
        let d = found(&root);
        let t = &d.target;
        let what = format!("{major}.{minor}.{patch}");
        assert_eq!(
            (t.family, t.interface, t.rules.name.as_str()),
            (Family::VanillaLegacy, 11_200, "vanilla-legacy"),
            "{what}"
        );
        assert!(t.rules.legacy && !t.rules.multi_toc, "{what}");
        assert_eq!(d.default_id, "vanilla", "{what}");
    }
    let played = CustomRoot::at(env.home().join("Games/turtle-played"))
        .version(1, 18, 1, 7272)
        .last_addon_version(11_201)
        .build();
    assert_eq!(found(&played).target.interface, 11_201);
}

#[test]
fn exe_names_and_versions() {
    let env = TestEnv::new();
    for name in ["WOW.EXE", "wow.exe", "Wow.exe"] {
        let root = CustomRoot::at(env.home().join(name)).exe(name).build();
        assert_eq!(found(&root).target.exe_name.as_deref(), Some(name));
    }
    let both = CustomRoot::at(env.home().join("mop"))
        .version(5, 4, 8, 18414)
        .build();
    fs::write(both.join("Wow-64.exe"), pe_stub(5, 4, 8, 18414)).unwrap();
    assert_eq!(
        found(&both).target.exe_name.as_deref(),
        Some("Wow.exe"),
        "Wow.exe before Wow-64.exe"
    );
    fs::remove_file(both.join("Wow.exe")).unwrap();
    assert_eq!(found(&both).target.exe_name.as_deref(), Some("Wow-64.exe"));

    let product_only = env.home().join("product.exe");
    fs::write(&product_only, pe_stub_product_only(3, 3, 5, 12340)).unwrap();
    assert_eq!(
        exe_version(&product_only).map(|v| v.to_string()).as_deref(),
        Some("3.3.5.12340")
    );
    let like_real = env.home().join("file.exe");
    fs::write(&like_real, pe_stub_with([3, 3, 5, 12340], [3, 3, 0, 0])).unwrap();
    assert_eq!(
        exe_version(&like_real).map(|v| v.to_string()).as_deref(),
        Some("3.3.5.12340"),
        "the file version wins over a real client's 3.3.0.0 product version"
    );
}

#[test]
fn what_is_not_a_custom_root() {
    let env = TestEnv::new();
    let crash_only = env.home().join("crash");
    fs::create_dir_all(crash_only.join("Interface/AddOns")).unwrap();
    fs::write(crash_only.join("WowError.exe"), pe_stub(3, 3, 5, 12340)).unwrap();
    assert!(!is_custom_root(&crash_only));
    assert!(discover(&crash_only).0.is_none());

    let no_interface = env.home().join("bare");
    fs::create_dir_all(&no_interface).unwrap();
    fs::write(no_interface.join("Wow.exe"), pe_stub(3, 3, 5, 12340)).unwrap();
    assert!(!is_custom_root(&no_interface));

    let battlenet = BattlenetRoot::at(env.home().join("bnet"))
        .product("wow")
        .build();
    fs::write(battlenet.join("Wow.exe"), pe_stub(12, 1, 5, 0)).unwrap();
    fs::create_dir_all(battlenet.join("Interface")).unwrap();
    assert!(
        !is_custom_root(&battlenet),
        "a .build.info root is never custom"
    );
    assert!(
        !is_custom_root(&battlenet.join("_retail_")),
        "a flavor folder has an exe and Interface/ but is never custom"
    );
    assert!(discover(&battlenet.join("_retail_")).0.is_none());
}

#[test]
fn a_non_pe_exe_is_usable_and_warns() {
    let env = TestEnv::new();
    let root = CustomRoot::at(env.home().join("broken")).build();
    fs::write(root.join("Wow.exe"), b"MZ but nothing else").unwrap();
    let (target, warnings) = discover(&root);
    let t = target.unwrap().target;
    assert_eq!(
        (t.family, t.version),
        (Family::Unknown, ClientVersion::new(0, 0, 0))
    );
    assert_eq!(t.id, "broken");
    // Said once overrides applied (World), not by discovery.
    assert!(warnings.is_empty(), "{warnings:?}");
    let warning = omaddons::discover::unknown_flavor(&t).unwrap();
    assert_eq!(warning.code, WarningCode::UnknownFlavor);
    assert_eq!(
        warning.message,
        "broken: Wow.exe has no readable version; it is usable with conservative legacy rules"
    );
}

/// The development machine's own custom client (vision §4.7), when its path is given:
/// `OMADDONS_REAL_CUSTOM_ROOT=<root> cargo test --test integration discover_custom:: -- --ignored`.
#[test]
#[ignore = "needs a real custom client"]
fn real_custom_client() {
    let Some(root) = std::env::var_os("OMADDONS_REAL_CUSTOM_ROOT") else {
        common::checkpoint::skip("real_custom_client", "OMADDONS_REAL_CUSTOM_ROOT is not set");
        return;
    };
    let root = Path::new(&root);
    let before = tree_stat(root);
    let t = found(root).target;
    assert_eq!(
        (t.family, t.interface, t.label.as_deref()),
        (Family::WrathLegacy, 30_300, Some("localhost"))
    );
    assert_eq!(tree_stat(root), before, "nothing written");
}
