//! Each builder in `tests/common/` produces what it claims, checked against the real
//! parsers (BPSV, pelite, product.db, discovery) where they exist.

mod common;

use std::fs;

use common::addons::{addon_folder, toc};
use common::env::TestEnv;
use common::launchers::{self, Install, LutrisDir, Malformed};
use common::pe::{pe_stub, pe_stub_product_only, pe_versions};
use common::procfs::{FakeProc, running_as_root};
use common::product_db::ProductDb;
use common::trees::{
    BattlenetRoot, CustomRoot, LEGACY_VERSIONS, PRODUCTS, Prefix, listing, tree_hash,
};
use omaddons::discover::DiscoverySource;
use omaddons::discover::battlenet::{BattlenetHints, discover_root};
use omaddons::discover::bpsv::{Bpsv, flavor_product};
use omaddons::discover::dosdevices::map_windows_path;
use omaddons::discover::product_db::{ProductDbEntry, parse_product_db, read_product_db};
use omaddons::rules::{Family, RulesTable};

#[test]
fn pe_stubs_parse_with_pelite() {
    for &(major, minor, patch, build) in LEGACY_VERSIONS {
        let expected = [major, minor, patch, build];
        assert_eq!(
            pe_versions(&pe_stub(major, minor, patch, build)),
            (expected, expected)
        );
    }
    assert_eq!(
        pe_versions(&pe_stub_product_only(3, 3, 5, 12340)),
        ([0; 4], [3, 3, 5, 12340])
    );
}

#[test]
fn battlenet_roots_hold_every_product() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    let root = BattlenetRoot::at(prefix.wow_root()).all_products().build();
    let context = listing(&root);

    let text = fs::read_to_string(root.join(".build.info")).unwrap();
    let bpsv = Bpsv::parse(&text).unwrap();
    assert_eq!(bpsv.columns().len(), 15);
    assert_eq!(bpsv.rows().count(), PRODUCTS.len());
    for p in PRODUCTS {
        let dir = root.join(p.folder);
        let flavor = fs::read_to_string(dir.join(".flavor.info")).unwrap();
        assert_eq!(
            flavor_product(&flavor).as_deref(),
            Some(p.code),
            "{context}"
        );
        assert!(dir.join("Interface/AddOns").is_dir(), "{context}");
        assert!(dir.join("WTF").is_dir(), "{context}");
        let (file, _) = pe_versions(&fs::read(dir.join(p.exe)).unwrap());
        let parts: Vec<u16> = p
            .version
            .split('.')
            .take(3)
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(file[..3], parts[..], "{}", p.folder);
    }

    let before = tree_hash(&root);
    let (targets, warnings) = discover_root(
        &root,
        &RulesTable::builtin(),
        &BattlenetHints::default(),
        "battlenet",
        &DiscoverySource::Scan,
    );
    assert_eq!(targets.len(), PRODUCTS.len(), "{warnings:?}");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(targets.iter().all(|t| t.target.family != Family::Unknown));
    assert_eq!(tree_hash(&root), before, "discovery wrote nothing");

    assert_eq!(
        map_windows_path(prefix.path(), "C:/Program Files (x86)/World of Warcraft"),
        Some(root.clone())
    );
}

#[test]
fn battlenet_root_options() {
    let env = TestEnv::new();
    let root = BattlenetRoot::at(env.home().join("wow"))
        .product("wow")
        .product("wowf")
        .without_flavor_info("_forever_")
        .unknown_folder("_xyz_", Some("wowz"), Some("13.0.0.1"))
        .unknown_folder("_bare_", None, None)
        .last_addon_version("_retail_", 120_100)
        .partial("_forever_")
        .exe_stub(false)
        .build();
    let context = listing(&root);
    assert!(!root.join("_forever_/.flavor.info").exists(), "{context}");
    assert!(
        !root.join("_forever_/Wow.exe").exists(),
        "partial: {context}"
    );
    assert!(
        !root.join("_forever_/Interface").exists(),
        "partial: {context}"
    );
    assert_eq!(
        fs::read(root.join("_retail_/WoW.exe")).unwrap().len(),
        0,
        "no stub"
    );
    let config = fs::read_to_string(root.join("_retail_/WTF/Config.wtf")).unwrap();
    assert!(config.contains("SET lastAddonVersion \"120100\""));
    let xyz = fs::read_to_string(root.join("_xyz_/.flavor.info")).unwrap();
    assert_eq!(flavor_product(&xyz).as_deref(), Some("wowz"));
    assert!(!root.join("_bare_/.flavor.info").exists());
    let rows = omaddons::discover::bpsv::build_info_rows(
        &fs::read_to_string(root.join(".build.info")).unwrap(),
    )
    .unwrap();
    let products: Vec<_> = rows.iter().filter_map(|r| r.product.as_deref()).collect();
    assert_eq!(products, ["wow", "wowf", "wowz"]);

    let old = BattlenetRoot::at(env.home().join("old"))
        .product("wow_classic_era")
        .old_build_info_layout()
        .build();
    let bpsv = Bpsv::parse(&fs::read_to_string(old.join(".build.info")).unwrap()).unwrap();
    assert_eq!(bpsv.columns().len(), 13);
    assert_eq!(bpsv.column("Product"), None);
    assert_eq!(
        bpsv.rows().next().unwrap().get("Version"),
        Some("1.15.9.64520")
    );
}

