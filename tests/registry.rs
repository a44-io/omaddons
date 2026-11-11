//! The target registry over real trees, a real global lock and fsx sessions: identity
//! assignment, moves and copies, id stability, selectors, and read paths that never
//! write.

use crate::common;

use std::fs;
use std::path::PathBuf;

use common::env::TestEnv;
use common::trees::{BattlenetRoot, CustomRoot, Prefix, tree_hash};
use omaddons::discover::registry::{
    LockedRegistry, marker_path, read_marker, reconcile, resolve, resolve_one,
};
use omaddons::discover::{DiscoveredTarget, DiscoveryInput, discover};
use omaddons::error::{ErrorCode, WarningCode};
use omaddons::fsx::{GlobalLock, ScopeTable, WriteSession, Writer};
use omaddons::rules::RulesTable;
use omaddons::state::paths::Paths;
use omaddons::state::registry::{Origin, load_registry};
use support::{World, ids};

#[cfg(test)]
mod support {
    use super::*;

    /// An isolated home, its state paths and the builtin rules.
    pub struct World {
        pub env: TestEnv,
        pub paths: Paths,
        pub table: RulesTable,
    }

    impl World {
        pub fn new() -> Self {
            let env = TestEnv::new();
            let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
            Self {
                env,
                paths,
                table: RulesTable::builtin(),
            }
        }

        /// Discovery, then reconciliation with the registry on disk.
        pub fn targets(&self) -> omaddons::discover::registry::Resolved {
            let input = DiscoveryInput {
                home: self.env.home(),
                ..DiscoveryInput::default()
            };
            let found = discover(&input, &self.table);
            let registry = load_registry(&self.paths).unwrap().value;
            reconcile(&registry, &found.targets)
        }

        /// Runs `f` with the lock held and a session whose scope covers `dirs`.
        pub fn locked<T>(
            &self,
            dirs: &[PathBuf],
            f: impl FnOnce(&mut LockedRegistry<'_>, &mut WriteSession<'_>) -> T,
        ) -> T {
            let lock = GlobalLock::acquire(&self.paths, "test").unwrap();
            let scopes = ScopeTable::new(&self.paths, dirs, None).unwrap();
            let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "registry");
            let mut registry = LockedRegistry::load(&lock, &self.paths).unwrap();
            f(&mut registry, &mut s)
        }

        /// Registers `target` under the lock.
        pub fn register(&self, target: &DiscoveredTarget) -> omaddons::model::TargetKey {
            self.locked(std::slice::from_ref(&target.target.client_dir), |reg, s| {
                reg.ensure_registered(s, target).unwrap()
            })
        }
    }

    pub fn ids(targets: &[DiscoveredTarget]) -> Vec<(String, bool)> {
        targets
            .iter()
            .map(|t| (t.target.id.clone(), t.target.key.is_some()))
            .collect()
    }
}

fn two_retails(world: &World) -> (PathBuf, PathBuf) {
    let bnet = Prefix::new(&world.env, "battlenet");
    let a = BattlenetRoot::at(bnet.wow_root()).product("wow").build();
    let steam = Prefix::new(&world.env, "steam-wow");
    let b = BattlenetRoot::at(steam.wow_root()).product("wow").build();
    (a, b)
}

#[test]
fn reconciling_a_fresh_discovery_is_read_only_and_deterministic() {
    let world = World::new();
    two_retails(&world);
    CustomRoot::at(world.env.home().join("Games/wotlk")).build();
    let before = tree_hash(world.env.root());
    let first = world.targets();
    assert_eq!(
        ids(&first.targets),
        [
            ("retail".into(), false),
            ("retail@steam-wow".into(), false),
            ("wotlk".into(), false)
        ]
    );
    assert_eq!(world.targets(), first, "identical across runs");
    assert!(first.warnings.is_empty() && first.missing.is_empty());
    let table = RulesTable::builtin();
    assert_eq!(resolve("retail", &first.targets, &table).unwrap().len(), 1);
    assert_eq!(
        resolve_one("wotlk", &first.targets, &table, &[])
            .unwrap()
            .target
            .id,
        "wotlk"
    );
    assert_eq!(
        tree_hash(world.env.root()),
        before,
        "reconcile and resolve never write"
    );
}

