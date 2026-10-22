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

    let bottle = launchers::bottle(&env, Install::Native, "wow");
    assert_eq!(bottle, env.data().join("bottles/bottles/wow"));
    assert!(bottle.join("drive_c").is_dir());
    assert!(
        fs::read_to_string(bottle.join("bottle.yml"))
            .unwrap()
            .contains("Path: wow\n")
    );
    let moved = env.root().join("Bottles");
    let data = launchers::bottles_data(&env, Install::Flatpak, &moved);
    assert_eq!(
        data,
        home.join(".var/app/com.usebottles.bottles/data/bottles/data.yml")
    );
    assert_eq!(
        fs::read_to_string(&data).unwrap(),
        format!("custom_bottles_path: {}\n", moved.display())
    );
    assert_eq!(launchers::bottle_in(&moved, "w"), moved.join("w"));
    let custom = env.root().join("elsewhere");
    let placeholder = launchers::custom_path_bottle(&env, Install::Flatpak, "wow2", &custom);
    assert_eq!(
        placeholder,
        home.join(".var/app/com.usebottles.bottles/data/bottles/bottles/wow2/placeholder.yml")
    );
    assert_eq!(
        fs::read_to_string(&placeholder).unwrap(),
        format!("Path: {}\n", custom.display())
    );
    assert!(custom.join("drive_c").is_dir());
    let real = fs::read_to_string(custom.join("bottle.yml")).unwrap();
    assert!(
        real.contains(&format!("Path: {}\nCustom_Path: true\n", custom.display())),
        "{real}"
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
fn wtf_trees_and_seeded_backups() {
    use common::backups::{backup_dir, seed_backup, seed_many};
    use common::wtf::{Wtf, writable_again};
    use omaddons::backup::{self, BackupKind};
    use omaddons::state::paths::ClientPaths;

    let env = TestEnv::new();
    let client = env.home().join("Games/wow/_retail_");
    let wtf = Wtf::at(&client)
        .config_wtf("SET locale enUS\n")
        .saved_variables("WeakAuras", 64)
        .account("123456#1")
        .realm("Other")
        .character("Second")
        .saved_variables("Details", 10)
        .symlink("Config.link", "Config.wtf")
        .file("Account/123456#1/bindings-cache.wtf", b"bind")
        .mode("Account/123456#1/Other", 0o500)
        .build();
    let sv = |rel: &str| fs::read(wtf.join(rel)).unwrap().len();
    assert_eq!(sv("Account/FAKEACCT/SavedVariables/WeakAuras.lua"), 64);
    assert_eq!(
        sv("Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/WeakAuras.lua"),
        64
    );
    assert_eq!(
        sv("Account/123456#1/Other/Second/SavedVariables/Details.lua"),
        10
    );
    assert_eq!(
        fs::read_link(wtf.join("Config.link")).unwrap(),
        std::path::Path::new("Config.wtf")
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(wtf.join("Account/123456#1/Other"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o500);
    let big = Wtf::at(env.root().join("big")).big(10_000, 20).build();
    let count = files::walk_files(&big);
    assert_eq!(count, (20, 10_000));
    writable_again(&wtf);

    let at: jiff::Timestamp = "2026-10-20T18:02:00Z".parse().unwrap();
    let id = seed_backup(&client, BackupKind::Auto, at, 3);
    assert!(backup_dir(&client, &id).join("WTF").is_dir());
    let many = seed_many(&client, 3);
    let (listed, warnings) = backup::list(&ClientPaths::new(&client));
    assert!(warnings.is_empty());
    let ids: Vec<_> = listed.iter().map(|b| b.id).collect();
    assert_eq!(ids, [vec![id], many].concat(), "newest capture first");
    assert!(listed.iter().all(|b| b.restorable && b.qualifying));
    assert_eq!(listed[0].meta.as_ref().unwrap().files, 3);
    assert_eq!(listed[0].addons().unwrap().addons[0].name, "WeakAuras");
    assert!(backup::check_structure(&listed[0]).unwrap().complete());
}

#[cfg(test)]
mod files {
    use std::fs;

    /// Files below `dir` and their total size.
    pub fn walk_files(dir: &std::path::Path) -> (u64, u64) {
        let mut found = (0, 0);
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                let (n, bytes) = walk_files(&entry.path());
                found = (found.0 + n, found.1 + bytes);
            } else {
                found = (found.0 + 1, found.1 + entry.metadata().unwrap().len());
            }
        }
        found
    }
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

/// `OMADDONS_ALLOW_SKIP` names tests exactly, in a comma list; a prefix of a name, an
/// empty value or no value accepts nothing.
#[test]
fn skip_allowed_takes_exact_names_only() {
    use common::checkpoint::skip_allowed;

    let allowed = |allow: Option<&str>, test: &str| skip_allowed(allow, test);
    assert!(!allowed(None, "a"));
    assert!(!allowed(Some(""), "a"));
    assert!(allowed(Some("a"), "a"));
    assert!(allowed(Some("a, b"), "b"));
    assert!(allowed(Some(" a ,b"), "a"));
    assert!(allowed(Some("x,,y"), "y"));
    assert!(!allowed(Some("x,,y"), "z"));
    assert!(!allowed(
        Some("keyless_installs"),
        "keyless_installs_forever"
    ));
    assert!(!allowed(
        Some("keyless_installs_forever"),
        "keyless_installs"
    ));
}

/// The M2 helpers: HTTP sequences and releases, homes behind, drift, state readers,
/// `net.log` counts and release graphs.
mod m2 {
    use std::fs;
    use std::io::Read;

    use omaddons::sources::github::GithubSource;
    use omaddons::toc::parse_toc;
    use support::{FOO, foo_asset, foo_assets, get, newest};

    use super::common::addons::addon_folder;
    use super::common::drift::{add_stray_file, delete_managed, edit_managed, touch_identical};
    use super::common::env::TestEnv;
    use super::common::graph::{Folder, Node, release_graph};
    use super::common::home::Home;
    use super::common::http::{
        Resp, fixture_seq, gh_feed, gh_latest, gh_release, gh_release_v, paths, replay,
    };
    use super::common::netlog::net_log;
    use super::common::state::{files_index, history, key_of, record};
    use super::common::trees::{CustomRoot, tree_hash};

    #[cfg(test)]
    mod support {
        use omaddons::net::{Http, Request};
        use omaddons::sources::{SourceConfigView, SourceRegistry, Wanted, fetch_once};

        use super::super::common::addons::toc;
        use super::super::common::http::Asset;

        pub const FOO: &str = "https://github.com/o/foo";

        pub fn get(http: &dyn Http, url: &str) -> (u16, Vec<u8>) {
            let resp = http.send(Request::get(url, "selftest")).unwrap();
            (resp.status, resp.bytes().unwrap())
        }

        /// The newest release id of `r` through the production GitHub source.
        pub fn newest(http: &dyn Http, r: &str) -> String {
            let w = Wanted::stable(r.parse().unwrap());
            let mut results = fetch_once(
                [w],
                &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
                http,
            );
            let set = results.pop_first().unwrap().1.unwrap();
            set.releases[0].release_id.clone()
        }

        /// The TOC text and core.lua contents of gh:o/foo at `version`.
        pub fn foo_assets(version: &str) -> (String, String) {
            let text = toc(
                "120105",
                &["## Title: Foo", &format!("## Version: {version}")],
            );
            (text, version.to_owned())
        }

        /// One mainline asset of gh:o/foo from [`foo_assets`].
        pub fn foo_asset<'a>(text: &'a str, version: &'a str) -> [Asset<'a>; 1] {
            const MAINLINE: &[(&str, u32)] = &[("mainline", 120_105)];
            [(
                "Foo.zip",
                MAINLINE,
                vec![("Foo/Foo.toc", text), ("Foo/core.lua", version)],
            )]
        }
    }

    #[test]
    fn fixture_seq_serves_in_order_with_the_last_repeating() {
        let env = TestEnv::new();
        let scenario = env.root().join("http");
        let url = "https://github.com/o/foo/releases/download/v1/notes.txt";
        fixture_seq(
            &scenario,
            "notes",
            url,
            &[
                Resp::ok("one"),
                Resp::status(404),
                Resp::ok("two").header("ETag", "\"2\""),
            ],
        );
        let http = replay(&env, &scenario);
        let served: Vec<(u16, Vec<u8>)> = (0..4).map(|_| get(http.as_ref(), url)).collect();
        assert_eq!(
            served,
            [
                (200, b"one".to_vec()),
                (404, Vec::new()),
                (200, b"two".to_vec()),
                (200, b"two".to_vec()),
            ]
        );
        // A new process (a new replay) starts the sequence again.
        assert_eq!(get(replay(&env, &scenario).as_ref(), url).1, b"one");
    }

    #[test]
    fn two_tags_of_one_repo_coexist_and_latest_is_repointed() {
        let env = TestEnv::new();
        let scenario = env.root().join("http");
        let (one, two) = (foo_assets("1.0"), foo_assets("2.0"));
        gh_release(&scenario, FOO, "v1", &foo_asset(&one.0, &one.1));
        gh_release_v(&scenario, FOO, "v2", &foo_asset(&two.0, &two.1));
        let http = replay(&env, &scenario);
        assert_eq!(newest(http.as_ref(), "gh:o/foo"), "gh:o/foo@v1");

        gh_latest(&scenario, FOO, "v2");
        let http = replay(&env, &scenario);
        assert_eq!(newest(http.as_ref(), "gh:o/foo"), "gh:o/foo@v2");
        for tag in ["v1", "v2"] {
            let url = format!("{FOO}/releases/download/{tag}/release.json");
            assert_eq!(get(http.as_ref(), &url).0, 200, "{tag}");
        }
        let latest = fs::read_dir(scenario.join("github.com"))
            .unwrap()
            .filter(|f| {
                let name = f.as_ref().unwrap().file_name();
                name.to_string_lossy().starts_with("latest-")
            })
            .count();
        assert_eq!(latest, 1, "one releases/latest fixture");
    }

    #[test]
    fn gh_feed_decodes_and_answers_a_conditional_request_with_304() {
        let env = TestEnv::new();
        let scenario = env.root().join("http");
        let notes = [("bundle/v2+1", "<h2>Two</h2>"), ("v1", "<p>One & only</p>")];
        gh_feed(&scenario, FOO, &notes, Some("\"feed-1\""));
        let http = replay(&env, &scenario);
        let first = GithubSource::recent_tags("o", "foo", http.as_ref()).unwrap();
        let tags: Vec<&str> = first.tags.iter().map(|t| t.tag.as_str()).collect();
        assert_eq!(tags, ["bundle/v2+1", "v1"]);
        assert!(!first.revalidated);
        assert!(
            first.tags[0].updated > first.tags[1].updated,
            "newest first"
        );
        let again = GithubSource::recent_tags("o", "foo", http.as_ref()).unwrap();
        assert!(again.revalidated, "the 304 answered");
        assert_eq!(again.tags, first.tags);

        // A new process starts the sequence again: the 200 and its body.
        let body = get(
            replay(&env, &scenario).as_ref(),
            &format!("{FOO}/releases.atom"),
        )
        .1;
        let xml = String::from_utf8(body).unwrap();
        assert!(
            xml.contains("<content type=\"html\">&lt;p&gt;One &amp; only&lt;/p&gt;</content>"),
            "{xml}"
        );
        let entries = omaddons::sources::atom::parse(&xml);
        assert_eq!(entries[1].title, "v1");
    }

    #[test]
    fn installed_behind_records_the_old_release_while_the_new_is_latest() {
        let mut home = Home::new(&["wow", "wow_classic_era"]);
        let (one, two) = (foo_assets("1.0"), foo_assets("2.0"));
        let behind = home.installed_behind(
            FOO,
            ("v1", &foo_asset(&one.0, &one.1)),
            ("v2", &foo_asset(&two.0, &two.1)),
            &["retail"],
        );
        assert_eq!(home.http, behind);
        let foo = &record(&home, "retail").addons["foo"];
        assert_eq!(foo.release_id.as_deref(), Some("gh:o/foo@v1"));
        let http = replay(&home.env, &behind);
        assert_eq!(newest(http.as_ref(), "gh:o/foo"), "gh:o/foo@v2");
        let (memo, _) = omaddons::state::memo::load(&paths(&home.env));
        assert!(!memo.entries.is_empty());
        for entry in memo.entries.values() {
            let age = jiff::Timestamp::now().duration_since(entry.fetched_at);
            assert!(age >= jiff::SignedDuration::from_hours(23), "{age:?}");
        }
    }

    #[test]
    fn drift_mutators_change_exactly_what_they_say() {
        let env = TestEnv::new();
        let addons = env.root().join("AddOns");
        let foo = addon_folder(
            &addons,
            "Foo",
            &[("Foo.toc", "## Interface: 120105\n")],
            &[("core.lua", b"a"), ("libs/lib.lua", b"b")],
        );
        let bar = addon_folder(
            &addons,
            "Bar",
            &[("Bar.toc", "## Interface: 120105\n")],
            &[],
        );

        let toc = foo.join("Foo.toc");
        let mtime = |p: &std::path::Path| fs::metadata(p).unwrap().modified().unwrap();
        let (before, hash) = (mtime(&toc), tree_hash(&addons));
        touch_identical(&toc);
        assert_eq!(tree_hash(&addons), hash, "same bytes");
        let moved = mtime(&toc).duration_since(before).unwrap();
        assert!(moved >= std::time::Duration::from_secs(1), "{moved:?}");

        edit_managed(&foo.join("core.lua"), b"edited");
        assert_eq!(fs::read(foo.join("core.lua")).unwrap(), b"edited");
        assert_eq!(
            fs::read(foo.join("libs/lib.lua")).unwrap(),
            b"b",
            "nothing else"
        );

        add_stray_file(&foo, "extra/stray.lua", b"stray");
        assert_eq!(fs::read(foo.join("extra/stray.lua")).unwrap(), b"stray");

        delete_managed(&foo.join("libs/lib.lua"));
        assert!(!foo.join("libs/lib.lua").exists() && foo.join("libs").is_dir());
        delete_managed(&bar);
        assert!(!bar.exists() && foo.is_dir());
    }

    #[test]
    fn state_readers_and_net_log_view_see_one_install() {
        let home = Home::new(&["wow"]);
        let one = foo_assets("1.0");
        gh_release(&home.http, FOO, "v1", &foo_asset(&one.0, &one.1));
        let envelope = home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
        let tx = envelope["data"]["tx"].as_str().unwrap().to_owned();

        let key = key_of(&home, "retail");
        let rec = record(&home, "retail");
        assert_eq!(rec.key, key);
        assert_eq!(rec.addons["foo"].tx_id.to_string(), tx);
        let index = files_index(&home, "retail");
        assert_eq!(index.key, key);
        let folders: Vec<String> = index.folders.keys().map(ToString::to_string).collect();
        assert_eq!(folders, ["foo"], "keyed by the case-folded folder name");
        let entries = history(&home);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tx.to_string(), tx);

        let log = net_log(&home);
        assert_eq!(
            log.count("github.com", "release-latest"),
            1,
            "{}",
            log.context()
        );
        assert_eq!(
            log.count("github.com", "release-json"),
            1,
            "{}",
            log.context()
        );
        assert_eq!(log.count_purpose("asset"), 1, "{}", log.context());
        assert_eq!(log.count("api.github.com", "release-latest"), 0);
        assert_eq!(log.len(), 3, "{}", log.context());
    }

    #[test]
    fn release_graph_tocs_declare_their_dependencies() {
        let env = TestEnv::new();
        let scenario = env.root().join("http");
        let a = [
            Folder {
                name: "A",
                interface: "120105",
                deps: &["B", "A_Lib"],
                optional: &["C"],
            },
            Folder {
                name: "A_Lib",
                interface: "120105",
                deps: &[],
                optional: &[],
            },
        ];
        let b = [Folder {
            name: "B",
            interface: "120105",
            deps: &["A"],
            optional: &[],
        }];
        let nodes = [
            Node {
                base: "https://github.com/o/a",
                tag: "v1",
                folders: &a,
            },
            Node {
                base: "https://github.com/o/b",
                tag: "v3",
                folders: &b,
            },
        ];
        release_graph(&scenario, &nodes);
        let http = replay(&env, &scenario);
        assert_eq!(newest(http.as_ref(), "gh:o/b"), "gh:o/b@v3");

        let zip = get(
            http.as_ref(),
            "https://github.com/o/a/releases/download/v1/a.zip",
        )
        .1;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
        let mut toc_of = |path: &str| {
            let mut bytes = Vec::new();
            archive
                .by_name(path)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            parse_toc(&bytes)
        };
        let parsed = toc_of("A/A.toc");
        assert_eq!(parsed.dependencies, ["B", "A_Lib"]);
        assert_eq!(parsed.optional_deps, ["C"]);
        assert_eq!(parsed.version.as_deref(), Some("v1"));
        let lib = toc_of("A_Lib/A_Lib.toc");
        assert!(lib.dependencies.is_empty() && lib.optional_deps.is_empty());
        assert_eq!(lib.interfaces, [120_105]);
    }

    #[test]
    fn with_custom_registers_a_discovered_target() {
        let mut home = Home::new(&["wow_classic_era"]);
        let root = CustomRoot::at(home.env.root().join("pserver")).version(1, 12, 1, 5875);
        let dir = home.with_custom(root);
        assert!(dir.join("Interface/AddOns").is_dir());
        let envelope = home.run(&["targets"]).expect(0);
        let dirs: Vec<&str> = envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["client_dir"].as_str())
            .collect();
        assert!(
            dirs.iter().any(|d| std::path::Path::new(d) == dir),
            "{dirs:?}"
        );
        assert!(
            dirs.len() >= 2,
            "the Battle.net client is still there: {dirs:?}"
        );
    }
}

