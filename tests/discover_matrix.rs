//! M1 checkpoint 2: the discovery matrix over synthetic trees (vision §7). Each scenario
//! runs `discover()` and registry reconciliation twice, asserting every target's
//! identity, family, interface, kind, product, flavor folder, exe, label, qualifier and
//! route; that both runs serialize identically; and that nothing under the environment
//! (home, config, state; `discover()` keeps no cache) changed. The binary's `targets`
//! listing must then agree on every id and client dir.
//!
//! Expected values are vision §4.1's; interface = major×10000 + minor×100 + patch for
//! Battle.net clients, patch omitted for legacy ones. A mismatch is a defect in the
//! pieces, not in this table.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::env::TestEnv;
use common::launchers::{
    Install, LutrisDir, bottle, heroic_game, lutris_game, steam_libraries, steam_prefix, steam_root,
};
use common::procfs::running_as_root;
use common::product_db::ProductDb;
use common::trees::{
    BattlenetRoot, CustomRoot, LEGACY_VERSIONS, Prefix, listing, product, tree_hash,
};
use omaddons::discover::registry::{Resolved, reconcile};
use omaddons::discover::{DiscoveredTarget, Discovery, DiscoveryInput, discover};
use omaddons::rules::RulesTable;
use omaddons::state::paths::Paths;
use omaddons::state::registry::load_registry;
use serde_json::{Value, json};
use support::{Expect, bnet, check, custom, run};

#[cfg(test)]
mod support {
    use super::*;

    /// One expected target.
    #[derive(Debug, Clone)]
    pub struct Expect {
        /// The client dir, relative to the environment root.
        pub client: String,
        pub id: String,
        /// The id before collisions qualify it.
        pub default_id: String,
        pub family: &'static str,
        pub interface: u32,
        pub kind: &'static str,
        pub product: Option<&'static str>,
        pub flavor_dir: Option<String>,
        /// Lowercase.
        pub exe: Option<&'static str>,
        /// A custom client's realmlist label; a Battle.net target's is
        /// `<default id> (<qualifier>)`.
        pub label: Option<&'static str>,
        pub qualifier: &'static str,
        /// `product_db`, `scan`, `launcher:<name>`, …
        pub found_by: String,
    }

    impl Expect {
        pub fn id(mut self, id: &str) -> Self {
            self.id = id.to_owned();
            self
        }

        pub fn found(mut self, qualifier: &'static str, found_by: &str) -> Self {
            self.qualifier = qualifier;
            self.found_by = found_by.to_owned();
            self
        }

        pub fn label(mut self, label: &'static str) -> Self {
            self.label = Some(label);
            self
        }

        fn value(&self) -> Value {
            let label = match (self.label, &self.flavor_dir) {
                (Some(label), _) => Some(label.to_owned()),
                (None, Some(_)) => Some(format!("{} ({})", self.default_id, self.qualifier)),
                (None, None) => None,
            };
            let (install_kind, version) = match self.product {
                Some(code) => ("battlenet", product(code).version.to_owned()),
                None => {
                    let (major, minor) = (self.interface / 10_000, self.interface / 100 % 100);
                    let legacy = LEGACY_VERSIONS
                        .iter()
                        .find(|v| (u32::from(v.0), u32::from(v.1)) == (major, minor));
                    assert!(legacy.is_some(), "no legacy version for {}", self.interface);
                    let (a, b, c, d) = legacy.unwrap();
                    ("custom", format!("{a}.{b}.{c}.{d}"))
                }
            };
            json!({
                "client": self.client,
                "install_kind": install_kind,
                "version": version,
                "id": self.id,
                "family": self.family,
                "interface": self.interface,
                "kind": self.kind,
                "product": self.product,
                "flavor_dir": self.flavor_dir,
                "exe": self.exe,
                "label": label,
                "qualifier": self.qualifier,
                "found_by": self.found_by,
            })
        }
    }

    /// A Battle.net flavor folder `<root>/<folder>` of the vision §4.1 table, found by the
    /// scan of the `battlenet` prefix unless [`Expect::found`] says otherwise.
    #[allow(clippy::too_many_arguments)]
    pub fn bnet(
        root: &str,
        folder: &str,
        product: &'static str,
        exe: &'static str,
        family: &'static str,
        interface: u32,
        kind: &'static str,
        id: &str,
    ) -> Expect {
        Expect {
            client: format!("{root}/{folder}"),
            id: id.to_owned(),
            default_id: id.to_owned(),
            family,
            interface,
            kind,
            product: Some(product),
            flavor_dir: Some(folder.to_owned()),
            exe: Some(exe),
            label: None,
            qualifier: "battlenet",
            found_by: "scan".to_owned(),
        }
    }

