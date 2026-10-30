//! e2e suite: plans show reconciles a stored plan from history: a lost-output apply, a partial one, a plan pending then expired, a wiped plan store, ambiguity and short prefixes, history past its bound, unreadable plan files, matching versions that are no evidence, an interrupted apply, and every run read-only, offline and deterministic.
#![cfg(test)]

mod common;

use std::fs;

use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use ulid::Ulid;

use common::archives::zip_text;
use common::home::Home;
use common::http::paths;
use common::pause::Pause;
use common::plans::{
    age_plan, apply_with_stdout_closed, behind_everywhere, committed, evidence_txs,
    history_txs_for, plan_id, plans_dir, plant_lines, read_only, read_plan, show, show_human,
};
use common::state::record;
use common::tlog::{Ran, TestLog, tlog};
use omaddons::limits::{LOCK_ACQUIRE_TIMEOUT, PLAN_EVIDENCE_LINES_MAX, PLAN_MAX_BYTES};
use omaddons::state::paths::Paths;

/// A plan id no home here has stored or applied.
const UNKNOWN: &str = "01M8Q2VA7K3N5P8R1T4W6X9Y0Z";

/// The keys `plans show` takes from the plan's file: all `null` when it did not read.
const FILE_KEYS: [&str; 6] = [
    "intent",
    "approval",
    "created_at",
    "expires_at",
    "build_id",
    "build_matches",
];

// ---------------------------------------------------------------------------------------
// Support.

/// Writes a zip of the addon `name` (one folder that retail loads) into the home's
/// directory; returns its path.
fn addon_zip(home: &Home, name: &str) -> String {
    let toc = format!("{name}/{name}.toc");
    let toc_text = format!("## Interface: 120105\n## Title: {name}\n## Version: 1\n{name}.lua\n");
    let lua = format!("{name}/{name}.lua");
    let path = home
        .env
        .root()
        .join(format!("{}.zip", name.to_ascii_lowercase()));
    fs::write(
        &path,
        zip_text(&[(&toc, &toc_text), (&lua, "local loaded = true\n")]),
    )
    .unwrap();
    path.display().to_string()
}

/// A retail client with zips of Foo and of an unrelated Bar in the home: installs from
/// `--file` need no network.
struct Local {
    home: Home,
    foo: String,
    bar: String,
}

fn local(log: &TestLog) -> Local {
    let home = Home::new(&["wow"]);
    log.env(&home);
    let foo = addon_zip(&home, "Foo");
    let bar = addon_zip(&home, "Bar");
    Local { home, foo, bar }
}

impl Local {
    /// Previews installing Foo from its zip: a dry run that stores a plan.
    fn preview(&self, log: &TestLog) -> Ran {
        log.step("preview");
        let ran = log.run(
            &self.home,
            &[
                "install",
                "gh:o/foo",
                "--file",
                &self.foo,
                "-t",
                "retail",
                "--dry-run",
                "--json",
            ],
        );
        ran.ok();
        log.json("approval", &ran.data()["approval"]);
        ran
    }

    /// Installs Bar from its zip: a change on retail that carries no plan.
    fn install_bar(&self, log: &TestLog) -> Ran {
        log.run(
            &self.home,
            &[
                "install", "gh:o/bar", "--file", &self.bar, "-t", "retail", "--json",
            ],
        )
    }
}

/// Previews `update` on [`behind_everywhere`]'s home: Foo v1 → v2 on both clients.
fn preview_update(log: &TestLog, home: &Home) -> String {
    log.step("preview");
    let ran = log.run(home, &["update", "--dry-run", "--json"]);
    ran.ok();
    log.json("approval", &ran.data()["approval"]);
    plan_id(&ran)
}

/// `apply <id>`, which must succeed.
fn apply(log: &TestLog, home: &Home, id: &str) -> Ran {
    log.step("apply");
    let ran = log.run(home, &["apply", id, "--json"]);
    ran.ok();
    log.json("outcomes", &ran.data()["outcomes"]);
    ran
}

