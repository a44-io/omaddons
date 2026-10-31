//! e2e suite: the doctor's tolerant view against synthetic homes
//!
//! Clients built by the test builders, addons installed by the real binary, faults planted
//! on disk, and the view (`doctor::view::DoctorView`) loaded in-process and driven the way
//! `doctor diagnose` (E3) will drive it, with `status --json` run beside it. A layer that
//! cannot be read is one finding that holds none of the file's content, and every check
//! that needs it is skipped, never run on defaults; loading the view writes nothing; two
//! loads of one home agree; the detectors `status` publishes agree with `status`; `-t`
//! scopes per-client findings and keeps the machine's; and nothing names an account, realm
//! or character.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use common::addons::toc;
use common::archives::zip_text;
use common::home::Home;
use common::http::paths;
use common::privacy::{assert_private, assert_private_all};
use common::procfs::running_as_root;
use common::state::key_of;
use common::tlog::{TestLog, tlog};
use common::trees::CustomRoot;
use common::wtf::Wtf;
use omaddons::doctor::budget::Budget;
use omaddons::doctor::finding::Finding;
use omaddons::doctor::registry::{self, FAILURE_MODES, Layer, Tier, ids};
use omaddons::doctor::selection::{
    Scope, Selection, SkipReason, Skipped, SkippedTarget, TargetSel, partition, target_skips,
    visible_failures,
};
use omaddons::doctor::view::{
    Availability, BoundedRead, DoctorView, FileStat, ViewDiscovery, ViewOptions, merge_findings,
};
use omaddons::error::{ErrorCode, Result};
use omaddons::limits::DISCOVERY_FILE_MAX_BYTES;
use omaddons::model::{TargetKey, TxId};
use omaddons::state::paths::Paths;
use serde::Serialize;
use serde_json::{Value, json};

/// Planted in every broken file: no output may show it.
const MARKER: &str = "PLANTED-CONTENT-7f3a";
const RETAIL: &str = "120105";
const ERA: &str = "11509";
const LEGACY: &str = "30300";
/// The failure modes `status` publishes, whose detectors moved onto the view.
const MOVED: [&str; 7] = [
    ids::JOURNAL_PENDING,
    ids::STATE_VERSION_NEWER,
    ids::RECORD_FOLDER_MISSING,
    ids::MANAGED_FILES_MODIFIED,
    ids::OWNERSHIP_TRANSFERRED,
    ids::TARGET_VANISHED,
    ids::WIDGET_VERSION_DRIFT,
];

use support::{
    Agreement, Tuple, agreement_home, argv, assert_clean, broken_config, diagnose, edit_record,
    expected_skips, finding_tuples, install, json_of, load_view, plant_journal, set_mode,
    status_tuples, touch_every_layer, try_diagnose, view_with,
};

#[cfg(test)]
mod support {
    use super::*;

    /// What `doctor` runs by default: every tier but `deep`.
    const TIERS: [Tier; 3] = [Tier::Cached, Tier::Quick, Tier::Default];

    // ---------------------------------------------------------------------------------------
    // The view, and a diagnosis of it.

    /// The doctor's default view of `home`: discovery scans, reusing the cache.
    pub(super) fn load_view(home: &Home) -> DoctorView {
        view_with(home, ViewDiscovery::Scan)
    }

    pub(super) fn view_with(home: &Home, discovery: ViewDiscovery) -> DoctorView {
        DoctorView::load_at(
            paths(&home.env),
            ViewOptions {
                discovery,
                ..ViewOptions::default()
            },
        )
    }

    /// One diagnosis, as `doctor diagnose` will report it.
    #[derive(Debug, Serialize)]
    pub(super) struct Report {
        pub findings: Vec<Finding>,
        pub skipped: Vec<Skipped>,
        pub ran: Vec<&'static str>,
    }

    impl Report {
        /// The findings of the failure mode `id`.
        pub(super) fn of(&self, id: &str) -> Vec<&Finding> {
            self.findings.iter().filter(|f| f.id == id).collect()
        }

        /// The findings about the client `id` (by its id as findings name it).
        pub(super) fn about(&self, id: &str) -> Vec<&Finding> {
            self.findings
                .iter()
                .filter(|f| f.target.as_ref().is_some_and(|t| t.id == id))
                .collect()
        }

        /// The findings about no client.
        pub(super) fn machine(&self) -> Vec<&Finding> {
            self.findings
                .iter()
                .filter(|f| f.target.is_none())
                .collect()
        }
    }

    /// The steps the runtime of E3 (`doctor diagnose`) takes over a view, in its order and
    /// nothing more: the rows split by `partition`, each that runs detected (with no
    /// budget), the per-client skips added, the view failures a report shows merged with
    /// the findings, and the scope applied. E3's runtime replaces it, and E3's tests cover
    /// that one.
    pub(super) fn try_diagnose(
        log: &TestLog,
        view: &DoctorView,
        sel: &Selection,
    ) -> Result<Report> {
        let part = partition(view, sel, FAILURE_MODES, &TIERS)?;
        let mut detected = Vec::new();
        for row in &part.run {
            let Some(detect) = row.detect else {
                continue;
            };
            let found = detect(view, sel, &mut Budget::unlimited()).found;
            detected.extend(found.into_iter().map(|obs| row.finding(obs)));
        }
        let mut skipped = part.skipped.clone();
        skipped.extend(target_skips(view, &part));
        let failures = visible_failures(view, sel, &part, &skipped);
        let findings = merge_findings(failures, detected)
            .into_iter()
            .filter(|f| part.scope.includes(f.target.as_ref()))
            .collect();
        let report = Report {
            findings,
            skipped,
            ran: part.run.iter().map(|row| row.id).collect(),
        };
        log_view(log, view, &report);
        Ok(report)
    }

