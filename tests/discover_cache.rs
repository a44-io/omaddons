//! The discovery cache: reuse, invalidation by file stamps and listing, the three modes,
//! poisoned and corrupt caches.

use crate::common;

use std::fs;

use common::env::TestEnv;
use common::home::Home;
use common::trees::{BattlenetRoot, CustomRoot, Prefix};
use omaddons::discover::cache::{CacheStats, DiscoveryMode, discover_cached};
use omaddons::discover::{Discovery, DiscoveryInput, ScanLimits};
use omaddons::rules::RulesTable;
use omaddons::state::paths::Paths;
use support::{remapped, run, run_with};

#[cfg(test)]
mod support {
    use super::*;

    /// One discovery of the env's home in `mode`.
    pub fn run(env: &TestEnv, mode: DiscoveryMode) -> (Discovery, CacheStats) {
        run_with(env, mode, &RulesTable::builtin())
    }

    /// One discovery of the env's home in `mode` under `table`.
    pub fn run_with(
        env: &TestEnv,
        mode: DiscoveryMode,
        table: &RulesTable,
    ) -> (Discovery, CacheStats) {
        let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
        let input = DiscoveryInput {
            home: env.home(),
            ..DiscoveryInput::default()
        };
        discover_cached(&input, table, &paths, ScanLimits::default(), mode)
    }

    /// The builtin table with the custom 3.3 client mapped to `family`.
    pub fn remapped(family: &str) -> RulesTable {
        let overrides: omaddons::rules::RulesOverrides = toml::from_str(&format!(
            "[[version_families]]\ninstall_kind = \"custom\"\n\
             major = {{ min = 3, max = 3 }}\nminor = {{ min = 3, max = 3 }}\n\
             family = \"{family}\"\n"
        ))
        .unwrap();
        RulesTable::builtin().with_overrides(&overrides).unwrap()
    }
}

fn version_of(d: &Discovery, id: &str) -> String {
    d.targets
        .iter()
        .find(|t| t.default_id == id)
        .map(|t| t.target.version.to_string())
        .unwrap_or_default()
}

fn home(env: &TestEnv) -> std::path::PathBuf {
    let prefix = Prefix::new(env, "battlenet");
    let root = BattlenetRoot::at(prefix.wow_root())
        .product("wow")
        .product("wow_classic_era")
        .build();
    CustomRoot::at(env.home().join("Games/wotlk")).build();
    root
}

#[test]
fn an_unchanged_tree_reuses_every_root() {
    let env = TestEnv::new();
    home(&env);
    let (first, stats) = run(&env, DiscoveryMode::Full);
    assert_eq!(
        (stats.scanned, stats.parsed, stats.reused, stats.written),
        (true, 2, 0, true)
    );
    assert!(env.cache().join("omaddons/discovery.json").is_file());
    let (second, stats) = run(&env, DiscoveryMode::Full);
    assert_eq!((stats.parsed, stats.reused, stats.written), (0, 2, false));
    assert_eq!(
        second, first,
        "identical, rules refreshed on cached targets"
    );
    assert_eq!(
        second.targets[0].target.rules,
        first.targets[0].target.rules
    );
}

#[test]
fn rewriting_build_info_in_place_invalidates_the_root() {
    let env = TestEnv::new();
    let root = home(&env);
    run(&env, DiscoveryMode::Full);
    let text = fs::read_to_string(root.join(".build.info")).unwrap();
    fs::write(
        root.join(".build.info"),
        text.replace("12.1.5.70391", "12.1.7.71000"),
    )
    .unwrap();
    let (after, stats) = run(&env, DiscoveryMode::Full);
    assert_eq!((stats.parsed, stats.reused), (1, 1));
    assert_eq!(version_of(&after, "retail"), "12.1.7.71000");
}

#[test]
fn a_new_flavor_folder_invalidates_through_the_listing() {
    let env = TestEnv::new();
    let root = home(&env);
    run(&env, DiscoveryMode::Full);
    fs::create_dir_all(root.join("_xyz_")).unwrap();
    let (after, stats) = run(&env, DiscoveryMode::Full);
    assert_eq!((stats.parsed, stats.reused), (1, 1));
    assert!(after.targets.iter().any(|t| t.default_id == "xyz"));
}

