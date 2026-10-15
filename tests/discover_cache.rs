//! The discovery cache: reuse, invalidation by file stamps and listing, the three modes,
//! poisoned and corrupt caches.

mod common;

use std::fs;

use common::env::TestEnv;
use common::trees::{BattlenetRoot, CustomRoot, Prefix};
use omaddons::discover::cache::{CacheStats, DiscoveryMode, discover_cached};
use omaddons::discover::{Discovery, DiscoveryInput, ScanLimits};
use omaddons::rules::RulesTable;
use omaddons::state::paths::Paths;
use support::run;

#[cfg(test)]
mod support {
    use super::*;

    /// One discovery of the env's home in `mode`.
    pub fn run(env: &TestEnv, mode: DiscoveryMode) -> (Discovery, CacheStats) {
        let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
        let input = DiscoveryInput {
            home: env.home(),
            ..DiscoveryInput::default()
        };
        discover_cached(
            &input,
            &RulesTable::builtin(),
            &paths,
            ScanLimits::default(),
            mode,
        )
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
fn cached_mode_never_scans() {
    let env = TestEnv::new();
    home(&env);
    run(&env, DiscoveryMode::Full);
    CustomRoot::at(env.home().join("Games/newcomer"))
        .version(2, 4, 3, 8606)
        .build();
    let (cached, stats) = run(&env, DiscoveryMode::Cached);
    assert!(!stats.scanned);
    assert_eq!(stats.reused, 2);
    assert!(
        !cached.targets.iter().any(|t| t.default_id == "tbc"),
        "new roots wait for a full run"
    );
    let (full, _) = run(&env, DiscoveryMode::Full);
    assert!(full.targets.iter().any(|t| t.default_id == "tbc"));

    fs::remove_dir_all(env.home().join("Games/newcomer")).unwrap();
    let (gone, _) = run(&env, DiscoveryMode::Cached);
    assert!(
        !gone.targets.iter().any(|t| t.default_id == "tbc"),
        "a vanished root is dropped"
    );
    assert!(gone.diagnostics.iter().any(|d| d.contains("is gone")));

    let fresh = TestEnv::new();
    home(&fresh);
    let (_, stats) = run(&fresh, DiscoveryMode::Cached);
    assert!(stats.scanned, "without a cache, Cached behaves like Full");
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
        text.starts_with("{\n  \"schema_version\": \"1.0\""),
        "{text}"
    );
}

#[test]
fn cached_mode_is_fast() {
    let env = TestEnv::new();
    for i in 0..10 {
        CustomRoot::at(env.home().join(format!("Games/client{i}"))).build();
    }
    run(&env, DiscoveryMode::Full);
    run(&env, DiscoveryMode::Cached);
    let start = std::time::Instant::now();
    let (d, stats) = run(&env, DiscoveryMode::Cached);
    let took = start.elapsed();
    assert_eq!(
        (d.targets.len(), stats.reused, stats.scanned),
        (10, 10, false)
    );
    let budget = if cfg!(debug_assertions) { 50 } else { 5 };
    assert!(took.as_millis() < budget, "{took:?} for 10 cached roots");
}