    #[track_caller]
    pub(super) fn diagnose(log: &TestLog, view: &DoctorView, sel: &Selection) -> Report {
        log.step("diagnose");
        let report = try_diagnose(log, view, sel);
        assert!(
            report.is_ok(),
            "diagnose failed: {:?}",
            report.as_ref().err()
        );
        report.unwrap()
    }

    /// The loaded layers, whether each is available, the view's failures and the report,
    /// so a failure reads from the transcript alone. Asks only about loaded layers, so it
    /// loads nothing a detector did not.
    fn log_view(log: &TestLog, view: &DoctorView, report: &Report) {
        let loaded = view.loaded_layers();
        log.json("loaded layers", &loaded);
        let availability: BTreeMap<String, String> = loaded
            .iter()
            .map(|&layer| {
                let state = match view.availability(layer) {
                    Availability::Available => "available".to_owned(),
                    Availability::Unavailable { cause } => {
                        format!("unavailable ({} failed)", layer_name(cause))
                    }
                };
                (layer_name(layer), state)
            })
            .collect();
        log.json("availability", &availability);
        log.json("failures", &view.failure_findings());
        log.json("skipped", &report.skipped);
        log.json("ran", &report.ran);
        log.json("findings", &report.findings);
    }

    fn layer_name(layer: Layer) -> String {
        serde_json::to_value(layer)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// The clean twin of a fault: the home as it is gives no view failure and no skip, and
    /// loading it writes nothing.
    #[track_caller]
    pub(super) fn assert_clean(log: &TestLog, home: &Home) {
        log.step("clean twin");
        log.tree("clean twin", home.env.root());
        let view = load_view(home);
        let report = diagnose(log, &view, &Selection::all());
        log.assert_eq(
            "clean twin: no view failure",
            &view.failure_findings(),
            &Vec::new(),
        );
        log.assert_eq("clean twin: no skip", &report.skipped, &Vec::new());
        log.assert_unchanged("clean twin", home.env.root());
    }

    /// The skips of every row a default run selects that reads one of `unavailable`, all
    /// for `reason` and for every client: what a run reports when those layers failed.
    pub(super) fn expected_skips(unavailable: &[Layer], reason: SkipReason) -> Vec<Skipped> {
        FAILURE_MODES
            .iter()
            .filter(|row| TIERS.contains(&row.tier))
            .filter(|row| row.requires.iter().any(|layer| unavailable.contains(layer)))
            .map(|row| Skipped {
                id: row.id,
                reason,
                target: None,
            })
            .collect()
    }

    /// Each layer loaded and asked about, each discovered client's inventory built, and
    /// the view's failures, warnings and diagnostics read.
    pub(super) fn touch_every_layer(log: &TestLog, view: &DoctorView) {
        log.step("touch every layer");
        view.config();
        view.registry();
        view.records();
        view.journals();
        view.lock();
        view.history();
        view.caches();
        view.widget();
        view.skill();
        view.credentials();
        if let Some(world) = view.world() {
            for discovered in &world.resolved.targets {
                assert!(
                    view.inventory(discovered).is_some(),
                    "{}'s inventory",
                    discovered.target.id
                );
            }
        }
        for layer in Layer::ALL {
            assert_eq!(
                view.availability(layer),
                Availability::Available,
                "{layer:?}"
            );
        }
        log.json("loaded layers", &view.loaded_layers());
        log.json("failures", &view.failure_findings());
        log.json("warnings", &view.warnings());
        log.json("diagnostics", &view.diagnostics());
    }

    /// A finding as its wire form: the suite reads fields through it, so a field renamed
    /// in the finding model changes one helper, not every scenario.
    pub(super) fn json_of(finding: &Finding) -> Value {
        serde_json::to_value(finding).unwrap()
    }

    // ---------------------------------------------------------------------------------------
    // Building the states.

    /// Installs a one-folder addon `name` into `target` from a local zip, as `gh:o/<name>`.
    pub(super) fn install(log: &TestLog, home: &Home, target: &str, name: &str, interface: &str) {
        log.step(&format!("install {name} on {target}"));
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        let zip = dl.join(format!("{name}-{target}.zip"));
        let entry = format!("{name}/{name}.toc");
        fs::write(&zip, zip_text(&[(&entry, &toc(interface, &[]))])).unwrap();
        let reference = format!("gh:o/{}", name.to_ascii_lowercase());
        let zip = zip.display().to_string();
        log.run(
            home,
            &[
                "install", &reference, "--file", &zip, "-t", target, "--json",
            ],
        )
        .ok();
    }

    /// Reads `target`'s record as JSON, lets `edit` change it, and writes it back.
    pub(super) fn edit_record(home: &Home, target: &str, edit: impl FnOnce(&mut Value)) {
        let path = paths(&home.env).target_record(&key_of(home, target));
        let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        edit(&mut record);
        fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    }

    /// Marks the addon `name` of a record degraded, as an ownership transfer leaves it.
    pub(super) fn degrade(record: &mut Value, name: &str) {
        let addons = record["addons"].as_object_mut().unwrap();
        let addon = addons.values_mut().find(|a| a["name"] == name);
        assert!(addon.is_some(), "no {name} in the record");
        addon.unwrap()["state"] = json!("degraded");
    }

    /// A journal whose `prepared` line names `key` but does not decode: one recovery
    /// attributes to that client and cannot read.
    pub(super) fn plant_journal(log: &TestLog, home: &Home, key: TargetKey) -> TxId {
        let paths = paths(&home.env);
        let tx = TxId::new();
        let line = json!({"type": "prepared", "schema_version": "1.1", "target_key": key});
        fs::create_dir_all(paths.journal_dir()).unwrap();
        fs::write(paths.journal(&tx), format!("{line}\n")).unwrap();
        log.note(format!("planted an unreadable journal {tx} naming {key}"));
        tx
    }

    /// A `config.toml` whose second line does not parse, holding the marker.
    pub(super) fn broken_config() -> String {
        format!("check_interval_hours = 6\n[limits\ndefault_target = \"{MARKER}\"\n")
    }

    /// Sets `path`'s permission bits; returns the ones it had.
    pub(super) fn set_mode(path: &std::path::Path, mode: u32) -> u32 {
        let before = fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        before
    }

    pub(super) fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    // ---------------------------------------------------------------------------------------
    // The agreement home.

    /// Three clients holding the six states `status` and the view must agree on.
    pub(super) struct Agreement {
        pub home: Home,
        /// The 3.3.5a client's id; its directory is gone.
        pub legacy: String,
    }

    /// Retail, Classic Era and a 3.3.5a client, with Foo and Bar on retail and Foo on the
    /// other two, then: retail's Bar folder deleted (`record-folder-missing`), retail's Foo
    /// degraded (`ownership-transferred`), an unreadable journal naming retail
    /// (`journal-pending`, needing manual repair), a byte added to era's `Foo.toc`
    /// (`managed-files-modified`), era's record at schema 1.9 (`state-version-newer`) and
    /// the 3.3.5a client's directory removed (`target-vanished`).
    pub(super) fn agreement_home(log: &TestLog) -> Agreement {
        log.step("build the home");
        let home = Home::new(&["wow", "wow_classic_era"]);
        let legacy_dir = CustomRoot::at(home.env.home().join("Games/wow-335"))
            .version(3, 3, 5, 12_340)
            .build();
        let ran = log.run(&home, &["status", "--json"]);
        ran.ok();
        let legacy = ran.data()["targets"]
            .as_array()
            .and_then(|targets| targets.iter().find(|t| t["family"] == "wrath-legacy"))
            .and_then(|t| t["id"].as_str())
            .map(str::to_owned);
        assert!(legacy.is_some(), "no 3.3.5a client: {}", ran.envelope());
        let legacy = legacy.unwrap();
        install(log, &home, "retail", "Foo", RETAIL);
        install(log, &home, "retail", "Bar", RETAIL);
        install(log, &home, "era", "Foo", ERA);
        install(log, &home, &legacy, "Foo", LEGACY);
        assert_clean(log, &home);

        log.step("plant the six states");
        fs::remove_dir_all(home.addons("_retail_").join("Bar")).unwrap();
        edit_record(&home, "retail", |record| degrade(record, "Foo"));
        plant_journal(log, &home, key_of(&home, "retail"));
        let toc = home.addons("_classic_era_").join("Foo/Foo.toc");
        let mut bytes = fs::read(&toc).unwrap();
        bytes.push(b'\n');
        fs::write(&toc, bytes).unwrap();
        edit_record(&home, "era", |record| {
            record["schema_version"] = json!("1.9");
        });
        fs::remove_dir_all(&legacy_dir).unwrap();
        Agreement { home, legacy }
    }

    // ---------------------------------------------------------------------------------------
    // Comparing with `status`.

    /// `(id, client id, addon, severity, title)`.
    pub(super) type Tuple = (String, Option<String>, Option<String>, String, String);

    /// Every issue of a `status` envelope as a tuple, on its client or none, sorted.
    pub(super) fn status_tuples(envelope: &Value) -> Vec<Tuple> {
        let tuple = |issue: &Value, target: Option<&str>| {
            (
                issue["id"].as_str().unwrap().to_owned(),
                target.map(str::to_owned),
                issue["addon"].as_str().map(str::to_owned),
                issue["severity"].as_str().unwrap().to_owned(),
                issue["title"].as_str().unwrap().to_owned(),
            )
        };
        let mut out: Vec<Tuple> = Vec::new();
        for target in envelope["data"]["targets"].as_array().unwrap() {
            let id = target["id"].as_str();
            for issue in target["issues"].as_array().into_iter().flatten() {
                out.push(tuple(issue, id));
            }
        }
        for issue in envelope["data"]["issues"].as_array().into_iter().flatten() {
            out.push(tuple(issue, None));
        }
        out.sort();
        out
    }

    /// The same tuples from findings, sorted.
    pub(super) fn finding_tuples(findings: &[Finding]) -> Vec<Tuple> {
        let mut out: Vec<Tuple> = findings
            .iter()
            .map(|f| {
                let f = json_of(f);
                (
                    f["id"].as_str().unwrap().to_owned(),
                    f["target"]["id"].as_str().map(str::to_owned),
                    f["addon"].as_str().map(str::to_owned),
                    f["severity"].as_str().unwrap().to_owned(),
                    f["title"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        out.sort();
        out
    }
}

// ---------------------------------------------------------------------------------------
// Broken layers.

#[test]
fn a_broken_config_is_a_finding_and_its_dependents_are_skipped() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic_era"]);
    install(&log, &home, "retail", "Foo", RETAIL);
    install(&log, &home, "era", "Foo", ERA);
    assert_clean(&log, &home);

    log.step("plant a broken config.toml");
    let paths = paths(&home.env);
    fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
    fs::write(paths.config_file(), broken_config()).unwrap();
    log.tree("home", home.env.root());

    let view = load_view(&home);
    let report = diagnose(&log, &view, &Selection::all());
    log.step("assert");
    assert!(
        view.config().is_none(),
        "config.toml read as a configuration"
    );
    log.assert_eq(
        "findings",
        &report.findings.iter().map(|f| f.id).collect::<Vec<_>>(),
        &vec![ids::CONFIG_PARSE_ERROR],
    );
    let finding = json_of(&report.findings[0]);
    log.assert_eq(
        "the finding's evidence and target",
        &(
            &finding["evidence"]["reason"],
            &finding["evidence"]["line"],
            &finding["evidence"]["path"],
            &finding["target"],
        ),
        &(
            &json!("parse"),
            &json!(2),
            &json!(paths.config_file()),
            &Value::Null,
        ),
    );
    let skipped = expected_skips(
        &[
            Layer::Config,
            Layer::Discovery,
            Layer::Inventory,
            Layer::Credentials,
        ],
        SkipReason::ConfigUnavailable,
    );
    assert!(!skipped.is_empty(), "no row reads the config's dependents");
    log.assert_eq("skipped", &report.skipped, &skipped);
    for skip in &skipped {
        assert!(!report.ran.contains(&skip.id), "{} ran", skip.id);
    }
    for id in [ids::STATE_VERSION_NEWER, ids::OWNERSHIP_TRANSFERRED] {
        assert!(
            report.ran.contains(&id),
            "{id} did not run: {:?}",
            report.ran
        );
    }
    let text = serde_json::to_string(&report).unwrap();
    assert!(!text.contains(MARKER), "the report shows the file: {text}");
    log.step("unchanged");
    log.assert_unchanged("home", home.env.root());
}

#[test]
fn a_corrupt_registry_is_a_finding_and_records_only_detectors_still_run() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic_era"]);
    install(&log, &home, "retail", "Foo", RETAIL);
    install(&log, &home, "era", "Foo", ERA);
    assert_clean(&log, &home);

    log.step("plant a newer, degraded record and a truncated registry");
    let retail = key_of(&home, "retail");
    edit_record(&home, "retail", |record| {
        record["schema_version"] = json!("1.9");
        support::degrade(record, "Foo");
    });
    let paths = paths(&home.env);
    let registry = fs::read(paths.registry_file()).unwrap();
    fs::write(paths.registry_file(), &registry[..20]).unwrap();
    log.tree("home", home.env.root());

    let view = load_view(&home);
    let report = diagnose(&log, &view, &Selection::all());
    log.step("assert");
    assert!(
        view.registry().is_none(),
        "registry.json read as a registry"
    );
    let corrupt: Vec<Value> = report
        .of(ids::STATE_CORRUPT)
        .into_iter()
        .map(json_of)
        .collect();
    log.assert_eq(
        "state-corrupt",
        &corrupt
            .iter()
            .map(|f| {
                (
                    &f["evidence"]["reason"],
                    &f["evidence"]["path"],
                    &f["target"],
                )
            })
            .collect::<Vec<_>>(),
        &vec![(
            &json!("decode"),
            &json!(paths.registry_file()),
            &Value::Null,
        )],
    );
    log.assert_eq(
        "skipped",
        &report.skipped,
        &expected_skips(
            &[Layer::Registry, Layer::Discovery, Layer::Inventory],
            SkipReason::RegistryUnavailable,
        ),
    );
    // The registry cannot name the client: its key does.
    let named = |id: &str| {
        report
            .of(id)
            .into_iter()
            .map(|f| f.target.as_ref().map(|t| (t.id.clone(), t.key)))
            .collect::<Vec<_>>()
    };
    for id in [ids::STATE_VERSION_NEWER, ids::OWNERSHIP_TRANSFERRED] {
        log.assert_eq(id, &named(id), &vec![Some((retail.to_string(), retail))]);
    }
    log.step("unchanged");
    log.assert_unchanged("home", home.env.root());
}

#[test]
fn one_corrupt_record_skips_only_its_target() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic_era"]);
    install(&log, &home, "retail", "Foo", RETAIL);
    install(&log, &home, "era", "Foo", ERA);
    assert_clean(&log, &home);

    log.step("plant era's edited Foo and retail's corrupt record");
    let retail = key_of(&home, "retail");
    let toc = home.addons("_classic_era_").join("Foo/Foo.toc");
    let mut bytes = fs::read(&toc).unwrap();
    bytes.extend_from_slice(b"## X-Edited: yes\n");
    fs::write(&toc, bytes).unwrap();
    fs::write(paths(&home.env).target_record(&retail), "{").unwrap();
    log.tree("home", home.env.root());

    let view = load_view(&home);
    let report = diagnose(&log, &view, &Selection::all());
    log.step("assert");
    let corrupt: Vec<Option<TargetKey>> = report
        .of(ids::STATE_CORRUPT)
        .iter()
        .map(|f| f.target.as_ref().map(|t| t.key))
        .collect();
    log.assert_eq("state-corrupt", &corrupt, &vec![Some(retail)]);
    let records_rows: Vec<&'static str> = report
        .ran
        .iter()
        .copied()
        .filter(|id| registry::row(id).is_some_and(|row| row.requires.contains(&Layer::Records)))
        .collect();
    assert!(!records_rows.is_empty(), "no row that reads records ran");
    let expected: Vec<Skipped> = records_rows
        .iter()
        .map(|id| Skipped {
            id,
            reason: SkipReason::RecordsUnavailable,
            target: Some(SkippedTarget {
                id: "retail".to_owned(),
                key: Some(retail),
            }),
        })
        .collect();
    log.assert_eq("skipped", &report.skipped, &expected);
    let era_modified: Vec<Option<String>> = report
        .of(ids::MANAGED_FILES_MODIFIED)
        .iter()
        .map(|f| f.target.as_ref().map(|t| t.id.clone()))
        .collect();
    log.assert_eq(
        "managed-files-modified",
        &era_modified,
        &vec![Some("era".to_owned())],
    );
    for f in report.about("retail") {
        assert!(
            !records_rows.contains(&f.id),
            "{} reported retail, whose record is unreadable",
            f.id
        );
    }
    log.step("unchanged");
    log.assert_unchanged("home", home.env.root());
}

#[test]
fn newer_state_that_does_not_decode_is_a_finding() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic_era"]);
    install(&log, &home, "retail", "Foo", RETAIL);
    install(&log, &home, "era", "Foo", ERA);
    assert_clean(&log, &home);

    log.step("plant era's record from a newer omaddons");
    let era = key_of(&home, "era");
    let record = paths(&home.env).target_record(&era);
    fs::write(&record, r#"{"schema_version":"9.0","addons":"x"}"#).unwrap();
    log.tree("home", home.env.root());

    let view = load_view(&home);
    let report = diagnose(&log, &view, &Selection::all());
    log.step("assert");
    let newer: Vec<Value> = report
        .of(ids::STATE_VERSION_NEWER)
        .into_iter()
        .map(json_of)
        .collect();
    log.assert_eq(
        "state-version-newer",
        &newer
            .iter()
            .map(|f| {
                (
                    &f["target"]["id"],
                    &f["evidence"]["observed"],
                    &f["evidence"]["reason"],
                    &f["evidence"]["path"],
                )
            })
            .collect::<Vec<_>>(),
        &vec![(
            &json!("era"),
            &json!("9.0"),
            &json!("newer"),
            &json!(record),
        )],
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.reason == SkipReason::RecordsUnavailable
                && s.target.as_ref().is_some_and(|t| t.key == Some(era))),
        "no records skip names era: {:?}",
        report.skipped
    );
    assert!(
        report
            .skipped
            .iter()
            .all(|s| s.target.as_ref().is_some_and(|t| t.id == "era")),
        "a skip names another client: {:?}",
        report.skipped
    );
    log.step("unchanged");
    log.assert_unchanged("home", home.env.root());
}

/// The state files planted wrong, one per case.
#[cfg(test)]
mod faults {
    use super::*;

