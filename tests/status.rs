//! `omaddons status` end to end: a Battle.net retail client and a 3.3.5a custom client
//! (23 `Blizzard_*` folders, a localhost realmlist) in one home; identity before and
//! after the first install; no writes; `--cached` without a scan or without a cache;
//! live running state; leftover journals with the lock free and held; a deleted client.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};

use common::addons::toc;
use common::archives::zip_text;
use common::home::Home;
use common::http::paths;
use common::trees::{CustomRoot, tree_hash};
use jiff::Timestamp;
use omaddons::engine::journal::{self, CommitPayload, IndexChanges, JournalLine, Prepared};
use omaddons::fsx::GlobalLock;
use omaddons::model::{Actor, TargetKey, TxId, TxKind};
use omaddons::state::history::{self, HistoryEntry, TxResult};
use omaddons::state::records::TargetRecord;
use serde_json::Value;
use support::{Machine, target};

#[cfg(test)]
mod support {
    use super::*;

    /// Retail plus a 3.3.5a client under `<home>/Games/wow-335`.
    pub struct Machine {
        pub home: Home,
        pub legacy: PathBuf,
    }

    impl Machine {
        pub fn new() -> Self {
            let home = Home::new(&["wow"]);
            let legacy = CustomRoot::at(home.env.home().join("Games/wow-335"))
                .version(3, 3, 5, 12_340)
                .blizzard_folders(23)
                .realmlist("127.0.0.1")
                .build();
            Self { home, legacy }
        }

        /// `status <args> --json`, asserting the exit code.
        pub fn status(&self, args: &[&str], code: i32) -> Value {
            let mut argv = vec!["status"];
            argv.extend_from_slice(args);
            self.home.run(&argv).expect(code)
        }

        /// Installs a one-folder addon into `target` with a TOC for `interface`.
        pub fn install(&self, target: &str, interface: &str) {
            let dl = self.home.env.root().join("dl");
            fs::create_dir_all(&dl).unwrap();
            let zip = dl.join(format!("Foo-{target}.zip"));
            fs::write(&zip, zip_text(&[("Foo/Foo.toc", &toc(interface, &[]))])).unwrap();
            let path = zip.display().to_string();
            let r = self
                .home
                .run(&["install", "gh:o/foo", "--file", &path, "-t", target]);
            assert_eq!(r.code, 0, "{}", r.context());
        }

        /// Every file under `dir`, hashed; an absent dir hashes as empty.
        pub fn hash(dir: &Path) -> String {
            if dir.exists() {
                tree_hash(dir)
            } else {
                String::new()
            }
        }

        pub fn hashes(&self) -> Vec<String> {
            let env = &self.home.env;
            [
                env.state().join("omaddons"),
                env.cache().join("omaddons"),
                self.home.client("_retail_"),
                self.legacy.clone(),
            ]
            .iter()
            .map(|d| Self::hash(d))
            .collect()
        }
    }

    /// The target of `family` in a status envelope.
    pub fn target<'a>(envelope: &'a Value, family: &str) -> &'a Value {
        envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["family"] == family)
            .unwrap_or(&Value::Null)
    }
}

#[test]
fn two_clients_before_and_after_registration() {
    let m = Machine::new();
    let envelope = m.status(&[], 0);
    let retail = target(&envelope, "mainline");
    assert_eq!(
        (
            &retail["id"],
            &retail["interface"],
            &retail["kind"],
            &retail["install_kind"],
            &retail["key"]
        ),
        (
            &Value::from("retail"),
            &Value::from(120_105),
            &Value::from("live"),
            &Value::from("battlenet"),
            &Value::Null
        ),
        "{envelope}"
    );
    let legacy = target(&envelope, "wrath-legacy");
    assert_eq!(
        (
            &legacy["interface"],
            &legacy["label"],
            &legacy["folders"]["protected"],
            &legacy["key"]
        ),
        (
            &Value::from(30_300),
            &Value::from("localhost"),
            &Value::from(23),
            &Value::Null
        ),
        "{envelope}"
    );
    assert_eq!(envelope["data"]["needs_attention"], false);

    m.install("retail", "120105");
    let legacy_id = legacy["id"].as_str().unwrap().to_owned();
    m.install(&legacy_id, "30300");
    let first = m.status(&[], 0);
    let second = m.status(&[], 0);
    for family in ["mainline", "wrath-legacy"] {
        let key = &target(&first, family)["key"];
        assert!(key.is_string(), "{family}: {first}");
        assert_eq!(key, &target(&second, family)["key"], "{family}: stable");
        assert_eq!(target(&first, family)["addons"]["total"], 1);
    }
}

#[test]
fn status_writes_nothing() {
    use common::addons::addon_folder;
    use common::buggrabber::{BugDb, BugEntry, NOW, TimeKind, Writer, stack_era_c, write_files};

    let m = Machine::new();
    m.install("retail", "120105");
    // Lua errors read and dismissed (the errors memo and the dismissals exist), then a
    // session the memo has not seen: `--cached` is stale, plain `status` reads it live.
    let bug_toc = toc("120105", &["## SavedVariables: BugGrabberDB"]);
    addon_folder(
        &m.home.addons("_retail_"),
        "!BugGrabber",
        &[("!BugGrabber.toc", &bug_toc)],
        &[],
    );
    let entry = |session| {
        let at = stack_era_c("Foo", "a.lua", session);
        BugEntry::new(
            &format!("{at} attempt to call a nil value"),
            session,
            (TimeKind::Unix, NOW - 60),
        )
    };
    let wtf = m.home.client("_retail_").join("WTF");
    write_files(
        &wtf,
        &[BugDb::new(Writer::Modern, 3).entry(entry(3)).file()],
    );
    let clock = [("OMADDONS_NOW", "2026-10-15T18:30:00Z")];
    for args in [&["errors"][..], &["errors", "dismiss", "-t", "retail"]] {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.push("--json".to_owned());
        m.home.exec_with(&argv, &clock).expect(0);
    }
    assert!(m.home.env.cache().join("omaddons/errors.json").is_file());
    assert!(m.home.state().join("errors_ack.json").is_file());
    let next = BugDb::new(Writer::Modern, 4)
        .entry(entry(3))
        .entry(entry(4));
    write_files(&wtf, &[next.file()]);
    // A backup and the projection that adds it up, then one behind its back: `status`
    // reads both and rebuilds neither.
    let retail = m.home.client("_retail_");
    let manual = omaddons::backup::BackupKind::Manual;
    common::backups::seed_backup(&retail, manual, Timestamp::now(), 1);
    m.home.run(&["backup", "list"]).expect(0);
    common::backups::seed_backup(&retail, manual, Timestamp::now(), 1);
    m.status(&[], 0); // warms the discovery cache
    let before = m.hashes();
    let live = m.status(&[], 0);
    assert_eq!(target(&live, "mainline")["errors"]["new"], 1, "{live}");
    let cached = m.status(&["--cached"], 0);
    assert_eq!(target(&cached, "mainline")["errors"]["state"], "stale");
    let backups = &target(&cached, "mainline")["backups"];
    assert_eq!(
        (&backups["count"], &backups["stale"]),
        (&2.into(), &true.into())
    );
    m.status(&["--cached", "--offline"], 0);
    assert_eq!(m.hashes(), before);
}

