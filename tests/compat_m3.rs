//! e2e suite: the M3 build (c0ab5e9) against M4 state, checkpoint only.
//!
//! Checkpoint criterion 16 (M4-D58): the last M3 commit, `c0ab5e9`, built with the
//! toolchain it records (`1.97.1`), must never delete or overwrite anything M4 wrote that
//! M4 still needs: repair journals and their halves, quarantines, run directories, stored
//! plans, suspended sets and provisional journals. M3 learned this from the M2 build
//! (`tests/compat_m2.rs`), which deleted M3's journals unread until their names changed.
//! Each scene is built fresh per probe (identities are part of every repair predicate, so
//! a copied home would prove nothing), the M3 build runs its command, and the row records
//! what it did; the table is printed for README. Never a skip: a build that cannot be had
//! fails, since compiler inconvenience is not evidence that storage is safe.
//!
//! Ignored by default (it builds an old commit). Run with a debug build of this test,
//! whose failpoints crash the M4 scenes:
//! `cargo test --test compat_m3 -- --ignored --nocapture`
#![cfg(debug_assertions)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::compat::{M3_SHORT, m3};
use common::home::Home;
use common::tlog::{TestLog, tlog};
use common::trees::tree_hash;
use serde_json::{Value, json};
use support::{INSTALL, INSTALL_AGED, Scene, assert_rows, local, run_probe};

// The probes and helpers every family's scenes share (5.9.2 to 5.9.4 append theirs).
#[cfg(test)]
#[allow(dead_code)]
mod support {
    use super::*;
    use common::addons::{addon_folder, toc};
    use common::compat::{
        FolderBytes, M4Inventory, OldRunner, Row, age, clients, describe_lost, history_prefix_kept,
        meta, print_table, rel, stamped_pre_paths, table,
    };

    /// Where a probe's argv names the probe folder.
    pub const PROBE: &str = "<probe>";

    /// One M3 command, or a short sequence of them, on a fresh replica.
    pub struct Probe {
        pub name: &'static str,
        /// Whether two hours pass first ([`age`]).
        pub aged: bool,
        pub argv: &'static [&'static [&'static str]],
    }

    const STATUS: &[&str] = &["status", "--json", "--progress", "json"];
    const LIST: &[&str] = &["list", "--json"];
    const HISTORY: &[&str] = &["history", "--json"];
    const INSTALL_PROBE: &[&str] = &[
        "install",
        PROBE,
        "-t",
        "retail",
        "--json",
        "--progress",
        "json",
    ];
    const UNDO: &[&str] = &["undo", "--json", "--progress", "json"];
    const CONFIG_SET: &[&str] = &[
        "config",
        "set",
        "check_interval_hours",
        "6",
        "--json",
        "--progress",
        "json",
    ];

    pub const INSTALL: Probe = Probe {
        name: "install",
        aged: false,
        argv: &[INSTALL_PROBE],
    };
    pub const INSTALL_AGED: Probe = Probe {
        name: "install",
        aged: true,
        argv: &[INSTALL_PROBE],
    };

    /// The leaf's probes: readers then install at once; install two hours on; a bare
    /// undo (what an agent guesses first) and `config set`, each at once and two hours on.
    pub const PROBES: &[Probe] = &[
        Probe {
            name: "readers, then install",
            aged: false,
            argv: &[STATUS, LIST, HISTORY, INSTALL_PROBE],
        },
        INSTALL_AGED,
        Probe {
            name: "undo",
            aged: false,
            argv: &[UNDO],
        },
        Probe {
            name: "undo",
            aged: true,
            argv: &[UNDO],
        },
        Probe {
            name: "config set",
            aged: false,
            argv: &[CONFIG_SET],
        },
        Probe {
            name: "config set",
            aged: true,
            argv: &[CONFIG_SET],
        },
    ];

    /// What a scene's M4 check after the M3 command found: its account, or why it fails.
    pub type Next = fn(&TestLog, &Scene) -> Result<String, String>;

    /// `Interface/AddOns`' top-level entries with their tree hashes, as a scene expects
    /// them.
    pub type AddonsAfter = fn(&Home) -> Vec<(String, String)>;