    /// A custom (legacy) client at `client`, live, `wow.exe`, realmlist `127.0.0.1`.
    pub fn custom(client: &str, family: &'static str, interface: u32, id: &str) -> Expect {
        Expect {
            client: client.to_owned(),
            id: id.to_owned(),
            default_id: id.to_owned(),
            family,
            interface,
            kind: "live",
            product: None,
            flavor_dir: None,
            exe: Some("wow.exe"),
            label: Some("localhost"),
            qualifier: "",
            found_by: "scan".to_owned(),
        }
    }

    /// One discovery and reconciliation.
    pub struct Run {
        pub discovery: Discovery,
        pub resolved: Resolved,
    }

    impl Run {
        pub fn warning_codes(&self) -> Vec<String> {
            self.discovery
                .warnings
                .iter()
                .chain(&self.resolved.warnings)
                .map(|w| w.code.wire().to_owned())
                .collect()
        }

        pub fn target(&self, id: &str) -> &DiscoveredTarget {
            let found = self.resolved.targets.iter().find(|t| t.target.id == id);
            assert!(found.is_some(), "no target {id}: {}", self.context());
            found.unwrap()
        }

        pub fn context(&self) -> String {
            serde_json::to_string_pretty(&json!({
                "targets": self.resolved.targets,
                "warnings": self.discovery.warnings,
                "diagnostics": self.discovery.diagnostics,
            }))
            .unwrap()
        }
    }

    fn input(env: &TestEnv) -> DiscoveryInput {
        DiscoveryInput {
            home: env.home(),
            config_home: Some(env.config()),
            data_home: Some(env.data()),
            ..DiscoveryInput::default()
        }
    }

    fn once(env: &TestEnv) -> Run {
        let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
        let discovery = discover(&input(env), &RulesTable::builtin());
        let registry = load_registry(&paths).unwrap().value;
        let resolved = reconcile(&registry, &discovery.targets);
        Run {
            discovery,
            resolved,
        }
    }

    /// Discovery and reconciliation twice: identical results, and nothing written. `around`
    /// runs before and after (e.g. to lock and unlock a directory).
    pub fn run_with(env: &TestEnv, around: impl Fn(bool)) -> Run {
        fs::create_dir_all(env.state()).unwrap();
        let before = tree_hash(env.root());
        around(true);
        let first = once(env);
        let second = once(env);
        around(false);
        assert_eq!(first.context(), second.context(), "two runs differ");
        assert_eq!(
            tree_hash(env.root()),
            before,
            "discovery or reconciliation wrote\n{}",
            listing(env.root())
        );
        first
    }

    pub fn run(env: &TestEnv) -> Run {
        run_with(env, |_| {})
    }

    fn found_by(t: &DiscoveredTarget) -> String {
        let source = serde_json::to_value(&t.source).unwrap();
        match source["name"].as_str() {
            Some(name) => format!("launcher:{name}"),
            None => source["kind"].as_str().unwrap_or("").to_owned(),
        }
    }

    fn actual(t: &DiscoveredTarget, root: &Path) -> Value {
        let target = serde_json::to_value(&t.target).unwrap();
        let client = t.target.client_dir.strip_prefix(root).unwrap();
        json!({
            "client": client.display().to_string(),
            "install_kind": target["install_kind"],
            "version": target["version"],
            "id": t.target.id,
            "family": target["family"],
            "interface": t.target.interface,
            "kind": target["kind"],
            "product": t.target.product,
            "flavor_dir": t.target.flavor_dir,
            "exe": t.target.exe_name.as_ref().map(|e| e.to_ascii_lowercase()),
            "label": t.target.label,
            "qualifier": t.qualifier,
            "found_by": found_by(t),
        })
    }

