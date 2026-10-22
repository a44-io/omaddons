//! `WTF/` backups (M3-D23): a backup is `<meta>/backups/<id>/`, holding `backup.json`
//! (what it is and what it proves), `manifest.json` (every entry's stats, local only:
//! its paths name the account, realm and character), `addons.json` (which addon
//! versions produced it) and the `WTF/` copy itself.
//!
//! Listing reads `backup.json` files and checks that each manifest exists; it never
//! walks or hashes a tree (M3-D47). A full structure check is [`check_structure`], for
//! restores and `backup verify`. One capture is [`create`], the tree copy is [`tree`],
//! the bounded summary `status --cached` reads is [`inventory`], and the filesystem
//! hints are [`fs`].

pub mod auto;
pub mod create;
pub mod fs;
pub mod inventory;
pub mod prune;
pub mod tree;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::fsx::DirLock;
use crate::limits::{BACKUP_TREE_MAX_DEPTH, BACKUP_TREE_MAX_ENTRIES};
use crate::model::{Actor, BackupId, FolderName, TargetKey};
use crate::rules::Family;
use crate::state::paths::ClientPaths;
use crate::state::store::{self, SchemaVersion, VersionStatus, Versioned};

/// The fewest characters of an id that select a backup, as for undo's prefixes.
pub const ID_PREFIX_MIN: usize = 8;

/// Why a backup exists.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BackupKind {
    /// `backup create`.
    Manual,
    /// An automatic checkpoint.
    Auto,
    /// Taken by a whole restore before it replaced `WTF/`.
    BeforeRestore,
    /// `backup import`.
    Imported,
    /// What recovery preserved of an interrupted restore.
    Recovered,
}

/// What started an automatic backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// Before the day's first change that touches folders or SavedVariables.
    PreChange,
    /// After a play session that changed settings.
    PostSession,
}

/// Whether the tree was observed quiescent while it was copied (never a snapshot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Consistency {
    /// Every file settled across its copy, and a second walk found the tree unchanged.
    Stable,
    /// Something changed while it was copied.
    BestEffort,
}

/// What a backup's content is known to be (M3-D47).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Integrity {
    /// Only its metadata was checked: no content hash was ever compared.
    MetadataOnly,
    /// Every file matched the hashes in its manifest when last verified.
    Verified,
    /// A verification found content that does not match.
    Damaged,
}

/// Where a verified backup's hashes come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Baseline {
    /// Hashed while it was captured (`create --verify`).
    Capture,
    /// Checked against the export it was imported from.
    Import,
    /// Hashed later, by `backup verify`.
    Later,
}

/// How much of a copy shares its data with the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reflinked {
    All,
    Some,
    None,
}

/// The kind of one manifest entry.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

impl EntryKind {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Dir => "dir",
            Self::Symlink => "symlink",
        }
    }
}

/// An imported backup's original.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportSource {
    pub id: BackupId,
    pub target_id: String,
    pub family: Family,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
    #[schemars(with = "String")]
    pub captured_at: Timestamp,
    pub tool_version: String,
}

/// Where a backup's content came from, when not from capturing its client now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum BackupSource {
    /// A `recovered` backup: the interrupted restore whose rollback preserved it.
    Recovery { restore_tx: crate::model::TxId },
    /// An imported backup's original.
    Import(ImportSource),
}

/// `backup.json`: what a backup is and what it proves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackupMeta {
    pub id: BackupId,
    pub target: TargetKey,
    pub target_id: String,
    pub family: Family,
    /// When it was published here.
    #[schemars(with = "String")]
    pub created_at: Timestamp,
    /// When its content was captured (an import's: the original's capture).
    #[schemars(with = "String")]
    pub captured_at: Timestamp,
    pub kind: BackupKind,
    /// Automatic backups only.
    pub trigger: Option<Trigger>,
    pub actor: Actor,
    pub reason: Option<String>,
    pub client_version: String,
    pub client_running: bool,
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    /// Devices, FIFOs and sockets, never copied.
    pub skipped: u64,
    /// The apparent size of its files.
    pub bytes: u64,
    pub reflinked: Reflinked,
    /// Files still changing after a second copy.
    pub unsettled: u64,
    pub consistency: Consistency,
    pub integrity: Integrity,
    #[schemars(with = "Option<String>")]
    pub verified_at: Option<Timestamp>,
    pub baseline: Option<Baseline>,
    pub tool_version: String,
    /// Imports and recovered backups only.
    pub source: Option<BackupSource>,
    /// A recovered backup holding only some files: it restores only one addon's
    /// SavedVariables.
    #[serde(default)]
    pub partial: bool,
    /// The client's `WTF/` was a symlink out of the client dir when captured (a sync
    /// folder): such a backup is never restored over it.
    #[serde(default)]
    pub wtf_outside_client: bool,
}