#[test]
fn custom_roots() {
    let env = TestEnv::new();
    let root = CustomRoot::at(env.home().join("Games/wotlk"))
        .blizzard_folders(23)
        .last_addon_version(30_300)
        .build();
    let context = listing(&root);
    assert_eq!(
        pe_versions(&fs::read(root.join("Wow.exe")).unwrap()).0,
        [3, 3, 5, 12340]
    );
    assert_eq!(
        fs::read_to_string(root.join("Data/enUS/realmlist.wtf")).unwrap(),
        "set realmlist 127.0.0.1\n"
    );
    let blizzard: Vec<_> = fs::read_dir(root.join("Interface/AddOns"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("Blizzard_"))
        .collect();
    assert_eq!(blizzard.len(), 23, "{context}");
    let toc =
        fs::read_to_string(root.join("Interface/AddOns/Blizzard_AuctionUI/Blizzard_AuctionUI.toc"))
            .unwrap();
    assert!(toc.starts_with("## Interface: 30300\n"), "{toc}");
    assert!(
        fs::read_to_string(root.join("WTF/Config.wtf"))
            .unwrap()
            .contains("\"30300\"")
    );

    for &(major, minor, patch, build) in LEGACY_VERSIONS {
        let dir = env.home().join(format!("legacy-{major}{minor}{patch}"));
        let root = CustomRoot::at(dir)
            .version(major, minor, patch, build)
            .realmlist("logon.example.org")
            .build();
        assert_eq!(
            pe_versions(&fs::read(root.join("Wow.exe")).unwrap()).0,
            [major, minor, patch, build]
        );
        assert!(
            fs::read_to_string(root.join("Data/enUS/realmlist.wtf"))
                .unwrap()
                .contains("logon.example.org")
        );
    }
    let bare = CustomRoot::at(env.home().join("bare"))
        .no_realmlist()
        .exe("WoW.EXE")
        .build();
    assert!(!bare.join("Data/enUS/realmlist.wtf").exists());
    assert!(bare.join("WoW.EXE").is_file());
}

#[test]
fn prefixes_and_drives() {
    let env = TestEnv::new();
    let outside = env.root().join("games-drive");
    let prefix = Prefix::new(&env, "battlenet")
        .with_drive('d', &outside)
        .with_dirs(&["windows", "users/x", "ProgramData"]);
    assert_eq!(prefix.path(), env.home().join("Games/battlenet"));
    assert!(prefix.drive_c().join("users/x").is_dir());
    BattlenetRoot::at(outside.join("World of Warcraft"))
        .product("wow")
        .build();
    assert_eq!(
        map_windows_path(prefix.path(), "D:\\World of Warcraft\\_retail_"),
        Some(outside.join("World of Warcraft/_retail_"))
    );
    assert_eq!(map_windows_path(prefix.path(), "Z:/"), Some("/".into()));
}

#[test]
fn product_db_round_trips_through_the_reader() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    let db = ProductDb::new()
        .with_entry_for("agent", "C:/ProgramData/Battle.net/Agent")
        .install(
            "wow",
            "C:/Program Files (x86)/World of Warcraft",
            "_retail_",
            "12.1.0.69933",
        )
        .install(
            "wowf",
            "D:/Games/World of Warcraft",
            "_forever_",
            "1.60.1.70245",
        );
    let path = db.write(prefix.path());
    let bytes = fs::read(&path).unwrap();
    let needle = b"C:/Program Files (x86)/World of Warcraft";
    assert!(bytes.windows(needle.len()).any(|w| w == needle));
    let entries = read_product_db(prefix.path());
    assert_eq!(entries, parse_product_db(&bytes));
    assert_eq!(
        entries,
        [
            ProductDbEntry {
                product: Some("wow".into()),
                install_path: "C:/Program Files (x86)/World of Warcraft".into(),
                flavor_folder: Some("_retail_".into()),
                version: Some("12.1.0.69933".into()),
            },
            ProductDbEntry {
                product: Some("wowf".into()),
                install_path: "D:/Games/World of Warcraft".into(),
                flavor_folder: Some("_forever_".into()),
                version: Some("1.60.1.70245".into()),
            },
        ]
    );
}