    /// A home a scene built with the M4 build, and what its rows need.
    pub struct Scene {
        pub home: Home,
        pub family: &'static str,
        pub name: String,
        pub position: String,
        /// The folder transaction the M3 build may recover (only the rows that allow
        /// it); any other recovery by the M3 build fails the row.
        pub expect_m3_recovery: Option<String>,
        /// `Interface/AddOns`' top-level entries with their tree hashes after that
        /// recovery and the M3 command.
        pub addons_after_m3_recovery: Option<AddonsAfter>,
        /// A committed repair's subjects, which M4's undo needs as the repair left them.
        pub subjects: Vec<PathBuf>,
        /// The family's own checks after the M3 command, beyond `status` and `doctor`.
        pub next: Option<Next>,
    }

    /// A local addon folder `name` under `<TestEnv root>/src/`, installable by either
    /// build.
    pub fn local(home: &Home, name: &str) -> String {
        let src = home.env.root().join("src");
        addon_folder(
            &src,
            name,
            &[(&format!("{name}.toc"), &toc("120105", &[]))],
            &[],
        )
        .display()
        .to_string()
    }

    /// The probe folder `M3Probe`, made once per replica.
    pub fn probe_folder(home: &Home) -> String {
        let dir = home.env.root().join("src/M3Probe");
        if dir.exists() {
            return dir.display().to_string();
        }
        local(home, "M3Probe")
    }

    /// The command as a person would type it: `omaddons <argv>`, the probe folder as
    /// `<probe>`.
    fn typed(argv: &[&str]) -> String {
        format!("omaddons {}", argv.join(" "))
    }

    /// The trees the row's "everything else" is read from.
    fn watched(home: &Home) -> Vec<(String, PathBuf)> {
        let mut out = vec![
            ("state".to_owned(), home.env.state()),
            ("cache".to_owned(), home.env.cache()),
            ("config".to_owned(), home.env.config()),
        ];
        for client in clients(home) {
            out.push((rel(home, &client), client));
        }
        out.retain(|(_, dir)| dir.exists());
        out
    }

    /// Whether `path` is one of the folder transaction `tx`'s own artifacts: its journal,
    /// in-flight lock, stage, or snapshot halves (either name).
    fn own_artifact(home: &Home, tx: &str, path: &Path) -> bool {
        let state = home.state();
        let mut roots = vec![
            state.join("journal"),
            state.join("inflight"),
            state.join("snapshots"),
        ];
        for client in clients(home) {
            roots.push(meta(&client).join("stage"));
            roots.push(meta(&client).join("snapshots"));
        }
        roots.iter().any(|root| {
            path.strip_prefix(root).ok().is_some_and(|rest| {
                rest.components()
                    .next()
                    .is_some_and(|c| c.as_os_str().to_string_lossy().starts_with(tx))
            })
        })
    }

    /// Whether `path` lies in `tx`'s own stage on some client.
    fn in_own_stage(home: &Home, tx: &str, path: &Path) -> bool {
        clients(home)
            .iter()
            .any(|c| path.starts_with(meta(c).join("stage").join(tx)))
    }

    /// What a probe left, besides its row.
    pub struct Probed {
        pub row: Row,
        /// The protected inventory before the M3 command.
        pub inventory: M4Inventory,
        /// The M3 run's warnings, whole.
        pub warnings: Vec<Value>,
    }