#[test]
fn a_changed_last_addon_version_invalidates_a_legacy_root() {
    let env = TestEnv::new();
    home(&env);
    let (before, _) = run(&env, DiscoveryMode::Full);
    let wotlk = |d: &Discovery| {
        d.targets
            .iter()
            .find(|t| t.default_id == "wotlk")
            .map(|t| t.target.interface)
    };
    assert_eq!(wotlk(&before), Some(30_300));
    fs::write(
        env.home().join("Games/wotlk/WTF/Config.wtf"),
        "SET lastAddonVersion \"30000\"\n",
    )
    .unwrap();
    let (after, stats) = run(&env, DiscoveryMode::Full);
    assert_eq!((stats.parsed, wotlk(&after)), (1, Some(30_000)));
}

#[test]
fn no_scan_mode_never_scans() {
    let env = TestEnv::new();
    home(&env);
    run(&env, DiscoveryMode::Full);
    CustomRoot::at(env.home().join("Games/newcomer"))
        .version(2, 4, 3, 8606)
        .build();
    let (cached, stats) = run(&env, DiscoveryMode::NoScan);
    assert_eq!(
        (stats.scanned, stats.reused, stats.written),
        (false, 2, false)
    );
    assert!(
        !cached.targets.iter().any(|t| t.default_id == "tbc"),
        "new roots wait for a full run"
    );
    let (full, _) = run(&env, DiscoveryMode::Full);
    assert!(full.targets.iter().any(|t| t.default_id == "tbc"));

    fs::remove_dir_all(env.home().join("Games/newcomer")).unwrap();
    let (gone, _) = run(&env, DiscoveryMode::NoScan);
    assert!(
        !gone.targets.iter().any(|t| t.default_id == "tbc"),
        "a vanished root is dropped"
    );
    assert!(gone.diagnostics.iter().any(|d| d.contains("is gone")));

    // Without a cache, only the registered roots (none here) are parsed.
    let fresh = TestEnv::new();
    home(&fresh);
    let (none, stats) = run(&fresh, DiscoveryMode::NoScan);
    assert_eq!((stats.scanned, none.targets.len()), (false, 0));
}

#[test]
fn rescan_ignores_a_poisoned_cache() {
    let env = TestEnv::new();
    home(&env);
    run(&env, DiscoveryMode::Full);
    let cache = env.cache().join("omaddons/discovery.json");
    let poisoned = fs::read_to_string(&cache)
        .unwrap()
        .replace("12.1.5.70391", "99.0.0.1");
    fs::write(&cache, poisoned).unwrap();
    let (trusted, _) = run(&env, DiscoveryMode::Full);
    assert_eq!(
        version_of(&trusted, "retail"),
        "99.0.0.1",
        "Full trusts matching stamps"
    );
    let (rescanned, stats) = run(&env, DiscoveryMode::Rescan);
    assert_eq!((stats.reused, stats.written), (0, true));
    assert_eq!(version_of(&rescanned, "retail"), "12.1.5.70391");
    assert!(
        !fs::read_to_string(&cache).unwrap().contains("99.0.0.1"),
        "overwritten"
    );
}

#[test]
fn a_corrupt_cache_is_ignored_and_rewritten() {
    let env = TestEnv::new();
    home(&env);
    let cache = env.cache().join("omaddons/discovery.json");
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    fs::write(&cache, "{ not json").unwrap();
    let (d, stats) = run(&env, DiscoveryMode::Full);
    assert_eq!((stats.parsed, stats.written), (2, true));
    assert_eq!(d.targets.len(), 3);
    let text = fs::read_to_string(&cache).unwrap();
    assert!(
        text.starts_with("{\n  \"schema_version\": \"1.4\""),
        "{text}"
    );
}

/// The family of the custom client under `Games/wotlk`.
fn custom_family(d: &Discovery) -> String {
    d.targets
        .iter()
        .find(|t| t.target.install_root.ends_with("Games/wotlk"))
        .map(|t| t.target.family.to_string())
        .unwrap_or_default()
}