impl Versioned for BackupMeta {
    const KIND: &'static str = "backup";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// One entry of a backup's `WTF/`, as the source had it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestEntry {
    /// Relative to `WTF/`, with `/`, as spelled on disk.
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// The exact source mode (permission bits).
    pub mode: u32,
    pub mtime_ns: i64,
    /// A symlink's target.
    pub link: Option<String>,
    /// Set only by `create --verify`, an import or `backup verify`.
    pub sha256: Option<String>,
}

/// `manifest.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    pub entries: Vec<ManifestEntry>,
}

impl Versioned for Manifest {
    const KIND: &'static str = "backup_manifest";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// Where an addon came from, as its record said at capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceNote {
    /// The source (`github`, `wowi`, …).
    pub kind: String,
    #[serde(rename = "ref")]
    pub r#ref: String,
    pub release_id: Option<String>,
}

/// One managed addon when the backup was captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AddonProvenance {
    pub name: String,
    pub folders: Vec<FolderName>,
    pub version: Option<String>,
    pub source: SourceNote,
}

/// `addons.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AddonsFile {
    pub addons: Vec<AddonProvenance>,
}

impl Versioned for AddonsFile {
    const KIND: &'static str = "backup_addons";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// A backup found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRef {
    pub id: BackupId,
    /// `None` when `backup.json` is missing, unreadable or written by a newer omaddons
    /// in a shape this one cannot read.
    pub meta: Option<BackupMeta>,
    pub dir: PathBuf,
    /// Its metadata and manifest are readable, it is not `damaged`, and its client's
    /// `WTF/` was inside the client dir. Listing checks no more than that.
    pub restorable: bool,
    /// It counts as a checkpoint (M3-D31).
    pub qualifying: bool,
    /// A reader held it when listed (best effort: that instant only).
    pub busy: bool,
}

impl BackupRef {
    /// When its content was captured; `None` without metadata.
    pub fn captured_at(&self) -> Option<Timestamp> {
        self.meta.as_ref().map(|m| m.captured_at)
    }

    /// Its `addons.json`, when readable.
    pub fn addons(&self) -> Option<AddonsFile> {
        let path = self.dir.join("addons.json");
        let bytes = crate::discover::read_bytes(&path)?;
        store::load_bytes::<AddonsFile>(&path, &bytes)
            .ok()
            .map(|loaded| loaded.value)
    }

    /// The backup's `WTF/` copy.
    pub fn wtf(&self) -> PathBuf {
        self.dir.join("WTF")
    }
}

/// Whether `meta` counts as a checkpoint (M3-D31): captured here (not imported or
/// recovered), whole, and observed stable. Scheduling and the stale hint read its
/// `captured_at`, never a directory's age.
pub fn qualifying(meta: &BackupMeta) -> bool {
    matches!(
        meta.kind,
        BackupKind::Manual | BackupKind::Auto | BackupKind::BeforeRestore
    ) && !meta.partial
        && meta.consistency == Consistency::Stable
        && meta.source.is_none()
}

