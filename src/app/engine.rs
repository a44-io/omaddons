//! The transaction engine wired to one command run: the network stack (with
//! `--offline`, the git hosts and the download cap from config), the built-in sources,
//! the rules, the running policy and the archive limits, attributed to the run's actor
//! and argv.

use super::world::World;
use crate::backup::auto::AutoPolicy;
use crate::cli::Context;
use crate::engine::indexes::DepIndexes;
use crate::engine::inspect::ArchiveLimits;
use crate::engine::plan::DownloadMemo;
use crate::engine::plan::RequestedChange;
use crate::engine::retention::RetentionLimits;
use crate::engine::{Binding, EngineCtx, TxOutcomes, TxRequest, UndoOutcome, UndoRequest};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::limits::EffectiveLimits;
use crate::model::{Actor, Channel, PlanId, RunId, Target};
use crate::net::{Http, NetConfig, NetLog, stack};
use crate::output::ProgressSink;
use crate::resolve::cache::{Freshness, ReleaseCache};
use crate::resolve::catalog::Catalog;
use crate::rules::RulesTable;
use crate::sources::SourceRegistry;
use crate::sources::git_hosts::GitRunner;
use crate::state::config::{AutoBackup, RunningPolicy};
use crate::state::paths::Paths;

/// The name a run gives the global lock's holder info and the doorbell: `apply` while it
/// executes a stored plan (as the envelope's `command` is), else the command's own.
fn engine_command(applying: bool, command: &'static str) -> &'static str {
    if applying { "apply" } else { command }
}

/// The engine for one command.
pub struct Engine {
    paths: Paths,
    http: Box<dyn Http>,
    registry: SourceRegistry,
    table: RulesTable,
    policy: RunningPolicy,
    limits: ArchiveLimits,
    retention: RetentionLimits,
    actor: Actor,
    argv: Vec<String>,
    /// The command, recorded as the lock holder and the doorbell's: `apply` while it
    /// executes a stored plan.
    command: &'static str,
    /// `apply`'s plan, which every history line the engine commits records.
    applying: Option<PlanId>,
    /// The command's progress, which its engine runs report into.
    progress: ProgressSink,
    /// The run's notes: each committed transaction's undo, should the run then panic.
    notes: crate::output::RunNotes,
    /// Where dependencies resolve.
    catalog: Catalog,
    /// The channel dependencies install from.
    default_channel: Channel,
    /// What the command downloaded, so its engine runs (`install`'s probe and real
    /// run, every target) download each archive once.
    downloads: DownloadMemo,
    /// The Tukui list and WoWInterface index dependencies resolve from after the
    /// catalog, each loaded at most once per command.
    indexes: DepIndexes,
    /// Every discovered target, for recovery's running check.
    known_targets: Vec<Target>,
    /// `auto_backup`, and the limits automatic backups keep.
    auto_mode: AutoBackup,
    backup_limits: EffectiveLimits,
}

impl Engine {
    /// `usage` when config.toml's limits are invalid.
    pub fn new(ctx: &Context, world: &World, command: &'static str) -> Result<Self> {
        let limits = world.config.effective_limits().map_err(|(key, why)| {
            Error::new(
                ErrorCode::Usage,
                format!("config.toml {key}: {why}"),
                Suggestion::command(["omaddons", "config", "get", &key]),
            )
        })?;
        let net = NetConfig {
            offline: ctx.offline,
            git_hosts: world.config.git_hosts.clone(),
            download_max_bytes: limits.download_max_bytes,
            ..NetConfig::default()
        };
        let log = NetLog::new(&world.paths)?;
        Ok(Self {
            http: stack(&net, log.clone(), &world.paths),
            paths: world.paths.clone(),
            registry: SourceRegistry::builtin(&world.sources().with_offline(ctx.offline))
                .with_git(GitRunner::new(&net, log, &world.paths)),
            table: world.table.clone(),
            policy: world.config.running_policy.unwrap_or_default(),
            limits: ArchiveLimits::from(&limits),
            retention: RetentionLimits::from(&limits),
            actor: ctx.actor,
            argv: ctx.argv.clone(),
            command: engine_command(ctx.applying.is_some(), command),
            applying: ctx.applying,
            progress: ctx.progress.clone(),
            notes: ctx.notes.clone(),
            // A broken override is the command's own catalog warning, if it loads one.
            catalog: Catalog::load(&world.paths).0,
            default_channel: world.config.default_channel.unwrap_or_default(),
            downloads: DownloadMemo::default(),
            indexes: DepIndexes::default(),
            known_targets: world
                .resolved
                .targets
                .iter()
                .map(|d| d.target.clone())
                .collect(),
            auto_mode: world.config.auto_backup.unwrap_or_default(),
            backup_limits: limits,
        })
    }

    fn ctx<'a>(&'a self, cache: Option<&'a ReleaseCache<'a>>) -> EngineCtx<'a> {
        self.bound_ctx(cache, None)
    }

    /// The engine's context, its runs bound to a preview by `binding` (`--if-match`).
    fn bound_ctx<'a>(
        &'a self,
        cache: Option<&'a ReleaseCache<'a>>,
        binding: Option<&'a dyn Binding>,
    ) -> EngineCtx<'a> {
        EngineCtx {
            paths: &self.paths,
            http: self.http.as_ref(),
            registry: &self.registry,
            table: &self.table,
            policy: self.policy,
            limits: self.limits,
            retention: self.retention,
            actor: self.actor,
            argv: &self.argv,
            command: self.command,
            running: None,
            known_targets: &self.known_targets,
            cache,
            progress: &self.progress,
            catalog: Some(&self.catalog),
            default_channel: self.default_channel,
            downloads: Some(&self.downloads),
            indexes: Some(&self.indexes),
            notes: Some(&self.notes),
            binding,
            auto_backup: Some(AutoPolicy {
                mode: self.auto_mode,
                limits: &self.backup_limits,
            }),
            applying: self.applying,
        }
    }

    /// The command's release metadata over this engine's network and sources, one per
    /// command so that its engine runs fetch each ref once.
    pub fn cache(&self, freshness: Freshness) -> ReleaseCache<'_> {
        ReleaseCache::new(
            &self.paths,
            self.http.as_ref(),
            &self.registry,
            freshness,
            self.command,
            self.actor,
        )
        .with_progress(self.progress.clone())
    }

    /// The sources that are on.
    pub fn enabled_sources(&self) -> std::collections::BTreeSet<crate::sources::SourceId> {
        self.registry.enabled()
    }

    /// The network stack this command's requests go through.
    pub fn http(&self) -> &dyn Http {
        self.http.as_ref()
    }

    /// The sources, for what the release cache does not cover (changelogs).
    pub fn registry(&self) -> &SourceRegistry {
        &self.registry
    }

    /// The archive limits in force: inspection's, and a live folder's indexing.
    pub fn archive_limits(&self) -> ArchiveLimits {
        self.limits
    }

    /// The channel new installs (and dependencies) take.
    pub fn default_channel(&self) -> Channel {
        self.default_channel
    }

    /// Checks each job's release on its target as installing it would, without
    /// ownership, the running policy, staging or the lock (`adopt`), fetching through
    /// `cache` and downloading once per command.
    pub fn check_releases(
        &self,
        jobs: &[crate::engine::tx::CheckJob],
        cache: &ReleaseCache<'_>,
    ) -> Result<crate::engine::tx::CheckedReleases> {
        crate::engine::tx::check_releases(jobs, &self.ctx(Some(cache)))
    }

    /// Plans and commits `req`, fetching through `cache` (`None`: no memo).
    pub fn run(&self, req: &TxRequest, cache: Option<&ReleaseCache<'_>>) -> Result<TxOutcomes> {
        self.run_bound(req, cache, None)
    }

    /// [`Engine::run`], bound to a preview (`--if-match`) when `binding` is given. The
    /// clients whose code it may change are read around it (the errors ledger's light
    /// read, design 7.1.8), outside the commit window.
    pub fn run_bound(
        &self,
        req: &TxRequest,
        cache: Option<&ReleaseCache<'_>>,
        binding: Option<&dyn Binding>,
    ) -> Result<TxOutcomes> {
        let captured = self.light_capture(&read_around(req));
        let result = crate::engine::run(req, &self.bound_ctx(cache, binding));
        self.light_commit(captured, req.run);
        result
    }

    /// Reverts the transaction `req` selects.
    pub fn undo(&self, req: &UndoRequest) -> Result<UndoOutcome> {
        self.undo_bound(req, None)
    }

    /// The undo of the repair-family line `read.entries[at]`, and the report its run
    /// recorded once its run directory existed (`doctor undo`, §6.11).
    pub fn undo_repair_reported(
        &self,
        req: &UndoRequest,
        read: &crate::state::history::HistoryRead,
        at: usize,
    ) -> (Result<UndoOutcome>, crate::engine::repair::undo::Recorded) {
        crate::engine::repair::undo::undo_repair_reported(req, &self.ctx(None), read, at)
    }

    /// [`Engine::undo`], bound to a preview (`--if-match`) when `binding` is given. Every
    /// client of the request is read around it (which one the undo acts on is decided
    /// inside the engine), unless it is a dry run.
    pub fn undo_bound(
        &self,
        req: &UndoRequest,
        binding: Option<&dyn Binding>,
    ) -> Result<UndoOutcome> {
        let targets: Vec<&Target> = if req.dry_run {
            Vec::new()
        } else {
            req.targets.iter().map(|d| &d.target).collect()
        };
        let captured = self.light_capture(&targets);
        let result = crate::engine::undo(req, &self.bound_ctx(None, binding));
        self.light_commit(captured, req.run);
        result
    }

    /// Whether each client of `targets` runs now, by the detection the engine's running
    /// policy uses, in one scan: `None` for every one when that cannot be told.
    fn running_clients(&self, targets: &[Target]) -> impl Fn(&Target) -> Option<bool> + use<> {
        let dirs = crate::engine::tx::running_dirs(&self.ctx(None), targets);
        move |target: &Target| dirs.as_ref().map(|dirs| dirs.contains(&target.client_dir))
    }

    /// The errors ledger's light read before a change to `targets` (design 7.1.8): it
    /// looks at their BugGrabber files and whether they run, takes no lock and never
    /// fails. Nothing to look at for no target.
    pub fn light_capture(&self, targets: &[&Target]) -> super::errors_ledger::Captured {
        if targets.is_empty() {
            return super::errors_ledger::Captured::default();
        }
        let owned: Vec<Target> = targets.iter().map(|t| (*t).clone()).collect();
        let running = self.running_clients(&owned);
        super::errors_ledger::capture(&self.paths, targets, &running)
    }

    /// The light read after the change `run` made, whether it succeeded or not: one
    /// short session under the evidence lock, never the transaction lock. It never fails.
    pub fn light_commit(&self, captured: super::errors_ledger::Captured, run: RunId) {
        let targets = captured.targets();
        if targets.is_empty() {
            return;
        }
        let running = self.running_clients(&targets);
        super::errors_ledger::commit(
            &self.paths,
            captured,
            run,
            &running,
            self.command,
            self.actor,
        );
    }

    /// A whole restore's refusals before anything under the client dir is written: a
    /// newer state, and a client that runs or may.
    pub fn refuse_restore(&self, target: &Target, files: u64) -> Result<()> {
        crate::engine::restore::refuse_early(&self.ctx(None), target, files)
    }

    /// Restores what the command planned and began, bound to a preview (`--if-match`)
    /// when `binding` is given.
    pub fn restore(
        &self,
        req: crate::engine::restore::RestoreRequest<'_>,
        binding: Option<&dyn Binding>,
    ) -> Result<crate::engine::restore::RestoreOutcome> {
        crate::engine::restore::restore(&self.bound_ctx(None, binding), req)
    }

    /// Restores single addons' SavedVariables as the command planned and began, bound to
    /// a preview (`--if-match`) when `binding` is given.
    pub fn restore_addons(
        &self,
        req: crate::engine::restore_addons::AddonsRequest<'_>,
        binding: Option<&dyn Binding>,
    ) -> Result<crate::engine::restore_addons::AddonsOutcome> {
        crate::engine::restore_addons::restore(&self.bound_ctx(None, binding), req)
    }

    /// For a change command that ends with nothing to send to the engine: a journal a
    /// crash left is recovered as a commit would have, into the run's warnings. A dry
    /// run never recovers.
    pub fn recover_waiting(&self, ctx: &Context) {
        if !ctx.dry_run {
            ctx.notes
                .warn_all(&crate::engine::tx::recover_waiting(&self.ctx(None)));
        }
    }
}