#[test]
fn launcher_files_land_at_their_documented_locations() {
    let env = TestEnv::new();
    let home = env.home();
    let prefix = home.join("Games/battlenet");
    let exe = home.join("Games/wotlk/Wow.exe");

    let lutris = launchers::lutris_game(
        &env,
        LutrisDir::Config,
        "world-of-warcraft",
        Some(&exe),
        Some(&prefix),
    );
    assert_eq!(
        lutris,
        env.config().join("lutris/games/world-of-warcraft.yml")
    );
    let yaml = fs::read_to_string(&lutris).unwrap();
    assert!(
        yaml.contains(&format!("exe: {}", exe.display())) && yaml.contains("prefix: '"),
        "{yaml}"
    );
    let data = launchers::lutris_game(&env, LutrisDir::Data, "wotlk", Some(&exe), None);
    assert_eq!(data, env.data().join("lutris/games/wotlk.yml"));

    let heroic = launchers::heroic_game(&env, Install::Native, "wow", &prefix);
    assert_eq!(heroic, env.config().join("heroic/GamesConfig/wow.json"));
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&heroic).unwrap()).unwrap();
    assert_eq!(json["wow"]["winePrefix"], prefix.display().to_string());
    let flatpak = launchers::heroic_game(&env, Install::Flatpak, "wow", &prefix);
    assert_eq!(
        flatpak,
        home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic/GamesConfig/wow.json")
    );

    let library = env.root().join("mnt/SteamLibrary");
    let vdf = launchers::steam_libraries(&env, Install::Native, &[&library]);
    assert_eq!(vdf, env.data().join("Steam/steamapps/libraryfolders.vdf"));
    let text = fs::read_to_string(&vdf).unwrap();
    assert!(
        text.contains(&format!("\"path\"\t\t\"{}\"", library.display())),
        "{text}"
    );
    assert!(text.contains(&format!(
        "\"path\"\t\t\"{}\"",
        env.data().join("Steam").display()
    )));
    let pfx = launchers::steam_prefix(&library, 2_000_001);
    assert!(pfx.join("drive_c").is_dir());
    let flatpak_vdf = launchers::steam_libraries(&env, Install::Flatpak, &[]);
    assert_eq!(
        flatpak_vdf,
        home.join(
            ".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/libraryfolders.vdf"
        )
    );

    let bottle = launchers::bottle(&env, Install::Native, "wow", None);
    assert_eq!(bottle, env.data().join("bottles/bottles/wow"));
    assert!(bottle.join("drive_c").is_dir());
    assert!(
        fs::read_to_string(bottle.join("bottle.yml"))
            .unwrap()
            .contains("Path: wow\n")
    );
    let custom = env.root().join("elsewhere");
    let flatpak_bottle = launchers::bottle(&env, Install::Flatpak, "wow2", Some(&custom));
    assert_eq!(
        flatpak_bottle,
        home.join(".var/app/com.usebottles.bottles/data/bottles/bottles/wow2")
    );
    assert!(
        fs::read_to_string(flatpak_bottle.join("bottle.yml"))
            .unwrap()
            .contains(&format!("Path: {}\n", custom.display()))
    );

    let full = fs::read(&lutris).unwrap().len();
    launchers::malform(&lutris, Malformed::Truncated);
    assert_eq!(fs::read(&lutris).unwrap().len(), full / 2);
    launchers::malform(&heroic, Malformed::Binary);
    assert!(String::from_utf8(fs::read(&heroic).unwrap()).is_err());
    launchers::malform(&vdf, Malformed::Empty);
    assert!(fs::read(&vdf).unwrap().is_empty());
}