/// The backups under `client`'s `<meta>/backups/`, newest capture first (those without
/// readable metadata last, newest id first). Reads each `backup.json` (at most
/// `DISCOVERY_FILE_MAX_BYTES`) and checks each manifest exists; never walks a tree. A
/// directory whose name is not a backup id is not a backup. The warnings are a
/// `backup.json` of another version.
pub fn list(client: &ClientPaths) -> (Vec<BackupRef>, Vec<Warning>) {
    let mut warnings = Vec::new();
    let Ok(listing) = std::fs::read_dir(client.backups()) else {
        return (Vec::new(), warnings);
    };
    let mut found: Vec<BackupRef> = listing
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|entry| {
            let id: BackupId = entry.file_name().to_str()?.parse().ok()?;
            Some(read_one(id, entry.path(), &mut warnings))
        })
        .collect();
    found.sort_by(|a, b| {
        b.captured_at()
            .cmp(&a.captured_at())
            .then_with(|| b.id.cmp(&a.id))
    });
    (found, warnings)
}

fn read_one(id: BackupId, dir: PathBuf, warnings: &mut Vec<Warning>) -> BackupRef {
    let path = dir.join("backup.json");
    let meta = crate::discover::read_bytes(&path)
        .and_then(|bytes| store::load_bytes::<BackupMeta>(&path, &bytes).ok())
        .filter(|loaded| {
            warnings.extend(loaded.warning(&path));
            loaded.value.id == id
        })
        .map(|loaded| loaded.value);
    let manifest = std::fs::symlink_metadata(dir.join("manifest.json")).is_ok_and(|m| m.is_file());
    let restorable = meta
        .as_ref()
        .is_some_and(|m| manifest && m.integrity != Integrity::Damaged && !m.wtf_outside_client);
    let qualifying = meta.as_ref().is_some_and(qualifying);
    let busy = matches!(DirLock::try_exclusive(&dir), Ok(None));
    BackupRef {
        id,
        meta,
        dir,
        restorable,
        qualifying,
        busy,
    }
}

/// The backups in `backups` whose id starts with `prefix`, ignoring case.
pub fn matching<'a>(backups: &'a [BackupRef], prefix: &str) -> Vec<&'a BackupRef> {
    let prefix = prefix.to_ascii_uppercase();
    backups
        .iter()
        .filter(|b| b.id.to_string().starts_with(&prefix))
        .collect()
}

/// `usage` unless `prefix` is long enough to select a backup.
pub fn check_prefix(prefix: &str, fix: Suggestion) -> Result<()> {
    if prefix.chars().count() < ID_PREFIX_MIN {
        return Err(Error::new(
            ErrorCode::Usage,
            format!("backup id {prefix:?} is too short: give at least {ID_PREFIX_MIN} characters"),
            fix,
        ));
    }
    Ok(())
}

/// The one backup of `client` (the target `target_id`) whose id starts with `prefix`
/// (8 or more characters). `not_found` names the newest three; `ref_ambiguous` has one
/// alternative per candidate.
pub fn resolve(client: &ClientPaths, target_id: &str, prefix: &str) -> Result<BackupRef> {
    let list_fix = || Suggestion::command(["omaddons", "backup", "list", "-t", target_id]);
    check_prefix(prefix, list_fix())?;
    let (backups, _) = list(client);
    let found = matching(&backups, prefix);
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(not_found(prefix, &backups, Some(target_id))),
        many => Err(ambiguous(
            prefix,
            many.iter().map(|b| (b.id, target_id)).collect(),
        )),
    }
}

/// `not_found` for `prefix` on `target_id`, naming the newest three of `backups`.
/// `not_found` for `prefix` among `backups` (newest first) of `target_id`, or of every
/// client listed when `None`, naming the newest three.
pub fn not_found(prefix: &str, backups: &[BackupRef], target_id: Option<&str>) -> Error {
    let newest: Vec<String> = backups.iter().take(3).map(|b| b.id.to_string()).collect();
    let whose = target_id.map_or_else(String::new, |t| format!(" of {t}"));
    let message = if newest.is_empty() {
        format!("no backup{whose} has an id starting with {prefix}: there are none")
    } else {
        format!(
            "no backup{whose} has an id starting with {prefix}; the newest: {}",
            newest.join(", ")
        )
    };
    let fix = match target_id {
        Some(t) => Suggestion::command(["omaddons", "backup", "list", "-t", t]),
        None => Suggestion::command(["omaddons", "backup", "list"]),
    };
    Error::new(ErrorCode::NotFound, message, fix)
        .details(serde_json::json!({ "prefix": prefix, "target": target_id, "newest": newest }))
}