/// Another plan id made in the same millisecond as `id`: they share their first 10
/// characters.
fn sibling_of(id: &str) -> Ulid {
    let ulid: Ulid = id.parse().unwrap();
    Ulid::from_parts(ulid.timestamp_ms(), ulid.random() ^ 1)
}

/// `t + d`, as `OMADDONS_NOW` takes it.
fn at(t: Timestamp, d: SignedDuration) -> String {
    t.checked_add(d).unwrap().to_string()
}

/// The envelope's bytes before its `meta`, the one part that may differ between runs.
fn without_meta(stdout: &str) -> &str {
    stdout
        .rfind(",\"meta\":")
        .map_or(stdout, |at| &stdout[..at])
}

#[track_caller]
fn says(ran: &Ran, text: &str) {
    assert!(
        ran.stdout().contains(text),
        "the human view does not say {text:?}:\n{}",
        ran.stdout()
    );
}

/// What `plans show` reports of a plan file it did not read: every key it would take from
/// the file `null`, and no input.
fn file_keys_null(log: &TestLog, data: &Value) {
    for key in FILE_KEYS {
        log.assert_eq(&format!("{key} is null"), &data[key], &Value::Null);
    }
    log.assert_eq("no inputs", &data["inputs"], &json!([]));
}

/// `ref_ambiguous` for `prefix`, naming `ids` (sorted): the first as the fix, each other
/// one an alternative, with `whose` in the message.
fn ambiguous(log: &TestLog, ran: &Ran, prefix: &str, ids: &[String], whose: &str) {
    ran.fails(3, "ref_ambiguous");
    let error = &ran.envelope()["error"];
    let show = |id: &String| json!(["omaddons", "plans", "show", id]);
    log.assert_eq(
        "the first candidate is the fix",
        &error["fix_argv"],
        &show(&ids[0]),
    );
    let alternatives: Vec<Value> = error["alternatives"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| a["argv"].clone())
        .collect();
    log.assert_eq(
        "each other candidate is an alternative",
        &alternatives,
        &ids[1..].iter().map(show).collect::<Vec<_>>(),
    );
    log.assert_eq(
        "details",
        &error["details"],
        &json!({"prefix": prefix, "candidates": ids}),
    );
    let message = error["message"].as_str().unwrap_or_default();
    assert!(message.contains(whose), "{message}");
}

/// The history line that carries plan `id`.
fn plan_line(paths: &Paths, id: &str) -> String {
    let marker = format!("\"plan\":\"{id}\"");
    fs::read_to_string(paths.history_file())
        .unwrap()
        .lines()
        .find(|line| line.contains(&marker))
        .unwrap_or_else(|| panic!("no history line carries plan {id}")) // ubs:ignore — the assertion
        .to_owned()
}

// ---------------------------------------------------------------------------------------
// Scenarios.

#[test]
fn lost_output_apply_is_completed() {
    let log = tlog!();
    log.step("build");
    let home = behind_everywhere(&log);
    let id = preview_update(&log, &home);

    log.step("apply with stdout closed");
    apply_with_stdout_closed(&log, &home, &id);

    log.step("reconcile");
    let ran = show(&log, &home, &[&id], &[]);
    ran.ok();
    let data = ran.data();
    log.json("plans show", data);
    log.assert_eq("state", &data["state"], &json!("completed"));
    log.assert_eq("stored", &data["stored"], &json!(true));
    log.assert_eq("readable", &data["readable"], &json!(true));
    log.assert_eq(
        "evidence complete",
        &data["evidence"]["complete"],
        &json!(true),
    );
    log.assert_eq("intent", &data["intent"]["command"], &json!("update"));
    let evidence = evidence_txs(&ran);
    let mut clients: Vec<&str> = evidence.iter().map(|(t, _)| t.as_str()).collect();
    clients.sort_unstable();
    log.assert_eq("one commit per client", &clients, &vec!["era", "retail"]);
    log.assert_eq(
        "exactly the history lines carrying the plan",
        &evidence,
        &history_txs_for(&log, &home, &id),
    );
    let hints = ran.commands();
    let actions: Vec<&str> = hints.iter().map(|c| c.action.as_str()).collect();
    log.assert_eq(
        "the run's undo, then each transaction's",
        &actions,
        &vec!["undo_run", "undo", "undo"],
    );
    let mut undone: Vec<String> = hints[1..].iter().map(|c| c.argv[2].clone()).collect();
    let mut txs: Vec<String> = evidence.iter().map(|(_, tx)| tx.clone()).collect();
    undone.sort();
    txs.sort();
    log.assert_eq("each undo names a committed transaction", &undone, &txs);

    log.step("human");
    let human = show_human(&log, &home, &[&id], &[]);
    human.ok();
    says(&human, "completed, 2 of 2 clients committed");
    for tx in &txs {
        says(&human, &format!("undo: omaddons undo {tx}"));
    }
}

