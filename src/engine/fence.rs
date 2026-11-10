//! What a stuck repair protects, and the fences every mutating command carries for it
//! (E14, M4-D62).
//!
//! A doctor repair names no single target, so a repair journal recovery cannot settle
//! is isolated by what it protects instead: its primitives' writes, the backups its
//! rollback reads, and, when it owns a barrier (a suspension or an activation), the
//! records of the targets behind it and the trees its guards stamped. Every mutating
//! command loads those as fences into its write-scope table ([`table`]), so the one
//! writer refuses exactly the writes that overlap them, and every other command, client
//! and path proceeds. A repair journal whose `prepared` line cannot be read has
//! unknowable resources: it closes the set, and every change is refused until it is set
//! aside. Read commands use unfenced tables and always run.
//!
//! - **Loading** ([`load`]): from `journal::diagnose`'s repair verdicts, once per table.
//! - **Under the lock** ([`under_lock`]): a commit checks again, after its recovery, for a
//!   repair that became stuck after it built its table, and for its own first-hand writes.
//! - **Warnings** ([`stuck_warning`]): readers and recovery report one text per stuck
//!   repair, so a run reports it once.
//!
//! This module reads and decides; it never writes.

use std::path::{Path, PathBuf};

use super::barrier::{self, Stuck};
use super::journal::{self, Expected, JournalDiagnosis, JournalFamily, JournalVerdict, StopReason};
use super::repair::{PreparedRead, RepairPrepared, read_prepared};
use super::repair_action::{Guard, Primitive};
use super::resource::{PathKind, Resource, ResourceError, ResourceSet};
use super::simulate::cut_below_wtf;
use crate::doctor::registry::ids;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{FenceSet, FenceStop, Fenced, FsxError, ScopeTable, canonical_lenient};
use crate::model::{Target, TargetKey, TxId};
use crate::state::paths::{ClientPaths, Paths};

/// What `prepared`'s repair protects while it is stuck: writes only.
///
/// 1. every resource its `prepared` line records as written;
/// 2. for every write that keeps a backup, that backup's file, and its run's backup input
///    by name (what `doctor gc` pins);
/// 3. when it owns a barrier ([`barrier::of`]): the record of every target it names, and
///    for each guard what it stamped: an `epoch`'s tree, a `wtf_epoch`'s client `WTF/`
///    (that target's record when its client directory is not named), a `generation`'s or
///    `marker`'s record.
///
/// A recorded resource that is not one is an error: the caller then fails closed.
pub fn protected(
    prepared: &RepairPrepared,
    paths: &Paths,
) -> std::result::Result<ResourceSet, ResourceError> {
    let mut set = ResourceSet::new();
    for text in &prepared.resources.writes {
        set.write(Resource::parse(text)?);
    }
    for step in &prepared.primitives {
        if let Primitive::WriteFile {
            backup: Some(backup),
            ..
        } = &step.op
        {
            set.write(Resource::from_canonical(backup, PathKind::File)?);
            set.write(Resource::backup(&prepared.doctor_run_id, step.seq)?);
        }
    }
    if barrier::of(prepared, paths).is_some() {
        for key in &prepared.targets {
            set.write(Resource::Record(*key));
        }
        for guard in prepared.primitives.iter().flat_map(|step| &step.guards) {
            match guard {
                Guard::Epoch { path, .. } => {
                    set.write(Resource::from_canonical(path, PathKind::Subtree)?);
                }
                Guard::WtfEpoch { target, .. } => {
                    match prepared.client_dirs.iter().find(|c| c.target == *target) {
                        Some(client) => set.write(Resource::from_canonical(
                            &ClientPaths::new(&client.dir).wtf(),
                            PathKind::Subtree,
                        )?),
                        None => set.write(Resource::Record(*target)),
                    };
                }
                Guard::Generation { target, .. } | Guard::Marker { target, .. } => {
                    set.write(Resource::Record(*target));
                }
            }
        }
    }
    Ok(set)
}

/// The canonical fence paths of `set`'s writes, each with its `details.resource` text,
/// sorted and without duplicates. A path resource is taken as recorded, never resolved
/// again; a record is its record and files index; a journal its four names and its
/// in-flight lock; a backup input by name gives none (its file is a path resource of
/// its own). History and the request log are never fenced: both are shared append-only
/// files, and a fence on either would refuse every commit everywhere. Nothing below a
/// `WTF/` is named: such a path is cut after its `WTF` (or `WTF.previous`) component.
pub(crate) fn paths_of(set: &ResourceSet, paths: &Paths) -> Vec<(PathBuf, String)> {
    let never = [
        canonical_file(&paths.history_file()),
        canonical_file(&paths.net_log()),
    ];
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    let mut push = |path: PathBuf, text: Option<String>| {
        let cut = cut_below_wtf(&path);
        // ubs:ignore — comparing paths, not secrets
        let (path, text) = if cut == path {
            let text = text.unwrap_or_else(|| path.display().to_string());
            (path, text)
        } else {
            let text = format!("{}/", cut.display());
            (cut, text)
        };
        if !never.contains(&path) {
            out.push((path, text));
        }
    };
    for resource in set.writes() {
        match resource {
            Resource::Path { path, .. } => push(path.clone(), resource.text().ok()),
            Resource::Record(key) => {
                push(canonical_file(&paths.target_record(key)), None);
                push(canonical_file(&paths.files_index(key)), None);
            }
            Resource::Journal(tx) => {
                for path in journal_paths(paths, tx) {
                    push(canonical_file(&path), None);
                }
            }
            Resource::Backup { .. } => {}
        }
    }
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// What a journal's `protected` paths (`journal::diagnose`'s) stand for, as a resource
/// set, written or read: a directory on disk a subtree, any other path a file (the kind
/// only names it, for `details.resource`: overlap is the same either way). A path that
/// cannot be resolved stands for everything (fail closed).
pub(crate) fn resources_of(protected: &journal::ProtectedPaths) -> ResourceSet {
    let resource = |path: &PathBuf| {
        let kind = if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
            PathKind::Subtree
        } else {
            PathKind::File
        };
        Resource::canonical(path, kind)
            .or_else(|_| Resource::from_canonical(Path::new("/"), PathKind::Subtree))
    };
    let mut set = ResourceSet::new();
    for path in &protected.writes {
        if let Ok(r) = resource(path) {
            set.write(r);
        }
    }
    for path in &protected.reads {
        if let Ok(r) = resource(path) {
            set.read(r);
        }
    }
    set
}

/// Every file a journal of `tx` may be: its four names and its in-flight lock.
fn journal_paths(paths: &Paths, tx: &TxId) -> [PathBuf; 5] {
    [
        paths.journal(tx),
        paths.journal_wtf(tx),
        paths.journal_repair(tx),
        super::repair::provisional_journal(paths, tx),
        paths.inflight_lock(tx),
    ]
}

/// `path` with its parent resolved as `mutate()` resolves a target's, the last component
/// joined as it is; `path` itself when nothing of it resolves.
fn canonical_file(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => {
            canonical_lenient(parent).map_or_else(|| path.to_path_buf(), |p| p.join(name))
        }
        _ => path.to_path_buf(),
    }
}

/// What a stuck repair journal's fences are.
enum Protection {
    /// Its fence paths.
    Fences(Vec<(PathBuf, String)>),
    /// Its resources cannot be known: every change is refused.
    Closed,
    /// No complete line: nothing ran, nothing to protect.
    Nothing,
}

/// The [`Protection`] of the repair journal at `journal`: path fences when its `prepared`
/// line decodes (even with a later line this build cannot read: recovery keeps it whole,
/// and what it changed is known), closed when that line does not decode, declares a
/// newer major, breaks the activation rules, records a resource that is not one, or
/// cannot be read at all.
fn protection(paths: &Paths, journal: &Path) -> Protection {
    match read_prepared(paths, journal) {
        Ok(read) => protection_of(paths, &read),
        Err(_) => Protection::Closed,
    }
}

/// The [`Protection`] of a repair journal whose first line reads as `read`.
fn protection_of(paths: &Paths, read: &PreparedRead) -> Protection {
    match read {
        PreparedRead::Decoded(prepared) => match protected(prepared, paths) {
            Ok(set) => Protection::Fences(paths_of(&set, paths)),
            Err(_) => Protection::Closed,
        },
        PreparedRead::Absent => Protection::Nothing,
        PreparedRead::Undecodable | PreparedRead::TooNew => Protection::Closed,
    }
}