/// `ref_ambiguous` for `prefix`, with one `backup list <id> -t <target>` per candidate.
pub fn ambiguous(prefix: &str, candidates: Vec<(BackupId, &str)>) -> Error {
    let argv = |id: &BackupId, target: &str| {
        Suggestion::command(["omaddons", "backup", "list", &id.to_string(), "-t", target])
    };
    let ids: Vec<String> = candidates.iter().map(|(id, _)| id.to_string()).collect();
    let mut rest = candidates.iter();
    let fix = rest.next().map_or_else(
        || Suggestion::command(["omaddons", "backup", "list"]),
        |(id, t)| argv(id, t),
    );
    let mut error = Error::new(
        ErrorCode::RefAmbiguous,
        format!(
            "{} backups have ids starting with {prefix}: {}",
            ids.len(),
            ids.join(", ")
        ),
        fix,
    )
    .details(serde_json::json!({ "prefix": prefix, "candidates": ids }));
    for (id, target) in rest {
        error = error.alternative(argv(id, target));
    }
    error
}

/// How a backup's `WTF/` compares with its manifest: the entry set, kinds and sizes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeCheck {
    pub entries: u64,
    /// In the manifest, not in the tree.
    pub missing: u64,
    /// In the tree, not in the manifest.
    pub extra: u64,
    /// Another kind, or a file of another size.
    pub mismatched: u64,
    /// The first entry that differs, relative to `WTF/` (callers redact it).
    pub first: Option<String>,
}

impl TreeCheck {
    pub fn complete(&self) -> bool {
        self.missing == 0 && self.extra == 0 && self.mismatched == 0
    }
}

/// Compares `b`'s `WTF/` with its manifest without reading any file's content (a
/// restore's staging and `backup verify` call it; listing does not).
pub fn check_structure(b: &BackupRef) -> Result<TreeCheck> {
    let path = b.dir.join("manifest.json");
    let manifest = store::load::<Manifest>(&path)?
        .filter(|loaded| loaded.status != VersionStatus::Newer)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::StateCorrupt,
                format!("backup {} has no readable manifest", b.id),
                Suggestion::command(["omaddons", "backup", "list"]),
            )
        })?
        .value;
    let walked =
        tree::walk(&b.wtf(), BACKUP_TREE_MAX_ENTRIES, BACKUP_TREE_MAX_DEPTH).map_err(|e| {
            let names = manifest.entries.iter().map(|e| e.path.as_str());
            e.error(&crate::privacy::WtfPathRedactor::for_paths(names))
        })?;
    let on_disk: BTreeMap<&str, (EntryKind, u64)> = walked
        .entries
        .iter()
        .map(|e| (e.path.as_str(), (e.kind, e.size)))
        .collect();
    let mut check = TreeCheck {
        entries: manifest.entries.len() as u64,
        ..TreeCheck::default()
    };
    let note = |check: &mut TreeCheck, path: &str| {
        if check.first.is_none() {
            check.first = Some(path.to_owned());
        }
    };
    let mut listed = BTreeSet::new();
    for entry in &manifest.entries {
        listed.insert(entry.path.as_str());
        match on_disk.get(entry.path.as_str()) {
            None => {
                check.missing += 1;
                note(&mut check, &entry.path);
            }
            Some((kind, size)) => {
                let size_differs = entry.kind == EntryKind::File && *size != entry.size;
                if *kind != entry.kind || size_differs {
                    check.mismatched += 1;
                    note(&mut check, &entry.path);
                }
            }
        }
    }
    for path in on_disk.keys().filter(|p| !listed.contains(*p)) {
        check.extra += 1;
        note(&mut check, path);
    }
    Ok(check)
}

