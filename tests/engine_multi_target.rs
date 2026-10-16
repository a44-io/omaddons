//! The multi-target commit path, in-process: one `engine::run` over three Battle.net
//! clients, each target committing in its own transaction (own tx id, lock window,
//! journal, snapshots and history line) under one run id. A failpoint armed for the
//! second commit only (`engine.lock.acquired=error@2`) shows a target-level failure that
//! leaves the other two committed, and each committed transaction undoes on its own.
//! Downloads are shared by the run's targets (M2-D28): one `asset` request per archive,
//! failures included, and a `Deferred` asset resolved right before its download.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::archives::zip_text;
use common::http::{fixture, fixture_bytes};
use common::netlog::read_net_log;
use common::trees::tree_hash;
use omaddons::app::outcome::{ItemStatus, TargetsData};
use omaddons::engine::{UndoRequest, UndoSelector};
use omaddons::model::{Actor, RunId};
use omaddons::sources::SourceRegistry;
use omaddons::state::history::{self, HistoryFilter};
use omaddons::state::paths::ClientPaths;
use support::{CANONICAL, DOWNLOAD, Deferring, EVERY, requests, scene, scene_with, texts};

/// The `asset` lines in `net.log`.
fn asset_requests(s: &support::Scene) -> usize {
    read_net_log(&s.paths)
        .iter()
        .filter(|e| e.purpose == "asset")
        .count()
}

