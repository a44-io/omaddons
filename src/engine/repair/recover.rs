//! Recovering a repair a crash left (E5, M4-D31): every command that takes the global
//! lock settles each repair journal whose writer is gone, after the folder journals
//! (`journal::recover_all`, and so `recover_under` and `recover_waiting`).
//!
//! - **By phase.** A journal that never prepared, that committed or that an in-process
//!   rollback finished is pruned silently. One that stopped after `prepared` or a `step`
//!   line is rolled back by its steps' inverses ([`super::inverse::roll_back`], the
//!   routine the commit runs in process) and ends `rolled_back`; one that reached `done`
//!   is rolled forward ([`super::inverse::roll_forward`]). Either way only the journal
//!   and its in-flight lock are pruned: the run directory, the client-side `.repair`
//!   half, the cache quarantine and the backups wait for `doctor gc` (M4-D36).
//! - **Its own scope table**, built from the journal alone: the client dirs whose markers
//!   still name their targets, and the skill directories omaddons attributes without the
//!   journal's word. Recovery never takes write authority from a payload it cannot
//!   attribute independently (M4-D38's principle).
//! - **Stops.** A journal this build cannot read, one naming a client whose marker
//!   changed or a skill directory it cannot attribute, and a recorded step in neither its
//!   before nor its after state stop recovery with everything in place, the transaction
//!   in `details.journal_pending` and `target: null` (a repair names no single target).
//!   Until E14's fences, the first stop fails the command that recovers, so every
//!   committing command fails while `recover_under`'s callers and readers only warn.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::inverse::{OnDisk, append, manual_repair, pending, roll_back, roll_forward};
use super::journal::{self, ParsedRepair, RepairLine, RepairPhase, RepairPrepared};
use crate::engine::journal::{RecoveryEnv, StopReason, Unrecoverable, unattributed};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    self, InflightLock, LockUse, PruneReason, ScopeTable, WriteSession, Writer, canonical_lenient,
};
use crate::model::TxId;
use crate::state::paths::Paths;

/// A repair journal recovery still needs: started and neither committed nor rolled back,
/// or one this build cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingRepair {
    pub tx: TxId,
    /// Why recovery cannot finish it, when it cannot ([`Unrecoverable`]).
    pub unrecoverable: Option<Unrecoverable>,
}

/// The repair journals recovery still needs, by transaction, for readers and
/// `recoverable`. Writes nothing. A journal that cannot be read at all counts: recovery
/// reports it.
pub fn pending_repairs(paths: &Paths) -> Result<Vec<PendingRepair>> {
    let mut found = Vec::new();
    for (tx, path) in journal::list(paths).map_err(|error| unattributed(error, None))? {
        let unrecoverable = match journal::read(&path) {
            Ok(parsed) => match parsed.unrecoverable() {
                None if matches!(
                    parsed.phase,
                    RepairPhase::Committed | RepairPhase::RolledBack
                ) =>
                {
                    continue;
                }
                why => why,
            },
            Err(_) => None,
        };
        found.push(PendingRepair { tx, unrecoverable });
    }
    Ok(found)
}

/// Recovers every repair journal whose writer is gone, in transaction order, under the
/// lock `env` holds; `env.scopes` serves only the silent prunes. One `journal_recovered`
/// warning per repair rolled back or forward. The first stop is returned as the error,
/// with everything that journal left in place.
pub fn recover_repairs(env: &RecoveryEnv<'_>) -> Result<Vec<Warning>> {
    let mut warnings = Vec::new();
    for (tx, path) in journal::list(env.paths).map_err(|error| unattributed(error, None))? {
        if InflightLock::is_live(env.paths, &tx).map_err(|error| unattributed(error, Some(&tx)))? {
            continue;
        }
        let parsed = journal::read(&path).map_err(|error| pending(error, tx))?;
        warnings.extend(recover_one(env, tx, &path, &parsed)?);
    }
    Ok(warnings)
}

/// Settles one repair journal by its phase, then prunes it and its in-flight lock.
fn recover_one(
    env: &RecoveryEnv<'_>,
    tx: TxId,
    path: &Path,
    parsed: &ParsedRepair,
) -> Result<Option<Warning>> {
    if let Some(why) = parsed.unrecoverable() {
        return Err(unrecoverable(tx, parsed, why));
    }
    let settled = match (&parsed.prepared, parsed.phase) {
        (Some(prepared), RepairPhase::Prepared | RepairPhase::Stepped(_) | RepairPhase::Done) => {
            let scopes = scopes_of(env.paths, tx, prepared)?;
            let mut s = WriteSession::untracked(Writer::Doctor, &scopes, "repair recovery");
            if parsed.phase == RepairPhase::Done {
                roll_forward(&mut s, env.paths, path, prepared, parsed.torn_tail)
                    .map_err(|error| pending(error, tx))?;
                Some("completed it (rolled forward)")
            } else {
                roll_back(&mut s, &mut OnDisk, prepared, parsed.steps)
                    .map_err(|error| pending(error, tx))?;
                // A torn last line would swallow the closing line; the prune follows,
                // and a crash before it only repeats an idempotent rollback.
                if !parsed.torn_tail {
                    append(&mut s, path, &RepairLine::RolledBack)
                        .map_err(|error| pending(error, tx))?;
                }
                Some("undid it (rolled back)")
            }
        }
        // Nothing to finish or undo: never prepared, committed, or rolled back in process.
        _ => None,
    };
    let mut victims = vec![path.to_path_buf()];
    let inflight = env.paths.inflight_lock(&tx);
    if inflight.exists() {
        victims.push(inflight);
    }
    fsx::prune(
        env.scopes,
        PruneReason::PostCommitCleanup,
        &victims,
        LockUse::Held(env.lock),
    )
    .map_err(|error| pending(error, tx))?;
    Ok(settled.map(|what| {
        Warning::new(
            WarningCode::JournalRecovered,
            format!("repair transaction {tx} was interrupted; recovery {what}"),
        )
        .with_fix(Suggestion::command(["omaddons", "history", "--limit", "5"]))
    }))
}

