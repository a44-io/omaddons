//! e2e suite: snapshot retention reaches clients nobody changes any more: a commit or a backup on one client prunes every other client's old undo snapshots, at most once a day.
//!
//! A retail and a Classic Era client. Twelve changes on era leave twelve snapshots; their
//! history lines are then aged as days of play would age them (retention reads a
//! transaction's age from its line), and a change or a backup on retail must prune era
//! to exactly what era's own retention keeps: the newest 10 inside 7 days. The pass is
//! stamped in `<state>/retention_sweep.stamp`, so a second change the same day reaches no
//! other client; a client whose directory is gone is skipped; and a pass that fails is
//! a `retention_failed` warning naming the client, retried by the next command.
//!
//! Before any pass, `status` counts each client's undo snapshots (`undo_snapshots`, sized
//! with `-v`) and, with `doctor`, names the ones retention would prune now
//! (`fm-state_files-snapshots-past-retention`), by the rule the pass applies, writing
//! nothing; never within policy, and never for a client whose directory is gone.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};

use common::addons::toc;
use common::archives::zip_text;
use common::home::Home;
#[cfg(debug_assertions)]
use common::state::history;
use common::state::key_of;
use common::tlog::{Ran, TestLog, tlog};
use jiff::{SignedDuration, Timestamp};
use omaddons::model::TargetKey;
use serde_json::Value;

const HOUR: i64 = 3_600;
const DAY: i64 = 24 * HOUR;

/// The ages era's twelve transactions get, newest first: 26 hours, then 14 hours older
/// each, so the newest 11 are inside the 7-day window and the twelfth is past it.
fn ages() -> Vec<i64> {
    (0..12).map(|i| 26 * HOUR + i * 14 * HOUR).collect()
}

/// What era's own retention keeps of them: the newest 10 inside the window.
fn kept_ages() -> Vec<i64> {
    ages()[..10].to_vec()
}

/// `omaddons <args> --json --progress json`, both streams checked for private names.
fn json(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv = args.to_vec();
    argv.extend(["--json", "--progress", "json"]);
    let ran = log.run(home, &argv);
    ran.assert_private();
    ran
}

/// A local zip of one folder `name` loading at `interface`.
fn zip(home: &Home, name: &str, interface: &str) -> String {
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join(format!("{name}.zip"));
    let text = toc(interface, &["## Version: 1"]);
    let (toc_path, lua_path) = (format!("{name}/{name}.toc"), format!("{name}/core.lua"));
    fs::write(&zip, zip_text(&[(&toc_path, &text), (&lua_path, "1")])).unwrap();
    zip.display().to_string()
}

/// The home: a retail and a Classic Era client, twelve changes on era (Foo installed
/// and removed in turn), each leaving its undo snapshots, and retail never changed.
/// Returns era's key and its transactions in history order.
fn era_with_twelve_changes(log: &TestLog) -> (Home, TargetKey, Vec<String>) {
    log.step("twelve changes on era");
    let home = Home::new(&["wow", "wow_classic_era"]);
    let archive = zip(&home, "Foo", "11509");
    let mut txs = Vec::new();
    for n in 0..12 {
        let ran = if n % 2 == 0 {
            json(log, &home, &["install", &archive, "-t", "era"])
        } else {
            json(log, &home, &["remove", "Foo", "-t", "era"])
        };
        ran.ok();
        txs.push(ran.data()["tx"].as_str().unwrap().to_owned());
    }
    let era = key_of(&home, "era");
    assert_eq!(
        snapshots(&home, &txs).len(),
        12,
        "every change left its snapshot"
    );
    (home, era, txs)
}

