//! A home for planning at library level as a command plans (D1): a retail and a 3.3.5a
//! client, managed addons with their files index, a replay scenario, and `plan::plan`
//! with a persistent release cache whose facts are remembered after every plan, as
//! `install` remembers them, so a second plan can be served from the blob cache. Every
//! plan collects an approval witness per target. `tests/plan.rs`'s own `support::Env`
//! keeps its transient cache.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use omaddons::app::precondition::{PlanExtras, Precondition, of_plan};
use omaddons::engine::blobs::BlobCache;
use omaddons::engine::inspect::ArchiveLimits;
use omaddons::engine::plan::{DownloadMemo, Plan, PlanCtx, PlanRequest, TargetRequest, plan};
use omaddons::engine::witness::WitnessScope;
use omaddons::fsx::{InflightLock, ScopeTable, WriteSession, Writer};
use omaddons::model::{
    Actor, AddonReason, AddonRecord, AddonState, Channel, FolderName, Target, TargetKey, TxId,
    VersionConfidence,
};
use omaddons::net::NetLogEntry;
use omaddons::resolve::cache::{Freshness, ReleaseCache};
use omaddons::resolve::catalog::Catalog;
use omaddons::rules::RulesTable;
use omaddons::sources::{SourceConfigView, SourceRegistry};
use omaddons::state::config::RunningPolicy;
use omaddons::state::files_index::{FilesIndex, index_live_folder};
use omaddons::state::paths::Paths;
use omaddons::state::records::TargetRecord;
use omaddons::state::store;

use super::env::TestEnv;
use super::http::{paths, replay};
use super::netlog::read_net_log;
use super::targets::{battlenet, legacy_335};

/// One managed addon as [`PlanningHome::manage`] records it: its name, ref, the release
/// id's part after `@`, its version, and its folders with their files.
pub struct ManagedAddon<'a> {
    pub name: &'a str,
    pub r#ref: &'a str,
    pub release: &'a str,
    pub version: &'a str,
    pub folders: &'a [(&'a str, &'a [(&'a str, &'a str)])],
}

/// How [`PlanningHome::plan`] plans.
pub struct PlanOpts {
    pub force: bool,
    /// `install`'s policy by default.
    pub freshness: Freshness,
    pub registry: SourceRegistry,
    pub scope: WitnessScope,
    pub limits: ArchiveLimits,
}

impl Default for PlanOpts {
    fn default() -> Self {
        Self {
            force: false,
            freshness: Freshness::Revalidate,
            registry: SourceRegistry::builtin(&SourceConfigView::all_enabled()),
            scope: WitnessScope::default(),
            limits: ArchiveLimits::default(),
        }
    }
}

/// A retail and a 3.3.5a client, a replay scenario, and planning as a command plans.
pub struct PlanningHome {
    pub env: TestEnv,
    pub retail: Target,
    /// A 3.3.5a custom client.
    pub other: Target,
    pub paths: Paths,
    /// Where the replayed HTTP fixtures live.
    pub scenario: PathBuf,
    scopes: ScopeTable,
    running: BTreeSet<PathBuf>,
}

impl PlanningHome {
    pub fn new() -> Self {
        let env = TestEnv::new();
        let retail = battlenet(&env, "wow");
        let other = legacy_335(&env);
        let paths = paths(&env);
        let scopes = ScopeTable::new(
            &paths,
            &[retail.client_dir.clone(), other.client_dir.clone()],
            None,
        )
        .unwrap();
        let scenario = env.root().join("scenario");
        fs::create_dir_all(&scenario).unwrap();
        Self {
            env,
            retail,
            other,
            paths,
            scenario,
            scopes,
            running: BTreeSet::new(),
        }
    }