/// Whether the stuck repair journal at `journal` closes every command's fences (its
/// resources cannot be known), as [`load`] decides.
pub fn closes(paths: &Paths, journal: &Path) -> bool {
    matches!(protection(paths, journal), Protection::Closed)
}

/// [`closes`] for a repair journal whose first line was already read as `read`.
pub(crate) fn closes_read(paths: &Paths, read: &PreparedRead) -> bool {
    matches!(protection_of(paths, read), Protection::Closed)
}

/// The fences a mutating command honours now: for every repair journal whose writer is
/// gone and that recovery stops at, what it protects, with its own stop; and for every
/// journal of either family recovery would leave behind a barrier (E14), what it
/// protects, with the stop of the repair (or journal) it waits behind, so a folder
/// journal behind an owner is protected from every command, not only its own target's.
/// A recoverable or live one fences nothing (the next lock-taking command settles it),
/// nor does any other folder journal. A journal directory that cannot be listed closes
/// the set. Reads `journal::diagnose` only when a repair journal exists: otherwise one
/// listing.
pub fn load(paths: &Paths, client_dirs: &[PathBuf]) -> FenceSet {
    let mut set = FenceSet::new();
    match super::repair::journal::list(paths) {
        Ok(found) if found.is_empty() => return set,
        Ok(_) => {}
        Err(_) => {
            close_unlisted(&mut set, paths);
            return set;
        }
    }
    let diagnoses = journal::diagnose(paths, client_dirs, &journal::no_saved_variables);
    if diagnoses.unlisted.is_some() {
        close_unlisted(&mut set, paths);
    }
    // The stop of the journal `owner`, as its own fences would report it.
    let owner_stop = |owner: TxId| {
        let od = diagnoses.journals.iter().find(|od| od.tx == owner)?;
        let closed = od.family == JournalFamily::Repair && closes(paths, &od.path);
        stop_of(od, closed)
    };
    for d in &diagnoses.journals {
        match (d.family, d.verdict) {
            // Behind a barrier (E14), "fenced with that repair": what the journal itself
            // protects, with its owner's stop. Behind a live writer, nothing is fenced,
            // as for any live transaction.
            (JournalFamily::Folder, JournalVerdict::BehindBarrier { owner, .. }) => {
                let Some(stop) = owner_stop(owner) else {
                    continue;
                };
                let protected = resources_of(&d.protected);
                for (path, text) in paths_of(&protected, paths) {
                    set.fence(path, text, stop.clone());
                }
            }
            (JournalFamily::Repair, JournalVerdict::BehindBarrier { owner, .. }) => {
                if let Some(stop) = owner_stop(owner) {
                    protect(&mut set, d, protection(paths, &d.path), stop);
                }
            }
            (JournalFamily::Repair, _) => {
                let protection = protection(paths, &d.path);
                let closed = matches!(protection, Protection::Closed);
                if let Some(stop) = stop_of(d, closed) {
                    protect(&mut set, d, protection, stop);
                }
            }
            (JournalFamily::Folder, _) => {}
        }
    }
    set
}

/// Fences `set` with what the repair journal `d` protects, reporting `stop`.
fn protect(set: &mut FenceSet, d: &JournalDiagnosis, protection: Protection, stop: FenceStop) {
    match protection {
        Protection::Fences(fences) => {
            for (path, text) in fences {
                set.fence(path, text, stop.clone());
            }
        }
        Protection::Closed => set.close(format!("journal:{}", d.tx), stop),
        Protection::Nothing => {}
    }
}

/// Closes `set` for a journal directory that cannot be listed: any unfinished repair's
/// resources are unknowable.
fn close_unlisted(set: &mut FenceSet, paths: &Paths) {
    let dir = paths.journal_dir();
    set.close(
        dir.display().to_string(),
        FenceStop {
            tx: None,
            target: None,
            code: ErrorCode::Io,
            reason: Some(StopReason::Unattributed.as_str()),
            summary: format!(
                "the journal directory {} cannot be listed, so what an unfinished change protects cannot be known",
                dir.display()
            ),
            fix: explain_fix(Some(StopReason::Unattributed)),
        },
    );
}

/// The stop a fence of the stuck repair journal `d` reports: recovery's own (its code and
/// `details.reason`), the summary of [`stuck_warning`] (`closed` when its resources cannot
/// be read), and the `doctor explain` of its failure mode. `None` unless recovery stops
/// at it.
pub fn stop_of(d: &JournalDiagnosis, closed: bool) -> Option<FenceStop> {
    let Expected::Stops { code, reason, .. } = d.expected() else {
        return None;
    };
    Some(match d.family {
        JournalFamily::Repair => FenceStop {
            target: d.target,
            ..repair_stop(d.tx, code, Some(reason), closed)
        },
        // A folder journal a pass-3 repair waits behind (E14).
        JournalFamily::Folder => folder_stuck(d.tx, d.target, code, Some(reason)).stop,
    })
}

/// The stop of the repair `tx` recovery could not settle, with `code` and `reason`
/// (`None`: its recovery failed on I/O); `closed` when its resources cannot be read.
pub(crate) fn repair_stop(
    tx: TxId,
    code: ErrorCode,
    reason: Option<StopReason>,
    closed: bool,
) -> FenceStop {
    FenceStop {
        tx: Some(tx),
        target: None,
        code,
        reason: reason.map(StopReason::as_str),
        summary: summary(tx, reason, closed),
        fix: explain_fix(reason),
    }
}

/// The stop recovery reports for the repair `tx`, whose recovery failed with `error`:
/// its code, and its reason from its details.
pub(crate) fn stop_of_error(tx: TxId, error: &Error, closed: bool) -> FenceStop {
    let reason = error.details["reason"].as_str().and_then(StopReason::parse);
    repair_stop(tx, error.code, reason, closed)
}

/// `omaddons doctor explain <fm>` for a recovery stop with `reason`, its failure mode
/// the registry's (`doctor::registry::ids::for_stop_reason`); without a reason (recovery
/// itself failed on I/O, and the next lock-taking command retries) the pending journal's.
/// No `-t`: a repair has no target, and `doctor explain <fm>` lists every instance.
pub(crate) fn explain_fix(reason: Option<StopReason>) -> Suggestion {
    let fm = reason.map_or(ids::JOURNAL_PENDING, ids::for_stop_reason);
    crate::doctor::registry::explain_fix(fm, None)
}

/// What stopped, as a sentence with exactly one parenthesized reason (E11's differential
/// reads a warning's outcome from it): `io` when recovery failed without a stop reason.
fn summary(tx: TxId, reason: Option<StopReason>, closed: bool) -> String {
    let why = reason.map_or("io", StopReason::as_str);
    match (closed, reason) {
        (true, _) => format!(
            "repair transaction {tx} did not finish, and this omaddons cannot read what it changed ({why})"
        ),
        (false, Some(_)) => {
            format!(
                "repair transaction {tx} did not finish, and recovery could not settle it ({why})"
            )
        }
        (false, None) => {
            format!("repair transaction {tx} did not finish, and its recovery failed ({why})")
        }
    }
}

/// The one `journal_pending` warning for a stuck repair, as readers and recovery both
/// report it (so a run reports it once). `closed`: its resources cannot be read. Without
/// a stop reason, recovery itself failed on I/O: nothing is fenced for it, and the next
/// lock-taking command tries again.
pub fn stuck_warning(tx: TxId, reason: Option<StopReason>, closed: bool) -> Warning {
    let rest = match (closed, reason) {
        (true, _) => "every change is refused until it is set aside",
        (false, Some(_)) => "writes to what it protects are refused, and everything else proceeds",
        (false, None) => "the next command that changes something tries again",
    };
    Warning::new(
        WarningCode::JournalPending,
        format!("{}; {rest}", summary(tx, reason, closed)),
    )
    .with_fix(explain_fix(reason))
}

// ---------------------------------------------------------------------------------------
// Journals behind a barrier (E14).

/// How a journal names itself in a behind text: `transaction <tx>`, with ` on target <t>`
/// when it has one.
pub(crate) fn folder_named(tx: TxId, target: Option<TargetKey>) -> String {
    match target {
        Some(target) => format!("transaction {tx} on target {target}"),
        None => format!("transaction {tx}"),
    }
}

