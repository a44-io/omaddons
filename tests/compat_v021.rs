//! e2e suite: the previous release (0.2.1, 8727df8) against a client this build changed, checkpoint only.
//!
//! RG-30 (decision RG-D30). From the first change this build makes to a client, the
//! client's record is format 1.1: each addon may carry `skipped_releases`, decisions that
//! must never be dropped. The build a user who downgrades meets is the previous release,
//! `v0.2.1`, built here from its tag's commit with the toolchain it records. On a client
//! whose record is 1.1 with marks, it must still read (`status`, `list`, `history`) and
//! refuse every change (`state_too_new`), leaving the record byte for byte. An interrupted
//! change of this build, whose journal's commit payload holds a 1.1 record, must be
//! neither completed wrongly, nor rewritten, nor swept as garbage: the record, the journal
//! and the stage stay byte for byte after each of its commands, at once and two hours
//! on, and this build then recovers it with the marks kept.
//!
//! The M3 checkpoint (`tests/compat_m3.rs`) stays as the historical test it is; this one
//! says what a downgrading user meets. Ignored by default (it builds an old commit); run
//! with a debug build of this test, whose failpoints crash this build's scenes:
//! `cargo test --test integration compat_v021:: -- --ignored --nocapture`
#![cfg(debug_assertions)]

use crate::common;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use common::compat::{OldRunner, age, skip_marks, target_record as record_path, v0_2_1};
use common::home::Home;
use common::tlog::tlog;
use common::trees::tree_hash;
use support::{RETAIL, announce, interrupted, local, marked_home};

// The scenes the tests share.
#[cfg(test)]
mod support {
    use super::*;
    use common::addons::{addon_folder, toc};
    use common::compat::{OldBuild, V0_2_1_SHORT, mark_skips};
    use common::tlog::TestLog;

    pub const RETAIL: &str = "_retail_";

    /// `src/<name>` holding one addon folder at `version`, written over what was there.
    pub fn local(home: &Home, name: &str, version: &str) -> String {
        let src = home.env.root().join("src");
        addon_folder(
            &src,
            name,
            &[(
                &format!("{name}.toc"),
                &toc("120105", &[&format!("## Version: {version}")]),
            )],
            &[("core.lua", format!("-- {name} {version}\n").as_bytes())],
        )
        .display()
        .to_string()
    }

    /// A home whose retail client has `Foo` installed by this build and marked: the
    /// record is 1.1 with marks.
    pub fn marked_home(log: &TestLog) -> Home {
        let home = Home::new(&["wow"]);
        log.env(&home);
        log.step("this build installs Foo and marks two releases");
        let source = local(&home, "Foo", "1.0");
        log.run(&home, &["install", &source, "-t", "retail", "--json"])
            .ok();
        mark_skips(&home, "Foo");
        log.assert_eq("Foo's marks", &skip_marks(&home, "Foo"), &2);
        home
    }

    pub fn announce(log: &TestLog, build: &OldBuild) {
        log.note(format!(
            "binary: {V0_2_1_SHORT}, {}, sha256 {}, reused {}",
            build.provenance.toolchain, build.provenance.binary_sha256, build.reused
        ));
    }

    /// What one interrupted change of this build left: its journal and its stage.
    pub struct Interrupted {
        pub journal: PathBuf,
        pub stage: PathBuf,
    }

    impl Interrupted {
        /// The journal's bytes, the record's bytes, and the stage's tree hash.
        pub fn bytes(&self, home: &Home) -> (Vec<u8>, Vec<u8>, String) {
            (
                fs::read(&self.journal).unwrap_or_default(),
                fs::read(record_path(home)).unwrap(),
                if self.stage.exists() {
                    tree_hash(&self.stage)
                } else {
                    "absent".to_owned()
                },
            )
        }
    }

