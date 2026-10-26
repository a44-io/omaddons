//! Preservation (M3-D24 (c) rule 3): a durable home, as one `recovered` backup, for every
//! version a `writes_wtf` rollback displaced that no other copy holds, before recovery
//! prunes anything.
//!
//! The journal's `preserve` line is a write-ahead intent with a backup id reserved there;
//! [`run`] carries it out in steps that each check identities and skip what is done
//! (move the items into an assembly under the stage, describe it, publish it with one
//! rename into `backups/`), then closes it with `preserved`. A recovery that dies
//! anywhere in between resumes from the journal with the same id, and a listing never
//! sees a half-made backup: `backups/` gains the assembly whole, or not at all.
//!
//! This module only renames and writes. It never prunes: cleanup stays the journal's,
//! once every intent is closed. Any surprise is an error, which the caller turns into a
//! stop with everything kept.

use std::path::{Path, PathBuf};

use super::journal::{
    Disk, FileId, JournalLine, LiveView, Prepared, PreserveIntent, PreserveItem, Preserved,
};
use crate::backup::create::{DIR_MODE, addons, rename_new, write};
use crate::backup::inventory::{Change, Ring};
use crate::backup::tree::walk;
use crate::backup::{
    BackupKind, BackupMeta, BackupSource, Consistency, EntryKind, Integrity, Manifest, Reflinked,
};
use crate::error::{Error, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    MutateOpts, Op, RenameMode, StampDigest, WriteSession, ensure_dir_all, failpoint, mutate,
    mutate_with, sync_filesystem,
};
use crate::limits::{BACKUP_TREE_MAX_DEPTH, BACKUP_TREE_MAX_ENTRIES};
use crate::privacy::WtfPathRedactor;
use crate::state::paths::{ClientPaths, Paths};
use crate::state::store;

/// Carries out `intent`, journaled in `journal` for `prepared`'s rollback, from its first
/// step not done: decided from the end, so a resume after the publishing rename goes
/// straight to completion. Moves skip items already assembled; describing rewrites the
/// metadata; publishing is one `RENAME_NOREPLACE`. Then `preserved`, and the backup into
/// the inventory projection (best effort).
pub(super) fn run(
    s: &mut WriteSession<'_>,
    paths: &Paths,
    journal: &Path,
    prepared: &Prepared,
    intent: &PreserveIntent,
) -> Result<()> {
    let client = ClientPaths::new(&prepared.client_dir);
    let published = crate::backup::backup_dir(&client, &intent.id);
    let meta = match published_matches(&mut Disk, &published, intent) {
        // Published already: the items must be there, as recorded.
        Some(true) => store::load::<BackupMeta>(&crate::backup::meta_file(&published))?
            .map(|loaded| loaded.value)
            .ok_or_else(|| surprise(&published))?,
        Some(false) => return Err(surprise(&published)),
        None => {
            move_items(s, intent)?;
            failpoint::hit("engine.preserve.moved").map_err(|e| io(&intent.assembly, &e))?;
            let meta = describe(s, paths, prepared, intent)?;
            publish(s, &client, intent, &published)?;
            failpoint::hit("engine.preserve.published").map_err(|e| io(&published, &e))?;
            meta
        }
    };
    let done = JournalLine::Preserved(Preserved { id: intent.id });
    super::journal::append_line(s, journal, &done)?;
    let ring = Ring {
        command: "recovery",
        actor: prepared.commit.history.actor,
    };
    let change = Change::Added(Box::new(meta));
    crate::backup::inventory::record(paths, &client, &prepared.target_key, change, ring);
    Ok(())
}

/// Where an item `rel` sits under a backup (or assembly) `dir`: its `WTF/`, or a file in it.
pub(super) fn place(dir: &Path, rel: &str) -> PathBuf {
    let tree = dir.join("WTF");
    if rel.is_empty() { tree } else { tree.join(rel) }
}

/// Whether the backup `intent` publishes is at `published` with every item in place, as
/// [`run`] checks before resuming: `None` when nothing is there (it is still to publish).
pub(super) fn published_matches(
    view: &mut dyn LiveView,
    published: &Path,
    intent: &PreserveIntent,
) -> Option<bool> {
    view.entry(published)?;
    Some(
        intent
            .items
            .iter()
            .all(|item| FileId::same(view.id(&place(published, &item.rel)), Some(item.identity))),
    )
}

