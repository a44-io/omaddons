//! `<state>/omaddons/registry.json`, every registered target's identity (vision §5.1):
//! an immutable key, a human id, the paths it was last seen at and the default target.
//! Plus the marker `<client dir>/Interface/.omaddons/target.json` that carries the key
//! with the install, so a moved install re-links. Read-modify-write of the registry
//! happens only under the global lock (`discover::registry::LockedRegistry`).

use std::path::PathBuf;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::paths::Paths;
use super::store::{self, Loaded, SchemaVersion, VersionStatus, Versioned};
use crate::error::Result;
use crate::fsx::WriteSession;
use crate::model::TargetKey;

/// Every registered target and the default one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Registry {
    /// Sorted by key on write.
    pub targets: Vec<RegistryEntry>,
    pub default: Option<TargetKey>,
}

impl Versioned for Registry {
    const KIND: &'static str = "registry";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// One registered target. Paths are refreshed when a moved install re-links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegistryEntry {
    pub key: TargetKey,
    /// Permanent once registered, except through `targets rename`.
    pub id: String,
    pub install_root: PathBuf,
    pub client_dir: PathBuf,
    pub flavor_dir: Option<String>,
    pub product: Option<String>,
    /// What discovery qualified the id with on a collision.
    pub qualifier: String,
    pub origin: Origin,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub registered_at: Timestamp,
}

/// How a target came to be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Discovered,
    /// `targets add`.
    Manual,
}

impl Registry {
    pub fn entry(&self, key: &TargetKey) -> Option<&RegistryEntry> {
        self.targets.iter().find(|e| e.key == *key)
    }

    pub fn entry_by_id(&self, id: &str) -> Option<&RegistryEntry> {
        self.targets.iter().find(|e| e.id == id)
    }
}

/// The marker file: the target's key, beside its addons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TargetMarker {
    pub key: TargetKey,
}

impl Versioned for TargetMarker {
    const KIND: &'static str = "target_marker";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// Loads the registry; a missing file is an empty registry.
pub fn load_registry(paths: &Paths) -> Result<Loaded<Registry>> {
    Ok(
        store::load::<Registry>(&paths.registry_file())?.unwrap_or_else(|| Loaded {
            value: Registry::default(),
            version: Registry::CURRENT,
            status: VersionStatus::Current,
        }),
    )
}

/// Saves the registry with its entries sorted by key.
pub fn save_registry(
    s: &mut WriteSession<'_>,
    paths: &Paths,
    registry: &Registry,
    loaded_as: &VersionStatus,
) -> Result<()> {
    let mut sorted = registry.clone();
    sorted.targets.sort_by_key(|e| e.key);
    store::save(s, &paths.registry_file(), &sorted, Some(loaded_as))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::{ScopeTable, Writer};

    const KEY: &str = "01JB8Q2V9W0000000000000000";

    #[test]
    fn registry_round_trips_sorted_by_key() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        assert_eq!(load_registry(&paths).unwrap().value, Registry::default());

        let entry = |key: &str, id: &str| RegistryEntry {
            key: key.parse().unwrap(),
            id: id.into(),
            install_root: "/games/wow".into(),
            client_dir: "/games/wow/_retail_".into(),
            flavor_dir: Some("_retail_".into()),
            product: Some("wow".into()),
            qualifier: "battlenet".into(),
            origin: Origin::Discovered,
            registered_at: "2026-10-14T12:00:00Z".parse().unwrap(),
        };
        let registry = Registry {
            targets: vec![entry("01JB8Q2VB00000000000000000", "b"), entry(KEY, "a")],
            default: Some(KEY.parse().unwrap()),
        };
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "registry.json");
        save_registry(&mut s, &paths, &registry, &VersionStatus::Current).unwrap();
        let loaded = load_registry(&paths).unwrap().value;
        assert_eq!(loaded.targets[0].id, "a", "sorted by key");
        assert_eq!(
            loaded.entry_by_id("b").map(|e| e.flavor_dir.as_deref()),
            Some(Some("_retail_"))
        );
        let text = std::fs::read_to_string(paths.registry_file()).unwrap();
        assert!(
            text.starts_with("{\n  \"schema_version\": \"1.0\""),
            "{text}"
        );
        assert!(text.contains("\"origin\": \"discovered\""), "{text}");
    }
}