    /// A marked home where this build's install of Foo 2.0 was killed by `spec`.
    pub fn interrupted(log: &TestLog, spec: &str) -> (Home, Interrupted) {
        let home = marked_home(log);
        log.step("this build's update of Foo is killed");
        let source = local(&home, "Foo", "2.0");
        log.cmd(&home, &["install", &source, "-t", "retail", "--json"])
            .failpoints(spec)
            .run()
            .aborted();
        let journals: Vec<PathBuf> = fs::read_dir(home.state().join("journal"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(journals.len(), 1, "one journal: {journals:?}");
        let journal = journals[0].clone();
        let tx = journal
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_end_matches(".jsonl")
            .to_owned();
        let payload = fs::read_to_string(&journal).unwrap();
        assert!(
            payload.contains("\"skipped_releases\""),
            "the commit payload holds the 1.1 record with its marks"
        );
        let stage = home
            .client(RETAIL)
            .join("Interface/.omaddons/stage")
            .join(&tx);
        log.note(format!("journal {tx}, stage present: {}", stage.exists()));
        (home, Interrupted { journal, stage })
    }
}

/// The binary under test is `8727df8`'s, the commit tag `v0.2.1` names, built with its
/// recorded toolchain, and it says it is 0.2.1.
#[test]
#[ignore = "builds v0.2.1: cargo test --test integration compat_v021:: -- --ignored --nocapture"]
fn the_0_2_1_build_is_8727df8_built_with_its_recorded_toolchain() {
    let log = tlog!();
    log.step("build");
    let build = v0_2_1();
    log.json("provenance", &build.provenance);
    announce(&log, build);
    log.assert_eq(
        "commit",
        &build.provenance.commit.as_str(),
        &"8727df8dc168ff211676569be2d20c019b6e73af",
    );
    log.assert_eq("toolchain", &build.provenance.toolchain.as_str(), &"1.97.1");
    let home = Home::new(&["wow"]);
    log.env(&home);
    let runner = OldRunner { home: &home, build };
    log.step("version");
    let version = log.run(&runner, &["--version"]);
    version.exit(0);
    assert!(version.stdout().contains("0.2.1"), "{}", version.stdout());
}

/// On a client whose record is 1.1 with marks, the 0.2.1 build reads, and refuses every
/// change with `state_too_new`, leaving the record and the AddOns folder byte for byte.
#[test]
#[ignore = "builds v0.2.1: cargo test --test integration compat_v021:: -- --ignored --nocapture"]
fn the_0_2_1_build_reads_a_1_1_client_and_refuses_every_change_to_it() {
    let log = tlog!();
    let build = v0_2_1();
    announce(&log, build);
    let home = marked_home(&log);
    let runner = OldRunner { home: &home, build };
    let record = fs::read(record_path(&home)).unwrap();
    let addons = tree_hash(&home.client(RETAIL).join("Interface/AddOns"));
    let bar = local(&home, "Bar", "1.0");
    let foo_2 = local(&home, "Foo", "2.0");

    log.step("0.2.1 reads");
    for argv in [
        &["status", "--json"][..],
        &["list", "--json"],
        &["history", "--json"],
    ] {
        let ran = log.run(&runner, argv);
        ran.ok();
        log.assert_eq(
            &format!("{} leaves the record", argv[0]),
            &(fs::read(record_path(&home)).unwrap() == record),
            &true,
        );
    }
    let list = log.run(&runner, &["list", "-t", "retail", "--json"]);
    list.ok();
    let names: Vec<&str> = list.data()["targets"][0]["addons"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect())
        .unwrap_or_default();
    log.assert_eq("0.2.1 lists Foo", &names, &vec!["Foo"]);

    log.step("0.2.1 changes");
    let changes: [(&str, Vec<&str>); 6] = [
        (
            "install a new addon",
            vec!["install", &bar, "-t", "retail", "--json"],
        ),
        (
            "install over Foo",
            vec!["install", &foo_2, "-t", "retail", "--json"],
        ),
        (
            "reinstall Foo",
            vec!["install", &foo_2, "-t", "retail", "--reinstall", "--json"],
        ),
        (
            "remove Foo",
            vec!["remove", "Foo", "-t", "retail", "--json"],
        ),
        ("pin Foo", vec!["pin", "Foo", "-t", "retail", "--json"]),
        ("undo the last change", vec!["undo", "--json"]),
    ];
    for (name, argv) in changes {
        let case = log.case(name);
        let ran = case.run(&runner, &argv);
        ran.fails(4, "state_too_new");
        case.assert_eq(
            "the record, byte for byte",
            &(fs::read(record_path(&home)).unwrap() == record),
            &true,
        );
        case.assert_eq(
            "the AddOns folder",
            &tree_hash(&home.client(RETAIL).join("Interface/AddOns")),
            &addons,
        );
    }
}

/// RG-33: this build's `skip` (transaction kind `skip`, history 1.1, plan 1.2) meets the
/// 0.2.1 build three ways: a committed `skip` line in `history.jsonl`, a stored `skip`
/// plan, and an interrupted `skip` transaction. 0.2.1 reads history or refuses cleanly,
/// refuses to apply the plan and to undo the `skip`, and leaves the journal, the stage,
/// the plan, the record and `history.jsonl` byte for byte; this build then recovers the
/// interrupted `skip` and keeps every mark.
#[test]
#[ignore = "builds v0.2.1: cargo test --test integration compat_v021:: -- --ignored --nocapture"]
fn the_0_2_1_build_leaves_this_build_s_skips_alone() {
    let log = tlog!();
    let build = v0_2_1();
    announce(&log, build);
    let home = marked_home(&log);
    let state = common::http::paths(&home.env);

    log.step("this build commits a skip");
    let committed = log.run(
        &home,
        &["skip", "Foo", "--version", "9", "-t", "retail", "--json"],
    );
    committed.ok();
    let skip_tx = committed.data()["targets"][0]["tx"]
        .as_str()
        .unwrap()
        .to_owned();
    log.assert_eq("three marks", &skip_marks(&home, "Foo"), &3);

    log.step("this build stores a skip plan");
    let preview = log.run(
        &home,
        &[
            "skip",
            "Foo",
            "--version",
            "10",
            "-t",
            "retail",
            "--dry-run",
            "--json",
        ],
    );
    preview.ok();
    let plan_id: omaddons::model::PlanId =
        preview.data()["plan_id"].as_str().unwrap().parse().unwrap();
    let plan = state.plan(&plan_id);

    log.step("this build's skip is killed before it commits");
    log.cmd(
        &home,
        &["skip", "Foo", "--version", "11", "-t", "retail", "--json"],
    )
    .failpoints("engine.journal.prepared=abort")
    .run()
    .aborted();
    let journals: Vec<PathBuf> = fs::read_dir(home.state().join("journal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(journals.len(), 1, "one journal: {journals:?}");
    let journal = journals[0].clone();
    let tx = journal
        .file_name()
        .unwrap()
        .to_string_lossy()
        .trim_end_matches(".jsonl")
        .to_owned();
    let stage = home
        .client(RETAIL)
        .join("Interface/.omaddons/stage")
        .join(&tx);
    let bytes = || {
        (
            fs::read(&journal).unwrap_or_default(),
            fs::read(record_path(&home)).unwrap(),
            if stage.exists() {
                tree_hash(&stage)
            } else {
                "absent".to_owned()
            },
            fs::read(&plan).unwrap(),
            fs::read(state.history_file()).unwrap(),
        )
    };
    let before = bytes();
    log.assert_eq(
        "the journal is this build's skip",
        &String::from_utf8_lossy(&before.0).contains("\"kind\":\"skip\""),
        &true,
    );

    let runner = OldRunner { home: &home, build };
    let plan_text = plan_id.to_string();
    let probes: [(&str, Vec<&str>, bool); 6] = [
        ("status", vec!["status", "--json"], false),
        ("history", vec!["history", "--json"], false),
        ("list", vec!["list", "--json"], false),
        (
            "apply the skip plan",
            vec!["apply", &plan_text, "--json"],
            true,
        ),
        ("undo the skip", vec!["undo", &skip_tx, "--json"], true),
        ("undo", vec!["undo", "--json"], true),
    ];
    for (name, argv, refused) in probes {
        log.step(&format!("0.2.1: {name}"));
        let ran = log.run(&runner, &argv);
        log.note(format!(
            "exit {:?}, error {:?}, warnings {:?}",
            ran.envelope()["exit_code"],
            ran.error_code(),
            ran.warnings()
        ));
        if refused {
            log.assert_eq(
                &format!("{name}: refused"),
                &(ran.envelope()["exit_code"] != 0),
                &true,
            );
        } else {
            log.assert_eq(
                &format!("{name}: one envelope, read or refused cleanly"),
                &ran.envelope()["exit_code"].is_i64(),
                &true,
            );
        }
        let after = bytes();
        log.assert_eq(
            &format!("{name}: the journal"),
            &(after.0 == before.0),
            &true,
        );
        log.assert_eq(
            &format!("{name}: the record"),
            &(after.1 == before.1),
            &true,
        );
        log.assert_eq(&format!("{name}: the stage"), &after.2, &before.2);
        log.assert_eq(&format!("{name}: the plan"), &(after.3 == before.3), &true);
        log.assert_eq(
            &format!("{name}: history.jsonl"),
            &(after.4 == before.4),
            &true,
        );
    }

    log.step("this build recovers its skip and keeps every mark");
    let bar = local(&home, "Bar", "1.0");
    let recovered = log.run(&home, &["install", &bar, "-t", "retail", "--json"]);
    recovered.ok();
    log.assert_eq(
        "recovered",
        &recovered
            .warnings()
            .contains(&"journal_recovered".to_owned()),
        &true,
    );
    log.assert_eq("no journal left", &journal.exists(), &false);
    log.assert_eq("the marks are kept", &skip_marks(&home, "Foo"), &3);
}

/// RG-02: this build's history lines record `transitions`, and so does the history line
/// an interrupted change carries in its journal. With such lines in `history.jsonl`, and
/// a change of this build killed after its swap, the 0.2.1 build reads history (its
/// `history` and `undo` included) or refuses cleanly, refuses to change the client, and
/// leaves the journal, the record, the stage and `history.jsonl` byte for byte. This
/// build then rolls the change forward, writing the transitions the journal carried.
#[test]
#[ignore = "builds v0.2.1: cargo test --test integration compat_v021:: -- --ignored --nocapture"]
fn the_0_2_1_build_leaves_this_build_s_transitions_alone() {
    let log = tlog!();
    let build = v0_2_1();
    announce(&log, build);
    let home = marked_home(&log);
    let state = common::http::paths(&home.env);

    log.step("this build updates Foo to 2.0");
    let source = local(&home, "Foo", "2.0");
    let updated = log.run(&home, &["install", &source, "-t", "retail", "--json"]);
    updated.ok();
    let update_tx = updated.data()["tx"].as_str().unwrap().to_owned();
    let history = fs::read_to_string(state.history_file()).unwrap();
    log.assert_eq(
        "every line of this build's changes records transitions",
        &history
            .lines()
            .filter(|l| l.contains("\"kind\":\"install\""))
            .all(|l| l.contains("\"transitions\":[")),
        &true,
    );

    log.step("this build's update to 3.0 is killed after its swap");
    let source = local(&home, "Foo", "3.0");
    log.cmd(&home, &["install", &source, "-t", "retail", "--json"])
        .failpoints("engine.swapped=abort")
        .run()
        .aborted();
    let journals: Vec<PathBuf> = fs::read_dir(home.state().join("journal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(journals.len(), 1, "one journal: {journals:?}");
    let journal = journals[0].clone();
    let tx = journal
        .file_name()
        .unwrap()
        .to_string_lossy()
        .trim_end_matches(".jsonl")
        .to_owned();
    let stage = home
        .client(RETAIL)
        .join("Interface/.omaddons/stage")
        .join(&tx);
    let bytes = || {
        (
            fs::read(&journal).unwrap_or_default(),
            fs::read(record_path(&home)).unwrap(),
            if stage.exists() {
                tree_hash(&stage)
            } else {
                "absent".to_owned()
            },
            fs::read(state.history_file()).unwrap(),
        )
    };
    let before = bytes();
    log.assert_eq(
        "the journal's history line carries the transitions",
        &String::from_utf8_lossy(&before.0).contains("\"transitions\":[{\"addon\":\"Foo\""),
        &true,
    );

    let runner = OldRunner { home: &home, build };
    let bar = local(&home, "Bar", "1.0");
    let probes: [(&str, Vec<&str>, bool); 6] = [
        ("status", vec!["status", "--json"], false),
        ("history", vec!["history", "--json"], false),
        ("list", vec!["list", "--json"], false),
        (
            "install a new addon",
            vec!["install", &bar, "-t", "retail", "--json"],
            true,
        ),
        ("undo the update", vec!["undo", &update_tx, "--json"], true),
        ("undo", vec!["undo", "--json"], true),
    ];
    for (name, argv, refused) in probes {
        log.step(&format!("0.2.1: {name}"));
        let ran = log.run(&runner, &argv);
        log.note(format!(
            "exit {:?}, error {:?}, warnings {:?}",
            ran.envelope()["exit_code"],
            ran.error_code(),
            ran.warnings()
        ));
        if refused {
            log.assert_eq(
                &format!("{name}: refused"),
                &(ran.envelope()["exit_code"] != 0),
                &true,
            );
        } else {
            log.assert_eq(
                &format!("{name}: one envelope, read or refused cleanly"),
                &ran.envelope()["exit_code"].is_i64(),
                &true,
            );
        }
        let after = bytes();
        log.assert_eq(
            &format!("{name}: the journal"),
            &(after.0 == before.0),
            &true,
        );
        log.assert_eq(
            &format!("{name}: the record"),
            &(after.1 == before.1),
            &true,
        );
        log.assert_eq(&format!("{name}: the stage"), &after.2, &before.2);
        log.assert_eq(
            &format!("{name}: history.jsonl"),
            &(after.3 == before.3),
            &true,
        );
    }

    log.step("this build rolls its update forward with the transitions it carried");
    let recovered = log.run(&home, &["install", &bar, "-t", "retail", "--json"]);
    recovered.ok();
    log.assert_eq(
        "recovered",
        &recovered
            .warnings()
            .contains(&"journal_recovered".to_owned()),
        &true,
    );
    log.assert_eq("no journal left", &journal.exists(), &false);
    let history = fs::read_to_string(state.history_file()).unwrap();
    let line: serde_json::Value = history
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|l| l["tx"] == tx.as_str())
        .unwrap();
    log.assert_eq(
        "the rolled-forward line",
        &(
            &line["transitions"][0]["from"]["version"],
            &line["transitions"][0]["to"]["version"],
        ),
        &(&serde_json::json!("2.0"), &serde_json::json!("3.0")),
    );
    log.assert_eq("the marks are kept", &skip_marks(&home, "Foo"), &2);
}

/// An interrupted change of this build, at each position, is left byte for byte by every
/// command of the 0.2.1 build, at once and two hours on; this build then recovers it and
/// keeps the marks.
#[test]
#[ignore = "builds v0.2.1: cargo test --test integration compat_v021:: -- --ignored --nocapture"]
fn the_0_2_1_build_leaves_an_interrupted_change_of_this_build_alone() {
    let log = tlog!();
    let build = v0_2_1();
    announce(&log, build);
    for (spec, position) in [
        ("engine.journal.prepared=abort", "before the first rename"),
        ("engine.swapped=abort", "after the swap, before the record"),
    ] {
        for aged in [false, true] {
            let case = log.case(&format!(
                "{position}, {}",
                if aged { "2 h on" } else { "at once" }
            ));
            let (home, left) = interrupted(&case, spec);
            if aged {
                let moved = age(&home, &BTreeSet::new());
                case.note(format!("aged {} entries", moved.aged.len()));
            }
            let runner = OldRunner { home: &home, build };
            let before = left.bytes(&home);
            let bar = local(&home, "Bar", "1.0");
            let probes: [(&str, Vec<&str>); 6] = [
                ("status", vec!["status", "--json"]),
                ("list", vec!["list", "--json"]),
                ("history", vec!["history", "--json"]),
                (
                    "install a new addon",
                    vec!["install", &bar, "-t", "retail", "--json"],
                ),
                (
                    "remove Foo",
                    vec!["remove", "Foo", "-t", "retail", "--json"],
                ),
                ("undo", vec!["undo", "--json"]),
            ];
            for (name, argv) in probes {
                case.step(&format!("0.2.1: {name}"));
                let ran = case.run(&runner, &argv);
                case.note(format!(
                    "exit {:?}, error {:?}",
                    ran.envelope()["exit_code"],
                    ran.error_code()
                ));
                let after = left.bytes(&home);
                case.assert_eq(
                    &format!("{name}: the journal, byte for byte"),
                    &(after.0 == before.0),
                    &true,
                );
                case.assert_eq(
                    &format!("{name}: the record, byte for byte"),
                    &(after.1 == before.1),
                    &true,
                );
                case.assert_eq(&format!("{name}: the stage"), &after.2, &before.2);
            }
            case.step("this build recovers it, keeping the marks");
            let status = case.run(&home, &["install", &bar, "-t", "retail", "--json"]);
            status.ok();
            case.assert_eq(
                "recovered",
                &status.warnings().contains(&"journal_recovered".to_owned()),
                &true,
            );
            case.assert_eq("no journal left", &left.journal.exists(), &false);
            case.assert_eq("the marks are kept", &skip_marks(&home, "Foo"), &2);
        }
    }
}