#[test]
fn cached_never_scans_and_without_a_cache_lists_registered_targets() {
    let m = Machine::new();
    m.install("retail", "120105");
    m.status(&[], 0);
    let r = m.home.run(&["status", "--cached", "-v"]);
    let envelope = r.expect(0);
    assert!(
        r.stderr.contains("discovery: scanned no"),
        "{}",
        r.context()
    );
    assert_eq!(envelope["data"]["cached"], true);
    assert_eq!(
        envelope["data"]["targets"].as_array().unwrap().len(),
        2,
        "both, from the cache"
    );

    let cache = m.home.env.cache().join("omaddons/discovery.json");
    fs::remove_file(&cache).unwrap();
    let r = m.home.run(&["status", "--cached", "-v"]);
    let envelope = r.expect(0);
    assert!(
        r.stderr.contains("discovery: scanned no"),
        "{}",
        r.context()
    );
    let ids: Vec<&str> = envelope["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["retail"], "registered targets only");
    assert!(
        envelope["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["command"] == "omaddons status"),
        "{}",
        r.context()
    );
    assert!(!cache.exists(), "--cached never writes the cache");

    // Nothing registered and no cache yet: no client is known, and the human view still
    // says what to run next.
    let fresh = Machine::new();
    let human = fresh
        .home
        .exec(&["status".to_owned(), "--cached".to_owned()]);
    assert!(
        human.stdout.contains("Next: omaddons status"),
        "{}",
        human.context()
    );
}

#[test]
fn running_state_is_live_under_cached() {
    let mut m = Machine::new();
    m.status(&[], 0);
    assert_eq!(
        target(&m.status(&["--cached"], 0), "mainline")["running"],
        false
    );
    m.home.retail_running();
    assert_eq!(
        target(&m.status(&["--cached"], 0), "mainline")["running"],
        true
    );
    m.home.nothing_running();
    assert_eq!(
        target(&m.status(&["--cached"], 0), "mainline")["running"],
        false
    );
}

#[test]
fn a_leftover_journal_needs_attention_and_a_held_lock_does_not() {
    let m = Machine::new();
    m.install("retail", "120105");
    let key: TargetKey = target(&m.status(&[], 0), "mainline")["key"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let paths = paths(&m.home.env);
    let tx = TxId::new();
    let prepared = Prepared {
        schema_version: journal::CURRENT,
        tx,
        target_key: key,
        client_dir: m.home.client("_retail_"),
        kind: TxKind::Install,
        started_at: Timestamp::now(),
        creates_dirs: Vec::new(),
        writes_wtf: false,
        staged_stamp: None,
        staged_files: None,
        source_backup: None,
        client: None,
        commit: CommitPayload {
            record: TargetRecord::empty(key),
            index: IndexChanges::default(),
            history: HistoryEntry {
                schema_version: history::CURRENT,
                tx,
                kind: TxKind::Install,
                at: Timestamp::now(),
                target: Some(key),
                targets: Vec::new(),
                actor: Actor::Agent,
                reason: None,
                argv: vec!["omaddons".into()],
                ops: Vec::new(),
                result: TxResult::Committed,
                reverts: None,
                sv_deferred: false,
                sv_restored: None,
                created_addons_dir: false,
                run: None,
                backup: None,
                plan: None,
                doctor_run_id: None,
                repairs: Vec::new(),
            },
            stage_dir: m
                .home
                .client("_retail_")
                .join("Interface/.omaddons/stage/x"),
            sv_restore: None,
        },
    };
    fs::create_dir_all(paths.journal_dir()).unwrap();
    let line = serde_json::to_string(&JournalLine::Prepared(Box::new(prepared))).unwrap();
    fs::write(paths.journal(&tx), format!("{line}\n")).unwrap();
    let before = m.hashes();

    let envelope = m.status(&[], 0);
    let retail = target(&envelope, "mainline");
    assert_eq!(
        retail["attention"],
        serde_json::json!(["journal_pending"]),
        "{envelope}"
    );
    assert_eq!(
        envelope["data"]["journal"],
        serde_json::json!({"pending": 1, "in_progress": false})
    );
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "journal_pending")
    );
    // The most urgent issue, and the summary that ranks the machine by it. Its
    // remediation is the doctor's delegation (F2, M4-D53).
    let issue = &retail["issues"][0];
    let fix = serde_json::json!([
        "omaddons",
        "doctor",
        "fix",
        "--only",
        "fm-concurrency_primitives-journal-pending",
        "-t",
        retail["id"]
    ]);
    assert_eq!(
        (
            &issue["id"],
            &issue["severity"],
            &issue["remediation"]["argv"]
        ),
        (
            &Value::from("fm-concurrency_primitives-journal-pending"),
            &Value::from("P0"),
            &fix
        ),
        "{envelope}"
    );
    assert!(issue["remediation"]["command"].is_string());
    let summary = &envelope["data"]["summary"];
    assert_eq!(
        (&summary["state"], &summary["issues"]["p0"]),
        (&Value::from("attention"), &Value::from(1))
    );
    let envelope = m.status(&["--check"], 6);
    assert_eq!(
        (&envelope["ok"], &envelope["error"]["code"]),
        (&Value::Bool(false), &Value::from("check_failed"))
    );
    assert!(
        envelope["error"]["fix"]
            .as_str()
            .is_some_and(|f| !f.is_empty())
    );
    assert!(
        envelope["data"]["targets"]
            .as_array()
            .is_some_and(|t| !t.is_empty()),
        "data populated"
    );

    assert_eq!(m.hashes(), before, "status changed nothing");

    // Another process at work: a warning, not attention, and nothing recovered. (Taking
    // the lock writes its holder into the lock file, so hash after that.)
    let _held = GlobalLock::acquire(&paths, "someone else").unwrap();
    let before = m.hashes();
    let envelope = m.status(&[], 0);
    assert_eq!(
        target(&envelope, "mainline")["attention"],
        serde_json::json!([])
    );
    assert_eq!(envelope["data"]["journal"]["in_progress"], true);
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "tx_in_progress"),
        "{envelope}"
    );
    assert!(paths.journal(&tx).exists());
    assert_eq!(m.hashes(), before, "status changed nothing");
}

