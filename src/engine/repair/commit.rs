//! The repair commit (E5, M4-D31): a lowered repair run under the caller's global lock,
//! each primitive written ahead in the repair journal, every step that ran inverted when
//! one fails, and the journal pruned once the lock is released.
//!
//! **Two sessions.** A tracked `Writer::Doctor` session runs the primitives and nothing
//! else, so `actions.jsonl` holds exactly what undo reverts: its `seq` is the primitive's,
//! each overwriting write's backup lands where lowering reserved it (checked), and
//! `fsx.mutate.*@k` addresses primitive `k`. An untracked one writes the roots, the
//! journal lines and the history line.
//!
//! **The order**, every line fsync'd before what it announces runs:
//!
//! 1. revalidate, writing nothing: the run directory, every client's marker, the mounts,
//!    the steps themselves;
//! 2. the roots (doctor-owned directories only `doctor gc` removes);
//! 3. `prepared`;
//! 4. per primitive, its `step` line, then the primitive with its predicates; a failure
//!    rolls back every step that ran ([`super::inverse::roll_back`]) and ends the journal
//!    `rolled_back`;
//! 5. `done`; then the history line and `committed`, a failure there rolled forward in
//!    process ([`super::inverse::roll_forward`]).
//!
//! The commit neither recovers nor refuses because of other journals: `doctor fix` runs
//! the recoveries it disclosed under the same lock first, and each primitive's predicates
//! are the last line of defense.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::inverse::{OnDisk, append, expect_of, roll_back, roll_forward};
use super::journal::{
    ClientDirEntry, FORMAT, RepairLine, RepairPrepared, ResourcesWire, RollbackTarget, history_line,
};
use super::lower::{Lowered, LoweredStep};
use super::{RepairRun, RepairTx};
use crate::engine::repair_action::{Endpoint, Endpoints, Primitive, validate_steps};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    self, ActionRecord, Content, FsxError, GlobalLockGuard, LockUse, MutateOpts, Op, PruneReason,
    RenameMode, ScopeTable, SnapshotRoots, StampDigest, WriteSession, Writer, ensure_dir_all,
    failpoint, mutate_with,
};
use crate::model::{Actor, RunId, TxId};
use crate::state::{doorbell, history, history_tail};

/// Who commits a repair, why, and the command line history records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairMeta {
    pub run: RunId,
    pub actor: Actor,
    pub reason: Option<String>,
    pub argv: Vec<String>,
    /// The doorbell's command (`doctor fix`).
    pub command: &'static str,
}

/// What a commit runs under: the global lock (its type proves it is held, and is the only
/// way repair primitives reach `mutate()`), the scope table lowering used, and the run.
pub struct CommitCtx<'a> {
    pub lock: &'a GlobalLockGuard,
    pub scopes: &'a ScopeTable,
    pub run: &'a RepairRun,
    /// `fsx::mount_id` in production; a seam in unit tests.
    pub mount_id: &'a dyn Fn(&Path) -> io::Result<u64>,
}

/// How a repair ended.
#[derive(Debug)]
pub enum RepairOutcome {
    Committed {
        tx: TxId,
        primitives: u32,
        /// The size of the files the writes replaced, which their backups keep.
        bytes_backed_up: u64,
        /// Each overwriting write's step and its backup.
        backups: Vec<(u32, PathBuf)>,
    },
    /// A step failed, and every step that ran was inverted.
    RolledBack {
        tx: TxId,
        /// The step that failed; `None` when the failure came before the first step or
        /// after the last (the `done` line).
        failed_seq: Option<u32>,
        cause: Error,
        /// The steps inverted.
        backups_restored: u32,
    },
}

/// What is left once the lock is released: the doorbell, the journal and the in-flight
/// lock, and the history projection.
#[derive(Debug)]
pub struct RepairCleanup {
    rtx: RepairTx,
    journal: PathBuf,
    scopes: ScopeTable,
    command: &'static str,
    actor: Actor,
}

impl RepairCleanup {
    /// Called after the lock is released (§6.14.2 step 3). A failure is a warning: the
    /// repair stands, and the next recovery prunes a finished journal.
    pub fn finish(self) -> Vec<Warning> {
        let paths = self.scopes.paths().clone();
        doorbell::ring(&paths, self.command, self.actor);
        let victims = [self.journal.clone(), paths.inflight_lock(&self.rtx.tx())];
        let mut warnings = Vec::new();
        if let Err(err) = fsx::prune(
            &self.scopes,
            PruneReason::PostCommitCleanup,
            &victims,
            LockUse::Own(self.rtx.inflight()),
        ) {
            warnings.push(
                Warning::new(
                    WarningCode::RetentionFailed,
                    format!(
                        "repair {}'s journal could not be removed ({}); the repair stands, and the next change removes it",
                        self.rtx.tx(),
                        err.message
                    ),
                )
                .with_fix(Suggestion::command(["omaddons", "status"])),
            );
        }
        history_tail::rebuild_if_stale(&paths, self.command, self.actor);
        warnings
    }
}