#[test]
fn a_rules_change_re_parses_every_root_once() {
    let env = TestEnv::new();
    home(&env);
    let (before, _) = run(&env, DiscoveryMode::Full);
    let builtin = custom_family(&before);
    assert!(!builtin.is_empty());
    let table = remapped("titan");
    // Every root is parsed again, without --rescan, and the new family shows at once.
    let (after, stats) = run_with(&env, DiscoveryMode::Full, &table);
    assert_eq!((stats.parsed, stats.reused, stats.written), (2, 0, true));
    assert_eq!(custom_family(&after), "titan");
    // Once: the cache now rests on the new rules.
    let (again, stats) = run_with(&env, DiscoveryMode::Full, &table);
    assert_eq!((stats.parsed, stats.reused), (0, 2));
    assert_eq!(custom_family(&again), "titan");
    // `NoScan` looks only at the cached roots, but re-parses them on another basis (and
    // writes nothing, so the cache still rests on the remapped rules).
    let (no_scan, stats) = run(&env, DiscoveryMode::NoScan);
    assert_eq!(
        (stats.scanned, stats.parsed, stats.written),
        (false, 2, false)
    );
    assert_eq!(custom_family(&no_scan), builtin);
    let (no_scan, stats) = run_with(&env, DiscoveryMode::NoScan, &table);
    assert_eq!((stats.scanned, stats.parsed, stats.reused), (false, 0, 2));
    assert_eq!(custom_family(&no_scan), "titan");
}

#[test]
fn a_rules_edit_in_the_config_re_parses_and_any_other_edit_reuses() {
    let mut home = Home::new(&["wow"]);
    let custom = home.with_custom(CustomRoot::at(home.env.root().join("pserver")));
    let config = home.env.config().join("omaddons/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let status = |home: &Home| {
        let r = home.run(&["status", "-v"]);
        let envelope = r.expect(0);
        let family = envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["install_root"].as_str() == custom.to_str())
            .map(|t| t["family"].as_str().unwrap_or_default().to_owned());
        let line = r
            .stderr
            .lines()
            .find(|l| l.contains("discovery: scanned"))
            .map(str::to_owned);
        (line.unwrap_or_default(), family.unwrap_or_default())
    };
    let (_, builtin) = status(&home);
    let reused = "roots reused 2, parsed 0";
    let (line, _) = status(&home);
    assert!(line.contains(reused), "{line}");

    fs::write(&config, "default_channel = \"beta\"\n").unwrap();
    let (line, family) = status(&home);
    assert!(line.contains(reused), "an unrelated edit: {line}");
    assert_eq!(family, builtin);

    fs::write(
        &config,
        "[[rules.version_families]]\ninstall_kind = \"custom\"\n\
         major = { min = 3, max = 3 }\nminor = { min = 3, max = 3 }\nfamily = \"titan\"\n",
    )
    .unwrap();
    let (line, family) = status(&home);
    assert!(line.contains("roots reused 0, parsed 2"), "{line}");
    assert_eq!(family, "titan");
    let (line, _) = status(&home);
    assert!(line.contains(reused), "once: {line}");
}

#[test]
fn a_cache_from_another_binary_re_parses_once() {
    let env = TestEnv::new();
    home(&env);
    run(&env, DiscoveryMode::Full);
    // Another version's basis, or a 1.0 cache without one.
    let cache = env.cache().join("omaddons/discovery.json");
    let text = fs::read_to_string(&cache).unwrap();
    let ours = text
        .lines()
        .find(|l| l.trim_start().starts_with("\"basis\""))
        .unwrap()
        .to_owned();
    for other in ["  \"basis\": \"0\",", ""] {
        fs::write(&cache, text.replace(&ours, other)).unwrap();
        let (_, stats) = run(&env, DiscoveryMode::Full);
        assert_eq!((stats.parsed, stats.written), (2, true), "{other:?}");
        let (_, stats) = run(&env, DiscoveryMode::Full);
        assert_eq!((stats.parsed, stats.reused), (0, 2), "{other:?}");
    }
}

/// No-scan mode reuses all 10 cached roots and walks nothing (its 5 ms budget is a
/// release-build check in `tests/perf.rs`).
#[test]
fn no_scan_mode_reuses_every_root_without_scanning() {
    let env = TestEnv::new();
    for i in 0..10 {
        CustomRoot::at(env.home().join(format!("Games/client{i}"))).build();
    }
    run(&env, DiscoveryMode::Full);
    run(&env, DiscoveryMode::NoScan);
    let (d, stats) = run(&env, DiscoveryMode::NoScan);
    assert_eq!(
        (d.targets.len(), stats.reused, stats.scanned),
        (10, 10, false)
    );
}
