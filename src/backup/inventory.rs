//! `<cache>/omaddons/backups.json`, the backup inventory projection (M3-D37): per target,
//! what its backups add up to, so `status --cached` stays bounded however many backups
//! exist. It is validated, never trusted: [`load_validated`] does one `read_dir` of
//! `<meta>/backups/` and one of `<meta>/snapshots/` and compares the ids with what the
//! projection covers, so a backup (or an undo's `WTF.previous`) added or removed
//! behind its back makes it stale, never falsely current.
//!
//! Commands rebuild it ([`rebuild`]); `status --cached` never does. It is derived and
//! best effort, like the release memo: a failed write never fails a command, and
//! deleting it costs one "stale" until the next writer. `auto.capability` and
//! `auto.last` have no other authority, so a rebuild carries them over. A write that
//! changed it rings the doorbell, since `status --cached` reads it (M3-D4).

use std::collections::BTreeMap;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{BackupKind, BackupMeta, Trigger};
use crate::error::Result;
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::limits::{BACKUP_TREE_MAX_DEPTH, BACKUP_TREE_MAX_ENTRIES};
use crate::model::{Actor, BackupId, TargetKey, TxId};
use crate::state::doorbell;
use crate::state::paths::{ClientPaths, Paths};
use crate::state::store::{self, SchemaVersion, VersionStatus, Versioned};

/// The command and actor a projection write rings the doorbell for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ring<'a> {
    pub command: &'a str,
    pub actor: Actor,
}

/// The projection file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InventoryFile {
    #[serde(default)]
    pub targets: BTreeMap<TargetKey, TargetInventory>,
}

impl Versioned for InventoryFile {
    const KIND: &'static str = "backup_inventory";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// One target's backups, summed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TargetInventory {
    /// What it was built from: validation compares these with the directories.
    pub covers: Covers,
    pub counts_by_kind: BTreeMap<BackupKind, u64>,
    /// The backups' apparent size.
    pub bytes: u64,
    /// The apparent size of undo's `WTF.previous` trees.
    pub snapshot_bytes: u64,
    pub latest: Option<Latest>,
    pub latest_qualifying: Option<Qualifying>,
    pub auto: AutoState,
}

impl TargetInventory {
    /// Every backup counted.
    pub fn count(&self) -> u64 {
        self.counts_by_kind.values().sum()
    }
}

/// The ids a projection was built from, sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Covers {
    pub backups: Vec<BackupId>,
    /// Transactions whose snapshot holds a `WTF.previous`.
    pub wtf_previous: Vec<TxId>,
}

/// The newest backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Latest {
    pub id: BackupId,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub captured_at: Timestamp,
    pub kind: BackupKind,
    pub trigger: Option<Trigger>,
}

/// The newest backup that counts as a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Qualifying {
    pub id: BackupId,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub captured_at: Timestamp,
}

/// What automatic backups have observed on this target (M3-D30).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AutoState {
    pub capability: Capability,
    pub last: Option<AutoLast>,
}

/// Whether a clone-only copy worked here the last time one was tried.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Never tried.
    #[default]
    Unknown,
    Supported,
    Unsupported,
}

/// The last automatic attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AutoLast {
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub at: Timestamp,
    /// What it did (`created`, a skip reason, `failed`), as automatic backups name it.
    #[schemars(schema_with = "crate::backup::auto::last_result_schema")]
    pub result: String,
}

/// A projection as validated against the directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inventory {
    /// It covers exactly the backups and `WTF.previous` trees on disk.
    Fresh(TargetInventory),
    /// It covers others: its numbers, and the listing's backup count.
    Stale(TargetInventory, u64),
    /// No projection for this target: the listing's backup count.
    Missing(u64),
}

/// What [`rebuild`] built, and whether the file on disk changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebuilt {
    pub inventory: TargetInventory,
    pub changed: bool,
}

/// What the last automatic attempt on `key` recorded (whether its projection is fresh or
/// not: a rebuild carries it over), the default when none did.
pub fn auto_state(paths: &Paths, key: &TargetKey) -> AutoState {
    load(paths)
        .targets
        .remove(key)
        .map(|inventory| inventory.auto)
        .unwrap_or_default()
}

