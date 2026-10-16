//! The multi-target commit path, in-process: one `engine::run` over three Battle.net
//! clients, each target committing in its own transaction (own tx id, lock window,
//! journal, snapshots and history line) under one run id. A failpoint armed for the
//! second commit only (`engine.lock.acquired=error@2`) shows a target-level failure that
//! leaves the other two committed, and each committed transaction undoes on its own.

mod common;

use std::collections::BTreeSet;
use std::fs;

use common::trees::tree_hash;
use omaddons::app::outcome::{ItemStatus, TargetsData};
use omaddons::engine::{UndoRequest, UndoSelector};
use omaddons::model::{Actor, RunId};
use omaddons::state::history::{self, HistoryFilter};
use omaddons::state::paths::ClientPaths;
use support::{requests, scene};

#[cfg(test)]
mod support {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use omaddons::app::outcome::ItemRequest;
    use omaddons::discover::registry::reconcile;
    use omaddons::discover::{DiscoveredTarget, DiscoverySource};
    use omaddons::engine::inspect::ArchiveLimits;
    use omaddons::engine::plan::{PinIntent, RequestedChange};
    use omaddons::engine::retention::RetentionLimits;
    use omaddons::engine::{EngineCtx, TxOutcomes, TxRequest, TxTarget};
    use omaddons::model::{Actor, AddonReason, Channel, RunId, TxKind};
    use omaddons::net::Http;
    use omaddons::output::ProgressSink;
    use omaddons::rules::RulesTable;
    use omaddons::sources::SourceRegistry;
    use omaddons::state::config::RunningPolicy;
    use omaddons::state::paths::Paths;
    use omaddons::state::registry::load_registry;

    use super::common::env::TestEnv;
    use super::common::http::{Asset, gh_release, replay};
    use super::common::targets::battlenet;

    /// Three Battle.net clients (retail, classic, era) in their own roots.
    pub struct Scene {
        /// Keeps the tempdir alive.
        _env: TestEnv,
        pub paths: Paths,
        pub http: Box<dyn Http>,
        pub table: RulesTable,
        pub sources: SourceRegistry,
        pub argv: Vec<String>,
        nothing_runs: BTreeSet<PathBuf>,
        progress: ProgressSink,
        discovered: Vec<DiscoveredTarget>,
    }

    impl Scene {
        pub fn new(env: TestEnv, http: Box<dyn Http>) -> Self {
            let discovered = ["wow", "wow_classic", "wow_classic_era"]
                .iter()
                .map(|code| {
                    let target = battlenet(&env, code);
                    DiscoveredTarget {
                        default_id: target.id.clone(),
                        target,
                        qualifier: "battlenet".into(),
                        source: DiscoverySource::Scan,
                    }
                })
                .collect();
            Self {
                paths: super::common::http::paths(&env),
                _env: env,
                http,
                table: RulesTable::builtin(),
                sources: SourceRegistry::builtin(),
                argv: vec!["omaddons".into(), "update".into()],
                nothing_runs: BTreeSet::new(),
                progress: ProgressSink::none(),
                discovered,
            }
        }

        /// The targets as the registry knows them now (keys once registered).
        pub fn targets(&self) -> Vec<DiscoveredTarget> {
            let registry = load_registry(&self.paths).unwrap().value;
            reconcile(&registry, &self.discovered).targets
        }

        pub fn ctx(&self) -> EngineCtx<'_> {
            EngineCtx {
                paths: &self.paths,
                http: self.http.as_ref(),
                registry: &self.sources,
                table: &self.table,
                policy: RunningPolicy::default(),
                limits: ArchiveLimits::default(),
                retention: RetentionLimits::default(),
                actor: Actor::Agent,
                argv: &self.argv,
                command: "update",
                running: Some(&self.nothing_runs),
                cache: None,
                progress: &self.progress,
                catalog: None,
                default_channel: Channel::Stable,
            }
        }

        /// Installs `gh:o/foo` on every target as one run.
        pub fn install_everywhere(&self, run: RunId) -> TxOutcomes {
            let req = TxRequest {
                targets: self
                    .targets()
                    .into_iter()
                    .map(|d| TxTarget {
                        discovered: d,
                        changes: vec![RequestedChange::Install {
                            r#ref: "gh:o/foo".parse().unwrap(),
                            channel: Channel::Stable,
                            channel_explicit: false,
                            version: None,
                            file: None,
                            name: None,
                            pin: PinIntent::Keep,
                            reason: AddonReason::Explicit,
                            reinstall: false,
                        }],
                        default_kind: TxKind::Install,
                        dependency_locks: std::collections::BTreeMap::new(),
                    })
                    .collect(),
                force: false,
                while_running: false,
                dry_run: false,
                reason: None,
                kind: None,
                run,
                no_deps: false,
            };
            omaddons::engine::run(&req, &self.ctx()).unwrap()
        }
    }

    const EVERY: &str = "## Interface: 11509, 50504, 120105\n## Version: 1\n";

    /// The three clients, with `gh:o/foo` v1 (one zip for every client) to install.
    pub fn scene() -> Scene {
        let env = TestEnv::new();
        let scenario = env.root().join("http");
        let assets: [Asset<'_>; 1] = [(
            "Foo.zip",
            &[
                ("mainline", 120_105),
                ("classic", 11_509),
                ("mists", 50_504),
            ],
            vec![("Foo/Foo.toc", EVERY), ("Foo/core.lua", "-- foo")],
        )];
        gh_release(&scenario, "https://github.com/o/foo", "v1", &assets);
        let http = replay(&env, &scenario);
        Scene::new(env, http)
    }

    /// `gh:o/foo` asked for on each of `targets`.
    pub fn requests(targets: &[DiscoveredTarget]) -> Vec<ItemRequest> {
        targets
            .iter()
            .map(|d| ItemRequest {
                request: "gh:o/foo".into(),
                target: d.target.id.clone(),
                addon: None,
                r#ref: Some("gh:o/foo".parse().unwrap()),
                version: None,
            })
            .collect()
    }
}