#[test]
fn registration_writes_the_registry_and_marker_once() {
    let world = World::new();
    let root = CustomRoot::at(world.env.home().join("Games/wotlk")).build();
    let target = world.targets().targets.remove(0);
    let key = world.register(&target);

    let registry = load_registry(&world.paths).unwrap().value;
    let entry = registry.entry(&key).unwrap();
    assert_eq!(
        (entry.id.as_str(), entry.origin, entry.client_dir.as_path()),
        ("wotlk", Origin::Discovered, root.as_path())
    );
    assert_eq!(read_marker(&root), Some(key));
    assert_eq!(
        marker_path(&root),
        root.join("Interface/.omaddons/target.json")
    );
    let marker = fs::read_to_string(marker_path(&root)).unwrap();
    assert!(
        marker.contains("\"schema_version\": \"1.0\"") && marker.contains(&key.to_string()),
        "{marker}"
    );

    // The lock file records each holder, so compare what registration writes.
    let written = || {
        (
            tree_hash(&root),
            fs::read(world.paths.registry_file()).unwrap(),
        )
    };
    let after_first = written();
    let again = world.targets().targets.remove(0);
    assert_eq!(again.target.key, Some(key));
    assert_eq!(world.register(&again), key, "idempotent");
    assert_eq!(
        written(),
        after_first,
        "the second registration wrote nothing"
    );
}

#[test]
fn a_moved_install_relinks_by_marker() {
    let world = World::new();
    let games = world.env.home().join("Games");
    CustomRoot::at(games.join("wotlk")).build();
    let key = world.register(&world.targets().targets[0]);

    fs::rename(games.join("wotlk"), games.join("wotlk-moved")).unwrap();
    let moved = world.targets();
    assert_eq!(ids(&moved.targets), [("wotlk".into(), true)]);
    assert_eq!(moved.targets[0].target.key, Some(key));
    assert!(moved.missing.is_empty());
    assert_eq!(world.register(&moved.targets[0]), key);
    let registry = load_registry(&world.paths).unwrap().value;
    assert_eq!(
        registry.entry(&key).unwrap().client_dir,
        games.join("wotlk-moved")
    );
}

/// A client is uninstalled and installed again elsewhere with its `Interface/` copied
/// over: no client is at the registered directory any more, but that directory's leftover
/// still holds the marker. Read, the new client is the registered one (its key, its id,
/// its record); its first registration must say the same and re-link the key, never give
/// it a new key and id because the old marker is still there, which would detach its
/// addon records.
#[test]
fn a_client_read_as_moved_keeps_its_key_though_the_old_directory_keeps_a_marker() {
    let world = World::new();
    let games = world.env.home().join("Games");
    let old = CustomRoot::at(games.join("wotlk")).build();
    let key = world.register(&world.targets().targets[0]);

    // The new install, with the old one's `Interface/` (and so its marker).
    let new = CustomRoot::at(games.join("wotlk-new")).build();
    fs::copy(marker_path(&old), {
        let to = marker_path(&new);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        to
    })
    .unwrap();
    // The old one is no client any more: its executable is gone, its folders stay.
    for entry in fs::read_dir(&old).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            fs::remove_file(path).unwrap();
        }
    }
    assert_eq!(
        read_marker(&old),
        Some(key),
        "the leftover keeps its marker"
    );

    let read = world.targets();
    assert_eq!(ids(&read.targets), [("wotlk".into(), true)]);
    assert_eq!(read.targets[0].target.key, Some(key));
    assert_eq!(read.targets[0].target.client_dir, new);

    assert_eq!(world.register(&read.targets[0]), key, "the same key");
    let registry = load_registry(&world.paths).unwrap().value;
    assert_eq!(registry.targets.len(), 1);
    assert_eq!(
        (
            registry.entry(&key).unwrap().id.as_str(),
            &registry.entry(&key).unwrap().client_dir
        ),
        ("wotlk", &new)
    );
    assert_eq!(ids(&world.targets().targets), [("wotlk".into(), true)]);
}