/// The scope table a repair's recovery writes with, from its journal alone: the client
/// dirs whose markers still name their targets (registration writes markers under the
/// lock, before any journal), and the skill directories [`skill_dirs`] admits. A client
/// dir whose marker changed stops recovery: it is not that target's any more.
fn scopes_of(paths: &Paths, tx: TxId, prepared: &RepairPrepared) -> Result<ScopeTable> {
    let mut clients = Vec::new();
    for client in &prepared.client_dirs {
        // ubs:ignore — comparing target keys (public ULIDs), not secrets
        if crate::discover::registry::read_marker(&client.dir) != Some(client.target) {
            return Err(stop(
                ErrorCode::ScopeDenied,
                tx,
                StopReason::MarkerMismatch,
                format!(
                    "it names {}, whose marker is not target {}'s any more; recovery leaves it as it is",
                    client.dir.display(),
                    client.target
                ),
                manual_repair(),
            ));
        }
        clients.push(client.dir.clone());
    }
    let skills = skill_dirs(paths, tx, prepared, &clients)?;
    ScopeTable::new(paths, &clients, None)
        .and_then(|table| table.with_skill_dirs(&skills))
        .map_err(|error| pending(error, tx))
}

/// The skill directories `prepared` writes in. Every path a step or a rollback names is
/// below omaddons' own folders, the bar widget's or one of `clients`, or below a skill
/// directory omaddons attributes on its own: `Paths::skill_dir()` (B3 adds
/// `history_tail.skill_dirs[]`). Any other path stops recovery (`skill_dir`).
fn skill_dirs(
    paths: &Paths,
    tx: TxId,
    prepared: &RepairPrepared,
    clients: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    let resolved = |dirs: Vec<PathBuf>| -> Vec<PathBuf> {
        dirs.iter()
            .filter_map(|dir| canonical_lenient(dir))
            .collect()
    };
    let fixed = resolved(
        [
            paths.state_dir().to_path_buf(),
            paths.config_dir().to_path_buf(),
            paths.cache_dir().to_path_buf(),
            paths.omarchy_plugins_dir(),
        ]
        .into_iter()
        .chain(clients.iter().cloned())
        .collect(),
    );
    let attributable = resolved(vec![paths.skill_dir()]);
    let below = |path: &Path, dir: &PathBuf| path != dir.as_path() && path.starts_with(dir);
    let mut admitted: Vec<PathBuf> = Vec::new();
    for path in named(prepared) {
        if fixed.iter().any(|root| below(path, root)) {
            continue;
        }
        let Some(dir) = attributable.iter().find(|dir| below(path, dir)) else {
            return Err(stop(
                ErrorCode::ScopeDenied,
                tx,
                StopReason::SkillDir,
                format!(
                    "it writes in {}, which is not a skill directory omaddons installed; recovery leaves it as it is",
                    path.parent().unwrap_or(path).display()
                ),
                manual_repair(),
            ));
        };
        if !admitted.contains(dir) {
            admitted.push(dir.clone());
        }
    }
    Ok(admitted)
}

/// Every path a repair's steps and rollbacks name: each primitive's path, a rename's
/// destination, a write's backup and a created file's rollback destination.
fn named(prepared: &RepairPrepared) -> impl Iterator<Item = &Path> {
    use crate::engine::repair_action::Primitive;
    let steps = prepared.primitives.iter().flat_map(|step| {
        let other = match &step.op {
            Primitive::RenameNoReplace { to, .. } => Some(to.as_path()),
            Primitive::WriteFile { backup, .. } => backup.as_deref(),
            Primitive::Chmod { .. } => None,
        };
        std::iter::once(step.op.path()).chain(other)
    });
    steps.chain(
        prepared
            .rollbacks
            .iter()
            .map(|rollback| rollback.to.as_path()),
    )
}

/// A repair journal this build cannot read: `state_too_new` for a newer major format,
/// `state_corrupt` for lines it cannot read, left as it is.
fn unrecoverable(tx: TxId, parsed: &ParsedRepair, why: Unrecoverable) -> Error {
    let (code, reason, what) = match why {
        Unrecoverable::TooNew => (
            ErrorCode::StateTooNew,
            StopReason::JournalTooNew,
            format!(
                "a newer omaddons started it (journal format {}; this one reads {}), so it is left as it is",
                parsed
                    .format
                    .map_or_else(|| "unknown".to_owned(), |format| format.to_string()),
                journal::FORMAT
            ),
        ),
        Unrecoverable::Unreadable => (
            ErrorCode::StateCorrupt,
            StopReason::JournalUnreadable,
            "its journal holds lines this omaddons cannot read, so it is left for manual repair"
                .to_owned(),
        ),
    };
    stop(code, tx, reason, what, why.fix())
}

