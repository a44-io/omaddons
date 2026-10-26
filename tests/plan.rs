//! e2e suite: planning over replayed releases, and equal preview tokens across dry runs and sources (D1).
//!
//! Planning end to end over replayed GitHub releases whose assets are synthetic zips:
//! fetch once, select and verify per target, fall back, refuse early, write nothing
//! outside the cache. Then the preview token (`precondition::of_plan`) over real plans:
//! equal for two dry runs of an unchanged home, for every source when the second is
//! served from the blob cache, and for an unchanged `--file <dir>`.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use common::archives::zip_text;
use common::env::TestEnv;
use common::http::{Asset, gh_feed, gh_release, paths, replay, scenario};
use common::netlog::{assert_each_url_once, read_net_log};
use common::planning::{ManagedAddon, PlanOpts, PlanningHome};
use common::targets::battlenet;
use common::tlog::{TestLog, tlog};
use common::trees::tree_hash;
use omaddons::app::precondition::{PlanExtras, Precondition, check};
use omaddons::engine::blobs::BlobCache;
use omaddons::engine::edit::RecordEdit;
use omaddons::engine::inspect::ArchiveLimits;
use omaddons::engine::plan::{
    ChangeKind, OpKind, PinIntent, Plan, PlanCtx, PlanRequest, RequestedChange, TargetRequest, plan,
};
use omaddons::fsx::{InflightLock, ScopeTable, WriteSession, Writer};
use omaddons::model::{
    Actor, AddonReason, AddonRecord, AddonState, Channel, FolderName, Target, TargetKey, TxId,
    TxKind, VersionConfidence,
};
use omaddons::net::Http;
use omaddons::resolve::cache::ReleaseCache;
use omaddons::rules::RulesTable;
use omaddons::sources::{SourceConfigView, SourceRegistry};
use omaddons::state::config::RunningPolicy;
use omaddons::state::paths::Paths;
use omaddons::state::records::TargetRecord;
use omaddons::state::store;
use support::{
    Env, chosen, folders, foo_v1_with_v2_out, install, release, request, toc, update_foo,
    user_install,
};

const BASE: &str = "https://github.com/o/foo";

#[cfg(test)]
mod support {
    use super::*;

    /// A retail and a Forever target, a replay scenario, and planning's context.
    pub struct Env {
        pub env: TestEnv,
        pub retail: Target,
        pub forever: Target,
        pub paths: Paths,
        pub scopes: ScopeTable,
        pub scenario: PathBuf,
        pub tx: TxId,
        _inflight: InflightLock,
        pub running: BTreeSet<PathBuf>,
    }

    impl Env {
        pub fn new() -> Self {
            let env = TestEnv::new();
            let retail = battlenet(&env, "wow");
            let forever = battlenet(&env, "wowf");
            let paths = paths(&env);
            let scopes = ScopeTable::new(
                &paths,
                &[retail.client_dir.clone(), forever.client_dir.clone()],
                None,
            )
            .unwrap();
            let scenario = env.root().join("scenario");
            fs::create_dir_all(&scenario).unwrap();
            let tx = TxId::new();
            let inflight = InflightLock::create(&paths, &tx).unwrap();
            Self {
                env,
                retail,
                forever,
                paths,
                scopes,
                scenario,
                tx,
                _inflight: inflight,
                running: BTreeSet::new(),
            }
        }

        pub fn http(&self) -> Box<dyn Http> {
            replay(&self.env, &self.scenario)
        }

        pub fn plan(&self, targets: Vec<TargetRequest>, force: bool) -> Plan {
            self.plan_with(
                targets,
                force,
                &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
            )
        }

        /// [`Env::plan`] fetching through `registry`.
        pub fn plan_with(
            &self,
            targets: Vec<TargetRequest>,
            force: bool,
            registry: &SourceRegistry,
        ) -> Plan {
            let http = self.http();
            let blobs = BlobCache::new(&self.paths);
            let table = RulesTable::builtin();
            let argv: Vec<String> = ["omaddons", "install", "gh:o/foo"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            let cache = ReleaseCache::transient(http.as_ref(), registry);
            let catalog = omaddons::resolve::catalog::Catalog::default();
            let downloads = omaddons::engine::plan::DownloadMemo::default();
            let ctx = PlanCtx {
                paths: &self.paths,
                scopes: &self.scopes,
                http: http.as_ref(),
                registry,
                cache: &cache,
                progress: &omaddons::output::ProgressSink::none(),
                blobs: &blobs,
                table: &table,
                tx: self.tx,
                running: &self.running,
                running_unknown: None,
                known_targets: &[],
                policy: RunningPolicy::Safe,
                limits: ArchiveLimits::default(),
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
                force,
                while_running: false,
                dry_run: true,
                kind: None,
                no_deps: false,
                witness: None,
            };
            plan(&req, &ctx).unwrap()
        }

        /// Registers `target` with a record holding `addons` at generation 3.
        pub fn register(&self, target: &mut Target, addons: &[(&str, &str, &[&str])]) {
            self.register_with(target, addons, |_| {});
        }

        /// [`Env::register`], with `edit` applied to each addon's record.
        pub fn register_with(
            &self,
            target: &mut Target,
            addons: &[(&str, &str, &[&str])],
            edit: impl Fn(&mut AddonRecord),
        ) {
            let key = TargetKey::new();
            target.key = Some(key);
            let mut record = TargetRecord::empty(key);
            record.generation = 3;
            for (name, release, folders) in addons {
                let mut addon = AddonRecord {
                    r#ref: "gh:o/foo".parse().unwrap(),
                    name: (*name).into(),
                    version: release.trim_start_matches('v').into(),
                    version_confidence: VersionConfidence::Release,
                    release_id: Some(format!("gh:o/foo@{release}")),
                    channel: Channel::Stable,
                    pinned: false,
                    reason: AddonReason::Explicit,
                    required_by: Vec::new(),
                    folders: folders
                        .iter()
                        .map(|f| FolderName::new(f).unwrap())
                        .collect(),
                    state: AddonState::Ok,
                    installed_at: "2026-10-01T00:00:00Z".parse().unwrap(),
                    tx_id: TxId::new(),
                    blob_sha256: None,
                    revision: None,
                    left_out: Vec::new(),
                    any_flavor: false,
                };
                edit(&mut addon);
                record.addons.insert(TargetRecord::addon_key(name), addon);
            }
            fs::create_dir_all(self.paths.targets_dir()).unwrap();
            let mut s = WriteSession::untracked(Writer::Mutating, &self.scopes, "test");
            store::save(&mut s, &self.paths.target_record(&key), &record, None).unwrap();
        }
    }