#[test]
fn partial_apply_is_partial() {
    let log = tlog!();
    log.step("build");
    let home = behind_everywhere(&log);
    let id = preview_update(&log, &home);

    log.step("apply, second commit fails");
    let ran = log
        .cmd(&home, &["apply", &id, "--json"])
        .failpoints("engine.lock.acquired=error@2")
        .run();
    ran.ok();
    log.json("outcomes", &ran.data()["outcomes"]);
    let first = committed(&ran);
    log.assert_eq("exactly one client committed", &first.len(), &1);
    let other = if first[0].0 == "era" { "retail" } else { "era" };

    log.step("reconcile");
    let ran = show(&log, &home, &[&id], &[]);
    ran.ok();
    log.assert_eq("state", &ran.data()["state"], &json!("partial"));
    log.assert_eq(
        "the one commit is the evidence",
        &evidence_txs(&ran),
        &first,
    );
    let human = show_human(&log, &home, &[&id], &[]);
    human.ok();
    says(&human, "partial, 1 of 2 clients committed");
    says(&human, &format!("not recorded: {other}"));
    says(
        &human,
        "preview again for the rest: omaddons update --dry-run",
    );

    log.step("apply again");
    let again = log.run(&home, &["apply", &id, "--json"]);
    again.fails(1, "plan_stale");
    log.assert_eq(
        "already applied",
        &again.envelope()["error"]["details"]["reason"],
        &json!("already_applied"),
    );
    let ran = show(&log, &home, &[&id], &[]);
    ran.ok();
    log.assert_eq("still partial", &ran.data()["state"], &json!("partial"));
    log.assert_eq("the same evidence", &evidence_txs(&ran), &first);
}

