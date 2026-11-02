//! `doctor gc` (M4-D36): the one command that deletes what the doctor keeps (old run
//! directories, the `.repair` client halves of repair transactions, the cache
//! quarantines), and never what an unfinished transaction, an interrupted undo or a
//! set-aside that can still be reactivated needs.
//!
//! The retention plan holds every safety rule of gc, and nothing in reach of it can
//! write: [`view`] reads the home once ([`GcView::load`]), and [`plan`](plan::plan) turns
//! what it read into the deletions, the exclusions with their pins, the undos lost,
//! `latest`'s move and the cap. The deletion itself is this module's alone (M4-D36: in
//! `src/doctor/`, only this file may prune).
//!
//! **The flow** ([`gc`]): plan from a view read without the lock; a dry run stops there
//! (its token compared when `--if-match` binds it). A real run takes the global lock,
//! reads a fresh view and plans again, and that plan is the one carried out: it refuses
//! while a repair journal cannot be read, compares `--if-match`, moves or removes
//! `latest`, then hands the lock to one `prune()` that renames every victim into trash
//! under it and deletes after releasing it. It never recovers a journal (recovering
//! first would change what the token covers before it is compared), writes no history
//! line and makes no run directory.

mod plan;
mod view;

use std::path::{Path, PathBuf};

use jiff::Timestamp;

pub use plan::{
    DeletionKind, ExcludedKind, GcDeletion, GcExclusion, GcPlan, GcReport, LatestChange, Pin,
    Victim, invariant_violations, plan,
};
pub use view::{
    Anchor, Candidate, Client, GcInputs, GcView, Group, GuardState, JournalView, LatestState,
    RepairLineView, RepairOpView, SuspendedRepair,
};

use crate::app::precondition::{self, GcTokenParts};
use crate::doctor::finding::FindingTarget;
use crate::engine::fence;
use crate::engine::journal::StopReason;
use crate::engine::repair::PreparedRead;
use crate::engine::repair::journal as repair_journal;
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{
    self, FenceStop, GlobalLock, GlobalLockGuard, LockUse, Op, PruneReason, ScopeTable,
    WriteSession, Writer,
};
use crate::limits::{DOCTOR_GC_MAX_DELETIONS, DOCTOR_SCAN_MAX_ENTRIES};
use crate::model::{Actor, TxId};
use crate::state::paths::Paths;

/// The command's name, as the lock holder, the doorbell and the token name it.
pub const COMMAND: &str = "doctor gc";

/// One `doctor gc` run.
#[derive(Debug, Clone, Copy)]
pub struct GcRequest<'a> {
    /// What started before it is eligible: resolved once, when the command started.
    pub before: Timestamp,
    pub dry_run: bool,
    /// `--if-match`: the preview's token the run is bound to.
    pub if_match: Option<&'a str>,
    /// The user's command line, whose `--dry-run` form is a stale token's fix.
    pub argv: &'a [String],
}

/// What gc does to the world, in the order [`gc`] calls it: [`Disk`] in the binary, a
/// recording double in the tests of the flow.
pub trait GcWorld {
    type Guard;
    /// The registered clients, read strictly (an unreadable registry is an error).
    fn clients(&self) -> Result<Vec<Client>>;
    /// A fresh view of the home.
    fn view(&self, clients: &[Client], before: Timestamp) -> Result<GcView>;
    /// The pre-lock plan is made (the `doctor.gc.planned` failpoint; an error it injects
    /// stops the run before anything changed).
    fn planned(&self) -> Result<()>;
    /// The global lock, within its timeout, or `locked`.
    fn lock(&self) -> Result<Self::Guard>;
    /// The repair journals' fences, under the lock: the stop error of one that closes
    /// every change (a repair journal whose first line cannot be read).
    fn fenced(&self, clients: &[Client]) -> Option<Error>;
    /// The stop error of repair journal `tx`, which this build cannot read.
    fn unknowable(&self, tx: TxId) -> Error;
    /// Points `latest` at `runs/<to>`, under the lock.
    fn retarget(&self, clients: &[Client], to: &str) -> Result<()>;
    /// Renames `latest` first when `remove_latest`, then `victims`, into trash under the
    /// lock, releases it, and deletes them; how many were deleted.
    fn delete(
        &self,
        clients: &[Client],
        remove_latest: bool,
        victims: &[PathBuf],
        guard: Self::Guard,
    ) -> Result<usize>;
    /// Rings the widget's doorbell.
    fn ring(&self);
}