/// `key`'s projection, validated by one `read_dir` of `client`'s `<meta>/backups/` and
/// one of its `<meta>/snapshots/` (with an `lstat` per snapshot).
pub fn load_validated(paths: &Paths, client: &ClientPaths, key: &TargetKey) -> Inventory {
    validate(&load(paths), client, Some(key))
}

/// The projection file as it is (missing, corrupt or newer reads as empty), for a caller
/// that validates several targets against one read.
pub fn load_file(paths: &Paths) -> InventoryFile {
    load(paths)
}

/// `key`'s projection in `file`, validated as [`load_validated`] does; a target without
/// a key has none.
pub fn validate(file: &InventoryFile, client: &ClientPaths, key: Option<&TargetKey>) -> Inventory {
    let on_disk = covers(client);
    let count = on_disk.backups.len() as u64;
    match key.and_then(|key| file.targets.get(key)) {
        // ubs:ignore — comparing backup id sets, not secrets
        Some(inventory) if inventory.covers == on_disk => Inventory::Fresh(inventory.clone()),
        Some(inventory) => Inventory::Stale(inventory.clone(), count),
        None => Inventory::Missing(count),
    }
}

/// Rebuilds `key`'s projection from every `backup.json` and the `WTF.previous` trees
/// (a bounded stat walk each), carrying `auto` over, and writes it (best effort),
/// ringing for `ring` when that changed it.
pub fn rebuild(paths: &Paths, client: &ClientPaths, key: &TargetKey, ring: Ring<'_>) -> Rebuilt {
    let file = load(paths);
    let inventory = build(client, file.targets.get(key));
    store(paths, file, key, inventory, ring)
}

/// `client`'s projection from its backups' metadata; `auto` from `old`. A backup whose
/// metadata cannot be read is covered but counted under no kind.
fn build(client: &ClientPaths, old: Option<&TargetInventory>) -> TargetInventory {
    let (backups, _) = super::list(client);
    let on_disk = covers(client);
    let mut inventory = TargetInventory {
        snapshot_bytes: on_disk
            .wtf_previous
            .iter()
            .filter_map(|tx| wtf_previous(client, tx))
            .map(|previous| tree_bytes(&previous))
            .sum(),
        covers: on_disk,
        auto: old.map(|old| old.auto.clone()).unwrap_or_default(),
        ..TargetInventory::default()
    };
    let metas: Vec<&BackupMeta> = backups.iter().filter_map(|b| b.meta.as_ref()).collect();
    for meta in &metas {
        *inventory.counts_by_kind.entry(meta.kind).or_default() += 1;
        inventory.bytes += meta.bytes;
    }
    inventory.latest = metas.first().map(|m| Latest {
        id: m.id,
        captured_at: m.captured_at,
        kind: m.kind,
        trigger: m.trigger,
    });
    inventory.latest_qualifying = metas
        .iter()
        .find(|m| super::qualifying(m))
        .map(|m| Qualifying {
            id: m.id,
            captured_at: m.captured_at,
        });
    inventory
}

/// Puts `inventory` in `file` for `key` and writes the file when that changed it, then
/// rings the doorbell for `ring`.
fn store(
    paths: &Paths,
    mut file: InventoryFile,
    key: &TargetKey,
    inventory: TargetInventory,
    ring: Ring<'_>,
) -> Rebuilt {
    // ubs:ignore — comparing projections by target key, not secrets
    let changed = file.targets.get(key) != Some(&inventory);
    file.targets.insert(*key, inventory.clone());
    let written = changed && write(paths, &file).is_ok();
    if written {
        doorbell::ring(paths, ring.command, ring.actor);
    }
    Rebuilt {
        inventory,
        changed: written,
    }
}

/// One change a command made, applied without reading every backup where it can be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A backup was published.
    Added(Box<BackupMeta>),
    /// What the last automatic attempt observed.
    Auto(AutoState),
}

/// Applies `change` to `key`'s projection when the projection explains the directories
/// but for it, else rebuilds (then applies an `Auto`, which a rebuild cannot know);
/// writes the result (best effort), ringing for `ring` when that changed it. Returns
/// what [`rebuild`] does.
pub fn record(
    paths: &Paths,
    client: &ClientPaths,
    key: &TargetKey,
    change: Change,
    ring: Ring<'_>,
) -> Rebuilt {
    let file = load(paths);
    let on_disk = covers(client);
    let old = file.targets.get(key).cloned();
    let inventory = match change {
        Change::Auto(auto) => {
            let mut inventory = match old {
                Some(old) if old.covers == on_disk => old,
                old => build(client, old.as_ref()),
            };
            inventory.auto = auto;
            inventory
        }
        Change::Added(meta) => match old {
            Some(old) if explains(&old.covers, &meta.id, &on_disk) => added(old, &meta, on_disk),
            old => build(client, old.as_ref()),
        },
    };
    store(paths, file, key, inventory, ring)
}