    /// Serves release `tag` of gh:o/foo with `assets`.
    pub fn release(scenario: &Path, tag: &str, assets: &[Asset<'_>]) {
        gh_release(scenario, BASE, tag, assets);
    }

    /// Install gh:o/foo into `target`.
    pub fn install(target: &Target) -> TargetRequest {
        request(target, vec![user_install("gh:o/foo")], TxKind::Install)
    }

    /// `changes` on `target` for a command of `default_kind`.
    pub fn request(
        target: &Target,
        changes: Vec<RequestedChange>,
        default_kind: TxKind,
    ) -> TargetRequest {
        TargetRequest {
            target: target.clone(),
            changes,
            default_kind,
            dependency_locks: std::collections::BTreeMap::new(),
        }
    }

    /// A user's plain install of `r`: stable, no version, the pin kept.
    pub fn user_install(r: &str) -> RequestedChange {
        RequestedChange::Install {
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
            revision: None,
            any_flavor: omaddons::engine::plan::AnyFlavorIntent::Keep,
        }
    }

    /// Update the installed Foo on `target`.
    pub fn update_foo(target: &Target) -> TargetRequest {
        let update = RequestedChange::Update {
            addon: "foo".into(),
            installed_primary_interface: None,
        };
        request(target, vec![update], TxKind::Update)
    }

    /// Foo installed at v1 on retail (its folder on disk), its record edited by `edit`,
    /// and v2 the newest release.
    pub fn foo_v1_with_v2_out(edit: impl Fn(&mut AddonRecord)) -> (Env, Target) {
        let env = Env::new();
        let retail_toc = toc("120105");
        let mut retail = env.retail.clone();
        let addons = retail.addons_dir();
        fs::create_dir_all(addons.join("Foo")).unwrap();
        fs::write(addons.join("Foo/Foo.toc"), &retail_toc).unwrap();
        env.register_with(&mut retail, &[("Foo", "v1", &["Foo"])], edit);
        release(
            &env.scenario,
            "v2",
            &[(
                "Foo.zip",
                &[("mainline", 120_105)],
                vec![("Foo/Foo.toc", &retail_toc)],
            )],
        );
        (env, retail)
    }

    pub fn toc(interface: &str) -> String {
        format!("## Interface: {interface}\n")
    }

    /// The blob the target's first addon installs from.
    pub fn chosen(plan: &Plan, target: usize) -> String {
        plan.targets[target].work[0]
            .blob()
            .map(|b| b.display().to_string())
            .unwrap_or_default()
    }

    /// The TOC files of the target's first addon.
    pub fn folders(plan: &Plan, target: usize) -> Vec<String> {
        let inspected = plan.targets[target].work[0].inspected().unwrap();
        inspected
            .folders
            .iter()
            .flat_map(|f| f.toc_files())
            .collect()
    }
}

#[test]
fn one_ref_two_targets_is_fetched_once_and_selected_per_target() {
    let env = Env::new();
    let (mainline, forever) = (toc("120105"), toc("16001"));
    release(
        &env.scenario,
        "v1",
        &[
            (
                "Foo-mainline.zip",
                &[("mainline", 120_105)],
                vec![("Foo/Foo_Mainline.toc", &mainline), ("Foo/core.lua", "")],
            ),
            (
                "Foo-forever.zip",
                &[("forever", 16_001)],
                vec![("Foo/Foo.toc", &forever), ("Foo/core.lua", "")],
            ),
        ],
    );
    let plan = env.plan(vec![install(&env.retail), install(&env.forever)], false);
    assert!(plan.failures.is_empty(), "{:?}", plan.failures);
    assert_eq!(
        folders(&plan, 0),
        ["Foo_Mainline.toc"],
        "retail took the mainline zip"
    );
    assert_eq!(folders(&plan, 1), ["Foo.toc"], "Forever took its own zip");
    assert_ne!(chosen(&plan, 0), chosen(&plan, 1));
    for t in &plan.targets {
        assert_eq!(t.kind, Some(omaddons::model::TxKind::Install));
        assert_eq!(t.changes[0].kind, ChangeKind::Install);
        assert_eq!(t.changes[0].addon, "Foo");
        assert_eq!(
            t.ops
                .iter()
                .map(|o| (o.op, o.folder.as_str()))
                .collect::<Vec<_>>(),
            [(OpKind::Add, "Foo")]
        );
    }
    assert_eq!(
        plan.generations.values().copied().collect::<Vec<_>>(),
        [0, 0],
        "unregistered targets plan at generation 0"
    );
    assert!(
        plan.generations.contains_key(&env.retail.id),
        "keyed by the provisional id"
    );
    let log = read_net_log(&env.paths);
    assert_each_url_once(&log);
    assert_eq!(
        log.iter()
            .filter(|e| e.path.ends_with("/releases/latest"))
            .count(),
        1,
        "fetched once for both targets"
    );
}

#[test]
fn a_wrong_flavor_candidate_falls_back_and_none_fails_listing_every_try() {
    let env = Env::new();
    let (classic, retail) = (toc("11509"), toc("120105"));
    release(
        &env.scenario,
        "v1",
        &[
            (
                "Foo-tagged.zip",
                &[("mainline", 120_105)],
                vec![("Foo/Foo_Vanilla.toc", &classic)],
            ),
            ("Foo-plain.zip", &[], vec![("Foo/Foo.toc", &retail)]),
        ],
    );
    let plan = env.plan(vec![install(&env.retail)], false);
    assert!(plan.failures.is_empty(), "{:?}", plan.failures);
    assert!(
        chosen(&plan, 0).ends_with(&plan.targets[0].releases[0].blob_sha256.clone().unwrap()),
        "{}",
        chosen(&plan, 0)
    );
    assert_eq!(folders(&plan, 0), ["Foo.toc"], "fell back to the plain zip");

    let env = Env::new();
    release(
        &env.scenario,
        "v1",
        &[
            (
                "Foo-a.zip",
                &[("mainline", 120_105)],
                vec![("Foo/Foo_Vanilla.toc", &classic)],
            ),
            ("Foo-b.zip", &[], vec![("Foo/Foo_Mists.toc", &toc("50504"))]),
        ],
    );
    // Its only release: a look-back finds nothing older to read.
    gh_feed(&env.scenario, BASE, &[("v1", "")], None);
    let plan = env.plan(vec![install(&env.retail)], false);
    let failure = &plan.failures[0];
    let error = failure.cause.as_ref().unwrap();
    assert_eq!(
        (error.code.wire(), error.exit().code()),
        ("no_compatible_release", 3)
    );
    let tried: Vec<&str> = error.details["tried"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["asset"].as_str().unwrap())
        .collect();
    assert_eq!(tried, ["Foo-a.zip", "Foo-b.zip"]);
    assert!(
        error.details["tried"][0]["reason"]
            .as_str()
            .unwrap()
            .starts_with("wrong_flavor")
    );
    assert!(plan.targets[0].changes.is_empty());
}

#[test]
fn a_file_that_does_not_load_is_wrong_flavor() {
    let env = Env::new();
    let zip = env.env.root().join("classic.zip");
    fs::write(&zip, zip_text(&[("Foo/Foo_Vanilla.toc", &toc("11509"))])).unwrap();
    let mut request = install(&env.retail);
    if let RequestedChange::Install { file, .. } = &mut request.changes[0] {
        *file = Some(zip);
    }
    let plan = env.plan(vec![request], false);
    let error = plan.failures[0].cause.as_ref().unwrap();
    assert_eq!(
        (error.code.wire(), error.exit().code()),
        ("wrong_flavor", 4)
    );
    assert!(read_net_log(&env.paths).is_empty(), "--file never fetches");
}

#[test]
fn current_installs_are_no_ops_and_newer_releases_update() {
    let mut env = Env::new();
    let retail_toc = toc("120105");
    let addons = env.retail.addons_dir();
    for folder in ["Foo", "FooOld"] {
        fs::create_dir_all(addons.join(folder)).unwrap();
        fs::write(
            addons.join(folder).join(format!("{folder}.toc")),
            &retail_toc,
        )
        .unwrap();
    }
    let mut retail = env.retail.clone();
    env.register(&mut retail, &[("Foo", "v1", &["Foo", "FooOld"])]);
    env.retail = retail;
    release(
        &env.scenario,
        "v1",
        &[(
            "Foo.zip",
            &[("mainline", 120_105)],
            vec![
                ("Foo/Foo.toc", &retail_toc),
                ("FooOld/FooOld.toc", &retail_toc),
            ],
        )],
    );
    let plan = env.plan(vec![install(&env.retail)], false);
    assert!(plan.failures.is_empty(), "{:?}", plan.failures);
    assert!(
        plan.targets[0].changes.is_empty() && plan.targets[0].kind.is_none(),
        "already current"
    );
    assert!(!plan.has_ops());
    assert_eq!(plan.generations[&env.retail.key.unwrap().to_string()], 3);
    assert!(
        read_net_log(&env.paths)
            .iter()
            .all(|e| !e.path.ends_with(".zip")),
        "no download for a no-op"
    );

    let env2 = Env::new();
    let mut retail = env2.retail.clone();
    let addons = retail.addons_dir();
    for folder in ["Foo", "FooOld"] {
        fs::create_dir_all(addons.join(folder)).unwrap();
        fs::write(
            addons.join(folder).join(format!("{folder}.toc")),
            &retail_toc,
        )
        .unwrap();
    }
    env2.register(&mut retail, &[("Foo", "v1", &["Foo", "FooOld"])]);
    release(
        &env2.scenario,
        "v2",
        &[(
            "Foo.zip",
            &[("mainline", 120_105)],
            vec![("Foo/Foo.toc", &retail_toc)],
        )],
    );
    let plan = env2.plan(vec![install(&retail)], false);
    assert!(plan.failures.is_empty(), "{:?}", plan.failures);
    let t = &plan.targets[0];
    assert_eq!(t.kind, Some(omaddons::model::TxKind::Update));
    assert_eq!(
        (
            t.changes[0].kind,
            t.changes[0].from_version.as_deref(),
            t.changes[0].to_version.as_deref()
        ),
        (ChangeKind::Update, Some("1"), Some("v2"))
    );
    let ops: Vec<(OpKind, &str)> = t.ops.iter().map(|o| (o.op, o.folder.as_str())).collect();
    assert_eq!(ops, [(OpKind::Replace, "Foo"), (OpKind::Remove, "FooOld")]);
}

#[test]
fn refusals_surface_at_plan_time_and_nothing_is_written_outside_the_cache() {
    let mut env = Env::new();
    let retail_toc = toc("120105");
    release(
        &env.scenario,
        "v1",
        &[(
            "Foo.zip",
            &[("mainline", 120_105)],
            vec![
                ("Foo/Foo.toc", &retail_toc),
                ("Blizzard_Foo/Blizzard_Foo.toc", &retail_toc),
            ],
        )],
    );
    let addons = env.retail.addons_dir();
    let client = || tree_hash(addons.parent().unwrap());
    let before = (client(), tree_hash(&env.env.config()));
    let plan = env.plan(vec![install(&env.retail)], true);
    assert_eq!(
        plan.failures[0].cause.as_ref().unwrap().code.wire(),
        "protected_folder"
    );
    assert_eq!(
        (client(), tree_hash(&env.env.config())),
        before,
        "the client and config are untouched (downloads go to the cache, requests to net.log)"
    );

    fs::create_dir_all(addons.join("Kept")).unwrap();
    fs::write(addons.join("Kept/Kept.toc"), &retail_toc).unwrap();
    let mut retail = env.retail.clone();
    env.register(&mut retail, &[("Kept", "v1", &["Kept"])]);
    env.running.insert(retail.client_dir.clone());
    let removes = vec![
        RequestedChange::Remove {
            addon: "kept".into(),
        },
        RequestedChange::Remove {
            addon: "Nope".into(),
        },
    ];
    let plan = env.plan(vec![request(&retail, removes, TxKind::Remove)], false);
    let codes: Vec<&str> = plan
        .failures
        .iter()
        .map(|f| f.cause.as_ref().unwrap().code.wire())
        .collect();
    assert_eq!(codes, ["game_running", "not_managed"]);
    assert!(addons.join("Kept").is_dir());
    let stage = addons.parent().unwrap().join(".omaddons");
    assert!(!stage.exists(), "nothing staged");
}

#[test]
fn one_failing_ref_does_not_stop_the_others() {
    let env = Env::new();
    release(
        &env.scenario,
        "v1",
        &[(
            "Foo.zip",
            &[("mainline", 120_105)],
            vec![("Foo/Foo.toc", &toc("120105"))],
        )],
    );
    let mut request = install(&env.retail);
    request.changes.push(user_install("cf:61284"));
    let plan = env.plan(vec![request], false);
    assert_eq!(plan.failures.len(), 1);
    assert_eq!(plan.failures[0].error.code.wire(), "source_disabled");
    assert_eq!(
        plan.failures[0]
            .r#ref
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("cf:61284")
    );
    assert_eq!(plan.targets[0].changes.len(), 1, "gh:o/foo still planned");
}

#[test]
fn the_dry_run_shape() {
    let env = Env::new();
    release(
        &env.scenario,
        "v1",
        &[(
            "Foo.zip",
            &[("mainline", 120_105)],
            vec![("Foo/Foo.toc", &toc("120105")), ("Foo/core.lua", "x")],
        )],
    );
    let mut plan = env.plan(vec![install(&env.retail)], false);
    let value = serde_json::to_value(&plan).unwrap();
    assert_eq!(
        (
            value["dry_run"].as_bool(),
            value["plan_id"].is_null(),
            value["expires_at"].is_null()
        ),
        (Some(true), true, true)
    );
    // The plan itself, so the snapshot keeps the wire's field order.
    plan.created_at = "2026-10-14T00:00:00Z".parse().unwrap();
    insta::assert_json_snapshot!("plan_dry_run", plan);
}

#[test]
fn local_and_url_sources_fetch_without_requests() {
    let env = Env::new();
    let zip = env.env.root().join("Addon.zip");
    fs::write(&zip, zip_text(&[("Addon/Addon.toc", &toc("120105"))])).unwrap();
    let dir = env.env.root().join("MyAddon");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("MyAddon.toc"),
        "## Interface: 120105\n## Version: 2.5.1\n",
    )
    .unwrap();
    let wanted: Vec<omaddons::sources::Wanted> = [
        format!("file:{}", zip.display()),
        format!("file:{}", dir.display()),
        "url:https://github.com/o/r/releases/download/v1/X.zip".to_owned(),
    ]
    .iter()
    .map(|r| omaddons::sources::Wanted::stable(r.parse().unwrap()))
    .collect();
    let http = env.http();
    let sets = omaddons::sources::fetch_once(
        wanted,
        &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
        http.as_ref(),
    );
    assert_eq!(sets.len(), 3);
    assert!(sets.values().all(Result::is_ok), "{sets:?}");
    assert!(read_net_log(&env.paths).is_empty(), "no request");