    /// One state file planted wrong.
    pub(super) struct Fault {
        pub name: &'static str,
        /// Plants the fault; `false` when it cannot be planted here (the case is then noted).
        pub plant: fn(&TestLog, &Paths) -> bool,
        /// Takes it away again before the second hash, given the file's mode before the
        /// fault, when the tree cannot be hashed with it in place (a FIFO blocks the read, a
        /// mode-000 file fails it); `None` when it can.
        pub undo: Option<fn(&Paths, u32)>,
        /// The file the finding names.
        pub file: fn(&Paths) -> PathBuf,
        pub fm: &'static str,
        pub reason: &'static str,
        /// The layer the fault leaves unavailable (never read as empty), and the layers that
        /// go with it.
        pub unavailable: &'static [Layer],
        pub skip: SkipReason,
    }

    const REGISTRY_FAULT: &[Layer] = &[Layer::Registry, Layer::Discovery, Layer::Inventory];

    fn mode_000(log: &TestLog, path: &std::path::Path) -> bool {
        if running_as_root() {
            log.note("SKIP: root reads a mode-000 file");
            return false;
        }
        set_mode(path, 0o000);
        true
    }

    pub(super) const FAULTS: &[Fault] = &[
        Fault {
            name: "registry_over_the_bound",
            plant: |_, paths| {
                let mut bytes = fs::read(paths.registry_file()).unwrap();
                bytes.resize(usize::try_from(DISCOVERY_FILE_MAX_BYTES + 1).unwrap(), b' ');
                fs::write(paths.registry_file(), bytes).unwrap();
                true
            },
            undo: None,
            file: Paths::registry_file,
            fm: ids::STATE_CORRUPT,
            reason: "too_large",
            unavailable: REGISTRY_FAULT,
            skip: SkipReason::RegistryUnavailable,
        },
        Fault {
            name: "registry_mode_000",
            plant: |log, paths| mode_000(log, &paths.registry_file()),
            undo: Some(|paths, mode| {
                set_mode(&paths.registry_file(), mode);
            }),
            file: Paths::registry_file,
            fm: ids::STATE_CORRUPT,
            reason: "unreadable",
            unavailable: REGISTRY_FAULT,
            skip: SkipReason::RegistryUnavailable,
        },
        Fault {
            name: "registry_is_a_directory",
            plant: |_, paths| {
                fs::remove_file(paths.registry_file()).unwrap();
                fs::create_dir(paths.registry_file()).unwrap();
                true
            },
            undo: None,
            file: Paths::registry_file,
            fm: ids::STATE_CORRUPT,
            reason: "not_a_file",
            unavailable: REGISTRY_FAULT,
            skip: SkipReason::RegistryUnavailable,
        },
        Fault {
            name: "registry_dangling_symlink",
            plant: |_, paths| {
                fs::remove_file(paths.registry_file()).unwrap();
                std::os::unix::fs::symlink("gone.json", paths.registry_file()).unwrap();
                true
            },
            undo: None,
            file: Paths::registry_file,
            fm: ids::STATE_CORRUPT,
            reason: "unreadable",
            unavailable: REGISTRY_FAULT,
            skip: SkipReason::RegistryUnavailable,
        },
        Fault {
            name: "config_is_a_fifo",
            plant: |log, paths| {
                let made = std::process::Command::new("mkfifo")
                    .arg(paths.config_file())
                    .status();
                if !made.is_ok_and(|status| status.success()) {
                    log.note("SKIP: mkfifo is not available");
                    return false;
                }
                true
            },
            undo: Some(|paths, _| fs::remove_file(paths.config_file()).unwrap()),
            file: Paths::config_file,
            fm: ids::CONFIG_PARSE_ERROR,
            reason: "not_a_file",
            unavailable: &[
                Layer::Config,
                Layer::Discovery,
                Layer::Inventory,
                Layer::Credentials,
            ],
            skip: SkipReason::ConfigUnavailable,
        },
        Fault {
            name: "history_mode_000",
            plant: |log, paths| mode_000(log, &paths.history_file()),
            undo: Some(|paths, mode| {
                set_mode(&paths.history_file(), mode);
            }),
            file: Paths::history_file,
            fm: ids::STATE_CORRUPT,
            reason: "unreadable",
            unavailable: &[Layer::History],
            skip: SkipReason::HistoryUnavailable,
        },
    ];
}

#[test]
fn oversized_unreadable_and_odd_state_files_are_never_clean() {
    let log = tlog!();
    for fault in faults::FAULTS {
        let case = log.case(fault.name);
        let home = Home::new(&["wow"]);
        install(&case, &home, "retail", "Foo", RETAIL);
        let paths = paths(&home.env);
        // The config directory a planted config.toml goes in, made before the first hash.
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        assert_clean(&case, &home);

        case.step(&format!("plant {}", fault.name));
        if fault.undo.is_some() {
            case.tree("home", home.env.root());
        }
        let file = (fault.file)(&paths);
        let mode = fs::symlink_metadata(&file).map_or(0, |m| m.permissions().mode() & 0o7777);
        if !(fault.plant)(&case, &paths) {
            continue;
        }
        if fault.undo.is_none() {
            case.tree("home", home.env.root());
        }
        let view = load_view(&home);
        let report = diagnose(&case, &view, &Selection::all());

        case.step("assert");
        let layer = fault.unavailable[0];
        assert_ne!(
            view.availability(layer),
            Availability::Available,
            "{}: {layer:?} read as empty",
            fault.name
        );
        let found: Vec<Value> = report.of(fault.fm).into_iter().map(json_of).collect();
        case.json("found", &found);
        assert_eq!(found.len(), 1, "{}: {found:?}", fault.name);
        let finding = &found[0];
        case.assert_eq(
            "the finding",
            &(
                &finding["evidence"]["reason"],
                &finding["evidence"]["path"],
                &finding["target"],
            ),
            &(&json!(fault.reason), &json!(file), &Value::Null),
        );
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            finding["title"]
                .as_str()
                .is_some_and(|title| title.starts_with(&name)),
            "{}: {}",
            fault.name,
            finding["title"]
        );
        if fault.reason == "too_large" {
            case.assert_eq(
                "the bound",
                &(&finding["evidence"]["bytes"], &finding["evidence"]["limit"]),
                &(
                    &json!(DISCOVERY_FILE_MAX_BYTES + 1),
                    &json!("DISCOVERY_FILE_MAX_BYTES"),
                ),
            );
        }
        case.assert_eq(
            "skipped",
            &report.skipped,
            &expected_skips(fault.unavailable, fault.skip),
        );

        case.step("unchanged");
        if let Some(undo) = fault.undo {
            undo(&paths, mode);
        }
        case.assert_unchanged("home", home.env.root());
    }
}