/// SavedVariables in both writer formats, and the BugGrabber scenarios built on them.
#[cfg(test)]
mod saved_variables {
    use super::common::buggrabber::{
        self, BugDb, BugEntry, CHUNK_BYTES, NOW, SvFile, TimeKind, Writer,
    };
    use super::common::lua::{self, Table, Value};
    use super::common::privacy::SENDER;

    /// Every line of `bytes` as `writer` writes it: a leading blank line, CRLF only,
    /// bracketed keys, a trailing comma after every field, tab indentation by depth and
    /// `-- [n]` after each positional field (legacy) or neither (modern).
    fn assert_format(writer: Writer, bytes: &[u8], ctx: &str) {
        assert!(bytes.starts_with(b"\r\n"), "{ctx}: a leading blank line");
        let lone = bytes
            .iter()
            .enumerate()
            .filter(|(i, b)| **b == b'\n' && (*i == 0 || bytes[i - 1] != b'\r'))
            .count();
        assert_eq!(lone, 0, "{ctx}: CRLF line endings only");
        let text = String::from_utf8_lossy(bytes);
        // Per open table: the positional fields counted so far, and whether the table
        // itself is a positional field of its parent.
        let mut open: Vec<(usize, bool)> = Vec::new();
        for line in text.split("\r\n").skip(1).filter(|l| !l.is_empty()) {
            let body = line.trim_start_matches('\t');
            let tabs = line.len() - body.len();
            let closing = body.starts_with('}');
            let depth = open.len() - usize::from(closing);
            let expected_tabs = if writer == Writer::Legacy { depth } else { 0 };
            assert_eq!(tabs, expected_tabs, "{ctx}: indentation of {line:?}");
            // `n` of `-- [n]` for a positional field of the innermost open table.
            let next_position = |open: &mut Vec<(usize, bool)>| {
                let parent = open.last_mut().unwrap();
                parent.0 += 1;
                parent.0
            };
            let field_end = |positional: bool, n: usize| match (writer, positional) {
                (Writer::Legacy, true) => format!(", -- [{n}]"),
                _ => ",".to_owned(),
            };
            if closing {
                let (_, positional) = open.pop().unwrap();
                if open.is_empty() {
                    assert_eq!(body, "}", "{ctx}: a global's table");
                    continue;
                }
                let n = if positional {
                    next_position(&mut open)
                } else {
                    0
                };
                assert_eq!(body, format!("}}{}", field_end(positional, n)), "{ctx}");
            } else if body.ends_with('{') {
                let global = open.is_empty();
                if global {
                    let name = body.strip_suffix(" = {").unwrap();
                    assert!(
                        name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                        "{ctx}: {line:?}"
                    );
                }
                let keyed = body.starts_with("[\"") || body.starts_with('[');
                assert!(global || keyed || body == "{", "{ctx}: {line:?}");
                open.push((0, !global && !keyed));
            } else if body.starts_with('[') {
                assert!(
                    body.contains("] = ") && body.ends_with(','),
                    "{ctx}: {line:?}"
                );
            } else {
                let n = next_position(&mut open);
                let end = field_end(true, n);
                assert!(body.ends_with(&end), "{ctx}: {line:?} ends with {end:?}");
                let literal = &body[..body.len() - end.len()];
                let is_literal = (literal.starts_with('"') && literal.ends_with('"'))
                    || literal.parse::<f64>().is_ok()
                    || matches!(literal, "true" | "false");
                assert!(is_literal, "{ctx}: {line:?} is a positional literal");
            }
        }
        assert!(open.is_empty(), "{ctx}: every table closed");
    }