/// Whether `wtf` holds a SavedVariables file, account-wide or a character's: the settings
/// a backup is for. Names match ignoring case, as Wine's do; it reads one directory per
/// account, realm and character, never a file.
pub fn has_saved_variables(wtf: &Path) -> bool {
    let dirs = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|listing| {
                listing
                    .filter_map(std::result::Result::ok)
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default()
    };
    let named = |dir: &Path, name: &str| -> Vec<PathBuf> {
        dirs(dir)
            .into_iter()
            .filter(|d| d.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)))
            .collect()
    };
    let holds_lua = |dir: &Path| {
        std::fs::read_dir(dir).is_ok_and(|mut listing| {
            listing.any(|e| {
                e.is_ok_and(|e| {
                    e.file_type().is_ok_and(|t| t.is_file())
                        && Path::new(&e.file_name())
                            .extension()
                            .is_some_and(|x| x.eq_ignore_ascii_case("lua"))
                })
            })
        })
    };
    let saved_variables = |dir: &Path| named(dir, "SavedVariables").iter().any(|sv| holds_lua(sv));
    named(wtf, "Account").iter().any(|accounts| {
        dirs(accounts).iter().any(|account| {
            saved_variables(account)
                || dirs(account).iter().any(|realm| {
                    dirs(realm)
                        .iter()
                        .any(|character| saved_variables(character))
                })
        })
    })
}

/// Where a backup lives.
pub fn backup_dir(client: &ClientPaths, id: &BackupId) -> PathBuf {
    client.backups().join(id.to_string())
}

/// Whether `dir` holds a backup's metadata file (for tests and seeds).
pub fn meta_file(dir: &Path) -> PathBuf {
    dir.join("backup.json")
}

#[cfg(test)]
pub(crate) mod testing {
    //! Seeded backups for unit tests: written with `std::fs`, as a test may.

    use super::*;

    pub fn meta(id: BackupId, captured_at: &str, kind: BackupKind) -> BackupMeta {
        let at: Timestamp = captured_at.parse().unwrap();
        BackupMeta {
            id,
            target: "01JB8Q2V9W0000000000000000".parse().unwrap(),
            target_id: "retail".into(),
            family: Family::Mainline,
            created_at: at,
            captured_at: at,
            kind,
            // ubs:ignore — comparing backup kinds, not secrets
            trigger: (kind == BackupKind::Auto).then_some(Trigger::PostSession),
            actor: Actor::Human,
            reason: None,
            client_version: "12.1.5.70391".into(),
            client_running: false,
            files: 1,
            dirs: 1,
            symlinks: 0,
            skipped: 0,
            bytes: 7,
            reflinked: Reflinked::All,
            unsettled: 0,
            consistency: Consistency::Stable,
            integrity: Integrity::MetadataOnly,
            verified_at: None,
            baseline: None,
            tool_version: "0.1.0".into(),
            source: None,
            partial: false,
            wtf_outside_client: false,
        }
    }