// ---------------------------------------------------------------------------------------
// Writes.

#[test]
fn loading_every_layer_writes_nothing() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic_era"]);
    install(&log, &home, "retail", "Foo", RETAIL);
    install(&log, &home, "era", "Foo", ERA);
    assert_clean(&log, &home);

    log.step("plant a journal, a skill, a credential and blobs");
    let paths = paths(&home.env);
    let tx = plant_journal(&log, &home, key_of(&home, "retail"));
    fs::create_dir_all(paths.skill_dir()).unwrap();
    fs::write(
        paths.skill_dir().join("SKILL.md"),
        "---\nname: omaddons\n---\n",
    )
    .unwrap();
    // ubs:ignore — a canary planted to show nothing reads it, not a credential
    let token = "ghp_planted0token0for0the0doctor0view00";
    log.secret(token);
    let credentials = paths.credentials_dir();
    fs::create_dir_all(&credentials).unwrap();
    set_mode(&credentials, 0o700);
    let credential = credentials.join("github");
    fs::write(&credential, token).unwrap();
    set_mode(&credential, 0o600);
    fs::create_dir_all(paths.blobs_dir()).unwrap();
    for name in ["a.zip", "b.zip"] {
        fs::write(paths.blobs_dir().join(name), name).unwrap();
    }
    if paths.discovery_cache().exists() {
        fs::remove_file(paths.discovery_cache()).unwrap();
    }
    log.tree("home", home.env.root());

    for discovery in [ViewDiscovery::Scan, ViewDiscovery::Cached] {
        log.step(&format!("load a {discovery:?} view"));
        let view = view_with(&home, discovery);
        touch_every_layer(&log, &view);
        let report = diagnose(&log, &view, &Selection::all());
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let pending = view.journals().unwrap();
        assert!(
            pending
                .pending
                .iter()
                .any(|p| p.tx == tx && p.unrecoverable.is_some()),
            "the planted journal: {pending:?}"
        );
        let skill = view.skill().unwrap();
        assert!(
            matches!(skill.first().map(|s| &s.read), Some(BoundedRead::Bytes(_))),
            "the skill: {skill:?}"
        );
        let stored = view.credentials().unwrap();
        assert!(
            stored
                .files
                .iter()
                // ubs:ignore — comparing the file's path, not a secret
                .any(|c| c.path == credential
                    && matches!(c.stat, FileStat::Regular { mode: 0o600, .. })),
            "the credential: {stored:?}"
        );
        assert_eq!(view.caches().unwrap().blobs.entries.len(), 2);
        assert_eq!(view.world().unwrap().resolved.targets.len(), 2);
        log.step("unchanged");
        log.assert_unchanged("home", home.env.root());
        assert!(
            !paths.discovery_cache().exists(),
            "the view wrote the discovery cache"
        );
    }
}