#[cfg(test)]
mod support {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use omaddons::app::outcome::ItemRequest;
    use omaddons::discover::registry::reconcile;
    use omaddons::discover::{DiscoveredTarget, DiscoverySource};
    use omaddons::engine::inspect::ArchiveLimits;
    use omaddons::engine::plan::{PinIntent, RequestedChange};
    use omaddons::engine::retention::RetentionLimits;
    use omaddons::engine::{EngineCtx, TxOutcomes, TxRequest, TxTarget};
    use omaddons::error::Result;
    use omaddons::model::{Actor, AddonReason, Channel, RunId, TxKind};
    use omaddons::model::{
        Asset as ReleaseAsset, AssetFlavor, AssetLocation, HashFromHeader, Release, ReleaseSet,
    };
    use omaddons::net::Http;
    use omaddons::output::ProgressSink;
    use omaddons::resolve::cache::{Freshness, ReleaseCache};
    use omaddons::rules::RulesTable;
    use omaddons::sources::{ResolvedAsset, Source, SourceId, Wanted};
    use omaddons::sources::{SourceConfigView, SourceRegistry};
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
                sources: SourceRegistry::builtin(&SourceConfigView::all_enabled()),
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
                downloads: None,
                indexes: None,
            }
        }

        /// Installs `gh:o/foo` on every target as one run.
        pub fn install_everywhere(&self, run: RunId) -> TxOutcomes {
            omaddons::engine::run(&self.request("gh:o/foo", run), &self.ctx()).unwrap()
        }

        /// Installs `r` on every target as one run.
        pub fn request(&self, r: &str, run: RunId) -> TxRequest {
            TxRequest {
                targets: self
                    .targets()
                    .into_iter()
                    .map(|d| TxTarget {
                        discovered: d,
                        changes: vec![RequestedChange::Install {
                            r#ref: r.parse().unwrap(),
                            channel: Channel::Stable,
                            channel_explicit: false,
                            version: None,
                            file: None,
                            name: None,
                            pin: PinIntent::Keep,
                            reason: AddonReason::Explicit,
                            reinstall: false,
                            blob_sha256: None,
                            asset: None,
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
            }
        }

        /// Runs `req` fetching through `registry` and a release memo, which is written
        /// afterwards as commands write it.
        pub fn run_with(&self, req: &TxRequest, registry: &SourceRegistry) -> TxOutcomes {
            let cache = ReleaseCache::new(
                &self.paths,
                self.http.as_ref(),
                registry,
                Freshness::Revalidate,
            );
            let ctx = EngineCtx {
                registry,
                cache: Some(&cache),
                ..self.ctx()
            };
            let done = omaddons::engine::run(req, &ctx).unwrap();
            cache.remember(&done.plan.facts);
            done
        }
    }

    pub const EVERY: &str = "## Interface: 11509, 50504, 120105\n## Version: 1\n";

    /// Where [`Deferring`] resolves its asset: a token URL on an allowlisted host.
    pub const DOWNLOAD: &str = "https://api.tukui.org/v1/download/foo";
    /// What plans show for it instead.
    pub const CANONICAL: &str = "https://api.tukui.org/v1/addon/foo";

    /// A Tukui stand-in: `tukui:foo` has one release whose one asset (for every client)
    /// is `Deferred`. Each resolve hands out the next token, `<DOWNLOAD>/TOKEN-000<n>`,
    /// checked against the response's `ETag` MD5; `resolves` counts them.
    pub struct Deferring {
        pub resolves: Arc<AtomicUsize>,
    }

    impl Source for Deferring {
        fn id(&self) -> SourceId {
            SourceId::Tukui
        }

        fn fetch(&self, wanted: &[Wanted], _: &dyn Http) -> Vec<Result<ReleaseSet>> {
            let flavors = [
                ("mainline", 120_105),
                ("classic", 11_509),
                ("mists", 50_504),
            ];
            wanted
                .iter()
                .map(|w| {
                    Ok(ReleaseSet {
                        r#ref: w.r#ref.clone(),
                        releases: vec![Release {
                            release_id: format!("{}@1", w.r#ref),
                            version: "1".into(),
                            channel: Channel::Stable,
                            published_at: None,
                            assets: vec![ReleaseAsset {
                                filename: "foo-1.zip".into(),
                                location: AssetLocation::Deferred {
                                    source: SourceId::Tukui,
                                    key: "foo".into(),
                                },
                                size: None,
                                expected_hash: None,
                                flavors: flavors
                                    .iter()
                                    .map(|(flavor, interface)| AssetFlavor {
                                        flavor: (*flavor).into(),
                                        interface: Some(*interface),
                                    })
                                    .collect(),
                                name_flavors: Vec::new(),
                                nolib: false,
                            }],
                            changelog_url: None,
                            notes: None,
                            name_hint: None,
                        }],
                    })
                })
                .collect()
        }

        fn resolve_asset(&self, key: &str, _: &dyn Http) -> Result<ResolvedAsset> {
            assert_eq!(key, "foo");
            let n = self.resolves.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(ResolvedAsset {
                url: format!("{DOWNLOAD}/TOKEN-{n:04}"),
                expected_hash: None,
                hash_from_header: Some(HashFromHeader::EtagMd5),
                canonical_url: Some(CANONICAL.into()),
            })
        }
    }

    /// The three clients, with `gh:o/foo` v1 (one zip for every client) to install.
    pub fn scene() -> Scene {
        scene_with(|_| {})
    }

    /// Every file under `dir` but `net.log` (D4 redacts tokens there), as text.
    pub fn texts(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(listing) = std::fs::read_dir(dir) else {
            return;
        };
        for item in listing {
            let path = item.unwrap().path();
            if path.is_dir() {
                texts(&path, out);
            } else if !path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("net.log"))
            {
                let bytes = std::fs::read(&path).unwrap();
                out.push((
                    path.display().to_string(),
                    String::from_utf8_lossy(&bytes).into_owned(),
                ));
            }
        }
    }

    /// [`scene`] with more fixtures, written by `more` before replay loads them.
    pub fn scene_with(more: impl FnOnce(&Path)) -> Scene {
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
        more(&scenario);
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
    assert_eq!(asset_requests(&s), 1, "one download for three targets");
}

#[test]
fn a_failed_download_fails_every_target_once_and_a_new_run_retries() {
    let s = scene_with(|scenario| {
        fixture(
            scenario,
            "asset-o-foo-v1-Foo.zip",
            "https://github.com/o/foo/releases/download/v1/Foo.zip",
            404,
            &[],
            None,
        );
    });
    let done = s.install_everywhere(RunId::new());
    assert!(done.outcomes.iter().all(|o| o.tx.is_none()));
    let failures: Vec<(String, String, u8)> = done
        .failures
        .iter()
        .map(|f| {
            (
                f.target.clone(),
                f.error.code.wire().to_owned(),
                f.cause.as_ref().map_or(0, |c| c.exit().code()),
            )
        })
        .collect();
    assert_eq!(failures.len(), 3, "{failures:?}");
    assert!(
        failures
            .iter()
            .all(|(_, code, exit)| (code, exit) == (&failures[0].1, &failures[0].2)),
        "the same error and exit code on every target: {failures:?}"
    );
    assert_eq!(
        asset_requests(&s),
        1,
        "the failure is remembered for the run"
    );

    // A new run asks again.
    s.install_everywhere(RunId::new());
    assert_eq!(asset_requests(&s), 2);
}

#[test]
fn a_deferred_asset_is_resolved_right_before_its_one_download() {
    use md5::{Digest, Md5};

    let zip = zip_text(&[("Foo/Foo.toc", EVERY), ("Foo/core.lua", "-- foo")]);
    let md5: String = Md5::digest(&zip)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let etag = format!("\"{md5}\"");
    let s = scene_with(|scenario| {
        // The first token died before its download; the second serves the zip.
        let dead = format!("{DOWNLOAD}/TOKEN-0001");
        fixture(scenario, "token-1", &dead, 403, &[], None);
        let live = format!("{DOWNLOAD}/TOKEN-0002");
        fixture_bytes(
            scenario,
            "token-2",
            &live,
            200,
            &[("etag", &etag)],
            Some(&zip),
        );
    });
    let resolves = Arc::new(AtomicUsize::new(0));
    let mut registry = SourceRegistry::new();
    registry.register(Box::new(Deferring {
        resolves: Arc::clone(&resolves),
    }));

    let done = s.run_with(&s.request("tukui:foo", RunId::new()), &registry);
    assert!(done.failures.is_empty(), "{:?}", done.failures);
    assert_eq!(done.outcomes.iter().filter(|o| o.tx.is_some()).count(), 3);
    assert_eq!(
        resolves.load(Ordering::SeqCst),
        2,
        "once for the download, once more after its 403"
    );
    assert_eq!(
        asset_requests(&s),
        2,
        "the 403, then the zip, for three targets"
    );

    // Plans show the canonical URL; no token is stored or shown anywhere but net.log.
    let urls: Vec<Option<&str>> = done
        .plan
        .targets
        .iter()
        .flat_map(|t| &t.releases)
        .map(|r| r.url.as_deref())
        .collect();
    assert_eq!(urls, [Some(CANONICAL); 3]);
    let shown = format!(
        "{}{}",
        serde_json::to_string(&done.plan).unwrap(),
        serde_json::to_string(&done.outcomes).unwrap()
    );
    assert!(!shown.contains("TOKEN-"), "{shown}");
    let mut stored = Vec::new();
    for dir in [
        s.paths.state_dir(),
        s.paths.cache_dir(),
        s.paths.config_dir(),
    ] {
        texts(dir, &mut stored);
    }
    assert!(
        stored
            .iter()
            .any(|(path, _)| path.ends_with("releases.json")),
        "the memo was written: {:?}",
        stored.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    for (path, text) in &stored {
        assert!(!text.contains("TOKEN-"), "a token in {path}");
    }
}