    /// Every target of `run` matches `expected` (in any order), and the binary's
    /// `targets` listing gives the same ids for the same client dirs.
    pub fn check(name: &str, env: &TestEnv, run: &Run, expected: &[Expect]) {
        let sorted = |mut v: Vec<Value>| {
            v.sort_by_key(|x| x["client"].as_str().unwrap_or("").to_owned());
            v
        };
        let actual = sorted(
            run.resolved
                .targets
                .iter()
                .map(|t| actual(t, env.root()))
                .collect(),
        );
        let expected = sorted(expected.iter().map(Expect::value).collect());
        assert_eq!(
            serde_json::to_string_pretty(&actual).unwrap(),
            serde_json::to_string_pretty(&expected).unwrap(),
            "{name}: targets differ (left actual, right expected)\n{}\n{}",
            run.context(),
            listing(env.root())
        );

        let output = env.command().args(["targets", "--json"]).output().unwrap();
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        let mut listed: Vec<(String, String)> = envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let dir = PathBuf::from(t["client_dir"].as_str().unwrap());
                let rel = dir.strip_prefix(env.root()).unwrap().display().to_string();
                (rel, t["id"].as_str().unwrap().to_owned())
            })
            .collect();
        listed.sort();
        let mut library: Vec<(String, String)> = actual
            .iter()
            .map(|v| {
                (
                    v["client"].as_str().unwrap().to_owned(),
                    v["id"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        library.sort();
        assert_eq!(
            listed, library,
            "{name}: the binary disagrees\n{envelope:#}"
        );
    }
}

/// The vision §4.1 rows under `root` (relative to the environment root).
fn every_product(root: &str) -> Vec<Expect> {
    #[rustfmt::skip]
    let rows = [
        ("_retail_",          "wow",                 "wow.exe",         "mainline", 120_105, "live", "retail"),
        ("_ptr_",             "wowt",                "wowt.exe",        "mainline", 120_100, "test", "ptr"),
        ("_xptr_",            "wowxptr",             "wowt.exe",        "mainline", 120_105, "test", "xptr"),
        ("_beta_",            "wow_beta",            "wowb.exe",        "mainline", 120_001, "test", "beta"),
        ("_classic_",         "wow_classic",         "wowclassic.exe",  "mists",    50_504,  "live", "classic"),
        ("_classic_ptr_",     "wow_classic_ptr",     "wowclassict.exe", "mists",    50_504,  "test", "classic-ptr"),
        ("_classic_beta_",    "wow_classic_beta",    "wowb.exe",        "forever",  16_001,  "test", "classic-beta"),
        ("_classic_era_",     "wow_classic_era",     "wowclassic.exe",  "vanilla",  11_509,  "live", "era"),
        ("_classic_era_ptr_", "wow_classic_era_ptr", "wowclassict.exe", "tbc",      20_506,  "test", "era-ptr"),
        ("_anniversary_",     "wow_anniversary",     "wowclassic.exe",  "tbc",      20_506,  "live", "anniversary"),
        ("_classic_titan_",   "wow_classic_titan",   "wowclassic.exe",  "titan",    38_002,  "live", "titan"),
        ("_forever_",         "wowf",                "wow.exe",         "forever",  16_001,  "live", "forever"),
    ];
    rows.iter()
        .map(|(folder, product, exe, family, interface, kind, id)| {
            bnet(root, folder, product, exe, family, *interface, kind, id)
        })
        .collect()
}

const BNET: &str = "home/Games/battlenet/drive_c/Program Files (x86)/World of Warcraft";

#[test]
fn every_battlenet_product() {
    let env = TestEnv::new();
    BattlenetRoot::at(Prefix::new(&env, "battlenet").wow_root())
        .all_products()
        .build();
    let run = run(&env);
    check("every product", &env, &run, &every_product(BNET));
    assert!(run.warning_codes().is_empty(), "{}", run.context());
}

#[test]
fn both_forever_layouts_are_forever() {
    let env = TestEnv::new();
    BattlenetRoot::at(Prefix::new(&env, "beta").wow_root())
        .product("wow_classic_beta")
        .build();
    BattlenetRoot::at(Prefix::new(&env, "launch").wow_root())
        .product("wowf")
        .build();
    let run = run(&env);
    let root = |p: &str| format!("home/Games/{p}/drive_c/Program Files (x86)/World of Warcraft");
    let beta = bnet(
        &root("beta"),
        "_classic_beta_",
        "wow_classic_beta",
        "wowb.exe",
        "forever",
        16_001,
        "test",
        "classic-beta",
    );
    let launch = bnet(
        &root("launch"),
        "_forever_",
        "wowf",
        "wow.exe",
        "forever",
        16_001,
        "live",
        "forever",
    );
    check(
        "forever layouts",
        &env,
        &run,
        &[beta.found("beta", "scan"), launch.found("launch", "scan")],
    );
}

#[test]
fn every_legacy_rules_row() {
    let env = TestEnv::new();
    let clients = [
        (
            "vanilla-pfx",
            (1, 12, 1, 5875),
            Some("logon.vanilla.example"),
            0,
        ),
        ("tbc-pfx", (2, 4, 3, 8606), Some("logon.tbc.example"), 0),
        ("wotlk-pfx", (3, 3, 5, 12340), Some("127.0.0.1"), 23),
        ("cata-pfx", (4, 3, 4, 15595), None, 0),
        ("mists-pfx", (5, 4, 8, 18414), Some("localhost:3724"), 0),
    ];
    for (prefix, (major, minor, patch, build), realmlist, blizzard) in clients {
        let root = CustomRoot::at(Prefix::new(&env, prefix).drive_c().join("WoW"))
            .version(major, minor, patch, build)
            .blizzard_folders(blizzard);
        match realmlist {
            Some(host) => root.realmlist(host),
            None => root.no_realmlist(),
        }
        .build();
    }
    let run = run(&env);
    let at = |p: &str| format!("home/Games/{p}/drive_c/WoW");
    check(
        "legacy rows",
        &env,
        &run,
        &[
            custom(&at("vanilla-pfx"), "vanilla-legacy", 11_200, "vanilla")
                .found("vanilla-pfx", "scan")
                .label("logon.vanilla.example"),
            custom(&at("tbc-pfx"), "tbc-legacy", 20_400, "tbc")
                .found("tbc-pfx", "scan")
                .label("logon.tbc.example"),
            custom(&at("wotlk-pfx"), "wrath-legacy", 30_300, "wotlk").found("wotlk-pfx", "scan"),
            custom(&at("cata-pfx"), "cata-legacy", 40_300, "cata")
                .found("cata-pfx", "scan")
                .label("WoW"),
            custom(&at("mists-pfx"), "mists-legacy", 50_400, "mists").found("mists-pfx", "scan"),
        ],
    );
}

#[test]
fn a_mapped_drive_outside_drive_c_is_found_only_through_product_db() {
    let env = TestEnv::new();
    let drive_d = env.root().join("drive-d");
    let prefix = Prefix::new(&env, "battlenet").with_drive('d', &drive_d);
    BattlenetRoot::at(drive_d.join("Games/World of Warcraft"))
        .product("wow")
        .build();
    // Without product.db nothing points outside the prefix.
    let blind = run(&env);
    assert!(blind.resolved.targets.is_empty(), "{}", blind.context());

    ProductDb::new()
        .install(
            "wow",
            "D:/Games/World of Warcraft",
            "_retail_",
            "12.1.5.70391",
        )
        .write(prefix.path());
    let run = run(&env);
    check(
        "mapped drive",
        &env,
        &run,
        &[bnet(
            "drive-d/Games/World of Warcraft",
            "_retail_",
            "wow",
            "wow.exe",
            "mainline",
            120_105,
            "live",
            "retail",
        )
        .found("battlenet", "product_db")],
    );
}

#[test]
fn a_custom_client_outside_any_prefix_is_found_by_the_scan() {
    let env = TestEnv::new();
    CustomRoot::at(env.home().join("Games/turtle"))
        .version(1, 12, 1, 5875)
        .build();
    let run = run(&env);
    check(
        "custom outside a prefix",
        &env,
        &run,
        &[
            custom("home/Games/turtle", "vanilla-legacy", 11_200, "vanilla")
                .found("turtle", "scan"),
        ],
    );
}

#[test]
fn every_launcher_config_resolves_its_root_with_its_qualifier() {
    let env = TestEnv::new();
    let root = env.root();
    // Every client sits outside <home>/Games, so only its launcher config finds it.
    let pfx = Prefix::at(root.join("lutris-pfx"));
    CustomRoot::at(pfx.drive_c().join("WoW"))
        .version(1, 12, 1, 5875)
        .build();
    lutris_game(&env, LutrisDir::Config, "turtle", None, Some(pfx.path()));
    let pfx = Prefix::at(root.join("heroic-pfx"));
    CustomRoot::at(pfx.drive_c().join("WoW"))
        .version(2, 4, 3, 8606)
        .build();
    heroic_game(&env, Install::Native, "tbc", pfx.path());
    let pfx = Prefix::at(root.join("heroic-flatpak-pfx"));
    BattlenetRoot::at(pfx.wow_root())
        .product("wow_classic_era")
        .build();
    heroic_game(&env, Install::Flatpak, "era", pfx.path());
    let library = root.join("disk2/SteamLibrary");
    steam_libraries(&env, Install::Native, &[&library]);
    let pfx = steam_prefix(&library, 2_147_483_649);
    BattlenetRoot::at(pfx.join("drive_c/Program Files (x86)/World of Warcraft"))
        .product("wow_anniversary")
        .build();
    steam_libraries(&env, Install::Flatpak, &[]);
    let pfx = steam_prefix(&steam_root(&env, Install::Flatpak), 7);
    CustomRoot::at(pfx.join("drive_c/WoW"))
        .version(4, 3, 4, 15595)
        .build();
    let mists = bottle(&env, Install::Native, "mists");
    CustomRoot::at(mists.join("drive_c/WoW"))
        .version(5, 4, 8, 18414)
        .build();
    let wotlk = bottle(&env, Install::Flatpak, "wotlk");
    CustomRoot::at(wotlk.join("drive_c/WoW")).build();

    let run = run(&env);
    let steam_flatpak = "home/.var/app/com.valvesoftware.Steam/.local/share/Steam";
    let bottles_flatpak = "home/.var/app/com.usebottles.bottles/data/bottles/bottles";
    let wow = "drive_c/Program Files (x86)/World of Warcraft";
    check(
        "launchers",
        &env,
        &run,
        &[
            custom(
                "lutris-pfx/drive_c/WoW",
                "vanilla-legacy",
                11_200,
                "vanilla",
            )
            .found("lutris", "launcher:lutris"),
            custom("heroic-pfx/drive_c/WoW", "tbc-legacy", 20_400, "tbc")
                .found("heroic", "launcher:heroic"),
            bnet(
                &format!("heroic-flatpak-pfx/{wow}"),
                "_classic_era_",
                "wow_classic_era",
                "wowclassic.exe",
                "vanilla",
                11_509,
                "live",
                "era",
            )
            .found("heroic", "launcher:heroic"),
            bnet(
                &format!("disk2/SteamLibrary/steamapps/compatdata/2147483649/pfx/{wow}"),
                "_anniversary_",
                "wow_anniversary",
                "wowclassic.exe",
                "tbc",
                20_506,
                "live",
                "anniversary",
            )
            .found("steam", "launcher:steam"),
            custom(
                &format!("{steam_flatpak}/steamapps/compatdata/7/pfx/drive_c/WoW"),
                "cata-legacy",
                40_300,
                "cata",
            )
            .found("steam", "launcher:steam"),
            custom(
                "home/.local/share/bottles/bottles/mists/drive_c/WoW",
                "mists-legacy",
                50_400,
                "mists",
            )
            .found("bottles", "launcher:bottles"),
            custom(
                &format!("{bottles_flatpak}/wotlk/drive_c/WoW"),
                "wrath-legacy",
                30_300,
                "wotlk",
            )
            .found("bottles", "launcher:bottles"),
        ],
    );
}

#[test]
fn an_unknown_flavor_folder_is_a_usable_unknown_target() {
    let env = TestEnv::new();
    BattlenetRoot::at(Prefix::new(&env, "battlenet").wow_root())
        .product("wow")
        .unknown_folder("_xyz_", None, None)
        .build();
    let run = run(&env);
    let xyz = run.target("xyz");
    assert_eq!(
        (
            serde_json::to_value(xyz.target.family).unwrap(),
            xyz.target.interface,
            xyz.target.client_dir.join("Interface").is_dir()
        ),
        (json!("unknown"), 0, true),
        "{}",
        run.context()
    );
    // Said by the World once overrides applied, not by discovery.
    assert!(run.warning_codes().is_empty(), "{}", run.context());
    let warning = omaddons::discover::unknown_flavor(&xyz.target).unwrap();
    assert!(
        warning.message.ends_with(
            "flavor folder _xyz_ has no readable version; it is usable with permissive modern rules"
        ),
        "{}",
        warning.message
    );
    run.target("retail");

    // A product the catalog does not know, with a .build.info row, reads as mainline.
    let env = TestEnv::new();
    BattlenetRoot::at(Prefix::new(&env, "battlenet").wow_root())
        .unknown_folder("_xyz_", Some("wowz"), Some("13.0.0.71000"))
        .build();
    let run = support::run(&env);
    let xyz = run.target("xyz");
    assert_eq!(
        (
            serde_json::to_value(xyz.target.family).unwrap(),
            xyz.target.interface,
            xyz.target.product.as_deref()
        ),
        (json!("mainline"), 130_000, Some("wowz")),
        "{}",
        run.context()
    );
}

/// Battle.net installs other Blizzard games in WoW's layout: Warcraft III: Reforged has
/// a `.build.info` and a `_retail_` folder whose product is `w3`. It is no client, so
/// it is skipped without a word, and the WoW root beside it is found as before.
#[test]
fn another_blizzard_game_in_the_prefix_is_no_client() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    BattlenetRoot::at(prefix.wow_root()).product("wow").build();
    BattlenetRoot::at(prefix.drive_c().join("Program Files (x86)/Warcraft III"))
        .unknown_folder("_retail_", Some("w3"), Some("3.0.1.24371"))
        .build();
    let run = run(&env);
    check(
        "w3",
        &env,
        &run,
        &[bnet(
            BNET, "_retail_", "wow", "wow.exe", "mainline", 120_105, "live", "retail",
        )],
    );
    assert!(run.warning_codes().is_empty(), "{}", run.context());
    let output = env.command().args(["status", "--json"]).output().unwrap();
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["warnings"], json!([]), "{status:#}");
}