/// A copy of a registered client carries its marker, but it is another client: adding
/// it by hand under the original's id is refused, as renaming to a taken id is. Never
/// two targets under one id, which `-t` could then not tell apart.
#[test]
fn a_copy_cannot_be_added_under_the_originals_id() {
    let world = World::new();
    let games = world.env.home().join("Games");
    let original = CustomRoot::at(games.join("a-wotlk")).build();
    let key = world.register(&world.targets().targets[0]);
    // Outside what discovery scans, as a client added by hand is.
    let copy = world.env.root().join("elsewhere/b-wotlk");
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    let status = std::process::Command::new("cp")
        .arg("-a")
        .arg(&original)
        .arg(&copy)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read_marker(&copy), Some(key));

    world.locked(std::slice::from_ref(&copy), |reg, s| {
        let refused = reg
            .add_manual(s, &world.table, &copy, Some("wotlk"))
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::Usage, "{refused:?}");
        // Without the name it is added as the copy it is: its own key and id.
        let (found, _) = reg.add_manual(s, &world.table, &copy, None).unwrap();
        assert_eq!(found.len(), 1);
        assert_ne!(found[0].target.key, Some(key));
        assert_ne!(found[0].target.id, "wotlk");
    });
    let registry = load_registry(&world.paths).unwrap().value;
    let named: Vec<&str> = registry.targets.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        named.iter().filter(|id| **id == "wotlk").count(),
        1,
        "{named:?}"
    );
    assert_eq!(read_marker(&original), Some(key));
}

#[test]
fn a_copied_install_gets_a_new_key() {
    let world = World::new();
    let games = world.env.home().join("Games");
    let original = CustomRoot::at(games.join("a-wotlk")).build();
    let key = world.register(&world.targets().targets[0]);

    let copy = games.join("b-wotlk");
    let status = std::process::Command::new("cp")
        .arg("-a")
        .arg(&original)
        .arg(&copy)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read_marker(&copy), Some(key));

    let resolved = world.targets();
    assert_eq!(
        ids(&resolved.targets),
        [("wotlk".into(), true), ("wotlk@b-wotlk".into(), false)]
    );
    assert_eq!(
        resolved.targets[0].target.key,
        Some(key),
        "the original keeps the key"
    );
    assert_eq!(resolved.warnings.len(), 1);
    assert_eq!(resolved.warnings[0].code, WarningCode::TargetKeyDuplicate);
    assert!(
        resolved.warnings[0].message.contains("wotlk@b-wotlk")
            && resolved.warnings[0].message.contains("wotlk")
    );
    // Both clients claiming the key, sorted (omad-5cg5).
    assert_eq!(resolved.warnings[0].targets, ["wotlk", "wotlk@b-wotlk"]);

    let new_key = world.register(&resolved.targets[1]);
    assert_ne!(new_key, key);
    assert_eq!(read_marker(&copy), Some(new_key));
    assert_eq!(read_marker(&original), Some(key));
    let after = world.targets();
    assert!(after.warnings.is_empty());
    assert_eq!(
        ids(&after.targets),
        [("wotlk".into(), true), ("wotlk@b-wotlk".into(), true)]
    );
}