    /// Registers `target` (its marker, a record at generation 3 holding `addons`), writes
    /// the addons' live folders, and records them in a files index stamped well after
    /// every file's ctime, so their clean files are reused by a witness.
    pub fn manage(&self, target: &mut Target, addons: &[ManagedAddon<'_>]) {
        let key = TargetKey::new();
        target.key = Some(key);
        let marker = omaddons::discover::registry::marker_path(&target.client_dir);
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(
            &marker,
            format!("{{\"schema_version\":\"1.0\",\"key\":\"{key}\"}}\n"),
        )
        .unwrap();
        let mut record = TargetRecord::empty(key);
        record.generation = 3;
        let now = jiff::Timestamp::now().as_nanosecond();
        let mut index = FilesIndex {
            key,
            written_at_ns: now,
            folders: BTreeMap::new(),
        };
        for addon in addons {
            let mut folders = Vec::new();
            for (folder, files) in addon.folders {
                self.write_folder(target, folder, files);
                let dir = target.addons_dir().join(folder);
                let indexed = index_live_folder(&dir, &ArchiveLimits::default())
                    .unwrap()
                    .stamped(now + 10_000_000_000);
                let name = FolderName::new(folder).unwrap();
                index.folders.insert(name.key().to_owned(), indexed);
                folders.push(name);
            }
            let record_addon = AddonRecord {
                r#ref: addon.r#ref.parse().unwrap(),
                name: addon.name.into(),
                version: addon.version.into(),
                version_confidence: VersionConfidence::Release,
                release_id: Some(format!("{}@{}", addon.r#ref, addon.release)),
                channel: Channel::Stable,
                pinned: false,
                reason: AddonReason::Explicit,
                required_by: Vec::new(),
                folders,
                state: AddonState::Ok,
                installed_at: "2026-10-01T00:00:00Z".parse().unwrap(),
                tx_id: TxId::new(),
                blob_sha256: None,
                revision: None,
                left_out: Vec::new(),
                any_flavor: false,
            };
            record
                .addons
                .insert(TargetRecord::addon_key(addon.name), record_addon);
        }
        fs::create_dir_all(self.paths.targets_dir()).unwrap();
        let mut s = WriteSession::untracked(Writer::Mutating, &self.scopes, "test");
        store::save(&mut s, &self.paths.target_record(&key), &record, None).unwrap();
        store::save(&mut s, &self.paths.files_index(&key), &index, None).unwrap();
    }

    /// A hand-made (unmanaged) folder in `target`'s AddOns.
    pub fn hand_made(&self, target: &Target, folder: &str, files: &[(&str, &str)]) {
        self.write_folder(target, folder, files);
    }

    fn write_folder(&self, target: &Target, folder: &str, files: &[(&str, &str)]) {
        for (path, text) in files {
            let path = target.addons_dir().join(folder).join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    /// `plan::plan` as a command runs it: a dry run with a fresh planning transaction
    /// (and its in-flight lock), a persistent release cache whose facts are remembered
    /// afterwards, and an approval witness per target under `opts.scope`.
    pub fn plan(&self, targets: Vec<TargetRequest>, opts: &PlanOpts) -> Plan {
        let tx = TxId::new();
        let _inflight = InflightLock::create(&self.paths, &tx).unwrap();
        let http = replay(&self.env, &self.scenario);
        let blobs = BlobCache::new(&self.paths);
        let table = RulesTable::builtin();
        let argv: Vec<String> = ["omaddons", "install"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let cache = ReleaseCache::new(
            &self.paths,
            http.as_ref(),
            &opts.registry,
            opts.freshness,
            "install",
            Actor::Agent,
        );
        let catalog = Catalog::default();
        let downloads = DownloadMemo::default();
        let planned = {
            let ctx = PlanCtx {
                paths: &self.paths,
                scopes: &self.scopes,
                http: http.as_ref(),
                registry: &opts.registry,
                cache: &cache,
                progress: &omaddons::output::ProgressSink::none(),
                blobs: &blobs,
                table: &table,
                tx,
                running: &self.running,
                running_unknown: None,
                known_targets: &[],
                policy: RunningPolicy::Safe,
                limits: opts.limits,
                retention: omaddons::engine::retention::RetentionLimits::default(),
                actor: Actor::Agent,
                argv: &argv,
                catalog: &catalog,
                default_channel: Channel::Stable,
                downloads: &downloads,
                indexes: None,
            };
            let req = PlanRequest {
                targets,
                force: opts.force,
                while_running: false,
                dry_run: true,
                kind: None,
                no_deps: false,
                witness: Some(opts.scope.clone()),
            };
            plan(&req, &ctx).unwrap()
        };
        cache.remember(&planned.facts);
        planned
    }

    /// The token `command` would carry for `plan` (`precondition::of_plan`).
    pub fn token(
        &self,
        command: &'static str,
        plan: &Plan,
        extras: &PlanExtras<'_>,
    ) -> Precondition {
        of_plan(command, plan, &plan.witnesses, extras)
    }

    /// The home's `net.log` so far.
    pub fn net_log(&self) -> Vec<NetLogEntry> {
        read_net_log(&self.paths)
    }
}
