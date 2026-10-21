//! Seeded `WTF/` backups under a client's `<meta>/backups/`, laid out as `backup create`
//! publishes them: `backup.json`, `manifest.json`, `addons.json` and a `WTF/` copy.

use std::fs;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use omaddons::backup::{
    AddonProvenance, AddonsFile, BackupKind, BackupMeta, Consistency, EntryKind, Integrity,
    Manifest, ManifestEntry, Reflinked, SourceNote, Trigger,
};
use omaddons::model::{Actor, BackupId, FolderName};
use omaddons::rules::Family;
use omaddons::state::store;

/// What a seeded backup records about its capture.
pub fn meta(kind: BackupKind, captured_at: Timestamp, files: u64) -> BackupMeta {
    BackupMeta {
        id: BackupId::new(),
        target: "01JB8Q2V9W0000000000000000".parse().unwrap(),
        target_id: "retail".to_owned(),
        family: Family::Mainline,
        created_at: captured_at,
        captured_at,
        kind,
        // ubs:ignore — comparing backup kinds, not secrets
        trigger: (kind == BackupKind::Auto).then_some(Trigger::PostSession),
        actor: Actor::Human,
        reason: None,
        client_version: "12.1.5.70391".to_owned(),
        client_running: false,
        files,
        dirs: 4,
        symlinks: 0,
        skipped: 0,
        bytes: files * 100,
        reflinked: Reflinked::All,
        unsettled: 0,
        consistency: Consistency::Stable,
        integrity: Integrity::MetadataOnly,
        verified_at: None,
        baseline: None,
        tool_version: "0.1.0".to_owned(),
        source: None,
        partial: false,
        wtf_outside_client: false,
    }
}

/// A backup of the client at `client_dir` of `kind`, captured at `captured_at`, holding
/// `files` SavedVariables files of 100 bytes; returns its id.
pub fn seed_backup(
    client_dir: &Path,
    kind: BackupKind,
    captured_at: Timestamp,
    files: u64,
) -> BackupId {
    seed_meta(client_dir, &meta(kind, captured_at, files))
}

/// A backup with exactly `meta`; returns its id.
pub fn seed_meta(client_dir: &Path, meta: &BackupMeta) -> BackupId {
    let dir = backup_dir(client_dir, &meta.id);
    let sv = "Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables";
    let mut entries = Vec::new();
    for dir_rel in [
        "Account",
        "Account/FAKEACCT",
        "Account/FAKEACCT/FakeRealm",
        "Account/FAKEACCT/FakeRealm/FakeChar",
        sv,
    ] {
        fs::create_dir_all(dir.join("WTF").join(dir_rel)).unwrap();
        entries.push(entry(dir_rel, EntryKind::Dir, 0));
    }
    for n in 0..meta.files {
        let rel = format!("{sv}/Addon{n}.lua");
        fs::write(dir.join("WTF").join(&rel), [b'x'; 100]).unwrap();
        entries.push(entry(&rel, EntryKind::File, 100));
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    write(&dir.join("backup.json"), meta);
    write(&dir.join("manifest.json"), &Manifest { entries });
    let addons = AddonsFile {
        addons: vec![AddonProvenance {
            name: "WeakAuras".to_owned(),
            folders: vec![
                FolderName::new("WeakAuras").unwrap(),
                FolderName::new("WeakAurasOptions").unwrap(),
            ],
            version: Some("5.19.2".to_owned()),
            source: SourceNote {
                kind: "github".to_owned(),
                r#ref: "gh:WeakAuras/WeakAuras2".to_owned(),
                release_id: Some("gh:WeakAuras/WeakAuras2@5.19.2".to_owned()),
            },
        }],
    };
    write(&dir.join("addons.json"), &addons);
    meta.id
}

/// `n` manual backups an hour apart, newest first in the returned ids; one file each.
pub fn seed_many(client_dir: &Path, n: usize) -> Vec<BackupId> {
    let start: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
    let mut ids: Vec<BackupId> = (0..n)
        .map(|i| {
            let at = start + jiff::SignedDuration::from_hours(i64::try_from(i).unwrap());
            seed_backup(client_dir, BackupKind::Manual, at, 1)
        })
        .collect();
    ids.reverse();
    ids
}

/// `<client_dir>/Interface/.omaddons/backups/<id>`.
pub fn backup_dir(client_dir: &Path, id: &BackupId) -> PathBuf {
    client_dir
        .join("Interface/.omaddons/backups")
        .join(id.to_string())
}

fn entry(path: &str, kind: EntryKind, size: u64) -> ManifestEntry {
    ManifestEntry {
        path: path.to_owned(),
        kind,
        size,
        mode: if kind == EntryKind::Dir { 0o755 } else { 0o644 },
        mtime_ns: 0,
        link: None,
        sha256: None,
    }
}

fn write<T: store::Versioned>(path: &Path, value: &T) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, store::encode(value).unwrap()).unwrap();
}