/// Runs `lowered` as one repair under `ctx.lock`. `Err` means nothing was written (a
/// revalidation failed), or a rollback or a roll forward could not finish: the journal
/// is then kept for the next lock-taking command, named in `details.journal_pending`.
pub fn commit(
    ctx: &CommitCtx<'_>,
    rtx: RepairTx,
    lowered: &Lowered,
    meta: &RepairMeta,
) -> Result<(RepairOutcome, RepairCleanup)> {
    let _held: &GlobalLockGuard = ctx.lock;
    let tx = rtx.tx();
    revalidate(ctx, lowered)?;
    let prepared = prepared_line(ctx.run, tx, lowered, meta)?;
    let journal = ctx.scopes.paths().journal_repair(&tx);
    let outcome = run(ctx, lowered, &prepared, &journal)?;
    let cleanup = RepairCleanup {
        rtx,
        journal,
        scopes: ctx.scopes.clone(),
        command: meta.command,
        actor: meta.actor,
    };
    Ok((outcome, cleanup))
}

/// The journal's first line, built from what lowering decided.
fn prepared_line(
    run: &RepairRun,
    tx: TxId,
    lowered: &Lowered,
    meta: &RepairMeta,
) -> Result<RepairPrepared> {
    let resources = ResourcesWire::of(&lowered.resources)
        .map_err(|err| Error::internal(format!("repair {tx}: {err}")))?;
    Ok(RepairPrepared {
        format: FORMAT,
        tx,
        run: meta.run,
        doctor_run_id: run.id.clone(),
        actor: meta.actor,
        reason: meta.reason.clone(),
        argv: meta.argv.clone(),
        at: crate::clock::now(),
        targets: lowered.targets.clone(),
        client_dirs: lowered
            .client_dirs
            .iter()
            .map(|(dir, target)| ClientDirEntry {
                dir: dir.clone(),
                target: *target,
            })
            .collect(),
        resources,
        fixes: lowered.fixes.clone(),
        primitives: lowered.steps.iter().map(|s| s.step.clone()).collect(),
        rollbacks: lowered
            .steps
            .iter()
            .filter_map(|s| {
                s.rollback_to.clone().map(|to| RollbackTarget {
                    seq: s.step.seq,
                    to,
                })
            })
            .collect(),
    })
}

/// Everything checked again under the lock before anything is written.
fn revalidate(ctx: &CommitCtx<'_>, lowered: &Lowered) -> Result<()> {
    let nothing = || Suggestion::command(["omaddons", "status"]);
    if !ctx.run.dir.is_dir() {
        return Err(Error::io(
            &ctx.run.dir,
            &io::Error::new(io::ErrorKind::NotFound, "the doctor run directory is gone"),
            nothing(),
        ));
    }
    for (dir, key) in &lowered.client_dirs {
        if crate::discover::registry::read_marker(dir) != Some(*key) {
            return Err(Error::new(
                ErrorCode::ScopeDenied,
                format!(
                    "{}'s marker is not target {key}'s any more; nothing was written",
                    dir.display()
                ),
                nothing(),
            ));
        }
    }
    let steps: Vec<_> = lowered.steps.iter().map(|s| s.step.clone()).collect();
    validate_steps(&lowered.fixes, &steps)
        .map_err(|err| Error::internal(format!("a lowered repair does not validate: {err}")))?;
    for lowered_step in &lowered.steps {
        let step = &lowered_step.step;
        let moves = match &step.op {
            Primitive::RenameNoReplace { from, to, .. } => Some((from, to, true)),
            Primitive::WriteFile { path, .. } => lowered_step
                .rollback_to
                .as_ref()
                .map(|to| (path, to, false)),
            Primitive::Chmod { .. } => None,
        };
        if let Some((from, to, moves_from)) = moves {
            same_mount(ctx, from, to, moves_from)?;
        }
    }
    Ok(())
}

/// Refused unless a rename from `from` to `to` stays on one mount and, when `from` is on
/// disk now (`moves_from`), moves no mount point.
fn same_mount(ctx: &CommitCtx<'_>, from: &Path, to: &Path, moves_from: bool) -> Result<()> {
    let refuse = |why: &str| {
        Error::new(
            ErrorCode::ScopeDenied,
            format!(
                "{} and {}: {why}, and a quarantine never crosses a mount; nothing was written",
                from.display(),
                to.display()
            ),
            Suggestion::command(["omaddons", "status"]),
        )
    };
    let mount = |path: &Path| -> Result<u64> {
        (ctx.mount_id)(path)
            .map_err(|err| refuse(&format!("{} has no mount id ({err})", path.display())))
    };
    let deepest = |dir: &Path| -> Result<u64> {
        let found = dir
            .ancestors()
            .find(|a| std::fs::symlink_metadata(a).is_ok());
        found.map_or_else(|| Err(refuse("no folder above them exists")), mount)
    };
    let parent = |path: &Path| path.parent().unwrap_or(path).to_path_buf();
    let source = deepest(&parent(from))?;
    if moves_from && std::fs::symlink_metadata(from).is_ok() && mount(from)? != source {
        return Err(refuse("the first is a mount point now"));
    }
    if deepest(&parent(to))? != source {
        return Err(refuse("they are on different mounts now"));
    }
    Ok(())
}

