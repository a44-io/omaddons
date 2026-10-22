//! Target records, `<state>/omaddons/targets/<key>.json`: the managed addons on one
//! target and its `generation`, which every commit increments. Lock-free planning
//! records generations and re-plans under the lock if one changed (D24).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::paths::Paths;
use super::store::{self, Loaded, SchemaVersion, VersionStatus, Versioned};
use crate::error::Result;
use crate::model::{AddonRecord, TargetKey};

/// The managed addons of one target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TargetRecord {
    pub key: TargetKey,
    pub generation: u64,
    /// Keyed by the ASCII case-folded addon name.
    pub addons: BTreeMap<String, AddonRecord>,
}

impl Versioned for TargetRecord {
    const KIND: &'static str = "target_record";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

impl TargetRecord {
    /// A target nothing has been installed into yet.
    pub fn empty(key: TargetKey) -> Self {
        Self {
            key,
            generation: 0,
            addons: BTreeMap::new(),
        }
    }

    /// The map key of an addon name.
    pub fn addon_key(name: &str) -> String {
        name.to_ascii_lowercase()
    }

    /// The addon that owns the folder with this key (`FolderName::key()`), if any.
    pub fn owner_of(&self, folder_key: &str) -> Option<&str> {
        self.addons
            .iter()
            .find(|(_, addon)| addon.folders.iter().any(|f| f.key() == folder_key))
            .map(|(key, _)| key.as_str())
    }

    /// Increments `generation`; called exactly once per commit.
    pub fn bump(&mut self) {
        self.generation += 1;
    }
}

/// Loads a target's record; a missing record is an empty one at generation 0.
pub fn load_record(paths: &Paths, key: &TargetKey) -> Result<Loaded<TargetRecord>> {
    Ok(
        store::load::<TargetRecord>(&paths.target_record(key))?.unwrap_or_else(|| Loaded {
            value: TargetRecord::empty(*key),
            version: TargetRecord::CURRENT,
            status: VersionStatus::Current,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::{ScopeTable, WriteSession, Writer};
    use crate::model::{AddonReason, AddonState, Channel, FolderName, VersionConfidence};

    const KEY: &str = "01JB8Q2V9W0000000000000000";

    fn addon(name: &str, folders: &[&str]) -> AddonRecord {
        AddonRecord {
            r#ref: "gh:WeakAuras/WeakAuras2".parse().unwrap(),
            name: name.into(),
            version: "5.19.10".into(),
            version_confidence: VersionConfidence::Release,
            release_id: None,
            channel: Channel::Stable,
            pinned: false,
            reason: AddonReason::Explicit,
            required_by: vec![],
            folders: folders
                .iter()
                .map(|f| FolderName::new(f).unwrap())
                .collect(),
            state: AddonState::Ok,
            installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
            tx_id: "01JB8Q2VA00000000000000000".parse().unwrap(),
            blob_sha256: None,
            revision: None,
        }
    }

    #[test]
    fn records_round_trip_and_start_empty() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(
            &root,
            root.join(".config"),
            root.join(".local/state"),
            root.join(".cache"),
        );
        let key: TargetKey = KEY.parse().unwrap();

        let fresh = load_record(&paths, &key).unwrap();
        assert_eq!((fresh.value.generation, fresh.value.addons.len()), (0, 0));

        let mut record = fresh.value;
        record.addons.insert(
            TargetRecord::addon_key("WeakAuras"),
            addon("WeakAuras", &["WeakAuras", "WeakAurasOptions"]),
        );
        record.bump();
        assert_eq!(record.generation, 1);

        std::fs::create_dir_all(paths.targets_dir()).unwrap();
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "test");
        store::save(
            &mut s,
            &paths.target_record(&key),
            &record,
            Some(&fresh.status),
        )
        .unwrap();
        let text = std::fs::read_to_string(paths.target_record(&key)).unwrap();
        assert!(
            text.starts_with("{\n  \"schema_version\": \"1.0\",\n  \"key\""),
            "{text}"
        );
        assert_eq!(load_record(&paths, &key).unwrap().value, record);
    }

    #[test]
    fn owner_of_matches_folder_keys_case_insensitively() {
        let mut record = TargetRecord::empty(KEY.parse().unwrap());
        record.addons.insert(
            "weakauras".into(),
            addon("WeakAuras", &["WeakAuras", "WeakAurasOptions"]),
        );
        record
            .addons
            .insert("plater".into(), addon("Plater", &["Plater"]));
        let key = |name: &str| FolderName::new(name).unwrap().key().to_owned();
        assert_eq!(record.owner_of(&key("WEAKAURASOPTIONS")), Some("weakauras"));
        assert_eq!(record.owner_of(&key("plater")), Some("plater"));
        assert_eq!(record.owner_of(&key("Details")), None);
        assert_eq!(TargetRecord::addon_key("WeakAuras"), "weakauras");
    }
}