/// Where an item of a preservation intent is, as [`move_items`] checks before each move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Placed {
    /// In the assembly, with its identity: moved already.
    InAssembly,
    /// Where recovery found it: still to move.
    AtFound,
    /// In neither place: a surprise.
    Neither,
}

/// [`Placed`] for `item` of the intent assembled at `assembly`: the assembly is checked
/// first.
pub(super) fn placed(view: &mut dyn LiveView, item: &PreserveItem, assembly: &Path) -> Placed {
    if FileId::same(view.id(&place(assembly, &item.rel)), Some(item.identity)) {
        Placed::InAssembly
    } else if FileId::same(view.id(&item.path), Some(item.identity)) {
        Placed::AtFound
    } else {
        Placed::Neither
    }
}

/// Moves each item from where recovery found it to its place in the assembly (`0700`,
/// as every backup directory is): an item there already is skipped, one in neither place
/// is an error. Whole trees move with their stamp digest (M3-D25) when `s` logs actions.
fn move_items(s: &mut WriteSession<'_>, intent: &PreserveIntent) -> Result<()> {
    for item in &intent.items {
        let to = place(&intent.assembly, &item.rel);
        match placed(&mut Disk, item, &intent.assembly) {
            Placed::InAssembly => continue,
            Placed::Neither => return Err(surprise(&item.path)),
            Placed::AtFound => {}
        }
        ensure_dir_all(s, &intent.assembly)?;
        mutate(s, &intent.assembly, Op::Chmod { mode: DIR_MODE })?;
        if let Some(parent) = to.parent() {
            ensure_dir_all(s, parent)?;
        }
        // A stamp digest is a stat walk of the whole tree, which only a session that logs
        // its actions records; recovery's sessions log none. It decides what the actions
        // log records, never an outcome, so it reads the disk directly.
        let tree = std::fs::symlink_metadata(&item.path).is_ok_and(|m| m.is_dir());
        let stamp_before = if tree && s.actions_log().is_some() {
            Some(StampDigest::compute(&item.path).map_err(|e| {
                Error::io(&item.path, &e, Suggestion::command(["omaddons", "status"]))
            })?)
        } else {
            None
        };
        let opts = MutateOpts {
            stamp_before,
            ..MutateOpts::default()
        };
        let op = Op::Rename {
            to: &to,
            mode: RenameMode::NoReplace,
        };
        mutate_with(s, &item.path, op, opts)?;
    }
    Ok(())
}

/// Writes the assembly's `manifest.json` (a bounded stat walk of its `WTF/`),
/// `addons.json` and `backup.json` (`recovered`, `best_effort`, `metadata_only`: it never
/// claims more), then one `syncfs`. Rewritten whole when present.
fn describe(
    s: &mut WriteSession<'_>,
    paths: &Paths,
    prepared: &Prepared,
    intent: &PreserveIntent,
) -> Result<BackupMeta> {
    let names = WtfPathRedactor::for_client(&ClientPaths::new(&intent.assembly));
    let walked = walk(
        &intent.assembly.join("WTF"),
        BACKUP_TREE_MAX_ENTRIES,
        BACKUP_TREE_MAX_DEPTH,
    )
    .map_err(|e| e.error(&names))?;
    let count = |kind: EntryKind| walked.entries.iter().filter(|e| e.kind == kind).count() as u64;
    let bytes = walked
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .map(|e| e.size)
        .sum();
    let (target_id, family, client_version) = described_client(prepared)?;
    let now = crate::clock::now();
    let meta = BackupMeta {
        id: intent.id,
        target: prepared.target_key,
        target_id,
        family,
        created_at: now,
        captured_at: now,
        kind: BackupKind::Recovered,
        trigger: None,
        actor: prepared.commit.history.actor,
        reason: None,
        client_version,
        client_running: false,
        files: count(EntryKind::File),
        dirs: count(EntryKind::Dir),
        symlinks: count(EntryKind::Symlink),
        skipped: walked.skipped,
        bytes,
        // Moved, never copied: the files take no space they did not take before.
        reflinked: Reflinked::All,
        unsettled: 0,
        consistency: Consistency::BestEffort,
        integrity: Integrity::MetadataOnly,
        verified_at: None,
        baseline: None,
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        source: Some(BackupSource::Recovery {
            restore_tx: prepared.tx,
        }),
        partial: intent.items.iter().any(|item| !item.rel.is_empty()),
        wtf_outside_client: false,
    };
    let manifest = Manifest {
        entries: walked.entries,
    };
    write(s, &intent.assembly.join("manifest.json"), &manifest)?;
    write(
        s,
        &intent.assembly.join("addons.json"),
        &addons(paths, &prepared.target_key)?,
    )?;
    write(s, &crate::backup::meta_file(&intent.assembly), &meta)?;
    sync_filesystem(&intent.assembly).map_err(Error::from)?;
    Ok(meta)
}