/// Ages each history line of `txs` (oldest first) to `ages` (newest first), as days of
/// play would; every other line is left as it is.
fn age_lines(log: &TestLog, home: &Home, txs: &[String], ages: &[i64]) {
    log.step("age era's history lines");
    let now = Timestamp::now();
    let path = home.state().join("history.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let mut out = String::new();
    for line in text.lines() {
        let mut value: Value = serde_json::from_str(line).unwrap();
        if let Some(at) = txs.iter().position(|tx| value["tx"] == tx.as_str()) {
            let age = ages[txs.len() - 1 - at];
            value["at"] = (now - SignedDuration::from_secs(age)).to_string().into();
        }
        out.push_str(&serde_json::to_string(&value).unwrap());
        out.push('\n');
    }
    fs::write(&path, out).unwrap();
}

/// The transactions of `txs` with an undo snapshot left on either side.
fn snapshots(home: &Home, txs: &[String]) -> Vec<String> {
    let client = home
        .client("_classic_era_")
        .join("Interface/.omaddons/snapshots");
    let state = home.state().join("snapshots");
    txs.iter()
        .filter(|tx| {
            [client.join(tx.as_str()), state.join(tx.as_str())]
                .iter()
                .any(|p| p.exists())
        })
        .cloned()
        .collect()
}

/// The transactions of `txs` (oldest first) whose snapshots era's own retention keeps:
/// those `age_lines` aged to one of `kept_ages`.
fn kept(txs: &[String], kept_ages: &[i64]) -> Vec<String> {
    let ages = ages();
    txs.iter()
        .enumerate()
        .filter(|(at, _)| kept_ages.contains(&ages[txs.len() - 1 - at]))
        .map(|(_, tx)| tx.clone())
        .collect()
}

fn stamp(home: &Home) -> PathBuf {
    home.state().join("retention_sweep.stamp")
}

/// Every line of `history.jsonl`.
fn history_lines(home: &Home) -> Vec<String> {
    let text = fs::read_to_string(home.state().join("history.jsonl")).unwrap();
    text.lines().map(str::to_owned).collect()
}

fn retention_warnings(ran: &Ran) -> Vec<String> {
    ran.envelope()["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "retention_failed")
        .map(|w| w["message"].as_str().unwrap().to_owned())
        .collect()
}

fn install_on_retail(log: &TestLog, home: &Home, name: &str) -> Ran {
    let zip = zip(home, name, "120105");
    json(log, home, &["install", &zip, "-t", "retail"])
}

#[test]
fn a_commit_on_one_client_prunes_another_clients_old_snapshots() {
    let log = tlog!();
    let (home, era, txs) = era_with_twelve_changes(&log);
    age_lines(&log, &home, &txs, &ages());
    let before = history_lines(&home);

    log.step("install on retail");
    let ran = install_on_retail(&log, &home, "Bar");
    ran.ok();
    log.assert_eq(
        "no retention warning",
        &retention_warnings(&ran),
        &Vec::<String>::new(),
    );
    log.assert_eq(
        "era kept exactly what its own retention keeps",
        &snapshots(&home, &txs),
        &kept(&txs, &kept_ages()),
    );
    let after = history_lines(&home);
    log.assert_eq(
        "history: the old lines unchanged",
        &after[..before.len()].to_vec(),
        &before,
    );
    log.assert_eq("history: one line more", &after.len(), &(before.len() + 1));
    let last: Value = serde_json::from_str(after.last().unwrap()).unwrap();
    assert_ne!(last["target"], era.to_string(), "the new line is retail's");
    assert!(stamp(&home).is_file(), "the pass is stamped");
}

#[test]
fn a_backup_on_one_client_prunes_another_clients_old_snapshots() {
    let log = tlog!();
    let (home, _, txs) = era_with_twelve_changes(&log);
    common::wtf::Wtf::at(home.client("_retail_"))
        .saved_variables("Bar", 64)
        .build();
    age_lines(&log, &home, &txs, &ages());

    log.step("back up retail");
    let ran = json(&log, &home, &["backup", "create", "-t", "retail"]);
    ran.ok();
    log.assert_eq(
        "no retention warning",
        &retention_warnings(&ran),
        &Vec::<String>::new(),
    );
    assert!(
        ran.data()["targets"][0]["backup"].is_object(),
        "a backup was made: {}",
        ran.data()
    );
    log.assert_eq(
        "era kept exactly what its own retention keeps",
        &snapshots(&home, &txs),
        &kept(&txs, &kept_ages()),
    );
    assert!(stamp(&home).is_file(), "the pass is stamped");
}

#[test]
fn the_pass_runs_at_most_once_a_day() {
    let log = tlog!();
    let (home, _, txs) = era_with_twelve_changes(&log);
    age_lines(&log, &home, &txs, &ages());
    install_on_retail(&log, &home, "Bar").ok();
    let first = kept(&txs, &kept_ages());
    log.assert_eq("the first pass pruned era", &snapshots(&home, &txs), &first);

    // Nine days on, every one of them is past the window: a pass would keep only the
    // most recent. The same day's second change on retail reaches no other client.
    let later: Vec<i64> = ages().iter().map(|age| age + 9 * DAY).collect();
    age_lines(&log, &home, &txs, &later);
    log.step("a second change on retail, the same day");
    install_on_retail(&log, &home, "Baz").ok();
    log.assert_eq("era untouched", &snapshots(&home, &txs), &first);

    // A day after the stamp, the next change passes again.
    log.step("the stamp a day old");
    let at = Timestamp::now() - SignedDuration::from_secs(DAY + HOUR);
    log.assert_eq(
        "the stamp is one instant",
        &fs::read_to_string(stamp(&home))
            .unwrap()
            .trim()
            .parse::<Timestamp>()
            .is_ok(),
        &true,
    );
    fs::write(stamp(&home), format!("{at}\n")).unwrap();
    install_on_retail(&log, &home, "Qux").ok();
    log.assert_eq(
        "era kept only its most recent",
        &snapshots(&home, &txs),
        &vec![txs.last().unwrap().clone()],
    );
}

#[test]
fn a_client_whose_directory_is_gone_is_skipped_without_a_warning() {
    let log = tlog!();
    let (home, _, txs) = era_with_twelve_changes(&log);
    age_lines(&log, &home, &txs, &ages());
    log.step("era's directory gone");
    let era_dir = home.client("_classic_era_");
    let away: &Path = &home.env.root().join("era-away");
    fs::rename(&era_dir, away).unwrap();

    let ran = install_on_retail(&log, &home, "Bar");
    ran.ok();
    log.assert_eq(
        "no retention warning",
        &retention_warnings(&ran),
        &Vec::<String>::new(),
    );
    assert!(
        !stamp(&home).exists(),
        "nothing was reached, so nothing is stamped"
    );
    fs::rename(away, &era_dir).unwrap();
    log.assert_eq("era untouched", &snapshots(&home, &txs).len(), &12);
}

/// Failpoints arm only in debug builds.
#[cfg(debug_assertions)]
#[test]
fn a_failed_pass_names_the_client_and_the_next_command_converges() {
    let log = tlog!();
    let (home, _, txs) = era_with_twelve_changes(&log);
    age_lines(&log, &home, &txs, &ages());

    log.step("install on retail, the pass failing");
    let zip = zip(&home, "Bar", "120105");
    let argv = [
        "install",
        &zip,
        "-t",
        "retail",
        "--json",
        "--progress",
        "json",
    ];
    let ran = log
        .cmd(&home, &argv)
        .failpoints("engine.retention.sweep=error")
        .run();
    ran.ok().assert_private();
    let warnings = retention_warnings(&ran);
    log.assert_eq("one retention_failed", &warnings.len(), &1);
    assert!(
        warnings[0].starts_with("pruning era's snapshots failed"),
        "names era: {warnings:?}"
    );
    log.assert_eq(
        "the install committed",
        &ran.data()["items"][0]["status"],
        &"installed".into(),
    );
    log.assert_eq("era untouched", &snapshots(&home, &txs).len(), &12);
    assert!(!stamp(&home).exists(), "a failed pass is not stamped");
    log.assert_eq("its history line", &history(&home).len(), &13);

    log.step("the next command");
    let ran = json(&log, &home, &["remove", "Bar", "-t", "retail"]);
    ran.ok();
    log.assert_eq(
        "no retention warning",
        &retention_warnings(&ran),
        &Vec::<String>::new(),
    );
    log.assert_eq(
        "converged",
        &snapshots(&home, &txs),
        &kept(&txs, &kept_ages()),
    );
    assert!(stamp(&home).is_file(), "stamped");
}

/// Twelve ages past the window, newest first: 8 days, then an hour older each.
fn old_ages() -> Vec<i64> {
    (0..12).map(|i| 8 * DAY + i * HOUR).collect()
}

/// The client `id`'s `undo_snapshots` in a `status` envelope.
fn undo_of(ran: &Ran, id: &str) -> Value {
    ran.data()["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap()["undo_snapshots"]
        .clone()
}

/// The findings of `fm` in a `doctor` envelope.
fn findings_of(ran: &Ran, fm: &str) -> Vec<Value> {
    ran.envelope()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["id"] == fm)
        .cloned()
        .collect()
}

const PAST_RETENTION: &str = "fm-state_files-snapshots-past-retention";

#[test]
fn status_and_the_doctor_name_the_snapshots_retention_would_prune() {
    let log = tlog!();
    let (home, era, txs) = era_with_twelve_changes(&log);
    age_lines(&log, &home, &txs, &old_ages());
    let oldest = fs::read_to_string(home.state().join("history.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["tx"] == txs[0].as_str())
        .unwrap()["at"]
        .clone();
    let snapshots_dir = home
        .client("_classic_era_")
        .join("Interface/.omaddons/snapshots");
    log.tree("era", &home.client("_classic_era_"));
    log.tree("state snapshots", &home.state().join("snapshots"));

    log.step("status: counted, not sized");
    let ran = json(&log, &home, &["status"]);
    ran.ok();
    let undo = undo_of(&ran, "era");
    log.assert_eq("count", &undo["count"], &12.into());
    log.assert_eq("oldest_at", &undo["oldest_at"], &oldest);
    log.assert_eq(
        "prunable: all but the most recent",
        &undo["prunable"],
        &11.into(),
    );
    log.assert_eq("bytes", &undo["bytes"], &Value::Null);
    log.assert_eq("truncated", &undo["truncated"], &false.into());
    let retail = undo_of(&ran, "retail");
    log.assert_eq("retail: none", &retail["count"], &0.into());
    let era_issue: Vec<&Value> = ran.data()["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "era")
        .unwrap()["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["id"] == PAST_RETENTION)
        .collect();
    log.assert_eq("era's issue", &era_issue.len(), &1);
    log.assert_eq("its severity", &era_issue[0]["severity"], &"P3".into());

    log.step("status -v: sized, both halves");
    let ran = json(&log, &home, &["status", "-v"]);
    ran.ok();
    let bytes = undo_of(&ran, "era")["bytes"].as_u64().unwrap();
    let expected: u64 = txs
        .iter()
        .flat_map(|tx| {
            [
                snapshots_dir.join(tx),
                home.state().join("snapshots").join(tx),
            ]
        })
        .map(|p| tree_bytes(&p))
        .sum();
    log.assert_eq("bytes", &bytes, &expected);
    assert!(bytes > 0, "the snapshots hold the removed folders");

    log.step("status --cached: no history read, so nothing to say");
    let ran = json(&log, &home, &["status", "--cached"]);
    ran.ok();
    log.assert_eq("null", &undo_of(&ran, "era"), &Value::Null);

    log.step("the human view");
    let ran = log.run(&home, &["status"]);
    let line = ran
        .stdout()
        .lines()
        .find(|l| l.trim_start().starts_with("backups:") && l.contains("undo:"))
        .unwrap_or_default()
        .to_owned();
    assert!(
        line.ends_with(" · undo: 12 snapshots (over policy)"),
        "{}",
        ran.stdout()
    );

    log.step("doctor: one finding, the eleven oldest");
    let ran = json(&log, &home, &["doctor"]);
    let found = findings_of(&ran, PAST_RETENTION);
    log.assert_eq("one finding", &found.len(), &1);
    let f = &found[0];
    log.assert_eq("on era", &f["target"]["key"], &era.to_string().into());
    log.assert_eq("count", &f["evidence"]["count"], &11.into());
    log.assert_eq(
        "txs, oldest first",
        &f["evidence"]["txs"],
        &serde_json::to_value(&txs[..11]).unwrap(),
    );
    log.assert_eq("oldest_at", &f["evidence"]["oldest_at"], &oldest);
    log.assert_eq(
        "path",
        &f["evidence"]["path"],
        &snapshots_dir.display().to_string().into(),
    );

    log.step("nothing was pruned or written by reading");
    log.assert_eq("every snapshot kept", &snapshots(&home, &txs).len(), &12);
    log.assert_unchanged("era", &home.client("_classic_era_"));
    log.assert_unchanged("state snapshots", &home.state().join("snapshots"));
}

/// The doctor's fixture (M4-D47) is found as its README says, and its manual remedy, the
/// next change on the client, clears it: that change's own retention prunes them.
#[test]
fn the_fixture_is_found_and_the_next_change_clears_it() {
    use crate::fixture::Fixture;
    use crate::fixture::fixtures::fm_state_files_snapshots_past_retention::SnapshotsPastRetention;
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let fx = SnapshotsPastRetention::default();
    log.step("corrupt");
    fx.corrupt(&home);
    let subject = fx.subject(&home);
    log.assert_eq("two transactions, both halves", &subject.len(), &4);
    assert!(subject.iter().all(|p| p.exists()), "{subject:?}");

    log.step("doctor finds it");
    let ran = json(&log, &home, &["doctor", "--only", PAST_RETENTION]);
    let found = findings_of(&ran, PAST_RETENTION);
    log.assert_eq("one finding", &found.len(), &1);
    log.assert_eq("count", &found[0]["evidence"]["count"], &2.into());
    log.assert_eq("severity", &found[0]["severity"], &"P3".into());
    log.assert_eq(
        "its advice is prose",
        &found[0]["remediation"]["argv"],
        &Value::Null,
    );

    log.step("the remedy: the next change");
    fx.remedy(&home);
    fx.assert_healthy(&home);
}

/// The apparent size of the regular files under `path` (symlinks not followed).
fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    fs::read_dir(path)
        .unwrap()
        .map(|e| tree_bytes(&e.unwrap().path()))
        .sum()
}

#[test]
fn the_doctor_says_nothing_within_policy_or_for_a_missing_client() {
    let log = tlog!();
    let (home, _, txs) = era_with_twelve_changes(&log);
    log.step("within policy: twelve changes made just now");
    let ran = json(&log, &home, &["doctor"]);
    log.assert_eq(
        "every one is under a day old, so none is due",
        &findings_of(&ran, PAST_RETENTION).len(),
        &0,
    );
    let ran = json(&log, &home, &["status"]);
    log.assert_eq("prunable", &undo_of(&ran, "era")["prunable"], &0.into());

    log.step("past it, but era's directory is gone");
    age_lines(&log, &home, &txs, &old_ages());
    fs::rename(
        home.client("_classic_era_"),
        home.env.root().join("era-unplugged"),
    )
    .unwrap();
    let ran = json(&log, &home, &["doctor"]);
    log.assert_eq(
        "no finding for a missing client",
        &findings_of(&ran, PAST_RETENTION).len(),
        &0,
    );
    let ran = json(&log, &home, &["status"]);
    log.assert_eq("null for it", &undo_of(&ran, "era"), &Value::Null);
}