#[test]
fn a_last_addon_version_disagreement_is_a_warning_not_a_failure() {
    let env = TestEnv::new();
    BattlenetRoot::at(Prefix::new(&env, "battlenet").wow_root())
        .all_products()
        .last_addon_version("_retail_", 120_100)
        .build();
    let run = run(&env);
    check("disagreement", &env, &run, &every_product(BNET));
    assert_eq!(
        run.warning_codes(),
        ["client_not_launched_since_patch"],
        "{}",
        run.context()
    );
}

#[test]
fn broken_and_partial_installs_never_fail_discovery() {
    let env = TestEnv::new();
    // A half-installed Era folder beside a working retail.
    BattlenetRoot::at(Prefix::new(&env, "partial").wow_root())
        .product("wow")
        .product("wow_classic_era")
        .partial("_classic_era_")
        .build();
    // A root whose .build.info is garbage.
    let garbage = BattlenetRoot::at(Prefix::new(&env, "garbage").wow_root())
        .product("wow_anniversary")
        .build();
    fs::write(garbage.join(".build.info"), b"\xff\xfe\0not|a|table\n|||").unwrap();
    // A client inside a directory discovery cannot list.
    let locked = Prefix::new(&env, "locked").drive_c().join("Locked");
    CustomRoot::at(locked.join("WoW")).build();
    let lock = |on: bool| {
        let mode = if on { 0o000 } else { 0o755 };
        fs::set_permissions(&locked, fs::Permissions::from_mode(mode)).unwrap();
    };

    let run = support::run_with(&env, lock);
    let retail = run.target("retail");
    assert!(retail.target.client_dir.ends_with("_retail_"));
    let ids: Vec<&str> = run
        .resolved
        .targets
        .iter()
        .map(|t| t.target.id.as_str())
        .collect();
    if !running_as_root() {
        assert!(
            !ids.contains(&"wotlk"),
            "a locked directory is skipped: {}",
            run.context()
        );
        assert!(
            run.discovery
                .diagnostics
                .iter()
                .any(|d| d.contains("cannot list")),
            "{}",
            run.context()
        );
    }
}

#[test]
fn colliding_retail_installs_qualify_the_later_route() {
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    BattlenetRoot::at(prefix.wow_root()).product("wow").build();
    ProductDb::new()
        .install(
            "wow",
            "C:/Program Files (x86)/World of Warcraft",
            "_retail_",
            "12.1.5.70391",
        )
        .write(prefix.path());
    // A second retail in a Steam prefix, whose path sorts before <home>/Games.
    let pfx = steam_prefix(&steam_root(&env, Install::Native), 2_147_483_650);
    BattlenetRoot::at(pfx.join("drive_c/Program Files (x86)/World of Warcraft"))
        .product("wow")
        .build();
    let run = run(&env);
    let steam = "home/.local/share/Steam/steamapps/compatdata/2147483650/pfx/drive_c/Program Files (x86)/World of Warcraft";
    check(
        "collisions",
        &env,
        &run,
        &[
            bnet(
                BNET, "_retail_", "wow", "wow.exe", "mainline", 120_105, "live", "retail",
            )
            .found("battlenet", "product_db"),
            bnet(
                steam, "_retail_", "wow", "wow.exe", "mainline", 120_105, "live", "retail",
            )
            .id("retail@steam")
            .found("steam", "launcher:steam"),
        ],
    );
}