    let plan = env.plan(
        vec![request(
            &env.retail,
            vec![user_install(&format!("file:{}", dir.display()))],
            TxKind::Install,
        )],
        false,
    );
    assert!(plan.failures.is_empty(), "{:?}", plan.failures);
    let record = plan.targets[0].work[0].record().unwrap();
    assert_eq!(
        (
            record.version.as_str(),
            record.version_confidence,
            record.name.as_str()
        ),
        ("2.5.1", VersionConfidence::Toc, "MyAddon"),
        "the TOC gives a local addon its version"
    );
}

#[test]
fn an_update_keeps_a_dependency_and_its_requirers() {
    let (env, retail) = foo_v1_with_v2_out(|r| {
        r.reason = AddonReason::Dependency;
        r.required_by = vec!["Zed".into(), "Bar".into(), "Zed".into()];
    });
    let plan = env.plan(vec![update_foo(&retail)], false);
    assert!(plan.failures.is_empty(), "{:?}", plan.failures);
    let t = &plan.targets[0];
    assert_eq!(
        (t.changes[0].kind, t.changes[0].to_version.as_deref()),
        (ChangeKind::Update, Some("v2"))
    );
    let record = t.work[0].record().unwrap();
    assert_eq!(
        (record.reason, record.pinned, record.channel),
        (AddonReason::Dependency, false, Channel::Stable),
        "an update passes the recorded reason, so a dependency stays one"
    );
    assert_eq!(
        record.required_by,
        ["Bar", "Zed"],
        "sorted and de-duplicated"
    );

    // An explicit install of the same dependency promotes it.
    let (env, retail) = foo_v1_with_v2_out(|r| r.reason = AddonReason::Dependency);
    let plan = env.plan(vec![install(&retail)], false);
    let record = plan.targets[0].work[0].record().unwrap();
    assert_eq!(record.reason, AddonReason::Explicit);
}

#[test]
fn an_update_of_a_pinned_addon_is_a_no_op_without_a_fetch() {
    let (env, retail) = foo_v1_with_v2_out(|r| r.pinned = true);
    for request in [update_foo(&retail), install(&retail)] {
        let plan = env.plan(vec![request], false);
        assert!(plan.failures.is_empty(), "{:?}", plan.failures);
        let t = &plan.targets[0];
        assert!(t.work.is_empty() && t.changes.is_empty() && !plan.has_ops());
        let codes: Vec<&str> = t.warnings.iter().map(|w| w.code.wire()).collect();
        assert_eq!(codes, ["addon_pinned"]);
        assert_eq!(
            t.unchanged_warnings, t.warnings,
            "kept for the commit window"
        );
    }
    assert!(
        read_net_log(&env.paths).is_empty(),
        "a pinned addon's releases are not fetched"
    );

    // --version moves it, still pinned.
    let mut versioned = install(&retail);
    if let RequestedChange::Install { version, pin, .. } = &mut versioned.changes[0] {
        *version = Some("v2".into());
        *pin = PinIntent::Pin;
    }
    let plan = env.plan(vec![versioned], false);
    assert!(plan.failures.is_empty(), "{:?}", plan.failures);
    let record = plan.targets[0].work[0].record().unwrap();
    assert_eq!((record.version.as_str(), record.pinned), ("v2", true));
}

#[test]
fn a_record_only_difference_plans_a_records_only_edit() {
    let (env, retail) = foo_v1_with_v2_out(|r| {
        r.reason = AddonReason::Dependency;
        r.release_id = Some("gh:o/foo@v2".into());
        r.version = "v2".into();
    });
    let plan = env.plan(vec![install(&retail)], false);
    assert!(plan.failures.is_empty(), "{:?}", plan.failures);
    let t = &plan.targets[0];
    assert!(t.notes.is_empty() && t.warnings.is_empty(), "{t:?}");
    assert_eq!(
        t.kind,
        Some(TxKind::Install),
        "a promotion keeps the command's kind"
    );
    let value = serde_json::to_value(&t.changes[0]).unwrap();
    assert_eq!(
        (
            &value["kind"],
            &value["fields"],
            &value["folders"]["recorded"]
        ),
        (
            &serde_json::json!("edit"),
            &serde_json::json!(["reason"]),
            &serde_json::json!(["Foo"])
        )
    );
    let record = t.work[0].record().unwrap();
    assert_eq!(
        (record.reason, record.release_id.as_deref()),
        (AddonReason::Explicit, Some("gh:o/foo@v2")),
        "promoted at the same release"
    );
    assert!(
        read_net_log(&env.paths)
            .iter()
            .all(|e| !e.path.ends_with(".zip")),
        "nothing downloaded"
    );

    // The same request with nothing to change has nothing to say.
    let (env, retail) = foo_v1_with_v2_out(|r| {
        r.release_id = Some("gh:o/foo@v2".into());
        r.version = "v2".into();
    });
    let plan = env.plan(vec![install(&retail)], false);
    assert!(plan.targets[0].notes.is_empty() && plan.targets[0].work.is_empty());

    // A folder missing: a reinstall case, not an edit; unchanged with a -v note.
    let (env, retail) = foo_v1_with_v2_out(|r| {
        r.reason = AddonReason::Dependency;
        r.release_id = Some("gh:o/foo@v2".into());
        r.folders.push(FolderName::new("FooGone").unwrap());
    });
    let mut pin = install(&retail);
    if let RequestedChange::Install { pin, .. } = &mut pin.changes[0] {
        *pin = PinIntent::Pin;
    }
    let plan = env.plan(vec![pin], false);
    let t = &plan.targets[0];
    assert!(t.work.is_empty(), "{:?}", t.changes);
    assert!(t.notes[0].contains("folders are missing"), "{:?}", t.notes);
}

#[test]
fn a_records_only_edit_plans_as_an_edit_with_its_fields_and_recorded_folders() {
    let (env, retail) = foo_v1_with_v2_out(|_| {});
    let edit = |edit| RequestedChange::Edit {
        addon: "foo".into(),
        edit,
    };
    let pin = edit(RecordEdit::SetPinned(true));
    let plan = env.plan(vec![request(&retail, vec![pin], TxKind::Install)], false);
    assert!(plan.failures.is_empty(), "{:?}", plan.failures);
    let value = serde_json::to_value(&plan).unwrap();
    let target = &value["targets"][0];
    assert_eq!(target["kind"], "pin", "a pinned-only edit");
    assert_eq!(
        target["changes"][0],
        serde_json::json!({
            "addon": "Foo",
            "ref": "gh:o/foo",
            "kind": "edit",
            "fields": ["pinned"],
            "from_version": "1",
            "to_version": "1",
            "folders": {"added": [], "replaced": [], "removed": [], "skipped": [], "recorded": ["Foo"]},
        })
    );
    assert_eq!(target["ops"], serde_json::json!([]), "no folder moves");
    assert!(
        read_net_log(&env.paths).is_empty(),
        "an edit needs no network"
    );

    // Other fields: the command's kind.
    let channel = edit(RecordEdit::SetChannel(Channel::Beta));
    let plan = env.plan(vec![request(&retail, vec![channel], TxKind::Update)], false);
    let value = serde_json::to_value(&plan).unwrap();
    assert_eq!(value["targets"][0]["kind"], "update");
    assert_eq!(
        value["targets"][0]["changes"][0]["fields"],
        serde_json::json!(["channel"])
    );
}

/// Where the stand-in source's one asset is served.
const WOWI_ZIP: &str = "https://cdn.wowinterface.com/downloads/file5995/Foo.zip";

/// Where a second asset of the stand-in source is served.
const WOWI_OLD_ZIP: &str = "https://cdn.wowinterface.com/downloads/file5995/Foo-classic.zip";

/// A WoWInterface stand-in: release 1 of any `wowi:` ref is `Foo-classic.zip` at
/// [`WOWI_OLD_ZIP`] when `classic` is set, then `Foo.zip` at [`WOWI_ZIP`], published
/// with `hash`.
struct HashSource {
    hash: Option<omaddons::model::ExpectedHash>,
    classic: bool,
}

impl HashSource {
    fn asset(
        filename: &str,
        url: &str,
        hash: Option<omaddons::model::ExpectedHash>,
    ) -> omaddons::model::Asset {
        omaddons::model::Asset {
            filename: filename.to_owned(),
            location: omaddons::model::AssetLocation::Url(url.to_owned()),
            size: None,
            expected_hash: hash,
            flavors: Vec::new(),
            name_flavors: Vec::new(),
            nolib: false,
        }
    }
}

impl omaddons::sources::Source for HashSource {
    fn id(&self) -> omaddons::sources::SourceId {
        omaddons::sources::SourceId::Wowi
    }