/// The target id, family and client version a recovered backup is described with: as the
/// transaction recorded them, else as its source backup did.
pub(super) fn described_client(
    prepared: &Prepared,
) -> Result<(String, crate::rules::Family, String)> {
    if let Some(client) = &prepared.client {
        return Ok((
            client.target_id.clone(),
            client.family,
            client.version.clone(),
        ));
    }
    let source = prepared.source_backup.and_then(|source| {
        let client = ClientPaths::new(&prepared.client_dir);
        let dir = crate::backup::backup_dir(&client, &source.id);
        store::load::<BackupMeta>(&crate::backup::meta_file(&dir))
            .ok()
            .flatten()
    });
    source
        .map(|loaded| {
            let meta = loaded.value;
            (meta.target_id, meta.family, meta.client_version)
        })
        .ok_or_else(|| {
            Error::internal(format!(
                "transaction {} does not say which client it changed, so what it displaced cannot be described yet",
                prepared.tx
            ))
        })
}

/// Publishes the assembly as `backups/<id>` with one rename onto a path that must not
/// exist: a listing sees the whole backup or none of it.
fn publish(
    s: &mut WriteSession<'_>,
    client: &ClientPaths,
    intent: &PreserveIntent,
    published: &Path,
) -> Result<()> {
    let backups = client.backups();
    ensure_dir_all(s, &backups)?;
    mutate(s, &backups, Op::Chmod { mode: DIR_MODE })?;
    mutate(s, &intent.assembly, Op::Chmod { mode: DIR_MODE })?;
    rename_new(s, &intent.assembly, published)
}

/// An I/O error at `path` (a failpoint's, here).
fn io(path: &Path, e: &std::io::Error) -> Error {
    Error::io(path, e, Suggestion::command(["omaddons", "status"]))
}

/// An item, or the published backup, is not what the intent recorded.
fn surprise(path: &Path) -> Error {
    Error::internal(format!(
        "{} is not what the preservation intent recorded",
        path.display()
    ))
}

/// `restore_rolled_back`: an interrupted restore was rolled back, and what it had
/// displaced, which no other copy held, is kept as the backup `intent` published. It
/// names only the backup, the target and the addon folders, never an account, realm or
/// character. A backup of single files is partial, so its fix restores them by folder
/// (`--addon`), the only way it restores.
pub(super) fn rolled_back(prepared: &Prepared, intent: &PreserveIntent) -> Warning {
    let target = prepared.client.as_ref().map_or_else(
        || prepared.target_key.to_string(),
        |client| client.target_id.clone(),
    );
    let mut argv = vec![
        "omaddons".to_owned(),
        "backup".to_owned(),
        "restore".to_owned(),
        intent.id.to_string(),
        "-t".to_owned(),
        target.clone(),
    ];
    let mut folders: Vec<String> = intent
        .items
        .iter()
        .filter_map(|item| super::undo::sv_folder(&item.rel))
        .map(|folder| folder.as_str().to_owned())
        .collect();
    folders.sort_by_key(|f| f.to_ascii_lowercase());
    folders.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    for folder in folders {
        argv.extend(["--addon".to_owned(), folder]);
    }
    Warning::new(
        WarningCode::RestoreRolledBack,
        format!(
            "{target}: an interrupted restore was rolled back; what it had displaced, which no other copy held, is kept as backup {}",
            intent.id
        ),
    )
    .with_fix(Suggestion::command(argv))
}