/// Runs `doctor gc` against `world`: the report, `dry_run` and `precondition` filled.
pub fn gc<W: GcWorld>(world: &W, req: &GcRequest<'_>) -> Result<GcReport> {
    let preview = precondition::preview_argv(req.argv);
    let clients = world.clients()?;
    let first = plan(&world.view(&clients, req.before)?, DOCTOR_GC_MAX_DELETIONS);
    world.planned()?;
    if req.dry_run {
        let token = token_of(&first);
        precondition::check(req.if_match, &token, &preview)?;
        return Ok(GcReport {
            dry_run: true,
            precondition: Some(token.token()),
            ..first.report
        });
    }

    let guard = world.lock()?;
    // Under the lock everything is read again, and this plan is the one carried out.
    let clients = world.clients()?;
    if let Some(error) = world.fenced(&clients) {
        return Err(error);
    }
    let fresh = plan(&world.view(&clients, req.before)?, DOCTOR_GC_MAX_DELETIONS);
    if let Some(tx) = fresh.unknowable_journal {
        return Err(world.unknowable(tx));
    }
    precondition::check(req.if_match, &token_of(&fresh), &preview)?;

    let mut remove_latest = false;
    if let Some(change) = &fresh.report.latest {
        match &change.to {
            Some(to) => world.retarget(&clients, to)?,
            None => remove_latest = true,
        }
    }
    let victims: Vec<PathBuf> = fresh.victims.iter().map(|v| v.path.clone()).collect();
    if remove_latest || !victims.is_empty() {
        let deleted = world.delete(&clients, remove_latest, &victims, guard)?;
        if deleted > 0 {
            world.ring();
        }
    }
    Ok(GcReport {
        dry_run: false,
        precondition: None,
        ..fresh.report
    })
}

/// The plan's token.
fn token_of(plan: &GcPlan) -> precondition::Precondition {
    precondition::of_gc(&GcTokenParts {
        victims: &plan.victims,
        latest: plan.report.latest.as_ref(),
        undo_lost: &plan.report.lost_undos,
    })
}

/// The home on disk.
pub struct Disk<'a> {
    pub paths: &'a Paths,
    pub actor: Actor,
}

impl Disk<'_> {
    fn client_dirs(clients: &[Client]) -> Vec<PathBuf> {
        clients.iter().map(|c| c.dir.clone()).collect()
    }

    /// The write-scope table of a mutating command, with the fences of every stuck repair.
    fn scopes(&self, clients: &[Client]) -> Result<ScopeTable> {
        fence::table(self.paths, &Self::client_dirs(clients), None)
    }
}

impl GcWorld for Disk<'_> {
    type Guard = GlobalLockGuard;

    fn clients(&self) -> Result<Vec<Client>> {
        let registry = crate::state::registry::load_registry(self.paths)?.value;
        Ok(registry
            .targets
            .into_iter()
            .map(|entry| Client {
                dir: entry.client_dir,
                target: FindingTarget {
                    id: entry.id,
                    key: entry.key,
                },
            })
            .collect())
    }

    fn view(&self, clients: &[Client], before: Timestamp) -> Result<GcView> {
        GcView::load(&GcInputs {
            paths: self.paths,
            clients,
            now: crate::clock::now(),
            before,
            bound: DOCTOR_SCAN_MAX_ENTRIES,
        })
    }

    fn planned(&self) -> Result<()> {
        fsx::failpoint::hit("doctor.gc.planned").map_err(|err| {
            Error::new(
                ErrorCode::Io,
                format!("doctor gc stopped after planning: {err}"),
                Suggestion::text("nothing was deleted; run doctor gc again"),
            )
        })
    }

    fn lock(&self) -> Result<GlobalLockGuard> {
        GlobalLock::acquire(self.paths, COMMAND)
    }

    fn fenced(&self, clients: &[Client]) -> Option<Error> {
        let fences = fence::load(self.paths, &Self::client_dirs(clients));
        fences
            .closed()
            .map(|(resource, stop)| stop_error(stop, Some(resource)))
    }

    fn unknowable(&self, tx: TxId) -> Error {
        let too_new = repair_journal::read(self.paths, &self.paths.journal_repair(&tx))
            .is_ok_and(|parsed| parsed.prepared_read() == PreparedRead::TooNew);
        let (code, reason) = if too_new {
            (ErrorCode::StateTooNew, StopReason::JournalTooNew)
        } else {
            (ErrorCode::StateCorrupt, StopReason::JournalUnreadable)
        };
        stop_error(&fence::repair_stop(tx, code, Some(reason), true), None)
    }

    fn retarget(&self, clients: &[Client], to: &str) -> Result<()> {
        let scopes = self.scopes(clients)?;
        let mut session = WriteSession::untracked(Writer::Doctor, &scopes, COMMAND);
        let target = Path::new("runs").join(to);
        fsx::mutate(
            &mut session,
            &self.paths.doctor_latest(),
            Op::SymlinkAtomic { target: &target },
        )?;
        Ok(())
    }

    fn delete(
        &self,
        clients: &[Client],
        remove_latest: bool,
        victims: &[PathBuf],
        guard: GlobalLockGuard,
    ) -> Result<usize> {
        let scopes = self.scopes(clients)?;
        let latest = remove_latest.then(|| self.paths.doctor_latest());
        let victims: Vec<PathBuf> = latest.into_iter().chain(victims.iter().cloned()).collect();
        let report = fsx::prune(
            &scopes,
            PruneReason::DoctorGc,
            &victims,
            LockUse::HandOff(guard),
        )?;
        Ok(report.removed.len())
    }

    fn ring(&self) {
        crate::state::doorbell::ring(self.paths, COMMAND, self.actor);
    }
}