    fn fetch(
        &self,
        wanted: &[omaddons::sources::Wanted],
        _: &dyn Http,
    ) -> Vec<omaddons::error::Result<omaddons::model::ReleaseSet>> {
        wanted
            .iter()
            .map(|w| {
                Ok(omaddons::model::ReleaseSet {
                    r#ref: w.r#ref.clone(),
                    releases: vec![omaddons::model::Release {
                        release_id: format!("{}@1", w.r#ref),
                        version: "1".to_owned(),
                        channel: Channel::Stable,
                        published_at: None,
                        assets: self
                            .classic
                            .then(|| Self::asset("Foo-classic.zip", WOWI_OLD_ZIP, None))
                            .into_iter()
                            .chain([Self::asset("Foo.zip", WOWI_ZIP, self.hash.clone())])
                            .collect(),
                        changelog_url: None,
                        notes: None,
                        name_hint: None,
                        revision: None,
                    }],
                })
            })
            .collect()
    }
}

#[test]
fn a_published_hash_of_either_algorithm_is_checked_and_shown_in_the_plan() {
    use md5::{Digest, Md5};
    use omaddons::model::ExpectedHash;

    let body = zip_text(&[("Foo/Foo.toc", &toc("120105"))]);
    let hex = |bytes: &[u8]| -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() };
    let md5 = hex(&Md5::digest(&body));
    let sha = hex(&sha2::Sha256::digest(&body));
    let classic_zip = zip_text(&[("Foo/Foo.toc", &toc("11509"))]);
    let plan_of = |hash: Option<ExpectedHash>, classic: bool| {
        let env = Env::new();
        common::http::fixture_bytes(&env.scenario, "wowi-foo", WOWI_ZIP, 200, &[], Some(&body));
        common::http::fixture_bytes(
            &env.scenario,
            "wowi-foo-classic",
            WOWI_OLD_ZIP,
            200,
            &[],
            Some(&classic_zip),
        );
        let mut registry = SourceRegistry::new();
        registry.register(Box::new(HashSource { hash, classic }));
        let changes = vec![user_install("wowi:5995")];
        let plan = env.plan_with(
            vec![request(&env.retail, changes, TxKind::Install)],
            false,
            &registry,
        );
        (env, plan)
    };
    let plan_with = |hash: Option<ExpectedHash>| plan_of(hash, false);