/// Stuck journals each get their own issue, title and `doctor explain`, beside a
/// recoverable one's unchanged issue: on retail an unrecognized step, a copy of it a newer
/// omaddons started and a copy naming classic's client dir; on classic a recoverable
/// journal (its install proceeds: retail's journals only warn another client). Retail and
/// classic need a home of their own, so no [`Machine`].
#[test]
fn a_stuck_journal_has_its_own_issue_title_and_remediation() {
    use common::doctor::plant;
    use serde_json::json;

    let home = Home::new(&["wow", "wow_classic"]);
    let (stuck, _) = plant::journal_step_unrecognized(&home, "retail");
    let too_new = plant::copy_journal(&home, &stuck);
    plant::edit_line(&home, &too_new, plant::is_prepared, |line| {
        line["schema_version"] = json!("9.0");
    });
    let mismatch = plant::copy_journal(&home, &stuck);
    let classic_dir = home.client("_classic_").display().to_string();
    plant::edit_line(&home, &mismatch, plant::is_prepared, |line| {
        line["client_dir"] = json!(classic_dir);
    });
    plant::journal_pending(&home, "classic");
    assert_eq!(plant::journals(&home).len(), 4);

    let hashes = || {
        [
            home.state(),
            home.client("_retail_"),
            home.client("_classic_"),
        ]
        .iter()
        .map(|dir| Machine::hash(dir))
        .collect::<Vec<_>>()
    };
    let before = hashes();
    let envelope = home.run(&["status"]).expect(0);
    let by_id = |id: &str| {
        envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()
            .clone()
    };
    let issues = |target: &Value| -> Vec<(String, String, Value)> {
        target["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["id"].as_str().unwrap().to_owned(),
                    i["title"].as_str().unwrap().to_owned(),
                    i["remediation"]["argv"].clone(),
                )
            })
            .collect()
    };
    // A stuck journal's remediation is its forced set-aside (F2).
    let set_aside = |id: &str| {
        json!([
            "omaddons", "doctor", "fix", "--only", id, "-t", "retail", "--force", "--yes"
        ])
    };
    let (retail, classic) = (by_id("retail"), by_id("classic"));
    let mut found = issues(&retail);
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let row = |id: &str, title: &str| (id.to_owned(), title.to_owned(), set_aside(id));
    assert_eq!(
        found,
        [
            row(
                "fm-concurrency_primitives-journal-marker-mismatch",
                "An interrupted change names a client directory that is no longer this client's",
            ),
            row(
                "fm-concurrency_primitives-journal-step-unrecognized",
                "An interrupted change stopped at step 1, which matches neither its before nor its after state",
            ),
            row(
                "fm-concurrency_primitives-journal-too-new",
                "An interrupted change (install) was started by a newer omaddons; this build cannot read its journal (format 9.0)",
            ),
        ],
        "{retail:#}"
    );
    // A journal recovery can settle is the doctor's delegation (F2, M4-D53).
    assert_eq!(
        issues(&classic),
        [(
            "fm-concurrency_primitives-journal-pending".to_owned(),
            "An interrupted change waits to be finished".to_owned(),
            json!([
                "omaddons",
                "doctor",
                "fix",
                "--only",
                "fm-concurrency_primitives-journal-pending",
                "-t",
                "classic"
            ]),
        )],
        "{classic:#}"
    );
    for target in [&retail, &classic] {
        assert_eq!(
            target["attention"],
            json!(["journal_pending"]),
            "{target:#}"
        );
    }
    assert_eq!(
        envelope["data"]["summary"]["issues"]["p0"], 4,
        "{envelope:#}"
    );
    assert_eq!(hashes(), before, "status changed nothing");
}