/// `<waiting> waits behind <owner>, which recovery could not settle (<reason>); <rest>`,
/// or `which this omaddons cannot read (<reason>)` when `stuck`'s resources cannot be
/// read: one parenthesized reason, the stop's (`io` for a recovery that failed without
/// one).
fn behind_text(waiting: &str, owner: &str, stuck: &Stuck, rest: &str) -> String {
    let why = stuck.stop.reason.unwrap_or("io");
    let which = if stuck.closed {
        "which this omaddons cannot read"
    } else {
        "which recovery could not settle"
    };
    format!("{waiting} waits behind {owner}, {which} ({why}); {rest}")
}

/// Where a journal waits, for its behind text.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Behind {
    /// A folder journal behind the repair `owner`: left as it is.
    Folder {
        tx: TxId,
        target: Option<TargetKey>,
        owner: TxId,
    },
    /// A repair behind the repair `owner` that names it: what it protects is fenced.
    Repair { tx: TxId, owner: TxId },
    /// A repair whose resources overlap the journal `owner` pass 2 left unresolved.
    Unresolved { tx: TxId, owner: TxId },
}

/// The one `journal_pending` warning of a journal behind a barrier, as recovery and
/// readers both report it, with the stop of the repair (or journal) it waits behind and
/// that stop's `doctor explain` as its fix.
pub(crate) fn behind_warning(behind: Behind, stuck: &Stuck) -> Warning {
    let fenced = "writes to what it protects are refused";
    let text = match behind {
        Behind::Folder { tx, target, owner } => behind_text(
            &folder_named(tx, target),
            &format!("repair {owner}"),
            stuck,
            "it is left as it is",
        ),
        Behind::Repair { tx, owner } => behind_text(
            &format!("repair transaction {tx}"),
            &format!("repair {owner}"),
            stuck,
            fenced,
        ),
        Behind::Unresolved { tx, owner } => behind_text(
            &format!("repair transaction {tx}"),
            &format!("transaction {owner}"),
            stuck,
            fenced,
        ),
    };
    Warning::new(WarningCode::JournalPending, text).with_fix(stuck.stop.fix.clone())
}

/// What fails a command whose own target's folder journal `tx` (or one with no readable
/// target) waits behind the unsettled repair `owner`: that repair's own stop (its code and
/// `details.reason`, M4-D62's "fenced with that repair"), with `details.journal_pending`
/// the waiting journal, `details.tx` the repair and `details.resource` what is held back.
pub(crate) fn behind_error(
    tx: TxId,
    target: Option<TargetKey>,
    owner: TxId,
    stuck: &Stuck,
) -> Error {
    let message = behind_text(
        &folder_named(tx, target),
        &format!("repair {owner}"),
        stuck,
        "it is left as it is",
    );
    let mut details = serde_json::json!({
        "journal_pending": tx,
        "resource": format!("journal:{tx}"),
        "target": target,
        "tx": owner,
    });
    if let Some(reason) = stuck.stop.reason {
        details["reason"] = serde_json::json!(reason);
    }
    Error::new(stuck.stop.code, message, stuck.stop.fix.clone()).details(details)
}

/// The stop of a folder journal recovery left unresolved: its `code` and `reason`, for a
/// repair that waits behind it.
pub(crate) fn folder_stuck(
    tx: TxId,
    target: Option<TargetKey>,
    code: ErrorCode,
    reason: Option<StopReason>,
) -> Stuck {
    let why = reason.map_or("io", StopReason::as_str);
    Stuck {
        stop: FenceStop {
            tx: Some(tx),
            target,
            code,
            reason: reason.map(StopReason::as_str),
            summary: format!(
                "{} did not finish, and recovery could not settle it ({why})",
                folder_named(tx, target)
            ),
            fix: explain_fix(reason),
        },
        closed: false,
    }
}

/// A mutating command's write-scope table: [`ScopeTable::new`] with the fences of
/// [`load`]. Recovery's own tables, a read command's derived data and the doctor's own
/// run records are built unfenced (`every_mutating_scope_table_carries_fences` lists them).
pub fn table(
    paths: &Paths,
    client_dirs: &[PathBuf],
    skill_dir: Option<&Path>,
) -> Result<ScopeTable> {
    Ok(ScopeTable::new(paths, client_dirs, skill_dir)?.with_fences(load(paths, client_dirs)))
}

/// The commit's gate, under the global lock after its recovery and `refuse_newer_state`,
/// before its first write. In order:
///
/// 1. a repair that became stuck after `scopes` was built (a fencing journal the table
///    does not hold, the lowest first), or a set that closed since, refuses with its stop;
/// 2. `target`'s record or files index under one of the table's fences refuses;
/// 3. a path of `writes` (the commit's first-hand writes) under one refuses;
/// 4. a closed table refuses.
///
/// A fence the table holds that is gone now (a repair this command's own recovery just
/// settled) only over-protects this one command.
pub fn under_lock(
    scopes: &ScopeTable,
    paths: &Paths,
    target: Option<&Target>,
    writes: &[PathBuf],
) -> Result<()> {
    let held = scopes.fences();
    let now = load(paths, scopes.client_dirs());
    let had = held.txs();
    let closing = now.closed().map(|(_, stop)| stop);
    let newly = now
        .iter()
        .map(|fence| &fence.stop)
        .chain(closing)
        .filter(|stop| stop.tx.is_some_and(|tx| !had.contains(&tx)))
        .min_by_key(|stop| stop.tx);
    // A set closed since by a journal directory that cannot be listed has no journal.
    let late = newly.or_else(|| closing.filter(|_| !held.is_closed()));
    if let Some(stop) = late {
        let resource = stop.tx.map_or_else(
            || paths.journal_dir().display().to_string(),
            |tx| format!("journal:{tx}"),
        );
        let closed = closing.is_some_and(|closer| closer.tx == stop.tx);
        let mut error = refusal(resource, stop.clone(), closed);
        error.message = format!(
            "{}; it could not be settled after this command started, so the command changes nothing",
            stop.summary
        );
        return Err(error);
    }
    let records = target
        .and_then(|target| target.key)
        .map(|key| vec![paths.target_record(&key), paths.files_index(&key)])
        .unwrap_or_default();
    for path in records.iter().chain(writes) {
        if let Some(fence) = held.covering(&canonical_file(path)) {
            return Err(refusal(fence.resource.clone(), fence.stop.clone(), false));
        }
    }
    if let Some((resource, stop)) = held.closed() {
        return Err(refusal(resource.to_owned(), stop.clone(), true));
    }
    Ok(())
}

/// Refuses, as the one writer would, when `fences` is closed or one of them overlaps a
/// path of `paths` (each canonicalized through its parent, a deletion's whole tree
/// included): the first such fence's stop. For a command that deletes trees `prune()`
/// would otherwise skip one by one (`targets rm --purge`).
pub fn refuse_covered(fences: &FenceSet, paths: &[PathBuf]) -> Result<()> {
    if let Some((resource, stop)) = fences.closed() {
        return Err(refusal(resource.to_owned(), stop.clone(), true));
    }
    for path in paths {
        if let Some(fence) = fences.covering(&canonical_file(path)) {
            return Err(refusal(fence.resource.clone(), fence.stop.clone(), false));
        }
    }
    Ok(())
}