    /// A backup of `client` with one `WTF/Config.wtf` file and its manifest.
    pub fn seed(client: &ClientPaths, meta: &BackupMeta) -> PathBuf {
        let dir = backup_dir(client, &meta.id);
        std::fs::create_dir_all(dir.join("WTF")).unwrap();
        std::fs::write(dir.join("WTF/Config.wtf"), b"SET a 1").unwrap();
        std::fs::write(meta_file(&dir), store::encode(meta).unwrap()).unwrap();
        let manifest = Manifest {
            entries: vec![ManifestEntry {
                path: "Config.wtf".into(),
                kind: EntryKind::File,
                size: 7,
                mode: 0o644,
                mtime_ns: 0,
                link: None,
                sha256: None,
            }],
        };
        std::fs::write(dir.join("manifest.json"), store::encode(&manifest).unwrap()).unwrap();
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{meta, seed};
    use super::*;

    const A: &str = "01M6Q2VA000000000000000000";
    const B: &str = "01M6Q2VB000000000000000000";
    const C: &str = "01M6Q3XC000000000000000000";

    fn id(s: &str) -> BackupId {
        s.parse().unwrap()
    }

    fn client() -> (tempfile::TempDir, ClientPaths) {
        let dir = tempfile::tempdir().unwrap();
        let client = ClientPaths::new(dir.path());
        (dir, client)
    }

    #[test]
    fn listing_is_newest_capture_first_and_damage_never_fails_it() {
        let (_dir, c) = client();
        seed(&c, &meta(id(A), "2026-10-18T21:15:00Z", BackupKind::Manual));
        seed(&c, &meta(id(B), "2026-10-20T18:02:00Z", BackupKind::Auto));
        let broken = seed(&c, &meta(id(C), "2026-10-19T00:00:00Z", BackupKind::Manual));
        std::fs::write(meta_file(&broken), b"{ not json").unwrap();
        let no_manifest = backup_dir(&c, &id("01M6Q4ZZ000000000000000000"));
        seed(
            &c,
            &meta(
                id("01M6Q4ZZ000000000000000000"),
                "2026-10-17T00:00:00Z",
                BackupKind::Manual,
            ),
        );
        std::fs::remove_file(no_manifest.join("manifest.json")).unwrap();
        std::fs::create_dir_all(c.backups().join("not-a-backup")).unwrap();
        std::fs::write(c.backups().join("01M6Q5AA000000000000000000"), b"a file").unwrap();

        let (found, warnings) = list(&c);
        assert!(warnings.is_empty());
        let ids: Vec<String> = found.iter().map(|b| b.id.to_string()).collect();
        assert_eq!(ids, [B, A, "01M6Q4ZZ000000000000000000", C]);
        let flags: Vec<(bool, bool)> = found.iter().map(|b| (b.restorable, b.qualifying)).collect();
        assert_eq!(
            flags,
            [(true, true), (true, true), (false, true), (false, false)]
        );
        assert!(found[3].meta.is_none());
        assert!(found.iter().all(|b| !b.busy));
    }

    #[test]
    fn what_qualifies_as_a_checkpoint() {
        let base = meta(id(A), "2026-10-18T21:15:00Z", BackupKind::Manual);
        assert!(qualifying(&base));
        for kind in [BackupKind::Auto, BackupKind::BeforeRestore] {
            assert!(
                qualifying(&BackupMeta {
                    kind,
                    ..base.clone()
                }),
                "{kind:?}"
            );
        }
        for kind in [BackupKind::Imported, BackupKind::Recovered] {
            assert!(
                !qualifying(&BackupMeta {
                    kind,
                    ..base.clone()
                }),
                "{kind:?}"
            );
        }
        assert!(!qualifying(&BackupMeta {
            partial: true,
            ..base.clone()
        }));
        assert!(!qualifying(&BackupMeta {
            consistency: Consistency::BestEffort,
            ..base.clone()
        }));
    }

    #[test]
    fn damaged_outside_and_busy_backups_are_flagged() {
        let (_dir, c) = client();
        let damaged = BackupMeta {
            integrity: Integrity::Damaged,
            ..meta(id(A), "2026-10-18T21:15:00Z", BackupKind::Manual)
        };
        seed(&c, &damaged);
        let outside = BackupMeta {
            wtf_outside_client: true,
            ..meta(id(B), "2026-10-19T21:15:00Z", BackupKind::Manual)
        };
        seed(&c, &outside);
        let held = seed(&c, &meta(id(C), "2026-10-20T21:15:00Z", BackupKind::Manual));
        let _reader = DirLock::shared(&held).unwrap();
        let (found, _) = list(&c);
        let flags: Vec<(bool, bool)> = found.iter().map(|b| (b.restorable, b.busy)).collect();
        assert_eq!(flags, [(true, true), (false, false), (false, false)]);
        assert!(
            DirLock::try_exclusive(&held).unwrap().is_none(),
            "listing let it be"
        );
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn prefixes_resolve_from_eight_characters() {
        let (_dir, c) = client();
        // A and B share their first 8 characters, C only its first 5 with them.
        let (a, b) = ("01M6Q2VA000000000000000000", "01M6Q2VA100000000000000000");
        seed(&c, &meta(id(a), "2026-10-18T21:15:00Z", BackupKind::Manual));
        seed(&c, &meta(id(b), "2026-10-19T21:15:00Z", BackupKind::Manual));
        seed(&c, &meta(id(C), "2026-10-20T21:15:00Z", BackupKind::Manual));
        assert_eq!(resolve(&c, "retail", "01M6Q3XC").unwrap().id, id(C));
        assert_eq!(
            resolve(&c, "retail", "01m6q2va1").unwrap().id,
            id(b),
            "any case"
        );
        assert_eq!(
            resolve(&c, "retail", "01M6Q3X").unwrap_err().code,
            ErrorCode::Usage,
            "seven characters"
        );

        let two = resolve(&c, "retail", "01M6Q2VA").unwrap_err();
        assert_eq!(two.code, ErrorCode::RefAmbiguous);
        let body = crate::error::ErrorBody::from(two);
        assert_eq!(body.details["candidates"], serde_json::json!([b, a]));

        let unknown = resolve(&c, "retail", "01ZZZZZZ").unwrap_err();
        assert_eq!(unknown.code, ErrorCode::NotFound);
        let body = crate::error::ErrorBody::from(unknown);
        assert_eq!(body.details["newest"], serde_json::json!([C, b, a]));
        assert_eq!(
            body.fix_argv,
            Some(argv(&["omaddons", "backup", "list", "-t", "retail"]))
        );
    }

    #[test]
    fn an_ambiguous_prefix_names_every_candidate() {
        let error = ambiguous("01M6Q2V0", vec![(id(A), "retail"), (id(B), "era")]);
        assert_eq!(error.code, ErrorCode::RefAmbiguous);
        let body = crate::error::ErrorBody::from(error);
        assert_eq!(
            body.fix_argv,
            Some(argv(&["omaddons", "backup", "list", A, "-t", "retail"]))
        );
        assert_eq!(body.alternatives.len(), 1);
        assert_eq!(
            body.alternatives[0].argv(),
            Some(argv(&["omaddons", "backup", "list", B, "-t", "era"]).as_slice())
        );
    }

    #[test]
    fn saved_variables_are_found_account_wide_or_per_character_in_any_case() {
        let dir = tempfile::tempdir().unwrap();
        let wtf = dir.path().join("WTF");
        assert!(!has_saved_variables(&wtf), "no WTF/");
        std::fs::create_dir_all(wtf.join("Account/FAKEACCT/SavedVariables")).unwrap();
        std::fs::write(wtf.join("Config.wtf"), b"SET a 1").unwrap();
        assert!(!has_saved_variables(&wtf), "settings, but no addon's");
        std::fs::write(wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua.bak"), b"").unwrap();
        assert!(!has_saved_variables(&wtf), "a .bak alone");
        let character = wtf.join("Account/FAKEACCT/FakeRealm/FakeChar/savedvariables");
        std::fs::create_dir_all(&character).unwrap();
        std::fs::write(character.join("Foo.LUA"), b"FooDB = {}").unwrap();
        assert!(has_saved_variables(&wtf), "a character's, any case");
        std::fs::remove_file(character.join("Foo.LUA")).unwrap();
        std::fs::write(wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua"), b"").unwrap();
        assert!(has_saved_variables(&wtf), "account-wide");
    }

    #[test]
    fn structure_checks_compare_entries_kinds_and_sizes() {
        let (_dir, c) = client();
        let dir = seed(&c, &meta(id(A), "2026-10-18T21:15:00Z", BackupKind::Manual));
        let (found, _) = list(&c);
        let b = &found[0];
        assert_eq!(
            check_structure(b).unwrap(),
            TreeCheck {
                entries: 1,
                ..TreeCheck::default()
            }
        );
        std::fs::write(dir.join("WTF/Config.wtf"), b"SET a 12").unwrap();
        std::fs::write(dir.join("WTF/extra.txt"), b"").unwrap();
        let check = check_structure(b).unwrap();
        assert_eq!((check.mismatched, check.extra, check.missing), (1, 1, 0));
        assert!(!check.complete());
        assert_eq!(check.first.as_deref(), Some("Config.wtf"));
        std::fs::remove_file(dir.join("WTF/Config.wtf")).unwrap();
        assert_eq!(check_structure(b).unwrap().missing, 1);
        std::fs::remove_file(dir.join("manifest.json")).unwrap();
        assert_eq!(
            check_structure(b).unwrap_err().code,
            ErrorCode::StateCorrupt
        );
    }
}
