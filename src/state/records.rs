//! Target records, `<state>/omaddons/targets/<key>.json`: the managed addons on one
//! target and its `generation`, which every commit increments. Lock-free planning
//! records generations and re-plans under the lock if one changed (D24).

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::paths::Paths;
use super::store::{self, Loaded, SchemaVersion, VersionStatus, Versioned};
use crate::error::Result;
use crate::model::{AddonRecord, InstalledRelease, ReleaseTransition, TargetKey, TransitionChange};

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
    /// 1.1: each addon's `skipped_releases`, the releases `update` passes over (the
    /// regression guard, RG-30). A 1.0 record reads as having none. A build that knows
    /// only 1.0 reads a 1.1 record and refuses to change it (`state_too_new`), so it can
    /// never drop the marks by rewriting it.
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 1);
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

/// What a transaction did to each addon's installed release, from the record it found
/// (`before`) and the record it left (`after`) (design 7.2.1): one transition per addon
/// that only `before` holds, only `after` holds, or both hold at another
/// [`InstalledRelease`] (its version, release key or artifact witness), sorted by addon
/// name ignoring case, as the record keys them. `reverted` (an undo) names each of them
/// [`TransitionChange::Reverted`].
///
/// Only the installed release counts: a pin, a skip mark, a channel, a dependency's
/// `required_by` and a reinstall of the same release move none. A local file or folder
/// installed again under the same version is no transition either, since its release id
/// names no source release ([`crate::model::ReleaseKey::of_record`]).
///
/// The commit (`engine::tx`) and an undo (`engine::undo`) both call it, so a line
/// recovery writes holds exactly what a clean commit would have.
pub fn transitions(
    before: &TargetRecord,
    after: &TargetRecord,
    reverted: bool,
) -> Vec<ReleaseTransition> {
    // In key order: the case-folded name, one per addon.
    let keys: BTreeSet<&String> = before.addons.keys().chain(after.addons.keys()).collect();
    keys.into_iter()
        .filter_map(|key| {
            let (was, is) = (before.addons.get(key), after.addons.get(key));
            let from = was.map(InstalledRelease::of);
            let to = is.map(InstalledRelease::of);
            if from == to {
                return None;
            }
            let change = match (&from, &to) {
                _ if reverted => TransitionChange::Reverted,
                (None, _) => TransitionChange::Installed,
                (_, None) => TransitionChange::Removed,
                _ => TransitionChange::Updated,
            };
            // The name the record leaves, else the one it held.
            let addon = is.or(was)?.name.clone();
            Some(ReleaseTransition {
                addon,
                change,
                from,
                to,
            })
        })
        .collect()
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
            left_out: Vec::new(),
            any_flavor: false,
            skipped_releases: Vec::new(),
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
            text.starts_with("{\n  \"schema_version\": \"1.1\",\n  \"key\""),
            "{text}"
        );
        assert_eq!(load_record(&paths, &key).unwrap().value, record);
    }

    /// `name` at `version` of `gh:o/<name>`, released as `<ref>@<version>` at `revision`
    /// from an archive hashing to `blob`; `local` installs it from a zip instead.
    fn at(name: &str, version: &str, revision: Option<u32>, blob: Option<&str>) -> AddonRecord {
        let slug = name.to_ascii_lowercase();
        AddonRecord {
            r#ref: format!("gh:o/{slug}").parse().unwrap(),
            release_id: Some(format!("gh:o/{slug}@{version}")),
            version: version.into(),
            revision,
            blob_sha256: blob.map(str::to_owned),
            ..addon(name, &[slug.as_str()])
        }
    }

    fn local(name: &str, version: &str, content: &str) -> AddonRecord {
        AddonRecord {
            r#ref: format!("file:/home/me/{name}.zip").parse().unwrap(),
            release_id: Some(format!("file:/home/me/{name}.zip@{content}")),
            version: version.into(),
            ..addon(name, &[name])
        }
    }

    fn holding(addons: Vec<AddonRecord>) -> TargetRecord {
        let mut record = TargetRecord::empty(KEY.parse().unwrap());
        for a in addons {
            record.addons.insert(TargetRecord::addon_key(&a.name), a);
        }
        record
    }

    /// `(addon, change, from version, to version, from revision, to revision)` per
    /// transition, for compact assertions; `-` is no release on that side.
    fn moves(transitions: &[ReleaseTransition]) -> Vec<String> {
        let side = |r: &Option<InstalledRelease>| match r {
            None => "-".to_owned(),
            Some(r) => match r.release.as_ref().and_then(|k| k.revision) {
                Some(rev) => format!("{}#r{rev}", r.version),
                None => r.version.clone(),
            },
        };
        transitions
            .iter()
            .map(|t| {
                let change = serde_json::to_value(t.change).unwrap();
                format!(
                    "{} {} {} -> {}",
                    t.addon,
                    change.as_str().unwrap(),
                    side(&t.from),
                    side(&t.to)
                )
            })
            .collect()
    }

    /// One case: its name, the addons before and after, whether it is an undo, and the
    /// moves expected (as [`moves`] spells them).
    type Case = (
        &'static str,
        Vec<AddonRecord>,
        Vec<AddonRecord>,
        bool,
        Vec<&'static str>,
    );

    #[test]
    fn transitions_name_each_addon_whose_installed_release_moved() {
        let pinned = |a: AddonRecord| AddonRecord { pinned: true, ..a };
        let required = |a: AddonRecord| AddonRecord {
            required_by: vec!["Other".into()],
            ..a
        };
        let cases: Vec<Case> = vec![
            ("nothing", vec![], vec![], false, vec![]),
            (
                "added",
                vec![],
                vec![at("Foo", "v1", None, Some("aa"))],
                false,
                vec!["Foo installed - -> v1"],
            ),
            (
                "removed",
                vec![at("Foo", "v1", None, Some("aa"))],
                vec![],
                false,
                vec!["Foo removed v1 -> -"],
            ),
            (
                "updated",
                vec![at("Foo", "v1", None, Some("aa"))],
                vec![at("Foo", "v2", None, Some("bb"))],
                false,
                vec!["Foo updated v1 -> v2"],
            ),
            (
                "unchanged: a reinstall of the same release",
                vec![at("Foo", "v1", None, Some("aa"))],
                vec![at("Foo", "v1", None, Some("aa"))],
                false,
                vec![],
            ),
            (
                "unchanged: a pin and a requester are no move",
                vec![at("Foo", "v1", None, Some("aa"))],
                vec![required(pinned(at("Foo", "v1", None, Some("aa"))))],
                false,
                vec![],
            ),
            (
                "a WowAce re-tag: the same label at another revision",
                vec![at("Foo", "v12", Some(401), Some("aa"))],
                vec![at("Foo", "v12", Some(403), Some("bb"))],
                false,
                vec!["Foo updated v12#r401 -> v12#r403"],
            ),
            (
                "the same release with other bytes",
                vec![at("Foo", "v1", None, Some("aa"))],
                vec![at("Foo", "v1", None, Some("bb"))],
                false,
                vec!["Foo updated v1 -> v1"],
            ),
            (
                "a local zip installed again under its version names no release",
                vec![local("Bar", "2.1.0", "h1")],
                vec![local("Bar", "2.1.0", "h2")],
                false,
                vec![],
            ),
            (
                "a local zip at a new version",
                vec![local("Bar", "2.1.0", "h1")],
                vec![local("Bar", "2.2.0", "h2")],
                false,
                vec!["Bar updated 2.1.0 -> 2.2.0"],
            ),
            (
                "an undo: every move is reverted, an install's undo included",
                vec![
                    at("Foo", "v2", None, Some("bb")),
                    at("New", "v1", None, Some("cc")),
                ],
                vec![
                    at("Foo", "v1", None, Some("aa")),
                    at("Old", "v3", None, None),
                ],
                true,
                vec![
                    "Foo reverted v2 -> v1",
                    "New reverted v1 -> -",
                    "Old reverted - -> v3",
                ],
            ),
            (
                "sorted by name ignoring case, the unchanged left out",
                vec![
                    at("Zed", "1", None, None),
                    at("alpha", "1", None, None),
                    at("Mid", "1", None, None),
                ],
                vec![
                    at("Zed", "2", None, None),
                    at("alpha", "2", None, None),
                    at("Mid", "1", None, None),
                ],
                false,
                vec!["alpha updated 1 -> 2", "Zed updated 1 -> 2"],
            ),
        ];
        for (name, before, after, reverted, expected) in cases {
            let (before, after) = (holding(before), holding(after));
            let got = transitions(&before, &after, reverted);
            assert_eq!(moves(&got), expected, "{name}");
            // Each side is the record's own release, key and witness included.
            let of = |record: &TargetRecord, addon: &str| {
                record
                    .addons
                    .get(&TargetRecord::addon_key(addon))
                    .map(InstalledRelease::of)
            };
            for t in &got {
                assert_eq!(t.from, of(&before, &t.addon), "{name}: {} from", t.addon);
                assert_eq!(t.to, of(&after, &t.addon), "{name}: {} to", t.addon);
            }
        }
    }

    #[test]
    fn transitions_keep_the_name_the_record_gives() {
        // A record renamed in case keeps its key: the transition names it as it is after,
        // and as it was before when it is gone.
        let before = holding(vec![
            at("foo", "v1", None, None),
            at("Gone", "v1", None, None),
        ]);
        let after = holding(vec![at("Foo", "v2", None, None)]);
        let names: Vec<String> = transitions(&before, &after, false)
            .into_iter()
            .map(|t| t.addon)
            .collect();
        assert_eq!(names, ["Foo", "Gone"]);
    }

    /// A record as 1.0 wrote it: the same keys, no `skipped_releases`.
    fn record_1_0(version: &str) -> String {
        format!(
            r#"{{
  "schema_version": "{version}",
  "key": "{KEY}",
  "generation": 3,
  "addons": {{
    "weakauras": {{
      "ref": "gh:WeakAuras/WeakAuras2",
      "name": "WeakAuras",
      "version": "5.19.10",
      "version_confidence": "release",
      "release_id": null,
      "channel": "stable",
      "pinned": false,
      "reason": "explicit",
      "required_by": [],
      "folders": ["WeakAuras"],
      "state": "ok",
      "installed_at": "2026-10-14T21:00:00Z",
      "tx_id": "01JB8Q2VA00000000000000000"
    }}
  }}
}}
"#
        )
    }

    fn paths_in(root: &std::path::Path) -> Paths {
        Paths::with_roots(
            root,
            root.join(".config"),
            root.join(".local/state"),
            root.join(".cache"),
        )
    }

    #[test]
    fn a_1_0_record_loads_with_no_marks_and_saves_as_1_1() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = paths_in(&root);
        let key: TargetKey = KEY.parse().unwrap();
        std::fs::create_dir_all(paths.targets_dir()).unwrap();
        std::fs::write(paths.target_record(&key), record_1_0("1.0")).unwrap();

        let loaded = load_record(&paths, &key).unwrap();
        assert_eq!(loaded.status, VersionStatus::Older);
        assert_eq!(loaded.version, SchemaVersion::new(1, 0));
        let addon = &loaded.value.addons["weakauras"];
        assert!(addon.skipped_releases.is_empty());

        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "test");
        store::save(
            &mut s,
            &paths.target_record(&key),
            &loaded.value,
            Some(&loaded.status),
        )
        .unwrap();
        let text = std::fs::read_to_string(paths.target_record(&key)).unwrap();
        assert!(text.contains("\"schema_version\": \"1.1\""), "{text}");
        assert!(
            !text.contains("skipped_releases"),
            "a record without marks writes as before: {text}"
        );
        assert_eq!(load_record(&paths, &key).unwrap().value, loaded.value);
    }

    #[test]
    fn a_1_2_record_is_read_and_refused_for_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = paths_in(&root);
        let key: TargetKey = KEY.parse().unwrap();
        std::fs::create_dir_all(paths.targets_dir()).unwrap();
        let newer = record_1_0("1.2");
        std::fs::write(paths.target_record(&key), &newer).unwrap();

        let loaded = load_record(&paths, &key).unwrap();
        assert_eq!(loaded.status, VersionStatus::Newer);
        let refused = store::refuse_newer::<TargetRecord>(&paths.target_record(&key)).unwrap_err();
        assert_eq!(refused.code, crate::error::ErrorCode::StateTooNew);
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "test");
        assert!(
            store::save(
                &mut s,
                &paths.target_record(&key),
                &loaded.value,
                Some(&loaded.status)
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(paths.target_record(&key)).unwrap(),
            newer,
            "a newer record is never rewritten"
        );
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
