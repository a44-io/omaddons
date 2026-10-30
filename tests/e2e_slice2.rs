//! e2e suite: slice 2's exit demo (preview, then apply exactly)
//!
//! One `demo_*` test per item of slice 2's exit demo, each run end to end through the real
//! binary on a synthetic home, logged with `TestLog`: install then `update --dry-run` then
//! `apply`; a release landing in between; a late hand edit under the lock; a second `apply`;
//! two appliers; `plans show` after a lost output; a Tukui plan from the cache; the widget's
//! Update scenarios. The slice's demo is recorded from a run of `argc e2e slice2`.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use common::home::Home;
use common::landing::{Landing, Source};
use common::memo;
use common::netlog::net_log;
use common::plans;
use common::tlog::tlog;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use support::{
    Demo, apply_hint, assert_nothing_changed, assert_release, assert_untouched, baseline, behind,
    expected_fix, outcomes, regular_files, requests, selected, stale,
};

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};

    use omaddons::app::precondition::preview_argv;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    use crate::common::home::Home;
    use crate::common::landing::{Landing, Source};
    use crate::common::memo;
    use crate::common::netlog::{NetLogView, net_log};
    use crate::common::plans;
    use crate::common::tlog::{Ran, TestLog};

    /// A home and the landing that serves its addon.
    pub struct Demo {
        pub home: Home,
        pub landing: Landing,
    }

    /// Step "build": release 1 of `source`'s addon installed on each of `targets`, then
    /// release 2 published as the newest and the release memo a day old, so the next
    /// `update` asks the source again.
    pub fn behind(log: &TestLog, products: &[&str], targets: &[&str], source: Source) -> Demo {
        log.step("build");
        let mut home = Home::new(products);
        log.env(&home);
        let landing = Landing::start(&mut home, source);
        let mut demo = Demo { home, landing };
        for &target in targets {
            let r#ref = demo.landing.r#ref();
            log.run(&demo.home, &["install", r#ref, "-t", target, "--json"])
                .ok();
            assert_release(log, &demo, target, 1);
        }
        let version = demo.landing.land(&mut demo.home);
        memo::age(&demo.home, 24);
        log.note(format!(
            "landed {} release 2 ({version}); the release memo is a day old",
            source.label()
        ));
        demo
    }

    /// The client folder of a target id the demo homes have.
    pub fn folder(target: &str) -> &'static str {
        match target {
            "retail" => "_retail_",
            "classic" => "_classic_",
            other => panic!("no client folder is known for target {other}"), // ubs:ignore — a builder bug
        }
    }

    /// The landing's marker for release `n` is on `target`'s AddOns, with its content.
    #[track_caller]
    pub fn assert_release(log: &TestLog, demo: &Demo, target: &str, n: u32) {
        let Some((path, expected)) = demo.landing.marker(n) else {
            let source = demo.landing.source().label();
            panic!("the {source} landing has no marker for release {n}"); // ubs:ignore — a builder bug
        };
        let file = demo.home.addons(folder(target)).join(&path);
        let actual = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
            Err(error) => panic!("read {}: {error}", file.display()), // ubs:ignore — the assertion
        };
        log.assert_eq(
            &format!("{target}: {path} is release {n}"),
            &actual,
            &expected,
        );
    }

    /// What a refused run must leave as it was: each target's AddOns and the records (as
    /// trees in the log), history's length and every plan file's digest.
    pub struct Baseline {
        pub history: usize,
        pub plans: BTreeMap<String, String>,
    }

    pub fn baseline(log: &TestLog, home: &Home, targets: &[&str]) -> Baseline {
        for target in targets {
            log.tree(&format!("addons {target}"), &home.addons(folder(target)));
        }
        log.tree("records", &home.state().join("targets"));
        let baseline = Baseline {
            history: history_lines(home),
            plans: plan_digests(home),
        };
        log.json(
            "baseline",
            &json!({"history_lines": baseline.history, "plan_files": baseline.plans}),
        );
        baseline
    }

    /// Each target's AddOns, the records and history's length are as `before` found them
    /// (a dry run may still store its plan).
    #[track_caller]
    pub fn assert_untouched(log: &TestLog, home: &Home, targets: &[&str], before: &Baseline) {
        for target in targets {
            log.assert_unchanged(&format!("addons {target}"), &home.addons(folder(target)));
        }
        log.assert_unchanged("records", &home.state().join("targets"));
        log.assert_eq("history lines", &history_lines(home), &before.history);
    }

    /// [`assert_untouched`], and every plan file is byte for byte as it was.
    #[track_caller]
    pub fn assert_nothing_changed(log: &TestLog, home: &Home, targets: &[&str], before: &Baseline) {
        assert_untouched(log, home, targets, before);
        log.assert_eq("plan files", &plan_digests(home), &before.plans);
    }

    /// The non-empty lines of `<state>/omaddons/history.jsonl`; 0 when it is absent.
    pub fn history_lines(home: &Home) -> usize {
        let path = home.state().join("history.jsonl");
        match fs::read_to_string(&path) {
            Ok(text) => text.lines().filter(|line| !line.trim().is_empty()).count(),
            Err(error) if error.kind() == ErrorKind::NotFound => 0,
            Err(error) => panic!("read {}: {error}", path.display()), // ubs:ignore — the assertion
        }
    }

    /// Each plan file's name and the SHA-256 of its bytes (leases left out).
    pub fn plan_digests(home: &Home) -> BTreeMap<String, String> {
        plans::plan_files(&home.env)
            .iter()
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, sha256(&fs::read(path).unwrap()))
            })
            .collect()
    }

    fn sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// The `net.log` entries after the first `before`, logged before anything counts them.
    pub fn requests(log: &TestLog, home: &Home, before: usize) -> NetLogView {
        let mut view = net_log(home);
        view.entries = view.entries.split_off(before);
        log.note(format!(
            "{} requests since the mark:\n{}",
            view.len(),
            view.context()
        ));
        view
    }

    /// The fix a lifecycle or selection refusal of `plan` carries: its preview again.
    pub fn expected_fix(home: &Home, plan: &str) -> Vec<String> {
        let display = plans::read_plan(&home.env, plan)["display_argv"].clone();
        let display: Vec<String> = serde_json::from_value(display).unwrap();
        preview_argv(&display)
    }

    /// `plan_stale`, exit 1 and not retryable: the refusal's `details`, the error logged.
    #[track_caller]
    pub fn stale(log: &TestLog, ran: &Ran) -> Value {
        ran.fails(1, "plan_stale");
        let error = ran.envelope()["error"].clone();
        log.json("refusal", &error);
        log.assert_eq("retryable", &error["retryable"], &json!(false));
        error["details"].clone()
    }

    /// `commands[]` offers exactly `omaddons apply <id>`.
    pub fn apply_hint(ran: &Ran, id: &str) -> bool {
        ran.commands()
            .iter()
            .any(|command| command.action == "apply" && command.argv == ["omaddons", "apply", id])
    }

    /// `data.outcomes[]` as `(target, status, tx)`, logged.
    pub fn outcomes(log: &TestLog, ran: &Ran) -> Vec<(String, String, Option<String>)> {
        let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
        let outcomes: Vec<_> = ran.data()["outcomes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|outcome| {
                (
                    text(&outcome["target"]),
                    text(&outcome["status"]),
                    outcome["tx"].as_str().map(str::to_owned),
                )
            })
            .collect();
        log.json("outcomes", &outcomes);
        outcomes
    }

    /// The one change `approval.selection` holds; the test fails when it holds another
    /// number.
    #[track_caller]
    pub fn selected(log: &TestLog, approval: &Value) -> Value {
        let selection = approval["selection"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        log.assert_eq("changes approved", &selection.len(), &1);
        selection[0].clone()
    }

    /// Every regular file under `dir`, symlinks not followed; none when `dir` is absent.
    pub fn regular_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return,
            Err(error) => panic!("list {}: {error}", dir.display()), // ubs:ignore — the assertion
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_dir() {
                regular_files(&path, out);
            } else if kind.is_file() {
                out.push(path);
            }
        }
    }
}