/// The error a fence refuses with, as the one writer builds it.
fn refusal(resource: String, stop: FenceStop, closed: bool) -> Error {
    FsxError::Fenced(Box::new(Fenced {
        resource,
        stop,
        closed,
    }))
    .into()
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::BTreeSet;
    use std::os::unix::fs::PermissionsExt;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use clap::Parser;

    use super::*;
    use crate::engine::journal::{
        REPAIR_DIAGNOSES, RecoveryEnv, pending_warnings, recover_all, recover_under,
    };
    use crate::engine::repair::{
        CommitCtx, LowerCtx, PlannedFix, RepairMeta, RepairTx, commit, lower,
    };
    use crate::engine::repair_action::{
        Attribution, Fix, JournalSet, Mode, QuarantineRoot, Sha256, Stamp, SuspendedArtifact,
    };
    use crate::engine::testing::{RepairHome, repair_home, repair_prepared};
    #[cfg(debug_assertions)]
    use crate::fsx::with_spec;
    use crate::fsx::{self, GlobalLock};
    use crate::model::Actor;

    const FILE_MODE: &str = "fm-secrets-credential-file-mode";
    const SKILL: &str = "fm-plugins-skill-version-drift";
    const NESTED: &str = "fm-external_artifacts-nested-addon-folder";
    const CORRUPT: &str = "fm-schemas-state-corrupt";
    const SET_ASIDE: &str = "fm-concurrency_primitives-journal-step-unrecognized";

    fn planned(fm: &'static str, fix: Fix) -> PlannedFix {
        PlannedFix {
            fm: Cow::Borrowed(fm),
            fix,
        }
    }

    fn mode(bits: u32) -> Mode {
        Mode::new(bits).unwrap()
    }

    fn chmod(home: &RepairHome) -> PlannedFix {
        planned(
            FILE_MODE,
            Fix::Chmod {
                path: home.credential(),
                mode: mode(0o600),
            },
        )
    }

    fn write(path: PathBuf) -> PlannedFix {
        planned(
            SKILL,
            Fix::Write {
                path,
                content: b"-- fixed\n".to_vec(),
                mode: mode(0o644),
            },
        )
    }

    fn quarantine(path: PathBuf, root: QuarantineRoot) -> PlannedFix {
        planned(CORRUPT, Fix::Quarantine { path, root })
    }

    fn client_root(home: &RepairHome) -> QuarantineRoot {
        QuarantineRoot::Client {
            client_dir: home.client.client_dir().to_path_buf(),
        }
    }

    fn flatten(home: &RepairHome, inner: &str, outer: &str) -> PlannedFix {
        planned(
            NESTED,
            Fix::Flatten {
                inner: home.addons(inner),
                outer: home.addons(outer),
            },
        )
    }

    /// The set-aside of the stuck folder journal `repair_home` leaves: its journal file
    /// and its client stage, with `guards`.
    fn suspension(home: &RepairHome, guards: Vec<Guard>) -> PlannedFix {
        let stuck = home.stuck();
        planned(
            SET_ASIDE,
            Fix::Suspend {
                journal: JournalSet {
                    tx: stuck,
                    family: JournalFamily::Folder,
                    target: Some(home.key),
                    artifacts: vec![
                        SuspendedArtifact {
                            path: home.paths.journal(&stuck),
                            root: QuarantineRoot::State,
                        },
                        SuspendedArtifact {
                            path: home.client.stage(&stuck),
                            root: client_root(home),
                        },
                    ],
                    guards,
                    attribution: Attribution::Complete,
                    unattributed: Vec::new(),
                },
            },
        )
    }

    fn path(p: PathBuf, kind: PathKind) -> Resource {
        Resource::from_canonical(&p, kind).unwrap()
    }

    fn texts(set: &BTreeSet<Resource>) -> Vec<String> {
        set.iter().map(|r| r.text().unwrap()).collect()
    }

    // -----------------------------------------------------------------------------------
    // What a stuck repair protects.

    #[test]
    fn protected_covers_each_fix_kind() {
        type Extras = fn(&RepairHome, &RepairPrepared) -> Vec<Resource>;
        type Case = (&'static str, fn(&RepairHome) -> PlannedFix, Extras);
        fn none(_: &RepairHome, _: &RepairPrepared) -> Vec<Resource> {
            Vec::new()
        }
        /// A barrier owner's extra: the record of every target it names.
        fn records(_: &RepairHome, p: &RepairPrepared) -> Vec<Resource> {
            p.targets.iter().map(|k| Resource::Record(*k)).collect()
        }
        let cases: Vec<Case> = vec![
            ("chmod_token", chmod, none),
            (
                "write_over_file_with_backup_home_side",
                |home| write(home.skill_file()),
                |home, p| {
                    vec![
                        path(home.run.dir.join("backups/1"), PathKind::File),
                        Resource::backup(&p.doctor_run_id, 1).unwrap(),
                    ]
                },
            ),
            (
                "write_over_file_with_backup_client_side",
                |home| write(home.addons("Stray/stray.lua")),
                |home, p| {
                    vec![
                        path(
                            home.client.snapshots_repair(&p.tx).join("files/1"),
                            PathKind::File,
                        ),
                        Resource::backup(&p.doctor_run_id, 1).unwrap(),
                    ]
                },
            ),
            (
                "write_creating_file",
                |home| write(home.paths.skill_dir().join("NOTES.md")),
                none,
            ),
            (
                "quarantine_state_file",
                |home| quarantine(home.record(), QuarantineRoot::State),
                none,
            ),
            (
                "quarantine_client_dir",
                |home| quarantine(home.addons("Stray"), client_root(home)),
                none,
            ),
            (
                "flatten_two_renames",
                |home| flatten(home, "Wrap/Inner", "Wrap"),
                none,
            ),
            (
                "flatten_three_renames",
                |home| flatten(home, "Same/Same", "Same"),
                none,
            ),
            (
                "suspension_with_generation_marker_epoch_guards",
                |home| {
                    suspension(
                        home,
                        vec![
                            Guard::Generation {
                                target: home.key,
                                generation: 7,
                            },
                            Guard::Marker {
                                target: home.key,
                                sha256: Some(Sha256([2; 32])),
                            },
                            Guard::Epoch {
                                path: home.client.addons(),
                                stamp: Some(Stamp([3; 32])),
                            },
                        ],
                    )
                },
                |home, p| {
                    let mut extras = records(home, p);
                    extras.push(Resource::Record(home.key));
                    extras.push(path(home.client.addons(), PathKind::Subtree));
                    extras
                },
            ),
            (
                "suspension_with_wtf_epoch",
                |home| {
                    suspension(
                        home,
                        vec![Guard::WtfEpoch {
                            target: home.key,
                            stamp: Some(Stamp([4; 32])),
                        }],
                    )
                },
                |home, p| {
                    let mut extras = records(home, p);
                    extras.push(path(home.client.wtf(), PathKind::Subtree));
                    extras
                },
            ),
            (
                "suspension_without_guards",
                |home| suspension(home, Vec::new()),
                records,
            ),
        ];
        for (case, fix, extras) in cases {
            let home = repair_home();
            let p = repair_prepared(&home, &[], &[fix(&home)]);
            let got = protected(&p, &home.paths).unwrap();
            let mut want: BTreeSet<Resource> = p
                .resources
                .writes
                .iter()
                .map(|text| Resource::parse(text).unwrap())
                .collect();
            want.extend(extras(&home, &p));
            assert_eq!(
                texts(got.writes()),
                texts(&want),
                "{case}: primitives {:#?}",
                p.primitives
            );
            assert!(got.reads().is_empty(), "{case}: writes only");
            if case.starts_with("suspension") {
                assert!(
                    p.targets.contains(&home.key),
                    "{case}: the suspended client's target is named: {:?}",
                    p.targets
                );
            }
        }
    }

    #[test]
    fn an_activation_protects_what_a_suspension_does() {
        let home = repair_home();
        let mut p = repair_prepared(&home, &[], &[suspension(&home, Vec::new())]);
        let suspended = protected(&p, &home.paths).unwrap();
        for step in &mut p.primitives {
            if let Primitive::RenameNoReplace { activation, .. } = &mut step.op {
                *activation = true;
            }
        }
        assert_eq!(
            barrier::of(&p, &home.paths).map(|b| b.kind),
            Some(barrier::BarrierKind::Activation)
        );
        let activated = protected(&p, &home.paths).unwrap();
        assert_eq!(texts(activated.writes()), texts(suspended.writes()));
        assert!(
            activated.writes().contains(&Resource::Record(home.key)),
            "{:?}",
            texts(activated.writes())
        );
    }

    #[test]
    fn paths_of_translates_each_resource() {
        let home = repair_home();
        let tx = TxId::new();
        let key = home.key;
        let display = |p: &Path| p.display().to_string();
        let stray = home.addons("Stray");
        let deep = home
            .client
            .wtf()
            .join("Account/FAKEACCT/SavedVariables/Stray.lua");
        type Case = (&'static str, Resource, Vec<(PathBuf, String)>);
        let cases: Vec<Case> = vec![
            (
                "path_file",
                path(home.credential(), PathKind::File),
                vec![(home.credential(), display(&home.credential()))],
            ),
            (
                "path_subtree",
                path(stray.clone(), PathKind::Subtree),
                vec![(stray.clone(), format!("{}/", display(&stray)))],
            ),
            (
                "record",
                Resource::Record(key),
                vec![
                    (
                        home.paths.files_index(&key),
                        display(&home.paths.files_index(&key)),
                    ),
                    (
                        home.paths.target_record(&key),
                        display(&home.paths.target_record(&key)),
                    ),
                ],
            ),
            ("journal", Resource::Journal(tx), {
                let mut all: Vec<(PathBuf, String)> = journal_paths(&home.paths, &tx)
                    .into_iter()
                    .map(|p| {
                        let p = canonical_file(&p);
                        let text = display(&p);
                        (p, text)
                    })
                    .collect();
                all.sort();
                all
            }),
            ("backup", Resource::backup("run", 1).unwrap(), Vec::new()),
            (
                "history_is_never_a_fence",
                path(home.paths.history_file(), PathKind::File),
                Vec::new(),
            ),
            (
                "net_log_is_never_a_fence",
                path(home.paths.net_log(), PathKind::File),
                Vec::new(),
            ),
            (
                "wtf_path_is_cut",
                path(deep.clone(), PathKind::File),
                vec![(
                    home.client.wtf(),
                    format!("{}/", display(&home.client.wtf())),
                )],
            ),
        ];
        for (case, resource, want) in cases {
            let mut set = ResourceSet::new();
            set.write(resource.clone());
            let got = paths_of(&set, &home.paths);
            assert_eq!(got, want, "{case}: {resource:?}");
            let shown = format!("{got:?}");
            assert!(!shown.contains("FAKEACCT"), "{case}: below WTF/: {shown}");
            assert_eq!(
                got.len(),
                journal_count(case),
                "{case}: the journal's five paths, a record's two"
            );
        }
        fn journal_count(case: &str) -> usize {
            match case {
                "journal" => 5,
                "record" => 2,
                "backup" | "history_is_never_a_fence" | "net_log_is_never_a_fence" => 0,
                _ => 1,
            }
        }
    }

    #[test]
    fn directory_and_child_fixes_conflict() {
        // A quarantine of a directory and a write into its child, each lowered by E5's
        // `lower` (lowered together, the write would find its parent already moved).
        let home = repair_home();
        let resources = |fix: PlannedFix| {
            repair_prepared(&home, &[], &[fix])
                .resources
                .to_set()
                .unwrap()
        };
        let dir = resources(quarantine(home.addons("Stray"), client_root(&home)));
        let child = resources(write(home.addons("Stray/stray.lua")));
        let conflicts = dir.conflicts(&child);
        let stray = path(home.addons("Stray"), PathKind::File);
        let lua = path(home.addons("Stray/stray.lua"), PathKind::File);
        assert!(
            conflicts
                .iter()
                .any(|c| c.ours.overlaps(&stray) && c.theirs.overlaps(&lua)),
            "both named: {conflicts:#?}"
        );
    }

    // -----------------------------------------------------------------------------------
    // Loading the fences.

    fn meta() -> RepairMeta {
        RepairMeta {
            run: "01JB8Q2VA00000000000000004".parse().unwrap(),
            actor: Actor::Human,
            reason: Some("a test repair".to_owned()),
            argv: ["omaddons", "doctor", "fix"].map(str::to_owned).to_vec(),
            command: "doctor fix",
            undo: None,
        }
    }

    /// `fixes` committed on `home` with `spec` armed: a `panic` stands in for a crash,
    /// leaving the journal as it was and its writer gone. Debug builds: it arms a
    /// failpoint, as every test that calls it does through it.
    #[cfg(debug_assertions)]
    fn crashed(home: &RepairHome, fixes: &[PlannedFix], spec: &str) -> TxId {
        let rtx = RepairTx::begin(&home.paths).unwrap();
        let tx = rtx.tx();
        let lowering = LowerCtx {
            paths: &home.paths,
            tx,
            run: &home.run,
            clients: &home.planned,
            skill_dirs: &home.skill_dirs,
            scopes: &home.scopes,
            mount_id: &fsx::mount_id,
        };
        let lowered = lower(&lowering, fixes).unwrap();
        let lock = GlobalLock::acquire(&home.paths, "test").unwrap();
        let ctx = CommitCtx {
            lock: &lock,
            scopes: &home.scopes,
            run: &home.run,
            mount_id: &fsx::mount_id,
        };
        let _ = catch_unwind(AssertUnwindSafe(|| {
            with_spec(spec, || commit(&ctx, rtx, &lowered, &meta()))
        }));
        tx
    }

    /// A chmod of the credential that ran, then the file changed by hand: its step is in
    /// neither state, and recovery stops at it (`step_unrecognized`).
    #[cfg(debug_assertions)]
    fn stuck(home: &RepairHome) -> TxId {
        let tx = crashed(home, &[chmod(home)], "fsx.mutate.executed=panic@1");
        set_mode(&home.credential(), 0o640);
        tx
    }

    fn set_mode(path: &Path, bits: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
    }

    fn edit_journal(home: &RepairHome, tx: &TxId, edit: impl FnOnce(String) -> String) {
        let journal = home.paths.journal_repair(tx);
        let text = std::fs::read_to_string(&journal).unwrap();
        std::fs::write(&journal, edit(text)).unwrap();
    }

    /// Removes the unreadable folder journal `repair_home` leaves, so recovery runs.
    fn no_folder_journal(home: &RepairHome) {
        let stuck = home.stuck();
        std::fs::remove_file(home.paths.journal(&stuck)).unwrap();
        std::fs::remove_file(home.paths.inflight_lock(&stuck)).unwrap();
    }

    fn clients(home: &RepairHome) -> Vec<PathBuf> {
        vec![home.client.client_dir().to_path_buf()]
    }

    /// What a test reads of a fence set: its fences' paths and texts, and what closed it.
    #[derive(Debug, PartialEq, Eq)]
    struct Shape {
        fences: Vec<(PathBuf, String, Option<TxId>, &'static str)>,
        closed: Option<(String, Option<TxId>, ErrorCode, Option<&'static str>)>,
    }

    fn shape(set: &FenceSet) -> Shape {
        Shape {
            fences: set
                .iter()
                .map(|f| {
                    (
                        f.path.clone(),
                        f.resource.clone(),
                        f.stop.tx,
                        f.stop.code.wire(),
                    )
                })
                .collect(),
            closed: set
                .closed()
                .map(|(resource, stop)| (resource.to_owned(), stop.tx, stop.code, stop.reason)),
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn load_maps_each_verdict() {
        type Plant = fn(&RepairHome) -> (Option<TxId>, Option<std::fs::File>);
        type Want = fn(&RepairHome, Option<TxId>) -> Shape;
        /// One fence, on the credential the chmod changed, with the stop's code.
        fn credential_fence(home: &RepairHome, tx: Option<TxId>, code: &'static str) -> Shape {
            Shape {
                fences: vec![(
                    home.credential(),
                    home.credential().display().to_string(),
                    tx,
                    code,
                )],
                closed: None,
            }
        }
        let empty: Want = |_, _| Shape {
            fences: Vec::new(),
            closed: None,
        };
        /// An unknowable repair puts every other journal behind it (E14): the stuck folder
        /// journal `repair_home` leaves is fenced (its artifacts, as it is untrusted) with
        /// that repair's stop, beside the close.
        fn behind_unknowable(
            home: &RepairHome,
            tx: TxId,
            code: &'static str,
        ) -> Vec<(PathBuf, String, Option<TxId>, &'static str)> {
            let stuck = home.stuck();
            let stage = home.client.stage(&stuck);
            let file = |path: PathBuf| {
                let text = path.display().to_string();
                (path, text, Some(tx), code)
            };
            vec![
                (
                    stage.clone(),
                    format!("{}/", stage.display()),
                    Some(tx),
                    code,
                ),
                file(home.paths.inflight_lock(&stuck)),
                file(home.paths.journal(&stuck)),
            ]
        }
        let cases: Vec<(&str, Plant, Want)> = vec![
            (
                "recoverable_repair",
                |home| {
                    let tx = crashed(home, &[chmod(home)], "doctor.repair.step=panic@1");
                    (Some(tx), None)
                },
                empty,
            ),
            (
                "stuck_readable_repair",
                |home| (Some(stuck(home)), None),
                |home, tx| credential_fence(home, tx, "internal"),
            ),
            (
                "undecodable_prepared",
                |home| {
                    let tx = crashed(home, &[chmod(home)], "doctor.repair.step=panic@1");
                    edit_journal(home, &tx, |text| {
                        let (_, rest) = text.split_once('\n').unwrap();
                        format!("not json\n{rest}")
                    });
                    (Some(tx), None)
                },
                |home, tx| {
                    let tx = tx.unwrap();
                    Shape {
                        fences: behind_unknowable(home, tx, "state_corrupt"),
                        closed: Some((
                            format!("journal:{tx}"),
                            Some(tx),
                            ErrorCode::StateCorrupt,
                            Some("journal_unreadable"),
                        )),
                    }
                },
            ),
            (
                "newer_major_prepared",
                |home| {
                    let tx = crashed(home, &[chmod(home)], "doctor.repair.step=panic@1");
                    edit_journal(home, &tx, |text| {
                        text.replacen("\"format\":\"1.0\"", "\"format\":\"9.0\"", 1)
                    });
                    (Some(tx), None)
                },
                |home, tx| {
                    let tx = tx.unwrap();
                    Shape {
                        fences: behind_unknowable(home, tx, "state_too_new"),
                        closed: Some((
                            format!("journal:{tx}"),
                            Some(tx),
                            ErrorCode::StateTooNew,
                            Some("journal_too_new"),
                        )),
                    }
                },
            ),
            (
                "decodable_prepared_bad_later_line",
                |home| {
                    let tx = crashed(home, &[chmod(home)], "doctor.repair.step=panic@1");
                    edit_journal(home, &tx, |text| {
                        let (first, rest) = text.split_once('\n').unwrap();
                        format!("{first}\nnot json\n{rest}")
                    });
                    (Some(tx), None)
                },
                |home, tx| credential_fence(home, tx, "state_corrupt"),
            ),
            (
                "no_complete_first_line",
                |home| {
                    let tx = crashed(home, &[chmod(home)], "doctor.repair.step=panic@1");
                    edit_journal(home, &tx, |text| {
                        let (first, _) = text.split_once('\n').unwrap();
                        first[..first.len() / 2].to_owned()
                    });
                    (Some(tx), None)
                },
                empty,
            ),
            (
                "live_repair",
                |home| {
                    let tx = stuck(home);
                    let holder = std::fs::File::open(home.paths.inflight_lock(&tx)).unwrap();
                    holder.lock().unwrap();
                    (Some(tx), Some(holder))
                },
                empty,
            ),
            (
                "unlisted_journal_dir",
                |home| {
                    stuck(home);
                    set_mode(&home.paths.journal_dir(), 0o000);
                    (None, None)
                },
                |home, _| Shape {
                    fences: Vec::new(),
                    closed: Some((
                        home.paths.journal_dir().display().to_string(),
                        None,
                        ErrorCode::Io,
                        Some("unattributed"),
                    )),
                },
            ),
            ("folder_journal_only", |_| (None, None), empty),
        ];
        for (case, plant, want) in cases {
            let home = repair_home();
            let (tx, _held) = plant(&home);
            let got = load(&home.paths, &clients(&home));
            set_mode(&home.paths.journal_dir(), 0o755);
            assert_eq!(shape(&got), want(&home, tx), "{case}");
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_fence_refusal_validates_against_its_codes_details() {
        // Every stop a fence can carry: a readable repair's (internal, step_unrecognized)
        // and a closed one's, with a journal and without.
        let home = repair_home();
        let tx = stuck(&home);
        let set = load(&home.paths, &clients(&home));
        let fence = set.covering(&home.credential()).unwrap();
        let mut refusals = vec![refusal(fence.resource.clone(), fence.stop.clone(), false)];
        let stopped = |code, reason: StopReason| FenceStop {
            tx: Some(tx),
            target: None,
            code,
            reason: Some(reason.as_str()),
            summary: summary(tx, Some(reason), true),
            fix: explain_fix(Some(reason)),
        };
        for (code, reason) in [
            (ErrorCode::StateCorrupt, StopReason::JournalUnreadable),
            (ErrorCode::StateTooNew, StopReason::JournalTooNew),
            (ErrorCode::ScopeDenied, StopReason::MarkerMismatch),
            (ErrorCode::ScopeDenied, StopReason::SkillDir),
            (ErrorCode::Io, StopReason::Unattributed),
            (ErrorCode::Internal, StopReason::StepUnrecognized),
        ] {
            refusals.push(refusal(
                format!("journal:{tx}"),
                stopped(code, reason),
                true,
            ));
        }
        let mut unlisted = FenceSet::new();
        close_unlisted(&mut unlisted, &home.paths);
        let (resource, stop) = unlisted.closed().unwrap();
        refusals.push(refusal(resource.to_owned(), stop.clone(), true));
        for error in refusals {
            assert_eq!(
                error.code.check_details(&error.details),
                Ok(()),
                "{}: {}",
                error.code.wire(),
                error.details
            );
            assert!(error.details["resource"].is_string(), "{}", error.details);
        }
    }

    // -----------------------------------------------------------------------------------
    // The gate under the lock.

    fn hand_stop(tx: TxId) -> FenceStop {
        FenceStop {
            tx: Some(tx),
            target: None,
            code: ErrorCode::Internal,
            reason: Some(StopReason::StepUnrecognized.as_str()),
            summary: summary(tx, Some(StopReason::StepUnrecognized), false),
            fix: explain_fix(Some(StopReason::StepUnrecognized)),
        }
    }

    fn fenced_table(home: &RepairHome, fenced: &[PathBuf]) -> ScopeTable {
        let mut set = FenceSet::new();
        let tx = TxId::new();
        for p in fenced {
            set.fence(p.clone(), p.display().to_string(), hand_stop(tx));
        }
        home.scopes.clone().with_fences(set)
    }

    fn target_of(home: &RepairHome) -> Target {
        let mut target = crate::engine::testing::target(home.client.client_dir());
        target.key = Some(home.key);
        target
    }

    #[cfg(debug_assertions)]
    #[test]
    fn under_lock_refuses_a_newly_stuck_repair() {
        let home = repair_home();
        let scopes = table(&home.paths, &clients(&home), None).unwrap();
        assert!(
            scopes.fences().is_empty(),
            "nothing stuck when it was built"
        );
        let tx = stuck(&home);
        let err = under_lock(&scopes, &home.paths, None, &[]).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal, "{err:?}");
        assert_eq!(err.details["resource"], format!("journal:{tx}"));
        assert_eq!(err.details["tx"], tx.to_string());
        assert_eq!(err.details["reason"], "step_unrecognized");
        assert!(
            err.message
                .contains("it could not be settled after this command started"),
            "{}",
            err.message
        );
        assert_eq!(err.code.check_details(&err.details), Ok(()));
    }

    #[test]
    fn under_lock_refuses_a_fenced_target() {
        let home = repair_home();
        let scopes = fenced_table(&home, &[home.paths.target_record(&home.key)]);
        let err = under_lock(&scopes, &home.paths, Some(&target_of(&home)), &[]).unwrap_err();
        assert_eq!(
            err.details["resource"],
            home.paths.target_record(&home.key).display().to_string()
        );
        assert!(
            under_lock(&scopes, &home.paths, None, &[]).is_ok(),
            "no target, no record"
        );
    }

    #[test]
    fn under_lock_refuses_a_fenced_first_hand_write() {
        let home = repair_home();
        let scopes = fenced_table(&home, &[home.addons("Stray")]);
        let err = under_lock(
            &scopes,
            &home.paths,
            None,
            &[home.addons("Stray/stray.lua")],
        )
        .unwrap_err();
        assert_eq!(
            err.details["resource"],
            home.addons("Stray").display().to_string()
        );
        assert!(
            under_lock(&scopes, &home.paths, None, &[home.addons("Wrap")]).is_ok(),
            "another folder"
        );
    }

    #[test]
    fn under_lock_refuses_a_closed_table() {
        let home = repair_home();
        let tx = TxId::new();
        let mut set = FenceSet::new();
        set.close(format!("journal:{tx}"), hand_stop(tx));
        let scopes = home.scopes.clone().with_fences(set);
        let err = under_lock(&scopes, &home.paths, None, &[]).unwrap_err();
        assert_eq!(err.details["resource"], format!("journal:{tx}"));
        assert!(
            err.message
                .ends_with("every change is refused until it is set aside")
        );
    }

    #[test]
    fn under_lock_passes_with_fewer_fences_now() {
        let home = repair_home();
        // The table holds a fence of a repair that is gone now (settled since).
        let scopes = fenced_table(&home, &[home.credential()]);
        assert_eq!(
            under_lock(
                &scopes,
                &home.paths,
                Some(&target_of(&home)),
                &[home.addons("Foo"), home.client.wtf()]
            )
            .map_err(|e| e.message.clone()),
            Ok(())
        );
    }

    // -----------------------------------------------------------------------------------
    // One warning per stuck repair.

    fn the_warning(warnings: &[Warning], tx: &TxId) -> Warning {
        let named: Vec<&Warning> = warnings
            .iter()
            .filter(|w| w.message.contains(&tx.to_string()))
            .collect();
        assert_eq!(named.len(), 1, "{warnings:#?}");
        named[0].clone()
    }

    #[cfg(debug_assertions)]
    #[test]
    fn the_reader_and_recovery_warnings_are_equal() {
        type Plant = fn(&RepairHome) -> TxId;
        let cases: Vec<(&str, Plant, bool)> = vec![
            ("readable", stuck, false),
            (
                "closed",
                |home| {
                    let tx = crashed(home, &[chmod(home)], "doctor.repair.step=panic@1");
                    edit_journal(home, &tx, |text| {
                        text.replacen("\"format\":\"1.0\"", "\"format\":\"9.0\"", 1)
                    });
                    tx
                },
                true,
            ),
        ];
        for (case, plant, closed) in cases {
            let home = repair_home();
            no_folder_journal(&home);
            let tx = plant(&home);
            let read = the_warning(&pending_warnings(&home.paths).unwrap(), &tx);
            let lock = GlobalLock::acquire(&home.paths, "test").unwrap();
            let env = RecoveryEnv {
                lock: &lock,
                paths: &home.paths,
                scopes: &home.scopes,
                own: &[],
                sv_writable: &journal::no_saved_variables,
            };
            let recovered = recover_all(&env).expect("a repair stop never fails the caller");
            let recovered = the_warning(&recovered, &tx);
            assert_eq!(read, recovered, "{case}");
            assert_eq!(read.code, WarningCode::JournalPending, "{case}");
            assert_eq!(
                read.message.contains("every change is refused"),
                closed,
                "{case}: {}",
                read.message
            );
            let under = recover_under(
                &lock,
                &home.paths,
                &journal::no_saved_variables,
                "test",
                Actor::Human,
            );
            assert_eq!(
                the_warning(&under, &tx),
                read,
                "{case}: under the lock alone"
            );
            assert!(home.paths.journal_repair(&tx).exists(), "{case}: kept");
        }
    }

    /// A set-aside of the stuck install `repair_home` leaves (its journal alone) that
    /// recorded its one step and crashed, recovery unable to settle it: the journal it
    /// would move was replaced since (another inode), so the step is in neither state. The
    /// install's journal, readable, waits behind it. Returns the set-aside's transaction.
    fn stuck_set_aside(home: &RepairHome) -> TxId {
        use crate::engine::repair::RepairLine;
        home.readable_stuck_journal();
        let stuck = home.stuck();
        let journal = home.paths.journal(&stuck);
        let set_aside = planned(
            SET_ASIDE,
            Fix::Suspend {
                journal: JournalSet {
                    tx: stuck,
                    family: JournalFamily::Folder,
                    target: Some(home.key),
                    artifacts: vec![SuspendedArtifact {
                        path: journal.clone(),
                        root: QuarantineRoot::State,
                    }],
                    guards: Vec::new(),
                    attribution: Attribution::Complete,
                    unattributed: Vec::new(),
                },
            },
        );
        let p = repair_prepared(home, &[], &[set_aside]);
        let copy = journal.with_extension("copy");
        std::fs::copy(&journal, &copy).unwrap();
        std::fs::rename(&copy, &journal).unwrap();
        let prepared = serde_json::to_string(&RepairLine::Prepared(Box::new(p.clone()))).unwrap();
        std::fs::write(
            home.paths.journal_repair(&p.tx),
            format!("{prepared}\n{{\"type\":\"step\",\"seq\":1}}\n"),
        )
        .unwrap();
        p.tx
    }

    #[test]
    fn load_fences_a_behind_journal_with_its_owner() {
        let home = repair_home();
        let owner = stuck_set_aside(&home);
        let stuck = home.stuck();
        let set = load(&home.paths, &clients(&home));
        let stage = home.client.stage(&stuck);
        for path in [stage.join("Foo"), home.paths.journal(&stuck)] {
            let fence = set.covering(&path);
            assert!(
                fence.is_some(),
                "{} is fenced: {:#?}",
                path.display(),
                shape(&set)
            );
            let fence = fence.unwrap();
            assert_eq!(
                (fence.stop.tx, fence.stop.code, fence.stop.reason),
                (Some(owner), ErrorCode::Internal, Some("step_unrecognized")),
                "fenced with that repair: {}",
                path.display()
            );
            assert!(
                fence.stop.summary.starts_with(&format!(
                    "repair transaction {owner} did not finish, and recovery could not settle it"
                )),
                "{}",
                fence.stop.summary
            );
        }
        // Without the owner nothing holds it back, and a folder journal fences nothing.
        std::fs::remove_file(home.paths.journal_repair(&owner)).unwrap();
        let set = load(&home.paths, &clients(&home));
        assert!(set.is_empty(), "{:#?}", shape(&set));
    }

    #[test]
    fn the_reader_and_recovery_behind_warnings_are_equal() {
        let home = repair_home();
        let owner = stuck_set_aside(&home);
        let stuck = home.stuck();
        let read = pending_warnings(&home.paths).unwrap();
        let lock = GlobalLock::acquire(&home.paths, "test").unwrap();
        let env = RecoveryEnv {
            lock: &lock,
            paths: &home.paths,
            scopes: &home.scopes,
            own: &[],
            sv_writable: &journal::no_saved_variables,
        };
        let recovered = recover_all(&env).expect("another target's journal only warns");
        // The one warning whose subject is `lead` (the behind text names the owner too).
        let about = |warnings: &[Warning], lead: &str| {
            let found: Vec<&Warning> = warnings
                .iter()
                .filter(|w| w.message.starts_with(lead))
                .collect();
            assert_eq!(found.len(), 1, "{lead}: {warnings:#?}");
            found[0].clone()
        };
        for lead in [
            format!("transaction {stuck} "),
            format!("repair transaction {owner} "),
        ] {
            assert_eq!(
                about(&read, &lead),
                about(&recovered, &lead),
                "{lead}: one text, read or recovered"
            );
        }
        let behind = about(&read, &format!("transaction {stuck} "));
        assert_eq!(
            behind.message,
            format!(
                "transaction {stuck} on target {} waits behind repair {owner}, which recovery could not settle (step_unrecognized); it is left as it is",
                home.key
            )
        );
        assert_eq!(
            behind.fix,
            Some(explain_fix(Some(StopReason::StepUnrecognized)))
        );
        assert!(home.paths.journal(&stuck).exists(), "left as it is");
    }

    #[test]
    fn each_warning_has_one_reason() {
        let tx = TxId::new();
        let reasons = std::iter::once(None).chain(StopReason::ALL.into_iter().map(Some));
        for reason in reasons {
            for closed in [false, true] {
                let warning = stuck_warning(tx, reason, closed);
                assert_eq!(
                    (
                        warning.message.matches('(').count(),
                        warning.message.matches(')').count()
                    ),
                    (1, 1),
                    "{reason:?} closed={closed}: {}",
                    warning.message
                );
                let named = reason.map_or("io", StopReason::as_str);
                assert!(
                    warning.message.contains(&format!("({named})")),
                    "{}",
                    warning.message
                );
                // An I/O failure of recovery fences nothing: its warning promises no refusal.
                let io_only = reason.is_none() && !closed;
                assert_eq!(
                    warning.message.contains("refused"),
                    !io_only,
                    "{reason:?} closed={closed}: {}",
                    warning.message
                );
            }
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn pending_warnings_titles_stuck_and_closed_repairs() {
        let home = repair_home();
        no_folder_journal(&home);
        let tx = stuck(&home);
        let warning = the_warning(&pending_warnings(&home.paths).unwrap(), &tx);
        assert_eq!(
            warning.message,
            format!(
                "repair transaction {tx} did not finish, and recovery could not settle it (step_unrecognized); writes to what it protects are refused, and everything else proceeds"
            )
        );
        assert_eq!(
            warning.fix.as_ref().and_then(|f| f.argv()),
            Some(
                &[
                    "omaddons",
                    "doctor",
                    "explain",
                    "fm-concurrency_primitives-journal-step-unrecognized"
                ]
                .map(str::to_owned)[..]
            )
        );
        let home = repair_home();
        no_folder_journal(&home);
        let tx = crashed(&home, &[chmod(&home)], "doctor.repair.step=panic@1");
        edit_journal(&home, &tx, |text| {
            let (_, rest) = text.split_once('\n').unwrap();
            format!("not json\n{rest}")
        });
        let warning = the_warning(&pending_warnings(&home.paths).unwrap(), &tx);
        assert_eq!(
            warning.message,
            format!(
                "repair transaction {tx} did not finish, and this omaddons cannot read what it changed (journal_unreadable); every change is refused until it is set aside"
            )
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn pending_warnings_costs_one_listing_without_repairs() {
        let diagnosed = || REPAIR_DIAGNOSES.with(std::cell::Cell::get);
        let home = repair_home();
        let before = diagnosed();
        pending_warnings(&home.paths).unwrap();
        assert_eq!(
            diagnosed(),
            before,
            "a folder journal alone simulates nothing"
        );
        let tx = crashed(&home, &[chmod(&home)], "doctor.repair.step=panic@1");
        let warnings = pending_warnings(&home.paths).unwrap();
        assert_eq!(diagnosed(), before + 1, "one simulation for the repairs");
        assert!(
            the_warning(&warnings, &tx)
                .message
                .contains("the next omaddons command"),
            "a recoverable repair keeps its text"
        );
    }

    #[test]
    fn explain_fix_parses() {
        let reasons = std::iter::once(None).chain(StopReason::ALL.into_iter().map(Some));
        for reason in reasons {
            let fix = explain_fix(reason);
            let argv = fix.argv().unwrap();
            assert!(
                crate::cli::Cli::try_parse_from(argv).is_ok(),
                "{reason:?}: {argv:?}"
            );
            let fm = &argv[3];
            assert!(
                fm.starts_with("fm-")
                    && fm[3..].split_once('-').is_some_and(|(subsystem, rest)| {
                        !subsystem.is_empty()
                            && subsystem
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                            && !rest.is_empty()
                            && rest
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                    }),
                "{fm}"
            );
            assert!(
                crate::doctor::registry::FAILURE_MODES
                    .iter()
                    .any(|row| row.id == fm.as_str()),
                "{fm} is a registered failure mode"
            );
        }
    }

    // -----------------------------------------------------------------------------------
    // Every mutating table is fenced.

    /// The production files that build a write-scope table with `ScopeTable::new` and
    /// not through [`table`], how many, and why each is left unfenced.
    const UNFENCED: &[(&str, usize, &str)] = &[
        (
            "src/app/apply.rs",
            1,
            "the plan store's lease, which a stored plan's apply takes",
        ),
        (
            "src/app/doctor.rs",
            1,
            "the doctor runtime: its own run directory",
        ),
        (
            "src/backup/inventory.rs",
            1,
            "derived data: the backup inventory projection",
        ),
        (
            "src/discover/cache.rs",
            1,
            "derived data: the discovery cache",
        ),
        (
            "src/doctor/fix_runtime.rs",
            2,
            "the doctor runtime's run directory and doctor diff's planning roots; the repair commit's table is fenced under the lock",
        ),
        (
            "src/engine/journal.rs",
            1,
            "recovery's own table (recover_under)",
        ),
        (
            "src/engine/repair/recover.rs",
            1,
            "a repair recovery's own table",
        ),
        (
            "src/engine/repair/undo.rs",
            1,
            "the doctor runtime: an undo's own run directory; its commit's table is fenced under the lock",
        ),
        (
            "src/engine/restore.rs",
            1,
            "cleanup of the command's own abandoned transaction",
        ),
        (
            "src/engine/simulate.rs",
            1,
            "the recovery simulation, which writes nothing",
        ),
        (
            "src/engine/tx.rs",
            1,
            "check_releases: cache blobs for a read command",
        ),
        ("src/errors/memo.rs", 1, "derived data: the errors memo"),
        ("src/net/log.rs", 1, "the request log"),
        (
            "src/net/record.rs",
            1,
            "derived data: recorded HTTP fixtures (debug builds)",
        ),
        ("src/net/validators.rs", 1, "derived data: HTTP validators"),
        (
            "src/sources/git_hosts.rs",
            1,
            "derived data: git's empty home in the cache",
        ),
        (
            "src/sources/wowi/index.rs",
            1,
            "derived data: the WoWInterface index",
        ),
        ("src/state/about_memo.rs", 1, "derived data: the about memo"),
        ("src/state/doorbell.rs", 1, "derived data: the doorbell"),
        (
            "src/state/history_tail.rs",
            1,
            "derived data: the history projection",
        ),
        ("src/state/memo.rs", 1, "derived data: the release memo"),
        (
            "src/state/plans.rs",
            2,
            "the plan store, which dry runs write and are never refused",
        ),
    ];

    /// Holds the per-file counts of `ScopeTable::new(` in `sources` (path relative to the
    /// crate, production code) to [`UNFENCED`] and this file's own one: the problems, as
    /// messages.
    fn unlisted_tables(
        sources: &[(String, String)],
        unfenced: &[(&str, usize, &str)],
    ) -> Vec<String> {
        let mut problems = Vec::new();
        for (file, code) in sources {
            let n = code.matches("ScopeTable::new(").count();
            // ubs:ignore — comparing file names, not secrets
            let own = usize::from(file == "src/engine/fence.rs");
            let listed = unfenced
                .iter()
                // ubs:ignore — comparing file names, not secrets
                .find(|(listed, _, _)| *listed == file)
                .map_or(0, |(_, count, _)| *count);
            if n != own + listed {
                problems.push(format!(
                    "{file} builds {n} write-scope tables, {} listed: build a mutating command's table with engine::fence::table, or list it in UNFENCED with its reason (derived data, recovery, own-transaction cleanup, the doctor runtime)",
                    own + listed
                ));
            }
        }
        for (listed, _, _) in unfenced {
            // ubs:ignore — comparing file names, not secrets
            if !sources.iter().any(|(file, _)| file == listed) {
                problems.push(format!(
                    "{listed} is listed in UNFENCED but builds no table"
                ));
            }
        }
        problems
    }

    #[test]
    fn every_mutating_scope_table_carries_fences() {
        use crate::source_scan::{Literals, production_code, sources};

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let scanned: Vec<(String, String)> = sources(&root.join("src"))
            .into_iter()
            .filter_map(|(path, text)| {
                let code = production_code(&text, Literals::Blank)?;
                let file = path.strip_prefix(root).unwrap().display().to_string();
                Some((file, code))
            })
            .filter(|(file, code)| {
                // ubs:ignore — comparing file names, not secrets
                file == "src/engine/fence.rs" || code.contains("ScopeTable::new(")
            })
            .collect();
        let problems = unlisted_tables(&scanned, UNFENCED);
        assert!(problems.is_empty(), "{}", problems.join("\n"));
        for (file, _, why) in UNFENCED {
            assert!(!why.is_empty(), "{file}: a reason");
        }
    }

    #[test]
    fn the_scan_reports_an_unlisted_table() {
        let sources = vec![
            (
                "src/engine/fence.rs".to_owned(),
                "ScopeTable::new(a, b, c)".to_owned(),
            ),
            (
                "src/app/new_command.rs".to_owned(),
                "let s = ScopeTable::new(p, &[], None)?;".to_owned(),
            ),
            (
                "src/state/memo.rs".to_owned(),
                "let s = ScopeTable::new(p, &[], None)?;".to_owned(),
            ),
        ];
        let unfenced = [
            ("src/state/memo.rs", 2, "derived data"),
            ("src/gone.rs", 1, "derived data"),
        ];
        let problems = unlisted_tables(&sources, &unfenced);
        assert_eq!(
            problems,
            vec![
                "src/app/new_command.rs builds 1 write-scope tables, 0 listed: build a mutating command's table with engine::fence::table, or list it in UNFENCED with its reason (derived data, recovery, own-transaction cleanup, the doctor runtime)".to_owned(),
                "src/state/memo.rs builds 1 write-scope tables, 2 listed: build a mutating command's table with engine::fence::table, or list it in UNFENCED with its reason (derived data, recovery, own-transaction cleanup, the doctor runtime)".to_owned(),
                "src/gone.rs is listed in UNFENCED but builds no table".to_owned(),
            ]
        );
    }
}