/// The error a stuck repair journal's stop is reported as: its code, its summary, the
/// `doctor explain` of its failure mode, and its `details` (`reason`, `tx`, `target`,
/// and the `resource` a closed fence set names).
fn stop_error(stop: &FenceStop, resource: Option<&str>) -> Error {
    let mut details = serde_json::Map::new();
    if let Some(reason) = stop.reason {
        details.insert("reason".into(), reason.into());
    }
    details.insert("target".into(), serde_json::json!(stop.target));
    if let Some(tx) = stop.tx {
        details.insert("tx".into(), serde_json::json!(tx));
    }
    if let Some(resource) = resource {
        details.insert("resource".into(), resource.into());
    }
    Error::new(
        stop.code,
        format!(
            "{}; doctor gc deletes nothing until it is settled",
            stop.summary
        ),
        stop.fix.clone(),
    )
    .details(serde_json::Value::Object(details))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use jiff::SignedDuration;
    use ulid::Ulid;

    use super::*;
    use crate::doctor::runs::RunSummary;
    use crate::doctor::runtime::DoctorRunId;
    use crate::fsx::FileId;
    use crate::output::RunKind;

    const NOW: &str = "2026-11-30T00:00:00Z";

    fn now() -> Timestamp {
        NOW.parse().unwrap()
    }

    /// A finished run `days` old: its id and its group.
    fn run(days: i64, n: u128) -> (DoctorRunId, Group) {
        let at = now()
            .checked_sub(SignedDuration::from_hours(24 * days))
            .unwrap();
        let ms = u64::try_from(at.as_millisecond()).unwrap();
        let id = DoctorRunId::new(at, Ulid::from_parts(ms, n));
        let path = PathBuf::from(format!("/s/doctor/runs/{}", id.as_str()));
        let group = Group {
            anchor: Anchor::Run(id.clone()),
            started_at: Some(at),
            txs: Vec::new(),
            tx: None,
            live: false,
            report: Some(RunSummary {
                kind: Some(RunKind::Diagnose),
                state: "DONE_OK".to_owned(),
                tx: None,
                undo_tx: None,
            }),
            candidates: vec![Candidate {
                kind: DeletionKind::Run,
                canonical: path.clone(),
                path,
                identity: FileId {
                    dev: 1,
                    ino: u64::try_from(n).unwrap(),
                    born_ns: None,
                },
                bytes: Some(1),
                kept_bytes: false,
                tx: None,
                target: None,
                staged: false,
            }],
        };
        (id, group)
    }

    fn view(groups: Vec<Group>) -> GcView {
        GcView {
            now: now(),
            before: now()
                .checked_sub(SignedDuration::from_hours(24 * 30))
                .unwrap(),
            groups,
            journals: Vec::new(),
            repairs: Vec::new(),
            unreadable_history: Vec::new(),
            latest: LatestState::Missing,
            unknowable_journal: None,
            listings_complete: true,
        }
    }

    /// A world that hands out `views` in turn and records every call.
    struct Recording {
        views: RefCell<VecDeque<GcView>>,
        calls: RefCell<Vec<String>>,
        fenced: Option<Error>,
    }

    impl Recording {
        fn new(views: Vec<GcView>) -> Self {
            Self {
                views: RefCell::new(views.into()),
                calls: RefCell::new(Vec::new()),
                fenced: None,
            }
        }

        fn note(&self, call: impl Into<String>) {
            self.calls.borrow_mut().push(call.into());
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl GcWorld for Recording {
        type Guard = ();

        fn clients(&self) -> Result<Vec<Client>> {
            self.note("clients");
            Ok(Vec::new())
        }

        fn view(&self, _: &[Client], _: Timestamp) -> Result<GcView> {
            self.note("view");
            self.views
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| Error::internal("no view left"))
        }

        fn planned(&self) -> Result<()> {
            self.note("planned");
            Ok(())
        }

        fn lock(&self) -> Result<()> {
            self.note("lock");
            Ok(())
        }

        fn fenced(&self, _: &[Client]) -> Option<Error> {
            self.note("fenced");
            self.fenced.clone()
        }

        fn unknowable(&self, tx: TxId) -> Error {
            self.note(format!("unknowable {tx}"));
            Error::new(ErrorCode::StateCorrupt, "unreadable", Suggestion::text("x"))
        }

        fn retarget(&self, _: &[Client], to: &str) -> Result<()> {
            self.note(format!("retarget {to}"));
            Ok(())
        }

        fn delete(
            &self,
            _: &[Client],
            remove_latest: bool,
            victims: &[PathBuf],
            (): (),
        ) -> Result<usize> {
            self.note(format!("delete latest={remove_latest} {}", victims.len()));
            Ok(victims.len() + usize::from(remove_latest))
        }

        fn ring(&self) {
            self.note("ring");
        }
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    fn request<'a>(argv: &'a [String], dry_run: bool, if_match: Option<&'a str>) -> GcRequest<'a> {
        GcRequest {
            before: now()
                .checked_sub(SignedDuration::from_hours(24 * 30))
                .unwrap(),
            dry_run,
            if_match,
            argv,
        }
    }

    fn token(view: &GcView) -> String {
        token_of(&plan(view, DOCTOR_GC_MAX_DELETIONS)).token()
    }

    #[test]
    fn a_dry_run_plans_once_and_takes_no_lock() {
        let one = view(vec![run(40, 1).1]);
        let world = Recording::new(vec![one.clone()]);
        let line = argv(&["omaddons", "doctor", "gc", "--before", "30d", "--dry-run"]);
        let report = gc(&world, &request(&line, true, None)).unwrap();
        assert_eq!(world.calls(), ["clients", "view", "planned"]);
        assert!(report.dry_run);
        assert_eq!(report.precondition.as_deref(), Some(token(&one).as_str()));
        assert_eq!(report.deletions.len(), 1);
    }

    #[test]
    fn a_bound_dry_run_compares_its_token() {
        let one = view(vec![run(40, 1).1]);
        let two = view(vec![run(40, 1).1, run(41, 2).1]);
        let line = argv(&["omaddons", "doctor", "gc", "--before", "30d", "--dry-run"]);
        let stale = token(&one);
        let world = Recording::new(vec![two]);
        let error = gc(&world, &request(&line, true, Some(&stale))).unwrap_err();
        assert_eq!(error.code, ErrorCode::PlanStale, "{error:?}");
        assert_eq!(
            error.details["changed"],
            serde_json::json!(["selection", "observed"])
        );
        assert_eq!(world.calls(), ["clients", "view", "planned"]);
    }

    #[test]
    fn the_real_run_plans_again_under_the_lock() {
        // The world changed between the preview's view and the lock: the fresh plan is
        // the one carried out.
        let one = view(vec![run(40, 1).1]);
        let two = view(vec![run(40, 1).1, run(41, 2).1]);
        let world = Recording::new(vec![one, two]);
        let line = argv(&["omaddons", "doctor", "gc", "--before", "30d", "--yes"]);
        let report = gc(&world, &request(&line, false, None)).unwrap();
        assert_eq!(
            world.calls(),
            [
                "clients",
                "view",
                "planned",
                "lock",
                "clients",
                "fenced",
                "view",
                "delete latest=false 2",
                "ring",
            ]
        );
        assert!(!report.dry_run);
        assert_eq!(report.precondition, None);
        assert_eq!(report.deletions.len(), 2);
    }

    #[test]
    fn the_bound_real_run_compares_the_fresh_plan() {
        let one = view(vec![run(40, 1).1]);
        let two = view(vec![run(40, 1).1, run(41, 2).1]);
        let line = argv(&["omaddons", "doctor", "gc", "--before", "30d", "--yes"]);
        let preview = token(&one);
        let world = Recording::new(vec![one.clone(), two.clone()]);
        let error = gc(&world, &request(&line, false, Some(&preview))).unwrap_err();
        assert_eq!(error.code, ErrorCode::PlanStale, "{error:?}");
        assert_eq!(
            error.fix.argv(),
            Some(
                argv(&[
                    "omaddons",
                    "doctor",
                    "gc",
                    "--before",
                    "30d",
                    "--yes",
                    "--dry-run"
                ])
                .as_slice()
            )
        );
        assert!(
            !world.calls().iter().any(|c| c.starts_with("delete")),
            "{:?}",
            world.calls()
        );
        // Bound to the plan the lock sees, it deletes.
        let fresh = token(&two);
        let world = Recording::new(vec![one, two]);
        let report = gc(&world, &request(&line, false, Some(&fresh))).unwrap();
        assert_eq!(report.deletions.len(), 2);
    }

    #[test]
    fn an_unknowable_journal_refuses_before_any_write() {
        let tx = TxId::from_ulid(Ulid::from_parts(1, 9));
        let mut locked = view(vec![run(40, 1).1]);
        locked.unknowable_journal = Some(tx);
        let world = Recording::new(vec![view(vec![run(40, 1).1]), locked]);
        let line = argv(&["omaddons", "doctor", "gc", "--before", "30d", "--yes"]);
        let error = gc(&world, &request(&line, false, None)).unwrap_err();
        assert_eq!(error.code, ErrorCode::StateCorrupt);
        assert_eq!(
            world.calls(),
            [
                "clients",
                "view",
                "planned",
                "lock",
                "clients",
                "fenced",
                "view",
                &format!("unknowable {tx}"),
            ]
        );
    }

    #[test]
    fn a_closed_fence_refuses_before_any_write() {
        let mut world = Recording::new(vec![view(vec![run(40, 1).1])]);
        world.fenced = Some(Error::new(
            ErrorCode::StateTooNew,
            "a repair journal a newer omaddons wrote",
            Suggestion::text("x"),
        ));
        let line = argv(&["omaddons", "doctor", "gc", "--before", "30d", "--yes"]);
        let error = gc(&world, &request(&line, false, None)).unwrap_err();
        assert_eq!(error.code, ErrorCode::StateTooNew);
        assert_eq!(
            world.calls(),
            ["clients", "view", "planned", "lock", "clients", "fenced"]
        );
    }

    #[test]
    fn latest_moves_before_anything_is_deleted() {
        let (old, old_group) = run(40, 1);
        let (young, young_group) = run(1, 2);
        let mut retarget = view(vec![old_group.clone(), young_group]);
        retarget.latest = LatestState::Link {
            names: Some(old.clone()),
            resolves: true,
        };
        let world = Recording::new(vec![retarget.clone(), retarget]);
        let line = argv(&["omaddons", "doctor", "gc", "--before", "30d", "--yes"]);
        gc(&world, &request(&line, false, None)).unwrap();
        let calls = world.calls();
        assert_eq!(
            calls[calls.len() - 3..],
            [
                format!("retarget {}", young.as_str()),
                "delete latest=false 1".to_owned(),
                "ring".to_owned(),
            ]
        );
        // With no finished run left, latest is removed with the victims.
        let mut removal = view(vec![old_group]);
        removal.latest = LatestState::Link {
            names: Some(old),
            resolves: true,
        };
        let world = Recording::new(vec![removal.clone(), removal]);
        gc(&world, &request(&line, false, None)).unwrap();
        assert_eq!(
            world.calls().last().map(String::as_str),
            Some("ring"),
            "{:?}",
            world.calls()
        );
        assert!(world.calls().contains(&"delete latest=true 1".to_owned()));
    }

    #[test]
    fn nothing_to_delete_takes_the_lock_and_deletes_nothing() {
        let world = Recording::new(vec![view(vec![run(1, 1).1]), view(vec![run(1, 1).1])]);
        let line = argv(&["omaddons", "doctor", "gc", "--before", "30d", "--yes"]);
        let report = gc(&world, &request(&line, false, None)).unwrap();
        assert!(report.deletions.is_empty());
        assert_eq!(
            world.calls(),
            [
                "clients", "view", "planned", "lock", "clients", "fenced", "view"
            ]
        );
    }

    /// gc never recovers a journal (M4-D45's lesson for `doctor fix`): recovering first
    /// would change what its token covers before the token is compared.
    #[test]
    fn no_recovery_runs() {
        use crate::source_scan::{Literals, production_code};
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for file in ["src/doctor/gc.rs", "src/app/doctor_gc.rs"] {
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            let code = production_code(&text, Literals::Blank).unwrap_or_default();
            for call in [
                "recover_all",
                "recover_under",
                "recover(",
                "recover_journals",
            ] {
                assert!(!code.contains(call), "{file} calls {call}");
            }
        }
    }
}