/// Whether `covers` plus the backup `id` is exactly `on_disk`.
fn explains(covers: &Covers, id: &BackupId, on_disk: &Covers) -> bool {
    let mut backups = covers.backups.clone();
    backups.push(*id);
    backups.sort();
    backups == on_disk.backups && covers.wtf_previous == on_disk.wtf_previous
}

/// `inventory` with the published backup `meta` counted.
fn added(mut inventory: TargetInventory, meta: &BackupMeta, on_disk: Covers) -> TargetInventory {
    inventory.covers = on_disk;
    *inventory.counts_by_kind.entry(meta.kind).or_default() += 1;
    inventory.bytes += meta.bytes;
    if inventory
        .latest
        .as_ref()
        .is_none_or(|l| l.captured_at <= meta.captured_at)
    {
        inventory.latest = Some(Latest {
            id: meta.id,
            captured_at: meta.captured_at,
            kind: meta.kind,
            trigger: meta.trigger,
        });
    }
    if super::qualifying(meta)
        && inventory
            .latest_qualifying
            .as_ref()
            .is_none_or(|q| q.captured_at <= meta.captured_at)
    {
        inventory.latest_qualifying = Some(Qualifying {
            id: meta.id,
            captured_at: meta.captured_at,
        });
    }
    inventory
}

/// The projection file; missing, corrupt or newer reads as empty.
fn load(paths: &Paths) -> InventoryFile {
    match store::load::<InventoryFile>(&paths.backup_inventory()) {
        Ok(Some(loaded)) if loaded.status != VersionStatus::Newer => loaded.value,
        _ => InventoryFile::default(),
    }
}

/// Writes the projection unless a newer omaddons wrote it.
fn write(paths: &Paths, file: &InventoryFile) -> Result<()> {
    let path = paths.backup_inventory();
    if let Ok(Some(loaded)) = store::load::<InventoryFile>(&path)
        && loaded.status == VersionStatus::Newer
    {
        return Ok(());
    }
    let json = store::encode(file)?;
    let scopes = ScopeTable::new(paths, &[], None)?;
    let mut s = WriteSession::untracked(Writer::Derived, &scopes, "backup-inventory");
    ensure_dir_all(&mut s, paths.cache_dir())?;
    let op = Op::WriteFile {
        content: Content::Bytes(&json),
        mode: Some(0o644),
    };
    mutate(&mut s, &path, op)?;
    Ok(())
}