/// `message` with every account, realm and character name `prepared`'s client and stage
/// hold replaced by its placeholder, for a stop's cause.
pub(super) fn redacted(prepared: &Prepared, message: &str) -> String {
    let live = WtfPathRedactor::for_client(&ClientPaths::new(&prepared.client_dir));
    let staged = WtfPathRedactor::for_client(&ClientPaths::new(&prepared.commit.stage_dir));
    staged.text(&live.text(message))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::engine::journal::FileId;
    use crate::fsx::Writer;

    /// Recovery's sessions log no actions, so a whole tree moves into the assembly without
    /// the stat walk of a stamp digest nobody would record: one holding a folder that
    /// cannot be listed, which that walk cannot read, moves all the same.
    #[test]
    fn an_untracked_move_takes_no_stamp_of_the_tree() {
        let scene = crate::engine::testing::Scene::new();
        let stage = scene.client.stage_wtf(&scene.tx);
        let tree = stage.join("WTF");
        let sealed = tree.join("Account/Sealed");
        std::fs::create_dir_all(&sealed).unwrap();
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            StampDigest::compute(&tree).is_err(),
            "the walk cannot read it"
        );
        let intent = PreserveIntent {
            id: "01M6X00000000000000000000A".parse().unwrap(),
            items: vec![PreserveItem {
                path: tree.clone(),
                identity: FileId::of(&tree).unwrap(),
                rel: String::new(),
            }],
            assembly: stage.join("recovered/01M6X00000000000000000000A"),
        };
        let mut s = WriteSession::untracked(Writer::BackupRestore, &scene.scopes, "recovery");
        let moved = move_items(&mut s, &intent);
        let placed = place(&intent.assembly, "");
        std::fs::set_permissions(
            placed.join("Account/Sealed"),
            std::fs::Permissions::from_mode(0o700),
        )
        .ok();
        moved.unwrap();
        assert!(placed.is_dir() && !tree.exists());
    }

    #[test]
    fn placed_decides_like_move_items() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (found, assembly) = (root.join("found"), root.join("assembly"));
        std::fs::create_dir_all(&found).unwrap();
        let tree = PreserveItem {
            path: found.clone(),
            identity: FileId::of(&found).unwrap(),
            rel: String::new(),
        };
        let file = root.join("X.lua");
        std::fs::write(&file, "x").unwrap();
        let single = PreserveItem {
            path: file.clone(),
            identity: FileId::of(&file).unwrap(),
            rel: "Account/X.lua".into(),
        };
        let at = |item: &PreserveItem| placed(&mut Disk, item, &assembly);

        assert_eq!(at(&tree), Placed::AtFound, "at_found");
        std::fs::create_dir_all(&assembly).unwrap();
        std::fs::rename(&found, place(&assembly, "")).unwrap();
        assert_eq!(at(&tree), Placed::InAssembly, "in_assembly");
        std::fs::rename(place(&assembly, ""), root.join("elsewhere")).unwrap();
        assert_eq!(at(&tree), Placed::Neither, "neither: moved elsewhere");

        // One file in both places (a hard link): the assembly is checked first.
        let linked = place(&assembly, &single.rel);
        std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
        std::fs::hard_link(&file, &linked).unwrap();
        assert_eq!(at(&single), Placed::InAssembly, "both");
    }

    #[test]
    fn published_matches_like_run() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let published = root.join("backups/01M6X00000000000000000000A");
        let items = ["Account/A.lua", "Account/B.lua"].map(|rel| {
            let path = place(&published, rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, rel).unwrap();
            PreserveItem {
                identity: FileId::of(&path).unwrap(),
                path: root.join("found").join(rel),
                rel: rel.into(),
            }
        });
        let intent = PreserveIntent {
            id: "01M6X00000000000000000000A".parse().unwrap(),
            items: items.to_vec(),
            assembly: root.join("assembly"),
        };
        let matches = || published_matches(&mut Disk, &published, &intent);

        assert_eq!(matches(), Some(true), "published_with_every_item");
        std::fs::remove_file(place(&published, "Account/B.lua")).unwrap();
        assert_eq!(matches(), Some(false), "published_missing_an_item");
        std::fs::remove_dir_all(&published).unwrap();
        assert_eq!(matches(), None, "nothing_published");
    }
}
