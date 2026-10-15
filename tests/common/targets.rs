//! Real-shaped targets: discovered from synthetic client trees, as the CLI would.

use omaddons::discover::DiscoverySource;
use omaddons::discover::battlenet::{BattlenetHints, discover_root};
use omaddons::discover::custom::discover_custom_root;
use omaddons::model::Target;
use omaddons::rules::RulesTable;

use super::env::TestEnv;
use super::trees::{BattlenetRoot, CustomRoot, product};

/// The Battle.net target of product `code`, in its own install root under the env.
pub fn battlenet(env: &TestEnv, code: &str) -> Target {
    let root = BattlenetRoot::at(env.root().join(code))
        .product(code)
        .build();
    let (targets, _) = discover_root(
        &root,
        &RulesTable::builtin(),
        &BattlenetHints::default(),
        "battlenet",
        &DiscoverySource::Scan,
    );
    let folder = product(code).folder;
    let found = targets
        .into_iter()
        .find(|t| t.target.flavor_dir.as_deref() == Some(folder));
    assert!(found.is_some(), "no {code} target");
    found.unwrap().target
}

/// A 3.3.5a custom client.
pub fn legacy_335(env: &TestEnv) -> Target {
    let root = CustomRoot::at(env.root().join("wotlk"))
        .version(3, 3, 5, 12_340)
        .build();
    let (found, warnings) = discover_custom_root(
        &root,
        &RulesTable::builtin(),
        "custom",
        &DiscoverySource::Scan,
    );
    assert!(found.is_some(), "{warnings:?}");
    found.unwrap().target
}