/// The commit itself, from the roots on.
fn run(
    ctx: &CommitCtx<'_>,
    lowered: &Lowered,
    prepared: &RepairPrepared,
    journal: &Path,
) -> Result<RepairOutcome> {
    let tx = prepared.tx;
    let paths = ctx.scopes.paths();
    let mut u = WriteSession::untracked(Writer::Doctor, ctx.scopes, "repair journal");
    for root in &lowered.roots {
        ensure_dir_all(&mut u, root).map_err(Error::from)?;
    }
    ensure_dir_all(&mut u, &paths.journal_dir()).map_err(Error::from)?;
    append(
        &mut u,
        journal,
        &RepairLine::Prepared(Box::new(prepared.clone())),
    )?;
    // G3: the barrier before the first step line goes here.
    if let Err(err) = failpoint::hit("doctor.repair.prepared") {
        return rolled_back(&mut u, prepared, journal, 0, None, io_cause(journal, &err));
    }
    let mut tracked = WriteSession::tx(
        tx,
        Writer::Doctor,
        ctx.scopes,
        SnapshotRoots::for_repair(ctx.run.dir.join("backups"), &tx),
        ctx.run.dir.join("actions.jsonl"),
    );
    let mut bytes_backed_up = 0;
    let mut backups = Vec::new();
    for lowered_step in &lowered.steps {
        let seq = lowered_step.step.seq;
        if let Err(cause) = append(&mut u, journal, &RepairLine::Step { seq }) {
            return rolled_back(&mut u, prepared, journal, seq - 1, Some(seq), cause);
        }
        // G3: the barrier after a step line that follows a rename goes here.
        let ran = failpoint::hit("doctor.repair.step")
            .map_err(|err| (io_cause(journal, &err), false))
            .and_then(|()| execute(&mut tracked, lowered_step));
        let record = match ran {
            Ok(record) => record,
            // A step fsx refused before acting on it never ran, whatever its paths say
            // now: the walk starts below it. One that may have acted is classified.
            Err((cause, may_have_run)) => {
                let recorded = if may_have_run { seq } else { seq - 1 };
                return rolled_back(&mut u, prepared, journal, recorded, Some(seq), cause);
            }
        };
        if let Primitive::WriteFile {
            backup: Some(reserved),
            ..
        } = &lowered_step.step.op
        {
            if record.backup.as_deref() != reserved.to_str() {
                let cause = Error::internal(format!(
                    "repair {tx}: step {seq}'s backup is at {}, not where lowering reserved it ({})",
                    record.backup.as_deref().unwrap_or("nowhere"),
                    reserved.display()
                ));
                return rolled_back(&mut u, prepared, journal, seq, Some(seq), cause);
            }
            bytes_backed_up += lowered_step.backup_bytes;
            backups.push((seq, reserved.clone()));
        }
    }
    // G3: `session.finish()` and the commit window's barrier go here, before `done`.
    let n = u32::try_from(lowered.steps.len()).unwrap_or(u32::MAX);
    if let Err(cause) = append(&mut u, journal, &RepairLine::Done) {
        return rolled_back(&mut u, prepared, journal, n, None, cause);
    }
    // From `done` on, the repair stands: a failure is rolled forward, never back.
    let finished = failpoint::hit("doctor.repair.done")
        .map_err(|err| io_cause(journal, &err))
        .and_then(|()| history::append(&mut u, paths, &history_line(prepared)))
        .and_then(|()| {
            failpoint::hit("doctor.repair.history").map_err(|err| io_cause(journal, &err))
        })
        .and_then(|()| append(&mut u, journal, &RepairLine::Committed));
    if let Err(cause) = finished {
        roll_forward(&mut u, paths, journal, prepared, false)
            .map_err(|err| pending(recovery_failed(cause, &err), tx))?;
    }
    // A crash point only: once `committed` is durable there is nothing left to undo, so
    // an injected error here changes nothing.
    let _ = failpoint::hit("doctor.repair.committed");
    Ok(RepairOutcome::Committed {
        tx,
        primitives: n,
        bytes_backed_up,
        backups,
    })
}

/// Runs one primitive through the tracked session, its `pre` checked immediately before.
/// An error says whether the primitive may have acted: only an I/O error may come from
/// its execution; a refusal (a predicate, the scope table, an unsupported rename) comes
/// before it.
fn execute(
    tracked: &mut WriteSession<'_>,
    lowered_step: &LoweredStep,
) -> std::result::Result<ActionRecord, (Error, bool)> {
    let step = &lowered_step.step;
    let unfit = |what: &str| (Error::internal(format!("step {}: {what}", step.seq)), false);
    let done = match (&step.op, &step.pre) {
        (Primitive::WriteFile { path, mode, .. }, Endpoints::One(pre)) => {
            let Some(content) = lowered_step.content.as_deref() else {
                return Err(unfit("a write lowered without its content"));
            };
            mutate_with(
                tracked,
                path,
                Op::WriteFile {
                    content: Content::Bytes(content),
                    mode: Some(mode.bits()),
                },
                MutateOpts {
                    expect: Some(expect_of(pre)),
                    ..MutateOpts::default()
                },
            )
        }
        (Primitive::Chmod { path, mode }, Endpoints::One(pre)) => mutate_with(
            tracked,
            path,
            Op::Chmod { mode: mode.bits() },
            MutateOpts {
                expect: Some(expect_of(pre)),
                ..MutateOpts::default()
            },
        ),
        (
            Primitive::RenameNoReplace { from, to, .. },
            Endpoints::Pair {
                path: source,
                to: destination,
            },
        ) => {
            let stamp_before = match source {
                Endpoint::Dir {
                    stamp: Some(stamp), ..
                } => Some(StampDigest::from_bytes(stamp.0)),
                _ => None,
            };
            mutate_with(
                tracked,
                from,
                Op::Rename {
                    to,
                    mode: RenameMode::NoReplace,
                },
                MutateOpts {
                    expect: Some(expect_of(source)),
                    expect_to: Some(expect_of(destination)),
                    stamp_before,
                    temp_symlink: matches!(source, Endpoint::Symlink { .. }),
                    ..MutateOpts::default()
                },
            )
        }
        _ => return Err(unfit("its endpoints do not fit its operation")),
    };
    done.map_err(|err| {
        let may_have_run = matches!(err, FsxError::Io { .. });
        (Error::from(err), may_have_run)
    })
}