    for (hash, wire) in [
        (ExpectedHash::md5(&md5), serde_json::json!({"md5": md5})),
        (
            ExpectedHash::sha256(&sha),
            serde_json::json!({"sha256": sha}),
        ),
    ] {
        let (_env, plan) = plan_with(hash);
        assert!(plan.failures.is_empty(), "{:?}", plan.failures);
        let value = serde_json::to_value(&plan).unwrap();
        assert_eq!(value["targets"][0]["releases"][0]["expected_hash"], wire);
        assert_eq!(value["targets"][0]["changes"][0]["kind"], "install");
    }

    // A tampered download (the source published another MD5): `hash_mismatch` after its
    // retry, no longer retryable, with what was tried; nothing is kept.
    let tampered = || ExpectedHash::md5(&hex(&Md5::digest(b"other")));
    let (env, plan) = plan_with(tampered());
    assert_eq!(plan.failures.len(), 1, "{plan:?}");
    let error = &plan.failures[0].error;
    assert_eq!(
        (error.code, error.retryable),
        (omaddons::error::ErrorCode::HashMismatch, false)
    );
    assert!(error.message.contains("MD5"), "{}", error.message);
    let reason = error.details["tried"][0]["reason"].as_str().unwrap();
    assert!(reason.starts_with("hash_mismatch:"), "{reason}");
    assert!(error.details["expected"].is_string(), "{:?}", error.details);
    assert!(plan.targets[0].changes.is_empty());
    assert!(!env.paths.blob(&sha).exists());
    assert_eq!(
        read_net_log(&env.paths).len(),
        2,
        "downloaded twice: the mismatch is retried once"
    );