    #[test]
    fn the_two_writers_render_their_formats_byte_for_byte() {
        let table = Table::default()
            .push(Value::str("a\"b"))
            .with("k", Value::Str(b"c\\d\ne\x01f".to_vec()))
            .push(Value::Table(Table::default().with("n", Value::Int(-3))))
            .with("x", Value::Bool(false));
        let globals = [("T", Value::Table(table)), ("U", Value::Num(0.5))];
        assert_eq!(
            String::from_utf8(lua::render(Writer::Modern, &globals)).unwrap(),
            "\r\nT = {\r\n\"a\\\"b\",\r\n[\"k\"] = \"c\\\\d\\ne\x01f\",\r\n{\r\n[\"n\"] = -3,\r\n},\r\n[\"x\"] = false,\r\n}\r\nU = 0.5\r\n"
        );
        assert_eq!(
            String::from_utf8(lua::render(Writer::Legacy, &globals)).unwrap(),
            "\r\nT = {\r\n\t\"a\\\"b\", -- [1]\r\n\t[\"k\"] = \"c\\\\d\\ne\x01f\",\r\n\t{\r\n\t\t[\"n\"] = -3,\r\n\t}, -- [2]\r\n\t[\"x\"] = false,\r\n}\r\nU = 0.5\r\n"
        );
        let db = BugDb::new(Writer::Legacy, 2).entry(BugEntry::new(
            "x:1: boom",
            2,
            (TimeKind::LocalString, NOW),
        ));
        assert_eq!(
            String::from_utf8(db.render()).unwrap(),
            "\r\nBugGrabberDB = {\r\n\t[\"session\"] = 2,\r\n\t[\"lastSanitation\"] = 3,\r\n\t[\"stopnag\"] = 120001,\r\n\t[\"errors\"] = {\r\n\t\t{\r\n\t\t\t[\"message\"] = \"x:1: boom\",\r\n\t\t\t[\"session\"] = 2,\r\n\t\t\t[\"time\"] = \"2026/10/15 18:30:00\",\r\n\t\t\t[\"counter\"] = 1,\r\n\t\t}, -- [1]\r\n\t},\r\n}\r\n"
        );
    }

