//! Planning end to end over replayed GitHub releases whose assets are synthetic zips:
//! fetch once, select and verify per target, fall back, refuse early, write nothing
//! outside the cache.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use common::archives::zip_text;
use common::env::TestEnv;
use common::http::{Asset, gh_release, paths, replay};
use common::netlog::{assert_each_url_once, read_net_log};
use common::targets::battlenet;
use common::trees::tree_hash;
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
use omaddons::rules::RulesTable;
use omaddons::sources::SourceRegistry;
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
            let http = self.http();
            let registry = SourceRegistry::builtin();
            let blobs = BlobCache::new(&self.paths);
            let table = RulesTable::builtin();
            let argv: Vec<String> = ["omaddons", "install", "gh:o/foo"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            let ctx = PlanCtx {
                paths: &self.paths,
                scopes: &self.scopes,
                http: http.as_ref(),
                registry: &registry,
                blobs: &blobs,
                table: &table,
                tx: self.tx,
                running: &self.running,
                policy: RunningPolicy::Safe,
                limits: ArchiveLimits::default(),
                actor: Actor::Agent,
                argv: &argv,
            };
            let req = PlanRequest {
                targets,
                force,
                while_running: false,
                dry_run: true,
                kind: None,
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
        }
    }

    /// Update the installed Foo on `target`.
    pub fn update_foo(target: &Target) -> TargetRequest {
        let update = RequestedChange::Update {
            addon: "foo".into(),
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
    request.changes.push(user_install("wowi:5108"));
    let plan = env.plan(vec![request], false);
    assert_eq!(plan.failures.len(), 1);
    assert_eq!(plan.failures[0].error.code.wire(), "source_disabled");
    assert_eq!(
        plan.failures[0]
            .r#ref
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("wowi:5108")
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
    let sets = omaddons::sources::fetch_once(wanted, &SourceRegistry::builtin(), http.as_ref());
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
fn a_record_only_difference_is_unchanged_with_a_note() {
    let (env, retail) = foo_v1_with_v2_out(|r| {
        r.reason = AddonReason::Dependency;
        r.release_id = Some("gh:o/foo@v2".into());
        r.version = "v2".into();
    });
    let plan = env.plan(vec![install(&retail)], false);
    assert!(plan.failures.is_empty(), "{:?}", plan.failures);
    let t = &plan.targets[0];
    assert!(
        t.work.is_empty() && t.warnings.is_empty(),
        "{:?}",
        t.warnings
    );
    assert_eq!(t.notes.len(), 1, "{:?}", t.notes);
    assert!(
        t.notes[0].contains("Foo unchanged") && t.notes[0].contains("record's reason"),
        "{}",
        t.notes[0]
    );

    // The same request with nothing to change has nothing to say.
    let (env, retail) = foo_v1_with_v2_out(|r| {
        r.release_id = Some("gh:o/foo@v2".into());
        r.version = "v2".into();
    });
    let plan = env.plan(vec![install(&retail)], false);
    assert!(plan.targets[0].notes.is_empty() && plan.targets[0].work.is_empty());
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