#[test]
fn scanning_discovery_reuses_the_cache_and_never_writes_it() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic_era"]);
    install(&log, &home, "retail", "Foo", RETAIL);
    install(&log, &home, "era", "Foo", ERA);
    log.step("status writes the discovery cache");
    log.run(&home, &["status", "--json"]).ok();
    let paths = paths(&home.env);
    let cache = fs::read(paths.discovery_cache()).unwrap();

    log.step("add a 3.3.5a client");
    let legacy = CustomRoot::at(home.env.home().join("Games/wow-335"))
        .version(3, 3, 5, 12_340)
        .build();
    log.tree("home", home.env.root());

    log.step("load the view");
    let view = load_view(&home);
    let world = view.world();
    assert!(world.is_some(), "discovery: {:?}", view.diagnostics());
    let found: Vec<(String, PathBuf)> = world
        .unwrap()
        .resolved
        .targets
        .iter()
        .map(|d| (d.target.id.clone(), d.target.client_dir.clone()))
        .collect();
    log.json("discovered", &found);
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(
        found.iter().any(|(_, dir)| *dir == legacy),
        "the new client is not discovered: {found:?}"
    );
    log.step("unchanged");
    assert!(
        fs::read(paths.discovery_cache()).unwrap() == cache,
        "the discovery cache changed"
    );
    log.assert_unchanged("home", home.env.root());
}