/// The clients a request's light read looks at: each whose requested changes may move
/// code, that is, any change but a record edit (a plain pin or unpin, an adoption, a
/// plain skip), whatever the command. None for a dry run, which also serves the
/// commands' internal probes.
fn read_around(req: &TxRequest) -> Vec<&Target> {
    if req.dry_run {
        return Vec::new();
    }
    req.targets
        .iter()
        .filter(|t| {
            t.changes
                .iter()
                .any(|change| !matches!(change, RequestedChange::Edit { .. }))
        })
        .map(|t| &t.discovered.target)
        .collect()
}

/// [`Engine::recover_waiting`] for a command that has built no engine: one is built only
/// when a journal waits.
pub fn recover_waiting(ctx: &Context, world: &World, command: &'static str) {
    if ctx.dry_run || !crate::engine::journal::recoverable(&world.paths).unwrap_or(false) {
        return;
    }
    if let Ok(engine) = Engine::new(ctx, world, command) {
        engine.recover_waiting(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_read_around_unless_a_dry_run_or_made_only_of_record_edits() {
        use crate::discover::target::DiscoveredTarget;
        use crate::engine::TxTarget;
        use crate::engine::edit::RecordEdit;
        use crate::model::TxKind;
        let pin = || RequestedChange::Edit {
            addon: "Foo".into(),
            edit: RecordEdit::SetPinned(true),
        };
        let remove = || RequestedChange::Remove {
            addon: "Foo".into(),
        };
        let client = |id: &str, changes: Vec<RequestedChange>| {
            let mut target = crate::engine::testing::target(&std::path::Path::new("/c").join(id));
            target.id = id.into();
            TxTarget {
                discovered: DiscoveredTarget {
                    target,
                    default_id: id.into(),
                    qualifier: "battlenet".into(),
                    source: crate::discover::DiscoverySource::Scan,
                    not_launched_since: None,
                },
                changes,
                default_kind: TxKind::Install,
                dependency_locks: std::collections::BTreeMap::new(),
            }
        };
        // (case, dry run, the clients and their changes, the clients read around)
        let cases: Vec<(&str, bool, Vec<TxTarget>, Vec<&str>)> = vec![
            ("no client", false, vec![], vec![]),
            ("a pin alone", false, vec![client("a", vec![pin()])], vec![]),
            (
                "a removal",
                false,
                vec![client("a", vec![remove()])],
                vec!["a"],
            ),
            (
                "a removal beside a pin on one client",
                false,
                vec![client("a", vec![pin(), remove()])],
                vec!["a"],
            ),
            (
                "only the client whose code may change",
                false,
                vec![client("a", vec![pin()]), client("b", vec![remove()])],
                vec!["b"],
            ),
            (
                "a dry run, an internal probe's too",
                true,
                vec![client("a", vec![remove()])],
                vec![],
            ),
        ];
        for (case, dry_run, targets, want) in cases {
            let req = TxRequest {
                targets,
                force: false,
                while_running: false,
                dry_run,
                reason: None,
                kind: None,
                run: RunId::new(),
                no_deps: false,
                witness: None,
            };
            let got: Vec<&str> = read_around(&req).iter().map(|t| t.id.as_str()).collect();
            assert_eq!(got, want, "{case}");
        }
    }

    #[test]
    fn the_engine_names_apply_while_applying() {
        for (name, applying, command, want) in [
            ("applying", true, "install", "apply"),
            ("applying_undo", true, "undo", "apply"),
            ("direct", false, "install", "install"),
            ("direct_restore", false, "backup restore", "backup restore"),
        ] {
            assert_eq!(
                engine_command(applying, command),
                want,
                "{name}: engine_command({applying}, {command:?})"
            );
        }
    }
}