    /// One replica: `scene` built afresh, aged when the probe ages, the inventory, history
    /// and bytes collected, the probe's M3 commands run, everything collected again, the
    /// M4 build's next commands run, the bytes checked again, and the row classified.
    pub fn run_probe(log: &TestLog, scene: &dyn Fn(&TestLog) -> Scene, probe: &Probe) -> Probed {
        let build = m3();
        let s = scene(log);
        let home = &s.home;
        let probe_dir = probe_folder(home);
        let age_label = if probe.aged { "2 h" } else { "at once" };
        log.note(format!("probe: {} ({age_label})", probe.name));
        if probe.aged {
            log.step("age");
            let spare = stamped_pre_paths(home);
            let aged = age(home, &spare);
            let show =
                |paths: &[PathBuf]| -> Vec<String> { paths.iter().map(|p| rel(home, p)).collect() };
            log.json("aged", &show(&aged.aged));
            log.json("spared", &show(&aged.spared));
        }

        log.step("inventory before");
        let mut inventory = M4Inventory::collect(home);
        for subject in &s.subjects {
            inventory.subject(subject);
        }
        log.json("inventory", &inventory.rel(home));
        let history_path = home.state().join("history.jsonl");
        let history = fs::read(&history_path).unwrap_or_default();
        let held = FolderBytes::held(home, &inventory);
        let trees = watched(home);
        for (label, dir) in &trees {
            log.tree(label, dir);
        }

        let mut warnings: Vec<Value> = Vec::new();
        let mut commands = Vec::new();
        let (mut exit, mut code) = (0, "ok".to_owned());
        for argv in probe.argv {
            let real: Vec<String> = argv
                .iter()
                .map(|a| {
                    if *a == PROBE {
                        probe_dir.clone()
                    } else {
                        (*a).to_owned()
                    }
                })
                .collect();
            log.step(&format!("m3 {}", argv[0]));
            let ran = log.run(&OldRunner { home, build }, &real);
            ran.assert_private();
            warnings.extend(
                ran.envelope()["warnings"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
            commands.push(typed(argv));
            exit = ran.run.code;
            code = ran.error_code().unwrap_or("ok").to_owned();
        }
        let codes: Vec<String> = warnings
            .iter()
            .map(|w| w["code"].as_str().unwrap_or_default().to_owned())
            .collect();

        log.step("inventory after");
        let recoveries: Vec<&Value> = warnings
            .iter()
            .filter(|w| w["code"] == "journal_recovered")
            .collect();
        let mut failures = Vec::new();
        let mut other = Vec::new();
        let recovered = match (&s.expect_m3_recovery, recoveries.as_slice()) {
            (_, []) => None,
            (Some(tx), [one])
                if one["message"]
                    .as_str()
                    .is_some_and(|m| m.contains(tx.as_str())) =>
            {
                Some(tx.clone())
            }
            (_, unexpected) => {
                let said: Vec<&str> = unexpected
                    .iter()
                    .filter_map(|w| w["message"].as_str())
                    .collect();
                other.push(format!("unexpected recovery: {}", said.join("; ")));
                failures.push("unexpected recovery".to_owned());
                None
            }
        };
        let exempt = |p: &Path| {
            recovered
                .as_deref()
                .is_some_and(|tx| own_artifact(home, tx, p))
        };
        let protected = inventory.without(exempt);
        let diff = protected.compare(&protected.rehash());
        log.json("inventory diff", &diff);
        if !diff.is_empty() {
            failures.push("protected entries changed".to_owned());
        }
        if !history_prefix_kept(&history, &fs::read(&history_path).unwrap_or_default()) {
            other.push("history rewritten".to_owned());
            failures.push("history rewritten".to_owned());
        }
        let held = match &recovered {
            Some(tx) => held.without(|p| in_own_stage(home, tx, p)),
            None => held,
        };
        let lost = held.lost(&FolderBytes::anywhere(home));
        if !lost.is_empty() {
            failures.push("bytes lost".to_owned());
        }
        if let (Some(_), Some(expected)) = (&recovered, s.addons_after_m3_recovery) {
            let addons = home.addons("_retail_");
            let now: Vec<(String, String)> = fs::read_dir(&addons)
                .into_iter()
                .flatten()
                .map(|e| e.unwrap().path())
                .map(|p| {
                    let name = p.file_name().unwrap().to_string_lossy().into_owned();
                    (name, tree_hash(&p))
                })
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            let want = expected(home);
            log.json(
                "addons after the M3 recovery",
                &json!({"now": now, "want": want}),
            );
            if now != want {
                failures.push("the recovered transaction did not end whole".to_owned());
                other.push(format!("AddOns: {now:?}, not {want:?}"));
            }
        }
        for (label, dir) in &trees {
            let changed = log.diff(label, dir);
            if !changed.is_empty() {
                other.push(format!("{label}: {}", changed.first(6).join(", ")));
            }
        }

        log.step("m4 next");
        let status = log
            .cmd(home, &["status", "--json", "--progress", "json"])
            .env("OMADDONS_TEST_FIXERS", "1")
            .run();
        status.assert_private();
        let doctor = log
            .cmd(home, &["doctor", "--json", "--progress", "json"])
            .env("OMADDONS_TEST_FIXERS", "1")
            .run();
        doctor.assert_private();
        let findings: Vec<String> = doctor.envelope()["findings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f["id"].as_str().map(str::to_owned))
            .collect();
        let mut m4_next = format!(
            "status exit {} [{}]; doctor exit {} [{}]",
            status.run.code,
            status.warnings().join(", "),
            doctor.run.code,
            findings.join(", ")
        );
        if let Some(next) = s.next {
            match next(log, &s) {
                Ok(said) => m4_next.push_str(&format!("; {said}")),
                Err(why) => {
                    m4_next.push_str(&format!("; failed: {why}"));
                    failures.push(why);
                }
            }
        }
        let lost_again = held.lost(&FolderBytes::anywhere(home));
        if !lost_again.is_empty() && lost.is_empty() {
            failures.push("bytes lost after the M4 build's next commands".to_owned());
        }
        let class = match (failures.is_empty(), recovered.is_some()) {
            (false, _) => "fail",
            (true, true) => "recovered_by_m3",
            (true, false) => "untouched",
        };
        let mut lost_all = lost;
        for again in lost_again {
            if !lost_all.contains(&again) {
                lost_all.push(again);
            }
        }
        let row = Row {
            family: s.family,
            scene: s.name.clone(),
            position: s.position.clone(),
            age: age_label,
            m3: commands.join("; "),
            exit,
            code,
            warnings: codes,
            protected: diff.describe(home),
            bytes: describe_lost(home, &lost_all),
            other,
            m4_next,
            class,
        };
        if matches!(class, "fail") {
            log.note(format!("FAIL: {}", failures.join("; ")));
        }
        log.json("row", &row);
        Probed {
            row,
            inventory,
            warnings,
        }
    }

    /// Prints the table, logs it (`observations`, and `table.md` in the test's log
    /// directory), and only then fails naming every `fail` row.
    pub fn assert_rows(log: &TestLog, title: &str, rows: &[Row]) {
        print_table(title, rows);
        log.json("observations", rows);
        fs::write(log.dir().join("table.md"), table(title, rows)).unwrap();
        let failed: Vec<String> = rows
            .iter()
            .filter(|r| matches!(r.class, "fail"))
            .map(|r| {
                format!(
                    "{} / {} / {}: protected {}; bytes {}; other {:?}; m4 next {}",
                    r.scene, r.position, r.age, r.protected, r.bytes, r.other, r.m4_next
                )
            })
            .collect();
        assert!(
            failed.is_empty(),
            "{} row(s) failed (transcript {}):\n{}",
            failed.len(),
            log.dir().join("transcript.md").display(),
            failed.join("\n")
        );
    }
}

/// The binary under test is `c0ab5e9`'s, built with its recorded toolchain, and it is
/// pre-M4: it has no `capabilities` command.
#[test]
#[ignore = "builds the M3 commit: cargo test --test compat_m3 -- --ignored --nocapture"]
fn the_m3_build_is_c0ab5e9_built_with_its_recorded_toolchain() {
    let log = tlog!();
    log.step("build");
    let build = m3();
    log.json("provenance", &build.provenance);
    log.note(format!(
        "binary: {M3_SHORT}, {}, sha256 {}, reused {}",
        build.provenance.toolchain, build.provenance.binary_sha256, build.reused
    ));
    log.assert_eq("toolchain", &build.provenance.toolchain.as_str(), &"1.97.1");
    assert!(
        build.provenance.rustc.contains("release: 1.97.1"),
        "rustc -vV names 1.97.1: {}",
        build.provenance.rustc
    );
    let home = Home::new(&["wow"]);
    log.env(&home);
    let runner = common::compat::OldRunner { home: &home, build };
    log.step("version");
    let version = log.run(&runner, &["--version"]);
    version.exit(0);
    assert!(version.stdout().contains("0.1.0"), "{}", version.stdout());
    log.step("pre-m4");
    let capabilities = log.run(&runner, &["capabilities", "--json"]);
    // M3's exit for `usage` is 2 (its `src/error.rs`); 64 is M4's.
    capabilities.exit(2);
    log.assert_eq(
        "no such command",
        &capabilities.error_code(),
        &Some("usage"),
    );
}

/// The control rows' scene and check.
#[cfg(test)]
mod control {
    use super::*;

    /// An M4 install killed by the failpoint `spec`.
    pub fn interrupted_install(log: &TestLog, spec: &'static str, position: &'static str) -> Scene {
        log.step("build");
        let home = Home::new(&["wow", "wow_classic_era"]);
        log.env(&home);
        let foo2 = local(&home, "Foo2");
        log.step("crash");
        log.cmd(&home, &["install", &foo2, "-t", "retail", "--json"])
            .failpoints(spec)
            .run()
            .aborted();
        let journals: Vec<String> = fs::read_dir(home.state().join("journal"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        log.note(format!("<state>/journal/: {journals:?}"));
        assert_eq!(journals.len(), 1, "one journal: {journals:?}");
        let tx = journals[0].trim_end_matches(".jsonl").to_owned();
        Scene {
            home,
            family: "control",
            name: "an interrupted M4 install".to_owned(),
            position: position.to_owned(),
            expect_m3_recovery: Some(tx),
            // Rolled back, then the probe installed: the probe folder alone.
            addons_after_m3_recovery: Some(|home: &Home| {
                vec![(
                    "M3Probe".to_owned(),
                    tree_hash(&home.env.root().join("src/M3Probe")),
                )]
            }),
            subjects: Vec::new(),
            next: Some(settled),
        }
    }

    /// After the M3 build's recovery: no journal left, and `status` reports none pending.
    fn settled(log: &TestLog, s: &Scene) -> Result<String, String> {
        let left: Vec<PathBuf> = fs::read_dir(s.home.state().join("journal"))
            .map(|l| l.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default();
        if !left.is_empty() {
            return Err(format!("journals left: {left:?}"));
        }
        let status = log.run(&s.home, &["status", "--json", "--progress", "json"]);
        status.ok();
        if status.warnings().iter().any(|w| w == "journal_pending") {
            return Err("status reports a pending journal".to_owned());
        }
        Ok("no journal left, none pending".to_owned())
    }
}

/// M4's folder journal is still M3's format: an M4 install interrupted after its
/// `prepared` line, or before its first swap, is recovered by the M3 build's next install
/// (rolled back), at once and two hours on, and nothing of M4's is touched.
#[test]
#[ignore = "builds the M3 commit: cargo test --test compat_m3 -- --ignored --nocapture"]
fn the_m3_build_recovers_an_interrupted_m4_folder_transaction() {
    let log = tlog!();
    let build = m3();
    log.note(format!(
        "binary: {M3_SHORT}, {}, sha256 {}, reused {}",
        build.provenance.toolchain, build.provenance.binary_sha256, build.reused
    ));
    let mut rows = Vec::new();
    for (spec, position) in [
        ("engine.journal.prepared=abort", "prepared"),
        ("engine.swap.before_rename=abort@1", "before 1"),
    ] {
        for probe in [&INSTALL, &INSTALL_AGED] {
            let age = if probe.aged { "2 h" } else { "at once" };
            let case = log.case(&format!("{position} {age}"));
            let scene = |log: &TestLog| control::interrupted_install(log, spec, position);
            let probed = run_probe(&case, &scene, probe);
            let row = &probed.row;
            case.assert_eq("exit", &row.exit, &0);
            case.assert_eq("class", &row.class, &"recovered_by_m3");
            case.assert_eq("no M4 artifacts", &probed.inventory.is_empty(), &true);
            let recovered: Vec<&Value> = probed
                .warnings
                .iter()
                .filter(|w| w["code"] == "journal_recovered")
                .collect();
            case.assert_eq("one recovery", &recovered.len(), &1);
            rows.push(probed.row);
        }
    }
    assert_rows(&log, "The M3 build after an interrupted M4 install", &rows);
}