    // One candidate judged (it does not load here), one tampered: a question of what
    // loads, as before.
    let (_env, plan) = plan_of(tampered(), true);
    assert_eq!(plan.failures.len(), 1, "{plan:?}");
    assert_eq!(
        plan.failures[0].error.code,
        omaddons::error::ErrorCode::NoCompatibleRelease
    );
}

// ---------------------------------------------------------------------------------------
// Equal preview tokens (D1, checkpoint criterion 6).

/// Each witness's public parts and cost, for the test log.
fn witnesses(plan: &Plan) -> serde_json::Value {
    serde_json::json!(
        plan.witnesses
            .iter()
            .map(|w| {
                let cost = w.cost();
                serde_json::json!({
                    "target": w.target.to_string(),
                    "marker": w.marker,
                    "folders": w.folders.iter().map(|f| serde_json::json!({
                        "role": f.role.wire(), "key": f.key, "digest": f.digest.to_string(),
                    })).collect::<Vec<_>>(),
                    "absent": w.absent,
                    "inputs": w.inputs.iter().map(|i| serde_json::json!({
                        "kind": i.kind.wire(), "source": i.source, "digest": i.digest,
                    })).collect::<Vec<_>>(),
                    "dependencies": w.dependencies.iter().map(|d| serde_json::json!({
                        "requester": d.requester, "folder": d.folder,
                        "by": d.by.wire(), "detail": d.by.detail(),
                    })).collect::<Vec<_>>(),
                    "cost": {
                        "files_hashed": cost.files_hashed,
                        "files_reused": cost.files_reused,
                        "bytes_hashed": cost.bytes_hashed,
                    },
                })
            })
            .collect::<Vec<_>>()
    )
}

/// A dry run of `targets` on `home`, its token and witnesses logged, with the `net.log`
/// lines it added.
fn planned(
    log: &TestLog,
    home: &PlanningHome,
    targets: Vec<omaddons::engine::plan::TargetRequest>,
    opts: &PlanOpts,
) -> (Plan, Precondition, Vec<omaddons::net::NetLogEntry>) {
    let before = home.net_log().len();
    let plan = home.plan(targets, opts);
    let added: Vec<omaddons::net::NetLogEntry> = home.net_log().split_off(before);
    let token = home.token("install", &plan, &PlanExtras::default());
    log.json(
        "net.log added",
        &added
            .iter()
            .map(|e| {
                format!(
                    "{} {}{} {} {:?}",
                    e.method, e.host, e.path, e.purpose, e.status
                )
            })
            .collect::<Vec<_>>(),
    );
    log.json("parts", &token.parts());
    log.json("witness", &witnesses(&plan));
    assert!(plan.failures.is_empty(), "{:?}", plan.failures);
    (plan, token, added)
}

/// `b`'s token equals `a`'s; on a failure, both tokens and their parts.
fn same_token(what: &str, a: &Precondition, b: &Precondition) {
    assert!(
        // ubs:ignore — a preview token is no credential (src/app/precondition.rs)
        a.token() == b.token(),
        "{what}: the tokens differ\n{}\n{:#?}\n{}\n{:#?}",
        a.token(),
        a.parts(),
        b.token(),
        b.parts()
    );
}

#[test]
fn two_dry_runs_of_an_unchanged_home_give_equal_tokens() {
    let log = tlog!();
    let home = PlanningHome::new();
    log.env(&home.env);
    log.step("seed home");
    let retail_toc = toc("120105");
    let mut retail = home.retail.clone();
    home.manage(
        &mut retail,
        &[ManagedAddon {
            name: "Foo",
            r#ref: "gh:o/foo",
            release: "v1",
            version: "1",
            folders: &[("Foo", &[("Foo.toc", &retail_toc), ("core.lua", "-- v1")])],
        }],
    );
    home.hand_made(
        &retail,
        "Bar",
        &[("Bar.toc", &retail_toc), ("bar.lua", "-- mine")],
    );
    release(
        &home.scenario,
        "v2",
        &[(
            "Foo.zip",
            &[("mainline", 120_105)],
            vec![("Foo/Foo.toc", &retail_toc), ("Foo/core.lua", "-- v2")],
        )],
    );
    gh_release(
        &home.scenario,
        "https://github.com/o/bar",
        "v1",
        &[(
            "Bar.zip",
            &[("mainline", 120_105)],
            vec![("Bar/Bar.toc", &retail_toc)],
        )],
    );
    // Planning writes `net.log`, the release memo and the blob cache, never the
    // records, the files index or the client.
    let records = home.paths.targets_dir();
    log.tree("records", &records);
    log.tree("client", &retail.client_dir);
    let opts = PlanOpts {
        force: true,
        ..PlanOpts::default()
    };
    let update = RequestedChange::Update {
        addon: "foo".into(),
        installed_primary_interface: None,
    };
    let mut tokens = Vec::new();
    for step in ["first plan", "second plan", "third plan"] {
        log.step(step);
        let changes = vec![update.clone(), user_install("gh:o/bar")];
        let (plan, token, _) = planned(
            &log,
            &home,
            vec![request(&retail, changes, TxKind::Install)],
            &opts,
        );
        let w = &plan.witnesses[0];
        let roles: Vec<(&str, &str)> = w
            .folders
            .iter()
            .map(|f| (f.key.as_str(), f.role.wire()))
            .collect();
        log.assert_eq(
            "replaced folders",
            &roles,
            &vec![("bar", "replaced"), ("foo", "replaced")],
        );
        let cost = w.cost();
        log.assert_eq(
            "Foo's clean files reused, the unmanaged Bar hashed",
            &(cost.files_reused > 0 && cost.files_hashed > 0),
            &true,
        );
        tokens.push(token);
    }
    log.step("compare");
    same_token("the second plan", &tokens[0], &tokens[1]);
    same_token("the third plan", &tokens[0], &tokens[2]);
    log.assert_unchanged("records", &records);
    log.assert_unchanged("client", &retail.client_dir);
}

/// Two dry runs of `r` on `target`: a download (`purpose`) on the first, none on the
/// second (the blob cache, through the facts the first remembered), and equal tokens.
fn a_cache_hit_equals_a_download(
    log: &TestLog,
    home: &PlanningHome,
    target: &Target,
    r: &str,
    purpose: &str,
) -> (Plan, Plan) {
    let mut plans = Vec::new();
    let mut tokens = Vec::new();
    for step in ["first plan", "second plan"] {
        log.step(step);
        let (plan, token, added) = planned(
            log,
            home,
            vec![request(target, vec![user_install(r)], TxKind::Install)],
            &PlanOpts::default(),
        );
        let downloads = added.iter().filter(|e| e.purpose == purpose).count();
        if plans.is_empty() {
            log.assert_eq("a download in the first plan", &(downloads > 0), &true);
        } else {
            log.assert_eq("downloads in the second plan", &downloads, &0);
        }
        plans.push(plan);
        tokens.push(token);
    }
    log.step("compare");
    same_token("a cache hit and a download", &tokens[0], &tokens[1]);
    let second = plans.remove(1);
    (plans.remove(0), second)
}

#[test]
fn a_cache_hit_equals_a_download_github() {
    let log = tlog!();
    let home = PlanningHome::new();
    log.env(&home.env);
    log.step("seed home");
    let retail_toc = toc("120105");
    release(
        &home.scenario,
        "v1",
        &[(
            "Foo.zip",
            &[("mainline", 120_105)],
            vec![("Foo/Foo.toc", &retail_toc)],
        )],
    );
    a_cache_hit_equals_a_download(&log, &home, &home.retail, "gh:o/foo", "asset");
}

#[test]
fn a_cache_hit_equals_a_download_git() {
    let log = tlog!();
    let mut home = PlanningHome::new();
    log.env(&home.env);
    log.step("seed home");
    // GroupBulletinBoard as recorded from GitHub, on the 3.3.5a client.
    home.scenario = scenario("git-github");
    let other = home.other.clone();
    a_cache_hit_equals_a_download(
        &log,
        &home,
        &other,
        "git:https://github.com/fondlez/GroupBulletinBoard#main",
        "asset",
    );
}

#[test]
fn a_cache_hit_equals_a_download_wowi() {
    let log = tlog!();
    let home = PlanningHome::new();
    log.env(&home.env);
    log.step("seed home");
    let bugsack_toc = format!("{}## Title: BugSack\n", toc("120105"));
    let zip = zip_text(&[("BugSack/BugSack.toc", &bugsack_toc)]);
    let md5 = common::wowi::wowi_file(&home.scenario, 5995, &zip);
    common::wowi::wowi_details(
        &home.scenario,
        &[5995],
        &[common::wowi::details(5995, "BugSack", "1.0", &md5)],
    );
    a_cache_hit_equals_a_download(&log, &home, &home.retail, "wowi:5995", "asset");
}

#[test]
fn a_cache_hit_equals_a_download_tukui() {
    use common::tukui::{elvui, etag, tukui_addon, tukui_download, tukui_list};
    let log = tlog!();
    let home = PlanningHome::new();
    log.env(&home.env);
    log.step("seed home");
    let elvui_toc = "## Interface: 120105\n## Title: ElvUI\n## X-Tukui-ProjectID: -2\n\
                     ## X-Tukui-ProjectFolders: ElvUI, ElvUI_Libraries, ElvUI_Options\n";
    let zip = zip_text(&[
        ("ElvUI/ElvUI.toc", elvui_toc),
        ("ElvUI_Libraries/ElvUI_Libraries.toc", elvui_toc),
        ("ElvUI_Options/ElvUI_Options.toc", elvui_toc),
    ]);
    tukui_list(&home.scenario, &[elvui()]);
    tukui_addon(&home.scenario, &elvui(), &["TOKEN-A", "TOKEN-B"]);
    tukui_download(&home.scenario, "elvui", "TOKEN-A", &zip, Some(&etag(&zip)));
    let (first, second) =
        a_cache_hit_equals_a_download(&log, &home, &home.retail, "tukui:elvui", "asset");
    // The very case deferred keying fixes: a URL after a download, none after a hit.
    let url = |plan: &Plan| plan.targets[0].releases[0].url.is_some();
    log.assert_eq("the first plan's release has its URL", &url(&first), &true);
    log.assert_eq("the second plan's release has none", &url(&second), &false);
}

#[test]
fn a_cache_hit_equals_a_download_wowace() {
    let log = tlog!();
    let mut home = PlanningHome::new();
    log.env(&home.env);
    log.step("seed home");
    // BugGrabber's tree as recorded from WowAce.
    home.scenario = scenario("wowace-bug-grabber");
    a_cache_hit_equals_a_download(
        &log,
        &home,
        &home.retail,
        "wowace:bug-grabber",
        "wowace-file",
    );
}

#[test]
fn a_file_directory_plans_with_one_release_id() {
    let log = tlog!();
    let home = PlanningHome::new();
    log.env(&home.env);
    log.step("seed home");
    let dir = home.env.root().join("src/Mine");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Mine.toc"), toc("120105")).unwrap();
    let from_dir = || {
        let mut change = user_install("gh:o/mine");
        if let RequestedChange::Install { file, .. } = &mut change {
            *file = Some(dir.clone());
        }
        vec![request(&home.retail, vec![change], TxKind::Install)]
    };
    let mut runs = Vec::new();
    for step in ["first plan", "second plan"] {
        log.step(step);
        runs.push(planned(&log, &home, from_dir(), &PlanOpts::default()));
    }
    log.step("compare");
    let id = |plan: &Plan| plan.targets[0].releases[0].release_id.clone();
    let first_id = id(&runs[0].0);
    log.assert_eq("one release id", &id(&runs[1].0), &first_id);
    log.assert_eq(
        "a listing id",
        &first_id.starts_with("gh:o/mine@listing:"),
        &true,
    );
    same_token("an unchanged folder", &runs[0].1, &runs[1].1);
    let inputs = |plan: &Plan| {
        plan.witnesses[0]
            .inputs
            .iter()
            .map(|i| (i.kind.wire(), i.digest.clone()))
            .collect::<Vec<_>>()
    };
    let first_inputs = inputs(&runs[0].0);
    log.assert_eq("one dir input", &first_inputs.len(), &1);
    log.assert_eq("a dir input", &first_inputs[0].0, &"dir");
    log.assert_eq("the same listing", &inputs(&runs[1].0), &first_inputs);

    log.step("an edited folder");
    fs::write(
        dir.join("Mine.toc"),
        format!("{}## Version: 2\n", toc("120105")),
    )
    .unwrap();
    let (edited, token, _) = planned(&log, &home, from_dir(), &PlanOpts::default());
    log.assert_eq("another release id", &(id(&edited) != first_id), &true);
    let preview: Vec<String> = ["omaddons", "install", "gh:o/mine", "--dry-run"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let changed = check(Some(&runs[0].1.token()), &token, &preview)
        .err()
        .map(|e| e.details["changed"].clone());
    log.assert_eq(
        "details.changed",
        &changed,
        &Some(serde_json::json!(["selection", "observed"])),
    );
}