// ---------------------------------------------------------------------------------------
// Agreement, scope, determinism and privacy.

#[test]
fn the_moved_detectors_agree_with_status() {
    let log = tlog!();
    let Agreement { home, legacy } = agreement_home(&log);

    log.step("status --json");
    let ran = log.run(&home, &["status", "--json"]);
    ran.ok();
    let issues = status_tuples(ran.envelope());
    log.json("status's issues", &issues);
    log.tree("home", home.env.root());

    // The planted journal cannot be read: the doctor reports it under its own row (F1
    // part 1), while `status` keeps `journal-pending` until part 2 puts it on the same
    // mapping. Everything else agrees exactly.
    let view = load_view(&home);
    let mut selected = MOVED.to_vec();
    selected.push(ids::JOURNAL_UNREADABLE);
    let report = diagnose(&log, &view, &Selection::only(&selected));
    log.step("assert");
    let journal = |t: &Tuple| t.0 == ids::JOURNAL_PENDING || t.0 == ids::JOURNAL_UNREADABLE;
    let findings: Vec<Tuple> = finding_tuples(&report.findings)
        .into_iter()
        .filter(|t| !journal(t))
        .collect();
    let others: Vec<Tuple> = issues.iter().filter(|t| !journal(t)).cloned().collect();
    log.assert_eq("findings equal status's issues", &findings, &others);
    let unreadable: Vec<Option<String>> = report
        .of(ids::JOURNAL_UNREADABLE)
        .iter()
        .map(|f| f.target.as_ref().map(|t| t.id.clone()))
        .collect();
    log.assert_eq(
        "the journal's own row",
        &unreadable,
        &vec![Some("retail".to_owned())],
    );
    assert!(
        issues.iter().any(|t| t.0 == ids::JOURNAL_PENDING),
        "status still reports it as journal-pending: {issues:?}"
    );
    // Every planted state is there: the agreement is not of two empty lists.
    let seen: std::collections::BTreeSet<&str> = report.findings.iter().map(|f| f.id).collect();
    let planted: std::collections::BTreeSet<&str> = [
        ids::JOURNAL_UNREADABLE,
        ids::STATE_VERSION_NEWER,
        ids::RECORD_FOLDER_MISSING,
        ids::MANAGED_FILES_MODIFIED,
        ids::OWNERSHIP_TRANSFERRED,
        ids::TARGET_VANISHED,
    ]
    .into();
    log.assert_eq("the six planted states", &seen, &planted);
    assert_eq!(report.about(&legacy).len(), 1, "{legacy}: {report:?}");
    log.step("unchanged");
    log.assert_unchanged("home", home.env.root());
}