/// A fresh install and an update, each previewed with `--dry-run` and applied through its
/// stored plan; the human preview names the same plan, `apply --dry-run` re-checks it
/// without changing anything, and history records the approval the update ran.
#[test]
fn demo_install_then_update_preview_then_apply() {
    let log = tlog!();
    log.step("build");
    let mut home = Home::new(&["wow"]);
    log.env(&home);
    let landing = Landing::start(&mut home, Source::GitHub);
    let mut demo = Demo { home, landing };

    log.step("preview the install");
    let preview = log.run(
        &demo.home,
        &["install", "gh:o/foo", "-t", "retail", "--dry-run", "--json"],
    );
    preview.ok();
    let p1 = plans::plan_id(&preview);
    let approval = preview.data()["approval"].clone();
    log.json("approval", &approval);
    let item = selected(&log, &approval);
    log.assert_eq(
        "an install of release 1 on retail",
        &(&item["op"], &item["target_id"], &item["to_version"]),
        &(
            &json!("install"),
            &json!("retail"),
            &json!(demo.landing.version(1)),
        ),
    );
    log.assert_eq(
        "commands[] offers its apply",
        &apply_hint(&preview, &p1),
        &true,
    );
    let file = plans::plans_dir(&demo.home.env).join(format!("{p1}.json"));
    let mode = fs::metadata(&file)
        .ok()
        .map(|meta| format!("{:o}", meta.permissions().mode() & 0o777));
    log.assert_eq("the plan file's mode", &mode, &Some("600".to_owned()));
    log.assert_eq(
        "nothing installed yet",
        &demo.home.addons("_retail_").join("Foo").exists(),
        &false,
    );

    log.step("apply the install");
    let applied = log.run(&demo.home, &["apply", &p1, "--json"]);
    applied.ok();
    log.assert_eq(
        "the applied command",
        &applied.data()["applied"]["command"],
        &json!("install"),
    );
    let done = outcomes(&log, &applied);
    let committed: Vec<_> = done
        .iter()
        .map(|(target, status, tx)| (target.as_str(), status.as_str(), tx.is_some()))
        .collect();
    log.assert_eq(
        "committed on retail",
        &committed,
        &vec![("retail", "committed", true)],
    );
    assert_release(&log, &demo, "retail", 1);

    log.step("a release lands");
    let version = demo.landing.land(&mut demo.home);
    memo::age(&demo.home, 24);
    log.note(format!(
        "landed github release 2 ({version}); the release memo is a day old"
    ));

    log.step("preview the update");
    let before = baseline(&log, &demo.home, &["retail"]);
    let preview = log.run(
        &demo.home,
        &["update", "foo", "-t", "retail", "--dry-run", "--json"],
    );
    preview.ok();
    let p2 = plans::plan_id(&preview);
    let approval = preview.data()["approval"].clone();
    log.json("approval", &approval);
    let item = selected(&log, &approval);
    log.assert_eq(
        "an update from release 1 to release 2",
        &(&item["op"], &item["from_version"], &item["to_version"]),
        &(
            &json!("update"),
            &json!(demo.landing.version(1)),
            &json!(demo.landing.version(2)),
        ),
    );
    log.assert_eq(
        "routine, with no reasons",
        &(&approval["routine"], &approval["reasons"]),
        &(&json!(true), &json!([])),
    );
    log.assert_eq(
        "commands[] offers its apply",
        &apply_hint(&preview, &p2),
        &true,
    );
    let plan = plans::read_plan(&demo.home.env, &p2);
    log.json("plan", &plan);
    assert_untouched(&log, &demo.home, &["retail"], &before);

    log.step("the human preview names the same plan");
    let human = log.run(&demo.home, &["update", "foo", "-t", "retail", "--dry-run"]);
    human.exit(0);
    let line = format!("Plan {p2} stored for 24 hours.");
    log.note(format!("looking for: {line}"));
    log.assert_eq(
        "stdout names the stored plan",
        &human.stdout().contains(&line),
        &true,
    );
    let stored: BTreeSet<String> = plans::plan_files(&demo.home.env)
        .iter()
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    log.assert_eq(
        "one plan file each for P1 and P2 (the identical preview reused P2)",
        &stored,
        &BTreeSet::from([p1.clone(), p2.clone()]),
    );

    log.step("re-check the plan without changing anything");
    let before = baseline(&log, &demo.home, &["retail"]);
    let check = log.run(&demo.home, &["apply", &p2, "--dry-run", "--json"]);
    check.ok();
    log.assert_eq(
        "approved is the preview's approval",
        &check.data()["approved"],
        &approval,
    );
    log.assert_eq(
        "the result is a dry run",
        &check.data()["result"]["dry_run"],
        &json!(true),
    );
    log.assert_eq(
        "every outcome unchanged, with no transaction",
        &outcomes(&log, &check),
        &vec![("retail".to_owned(), "unchanged".to_owned(), None)],
    );
    assert_nothing_changed(&log, &demo.home, &["retail"], &before);
    log.assert_eq(
        "no lease was taken",
        &plans::lease_path(&demo.home.env, &p2).exists(),
        &false,
    );

    log.step("apply the update");
    let mark = net_log(&demo.home).len();
    let applied = log.run(&demo.home, &["apply", &p2, "--json"]);
    applied.ok();
    log.assert_eq(
        "the applied command and argv",
        &(
            &applied.data()["applied"]["command"],
            &applied.data()["applied"]["display_argv"],
        ),
        &(&json!("update"), &plan["display_argv"]),
    );
    let done = outcomes(&log, &applied);
    log.assert_eq("one outcome", &done.len(), &1);
    let (target, status, tx2) = done[0].clone();
    log.assert_eq(
        "committed on retail",
        &(target.as_str(), status.as_str(), tx2.is_some()),
        &("retail", "committed", true),
    );
    let tx2 = tx2.unwrap_or_default();
    assert_release(&log, &demo, "retail", 2);
    let cost = requests(&log, &demo.home, mark);
    log.assert_eq(
        "one forced revalidation, and the archive the preview downloaded",
        &(
            cost.count_purpose("release-latest"),
            cost.count_purpose("asset"),
        ),
        &(1, 0),
    );
    let undo = applied
        .commands()
        .iter()
        .any(|command| command.action == "undo" && command.argv.contains(&tx2));
    log.assert_eq("commands[] offers the update's undo", &undo, &true);

    log.step("history records the approval");
    let history = log.run(&demo.home, &["history", "--json", "--limit", "5"]);
    history.ok();
    let entry = history.data()["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["tx"] == json!(tx2))
        .cloned()
        .unwrap_or(Value::Null);
    log.json("the update's history line", &entry);
    log.assert_eq(
        "it carries the plan and its display_argv",
        &(&entry["plan"], &entry["argv"]),
        &(&json!(p2), &plan["display_argv"]),
    );
    log.assert_eq(
        "history lines carrying P2",
        &plans::history_txs_for(&log, &demo.home, &p2),
        &vec![("retail".to_owned(), tx2.clone())],
    );
}

/// A release published after the preview makes `apply` stale (`selection`), never a
/// different install; the stale `apply` refreshed the memo and cached the new archive, so
/// the re-preview shows the new release without asking or downloading, and its plan
/// installs exactly that release.
#[test]
fn demo_a_release_landing_in_between_is_plan_stale_and_the_re_preview_shows_it() {
    let log = tlog!();
    let mut demo = behind(&log, &["wow"], &["retail"], Source::GitHub);

    log.step("preview");
    let preview = log.run(
        &demo.home,
        &["update", "foo", "-t", "retail", "--dry-run", "--json"],
    );
    preview.ok();
    let p = plans::plan_id(&preview);
    let item = selected(&log, &preview.data()["approval"]);
    log.assert_eq(
        "to release 2",
        &item["to_version"],
        &json!(demo.landing.version(2)),
    );

    log.step("a release lands after the preview");
    // Not aged: the landing is inside the 15-minute window, where only `apply`'s forced
    // revalidation can see it.
    let version = demo.landing.land(&mut demo.home);
    log.note(format!(
        "landed github release 3 ({version}) inside the window"
    ));

    log.step("apply the stale plan");
    let before = baseline(&log, &demo.home, &["retail"]);
    let mark = net_log(&demo.home).len();
    let ran = log.run(&demo.home, &["apply", &p, "--json"]);
    let details = stale(&log, &ran);
    log.assert_eq(
        "details.changed",
        &details["changed"],
        &json!(["selection"]),
    );
    log.assert_eq(
        "the fix previews the plan again",
        &ran.envelope()["error"]["fix_argv"],
        &json!(expected_fix(&demo.home, &p)),
    );
    assert_nothing_changed(&log, &demo.home, &["retail"], &before);
    assert_release(&log, &demo, "retail", 1);
    let cost = requests(&log, &demo.home, mark);
    log.assert_eq(
        "apply revalidated",
        &cost.count_purpose("release-latest"),
        &1,
    );

    log.step("the re-preview shows the new release");
    let mark = net_log(&demo.home).len();
    let again = log.run(
        &demo.home,
        &["update", "foo", "-t", "retail", "--dry-run", "--json"],
    );
    again.ok();
    let p3 = plans::plan_id(&again);
    // ubs:ignore — comparing plan ids, not secrets
    log.assert_eq("a new plan", &(p3 != p), &true);
    let item = selected(&log, &again.data()["approval"]);
    log.assert_eq(
        "to release 3",
        &item["to_version"],
        &json!(demo.landing.version(3)),
    );
    let cost = requests(&log, &demo.home, mark);
    log.assert_eq(
        "the memo and the cache the stale apply filled answered",
        &(
            cost.count_purpose("release-latest"),
            cost.count_purpose("asset"),
        ),
        &(0, 0),
    );

    log.step("apply the new plan");
    let applied = log.run(&demo.home, &["apply", &p3, "--json"]);
    applied.ok();
    let statuses: Vec<_> = outcomes(&log, &applied)
        .into_iter()
        .map(|(target, status, tx)| (target, status, tx.is_some()))
        .collect();
    log.assert_eq(
        "committed on retail",
        &statuses,
        &vec![("retail".to_owned(), "committed".to_owned(), true)],
    );
    assert_release(&log, &demo, "retail", 3);
    let lines = plans::history_txs_for(&log, &demo.home, &p3);
    let targets: Vec<&str> = lines.iter().map(|(target, _)| target.as_str()).collect();
    log.assert_eq("one history line, on retail", &targets, &vec!["retail"]);
}

/// A plan history records as applied is refused as `already_applied`, naming its
/// transaction, also an hour past its expiry; `plans show` agrees it completed.
#[test]
fn demo_a_second_apply_is_already_applied() {
    let log = tlog!();
    let demo = behind(&log, &["wow"], &["retail"], Source::GitHub);

    log.step("preview and apply");
    let preview = log.run(
        &demo.home,
        &["update", "foo", "-t", "retail", "--dry-run", "--json"],
    );
    preview.ok();
    let p = plans::plan_id(&preview);
    let applied = log.run(&demo.home, &["apply", &p, "--json"]);
    applied.ok();
    let done = outcomes(&log, &applied);
    let statuses: Vec<_> = done
        .iter()
        .map(|(target, status, tx)| (target.as_str(), status.as_str(), tx.is_some()))
        .collect();
    log.assert_eq(
        "committed on retail",
        &statuses,
        &vec![("retail", "committed", true)],
    );
    let tx = done[0].2.clone().unwrap_or_default();

    log.step("apply again");
    let before = baseline(&log, &demo.home, &["retail"]);
    let again = log.run(&demo.home, &["apply", &p, "--json"]);
    let details = stale(&log, &again);
    log.assert_eq(
        "already applied, naming its transaction",
        &(&details["reason"], &details["txs"]),
        &(&json!("already_applied"), &json!([tx])),
    );
    log.assert_eq(
        "the fix previews the plan again",
        &again.envelope()["error"]["fix_argv"],
        &json!(expected_fix(&demo.home, &p)),
    );
    assert_nothing_changed(&log, &demo.home, &["retail"], &before);
    assert_release(&log, &demo, "retail", 2);

    log.step("a day later it is still already applied");
    let expires = plans::read_plan(&demo.home.env, &p)["expires_at"]
        .as_str()
        .unwrap_or_default()
        .parse::<Timestamp>()
        .unwrap();
    let later = expires
        .checked_add(SignedDuration::from_hours(1))
        .unwrap()
        .to_string();
    log.note(format!(
        "the plan expires at {expires}; the clock reads {later}"
    ));
    let late = log
        .cmd(&demo.home, &["apply", &p, "--json"])
        .env("OMADDONS_NOW", &later)
        .run();
    let details = stale(&log, &late);
    log.assert_eq(
        "already_applied is checked before expiry",
        &details["reason"],
        &json!("already_applied"),
    );
    assert_nothing_changed(&log, &demo.home, &["retail"], &before);

    log.step("plans show agrees");
    let shown = plans::show(&log, &demo.home, &[&p], &[]);
    shown.ok();
    log.assert_eq(
        "completed, with the transaction",
        &(&shown.data()["state"], plans::evidence_txs(&shown)),
        &(&json!("completed"), vec![("retail".to_owned(), tx)]),
    );
}

/// A Tukui plan whose preview downloaded the archive applies from the blob cache: its
/// stored release has no URL, `apply` asks for the list only and resolves no download
/// link, and no download token is printed or kept anywhere.
#[test]
fn demo_a_tukui_plan_applies_from_the_cache() {
    let log = tlog!();
    log.step("build");
    let mut home = Home::new(&["wow"]);
    log.env(&home);
    let landing = Landing::start(&mut home, Source::Tukui);
    let demo = Demo { home, landing };
    let tokens = demo.landing.download_tokens(1);
    log.json("the landing's download tokens (synthetic)", &tokens);
    let mut seen: Vec<(String, String)> = Vec::new();

    log.step("preview downloads and caches the archive");
    let mark = net_log(&demo.home).len();
    let preview = log.run(
        &demo.home,
        &[
            "install",
            "tukui:elvui",
            "-t",
            "retail",
            "--dry-run",
            "--json",
        ],
    );
    preview.ok();
    seen.push(("preview stdout".to_owned(), preview.stdout().to_owned()));
    seen.push(("preview stderr".to_owned(), preview.stderr().to_owned()));
    let p = plans::plan_id(&preview);
    let cost = requests(&log, &demo.home, mark);
    log.assert_eq(
        "the list, a resolve, and one download",
        &(
            cost.count_purpose("tukui-list") >= 1,
            cost.count_purpose("tukui-addon") >= 1,
            cost.count_purpose("asset"),
        ),
        &(true, true, 1),
    );
    let releases = plans::read_plan(&demo.home.env, &p)["releases"].clone();
    log.json("plan releases", &releases);
    let stored: Vec<_> = releases
        .as_array()
        .into_iter()
        .flatten()
        .map(|release| {
            (
                release["source"].clone(),
                release["key"].clone(),
                release["url"].clone(),
            )
        })
        .collect();
    log.assert_eq(
        "one deferred Tukui release, stored without its URL",
        &stored,
        &vec![(json!("tukui"), json!("elvui"), Value::Null)],
    );

    log.step("apply from the cache");
    let mark = net_log(&demo.home).len();
    // `-v` makes the executed install report `asset_url`, so its `null` says that a
    // cache hit resolved no link, not that the key was left out.
    let applied = log.run(&demo.home, &["apply", &p, "--json", "-v"]);
    applied.ok();
    seen.push(("apply stdout".to_owned(), applied.stdout().to_owned()));
    seen.push(("apply stderr".to_owned(), applied.stderr().to_owned()));
    let statuses: Vec<_> = outcomes(&log, &applied)
        .into_iter()
        .map(|(target, status, tx)| (target, status, tx.is_some()))
        .collect();
    log.assert_eq(
        "committed on retail",
        &statuses,
        &vec![("retail".to_owned(), "committed".to_owned(), true)],
    );
    let item = &applied.data()["result"]["items"][0];
    log.json("the applied item", item);
    log.assert_eq(
        "-v in effect, and a cache hit resolves no URL",
        &(
            item.get("optional_dependencies").is_some(),
            &item["asset_url"],
        ),
        &(true, &Value::Null),
    );
    let cost = requests(&log, &demo.home, mark);
    log.assert_eq(
        "the list alone: no resolve, no download",
        &(
            cost.count_purpose("tukui-list"),
            cost.count_purpose("tukui-addon"),
            cost.count_purpose("asset"),
        ),
        &(1, 0, 0),
    );
    assert_release(&log, &demo, "retail", 1);

    log.step("no download token is kept or printed");
    let mut files = Vec::new();
    for dir in [
        demo.home.env.state(),
        demo.home.env.cache(),
        demo.home.env.config(),
    ] {
        regular_files(&dir, &mut files);
    }
    let net_log_searched = files
        .iter()
        .any(|path| path.file_name().is_some_and(|name| name == "net.log"));
    for path in &files {
        let bytes = fs::read(path).unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        seen.push((path.display().to_string(), text));
    }
    log.note(format!(
        "searched {} files under <state>, <cache> and <config>, and 4 output streams",
        files.len()
    ));
    log.assert_eq("net.log was searched", &net_log_searched, &true);
    let leaked: Vec<&str> = seen
        .iter()
        .filter(|(_, text)| tokens.iter().any(|token| text.contains(token.as_str())))
        .map(|(label, _)| label.as_str())
        .collect();
    log.assert_eq("no download token", &leaked, &Vec::<&str>::new());
}