/// Rolls back the steps `1..=recorded`, ends the journal `rolled_back`, and reports why.
/// A rollback that cannot finish returns its error with the journal kept.
fn rolled_back(
    u: &mut WriteSession<'_>,
    prepared: &RepairPrepared,
    journal: &Path,
    recorded: u32,
    failed_seq: Option<u32>,
    cause: Error,
) -> Result<RepairOutcome> {
    #[cfg(test)]
    tests::before_rollback();
    let tx = prepared.tx;
    let restored = roll_back(u, &mut OnDisk, prepared, recorded).map_err(|err| pending(err, tx))?;
    append(u, journal, &RepairLine::RolledBack).map_err(|err| pending(err, tx))?;
    Ok(RepairOutcome::RolledBack {
        tx,
        failed_seq,
        cause,
        backups_restored: restored,
    })
}

fn io_cause(journal: &Path, err: &io::Error) -> Error {
    Error::io(journal, err, Suggestion::command(["omaddons", "status"]))
}

/// `err` with `details.journal_pending` naming `tx`, whose journal is kept.
fn pending(mut err: Error, tx: TxId) -> Error {
    if !err.details.is_object() {
        err.details = json!({});
    }
    if let Some(details) = err.details.as_object_mut() {
        details.insert("journal_pending".into(), tx.to_string().into());
    }
    err
}