/// The backup ids and `WTF.previous` transactions on disk, sorted.
fn covers(client: &ClientPaths) -> Covers {
    let names = |dir: std::path::PathBuf| -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|listing| {
                listing
                    .filter_map(std::result::Result::ok)
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut backups: Vec<BackupId> = names(client.backups())
        .iter()
        .filter_map(|n| n.parse().ok())
        .collect();
    backups.sort();
    let snapshots = client.meta_dir().join("snapshots");
    let mut wtf_previous: Vec<TxId> = names(snapshots)
        .iter()
        .filter_map(|n| crate::state::paths::tx_named(n))
        .filter(|tx| self::wtf_previous(client, tx).is_some())
        .collect();
    wtf_previous.sort();
    wtf_previous.dedup();
    Covers {
        backups,
        wtf_previous,
    }
}

/// `tx`'s `WTF.previous` (the tree a restore replaced), in its snapshot under either
/// name: `<tx>.wtf` from omad-aepa on, `<tx>` for a restore before it.
fn wtf_previous(client: &ClientPaths, tx: &TxId) -> Option<std::path::PathBuf> {
    [client.snapshots_wtf(tx), client.snapshots(tx)]
        .into_iter()
        .map(|snapshot| snapshot.join("WTF.previous"))
        .find(|previous| std::fs::symlink_metadata(previous).is_ok_and(|m| m.is_dir()))
}

/// The apparent size of the files below `dir`, by a bounded stat walk; what it could
/// not walk counts nothing.
fn tree_bytes(dir: &std::path::Path) -> u64 {
    super::tree::walk(dir, BACKUP_TREE_MAX_ENTRIES, BACKUP_TREE_MAX_DEPTH)
        .map(|walk| {
            walk.entries
                .iter()
                .filter(|e| e.kind == super::EntryKind::File)
                .map(|e| e.size)
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::super::testing::{meta, seed};
    use super::*;
    use std::fs;

    const KEY: &str = "01JB8Q2V9W0000000000000000";
    const RING: Ring<'static> = Ring {
        command: "backup list",
        actor: Actor::Human,
    };

    fn rebuild(paths: &Paths, client: &ClientPaths, key: &TargetKey) -> Rebuilt {
        super::rebuild(paths, client, key, RING)
    }

    fn record(paths: &Paths, client: &ClientPaths, key: &TargetKey, change: Change) -> Rebuilt {
        super::record(paths, client, key, change, RING)
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        client: ClientPaths,
        key: TargetKey,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        let client = ClientPaths::new(root.join("_retail_"));
        Fixture {
            _dir: dir,
            paths,
            client,
            key: KEY.parse().unwrap(),
        }
    }

    fn id(s: &str) -> BackupId {
        s.parse().unwrap()
    }

    #[test]
    fn a_projection_validates_and_goes_stale_behind_its_back() {
        let f = fixture();
        assert_eq!(
            load_validated(&f.paths, &f.client, &f.key),
            Inventory::Missing(0)
        );
        seed(
            &f.client,
            &meta(
                id("01M6Q2VA000000000000000000"),
                "2026-10-18T21:15:00Z",
                BackupKind::Manual,
            ),
        );
        let auto = meta(
            id("01M6Q2VB000000000000000000"),
            "2026-10-20T18:02:00Z",
            BackupKind::Auto,
        );
        seed(&f.client, &auto);
        assert_eq!(
            load_validated(&f.paths, &f.client, &f.key),
            Inventory::Missing(2)
        );

        let rebuilt = rebuild(&f.paths, &f.client, &f.key);
        assert!(rebuilt.changed);
        let inventory = rebuilt.inventory;
        assert_eq!(inventory.count(), 2);
        assert_eq!(inventory.bytes, 14);
        assert_eq!(inventory.latest.as_ref().unwrap().id, auto.id);
        assert_eq!(
            inventory.latest.as_ref().unwrap().trigger,
            Some(Trigger::PostSession)
        );
        assert_eq!(inventory.latest_qualifying.as_ref().unwrap().id, auto.id);
        assert_eq!(
            load_validated(&f.paths, &f.client, &f.key),
            Inventory::Fresh(inventory.clone())
        );
        assert!(!rebuild(&f.paths, &f.client, &f.key).changed, "nothing new");

        // A backup added behind its back.
        seed(
            &f.client,
            &meta(
                id("01M6Q2VC000000000000000000"),
                "2026-10-21T00:00:00Z",
                BackupKind::Manual,
            ),
        );
        assert_eq!(
            load_validated(&f.paths, &f.client, &f.key),
            Inventory::Stale(inventory.clone(), 3)
        );
        rebuild(&f.paths, &f.client, &f.key);
        // An undo's WTF.previous added, then pruned by retention.
        let tx = TxId::new();
        let previous = f.client.snapshots(&tx).join("WTF.previous");
        fs::create_dir_all(&previous).unwrap();
        fs::write(previous.join("Config.wtf"), b"12345").unwrap();
        assert!(matches!(
            load_validated(&f.paths, &f.client, &f.key),
            Inventory::Stale(_, 3)
        ));
        let rebuilt = rebuild(&f.paths, &f.client, &f.key).inventory;
        assert_eq!(
            (rebuilt.snapshot_bytes, rebuilt.covers.wtf_previous.clone()),
            (5, vec![tx])
        );
        fs::remove_dir_all(f.client.snapshots(&tx)).unwrap();
        assert!(matches!(
            load_validated(&f.paths, &f.client, &f.key),
            Inventory::Stale(_, 3)
        ));
        // A restore's, named `<tx>.wtf` from omad-aepa on: the same.
        rebuild(&f.paths, &f.client, &f.key);
        let previous = f.client.snapshots_wtf(&tx).join("WTF.previous");
        fs::create_dir_all(&previous).unwrap();
        fs::write(previous.join("Config.wtf"), b"1234567").unwrap();
        assert!(matches!(
            load_validated(&f.paths, &f.client, &f.key),
            Inventory::Stale(_, 3)
        ));
        let rebuilt = rebuild(&f.paths, &f.client, &f.key).inventory;
        assert_eq!(
            (rebuilt.snapshot_bytes, rebuilt.covers.wtf_previous),
            (7, vec![tx])
        );
    }

    #[test]
    fn a_rebuild_carries_auto_over_and_record_applies_one_change() {
        let f = fixture();
        rebuild(&f.paths, &f.client, &f.key);
        let auto = AutoState {
            capability: Capability::Supported,
            last: Some(AutoLast {
                at: "2026-10-21T12:00:00Z".parse().unwrap(),
                result: "created".into(),
            }),
        };
        let recorded = record(&f.paths, &f.client, &f.key, Change::Auto(auto.clone()));
        assert!(recorded.changed);
        assert_eq!(recorded.inventory.auto, auto);
        let added = meta(
            id("01M6Q2VA000000000000000000"),
            "2026-10-18T21:15:00Z",
            BackupKind::Manual,
        );
        seed(&f.client, &added);
        let recorded = record(
            &f.paths,
            &f.client,
            &f.key,
            Change::Added(Box::new(added.clone())),
        );
        assert_eq!(recorded.inventory.count(), 1);
        assert_eq!(
            recorded.inventory.latest_qualifying.as_ref().unwrap().id,
            added.id
        );
        assert_eq!(
            recorded.inventory,
            rebuild(&f.paths, &f.client, &f.key).inventory,
            "the cheap path agrees with a rebuild"
        );
        assert_eq!(
            rebuild(&f.paths, &f.client, &f.key).inventory.auto,
            auto,
            "carried over"
        );

        // A change the projection does not explain falls back to a rebuild.
        seed(
            &f.client,
            &meta(
                id("01M6Q2VB000000000000000000"),
                "2026-10-19T00:00:00Z",
                BackupKind::Manual,
            ),
        );
        let third = meta(
            id("01M6Q2VC000000000000000000"),
            "2026-10-20T00:00:00Z",
            BackupKind::Manual,
        );
        seed(&f.client, &third);
        let recorded = record(&f.paths, &f.client, &f.key, Change::Added(Box::new(third)));
        assert_eq!(recorded.inventory.count(), 3);
        assert_eq!(recorded.inventory.auto, auto);
    }

    #[test]
    fn a_write_that_changed_the_projection_rings_and_no_other_does() {
        let f = fixture();
        let bell = f.paths.doorbell_file();
        seed(
            &f.client,
            &meta(
                id("01M6Q2VA000000000000000000"),
                "2026-10-18T21:15:00Z",
                BackupKind::Manual,
            ),
        );
        let ring = Ring {
            command: "backup create",
            actor: Actor::Agent,
        };
        assert!(super::rebuild(&f.paths, &f.client, &f.key, ring).changed);
        let rung: serde_json::Value = serde_json::from_slice(&fs::read(&bell).unwrap()).unwrap();
        assert_eq!(
            (&rung["command"], &rung["actor"]),
            (
                &serde_json::json!("backup create"),
                &serde_json::json!("agent")
            )
        );
        fs::remove_file(&bell).unwrap();
        assert!(!super::rebuild(&f.paths, &f.client, &f.key, ring).changed);
        assert!(!bell.exists(), "nothing changed: no ring");
    }

    #[test]
    fn a_projection_that_cannot_be_written_fails_nothing() {
        let f = fixture();
        fs::create_dir_all(f.paths.cache_dir().parent().unwrap()).unwrap();
        fs::write(f.paths.cache_dir(), b"a file where the cache dir belongs").unwrap();
        seed(
            &f.client,
            &meta(
                id("01M6Q2VA000000000000000000"),
                "2026-10-18T21:15:00Z",
                BackupKind::Manual,
            ),
        );
        let rebuilt = rebuild(&f.paths, &f.client, &f.key);
        assert!(!rebuilt.changed, "not written");
        assert_eq!(rebuilt.inventory.count(), 1);
        assert_eq!(
            load_validated(&f.paths, &f.client, &f.key),
            Inventory::Missing(1)
        );
    }
}