/// A stop of a repair's recovery: everything is left in place, and the reason is in the
/// text too, since `recover_under` turns the stop into a warning, which has no details.
fn stop(code: ErrorCode, tx: TxId, reason: StopReason, what: String, fix: Suggestion) -> Error {
    Error::new(
        code,
        format!(
            "repair transaction {tx} could not be recovered ({}): {what}",
            reason.as_str()
        ),
        fix,
    )
    .details(json!({
        "journal_pending": tx,
        "reason": reason.as_str(),
        "target": null,
    }))
}

/// What a reader says of a repair journal this build cannot recover.
pub(crate) fn unrecoverable_warning(tx: &TxId, why: Unrecoverable) -> Warning {
    Warning::new(
        WarningCode::JournalPending,
        format!(
            "repair transaction {tx} did not finish, and this omaddons cannot recover it ({}); it is left as it is",
            why.reason()
        ),
    )
    .with_fix(why.fix())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::engine::journal::{
        no_saved_variables, pending_warnings, recover_all, recover_under, recoverable,
    };
    use crate::engine::repair::{
        CommitCtx, LowerCtx, PlannedFix, RepairMeta, RepairTx, commit, history_line, lower,
    };
    use crate::engine::repair_action::{Fix, Mode, QuarantineRoot};
    use crate::engine::testing::{RepairHome, mark, repair_home, tree_hash_except};
    use crate::fsx::{GlobalLock, GlobalLockGuard, with_spec};
    use crate::model::{Actor, TargetKey};
    use crate::state::history::{self, HistoryFilter};

    const SKILL: &str = "fm-plugins-skill-version-drift";
    const FILE_MODE: &str = "fm-secrets-credential-file-mode";
    const NESTED: &str = "fm-external_artifacts-nested-addon-folder";
    const CORRUPT: &str = "fm-schemas-state-corrupt";
    const NEW_GUIDE: &[u8] = b"---\nname: omaddons\n---\nnew guide\n";
    /// The skill `repair_home` installs.
    const OLD_GUIDE: &[u8] = b"---\nname: omaddons\n---\nold guide\n";

    fn planned(fm: &'static str, fix: Fix) -> PlannedFix {
        PlannedFix {
            fm: Cow::Borrowed(fm),
            fix,
        }
    }

    fn mode(bits: u32) -> Mode {
        Mode::new(bits).unwrap()
    }

    /// Two renames: `Wrap/Inner` up to `Inner`, then `Wrap` into the client quarantine.
    fn flatten(home: &RepairHome) -> PlannedFix {
        planned(
            NESTED,
            Fix::Flatten {
                inner: home.addons("Wrap/Inner"),
                outer: home.addons("Wrap"),
            },
        )
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

    /// `Stray/` into the client quarantine.
    fn stray(home: &RepairHome) -> PlannedFix {
        planned(
            CORRUPT,
            Fix::Quarantine {
                path: home.addons("Stray"),
                root: QuarantineRoot::Client {
                    client_dir: home.client.client_dir().to_path_buf(),
                },
            },
        )
    }

    /// The cache's temporary link into the cache quarantine.
    fn link(home: &RepairHome) -> PlannedFix {
        planned(
            CORRUPT,
            Fix::Quarantine {
                path: home.temp_link(),
                root: QuarantineRoot::Cache,
            },
        )
    }

    /// Where the client quarantine keeps `Stray/`.
    fn parked_stray(home: &RepairHome, tx: &TxId) -> PathBuf {
        home.client
            .snapshots_repair(tx)
            .join("quarantine/Interface/AddOns/Stray")
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

    /// `fixes` lowered on `home` with `skill_dirs` admitted, then committed with `spec`
    /// armed. A `panic` action stands in for a crash: the commit stops there, its writer
    /// gone (its in-flight lock released), its journal as it was. A commit that ends
    /// leaves its journal too: its cleanup is dropped unrun.
    fn crashed_with(
        home: &RepairHome,
        skill_dirs: &[PathBuf],
        fixes: &[PlannedFix],
        spec: &str,
    ) -> TxId {
        let scopes = home.scopes.clone().with_skill_dirs(skill_dirs).unwrap();
        let rtx = RepairTx::begin(&home.paths).unwrap();
        let tx = rtx.tx();
        let lowering = LowerCtx {
            paths: &home.paths,
            tx,
            run: &home.run,
            clients: &home.planned,
            skill_dirs,
            scopes: &scopes,
            mount_id: &fsx::mount_id,
        };
        let lowered = lower(&lowering, fixes).unwrap();
        let lock = GlobalLock::acquire(&home.paths, "test").unwrap();
        let ctx = CommitCtx {
            lock: &lock,
            scopes: &scopes,
            run: &home.run,
            mount_id: &fsx::mount_id,
        };
        let _ = catch_unwind(AssertUnwindSafe(|| {
            with_spec(spec, || commit(&ctx, rtx, &lowered, &meta()))
        }));
        tx
    }

    fn crashed(home: &RepairHome, fixes: &[PlannedFix], spec: &str) -> TxId {
        crashed_with(home, &home.skill_dirs, fixes, spec)
    }

    fn env<'a>(home: &'a RepairHome, lock: &'a GlobalLockGuard) -> RecoveryEnv<'a> {
        RecoveryEnv {
            lock,
            paths: &home.paths,
            scopes: &home.scopes,
            own: &[],
            sv_writable: &no_saved_variables,
        }
    }

    /// `recover_repairs` under the global lock, as `recover_all` runs it after the folder
    /// journals.
    fn recover(home: &RepairHome) -> Result<Vec<Warning>> {
        let lock = GlobalLock::acquire(&home.paths, "test").unwrap();
        recover_repairs(&env(home, &lock))
    }

    /// Removes the stuck folder journal `repair_home` leaves, and its in-flight lock.
    fn no_folder_journal(home: &RepairHome) {
        let stuck = home.stuck();
        std::fs::remove_file(home.paths.journal(&stuck)).unwrap();
        std::fs::remove_file(home.paths.inflight_lock(&stuck)).unwrap();
    }

    /// What a repair here may change, by content: every entry below AddOns, the skill
    /// directory and the cache's `tmp/`, and the credential, each as its kind, mode and
    /// bytes or link target. Times and inodes are left out: a rename there and back
    /// changes a directory's time, not what it holds.
    fn subject(home: &RepairHome) -> BTreeMap<String, String> {
        let mut found = BTreeMap::new();
        for root in [
            home.client.addons(),
            home.paths.skill_dir(),
            home.paths.cache_dir().join("tmp"),
            home.credential(),
        ] {
            shape(&home.base, &root, &mut found);
        }
        found
    }

    fn shape(base: &Path, path: &Path, found: &mut BTreeMap<String, String>) {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return;
        };
        let mode = meta.permissions().mode() & 0o7777;
        let what = if meta.file_type().is_symlink() {
            let target = std::fs::read_link(path).unwrap();
            let target = target.strip_prefix(base).unwrap_or(&target).to_path_buf();
            format!("L:{}", target.display())
        } else if meta.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                shape(base, &entry.unwrap().path(), found);
            }
            format!("D:{mode:o}")
        } else {
            let digest = Sha256::digest(std::fs::read(path).unwrap());
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            format!("F:{mode:o}:{hex}")
        };
        let rel = path.strip_prefix(base).unwrap().display().to_string();
        found.insert(rel, what);
    }

    /// The subject once `fixes` committed on a fresh home.
    fn after(fixes: fn(&RepairHome) -> Vec<PlannedFix>) -> BTreeMap<String, String> {
        let home = repair_home();
        crashed(&home, &fixes(&home), "");
        subject(&home)
    }

    fn history_lines(home: &RepairHome, tx: &TxId) -> usize {
        history::read(&home.paths, &HistoryFilter::default())
            .unwrap()
            .entries
            .iter()
            .filter(|entry| entry.tx == *tx)
            .count()
    }

    /// Whether the repair's journal and its in-flight lock are still there.
    fn left(home: &RepairHome, tx: &TxId) -> (bool, bool) {
        (
            home.paths.journal_repair(tx).exists(),
            home.paths.inflight_lock(tx).exists(),
        )
    }

    fn phase(home: &RepairHome, tx: &TxId) -> RepairPhase {
        journal::read(&home.paths.journal_repair(tx)).unwrap().phase
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    }

    fn set_mode(path: &Path, bits: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
    }

    /// Rewrites the journal of `tx` through `edit`.
    fn edit_journal(home: &RepairHome, tx: &TxId, edit: impl FnOnce(String) -> String) {
        let path = home.paths.journal_repair(tx);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, edit(text)).unwrap();
    }

    fn appended(path: &Path, text: &str) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    /// The messages of `warnings` with `code`.
    fn messages(warnings: &[Warning], code: WarningCode) -> Vec<&str> {
        warnings
            .iter()
            .filter(|w| w.code == code)
            .map(|w| w.message.as_str())
            .collect()
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum End {
        Before,
        After,
    }

    type Setup = fn(&RepairHome) -> TxId;
    /// A case: its name, how its journal is left, the phase that leaves, the end state,
    /// what the warning says recovery did (none: silent) and the history lines after.
    type PhaseCase = (
        &'static str,
        Setup,
        RepairPhase,
        End,
        Option<&'static str>,
        usize,
    );

    #[test]
    fn recovery_settles_each_phase() {
        let flattened = after(|h| vec![flatten(h)]);
        let cases: Vec<PhaseCase> = vec![
            (
                "unprepared_empty",
                |h| {
                    let tx = TxId::new();
                    std::fs::write(h.paths.journal_repair(&tx), "").unwrap();
                    std::fs::write(h.paths.inflight_lock(&tx), "").unwrap();
                    tx
                },
                RepairPhase::Unprepared,
                End::Before,
                None,
                0,
            ),
            (
                "unprepared_torn",
                |h| {
                    let tx = crashed(h, &[flatten(h)], "doctor.repair.prepared=panic");
                    edit_journal(h, &tx, |text| text[..text.len() / 2].to_owned());
                    tx
                },
                RepairPhase::Unprepared,
                End::Before,
                None,
                0,
            ),
            (
                "prepared",
                |h| crashed(h, &[flatten(h)], "doctor.repair.prepared=panic"),
                RepairPhase::Prepared,
                End::Before,
                Some("undid it (rolled back)"),
                0,
            ),
            (
                "stepped_1_of_2",
                |h| crashed(h, &[flatten(h)], "doctor.repair.step=panic@1"),
                RepairPhase::Stepped(1),
                End::Before,
                Some("undid it (rolled back)"),
                0,
            ),
            (
                "stepped_2_of_2_ran",
                |h| crashed(h, &[flatten(h)], "fsx.mutate.executed=panic@2"),
                RepairPhase::Stepped(2),
                End::Before,
                Some("undid it (rolled back)"),
                0,
            ),
            (
                "done",
                |h| crashed(h, &[flatten(h)], "doctor.repair.done=panic"),
                RepairPhase::Done,
                End::After,
                Some("completed it (rolled forward)"),
                1,
            ),
            (
                "committed",
                |h| crashed(h, &[flatten(h)], ""),
                RepairPhase::Committed,
                End::After,
                None,
                1,
            ),
            (
                "rolled_back",
                |h| crashed(h, &[flatten(h)], "doctor.repair.step=error@2"),
                RepairPhase::RolledBack,
                End::Before,
                None,
                0,
            ),
        ];
        for (case, setup, at_crash, end, warned, lines) in cases {
            let home = repair_home();
            let before = subject(&home);
            let tx = setup(&home);
            assert_eq!(phase(&home, &tx), at_crash, "{case}: the crash's phase");
            let warnings = recover(&home).unwrap_or_else(|err| panic!("{case}: {err}")); // ubs:ignore — fails the test
            let recovered = messages(&warnings, WarningCode::JournalRecovered);
            match warned {
                Some(what) => assert_eq!(
                    recovered,
                    [format!(
                        "repair transaction {tx} was interrupted; recovery {what}"
                    )],
                    "{case}"
                ),
                None => assert!(warnings.is_empty(), "{case}: silent: {warnings:?}"),
            }
            let want = match end {
                End::Before => &before,
                End::After => &flattened,
            };
            assert_eq!(&subject(&home), want, "{case}: the {end:?} state");
            assert_eq!(history_lines(&home, &tx), lines, "{case}: history lines");
            assert_eq!(left(&home, &tx), (false, false), "{case}: journal and lock");
        }
    }

    type Fixes = fn(&RepairHome) -> PlannedFix;
    type Touch = fn(&RepairHome, &TxId);
    /// What must hold once a case recovered, each named.
    type Holds = fn(&RepairHome, &TxId) -> Vec<(&'static str, bool)>;
    /// A case: its name, the fix, the crash, what changes after it, whether recovery
    /// stops, and what must hold.
    type ClassCase = (&'static str, Fixes, &'static str, Touch, bool, Holds);

    #[test]
    fn recovery_classifies_by_predicates() {
        let nothing: Touch = |_, _| {};
        let ran = "fsx.mutate.executed=panic@1";
        let not_run = "doctor.repair.step=panic@1";
        let old_skill: Holds = |h, _| {
            vec![
                (
                    "old bytes",
                    std::fs::read(h.skill_file()).unwrap() == OLD_GUIDE,
                ),
                // ubs:ignore — comparing a file's mode bits, not a secret
                ("old mode", mode_of(&h.skill_file()) == 0o640),
            ]
        };
        // ubs:ignore — comparing a file's mode bits, not a secret
        let old_mode: Holds = |h, _| vec![("mode 0644", mode_of(&h.credential()) == 0o644)];
        let stray_back: Holds = |h, tx| {
            vec![
                ("back in AddOns", h.addons("Stray/stray.lua").is_file()),
                ("not parked", !parked_stray(h, tx).exists()),
            ]
        };
        let cases: Vec<ClassCase> = vec![
            ("write_post", write_over, ran, nothing, false, old_skill),
            ("write_pre", write_over, not_run, nothing, false, old_skill),
            (
                "write_neither",
                write_over,
                ran,
                |h, _| std::fs::write(h.skill_file(), "edited by hand\n").unwrap(),
                true,
                |h, _| {
                    vec![(
                        "left as found",
                        std::fs::read(h.skill_file()).unwrap() == b"edited by hand\n",
                    )]
                },
            ),
            ("create_post", write_create, ran, nothing, false, |h, _| {
                let kept = h.run.dir.join("quarantine/skill/rolled_back/1");
                vec![
                    ("gone from its place", !notes(h).exists()),
                    (
                        "quarantined, never deleted",
                        std::fs::read(kept).is_ok_and(|bytes| bytes == b"notes\n"),
                    ),
                ]
            }),
            ("chmod_post", chmod, ran, nothing, false, old_mode),
            ("chmod_pre", chmod, not_run, nothing, false, old_mode),
            (
                "chmod_neither",
                chmod,
                ran,
                |h, _| set_mode(&h.credential(), 0o640),
                true,
                // ubs:ignore — comparing a file's mode bits, not a secret
                |h, _| vec![("left as found", mode_of(&h.credential()) == 0o640)],
            ),
            ("rename_dir_post", stray, ran, nothing, false, stray_back),
            ("rename_dir_pre", stray, not_run, nothing, false, stray_back),
            (
                "rename_dir_neither",
                stray,
                ran,
                |h, tx| std::fs::rename(parked_stray(h, tx), h.base.join("elsewhere")).unwrap(),
                true,
                |h, _| {
                    vec![
                        ("source still absent", !h.addons("Stray").exists()),
                        ("moved away by hand", h.base.join("elsewhere").is_dir()),
                    ]
                },
            ),
            (
                "rename_dir_content_changed_after_crash",
                stray,
                ran,
                |h, tx| std::fs::write(parked_stray(h, tx).join("added.txt"), "x\n").unwrap(),
                false,
                |h, tx| {
                    vec![
                        ("back by identity", h.addons("Stray/added.txt").is_file()),
                        ("not parked", !parked_stray(h, tx).exists()),
                    ]
                },
            ),
            ("symlink_post", link, ran, nothing, false, |h, _| {
                vec![(
                    "the link is back",
                    std::fs::read_link(h.temp_link())
                        .is_ok_and(|target| target == Path::new("../blobs/ab")),
                )]
            }),
        ];
        for (case, fix, spec, touch, stops, holds) in cases {
            let home = repair_home();
            set_mode(&home.skill_file(), 0o640);
            let tx = crashed(&home, &[fix(&home)], spec);
            touch(&home, &tx);
            match recover(&home) {
                Ok(warnings) => {
                    assert!(!stops, "{case}: recovered: {warnings:?}");
                    assert_eq!(
                        messages(&warnings, WarningCode::JournalRecovered).len(),
                        1,
                        "{case}: {warnings:?}"
                    );
                    assert_eq!(left(&home, &tx), (false, false), "{case}");
                }
                Err(err) => {
                    assert!(stops, "{case}: {err}");
                    assert_eq!(
                        (err.code, err.details["reason"].as_str()),
                        (ErrorCode::Internal, Some("step_unrecognized")),
                        "{case}: {err}"
                    );
                    assert!(err.message.contains("journal step 1"), "{case}: {err}");
                    assert_eq!(left(&home, &tx), (true, true), "{case}: kept");
                }
            }
            for (what, held) in holds(&home, &tx) {
                assert!(held, "{case}: {what}");
            }
        }
    }

    #[test]
    fn recovery_is_idempotent() {
        let home = repair_home();
        let before = subject(&home);
        let tx = crashed(&home, &[flatten(&home)], "fsx.mutate.executed=panic@2");
        assert_eq!(recover(&home).unwrap().len(), 1);
        assert_eq!(subject(&home), before);
        let lock = [home.paths.lock_file()];
        let once = tree_hash_except(&home.base, &lock);
        assert!(recover(&home).unwrap().is_empty(), "nothing left to do");
        assert_eq!(
            tree_hash_except(&home.base, &lock),
            once,
            "and nothing written"
        );
        assert_eq!(left(&home, &tx), (false, false));
    }

    #[test]
    fn roll_forward_never_appends_twice() {
        let home = repair_home();
        let tx = crashed(&home, &[chmod(&home)], "doctor.repair.history=panic");
        assert_eq!(phase(&home, &tx), RepairPhase::Done);
        assert_eq!(history_lines(&home, &tx), 1, "written before the crash");
        let warnings = recover(&home).unwrap();
        assert_eq!(
            messages(&warnings, WarningCode::JournalRecovered),
            [format!(
                "repair transaction {tx} was interrupted; recovery completed it (rolled forward)"
            )]
        );
        assert_eq!(history_lines(&home, &tx), 1, "not appended again");
        assert_eq!(mode_of(&home.credential()), 0o600, "the repair stands");
    }

    #[test]
    fn a_torn_history_fragment_naming_the_tx_does_not_hide_it() {
        let home = repair_home();
        let tx = crashed(&home, &[chmod(&home)], "doctor.repair.done=panic");
        let prepared = journal::read(&home.paths.journal_repair(&tx))
            .unwrap()
            .prepared
            .unwrap();
        let line = serde_json::to_string(&history_line(&prepared)).unwrap();
        let fragment = &line[..line.len() / 2];
        assert!(fragment.contains(&tx.to_string()), "{fragment}");
        appended(&home.paths.history_file(), fragment);
        recover(&home).unwrap();
        assert_eq!(history_lines(&home, &tx), 1, "the whole line, once");
        let text = std::fs::read_to_string(home.paths.history_file()).unwrap();
        assert!(
            text.ends_with(&format!("{fragment}\n{line}\n")),
            "the fragment closed off, the line after it:\n{text}"
        );
    }

    #[test]
    fn a_torn_journal_tail_gets_no_closing_line() {
        let home = repair_home();
        let before = subject(&home);
        let tx = crashed(&home, &[flatten(&home)], "fsx.mutate.executed=panic@2");
        let torn = "{\"type\":\"do";
        appended(&home.paths.journal_repair(&tx), torn);
        // The prune fails once the journal is in the trash, so what recovery left in it
        // can be read there.
        let err = with_spec("fsx.prune.renamed=error", || recover(&home)).unwrap_err();
        assert_eq!(err.details["journal_pending"], tx.to_string(), "{err}");
        assert_eq!(subject(&home), before, "rolled back");
        let name = format!("{tx}.repair.jsonl");
        let mut trashed = Vec::new();
        let mut dirs = vec![home.paths.state_dir().join("trash")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.to_string_lossy().ends_with(&name) {
                    trashed.push(path);
                }
            }
        }
        assert_eq!(trashed.len(), 1, "{trashed:?}");
        let text = std::fs::read_to_string(&trashed[0]).unwrap();
        assert!(
            text.ends_with(torn),
            "no closing line after the torn one:\n{text}"
        );
    }

    #[test]
    fn stops_leave_everything() {
        let cases: Vec<(&str, Setup, ErrorCode, &str)> = vec![
            (
                "unreadable",
                |h| {
                    let tx = crashed(h, &[chmod(h)], "doctor.repair.step=panic@1");
                    edit_journal(h, &tx, |text| {
                        let (first, rest) = text.split_once('\n').unwrap();
                        format!("{first}\nnot json\n{rest}")
                    });
                    tx
                },
                ErrorCode::StateCorrupt,
                "journal_unreadable",
            ),
            (
                "too_new",
                |h| {
                    let tx = crashed(h, &[chmod(h)], "doctor.repair.step=panic@1");
                    edit_journal(h, &tx, |text| {
                        text.replacen("\"format\":\"1.0\"", "\"format\":\"2.0\"", 1)
                    });
                    tx
                },
                ErrorCode::StateTooNew,
                "journal_too_new",
            ),
            (
                "marker_changed",
                |h| {
                    let tx = crashed(h, &[flatten(h)], "fsx.mutate.executed=panic@1");
                    mark(h.client.client_dir(), TargetKey::new());
                    tx
                },
                ErrorCode::ScopeDenied,
                "marker_mismatch",
            ),
            (
                "unknown_skill_dir",
                |h| {
                    let other = h.paths.home().join("agent-2/skills/omaddons");
                    std::fs::create_dir_all(&other).unwrap();
                    std::fs::write(other.join("SKILL.md"), OLD_GUIDE).unwrap();
                    let fix = planned(
                        SKILL,
                        Fix::Write {
                            path: other.join("SKILL.md"),
                            content: NEW_GUIDE.to_vec(),
                            mode: mode(0o644),
                        },
                    );
                    let dirs = [h.paths.skill_dir(), other];
                    crashed_with(h, &dirs, &[fix], "fsx.mutate.executed=panic@1")
                },
                ErrorCode::ScopeDenied,
                "skill_dir",
            ),
            (
                "step_unrecognized",
                |h| {
                    let tx = crashed(h, &[chmod(h)], "fsx.mutate.executed=panic@1");
                    set_mode(&h.credential(), 0o640);
                    tx
                },
                ErrorCode::Internal,
                "step_unrecognized",
            ),
        ];
        for (case, setup, code, reason) in cases {
            let home = repair_home();
            let tx = setup(&home);
            let lock = [home.paths.lock_file()];
            let before = tree_hash_except(&home.base, &lock);
            let err = recover(&home).unwrap_err();
            assert_eq!(err.code, code, "{case}: {err}");
            assert_eq!(err.details["reason"], reason, "{case}: {}", err.details);
            assert_eq!(
                err.details["journal_pending"],
                tx.to_string(),
                "{case}: {}",
                err.details
            );
            assert_eq!(
                err.details.get("target"),
                Some(&serde_json::Value::Null),
                "{case}: a repair names no single target: {}",
                err.details
            );
            assert!(
                err.message.contains(&format!(
                    "repair transaction {tx} could not be recovered ({reason})"
                )),
                "{case}: {err}"
            );
            assert!(
                code.check_details(&err.details).is_ok(),
                "{case}: its details fit the code's spec: {}",
                err.details
            );
            assert_eq!(
                tree_hash_except(&home.base, &lock),
                before,
                "{case}: everything left in place: journal, lock, roots, backups, quarantine"
            );
            assert_eq!(left(&home, &tx), (true, true), "{case}");
        }
    }

    #[test]
    fn a_live_repair_is_skipped() {
        let home = repair_home();
        let tx = crashed(&home, &[flatten(&home)], "fsx.mutate.executed=panic@1");
        let holder = std::fs::File::open(home.paths.inflight_lock(&tx)).unwrap();
        holder.lock().unwrap();
        let lock = [home.paths.lock_file()];
        let before = tree_hash_except(&home.base, &lock);
        assert!(
            recover(&home).unwrap().is_empty(),
            "a live writer's journal is its own"
        );
        assert_eq!(tree_hash_except(&home.base, &lock), before);
        drop(holder);
        assert_eq!(recover(&home).unwrap().len(), 1, "once its writer is gone");
        assert_eq!(left(&home, &tx), (false, false));
    }

    #[test]
    fn recover_all_runs_repairs_after_folder_journals() {
        let home = repair_home();
        home.readable_stuck_journal();
        let tx = crashed(&home, &[flatten(&home)], "fsx.mutate.executed=panic@1");
        let lock = GlobalLock::acquire(&home.paths, "test").unwrap();
        let warnings = recover_all(&env(&home, &lock)).unwrap();
        let recovered = messages(&warnings, WarningCode::JournalRecovered);
        assert_eq!(recovered.len(), 2, "{warnings:?}");
        assert!(
            recovered[0].starts_with(&format!("transaction {} on target", home.stuck())),
            "the folder journal first: {recovered:?}"
        );
        assert!(
            recovered[1].starts_with(&format!("repair transaction {tx} ")),
            "then the repair: {recovered:?}"
        );
        assert!(!home.paths.journal(&home.stuck()).exists());
        assert_eq!(left(&home, &tx), (false, false));
    }

    #[test]
    fn a_stuck_repair_fails_recover_all_and_warns_under() {
        let home = repair_home();
        no_folder_journal(&home);
        let tx = crashed(&home, &[chmod(&home)], "fsx.mutate.executed=panic@1");
        set_mode(&home.credential(), 0o640);
        let lock = GlobalLock::acquire(&home.paths, "test").unwrap();
        let err = recover_all(&env(&home, &lock)).unwrap_err();
        assert_eq!(
            (err.code, err.details["reason"].as_str()),
            (ErrorCode::Internal, Some("step_unrecognized")),
            "every committing command fails until fences land: {err}"
        );
        assert_eq!(err.details["journal_pending"], tx.to_string());
        let warnings = recover_under(
            &lock,
            &home.paths,
            &no_saved_variables,
            "test",
            Actor::Human,
        );
        let pending = messages(&warnings, WarningCode::JournalPending);
        assert_eq!(pending.len(), 1, "{warnings:?}");
        assert!(
            pending[0].contains(&tx.to_string()) && pending[0].contains("(step_unrecognized)"),
            "the reason in its text: {}",
            pending[0]
        );
        assert_eq!(left(&home, &tx), (true, true), "kept");
        assert_eq!(mode_of(&home.credential()), 0o640, "left as found");
    }

    #[test]
    fn readers_report_pending_repairs_without_writing() {
        let home = repair_home();
        no_folder_journal(&home);
        let tx = crashed(&home, &[flatten(&home)], "fsx.mutate.executed=panic@1");
        let lock = [home.paths.lock_file()];
        let before = tree_hash_except(&home.base, &lock);
        let free = pending_warnings(&home.paths).unwrap();
        assert_eq!(
            messages(&free, WarningCode::JournalPending),
            [format!(
                "repair transaction {tx} did not finish; the next omaddons command that changes addons, settings or targets recovers it (dry runs never do)"
            )],
            "{free:?}"
        );
        assert!(recoverable(&home.paths).unwrap(), "its writer is gone");
        {
            let _held = GlobalLock::acquire(&home.paths, "test").unwrap();
            let busy = pending_warnings(&home.paths).unwrap();
            assert_eq!(
                messages(&busy, WarningCode::TxInProgress),
                [format!("repair transaction {tx} is in progress")],
                "{busy:?}"
            );
        }
        let holder = std::fs::File::open(home.paths.inflight_lock(&tx)).unwrap();
        holder.lock().unwrap();
        assert!(!recoverable(&home.paths).unwrap(), "its writer lives");
        drop(holder);
        assert_eq!(
            tree_hash_except(&home.base, &lock),
            before,
            "readers write nothing"
        );
        edit_journal(&home, &tx, |text| {
            text.replacen("\"format\":\"1.0\"", "\"format\":\"2.0\"", 1)
        });
        assert_eq!(
            pending_repairs(&home.paths).unwrap(),
            [PendingRepair {
                tx,
                unrecoverable: Some(Unrecoverable::TooNew),
            }]
        );
        let newer = pending_warnings(&home.paths).unwrap();
        assert!(
            messages(&newer, WarningCode::JournalPending)[0]
                .contains("cannot recover it (journal_too_new)"),
            "{newer:?}"
        );
    }

    #[test]
    fn finished_journals_are_not_pending() {
        for (case, spec) in [
            ("committed", ""),
            ("rolled_back", "doctor.repair.step=error@2"),
        ] {
            let home = repair_home();
            no_folder_journal(&home);
            crashed(&home, &[flatten(&home)], spec);
            assert_eq!(pending_repairs(&home.paths).unwrap(), [], "{case}");
            assert!(!recoverable(&home.paths).unwrap(), "{case}");
            assert!(pending_warnings(&home.paths).unwrap().is_empty(), "{case}");
        }
    }

    #[test]
    fn recovery_crashed_at_an_inverse_finishes_next_time() {
        let home = repair_home();
        let before = subject(&home);
        let tx = crashed(&home, &[flatten(&home)], "fsx.mutate.executed=panic@2");
        let err = with_spec("doctor.recover.step=error@1", || recover(&home)).unwrap_err();
        assert_eq!(err.details["journal_pending"], tx.to_string(), "{err}");
        assert_ne!(subject(&home), before, "only step 2 was inverted");
        assert_eq!(left(&home, &tx), (true, true), "kept for the next command");
        assert_eq!(recover(&home).unwrap().len(), 1);
        assert_eq!(subject(&home), before);
        assert_eq!(left(&home, &tx), (false, false));
    }

    #[test]
    fn recovery_prunes_only_the_journal_and_its_lock() {
        let home = repair_home();
        let fixes = [write_over(&home), stray(&home), link(&home)];
        let tx = crashed(&home, &fixes, "fsx.mutate.executed=panic@3");
        let backup = home.run.dir.join("backups/1");
        assert!(backup.is_file(), "the write's backup");
        recover(&home).unwrap();
        assert_eq!(left(&home, &tx), (false, false));
        for (what, kept) in [
            ("the run directory", home.run.dir.is_dir()),
            ("the backup", backup.is_file()),
            (
                "the client half",
                home.client.snapshots_repair(&tx).is_dir(),
            ),
            (
                "the cache quarantine",
                home.paths.cache_quarantine(&home.run.id).is_dir(),
            ),
        ] {
            assert!(kept, "{what} waits for doctor gc");
        }
    }
}