/// The failure that stopped a repair after `done`, when rolling it forward failed too.
fn recovery_failed(mut cause: Error, recovery: &Error) -> Error {
    cause.message = format!(
        "{}; finishing the repair failed too ({}), so its journal is kept and the next omaddons command that commits a change finishes it",
        cause.message, recovery.message
    );
    cause.retryable = false;
    if let Some(details) = cause.details.as_object_mut() {
        details.insert(
            "recovery_error".into(),
            json!({"code": recovery.code, "message": recovery.message}),
        );
    }
    cause
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::engine::repair::{LowerCtx, PlannedFix, lower};
    use crate::engine::repair_action::{Fix, Mode, QuarantineRoot};
    use crate::engine::testing::{RepairHome, repair_home, tree_hash, tree_hash_except};
    use crate::fsx::{GlobalLock, InflightLock, with_spec};
    use crate::state::history::HistoryFilter;

    thread_local! {
        /// Runs once, just before the next rollback walk on this thread.
        static BEFORE_ROLLBACK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    }

    pub(super) fn before_rollback() {
        if let Some(hook) = BEFORE_ROLLBACK.with(|hook| hook.borrow_mut().take()) {
            hook();
        }
    }

    const SKILL: &str = "fm-plugins-skill-version-drift";
    const FILE_MODE: &str = "fm-secrets-credential-file-mode";
    const NESTED: &str = "fm-external_artifacts-nested-addon-folder";
    const CORRUPT: &str = "fm-schemas-state-corrupt";
    const NEW_GUIDE: &[u8] = b"---\nname: omaddons\n---\nnew guide\n";

    fn planned(fm: &'static str, fix: Fix) -> PlannedFix {
        PlannedFix {
            fm: Cow::Borrowed(fm),
            fix,
        }
    }

    fn mode(bits: u32) -> Mode {
        Mode::new(bits).unwrap()
    }

    fn write_over(home: &RepairHome) -> PlannedFix {
        planned(
            SKILL,
            Fix::Write {
                path: home.skill_file(),
                content: NEW_GUIDE.to_vec(),
                mode: mode(0o644),
            },
        )
    }

    fn notes(home: &RepairHome) -> PathBuf {
        home.paths.skill_dir().join("NOTES.md")
    }

    fn write_create(home: &RepairHome) -> PlannedFix {
        planned(
            SKILL,
            Fix::Write {
                path: notes(home),
                content: b"notes\n".to_vec(),
                mode: mode(0o644),
            },
        )
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

    fn meta() -> RepairMeta {
        RepairMeta {
            run: "01JB8Q2VA00000000000000004".parse().unwrap(),
            actor: Actor::Human,
            reason: Some("a test repair".to_owned()),
            argv: ["omaddons", "doctor", "fix"].map(str::to_owned).to_vec(),
            command: "doctor fix",
        }
    }

    /// A repair lowered on `home`, with its transaction begun.
    fn lowered(home: &RepairHome, fixes: &[PlannedFix]) -> (RepairTx, Lowered) {
        let rtx = RepairTx::begin(&home.paths).unwrap();
        let ctx = LowerCtx {
            paths: &home.paths,
            tx: rtx.tx(),
            run: &home.run,
            clients: &home.planned,
            skill_dirs: &home.skill_dirs,
            scopes: &home.scopes,
            mount_id: &fsx::mount_id,
        };
        let lowered = lower(&ctx, fixes).unwrap();
        (rtx, lowered)
    }

    /// `lowered` committed under the global lock with `mount_id`, the lock released after.
    fn commit_with(
        home: &RepairHome,
        rtx: RepairTx,
        lowered: &Lowered,
        mount_id: &dyn Fn(&Path) -> io::Result<u64>,
    ) -> Result<(RepairOutcome, RepairCleanup)> {
        let lock = GlobalLock::acquire(&home.paths, "test").unwrap();
        let ctx = CommitCtx {
            lock: &lock,
            scopes: &home.scopes,
            run: &home.run,
            mount_id,
        };
        commit(&ctx, rtx, lowered, &meta())
    }

    fn commit_on(
        home: &RepairHome,
        fixes: &[PlannedFix],
    ) -> (TxId, Result<(RepairOutcome, RepairCleanup)>) {
        let (rtx, lowered) = lowered(home, fixes);
        let tx = rtx.tx();
        (tx, commit_with(home, rtx, &lowered, &fsx::mount_id))
    }

    /// The journal's lines, as `type` (with a step's `seq`).
    fn journal_lines(home: &RepairHome, tx: &TxId) -> Vec<String> {
        let text = std::fs::read_to_string(home.paths.journal_repair(tx)).unwrap_or_default();
        text.lines()
            .map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                match value["seq"].as_u64() {
                    Some(seq) => format!("step {seq}"),
                    None => value["type"].as_str().unwrap().to_owned(),
                }
            })
            .collect()
    }

    fn prepared_of(home: &RepairHome, tx: &TxId) -> RepairPrepared {
        let text = std::fs::read_to_string(home.paths.journal_repair(tx)).unwrap();
        super::super::journal::parse(&text).prepared.unwrap()
    }

    fn repair_lines(home: &RepairHome, tx: &TxId) -> Vec<crate::state::history::HistoryEntry> {
        history::read(&home.paths, &HistoryFilter::default())
            .unwrap()
            .entries
            .into_iter()
            .filter(|entry| entry.tx == *tx)
            .collect()
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    }

    fn expected(steps: usize, end: &[&str]) -> Vec<String> {
        let mut lines = vec!["prepared".to_owned()];
        lines.extend((1..=steps).map(|seq| format!("step {seq}")));
        lines.extend(end.iter().map(|end| (*end).to_owned()));
        lines
    }

    /// A client-side quarantine of transaction `tx`.
    fn parked(home: &RepairHome, tx: &TxId, rel: &str) -> PathBuf {
        home.client
            .snapshots_repair(tx)
            .join("quarantine/Interface/AddOns")
            .join(rel)
    }

    type Fixes = fn(&RepairHome) -> Vec<PlannedFix>;
    /// What must hold once a case committed, each named.
    type After = fn(&RepairHome, &TxId) -> Vec<(&'static str, bool)>;

    #[test]
    fn each_fix_kind_commits() {
        let cases: Vec<(&str, Fixes, After)> = vec![
            (
                "write_over",
                |h| vec![write_over(h)],
                |h, _| {
                    vec![
                        (
                            "new bytes",
                            std::fs::read(h.skill_file()).unwrap() == NEW_GUIDE,
                        ),
                        // ubs:ignore — comparing a file's mode bits, not a secret
                        ("mode 0644", mode_of(&h.skill_file()) == 0o644),
                    ]
                },
            ),
            (
                "write_create",
                |h| vec![write_create(h)],
                |h, _| vec![("created", std::fs::read(notes(h)).unwrap() == b"notes\n")],
            ),
            (
                "chmod",
                |h| vec![chmod(h)],
                |h, _| {
                    let mode = mode_of(&h.credential());
                    // ubs:ignore — comparing a file's mode bits, not a secret
                    vec![("mode 0600", mode == 0o600)]
                },
            ),
            (
                "quarantine_state_file",
                |h| vec![quarantine(h.record(), QuarantineRoot::State)],
                |h, _| {
                    let to = h
                        .run
                        .dir
                        .join(format!("quarantine/state/targets/{}.json", h.key));
                    vec![
                        ("source gone", !h.record().exists()),
                        ("quarantined", to.is_file()),
                    ]
                },
            ),
            (
                "quarantine_client_dir",
                |h| vec![quarantine(h.addons("Stray"), client_root(h))],
                |h, tx| {
                    vec![
                        ("source gone", !h.addons("Stray").exists()),
                        ("quarantined", parked(h, tx, "Stray/Stray.toc").is_file()),
                    ]
                },
            ),
            (
                "quarantine_temp_symlink",
                |h| vec![quarantine(h.temp_link(), QuarantineRoot::Cache)],
                |h, _| {
                    let to = h
                        .paths
                        .cache_quarantine(&h.run.id)
                        .join("tmp/.omaddons-link");
                    vec![
                        (
                            "source gone",
                            std::fs::symlink_metadata(h.temp_link()).is_err(),
                        ),
                        (
                            "the link moved, its target kept",
                            std::fs::read_link(&to).is_ok_and(|t| t == Path::new("../blobs/ab")),
                        ),
                    ]
                },
            ),
            (
                "flatten_two",
                |h| vec![flatten(h, "Wrap/Inner", "Wrap")],
                |h, tx| {
                    vec![
                        ("lifted", h.addons("Inner/Inner.toc").is_file()),
                        ("wrapper gone", !h.addons("Wrap").exists()),
                        ("wrapper parked", parked(h, tx, "Wrap/README.txt").is_file()),
                    ]
                },
            ),
            (
                "flatten_three",
                |h| vec![flatten(h, "Same/Same", "Same")],
                |h, tx| {
                    vec![
                        ("lifted", h.addons("Same/Same.toc").is_file()),
                        ("no provisional", !h.client.flatten_provisional(tx).exists()),
                        ("wrapper parked", parked(h, tx, "Same/notes.txt").is_file()),
                    ]
                },
            ),
        ];
        for (case, fixes, after) in cases {
            let home = repair_home();
            let (tx, result) = commit_on(&home, &fixes(&home));
            let (outcome, cleanup) = result.unwrap_or_else(|err| panic!("{case}: {err}")); // ubs:ignore — fails the test
            let RepairOutcome::Committed { primitives, .. } = outcome else {
                panic!("{case}: {outcome:?}"); // ubs:ignore — fails the test
            };
            for (what, holds) in after(&home, &tx) {
                assert!(holds, "{case}: {what}");
            }
            let lines = journal_lines(&home, &tx);
            assert_eq!(
                lines,
                expected(primitives as usize, &["done", "committed"]),
                "{case}"
            );
            let prepared = prepared_of(&home, &tx);
            let history = repair_lines(&home, &tx);
            assert_eq!(history.len(), 1, "{case}: one history line");
            assert!(history[0].well_formed(), "{case}");
            assert_eq!(
                serde_json::to_value(&history[0]).unwrap(),
                serde_json::to_value(history_line(&prepared)).unwrap(),
                "{case}: the line history_line builds"
            );
            assert!(
                cleanup.finish().is_empty(),
                "{case}: cleanup warns of nothing"
            );
            assert!(
                !home.paths.journal_repair(&tx).exists(),
                "{case}: journal pruned"
            );
            assert!(
                !home.paths.inflight_lock(&tx).exists(),
                "{case}: lock pruned"
            );
        }
    }

    fn actions(home: &RepairHome) -> Vec<ActionRecord> {
        std::fs::read_to_string(home.run.dir.join("actions.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn the_actions_log_holds_only_the_primitives() {
        let cases: [(&str, Fixes); 2] = [
            ("flatten_three", |h| vec![flatten(h, "Same/Same", "Same")]),
            ("write_over", |h| vec![write_over(h)]),
        ];
        for (case, fixes) in cases {
            let home = repair_home();
            let (tx, result) = commit_on(&home, &fixes(&home));
            result.unwrap();
            let prepared = prepared_of(&home, &tx);
            let records = actions(&home);
            assert_eq!(
                records.len(),
                prepared.primitives.len(),
                "{case}: {records:#?}"
            );
            for (record, step) in records.iter().zip(&prepared.primitives) {
                assert_eq!(record.seq, u64::from(step.seq), "{case}");
                assert!(record.ok, "{case}: {record:?}");
                assert_eq!(record.path, step.op.path().display().to_string(), "{case}");
                assert_eq!(record.tx, Some(tx), "{case}");
            }
        }
    }

    #[test]
    fn the_reserved_backup_holds_the_replaced_bytes() {
        let home = repair_home();
        let old = std::fs::read(home.skill_file()).unwrap();
        let (_, result) = commit_on(&home, &[write_over(&home)]);
        let (outcome, _) = result.unwrap();
        let backup = home.run.dir.join("backups/1");
        let RepairOutcome::Committed {
            bytes_backed_up,
            backups,
            ..
        } = outcome
        else {
            panic!("{outcome:?}"); // ubs:ignore — fails the test
        };
        assert_eq!(backups, vec![(1, backup.clone())]);
        assert_eq!(bytes_backed_up, old.len() as u64);
        assert_eq!(std::fs::read(&backup).unwrap(), old, "byte for byte");
    }

    #[test]
    fn a_chmod_over_a_same_mode_replacement_fails_its_identity_predicate() {
        let home = repair_home();
        let (rtx, lowered) = lowered(&home, &[chmod(&home)]);
        let tx = rtx.tx();
        // Another file, of the same mode, renamed over the credential after lowering.
        let credential = home.credential();
        let other = credential.with_file_name("github.new");
        std::fs::write(&other, "ghp_replacement\n").unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::rename(&other, &credential).unwrap();
        let (outcome, cleanup) = commit_with(&home, rtx, &lowered, &fsx::mount_id).unwrap();
        let RepairOutcome::RolledBack {
            failed_seq,
            backups_restored,
            ..
        } = outcome
        else {
            panic!("{outcome:?}"); // ubs:ignore — fails the test
        };
        assert_eq!((failed_seq, backups_restored), (Some(1), 0));
        assert_eq!(
            mode_of(&credential),
            0o644,
            "the replacement keeps its mode"
        );
        assert_eq!(std::fs::read(&credential).unwrap(), b"ghp_replacement\n");
        assert!(repair_lines(&home, &tx).is_empty(), "no history line");
        assert_eq!(journal_lines(&home, &tx), expected(1, &["rolled_back"]));
        assert!(cleanup.finish().is_empty());
        assert!(
            !home.paths.journal_repair(&tx).exists(),
            "a rolled-back journal is pruned"
        );
    }

    #[test]
    fn a_changed_path_rolls_back_every_step_that_ran() {
        let home = repair_home();
        let (rtx, lowered) = lowered(&home, &[flatten(&home, "Wrap/Inner", "Wrap")]);
        let tx = rtx.tx();
        std::fs::write(home.addons("Wrap/added.txt"), "after lowering\n").unwrap();
        let (outcome, _) = commit_with(&home, rtx, &lowered, &fsx::mount_id).unwrap();
        let RepairOutcome::RolledBack {
            failed_seq,
            backups_restored,
            ..
        } = outcome
        else {
            panic!("{outcome:?}"); // ubs:ignore — fails the test
        };
        assert_eq!((failed_seq, backups_restored), (Some(2), 1));
        assert!(
            home.addons("Wrap/Inner/Inner.toc").is_file(),
            "Inner is back in Wrap"
        );
        assert!(!home.addons("Inner").exists());
        assert_eq!(journal_lines(&home, &tx), expected(2, &["rolled_back"]));
    }

    #[test]
    fn a_failed_step_after_a_write_restores_the_backup_and_mode() {
        let home = repair_home();
        let skill = home.skill_file();
        std::fs::set_permissions(&skill, std::fs::Permissions::from_mode(0o640)).unwrap();
        let old = std::fs::read(&skill).unwrap();
        let fixes = [
            write_over(&home),
            quarantine(home.blob(), QuarantineRoot::Cache),
        ];
        let (rtx, lowered) = lowered(&home, &fixes);
        std::fs::remove_file(home.blob()).unwrap();
        let (outcome, _) = commit_with(&home, rtx, &lowered, &fsx::mount_id).unwrap();
        let RepairOutcome::RolledBack {
            failed_seq,
            backups_restored,
            ..
        } = outcome
        else {
            panic!("{outcome:?}"); // ubs:ignore — fails the test
        };
        assert_eq!((failed_seq, backups_restored), (Some(2), 1));
        assert_eq!(std::fs::read(&skill).unwrap(), old, "its old bytes");
        assert_eq!(mode_of(&skill), 0o640, "and its old mode");
    }

    #[test]
    fn a_created_file_is_rolled_back_into_quarantine_never_deleted() {
        let home = repair_home();
        let fixes = [
            write_create(&home),
            quarantine(home.blob(), QuarantineRoot::Cache),
        ];
        let (rtx, lowered) = lowered(&home, &fixes);
        let rollback = lowered.steps[0].rollback_to.clone().unwrap();
        std::fs::remove_file(home.blob()).unwrap();
        let (outcome, _) = commit_with(&home, rtx, &lowered, &fsx::mount_id).unwrap();
        assert!(
            matches!(
                outcome,
                RepairOutcome::RolledBack {
                    failed_seq: Some(2),
                    backups_restored: 1,
                    ..
                }
            ),
            "{outcome:?}"
        );
        assert!(
            !notes(&home).exists(),
            "the created file is gone from its place"
        );
        assert_eq!(
            std::fs::read(&rollback).unwrap(),
            b"notes\n",
            "and kept in quarantine"
        );
        assert!(rollback.starts_with(home.run.dir.join("quarantine/skill/rolled_back")));
    }

    #[test]
    fn an_error_after_execute_is_rolled_back_by_its_predicates() {
        let home = repair_home();
        let (rtx, lowered) = lowered(&home, &[chmod(&home)]);
        let (outcome, _) = with_spec("fsx.mutate.executed=error@1", || {
            commit_with(&home, rtx, &lowered, &fsx::mount_id)
        })
        .unwrap();
        assert!(
            matches!(
                outcome,
                RepairOutcome::RolledBack {
                    failed_seq: Some(1),
                    backups_restored: 1,
                    ..
                }
            ),
            "the chmod ran, and was inverted: {outcome:?}"
        );
        assert_eq!(mode_of(&home.credential()), 0o644, "its old mode");
    }

    #[test]
    fn a_rollback_that_meets_an_unrecognized_step_stops_and_keeps_the_journal() {
        let home = repair_home();
        let (rtx, lowered) = lowered(&home, &[flatten(&home, "Wrap/Inner", "Wrap")]);
        let tx = rtx.tx();
        let (lifted, elsewhere) = (home.addons("Inner"), home.base.join("elsewhere"));
        BEFORE_ROLLBACK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::rename(&lifted, &elsewhere).unwrap()
            }));
        });
        let err = with_spec("fsx.mutate.executed=error@2", || {
            commit_with(&home, rtx, &lowered, &fsx::mount_id)
        })
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal, "{}", err.message);
        assert_eq!(
            err.details["reason"], "step_unrecognized",
            "{}",
            err.details
        );
        assert_eq!(err.details["journal_pending"], tx.to_string());
        assert!(err.message.contains("journal step 1"), "{}", err.message);
        assert!(
            home.addons("Wrap/README.txt").is_file(),
            "step 2 was inverted first"
        );
        assert_eq!(
            journal_lines(&home, &tx),
            expected(2, &[]),
            "no rolled_back line"
        );
        assert!(
            home.paths.journal_repair(&tx).exists(),
            "the journal is kept"
        );
    }

    #[test]
    fn a_failure_after_done_rolls_forward_in_process() {
        for (case, spec) in [
            ("history_failpoint", "doctor.repair.history=error"),
            ("done_failpoint", "doctor.repair.done=error"),
            // Durable appends: `prepared`, `step 1`, the chmod's actions line, `done`, then
            // the history line.
            ("history_append", "fsx.append_durable.before=error@5"),
        ] {
            let home = repair_home();
            let (rtx, lowered) = lowered(&home, &[chmod(&home)]);
            let tx = rtx.tx();
            let (outcome, _) =
                with_spec(spec, || commit_with(&home, rtx, &lowered, &fsx::mount_id))
                    .unwrap_or_else(|err| panic!("{case}: {err}")); // ubs:ignore — fails the test
            assert!(
                matches!(outcome, RepairOutcome::Committed { .. }),
                "{case}: {outcome:?}"
            );
            assert_eq!(
                repair_lines(&home, &tx).len(),
                1,
                "{case}: exactly one history line"
            );
            assert_eq!(
                journal_lines(&home, &tx),
                expected(1, &["done", "committed"]),
                "{case}"
            );
            assert_eq!(mode_of(&home.credential()), 0o600, "{case}");
        }
    }

    #[test]
    fn revalidation_writes_nothing() {
        type Change = fn(&RepairHome, &mut Lowered);
        let cases: Vec<(&str, Change)> = vec![
            ("run_dir_missing", |h, _| {
                std::fs::remove_dir(&h.run.dir).unwrap()
            }),
            ("marker_changed", |h, _| {
                crate::engine::testing::mark(h.client.client_dir(), crate::model::TargetKey::new());
            }),
            ("mount_changed", |_, _| {}),
            ("invalid_step", |_, lowered| lowered.steps[0].step.seq = 2),
        ];
        for (case, change) in cases {
            let home = repair_home();
            let (rtx, mut lowered) = lowered(&home, &[flatten(&home, "Wrap/Inner", "Wrap")]);
            let tx = rtx.tx();
            change(&home, &mut lowered);
            let meta_dir = home.client.meta_dir();
            let other_mount = move |p: &Path| Ok(if p.starts_with(&meta_dir) { 2 } else { 1 });
            let mount: &dyn Fn(&Path) -> io::Result<u64> = if case == "mount_changed" {
                &other_mount
            } else {
                &fsx::mount_id
            };
            // The in-flight lock file exists already (the transaction began at lowering);
            // taking the global lock writes its own file, which says nothing of a repair.
            let lock = [home.paths.lock_file()];
            drop(GlobalLock::acquire(&home.paths, "test").unwrap());
            let before = tree_hash_except(&home.base, &lock);
            let err = commit_with(&home, rtx, &lowered, mount).unwrap_err();
            assert_eq!(
                tree_hash_except(&home.base, &lock),
                before,
                "{case}: nothing written: {err}"
            );
            assert!(
                !home.paths.journal_repair(&tx).exists(),
                "{case}: no journal"
            );
            assert!(
                !InflightLock::is_live(&home.paths, &tx).unwrap(),
                "{case}: lock released"
            );
            for root in &lowered.roots {
                assert!(!root.exists(), "{case}: no root {}", root.display());
            }
        }
    }

    #[test]
    fn the_roots_exist_before_prepared() {
        let home = repair_home();
        let (rtx, lowered) = lowered(&home, &[flatten(&home, "Wrap/Inner", "Wrap")]);
        let tx = rtx.tx();
        let before = tree_hash(&home.addons(""));
        let (outcome, _) = with_spec("doctor.repair.prepared=error", || {
            commit_with(&home, rtx, &lowered, &fsx::mount_id)
        })
        .unwrap();
        assert!(
            matches!(
                outcome,
                RepairOutcome::RolledBack {
                    failed_seq: None,
                    backups_restored: 0,
                    ..
                }
            ),
            "{outcome:?}"
        );
        assert!(!lowered.roots.is_empty());
        for root in &lowered.roots {
            assert!(root.is_dir(), "{}", root.display());
        }
        assert_eq!(journal_lines(&home, &tx), ["prepared", "rolled_back"]);
        assert_eq!(tree_hash(&home.addons("")), before, "nothing else changed");
    }

    #[test]
    fn cleanup_rings_the_doorbell_and_prunes_under_its_own_lock() {
        let home = repair_home();
        let (tx, result) = commit_on(&home, &[chmod(&home)]);
        let (_, cleanup) = result.unwrap();
        assert!(home.paths.inflight_lock(&tx).exists(), "held until cleanup");
        assert!(cleanup.finish().is_empty());
        let bell: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.paths.doorbell_file()).unwrap())
                .unwrap();
        assert_eq!(
            (bell["command"].as_str(), bell["actor"].as_str()),
            (Some("doctor fix"), Some("human"))
        );
        assert!(!home.paths.journal_repair(&tx).exists());
        assert!(!home.paths.inflight_lock(&tx).exists());
        let tail = std::fs::read_to_string(home.paths.history_tail()).unwrap();
        assert!(
            tail.contains(&tx.to_string()),
            "the history projection covers the repair"
        );
    }
}