#[test]
fn target_selection_scopes_findings_and_keeps_machine_ones() {
    let log = tlog!();
    let root = running_as_root();
    if root {
        log.note("SKIP: root reads a mode-000 history.jsonl; the machine finding is not asserted");
    }
    let cases = [
        "t_era",
        "t_vanished",
        "t_unknown",
        "t_era_config_broken",
        "t_era_both_broken",
    ];
    for name in cases {
        let case = log.case(name);
        let Agreement { home, legacy } = agreement_home(&case);
        let paths = paths(&home.env);
        let era = key_of(&home, "era");
        case.step("plant");
        if name.starts_with("t_era_") {
            fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
            fs::write(paths.config_file(), broken_config()).unwrap();
        }
        if name == "t_era_both_broken" {
            let registry = fs::read(paths.registry_file()).unwrap();
            fs::write(paths.registry_file(), &registry[..20]).unwrap();
        }
        // The tree is hashed before history.jsonl turns unreadable, and after it is back.
        case.tree("home", home.env.root());
        let history_mode = (!root).then(|| set_mode(&paths.history_file(), 0o000));

        let selector = match name {
            "t_vanished" => legacy.clone(),
            "t_unknown" => "nosuch".to_owned(),
            _ => "era".to_owned(),
        };
        let words = argv(&["omaddons", "doctor", "-t", &selector]);
        let sel = Selection::parse(Some(&selector), &[], &[], &words).unwrap();
        let view = load_view(&home);
        case.step("diagnose");
        let report = try_diagnose(&case, &view, &sel);

        case.step("assert");
        if name == "t_unknown" {
            let code = report.as_ref().err().map(|err| err.code);
            case.assert_eq(
                "-t nosuch",
                &format!("{code:?}"),
                &format!("{:?}", Some(ErrorCode::TargetNotFound)),
            );
        } else {
            assert!(report.is_ok(), "{name}: {:?}", report.as_ref().err());
            let report = report.unwrap();
            let history: Vec<Value> = report
                .machine()
                .into_iter()
                .filter(|f| f.id == ids::STATE_CORRUPT)
                .map(json_of)
                .filter(|f| f["evidence"]["path"] == json!(paths.history_file()))
                .collect();
            if !root {
                assert_eq!(history.len(), 1, "{name}: the machine's history finding");
            }
            let clients: std::collections::BTreeSet<String> = report
                .findings
                .iter()
                .filter_map(|f| f.target.as_ref().map(|t| t.id.clone()))
                .collect();
            // Sorted, so the expectation does not depend on the report's order.
            let ids_about = |id: &str| -> Vec<&'static str> {
                let mut found: Vec<&'static str> = report.about(id).iter().map(|f| f.id).collect();
                found.sort_unstable();
                found
            };
            match name {
                "t_era" => {
                    case.assert_eq("clients", &clients, &["era".to_owned()].into());
                    let mut expected = vec![ids::MANAGED_FILES_MODIFIED, ids::STATE_VERSION_NEWER];
                    expected.sort_unstable();
                    case.assert_eq("era's findings", &ids_about("era"), &expected);
                }
                "t_vanished" => {
                    case.assert_eq("clients", &clients, &[legacy.clone()].into());
                    case.assert_eq(
                        "the vanished client's findings",
                        &ids_about(&legacy),
                        &vec![ids::TARGET_VANISHED],
                    );
                }
                "t_era_config_broken" => {
                    case.assert_eq(
                        "scope",
                        &format!("{:?}", sel.scope(&view).unwrap()),
                        &format!("{:?}", Scope::Targets([TargetSel::Key(era)].into())),
                    );
                    case.assert_eq("clients", &clients, &["era".to_owned()].into());
                    case.assert_eq(
                        "era's findings",
                        &ids_about("era"),
                        &vec![ids::STATE_VERSION_NEWER],
                    );
                    for skip in expected_skips(
                        &[Layer::Discovery, Layer::Inventory],
                        SkipReason::ConfigUnavailable,
                    ) {
                        assert!(report.skipped.contains(&skip), "{skip:?}");
                    }
                }
                _ => {
                    case.assert_eq(
                        "scope",
                        &format!("{:?}", sel.scope(&view).unwrap()),
                        &format!(
                            "{:?}",
                            Scope::Unresolved {
                                cause: Layer::Config
                            }
                        ),
                    );
                    case.assert_eq("clients", &clients, &Default::default());
                    for id in [ids::STATE_VERSION_NEWER, ids::OWNERSHIP_TRANSFERRED] {
                        let skip = Skipped {
                            id,
                            reason: SkipReason::ConfigUnavailable,
                            target: None,
                        };
                        assert!(report.skipped.contains(&skip), "{skip:?}");
                        assert!(!report.ran.contains(&id), "{id} ran");
                    }
                }
            }
        }

        case.step("unchanged");
        if let Some(mode) = history_mode {
            set_mode(&paths.history_file(), mode);
        }
        case.assert_unchanged("home", home.env.root());
    }
}