#[test]
fn a_failed_commit_on_one_target_leaves_the_others_committed_and_undoable() {
    let s = scene();
    let run = RunId::new();
    let done =
        omaddons::fsx::with_spec("engine.lock.acquired=error@2", || s.install_everywhere(run));
    let targets = s.targets();
    let data = TargetsData::build(&done, run, &requests(&targets));

    // Commits go in key order; none is registered yet, so by id: classic, era, retail.
    let ids: Vec<(&str, bool, bool)> = data
        .targets
        .iter()
        .map(|t| (t.target.id.as_str(), t.tx.is_some(), t.error.is_some()))
        .collect();
    assert_eq!(
        ids,
        [
            ("classic", true, false),
            ("era", false, true),
            ("retail", true, false)
        ],
        "{data:#?}"
    );
    let era = &data.targets[1];
    assert_eq!(era.items[0].status, ItemStatus::Failed);
    assert!(data.partial());
    assert_eq!(data.run, Some(run));
    let hints: Vec<(String, Vec<String>)> = data
        .undo_commands()
        .iter()
        .map(|c| (c.action().to_owned(), c.argv().to_vec()))
        .collect();
    assert_eq!(hints.len(), 3);
    assert_eq!(
        hints[0],
        (
            "undo_run".to_owned(),
            vec![
                "omaddons".into(),
                "undo".into(),
                "--run".into(),
                run.to_string()
            ]
        )
    );
    assert!(hints[1..].iter().all(|(action, _)| action == "undo"));

    // Each committed target: its own tx, history line (of this run), snapshots; no journal.
    let committed: Vec<_> = data.targets.iter().filter_map(|t| t.tx).collect();
    assert_ne!(committed[0], committed[1]);
    let lines = history::read(&s.paths, &HistoryFilter::default())
        .unwrap()
        .entries;
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|l| l.run == Some(run)));
    let txs: BTreeSet<_> = lines.iter().map(|l| l.tx).collect();
    assert_eq!(txs, committed.iter().copied().collect());
    let leftover: Vec<_> = fs::read_dir(s.paths.journal_dir())
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(leftover.is_empty(), "journals left: {leftover:?}");
    let client = |id: &str| {
        targets
            .iter()
            .find(|d| d.target.id == id)
            .unwrap()
            .target
            .client_dir
            .clone()
    };
    for (id, tx) in [("classic", committed[0]), ("retail", committed[1])] {
        assert!(
            s.paths.state_snapshots_dir().join(tx.to_string()).is_dir(),
            "{id}"
        );
        assert!(ClientPaths::new(client(id)).snapshots(&tx).is_dir(), "{id}");
    }
    let addons = |id: &str| ClientPaths::new(client(id)).addons();
    assert!(!addons("era").join("Foo").exists(), "era never committed");

    // Undoing classic's transaction leaves retail byte-identical.
    let retail_before = tree_hash(&addons("retail"));
    let undone = omaddons::engine::undo(
        &UndoRequest {
            selector: UndoSelector::Tx(committed[0]),
            sv_policy: omaddons::engine::SvPolicy::Auto,
            dry_run: false,
            while_running: false,
            reason: None,
            targets: targets.clone(),
            run: RunId::new(),
        },
        &s.ctx(),
    )
    .unwrap();
    assert!(undone.outcome.tx.is_some());
    assert!(!addons("classic").join("Foo").exists());
    assert_eq!(tree_hash(&addons("retail")), retail_before);
    assert_eq!(
        history::read(&s.paths, &HistoryFilter::default())
            .unwrap()
            .entries
            .last()
            .map(|l| l.actor),
        Some(Actor::Agent)
    );
}

#[test]
fn without_a_failpoint_every_target_commits_in_one_run() {
    let s = scene();
    let run = RunId::new();
    let done = s.install_everywhere(run);
    let data = TargetsData::build(&done, run, &requests(&s.targets()));
    assert!(!data.partial());
    assert_eq!(data.summary.changed, 3);
    assert!(data.targets.iter().all(|t| t.tx.is_some()));
    let lines = history::read(
        &s.paths,
        &HistoryFilter {
            run: Some(run),
            ..HistoryFilter::default()
        },
    )
    .unwrap()
    .entries;
    assert_eq!(lines.len(), 3);
    assert_eq!(data.undo_commands().len(), 4);
}