#[test]
fn fake_proc_entries_read_back() {
    let env = TestEnv::new();
    let proc = FakeProc::new(env.root().join("proc"))
        .process(
            100,
            true,
            "Wow.exe",
            &[
                "C:\\Program Files (x86)\\World of Warcraft\\_retail_\\Wow.exe",
                "-launch",
            ],
            &[("WINEPREFIX", "/games/battlenet"), ("LANG", "C")],
        )
        .process(200, false, "WowClassic.exe", &["WowClassic.exe"], &[]);
    let pid = proc.root().join("100");
    assert_eq!(fs::read_to_string(pid.join("comm")).unwrap(), "Wow.exe\n");
    assert_eq!(
        fs::read(pid.join("cmdline")).unwrap(),
        b"C:\\Program Files (x86)\\World of Warcraft\\_retail_\\Wow.exe\0-launch\0"
    );
    assert_eq!(
        fs::read(pid.join("environ")).unwrap(),
        b"WINEPREFIX=/games/battlenet\0LANG=C\0"
    );
    if !running_as_root() {
        assert!(
            fs::read(proc.root().join("200/comm")).is_err(),
            "another user's process is unreadable"
        );
    }
}

#[test]
fn addon_helpers() {
    let env = TestEnv::new();
    let addons = env.home().join("AddOns");
    let text = toc("120105, 50504", &["## Title: Demo", "Demo.lua"]);
    assert_eq!(
        text,
        "## Interface: 120105, 50504\n## Title: Demo\nDemo.lua\n"
    );
    let dir = addon_folder(
        &addons,
        "Demo",
        &[("Demo.toc", &text)],
        &[("libs/Lib.lua", b"-- lib")],
    );
    assert_eq!(fs::read_to_string(dir.join("Demo.toc")).unwrap(), text);
    assert_eq!(fs::read(dir.join("libs/Lib.lua")).unwrap(), b"-- lib");
}

#[test]
fn tree_hash_tracks_bytes_names_and_links() {
    let env = TestEnv::new();
    let dir = env.root().join("tree");
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/file"), "abc").unwrap();
    std::os::unix::fs::symlink("target-a", dir.join("link")).unwrap();
    let base = tree_hash(&dir);
    assert_eq!(tree_hash(&dir), base, "stable");

    fs::write(dir.join("sub/file"), "abd").unwrap();
    let changed = tree_hash(&dir);
    assert_ne!(changed, base, "a byte");
    fs::write(dir.join("sub/file"), "abc").unwrap();
    assert_eq!(tree_hash(&dir), base, "restored");

    fs::rename(dir.join("sub/file"), dir.join("sub/renamed")).unwrap();
    assert_ne!(tree_hash(&dir), base, "a name");
    fs::rename(dir.join("sub/renamed"), dir.join("sub/file")).unwrap();

    fs::remove_file(dir.join("link")).unwrap();
    std::os::unix::fs::symlink("target-b", dir.join("link")).unwrap();
    assert_ne!(tree_hash(&dir), base, "a symlink target");

    fs::remove_file(dir.join("link")).unwrap();
    std::os::unix::fs::symlink("target-a", dir.join("link")).unwrap();
    fs::create_dir(dir.join("empty")).unwrap();
    assert_ne!(tree_hash(&dir), base, "a new directory");
}

#[test]
fn builders_are_deterministic() {
    let build = |env: &TestEnv| {
        let prefix = Prefix::new(env, "battlenet");
        BattlenetRoot::at(prefix.wow_root())
            .all_products()
            .unknown_folder("_xyz_", Some("wowz"), Some("13.0.0.1"))
            .last_addon_version("_retail_", 120_100)
            .build();
        CustomRoot::at(env.home().join("Games/wotlk"))
            .blizzard_folders(23)
            .build();
        ProductDb::new()
            .install(
                "wow",
                "C:/Program Files (x86)/World of Warcraft",
                "_retail_",
                "12.1.0.69933",
            )
            .write(prefix.path());
        tree_hash(&env.home())
    };
    let (a, b) = (TestEnv::new(), TestEnv::new());
    assert_eq!(build(&a), build(&b));
}

#[test]
fn the_binary_never_touches_the_real_home() {
    let env = TestEnv::new();
    let real_home = tempfile::tempdir().unwrap();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_omaddons"));
    cmd.env("HOME", real_home.path())
        .env("XDG_CONFIG_HOME", real_home.path().join(".config"))
        .env("XDG_STATE_HOME", real_home.path().join(".local/state"))
        .env("XDG_CACHE_HOME", real_home.path().join(".cache"))
        .arg("--json");
    env.apply_to(&mut cmd);
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read_dir(real_home.path()).unwrap().count(), 0);

    let assert = env.command().arg("--json").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains(r#""command":"help""#), "{stdout}");
    for dir in [env.home(), env.config(), env.state(), env.cache()] {
        assert!(dir.starts_with(env.root()));
    }
    let state = env.state();
    assert_eq!(
        fs::read_dir(&state).unwrap().count(),
        0,
        "{}",
        listing(&state)
    );
}