#[test]
fn unapplied_plan_is_pending_then_expired() {
    let log = tlog!();
    log.step("build");
    let l = local(&log);
    let preview = l.preview(&log);
    let id = plan_id(&preview);
    let expires: Timestamp = preview.data()["plan_expires_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    log.step("pending");
    let ran = show(&log, &l.home, &[&id], &[]);
    ran.ok();
    log.assert_eq("state", &ran.data()["state"], &json!("pending"));
    log.assert_eq(
        "no evidence, the whole window read",
        &ran.data()["evidence"],
        &json!({"targets": [], "complete": true}),
    );
    let human = show_human(&log, &l.home, &[&id], &[]);
    human.ok();
    says(&human, &format!("apply it with: omaddons apply {id}"));

    log.step("expired");
    let later = at(expires, SignedDuration::from_hours(1));
    let ran = show(&log, &l.home, &[&id], &[("OMADDONS_NOW", &later)]);
    ran.ok();
    log.assert_eq("state", &ran.data()["state"], &json!("expired"));
    let human = show_human(&log, &l.home, &[&id], &[("OMADDONS_NOW", &later)]);
    human.ok();
    says(&human, "expired at ");

    // Both commands ask `StoredPlan::is_expired`. The instant at which apply still runs
    // the plan comes last: that apply commits it.
    for (label, now, expired) in [
        (
            "a nanosecond past the expiry",
            at(expires, SignedDuration::from_nanos(1)),
            true,
        ),
        ("at the expiry", expires.to_string(), false),
    ] {
        log.step(&format!("{label}: plans show and apply agree"));
        let ran = show(&log, &l.home, &[&id], &[("OMADDONS_NOW", &now)]);
        ran.ok();
        let state = if expired { "expired" } else { "pending" };
        log.assert_eq("plans show's state", &ran.data()["state"], &json!(state));
        let applied = log
            .cmd(&l.home, &["apply", &id, "--json"])
            .env("OMADDONS_NOW", &now)
            .run();
        if expired {
            applied.fails(1, "plan_stale");
            log.assert_eq(
                "apply refuses it as expired",
                &applied.envelope()["error"]["details"]["reason"],
                &json!("expired"),
            );
        } else {
            applied.ok();
            log.assert_eq("apply runs it", &committed(&applied).len(), &1);
        }
    }
}

#[test]
fn deleted_cache_still_shows_the_commits() {
    let log = tlog!();
    log.step("build");
    let home = behind_everywhere(&log);
    let id = preview_update(&log, &home);
    apply(&log, &home, &id);
    let history = history_txs_for(&log, &home, &id);
    log.assert_eq("two commits", &history.len(), &2);

    log.step("wipe the plan store");
    fs::remove_dir_all(plans_dir(&home.env)).unwrap();

    log.step("full id");
    let ran = show(&log, &home, &[&id], &[]);
    ran.ok();
    let data = ran.data();
    log.assert_eq("state", &data["state"], &json!("unknown"));
    log.assert_eq("stored", &data["stored"], &json!(false));
    log.assert_eq("readable", &data["readable"], &json!(false));
    file_keys_null(&log, data);
    log.assert_eq("evidence", &evidence_txs(&ran), &history);
    log.assert_eq("complete", &data["evidence"]["complete"], &json!(true));
    let human = show_human(&log, &home, &[&id], &[]);
    human.ok();
    says(&human, "its file is no longer in the cache");

    log.step("prefix through history");
    let ran = show(&log, &home, &[&id[..10]], &[]);
    ran.ok();
    log.assert_eq("the plan history names", &ran.data()["plan_id"], &json!(id));
    log.assert_eq("the same evidence", &evidence_txs(&ran), &history);
}

#[test]
fn no_file_and_no_evidence_is_plan_not_found() {
    let log = tlog!();
    {
        let log = log.case("unknown_id");
        let home = Home::new(&["wow"]);
        log.env(&home);
        let ran = show(&log, &home, &[UNKNOWN], &[]);
        ran.fails(3, "plan_not_found");
        let error = &ran.envelope()["error"];
        log.assert_eq(
            "fix",
            &error["fix_argv"],
            &json!(["omaddons", "help", "plans", "show"]),
        );
        log.assert_eq("details", &error["details"], &json!({"plan_id": UNKNOWN}));
    }
    {
        let log = log.case("short_prefix");
        let home = Home::new(&["wow"]);
        log.env(&home);
        show(&log, &home, &[&UNKNOWN[..7]], &[]).fails(2, "usage");
    }
    {
        let log = log.case("two_stored_plans_share_a_prefix");
        let l = local(&log);
        let id = plan_id(&l.preview(&log));
        log.step("store a second plan made in the same millisecond");
        let sibling = sibling_of(&id);
        let template = read_plan(&l.home.env, &id);
        let created: Timestamp = template["created_at"].as_str().unwrap().parse().unwrap();
        age_plan(&l.home.env, &template, sibling, created);
        let mut ids = vec![id.clone(), sibling.to_string()];
        ids.sort();
        log.step("show by the shared prefix");
        let ran = show(&log, &l.home, &[&id[..10]], &[]);
        ambiguous(&log, &ran, &id[..10], &ids, "stored plans have");
    }
    {
        let log = log.case("two_history_ids_share_a_prefix");
        let l = local(&log);
        let id = plan_id(&l.preview(&log));
        apply(&log, &l.home, &id);
        log.step("plant a sibling plan's line and wipe the plan store");
        let paths = paths(&l.home.env);
        let sibling = sibling_of(&id).to_string();
        let line = plan_line(&paths, &id);
        let planted = line.replace(
            &format!("\"plan\":\"{id}\""),
            &format!("\"plan\":\"{sibling}\""),
        );
        assert_ne!(planted, line, "the line names its plan");
        plant_lines(&paths, &planted, 1);
        fs::remove_dir_all(plans_dir(&l.home.env)).unwrap();
        let mut ids = vec![id.clone(), sibling];
        ids.sort();
        log.step("show by the shared prefix");
        let ran = show(&log, &l.home, &[&id[..10]], &[]);
        ambiguous(&log, &ran, &id[..10], &ids, "plans in history have");
    }
    {
        let log = log.case("lowercase");
        let l = local(&log);
        let id = plan_id(&l.preview(&log));
        log.step("show by the lowercased id");
        let ran = show(&log, &l.home, &[&id.to_lowercase()], &[]);
        ran.ok();
        log.assert_eq("the plan", &ran.data()["plan_id"], &json!(id));
        log.assert_eq("state", &ran.data()["state"], &json!("pending"));
    }
}

#[test]
fn history_beyond_the_bound_is_incomplete() {
    let log = tlog!();
    log.step("build");
    let home = behind_everywhere(&log);
    let id = preview_update(&log, &home);
    let mut commits = committed(&apply(&log, &home, &id));
    commits.sort();

    log.step("a settings line to copy");
    log.run(
        &home,
        &["config", "set", "default_channel", "stable", "--json"],
    )
    .ok();
    let paths = paths(&home.env);
    let text = fs::read_to_string(paths.history_file()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let line = (*lines.last().unwrap()).to_owned();
    let settings: Value = serde_json::from_str(&line).unwrap();
    log.json("the line to plant", &settings);
    log.assert_eq("a settings line", &settings["kind"], &json!("config"));
    log.assert_eq(
        "it carries no plan",
        &settings.get("plan").cloned().unwrap_or(Value::Null),
        &Value::Null,
    );
    let marker = format!("\"plan\":\"{id}\"");
    let first = lines.iter().position(|l| l.contains(&marker)).unwrap();
    let last = lines.iter().rposition(|l| l.contains(&marker)).unwrap();
    // The scan reads `lines.len() - first` lines from the end to reach both commits.
    let to_both = lines.len() - first;
    let after_last = lines.len() - 1 - last;
    {
        let log = log.case("found_before_the_bound");
        log.step("plant lines until both commits are the last lines the scan reads");
        let n = PLAN_EVIDENCE_LINES_MAX - to_both;
        log.json("planted", &json!({"lines": n}));
        plant_lines(&paths, &line, n);
        log.step("reconcile");
        let ran = show(&log, &home, &[&id], &[]);
        ran.ok();
        log.assert_eq("state", &ran.data()["state"], &json!("completed"));
        log.assert_eq(
            "the bound cut the scan",
            &ran.data()["evidence"]["complete"],
            &json!(false),
        );
        let mut evidence = evidence_txs(&ran);
        evidence.sort();
        log.assert_eq("both commits", &evidence, &commits);
    }
    {
        let log = log.case("cut");
        log.step("plant lines until the bound stops the scan before the last commit");
        let after = after_last + (PLAN_EVIDENCE_LINES_MAX - to_both);
        let n = PLAN_EVIDENCE_LINES_MAX - after;
        log.json("planted", &json!({"lines": n}));
        plant_lines(&paths, &line, n);
        log.step("reconcile");
        let ran = show(&log, &home, &[&id], &[]);
        ran.ok();
        log.assert_eq(
            "no evidence, the scan cut",
            &ran.data()["evidence"],
            &json!({"targets": [], "complete": false}),
        );
        log.assert_eq("state", &ran.data()["state"], &json!("unknown"));
        let human = show_human(&log, &home, &[&id], &[]);
        human.ok();
        says(&human, "the search stopped there");
    }
}

#[test]
fn unreadable_plan_file_is_reported_never_trusted() {
    let log = tlog!();
    log.step("build");
    let l = local(&log);
    let id = plan_id(&l.preview(&log));
    let path = plans_dir(&l.home.env).join(format!("{id}.json"));
    let original = fs::read(&path).unwrap();
    let text = String::from_utf8(original.clone()).unwrap();
    let truncated = original[..original.len() / 2].to_vec();
    let brace = text.rfind('}').unwrap();
    let pad = " ".repeat(usize::try_from(PLAN_MAX_BYTES).unwrap());
    let oversized = format!("{}{pad}{}", &text[..brace], &text[brace..]).into_bytes();
    let edited = text.replace(
        "\"display_argv\":[\"omaddons\",\"install\"",
        "\"display_argv\":[\"omaddons\",\"remove\"",
    );
    assert_ne!(
        edited, text,
        "the plan's display_argv starts omaddons install"
    );
    for (name, bytes, why) in [
        ("truncated", truncated, "does not decode"),
        ("oversized", oversized, "larger than 256 KiB"),
        (
            "hand_edited_display_argv",
            edited.into_bytes(),
            "its display_argv is not its intent's",
        ),
    ] {
        let log = log.case(name);
        log.step("damage the plan file");
        fs::write(&path, &bytes).unwrap();
        log.step("reconcile");
        let ran = show(&log, &l.home, &[&id], &[]);
        ran.ok();
        let data = ran.data();
        log.assert_eq("state", &data["state"], &json!("unknown"));
        log.assert_eq("stored", &data["stored"], &json!(true));
        log.assert_eq("readable", &data["readable"], &json!(false));
        file_keys_null(&log, data);
        let human = show_human(&log, &l.home, &[&id], &[]);
        human.ok();
        says(&human, "its file cannot be read");
        says(&human, why);
        log.step("restore the plan file");
        fs::write(&path, &original).unwrap();
    }
}

#[test]
fn matching_versions_are_not_evidence() {
    let log = tlog!();
    log.step("build");
    let home = behind_everywhere(&log);
    log.step("preview");
    let preview = log.run(&home, &["update", "--dry-run", "--json"]);
    preview.ok();
    let id = plan_id(&preview);
    let selection = preview.data()["approval"]["selection"].clone();
    log.json("selection", &selection);

    log.step("a typed update, neither applied nor bound");
    log.run(&home, &["update", "--json"]).ok();

    log.step("every client holds the version the plan approved");
    let items = selection.as_array().unwrap();
    log.assert_eq("two clients approved", &items.len(), &2);
    for item in items {
        let target = item["target_id"].as_str().unwrap();
        let addon = item["addon"].as_str().unwrap();
        let version = record(&home, target)
            .addons
            .into_values()
            .find(|a| a.name == addon)
            .map(|a| a.version);
        log.assert_eq(
            &format!("{addon} on {target}"),
            &json!(version),
            &item["to_version"],
        );
    }

    log.step("reconcile");
    let ran = show(&log, &home, &[&id], &[]);
    ran.ok();
    log.assert_eq("state", &ran.data()["state"], &json!("pending"));
    log.assert_eq(
        "no evidence",
        &ran.data()["evidence"]["targets"],
        &json!([]),
    );
}

#[test]
fn interrupted_apply_is_unknown_until_recovered() {
    let log = tlog!();
    for (name, failpoint, recovered, after) in [
        (
            "rolled_forward",
            "engine.swapped=abort",
            "rolls it forward",
            "completed",
        ),
        (
            "rolled_back",
            "engine.journal.prepared=abort",
            "rolls it back",
            "pending",
        ),
    ] {
        let log = log.case(name);
        log.step("build");
        let l = local(&log);
        let id = plan_id(&l.preview(&log));

        log.step(&format!("apply aborted at {failpoint}"));
        log.cmd(&l.home, &["apply", &id, "--json"])
            .failpoints(failpoint)
            .run()
            .aborted();

        log.step("before recovery");
        let ran = show(&log, &l.home, &[&id], &[]);
        ran.ok();
        log.assert_eq("state", &ran.data()["state"], &json!("unknown"));
        log.assert_eq(
            "the journal is reported",
            &ran.warnings().contains(&"journal_pending".to_owned()),
            &true,
        );
        let human = show_human(&log, &l.home, &[&id], &[]);
        human.ok();
        says(&human, "an interrupted transaction");

        log.step(&format!("recover: a change on retail {recovered}"));
        let ran = l.install_bar(&log);
        ran.ok();
        log.assert_eq(
            "the journal was recovered",
            &ran.warnings().contains(&"journal_recovered".to_owned()),
            &true,
        );

        log.step("after");
        let ran = show(&log, &l.home, &[&id], &[]);
        ran.ok();
        log.assert_eq("state", &ran.data()["state"], &json!(after));
        let evidence = evidence_txs(&ran);
        log.assert_eq(
            "the evidence is history's",
            &evidence,
            &history_txs_for(&log, &l.home, &id),
        );
        let expected = usize::from(after == "completed");
        log.assert_eq("lines carrying the plan", &evidence.len(), &expected);
    }
}

#[test]
fn plans_show_is_read_only_offline_and_deterministic() {
    let log = tlog!();
    log.step("build");
    let home = behind_everywhere(&log);
    let bar = addon_zip(&home, "Bar");
    let id = preview_update(&log, &home);
    apply(&log, &home, &id);
    log.step("baseline");
    let baseline = show(&log, &home, &[&id], &[]);
    baseline.ok();
    let data = baseline.data().clone();
    log.assert_eq("state", &data["state"], &json!("completed"));
    {
        let log = log.case("offline");
        let ran = show(
            &log,
            &home,
            &[&id, "--offline"],
            &[("OMADDONS_OFFLINE", "1")],
        );
        ran.ok();
        log.assert_eq("the same data", ran.data(), &data);
    }
    {
        let log = log.case("dry_run_and_reason");
        let ran = show(&log, &home, &[&id, "--dry-run", "--reason", "x"], &[]);
        ran.ok();
        log.assert_eq(
            "the same envelope but meta",
            &without_meta(ran.stdout()),
            &without_meta(baseline.stdout()),
        );
    }
    {
        let log = log.case("if_match");
        let token = format!("pc1:{}", "0".repeat(64));
        let ran = show(&log, &home, &[&id, "--if-match", &token], &[]);
        ran.fails(2, "usage");
        log.assert_eq(
            "the fix drops the flag",
            &ran.envelope()["error"]["fix_argv"],
            &json!(["omaddons", "plans", "show", id]),
        );
    }
    {
        let log = log.case("bare_plans");
        let ran = read_only(&log, &home, &["plans", "--json"], &[]);
        ran.fails(2, "usage");
        log.assert_eq("command", &ran.envelope()["command"], &json!("plans show"));
        log.assert_eq(
            "fix",
            &ran.envelope()["error"]["fix_argv"],
            &json!(["omaddons", "help", "plans"]),
        );
    }
    {
        let log = log.case("target_flag");
        let ran = show(&log, &home, &[&id, "-t", "retail"], &[]);
        ran.ok();
        log.assert_eq("the same data", ran.data(), &data);
    }
    {
        let log = log.case("twice");
        let first = show(&log, &home, &[&id], &[]);
        let second = show(&log, &home, &[&id], &[]);
        first.ok();
        second.ok();
        log.assert_eq(
            "identical bytes but meta",
            &without_meta(first.stdout()),
            &without_meta(second.stdout()),
        );
    }
    {
        let log = log.case("verbose");
        let ran = show_human(&log, &home, &[&id, "-v"], &[]);
        ran.ok();
        says(&ran, "history: read ");
        says(&ran, " lines back to ");
    }
    {
        let log = log.case("while_the_lock_is_held");
        let pause = Pause::new(home.env.root());
        log.step("hold the global lock in another process");
        let holder = log
            .cmd(
                &home,
                &[
                    "install", "gh:o/bar", "--file", &bar, "-t", "retail", "--json",
                ],
            )
            .failpoints("engine.lock.acquired=pause")
            .pause(&pause)
            .spawn();
        log.reached(&pause, "engine.lock.acquired");
        log.step("reconcile while it waits");
        let ran = show(&log, &home, &[&id], &[]);
        // Released before this test's own assertions; a failure inside `show` drops the
        // holder's `Running`, which kills it, so nothing waits for the pause's limit.
        log.resume(&pause, "engine.lock.acquired");
        holder.wait().ok();
        ran.ok();
        log.assert_eq("the same data", ran.data(), &data);
        assert!(
            u128::from(ran.elapsed_ms) < LOCK_ACQUIRE_TIMEOUT.as_millis(),
            "plans show took {} ms: it waited for the lock",
            ran.elapsed_ms
        );
    }
}