    #[test]
    fn every_scenario_renders_in_both_formats_and_deterministically() {
        let names: Vec<&str> = buggrabber::scenarios(Writer::Modern)
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            names,
            [
                "modern_int",
                "modern_string_first_seen",
                "modern_string_last_seen",
                "r152_chunked",
                "embedded_bdui",
                "warnings",
                "forbidden",
                "personal",
                "received",
                "custom_names",
                "two_accounts",
                "mixed_times",
                "flood",
            ]
        );
        for writer in Writer::ALL {
            let scenarios = buggrabber::scenarios(writer);
            assert_eq!(scenarios, buggrabber::scenarios(writer), "deterministic");
            for (name, files) in &scenarios {
                assert!(!files.is_empty(), "{name}");
                for file in files {
                    let ctx = format!("{name} ({writer:?}) {}", file.rel);
                    assert_format(writer, &file.bytes, &ctx);
                    assert!(file.rel.starts_with("Account/"), "{ctx}");
                }
            }
        }
        let modern = buggrabber::modern_int(Writer::Modern).render();
        let legacy = buggrabber::modern_int(Writer::Legacy).render();
        assert!(!modern.contains(&b'\t') && legacy.contains(&b'\t'));
    }

    /// Every value written under the key `field` in `bytes`, in order, as written.
    fn strings_of(bytes: &[u8], field: &str) -> Vec<String> {
        let text = String::from_utf8_lossy(bytes);
        let key = format!("[\"{field}\"] = ");
        text.split("\r\n")
            .filter_map(|l| l.trim_start_matches('\t').strip_prefix(key.as_str()))
            .map(|v| v.trim_end_matches(',').to_owned())
            .collect()
    }

    #[test]
    fn the_r152_message_is_chunked_with_its_stack_and_locals_inside() {
        for writer in Writer::ALL {
            let db = buggrabber::r152_chunked(writer);
            let error = &db.entries[0];
            assert!(error.chunked);
            let message = error.message.as_bytes();
            let chunks: Vec<&[u8]> = message.chunks(CHUNK_BYTES).collect();
            assert_eq!(chunks.len(), 2);
            assert!(chunks.iter().all(|c| c.len() <= CHUNK_BYTES));
            assert!(error.message.contains("\nLocals:|r\n"));
            assert!(error.message.ends_with("\n  ---"));
            assert!(error.message.contains("Foo-1.2\\core.lua:12:"));
            assert!(error.message.contains("AceEvent-3 (Foo):120:"));
            assert!(error.stack.is_none() && error.locals.is_none());
            let bytes = db.render();
            assert_eq!(strings_of(&bytes, "type"), ["\"error\"", "\"event\""]);
            assert_eq!(strings_of(&bytes, "limit"), ["50"]);
            assert!(strings_of(&bytes, "lastSanitation").is_empty());
            // The message is a table of chunks, not a string.
            assert_eq!(strings_of(&bytes, "message")[0], "{");
        }
    }

    #[test]
    fn received_reports_carry_their_sender_and_the_current_session() {
        let db = buggrabber::received(Writer::Modern);
        let sent: Vec<&BugEntry> = db.entries.iter().filter(|e| e.source.is_some()).collect();
        assert_eq!(sent.len(), 2);
        for entry in sent {
            assert_eq!((entry.source, entry.session), (Some(SENDER), db.session));
        }
        assert!(db.entries[0].source.is_none() && db.entries[0].session < db.session);
        assert_eq!(
            strings_of(&db.render(), "source"),
            [format!("\"{SENDER}\""), format!("\"{SENDER}\"")]
        );
    }

    #[test]
    fn scenarios_plant_names_where_real_files_hide_them() {
        let text = |files: Vec<SvFile>| {
            files
                .iter()
                .map(|f| String::from_utf8_lossy(&f.bytes).into_owned())
                .collect::<String>()
        };
        let personal = text(vec![buggrabber::personal(Writer::Modern).file()]);
        for planted in [
            "Player-1234-0ABCDEF1",
            "|Hplayer:FakeChar-FakeRealm|h[FakeChar]|h",
            "name = \\\"FakeChar\\\"",
            "[\"source\"] = \"FakeChar\"",
        ] {
            assert!(personal.contains(planted), "{planted}");
        }
        let custom = text(vec![buggrabber::custom_names(Writer::Modern).file()]);
        assert!(custom.contains("FakeChar on FakeRealm pulled"));
        assert!(custom.contains("[string \\\"print('FakeChar')\\\"]:1:"));
        let accounts = buggrabber::two_accounts(Writer::Legacy);
        let rels: Vec<&str> = accounts.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(
            rels,
            [
                "Account/FAKEACCT/SavedVariables/!BugGrabber.lua",
                "Account/SECONDACCT/SavedVariables/!BugGrabber.lua"
            ]
        );
        let sessions: Vec<Vec<String>> = accounts
            .iter()
            .map(|f| strings_of(&f.bytes, "session"))
            .collect();
        assert_ne!(
            sessions[0][0], sessions[1][0],
            "independent session counters"
        );
        let bdui = text(buggrabber::embedded_bdui(Writer::Modern));
        assert!(bdui.contains("\r\nBDUI_SAVE = {") && bdui.contains("}\r\nBugGrabberDB = {"));
    }

    #[test]
    fn times_follow_their_era() {
        // First seen: a repeat keeps its old time, so the last entry is the older one.
        let first = buggrabber::modern_string_first_seen(Writer::Modern);
        assert!(first.entries[1].time.1 < first.entries[0].time.1);
        let last = buggrabber::modern_string_last_seen(Writer::Modern);
        assert!(last.entries[0].time.1 < last.entries[1].time.1);
        let times = strings_of(&buggrabber::mixed_times(Writer::Modern).render(), "time");
        assert_eq!(times.len(), 3);
        assert!(times[0].starts_with('"') && times[1].starts_with('"'));
        assert_eq!(times[2], (NOW - 3600).to_string());
        let flood = buggrabber::flood(Writer::Modern, 25);
        assert_eq!(flood.entries.len(), 25);
        assert_eq!(flood.tail.len(), 2, "a malformed tail");
        assert!(
            buggrabber::truncated(
                "[Interface/AddOns/SomeUIPack/Modules/Nameplates/Widgets/Indicator.lua]:77:"
            )
            .starts_with("...")
        );
    }
}
