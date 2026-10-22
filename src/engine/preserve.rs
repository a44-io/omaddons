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

use super::journal::{FileId, JournalLine, Prepared, PreserveIntent, Preserved, same};
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
    let meta = if std::fs::symlink_metadata(&published).is_ok() {
        // Published already: the items must be there, as recorded.
        for item in &intent.items {
            if !same(
                FileId::of(&place(&published, &item.rel)),
                Some(item.identity),
            ) {
                return Err(surprise(&published));
            }
        }
        store::load::<BackupMeta>(&crate::backup::meta_file(&published))?
            .map(|loaded| loaded.value)
            .ok_or_else(|| surprise(&published))?
    } else {
        move_items(s, intent)?;
        failpoint::hit("engine.preserve.moved").map_err(|e| io(&intent.assembly, &e))?;
        let meta = describe(s, paths, prepared, intent)?;
        publish(s, &client, intent, &published)?;
        failpoint::hit("engine.preserve.published").map_err(|e| io(&published, &e))?;
        meta
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
fn place(dir: &Path, rel: &str) -> PathBuf {
    let tree = dir.join("WTF");
    if rel.is_empty() { tree } else { tree.join(rel) }
}

/// Moves each item from where recovery found it to its place in the assembly (`0700`,
/// as every backup directory is): an item there already is skipped, one in neither place
/// is an error. Whole trees move with their stamp digest (M3-D25).
fn move_items(s: &mut WriteSession<'_>, intent: &PreserveIntent) -> Result<()> {
    for item in &intent.items {
        let to = place(&intent.assembly, &item.rel);
        if same(FileId::of(&to), Some(item.identity)) {
            continue;
        }
        if !same(FileId::of(&item.path), Some(item.identity)) {
            return Err(surprise(&item.path));
        }
        ensure_dir_all(s, &intent.assembly)?;
        mutate(s, &intent.assembly, Op::Chmod { mode: DIR_MODE })?;
        if let Some(parent) = to.parent() {
            ensure_dir_all(s, parent)?;
        }
        let tree = std::fs::symlink_metadata(&item.path).is_ok_and(|m| m.is_dir());
        let stamp_before = if tree {
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
fn described_client(prepared: &Prepared) -> Result<(String, crate::rules::Family, String)> {
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
/// names only the backup and the target, never an account, realm or character.
pub(super) fn rolled_back(prepared: &Prepared, intent: &PreserveIntent) -> Warning {
    let target = prepared.client.as_ref().map_or_else(
        || prepared.target_key.to_string(),
        |client| client.target_id.clone(),
    );
    Warning::new(
        WarningCode::RestoreRolledBack,
        format!(
            "{target}: an interrupted restore was rolled back; what it had displaced, which no other copy held, is kept as backup {}",
            intent.id
        ),
    )
    .with_fix(Suggestion::command([
        "omaddons".to_owned(),
        "backup".to_owned(),
        "restore".to_owned(),
        intent.id.to_string(),
        "-t".to_owned(),
        target,
    ]))
}

/// `message` with every account, realm and character name `prepared`'s client and stage
/// hold replaced by its placeholder, for a stop's cause.
pub(super) fn redacted(prepared: &Prepared, message: &str) -> String {
    let live = WtfPathRedactor::for_client(&ClientPaths::new(&prepared.client_dir));
    let staged = WtfPathRedactor::for_client(&ClientPaths::new(&prepared.commit.stage_dir));
    staged.text(&live.text(message))
}