#[test]
fn a_deleted_client_is_missing() {
    let m = Machine::new();
    let legacy_id = target(&m.status(&[], 0), "wrath-legacy")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    m.install(&legacy_id, "30300");
    fs::remove_dir_all(&m.legacy).unwrap();
    let envelope = m.status(&[], 0);
    let gone = envelope["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == legacy_id.as_str())
        .unwrap();
    assert_eq!(
        (&gone["missing"], &gone["attention"]),
        (&Value::Bool(true), &serde_json::json!(["target_missing"])),
        "{envelope}"
    );
    let rm = ["omaddons", "targets", "rm", legacy_id.as_str()];
    assert!(
        envelope["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["argv"] == serde_json::json!(rm)),
        "{envelope}"
    );
    assert_eq!(
        gone["issues"],
        serde_json::json!([{
            "id": "fm-userland_state-target-vanished",
            "severity": "P2",
            "title": "The client is gone or no longer installed",
            "addon": null,
            "remediation": {
                "command": rm.join(" "),
                "argv": rm,
                "destructive": true
            },
            "auto_fixable": false,
            "explain": {
                "command": format!("omaddons doctor explain fm-userland_state-target-vanished -t {legacy_id}"),
                "argv": ["omaddons", "doctor", "explain", "fm-userland_state-target-vanished", "-t", legacy_id.as_str()]
            }
        }])
    );
    let human = m.home.exec(&["status".to_owned()]);
    assert!(
        human.stdout.contains(&format!(
            "  P2 The client is gone or no longer installed → {}",
            rm.join(" ")
        )),
        "{}",
        human.context()
    );
    let envelope = m.status(&["--check"], 6);
    assert_eq!(envelope["error"]["fix_argv"], serde_json::json!(rm));
}

/// The `target_addons_shared` warnings of a `status` envelope.
fn shared_warnings(envelope: &Value) -> Vec<String> {
    envelope["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == "target_addons_shared")
        .map(|w| w["message"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn clients_sharing_one_addons_directory_are_warned_once() {
    let home = Home::new(&["wow", "wow_classic_era", "wow_anniversary"]);
    let era = home.addons("_classic_era_");
    let anniversary = home.addons("_anniversary_");
    fs::create_dir_all(&era).unwrap();
    if anniversary.exists() {
        fs::remove_dir_all(&anniversary).unwrap();
    }
    fs::create_dir_all(anniversary.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&era, &anniversary).unwrap();

    let r = home.run(&["status"]);
    let envelope = r.expect(0);
    let shared = shared_warnings(&envelope);
    assert_eq!(shared.len(), 1, "{}", r.context());
    let names = ["era and anniversary", "anniversary and era"];
    assert!(
        names.iter().any(|n| shared[0].contains(n)) && !shared[0].contains("retail"),
        "{}",
        shared[0]
    );
    // Every client of the group, typed (omad-5cg5).
    let typed: Vec<&Value> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "target_addons_shared")
        .map(|w| &w["targets"])
        .collect();
    assert_eq!(
        typed,
        [&serde_json::json!(["anniversary", "era"])],
        "{}",
        r.context()
    );
    // Narrowed to a client that shares nothing: no warning.
    let r = home.run(&["status", "-t", "retail"]);
    assert!(shared_warnings(&r.expect(0)).is_empty(), "{}", r.context());
    // Narrowed to one of the two: the warning, which concerns it.
    let r = home.run(&["status", "-t", "anniversary"]);
    assert_eq!(shared_warnings(&r.expect(0)).len(), 1, "{}", r.context());
}

/// Pending updates and adoptable folders (M2), from the release memo and the catalog,
/// never from the network.
#[cfg(test)]
mod updates {
    use std::fs;

    use serde_json::{Value, json};

    use super::common::addons::{addon_folder, toc};
    use super::common::home::Home;
    use super::common::http::Asset;
    use super::common::trees::tree_hash;

    /// Retail and Mists Classic; Foo and Bar v1 installed on retail while v2 of each
    /// is out, the memo aged (past the 6 h check interval).
    pub fn behind() -> Home {
        behind_of(&["Foo", "Bar"])
    }

    /// [`behind`] for `names`; each one ages the memo by a day.
    pub fn behind_of(names: &[&str]) -> Home {
        let mut home = Home::new(&["wow", "wow_classic"]);
        for name in names {
            let base = format!("https://github.com/o/{}", name.to_lowercase());
            let file = format!("{name}.zip");
            let path = format!("{name}/{name}.toc");
            let (t1, t2) = (
                toc("120105", &["## Version: v1"]),
                toc("120105", &["## Version: v2"]),
            );
            let a1: [Asset<'_>; 1] = [(&file, &[], vec![(path.as_str(), t1.as_str())])];
            let a2: [Asset<'_>; 1] = [(&file, &[], vec![(path.as_str(), t2.as_str())])];
            home.installed_behind(&base, ("v1", &a1), ("v2", &a2), &["retail"]);
        }
        home
    }

    /// The bytes of `net.log` (empty when there is none).
    pub fn net_log_bytes(home: &Home) -> Vec<u8> {
        fs::read(home.state().join("net.log")).unwrap_or_default()
    }

    /// `status --cached --json`, asserting it asked nothing.
    pub fn cached(home: &Home) -> Value {
        let before = net_log_bytes(home);
        let envelope = home.run(&["status", "--cached"]).expect(0);
        assert_eq!(net_log_bytes(home), before, "status made a request");
        envelope
    }

    pub fn target<'v>(envelope: &'v Value, id: &str) -> &'v Value {
        envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()
    }

    /// The `(action, argv)` of each recommendation.
    pub fn hints(envelope: &Value) -> Vec<(String, Value)> {
        envelope["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["action"].as_str().unwrap().to_owned(), c["argv"].clone()))
            .collect()
    }

    fn actions(envelope: &Value) -> Vec<String> {
        hints(envelope).into_iter().map(|(a, _)| a).collect()
    }

    #[test]
    fn status_counts_pending_updates_from_the_memo_and_recommends_update() {
        let home = behind();
        home.run(&["outdated"]).expect(0);
        let envelope = cached(&home);
        let retail = &target(&envelope, "retail")["updates"];
        assert_eq!(
            (
                &retail["available"],
                &retail["pinned"],
                &retail["stale"],
                retail["checked_at"].is_string()
            ),
            (&json!(2), &json!(0), &json!(false), true),
            "{envelope}"
        );
        // A target without records: nothing to check, no hint for it.
        let classic = &target(&envelope, "classic")["updates"];
        assert_eq!(
            (
                &classic["available"],
                &classic["checked_at"],
                &classic["stale"]
            ),
            (&json!(0), &Value::Null, &json!(false))
        );
        assert_eq!(
            hints(&envelope),
            [(
                "update".to_owned(),
                json!(["omaddons", "update", "-t", "retail"])
            )],
            "{envelope}"
        );
        // Updates are not attention.
        assert_eq!(envelope["data"]["needs_attention"], false);
        home.run(&["status", "--cached", "--check"]).expect(0);
        let human = home.exec(&["status".to_owned(), "--cached".to_owned()]);
        assert!(human.stdout.contains("2 updates"), "{}", human.stdout);

        home.run(&["update"]).expect(0);
        let envelope = cached(&home);
        assert_eq!(target(&envelope, "retail")["updates"]["available"], 0);
        assert!(!actions(&envelope).contains(&"update".to_owned()));
    }

    #[test]
    fn a_stale_or_missing_memo_recommends_a_check() {
        let home = behind_of(&["Foo"]);
        // A day old, past the 6 h default.
        let envelope = cached(&home);
        assert_eq!(target(&envelope, "retail")["updates"]["stale"], true);
        assert!(actions(&envelope).contains(&"check_updates".to_owned()));
        let (_, argv) = hints(&envelope)
            .into_iter()
            .find(|(a, _)| a == "check_updates")
            .unwrap();
        assert_eq!(argv, json!(["omaddons", "outdated"]));

        // Two days is the threshold now.
        let config = home.env.config().join("omaddons/config.toml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "check_interval_hours = 48\n").unwrap();
        let envelope = cached(&home);
        assert_eq!(target(&envelope, "retail")["updates"]["stale"], false);
        assert!(!actions(&envelope).contains(&"check_updates".to_owned()));

        // No memo at all: never checked.
        fs::remove_file(home.memo()).unwrap();
        let envelope = cached(&home);
        let retail = &target(&envelope, "retail")["updates"];
        assert_eq!(
            (
                &retail["checked_at"],
                &retail["stale"],
                &retail["unchecked"]
            ),
            (&Value::Null, &json!(false), &json!(1))
        );
        assert_eq!(
            actions(&envelope)
                .iter()
                .filter(|a| *a == "check_updates")
                .count(),
            1
        );
    }

    #[test]
    fn static_addons_need_no_check() {
        let home = Home::new(&["wow"]);
        let dir = home.env.root().join("MyAddon");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("MyAddon.toc"), toc("120105", &[])).unwrap();
        let path = dir.display().to_string();
        home.run(&["install", &path, "-t", "retail"]).expect(0);
        let envelope = cached(&home);
        let retail = &target(&envelope, "retail")["updates"];
        assert_eq!(
            (
                &retail["checked_at"],
                &retail["stale"],
                &retail["unchecked"]
            ),
            (&Value::Null, &json!(false), &json!(0))
        );
        assert!(!actions(&envelope).contains(&"check_updates".to_owned()));
    }

    #[test]
    fn adoptable_counts_catalog_folders_not_installed_here() {
        let home = behind();
        // Foo's catalog entry also names Foo_Extra; WeakAuras is in the built-in catalog.
        let catalog_d = home.env.config().join("omaddons/catalog.d");
        fs::create_dir_all(&catalog_d).unwrap();
        fs::write(
            catalog_d.join("50-foo.toml"),
            "schema_version = \"1.0\"\n\n[[addon]]\nname = \"Foo\"\nref = \"gh:o/foo\"\nfolders = [\"Foo\", \"Foo_Extra\"]\n",
        )
        .unwrap();
        let addons = home.addons("_retail_");
        for name in ["WeakAuras", "Foo_Extra", "Random"] {
            let file = format!("{name}.toc");
            addon_folder(&addons, name, &[(&file, &toc("120105", &[]))], &[]);
        }
        let envelope = cached(&home);
        let retail = target(&envelope, "retail");
        // WeakAuras only: Foo_Extra is a managed addon's leftover, Random unknown.
        assert_eq!(
            (&retail["adoptable"], &retail["folders"]["unmanaged"]),
            (&json!(1), &json!(3)),
            "{envelope}"
        );
        // Recommendations in order: adopt (instead of a look at unmanaged folders), then
        // the check.
        let order = actions(&envelope);
        assert_eq!(order, ["adopt", "check_updates"], "{envelope}");
        let adopt = &envelope["commands"][0]["argv"];
        assert_eq!(adopt, &json!(["omaddons", "adopt", "-t", "retail"]));
        assert_eq!(target(&envelope, "classic")["adoptable"], 0);
        // The hint runs: a report, which changes nothing.
        let tree = tree_hash(&addons);
        let r = home.run(&["adopt", "-t", "retail"]);
        r.expect(0);
        assert_eq!(tree_hash(&addons), tree, "{}", r.context());

        // Unknown folders alone: no adopt, the plain look.
        fs::remove_dir_all(addons.join("WeakAuras")).unwrap();
        let envelope = cached(&home);
        assert_eq!(
            actions(&envelope),
            ["check_updates", "list_unmanaged"],
            "{envelope}"
        );
    }

    /// omad-7mw6: with no `catalog.d` file, the built-in catalog's index counts them, as
    /// the parsed catalog would: each folder of a built-in entry, whatever its case, and
    /// never a folder no entry names; `status` and `status --cached` agree.
    #[test]
    fn adoptable_counts_builtin_folders_without_an_override() {
        let home = behind();
        assert!(!home.env.config().join("omaddons/catalog.d").exists());
        let addons = home.addons("_retail_");
        for name in ["WeakAuras", "weakaurasoptions", "Random"] {
            let file = format!("{name}.toc");
            addon_folder(&addons, name, &[(&file, &toc("120105", &[]))], &[]);
        }
        for envelope in [cached(&home), home.run(&["status", "--json"]).expect(0)] {
            let retail = target(&envelope, "retail");
            assert_eq!(
                (&retail["adoptable"], &retail["folders"]["unmanaged"]),
                (&json!(2), &json!(3)),
                "{envelope}"
            );
        }
    }

    /// omad-3oru: a folder whose own TOC identifies it as adoption would (an `X-Website`
    /// naming a source, a WoWInterface id, a built-in Tukui id) counts, offline and in
    /// `--cached` too; one naming an addon installed here, a CurseForge id alone or no
    /// id at all never does. So `status` recommends the take-over exactly when there is
    /// something to take over.
    #[test]
    fn adoptable_counts_folders_their_own_toc_identifies() {
        let home = behind();
        let addons = home.addons("_retail_");
        for (name, line) in [
            ("Baz", "## X-Website: https://github.com/o/baz"),
            ("Qux", "## X-WoWI-ID: 4242"),
            ("ElvUI", "## X-Tukui-ProjectID: -2"),
            ("FooCopy", "## X-Website: https://github.com/o/foo"),
            ("Curse", "## X-Curse-Project-ID: 12345"),
            ("Random", "## Title: Random"),
        ] {
            let file = format!("{name}.toc");
            addon_folder(&addons, name, &[(&file, &toc("120105", &[line]))], &[]);
        }
        for envelope in [cached(&home), home.run(&["status", "--json"]).expect(0)] {
            let retail = target(&envelope, "retail");
            // Baz, Qux and ElvUI: FooCopy is Foo's, which is installed here.
            assert_eq!(
                (&retail["adoptable"], &retail["folders"]["unmanaged"]),
                (&json!(3), &json!(6)),
                "{envelope}"
            );
            assert!(
                actions(&envelope).contains(&"adopt".to_owned()),
                "{envelope}"
            );
            assert!(
                !actions(&envelope).contains(&"list_unmanaged".to_owned()),
                "{envelope}"
            );
        }
        // Only what nothing identifies: the plain look, no take-over.
        for name in ["Baz", "Qux", "ElvUI"] {
            fs::remove_dir_all(addons.join(name)).unwrap();
        }
        let envelope = cached(&home);
        assert_eq!(target(&envelope, "retail")["adoptable"], 0, "{envelope}");
        assert!(
            actions(&envelope).contains(&"list_unmanaged".to_owned()),
            "{envelope}"
        );
        assert!(
            !actions(&envelope).contains(&"adopt".to_owned()),
            "{envelope}"
        );
    }

    #[test]
    fn list_shows_the_latest_known_release_from_the_memo() {
        let home = behind();
        home.run(&["outdated"]).expect(0);
        let before = net_log_bytes(&home);
        let envelope = home.run(&["list", "-t", "retail"]).expect(0);
        assert_eq!(net_log_bytes(&home), before, "list made a request");
        let foo = envelope["data"]["targets"][0]["addons"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == "Foo")
            .unwrap()
            .clone();
        assert_eq!(
            foo["latest"],
            json!({
                "version": "v2",
                "status": "update_available",
                "source": "github",
                "release_id": "gh:o/foo@v2",
                "asset": "Foo.zip",
                "published_at": null,
            })
        );
        home.run(&["pin", "foo", "-t", "retail"]).expect(0);
        let envelope = home.run(&["list", "-t", "retail"]).expect(0);
        let foo = &envelope["data"]["targets"][0]["addons"][1];
        assert_eq!(
            (&foo["name"], &foo["latest"]["status"]),
            (&json!("Foo"), &json!("pinned"))
        );
    }
}

/// `last_change`: what bare `undo` would revert, from the whole history (plain `status`)
/// or the history projection (`--cached`, M3-D37).
mod last_change {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::common::addons::toc;
    use super::common::home::Home;
    use super::common::http::Asset;
    use super::common::wtf::Wtf;

    /// Retail and Mists Classic with Foo v1 on both while v2 is out.
    fn behind_on_two() -> Home {
        let mut home = Home::new(&["wow", "wow_classic"]);
        let (t1, t2) = (
            toc("50504, 120105", &["## Version: v1"]),
            toc("50504, 120105", &["## Version: v2"]),
        );
        let a1: [Asset<'_>; 1] = [("Foo.zip", &[], vec![("Foo/Foo.toc", t1.as_str())])];
        let a2: [Asset<'_>; 1] = [("Foo.zip", &[], vec![("Foo/Foo.toc", t2.as_str())])];
        home.installed_behind(
            "https://github.com/o/foo",
            ("v1", &a1),
            ("v2", &a2),
            &["retail", "classic"],
        );
        home
    }

    fn projection(home: &Home) -> PathBuf {
        home.env.cache().join("omaddons/history_tail.json")
    }

    fn history(home: &Home) -> PathBuf {
        home.state().join("history.jsonl")
    }

    /// `status <args>`'s `(last_change, last_change_state)`.
    fn last(home: &Home, args: &[&str]) -> (Value, Value) {
        let mut argv = vec!["status"];
        argv.extend_from_slice(args);
        let envelope = home.run(&argv).expect(0);
        (
            envelope["data"]["last_change"].clone(),
            envelope["data"]["last_change_state"].clone(),
        )
    }

    #[test]
    fn an_update_on_two_clients_then_its_undo() {
        let home = behind_on_two();
        let update = home.run(&["update"]).expect(0);
        let run = update["data"]["run"].as_str().unwrap().to_owned();

        let (change, state) = last(&home, &[]);
        assert_eq!(state, "ok");
        assert_eq!(
            (
                &change["run"],
                &change["kind"],
                &change["targets"],
                &change["changes"]["replaced"]
            ),
            (
                &json!(run),
                &json!("update"),
                &json!(["classic", "retail"]),
                &json!(2)
            ),
            "{change}"
        );
        assert_eq!(
            change["undo"]["argv"],
            json!(["omaddons", "undo", "--run", run])
        );
        assert_eq!(
            last(&home, &["--cached"]),
            (change.clone(), json!("ok")),
            "the projection answers what the whole history does"
        );

        // Another run commits; the command shown still reverts the update, by its run.
        let shown: Vec<String> = serde_json::from_value(change["undo"]["argv"].clone()).unwrap();
        let bar = home.env.root().join("Bar");
        fs::create_dir_all(&bar).unwrap();
        fs::write(bar.join("Bar.toc"), toc("120105", &[])).unwrap();
        let path = bar.display().to_string();
        home.run(&["install", &path, "-t", "retail"]).expect(0);
        assert_eq!(last(&home, &["--cached"]).0["kind"], "install");
        let args: Vec<&str> = shown[1..].iter().map(String::as_str).collect();
        let undo = home.run(&args).expect(0);
        assert_eq!(undo["data"]["reverted_run"], run.as_str());
        let statuses: Vec<&Value> = undo["data"]["transactions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| &t["status"])
            .collect();
        assert_eq!(statuses, [&json!("undone"); 2]);
        let undo_run = undo["data"]["run"].as_str().unwrap().to_owned();
        let (change, state) = last(&home, &["--cached"]);
        assert_eq!(state, "ok");
        assert_eq!(
            (&change["run"], &change["kind"], &change["targets"]),
            (
                &json!(undo_run),
                &json!("undo"),
                &json!(["classic", "retail"])
            ),
            "{change}"
        );
        assert_eq!(last(&home, &[]).0, change);
        let human = home.exec(&["status".to_owned(), "--cached".to_owned()]);
        assert!(
            human
                .stdout
                .contains("last change: undo on classic, retail by ")
                && human
                    .stdout
                    .contains(&format!("· undo: omaddons undo --run {undo_run}")),
            "{}",
            human.context()
        );
    }

    #[test]
    fn a_projection_deleted_corrupt_or_bypassed_is_unknown_until_the_next_change() {
        let home = behind_on_two();
        let installed = last(&home, &[]);
        assert_eq!(last(&home, &["--cached"]), installed);
        let unknown = (Value::Null, json!("unknown"));

        // Deleted: unknown, and status builds nothing.
        fs::remove_file(projection(&home)).unwrap();
        assert_eq!(last(&home, &["--cached"]), unknown);
        assert!(!projection(&home).exists(), "status writes nothing");
        assert_eq!(
            last(&home, &[]),
            installed,
            "plain status reads the history"
        );
        let human = home.exec(&["status".to_owned(), "--cached".to_owned()]);
        assert!(
            human
                .stdout
                .contains("last change: unknown (run omaddons status for it)"),
            "{}",
            human.context()
        );
        // A settings change rebuilds it.
        home.run(&["config", "set", "check_interval_hours", "12"])
            .expect(0);
        assert_eq!(last(&home, &["--cached"]), installed);

        // Corrupt.
        fs::write(projection(&home), b"{ not json").unwrap();
        assert_eq!(last(&home, &["--cached"]), unknown);
        home.run(&["config", "set", "check_interval_hours", "6"])
            .expect(0);
        assert_eq!(last(&home, &["--cached"]), installed);

        // A line written around omaddons.
        let text = fs::read_to_string(history(&home)).unwrap();
        let first = text.lines().next().unwrap().to_owned();
        fs::write(history(&home), format!("{text}{first}\n")).unwrap();
        assert_eq!(last(&home, &["--cached"]), unknown);
        home.run(&["update"]).expect(0);
        let updated = last(&home, &[]);
        assert_eq!(updated.0["kind"], "update");
        assert_eq!(last(&home, &["--cached"]), updated);
    }

    #[test]
    fn backups_and_settings_changes_never_push_the_last_change_out() {
        let home = behind_on_two();
        Wtf::at(home.client("_retail_"))
            .saved_variables("Foo", 64)
            .build();
        home.run(&["update"]).expect(0);
        let updated = last(&home, &[]);
        assert_eq!(updated.0["kind"], "update");
        let env = [("OMADDONS_FS_FREE_MB", "1024")];
        for _ in 0..40 {
            let argv = ["backup", "create", "-t", "retail", "--json"].map(str::to_owned);
            let r = home.exec_with(&argv, &env);
            assert_eq!(r.code, 0, "{}", r.context());
        }
        for i in 0..20 {
            let hours = (i + 1).to_string();
            home.run(&["config", "set", "check_interval_hours", &hours])
                .expect(0);
        }
        let lines = fs::read_to_string(history(&home)).unwrap().lines().count();
        assert!(lines >= 62, "{lines} history lines");
        assert_eq!(last(&home, &["--cached"]), updated);
    }

    #[test]
    fn two_runs_whose_lines_alternate_give_what_bare_undo_selects() {
        let home = behind_on_two();
        let update = home.run(&["update"]).expect(0);
        let run = update["data"]["run"].as_str().unwrap().to_owned();
        // Put the installs' lines (one run each) between the update's two lines, as two
        // terminals writing at once would: install, update, install, update, per client.
        let text = fs::read_to_string(history(&home)).unwrap();
        let line = |kind: &str, n: usize| -> String {
            text.lines()
                .filter(|l| serde_json::from_str::<Value>(l).unwrap()["kind"] == kind)
                .nth(n)
                .unwrap()
                .to_owned()
        };
        let moved = [
            line("install", 0),
            line("update", 0),
            line("install", 1),
            line("update", 1),
        ];
        let mut rest: Vec<&str> = text
            .lines()
            .filter(|l| !moved.iter().any(|m| m == l))
            .collect();
        rest.extend(moved.iter().map(String::as_str));
        fs::write(history(&home), rest.join("\n") + "\n").unwrap();
        // The next change rebuilds the projection the rewrite made stale.
        home.run(&["config", "set", "check_interval_hours", "12"])
            .expect(0);

        let (change, state) = last(&home, &[]);
        assert_eq!(state, "ok");
        assert_eq!(
            (
                &change["run"],
                &change["targets"],
                &change["changes"]["replaced"]
            ),
            (&json!(run), &json!(["classic", "retail"]), &json!(2)),
            "{change}"
        );
        assert_eq!(last(&home, &["--cached"]), (change, json!("ok")));
        let r = home.run(&["undo", "--dry-run"]);
        let preview = r.expect(0);
        assert_eq!(
            preview["data"]["reverted_run"],
            run.as_str(),
            "{}",
            r.context()
        );
        let targets: Vec<&Value> = preview["data"]["transactions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| &t["target"]["id"])
            .collect();
        assert_eq!(targets.len(), 2, "{preview}");
    }

    #[test]
    fn a_fresh_home_has_no_last_change() {
        let home = Home::new(&["wow"]);
        let nothing = (Value::Null, json!("ok"));
        assert_eq!(last(&home, &[]), nothing);
        assert_eq!(last(&home, &["--cached"]), nothing);
        let envelope = home.run(&["status"]).expect(0);
        assert_eq!(envelope["data"]["summary"]["state"], "idle");
        let human = home.exec(&["status".to_owned()]);
        assert!(
            human.stdout.starts_with("Nothing needs doing\n"),
            "{}",
            human.context()
        );
        assert!(!human.stdout.contains("last change"), "{}", human.stdout);
    }
}

/// Backups in `status` (M3-D37, M3-D30, M3-D31, M3-D48): each client's block from the
/// inventory projection, fresh, stale or missing; what automatic attempts recorded; the
/// free space; the `backup_create` and `refresh_backups` recommendations; the human line.
/// Every run is checked for the fake account, realm and character names on both streams.
#[cfg(test)]
mod backups {
    use super::*;
    use common::backups::{meta, seed_backup, seed_meta};
    use common::home::Run;
    use common::privacy::assert_private_all;
    use common::wtf::Wtf;
    use jiff::SignedDuration;
    use omaddons::backup::{BackupKind, BackupMeta, BackupSource, Consistency, ImportSource};
    use omaddons::model::BackupId;
    use omaddons::rules::Family;
    use serde_json::json;

    /// A registered retail client whose `WTF/` holds SavedVariables.
    fn played() -> Home {
        let home = Home::new(&["wow"]);
        Wtf::at(home.client("_retail_"))
            .saved_variables("Foo", 100)
            .build();
        home.run(&["targets", "default", "retail"]).expect(0);
        home
    }

    /// `hours` before now.
    fn ago(hours: i64) -> Timestamp {
        Timestamp::now() - SignedDuration::from_hours(hours)
    }

    /// `omaddons <args>` with `extra` set, checked for privacy.
    fn run(home: &Home, args: &[&str], extra: &[(&str, &str)]) -> Run {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let r = home.exec_with(&argv, extra);
        assert_private_all(&r.stdout, &r.stderr);
        r
    }

    /// `status --json <args>`'s envelope.
    fn status(home: &Home, args: &[&str]) -> Value {
        let mut argv = vec!["status", "--json"];
        argv.extend_from_slice(args);
        run(home, &argv, &[]).expect(0)
    }

    /// Retail's backups block.
    fn block(envelope: &Value) -> Value {
        target(envelope, "mainline")["backups"].clone()
    }

    /// The recommendations' actions and argvs.
    fn actions(envelope: &Value) -> Vec<(String, Value)> {
        envelope["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["action"].as_str().unwrap().to_owned(), c["argv"].clone()))
            .collect()
    }

    fn has(envelope: &Value, action: &str) -> bool {
        actions(envelope).iter().any(|(a, _)| a == action)
    }

    fn projection(home: &Home) -> PathBuf {
        home.env.cache().join("omaddons/backups.json")
    }

    /// The projection with retail's `auto` replaced by `auto`.
    fn record_auto(home: &Home, auto: &Value) {
        let path = projection(home);
        let mut file: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for (_, t) in file["targets"].as_object_mut().unwrap() {
            t["auto"] = auto.clone();
        }
        fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    }

    #[test]
    fn four_backups_add_up_from_the_projection() {
        let home = played();
        let retail = home.client("_retail_");
        seed_backup(&retail, BackupKind::Manual, ago(72), 3);
        seed_backup(&retail, BackupKind::BeforeRestore, ago(48), 2);
        let imported = BackupMeta {
            source: Some(import_source()),
            ..meta(BackupKind::Imported, ago(30), 4)
        };
        seed_meta(&retail, &imported);
        let newest = ago(1);
        let auto = seed_backup(&retail, BackupKind::Auto, newest, 1);
        run(&home, &["backup", "list", "--json"], &[]).expect(0);
        for args in [&[][..], &["--cached"]] {
            let envelope = status(&home, args);
            assert_eq!(
                block(&envelope),
                json!({
                    "count": 4,
                    "stale": false,
                    "latest": {"id": auto.to_string(), "captured_at": newest.to_string(),
                        "kind": "auto", "trigger": "post_session"},
                    "latest_qualifying": {"id": auto.to_string(),
                        "captured_at": newest.to_string()},
                    "auto": {"mode": "daily", "capability": "unsupported", "last": null},
                    "bytes": 1000,
                    "snapshot_bytes": 0,
                    "free_bytes": 65_536u64 * 1024 * 1024,
                }),
                "{args:?}"
            );
            assert!(!has(&envelope, "backup_create"), "{envelope:#}");
            assert!(!has(&envelope, "refresh_backups"), "{envelope:#}");
        }
        let free = run(
            &home,
            &["status", "--json", "--cached"],
            &[("OMADDONS_FS_FREE_MB", "1234")],
        )
        .expect(0);
        assert_eq!(block(&free)["free_bytes"], 1234u64 * 1024 * 1024);
    }

    #[test]
    fn a_backup_behind_the_projections_back_makes_it_stale_until_listed() {
        let home = played();
        let retail = home.client("_retail_");
        seed_backup(&retail, BackupKind::Manual, ago(2), 1);
        run(&home, &["backup", "list", "--json"], &[]).expect(0);
        seed_backup(&retail, BackupKind::Manual, ago(1), 1);
        let envelope = status(&home, &["--cached"]);
        let b = block(&envelope);
        assert_eq!(
            (&b["stale"], &b["count"]),
            (&json!(true), &json!(2)),
            "{b:#}"
        );
        assert!(
            actions(&envelope).contains(&(
                "refresh_backups".to_owned(),
                json!(["omaddons", "backup", "list", "-t", "retail"])
            )),
            "{envelope:#}"
        );
        let human = run(&home, &["status", "--cached"], &[]);
        assert!(
            human.stdout.contains("details out of date"),
            "{}",
            human.stdout
        );
        run(&home, &["backup", "list", "-t", "retail", "--json"], &[]).expect(0);
        let envelope = status(&home, &["--cached"]);
        assert_eq!(block(&envelope)["stale"], false, "{envelope:#}");
        assert!(!has(&envelope, "refresh_backups"), "{envelope:#}");
    }

    #[test]
    fn without_a_projection_only_the_count_is_known_and_nothing_is_written() {
        let home = played();
        let retail = home.client("_retail_");
        seed_backup(&retail, BackupKind::Manual, ago(1), 1);
        seed_backup(&retail, BackupKind::Manual, ago(2), 1);
        assert!(!projection(&home).exists());
        let before = [
            tree_hash(&home.env.cache()),
            tree_hash(&home.env.state()),
            tree_hash(&retail),
        ];
        let envelope = status(&home, &["--cached"]);
        assert_eq!(
            block(&envelope),
            json!({
                "count": 2, "stale": true, "latest": null, "latest_qualifying": null,
                "auto": {"mode": "daily", "capability": "unsupported", "last": null},
                "bytes": null, "snapshot_bytes": null,
                "free_bytes": 65_536u64 * 1024 * 1024,
            })
        );
        // Whether a checkpoint exists is not known: the refresh comes first.
        assert!(has(&envelope, "refresh_backups"), "{envelope:#}");
        assert!(!has(&envelope, "backup_create"), "{envelope:#}");
        status(&home, &[]);
        assert!(!projection(&home).exists(), "status never rebuilds it");
        let after = [
            tree_hash(&home.env.cache()),
            tree_hash(&home.env.state()),
            tree_hash(&retail),
        ];
        assert_eq!(before, after);
    }

    #[test]
    fn what_automatic_attempts_recorded_is_reported_as_is() {
        let home = played();
        let retail = home.client("_retail_");
        seed_backup(&retail, BackupKind::Manual, ago(1), 1);
        run(&home, &["backup", "list", "--json"], &[]).expect(0);
        let at = ago(3).to_string();
        // The filesystem hint says it cannot clone; what an attempt observed wins.
        for (capability, result, line) in [
            ("supported", "created", "auto: daily, working"),
            (
                "unsupported",
                "no_reflink",
                "auto: daily, but this filesystem cannot clone; use daily-copy or back up by hand",
            ),
            (
                "supported",
                "space_low",
                "auto: daily, but space ran low (limits.backup_min_free_mb)",
            ),
        ] {
            let recorded = json!({"capability": capability,
                "last": {"at": at, "result": result}});
            record_auto(&home, &recorded);
            let envelope = status(&home, &["--cached"]);
            assert_eq!(
                block(&envelope)["auto"],
                json!({"mode": "daily", "capability": capability,
                    "last": {"at": at, "result": result}}),
                "{result}"
            );
            assert_eq!(
                block(&envelope)["stale"],
                false,
                "{result}: auto is not covers"
            );
            let human = run(&home, &["status", "--cached"], &[]);
            assert!(human.stdout.contains(line), "{result}: {}", human.stdout);
        }
        run(
            &home,
            &["config", "set", "auto_backup", "off", "--json"],
            &[],
        )
        .expect(0);
        // The backup is an hour old: "today" or, in the day's first hour, a date.
        let human = run(&home, &["status"], &[]);
        assert!(
            human.stdout.contains("  backups: 1 (newest ")
                && human
                    .stdout
                    .contains(", manual) · auto: off · 68.7 GB free"),
            "{}",
            human.stdout
        );
    }

    #[test]
    fn backup_create_is_recommended_when_no_recent_capture_qualifies() {
        let home = played();
        let retail = home.client("_retail_");
        run(
            &home,
            &["config", "set", "auto_backup", "off", "--json"],
            &[],
        )
        .expect(0);
        // Nothing yet: recommended, whatever the mode.
        let envelope = status(&home, &["--cached"]);
        let create = (
            "backup_create".to_owned(),
            json!(["omaddons", "backup", "create", "-t", "retail"]),
        );
        assert!(actions(&envelope).contains(&create), "{envelope:#}");
        // A week-old checkpoint, then captures today that are no checkpoint: an import of
        // an old archive, a recovered partial backup, a best-effort capture.
        seed_backup(&retail, BackupKind::Manual, ago(24 * 8), 1);
        let today: [BackupMeta; 3] = [
            BackupMeta {
                source: Some(import_source()),
                ..meta(BackupKind::Imported, ago(24 * 30), 1)
            },
            BackupMeta {
                partial: true,
                ..meta(BackupKind::Recovered, ago(1), 1)
            },
            BackupMeta {
                consistency: Consistency::BestEffort,
                ..meta(BackupKind::Manual, ago(1), 1)
            },
        ];
        for seeded in &today {
            seed_meta(&retail, seeded);
            run(&home, &["backup", "list", "--json"], &[]).expect(0);
            let envelope = status(&home, &["--cached"]);
            assert!(
                actions(&envelope).contains(&create),
                "{:?}: {envelope:#}",
                seeded.kind
            );
            assert!(!has(&envelope, "refresh_backups"), "{envelope:#}");
        }
        // A checkpoint today silences it.
        run(&home, &["backup", "create", "-t", "retail", "--json"], &[]).expect(0);
        let envelope = status(&home, &["--cached"]);
        assert!(!has(&envelope, "backup_create"), "{envelope:#}");
    }

    #[test]
    fn a_client_without_saved_variables_is_not_asked_to_back_up() {
        let home = Home::new(&["wow"]);
        home.run(&["targets", "default", "retail"]).expect(0);
        let envelope = status(&home, &["--cached"]);
        let b = block(&envelope);
        assert_eq!((&b["count"], &b["stale"]), (&json!(0), &json!(false)));
        assert!(!has(&envelope, "backup_create"), "{envelope:#}");
        let human = run(&home, &["status"], &[]);
        assert!(
            human.stdout.contains(
                "  backups: none · auto: daily, but this filesystem cannot clone; use daily-copy or back up by hand · 68.7 GB free"
            ),
            "{}",
            human.stdout
        );
    }

    fn import_source() -> BackupSource {
        BackupSource::Import(ImportSource {
            id: BackupId::new(),
            target_id: "retail".to_owned(),
            family: Family::Mainline,
            created_at: ago(24 * 30),
            captured_at: ago(24 * 30),
            tool_version: "0.1.0".to_owned(),
        })
    }
}