#[test]
fn two_loads_of_one_home_give_identical_findings_and_skips() {
    let log = tlog!();
    let Agreement { home, .. } = agreement_home(&log);
    log.step("plant a broken config.toml");
    let paths = paths(&home.env);
    fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
    fs::write(paths.config_file(), broken_config()).unwrap();
    log.tree("home", home.env.root());

    let first = diagnose(&log, &load_view(&home), &Selection::all());
    let second = diagnose(&log, &load_view(&home), &Selection::all());
    log.step("assert");
    assert!(!first.findings.is_empty() && !first.skipped.is_empty());
    log.assert_eq(
        "the two reports",
        &serde_json::to_string(&second).unwrap(),
        &serde_json::to_string(&first).unwrap(),
    );
    log.step("unchanged");
    log.assert_unchanged("home", home.env.root());
}

#[test]
fn no_view_output_names_an_account_realm_or_character() {
    let log = tlog!();
    let Agreement { home, .. } = agreement_home(&log);
    log.step("build WTF/ with SavedVariables");
    for folder in ["_retail_", "_classic_era_"] {
        Wtf::at(home.client(folder))
            .saved_variables("Foo", 256)
            .config_wtf("SET accountName \"FAKEACCT\"\nSET realmName \"FakeRealm\"\n")
            .build();
    }
    log.tree("home", home.env.root());

    let view = load_view(&home);
    let report = diagnose(&log, &view, &Selection::all());
    log.step("assert");
    assert!(!report.findings.is_empty());
    for (label, text) in [
        ("findings", serde_json::to_string(&report.findings).unwrap()),
        ("skipped", serde_json::to_string(&report.skipped).unwrap()),
        ("warnings", serde_json::to_string(&view.warnings()).unwrap()),
        (
            "failures",
            serde_json::to_string(&view.failure_findings()).unwrap(),
        ),
    ] {
        log.note(format!("private: {label}"));
        assert_private(&text);
    }
    log.step("unchanged");
    log.assert_unchanged("home", home.env.root());

    log.step("status --json --progress json");
    let ran = log.run(&home, &["status", "--json", "--progress", "json"]);
    ran.ok().assert_private();
    assert_private_all(ran.stdout(), ran.stderr());
}