/// Registered at a, copied to b, then a moved to c (omad-4h0d.18): two clients carry the
/// key and neither is at the registered directory. Reading gives the key to the first
/// found (b) and reports c as the copy; the first change on either side agrees with that
/// read, in both orders: c gets a new key, and b re-links the key, keeping `wotlk`.
#[test]
fn a_copy_read_as_such_gets_a_new_key_when_the_original_moved_away() {
    for c_first in [true, false] {
        let world = World::new();
        let games = world.env.home().join("Games");
        let original = CustomRoot::at(games.join("a-wotlk")).build();
        let key = world.register(&world.targets().targets[0]);
        let (copy, moved) = (games.join("b-wotlk"), games.join("c-wotlk"));
        let status = std::process::Command::new("cp")
            .arg("-a")
            .arg(&original)
            .arg(&copy)
            .status()
            .unwrap();
        assert!(status.success());
        fs::rename(&original, &moved).unwrap();

        let read = world.targets();
        assert_eq!(
            ids(&read.targets),
            [("wotlk".into(), true), ("wotlk@c-wotlk".into(), false)],
            "c_first={c_first}"
        );
        assert_eq!(read.targets[0].target.client_dir, copy);
        assert_eq!(read.warnings[0].code, WarningCode::TargetKeyDuplicate);

        let (b, c) = if c_first {
            let c = world.register(&read.targets[1]);
            (world.register(&world.targets().targets[0]), c)
        } else {
            let b = world.register(&read.targets[0]);
            let again = world.targets();
            assert_eq!(again.targets[1].target.client_dir, moved);
            (b, world.register(&again.targets[1]))
        };
        assert_eq!(
            b, key,
            "c_first={c_first}: b keeps the key it was shown with"
        );
        assert_ne!(c, key, "c_first={c_first}: the copy gets its own");
        assert_eq!(
            (read_marker(&copy), read_marker(&moved)),
            (Some(key), Some(c))
        );
        let registry = load_registry(&world.paths).unwrap().value;
        let entry = registry.entry(&key).unwrap();
        assert_eq!(
            (entry.id.as_str(), &entry.client_dir),
            ("wotlk", &copy),
            "c_first={c_first}"
        );
        let after = world.targets();
        assert!(after.warnings.is_empty(), "{:?}", after.warnings);
    }
}

#[test]
fn registered_targets_keep_their_ids() {
    let world = World::new();
    let steam = Prefix::new(&world.env, "steam-wow");
    BattlenetRoot::at(steam.wow_root()).product("wow").build();
    world.register(&world.targets().targets[0]);

    let bnet = Prefix::new(&world.env, "battlenet");
    BattlenetRoot::at(bnet.wow_root()).product("wow").build();
    let resolved = world.targets();
    assert_eq!(
        ids(&resolved.targets),
        [("retail@battlenet".into(), false), ("retail".into(), true)],
        "the earlier-sorted newcomer is qualified"
    );
}

#[test]
fn rename_validates_and_remove_keeps_the_marker() {
    let world = World::new();
    let (a, b) = two_retails(&world);
    let targets = world.targets().targets;
    let dirs: Vec<PathBuf> = targets
        .iter()
        .map(|t| t.target.client_dir.clone())
        .collect();
    world.locked(&dirs, |reg, s| {
        for t in &targets {
            reg.ensure_registered(s, t).unwrap();
        }
        let key_b = reg.registry().entry_by_id("retail@steam-wow").unwrap().key;
        for bad in [
            "retail",
            "01JB8Q2V9W0000000000000000",
            "wrath-legacy",
            "mainline",
            "wow_classic_era",
            "Bad Name",
        ] {
            let err = reg
                .rename(s, &world.table, "retail@steam-wow", bad)
                .unwrap_err();
            assert_eq!(
                (err.code, err.exit().code()),
                (ErrorCode::Usage, 2),
                "{bad}"
            );
        }
        assert_eq!(
            reg.rename(s, &world.table, "nope", "x").unwrap_err().code,
            ErrorCode::TargetNotFound
        );
        assert_eq!(
            reg.rename(s, &world.table, "retail@steam-wow", "steam")
                .unwrap()
                .key,
            key_b
        );
        assert_eq!(reg.set_default(s, "steam").unwrap(), key_b);
        let removed = reg.remove(s, "steam").unwrap();
        assert_eq!(removed.key, key_b);
        assert_eq!(
            reg.registry().default,
            None,
            "removing the default clears it"
        );
    });
    let b_client = b.join("_retail_");
    let key_b = read_marker(&b_client).unwrap();
    assert!(read_marker(&a.join("_retail_")).is_some());

    let readded = world.locked(std::slice::from_ref(&b_client), |reg, s| {
        reg.add_manual(
            s,
            &world.table,
            &b_client.join("Interface/AddOns"),
            Some("steam"),
        )
        .unwrap()
    });
    let (found, _) = readded;
    assert_eq!(found.len(), 1);
    assert_eq!(
        (found[0].target.key, found[0].target.id.as_str()),
        (Some(key_b), "steam"),
        "re-linked by marker"
    );
    let entry = load_registry(&world.paths)
        .unwrap()
        .value
        .entry(&key_b)
        .cloned()
        .unwrap();
    assert_eq!(entry.origin, Origin::Manual);
}

