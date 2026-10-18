//! The transaction engine wired to one command run: the network stack (with
//! `--offline`, the git hosts and the download cap from config), the built-in sources,
//! the rules, the running policy and the archive limits, attributed to the run's actor
//! and argv.

use super::world::World;
use crate::cli::Context;
use crate::engine::indexes::DepIndexes;
use crate::engine::inspect::ArchiveLimits;
use crate::engine::plan::DownloadMemo;
use crate::engine::retention::RetentionLimits;
use crate::engine::{EngineCtx, TxOutcomes, TxRequest, UndoOutcome, UndoRequest};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{Actor, Channel, Target};
use crate::net::{Http, NetConfig, NetLog, stack};
use crate::output::ProgressSink;
use crate::resolve::cache::{Freshness, ReleaseCache};
use crate::resolve::catalog::Catalog;
use crate::rules::RulesTable;
use crate::sources::SourceRegistry;
use crate::sources::git_hosts::GitRunner;
use crate::state::config::RunningPolicy;
use crate::state::paths::Paths;

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
    /// The command, recorded as the lock holder.
    command: &'static str,
    /// The command's progress, which its engine runs report into.
    progress: ProgressSink,
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
            registry: SourceRegistry::builtin(&world.sources()).with_git(GitRunner::new(&net, log)),
            table: world.table.clone(),
            policy: world.config.running_policy.unwrap_or_default(),
            limits: ArchiveLimits::from(&limits),
            retention: RetentionLimits::from(&limits),
            actor: ctx.actor,
            argv: ctx.argv.clone(),
            command,
            progress: ctx.progress.clone(),
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
        })
    }

    fn ctx<'a>(&'a self, cache: Option<&'a ReleaseCache<'a>>) -> EngineCtx<'a> {
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
        }
    }

    /// The command's release metadata over this engine's network and sources, one per
    /// command so that its engine runs fetch each ref once.
    pub fn cache(&self, freshness: Freshness) -> ReleaseCache<'_> {
        ReleaseCache::new(&self.paths, self.http.as_ref(), &self.registry, freshness)
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
        crate::engine::run(req, &self.ctx(cache))
    }

    /// Reverts the transaction `req` selects.
    pub fn undo(&self, req: &UndoRequest) -> Result<UndoOutcome> {
        crate::engine::undo(req, &self.ctx(None))
    }
}