#[test]
fn add_manual_names_one_target_only() {
    let world = World::new();
    let root = BattlenetRoot::at(world.env.root().join("bnet"))
        .product("wow")
        .product("wowf")
        .build();
    let dirs = vec![root.join("_retail_"), root.join("_forever_")];
    world.locked(&dirs, |reg, s| {
        let err = reg
            .add_manual(s, &world.table, &root, Some("main"))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert_eq!(
            err.fix.argv().unwrap()[3],
            root.join("_forever_").display().to_string()
        );
        assert_eq!(err.alternatives.len(), 1);
        let (found, _) = reg
            .add_manual(s, &world.table, &root.join("_retail_"), Some("main"))
            .unwrap();
        assert_eq!(found[0].target.id, "main");
        let (found, _) = reg.add_manual(s, &world.table, &root, None).unwrap();
        let ids: Vec<&str> = found.iter().map(|d| d.target.id.as_str()).collect();
        assert_eq!(ids, ["forever", "main"]);
        let err = reg
            .add_manual(s, &world.table, &world.env.root().join("nothing"), None)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::TargetNotFound);
    });
}

#[test]
fn every_selector_form() {
    let world = World::new();
    let bnet = BattlenetRoot::at(world.env.home().join("Games/wow"))
        .product("wow_classic_era")
        .build();
    let custom = CustomRoot::at(world.env.home().join("Games/wotlk")).build();
    let first = world.targets().targets;
    let wotlk = first.iter().find(|t| t.target.id == "wotlk").unwrap();
    let key = world.register(wotlk);
    let targets = world.targets().targets;
    let table = RulesTable::builtin();
    let one =
        |selector: &str| resolve_one(selector, &targets, &table, &[]).map(|d| d.target.id.clone());
    assert_eq!(one(&key.to_string()).unwrap(), "wotlk", "key");
    assert_eq!(one("era").unwrap(), "era", "id");
    assert_eq!(one("wow_classic_era").unwrap(), "era", "product code");
    assert_eq!(one("_classic_era_").unwrap(), "era", "flavor folder");
    assert_eq!(one("sod").unwrap(), "era", "game mode");
    assert_eq!(one("wrath-legacy").unwrap(), "wotlk", "family");
    assert_eq!(
        one(&bnet.display().to_string()).unwrap(),
        "era",
        "install root"
    );
    assert_eq!(
        one(&custom.join("Interface/AddOns").display().to_string()).unwrap(),
        "wotlk",
        "AddOns path"
    );
    let err = one("nothing-here").unwrap_err();
    assert_eq!(
        (err.code, err.exit().code()),
        (ErrorCode::TargetNotFound, 3)
    );
    assert_eq!(err.fix.argv().unwrap(), ["omaddons", "targets"]);
    assert_eq!(err.details["selector"], "nothing-here");
}

#[test]
fn concurrent_registration_loses_nothing() {
    let world = World::new();
    for name in ["a", "b", "c", "d"] {
        CustomRoot::at(world.env.home().join("Games").join(name)).build();
    }
    let targets = world.targets().targets;
    std::thread::scope(|scope| {
        for target in &targets {
            let world = &world;
            scope.spawn(move || world.register(target));
        }
    });
    let registry = load_registry(&world.paths).unwrap().value;
    let mut registered: Vec<&str> = registry.targets.iter().map(|e| e.id.as_str()).collect();
    registered.sort_unstable();
    assert_eq!(registered.len(), 4, "{registered:?}");
    let resolved = world.targets();
    assert!(resolved.targets.iter().all(|t| t.target.key.is_some()));
}
