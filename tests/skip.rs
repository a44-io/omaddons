//! e2e suite: a release that was left is passed over until the next one
//!
//! The regression guard's slice 1 (RG-35) through the real binary, in the sequences a
//! person, an agent and the widget run: an undone update stays away (gap 2), the
//! widget's preview after an undo, resuming at the next release, the redo, `skip` and
//! `unskip` ahead of an update, an explicit version that wins, pins with skips, the
//! commands that leave marks alone, two clients, three addons in one undo, twelve marks
//! on one addon, one release skipped twice, a mark on one WowAce revision, a stored
//! skip plan gone stale, and the re-tag lifecycle on one WowAce tag. Every step that
//! should ask no source is held to `net.log`.
//!
//! The GitHub fixture is `common::releases` (an addon at `v0` and twelve releases after
//! it); the WowAce one is `common::wowace::tag_at` (`v12.1.1` at revisions 1390 and 1402).
//! The unit rules behind each step are `resolve::updates`, `engine::undo` and
//! `app::skip`'s; `tests/skip_command.rs` covers the commands one by one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::fs;

use common::binding::age_release_memo;
use common::home::Home;
use common::http::paths;
use common::netlog::net_log;
use common::releases::{RUN, newest, publish_run, tag};
use common::state::record;
use common::tlog::{Ran, TestLog, tlog};
use common::wowace::{serve_recorded, tag_at};
use omaddons::model::{ReleaseKey, SkippedRelease};
use serde_json::{Value, json};

/// `omaddons <args> --json`, for a step that may ask a source.
fn json(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv = args.to_vec();
    argv.push("--json");
    log.run(home, &argv)
}

/// `omaddons <args>` (`--json` when `as_json`), for a step that must ask no source:
/// `net.log` gains no line.
fn no_request(log: &TestLog, home: &Home, args: &[&str], as_json: bool) -> Ran {
    let before = net_log(home).len();
    let mut argv = args.to_vec();
    if as_json {
        argv.push("--json");
    }
    let ran = log.run(home, &argv);
    log.assert_eq(
        &format!("{}: no request", args.join(" ")),
        &net_log(home).len(),
        &before,
    );
    ran
}

/// [`no_request`] with `--json`.
fn quiet(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    no_request(log, home, args, true)
}

/// [`no_request`], human output.
fn quiet_human(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    no_request(log, home, args, false)
}

/// Asserts the human output of `ran` holds `line`.
fn says(log: &TestLog, ran: &Ran, line: &str) {
    log.assert_eq(
        &format!("says {line:?}"),
        &ran.stdout().contains(line),
        &true,
    );
}

/// A home where each of `repos` (`(repo, Name)`) was installed at `v0` on each client of
/// `targets`, its twelve later releases are published, and `latest` is the newest, with
/// the release memo aged: the next check asks GitHub. Mists Classic too when `two`.
fn home_at(
    log: &TestLog,
    two: bool,
    repos: &[(&str, &str)],
    targets: &[&str],
    latest: &str,
) -> Home {
    log.step(&format!(
        "{repos:?} at v0 on {targets:?}, {latest} the newest"
    ));
    let home = Home::new(if two {
        &["wow", "wow_classic"]
    } else {
        &["wow"]
    });
    log.env(&home);
    for (repo, name) in repos {
        publish_run(&home, repo, name, "v0");
        for target in targets {
            json(
                log,
                &home,
                &["install", &format!("gh:o/{repo}"), "-t", target],
            )
            .ok();
        }
        newest(&home, repo, latest);
    }
    home
}

/// Foo at v0 on retail, v1 out, and an `outdated` that saw it: the memo offers v1.
fn behind(log: &TestLog) -> Home {
    let home = home_at(log, false, &[("foo", "Foo")], &["retail"], "v1");
    log.step("outdated sees v1");
    json(log, &home, &["outdated", "-t", "retail"]).ok();
    home
}

/// `addon`'s (a record key) installed version on `target`.
fn at(home: &Home, target: &str, addon: &str) -> String {
    record(home, target).addons[addon].version.clone()
}

/// `addon`'s (a record key) marks on `target`; none when it is not installed there.
fn marks(home: &Home, target: &str, addon: &str) -> Vec<SkippedRelease> {
    record(home, target)
        .addons
        .get(addon)
        .map(|a| a.skipped_releases.clone())
        .unwrap_or_default()
}

/// What each mark holds: its exact release, or `None` for a label mark.
fn held(marks: &[SkippedRelease]) -> Vec<Option<ReleaseKey>> {
    marks.iter().map(|m| m.release.clone()).collect()
}

/// `gh:o/<repo>@<tag>`'s key.
fn gh(repo: &str, tag: &str) -> Option<ReleaseKey> {
    ReleaseKey::parse(&format!("gh:o/{repo}@{tag}"), None)
}

/// `wowace:bug-grabber@<tag>`'s key at `revision`.
fn wowace(tag: &str, revision: u32) -> Option<ReleaseKey> {
    ReleaseKey::parse(&format!("wowace:bug-grabber@{tag}"), Some(revision))
}

/// The first client's first item of an `update`, `outdated` or `skip` result.
fn item(ran: &Ran) -> &Value {
    &ran.data()["targets"][0]["items"][0]
}

/// The number of lines `history.jsonl` holds.
fn lines(home: &Home) -> usize {
    fs::read_to_string(home.state().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .count()
}

/// 1. Gap 2: an update undone stays away. `update --dry-run` has nothing to do and
///    `status` counts it skipped, never available.
#[test]
fn an_undone_update_stays_away() {
    let log = tlog!();
    let home = behind(&log);
    log.step("update to v1, then undo it");
    json(&log, &home, &["update", "-t", "retail"]).ok();
    let ran = quiet(&log, &home, &["undo"]);
    ran.ok();
    log.assert_eq("back at v0", &at(&home, "retail", "foo"), &"v0".to_owned());

    log.step("update --dry-run: nothing to do");
    let ran = quiet(&log, &home, &["update", "--dry-run"]);
    ran.ok();
    log.assert_eq(
        "passed over",
        &(
            &item(&ran)["status"],
            &item(&ran)["skip_reason"],
            &item(&ran)["to_version"],
        ),
        &(&json!("skipped"), &json!("skipped_release"), &json!("v1")),
    );
    log.assert_eq("no plan to apply", &ran.data()["plan_id"], &Value::Null);

    log.step("status: none available, one skipped");
    let ran = quiet(&log, &home, &["status"]);
    ran.ok();
    let updates = &ran.data()["targets"][0]["updates"];
    log.assert_eq(
        "available, skipped",
        &(&updates["available"], &updates["skipped"]),
        &(&json!(0), &json!(1)),
    );
    log.assert_eq(
        "the machine has no update",
        &ran.data()["summary"]["updates"]["available"],
        &json!(0),
    );

    log.step("update says why, in words");
    let ran = quiet_human(&log, &home, &["update"]);
    ran.ok();
    says(&log, &ran, "Foo left at v0 · v1 is skipped");
}

/// 2. The widget's sequence after an undo: it reads `list -v`, previews Update all, and
///    finds nothing to apply; the plan it applied before the undo cannot run again.
#[test]
fn the_widgets_preview_after_an_undo_stores_nothing_to_apply() {
    let log = tlog!();
    let home = behind(&log);
    log.step("the widget previews Update all and applies it");
    let preview = json(&log, &home, &["update", "--dry-run"]);
    preview.ok();
    let plan = preview.data()["plan_id"].as_str().unwrap().to_owned();
    json(&log, &home, &["apply", &plan]).ok();
    log.assert_eq("at v1", &at(&home, "retail", "foo"), &"v1".to_owned());

    log.step("the player undoes it from History");
    quiet(&log, &home, &["undo"]).ok();

    log.step("the widget reads list -v");
    let ran = quiet(&log, &home, &["list", "-t", "retail", "-v"]);
    ran.ok();
    let foo = &ran.data()["targets"][0]["addons"][0];
    log.assert_eq(
        "latest.status",
        &foo["latest"]["status"],
        &json!("skipped_release"),
    );
    log.assert_eq(
        "its mark",
        &foo["skipped_releases"].as_array().map(Vec::len),
        &Some(1),
    );

    log.step("it previews Update all again: no change, no plan");
    let ran = quiet(&log, &home, &["update", "--dry-run"]);
    ran.ok();
    log.assert_eq("no plan", &ran.data()["plan_id"], &Value::Null);
    log.assert_eq(
        "nothing to apply",
        &ran.commands().iter().any(|c| c.action == "apply"),
        &false,
    );

    log.step("the plan from before the undo cannot run again");
    let ran = quiet(&log, &home, &["apply", &plan]);
    ran.fails(1, "plan_stale");
    log.assert_eq(
        "already applied",
        &ran.envelope()["error"]["details"]["reason"],
        &json!("already_applied"),
    );
    log.assert_eq("still v0", &at(&home, "retail", "foo"), &"v0".to_owned());
}

/// 3. Resume: the next release installs as an ordinary update; `outdated` is clean, and
///    the mark on the release that was left stays on the record.
#[test]
fn the_next_release_installs_and_the_old_mark_stays() {
    let log = tlog!();
    let home = behind(&log);
    json(&log, &home, &["update", "-t", "retail"]).ok();
    quiet(&log, &home, &["undo"]).ok();

    log.step("v2 appears; update installs it");
    newest(&home, "foo", "v2");
    let ran = json(&log, &home, &["update", "-t", "retail"]);
    ran.ok();
    log.assert_eq(
        "updated",
        &(
            &item(&ran)["status"],
            &item(&ran)["from_version"],
            &item(&ran)["to_version"],
        ),
        &(&json!("updated"), &json!("v0"), &json!("v2")),
    );

    log.step("outdated is clean");
    let ran = quiet(&log, &home, &["outdated", "-t", "retail"]);
    ran.ok();
    let summary = &ran.data()["summary"];
    log.assert_eq(
        "nothing waits",
        &(&summary["update_available"], &summary["skipped_release"]),
        &(&json!(0), &json!(0)),
    );
    log.assert_eq(
        "the mark on v1 stays",
        &held(&marks(&home, "retail", "foo")),
        &vec![gh("foo", "v1")],
    );
}

/// 4. Redo: undoing the undo puts the release back, clears its mark and says so.
#[test]
fn a_redo_puts_the_release_back_and_names_the_mark_it_cleared() {
    let log = tlog!();
    let home = behind(&log);
    json(&log, &home, &["update", "-t", "retail"]).ok();
    log.step("undo, in words");
    let ran = quiet_human(&log, &home, &["undo"]);
    ran.ok();
    says(
        &log,
        &ran,
        "1 release is skipped: update passes over it and resumes at the next release\n  Foo v1\n",
    );

    log.step("undo again: the redo");
    let ran = quiet_human(&log, &home, &["undo"]);
    ran.ok();
    says(&log, &ran, "1 skip mark cleared: Foo v1");
    log.assert_eq("at v1", &at(&home, "retail", "foo"), &"v1".to_owned());
    log.assert_eq("no mark", &marks(&home, "retail", "foo").len(), &0);

    log.step("history -v names the mark the redo cleared");
    let ran = quiet(&log, &home, &["history", "-v", "--limit", "1"]);
    ran.ok();
    log.assert_eq(
        "skips_cleared",
        &ran.data()["entries"][0]["items"][0]["skips_cleared"][0]["release"],
        &serde_json::to_value(gh("foo", "v1")).unwrap(),
    );
}

/// 5. Proactive: a release skipped before it is installed; `update` passes over it, and
///    `unskip` offers it again.
#[test]
fn skip_ahead_of_an_update_and_offer_it_again() {
    let log = tlog!();
    let home = behind(&log);
    log.step("skip the waiting release");
    let ran = quiet_human(&log, &home, &["skip", "foo", "-t", "retail"]);
    ran.ok();
    says(
        &log,
        &ran,
        "retail: Foo v1 is skipped (installed: v0): update resumes at the next release",
    );
    log.assert_eq(
        "marked",
        &held(&marks(&home, "retail", "foo")),
        &vec![gh("foo", "v1")],
    );

    log.step("update passes over it");
    let ran = quiet(&log, &home, &["update"]);
    ran.ok();
    log.assert_eq(
        "no transaction",
        &ran.data()["targets"][0]["tx"],
        &Value::Null,
    );
    log.assert_eq("why", &item(&ran)["skip_reason"], &json!("skipped_release"));

    log.step("unskip offers it again");
    let ran = quiet_human(&log, &home, &["unskip", "foo", "-t", "retail"]);
    ran.ok();
    says(&log, &ran, "retail: Foo v1 is offered again");
    log.assert_eq("no mark", &marks(&home, "retail", "foo").len(), &0);

    log.step("update installs it");
    let ran = json(&log, &home, &["update", "-t", "retail"]);
    ran.ok();
    log.assert_eq("v1", &at(&home, "retail", "foo"), &"v1".to_owned());
}

/// 6. Explicit wins, visibly: installing the skipped version by name installs it, clears
///    its mark and warns `skip_lifted`.
#[test]
fn an_explicit_version_installs_a_skipped_release_and_says_so() {
    let log = tlog!();
    let home = behind(&log);
    quiet(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    log.step("install v1 by name");
    let ran = json(
        &log,
        &home,
        &["install", "gh:o/foo", "--version", "v1", "-t", "retail"],
    );
    ran.ok();
    log.assert_eq(
        "skip_lifted",
        &ran.warnings().contains(&"skip_lifted".to_owned()),
        &true,
    );
    log.assert_eq("v1", &at(&home, "retail", "foo"), &"v1".to_owned());
    log.assert_eq("its mark gone", &marks(&home, "retail", "foo").len(), &0);
}

/// 7. Pins and skips together: pinned and skipped reads `pinned`; `unpin` leaves the
///    mark; `unskip`, then `update`, installs the release.
#[test]
fn pinned_and_skipped_reads_pinned_and_unpin_keeps_the_mark() {
    let log = tlog!();
    let home = behind(&log);
    log.step("pin, then skip the newer release");
    quiet(&log, &home, &["pin", "foo", "-t", "retail"]).ok();
    quiet(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    log.assert_eq(
        "marked",
        &held(&marks(&home, "retail", "foo")),
        &vec![gh("foo", "v1")],
    );
    let status = |label: &str, want: &str| {
        let ran = quiet(&log, &home, &["outdated", "-t", "retail", "-v"]);
        ran.ok();
        log.assert_eq(label, &item(&ran)["status"], &json!(want));
    };
    status("a pin is decided first", "pinned");

    log.step("unpin keeps the mark");
    quiet(&log, &home, &["unpin", "foo", "-t", "retail"]).ok();
    log.assert_eq("still marked", &marks(&home, "retail", "foo").len(), &1);
    status("now the mark shows", "skipped_release");

    log.step("unskip, then update installs v1");
    quiet(&log, &home, &["unskip", "foo", "-t", "retail"]).ok();
    json(&log, &home, &["update", "-t", "retail"]).ok();
    log.assert_eq("v1", &at(&home, "retail", "foo"), &"v1".to_owned());
}

/// 8. Other commands: `sync` without a version leaves an installed addon where it is,
///    `copy` gives the other client no mark, `export` writes none, `adopt` is unaffected.
#[test]
fn other_commands_leave_marks_where_they_are() {
    let log = tlog!();
    let home = home_at(&log, true, &[("foo", "Foo")], &["retail"], "v1");
    json(&log, &home, &["outdated", "-t", "retail"]).ok();
    quiet(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    let mark = held(&marks(&home, "retail", "foo"));

    log.step("sync, naming Foo without a version");
    let wanted = home.env.root().join("wanted.toml");
    fs::write(
        &wanted,
        "schema_version = \"1.0\"\n\n[target.\"retail\"]\naddons = [\"gh:o/foo\"]\n",
    )
    .unwrap();
    json(
        &log,
        &home,
        &["sync", wanted.to_str().unwrap(), "-t", "retail"],
    )
    .ok();
    log.assert_eq("left at v0", &at(&home, "retail", "foo"), &"v0".to_owned());
    log.assert_eq("the mark", &held(&marks(&home, "retail", "foo")), &mark);

    log.step("copy to Mists Classic: no mark goes with it");
    json(
        &log,
        &home,
        &["copy", "--from", "retail", "--to", "classic", "Foo"],
    )
    .ok();
    log.assert_eq(
        "installed there",
        &at(&home, "classic", "foo"),
        &"v1".to_owned(),
    );
    log.assert_eq("no mark there", &marks(&home, "classic", "foo").len(), &0);

    log.step("export writes no mark");
    let ran = quiet(&log, &home, &["export"]);
    ran.ok();
    let content = ran.data()["content"].as_str().unwrap();
    log.assert_eq("Foo exported", &content.contains("gh:o/foo"), &true);
    log.assert_eq("no mark", &content.contains("skip"), &false);

    log.step("adopt is unaffected");
    let baz = home.addons("_retail_").join("Baz");
    fs::create_dir_all(&baz).unwrap();
    fs::write(
        baz.join("Baz.toc"),
        common::addons::toc("120105", &["## Version: 3"]),
    )
    .unwrap();
    json(
        &log,
        &home,
        &[
            "adopt",
            "Baz",
            "--ref",
            "gh:o/baz",
            "-t",
            "retail",
            "--no-verify",
        ],
    )
    .ok();
    log.assert_eq("Baz adopted", &at(&home, "retail", "baz"), &"3".to_owned());
    log.assert_eq("the mark", &held(&marks(&home, "retail", "foo")), &mark);
}

/// 9. Two clients: a mark on one client does not hold the release on the other.
#[test]
fn a_mark_on_one_client_leaves_the_other() {
    let log = tlog!();
    let home = home_at(&log, true, &[("foo", "Foo")], &["retail", "classic"], "v1");
    json(&log, &home, &["outdated"]).ok();
    quiet(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    log.step("update every client");
    json(&log, &home, &["update"]).ok();
    log.assert_eq(
        "retail left, classic updated",
        &(at(&home, "retail", "foo"), at(&home, "classic", "foo")),
        &("v0".to_owned(), "v1".to_owned()),
    );
    log.assert_eq(
        "marks",
        &(
            marks(&home, "retail", "foo").len(),
            marks(&home, "classic", "foo").len(),
        ),
        &(1, 0),
    );
}

/// 10. Several addons: undoing a three-addon update marks all three; the `unskip --tx`
///     its result offers clears those three and not an older mark; `update` then
///     installs all three.
#[test]
fn an_undo_of_three_marks_three_and_its_unskip_clears_only_those() {
    let log = tlog!();
    let repos = [("foo", "Foo"), ("bar", "Bar"), ("baz", "Baz")];
    let home = home_at(&log, false, &repos, &["retail"], "v1");
    json(&log, &home, &["outdated", "-t", "retail"]).ok();
    log.step("an older label mark on Foo, then the update of all three");
    quiet(
        &log,
        &home,
        &["skip", "foo", "--version", "9.9", "-t", "retail"],
    )
    .ok();
    json(&log, &home, &["update", "-t", "retail"]).ok();

    log.step("undo marks the three releases it leaves");
    let ran = quiet(&log, &home, &["undo"]);
    ran.ok();
    let marked: Vec<&Value> = ran.data()["transactions"][0]["skips_marked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| &n["addon"])
        .collect();
    log.assert_eq(
        "three marked",
        &marked,
        &vec![&json!("Bar"), &json!("Baz"), &json!("Foo")],
    );
    let unskip: Vec<String> = ran
        .commands()
        .into_iter()
        .find(|c| c.action == "unskip")
        .unwrap()
        .argv;

    log.step("its unskip --tx");
    let argv: Vec<&str> = unskip.iter().skip(1).map(String::as_str).collect();
    quiet(&log, &home, &argv).ok();
    for repo in ["bar", "baz"] {
        log.assert_eq(
            &format!("{repo}: no mark"),
            &marks(&home, "retail", repo).len(),
            &0,
        );
    }
    let foo = marks(&home, "retail", "foo");
    log.assert_eq(
        "Foo keeps the older label mark",
        &foo.iter()
            .map(|m| (m.release.clone(), m.version.clone()))
            .collect::<Vec<_>>(),
        &vec![(None, "9.9".to_owned())],
    );

    log.step("update installs all three");
    json(&log, &home, &["update", "-t", "retail"]).ok();
    for repo in ["foo", "bar", "baz"] {
        log.assert_eq(repo, &at(&home, "retail", repo), &"v1".to_owned());
    }
}

/// 11. Durable: twelve releases skipped in turn keep twelve marks, and the first still
///     holds when the source's newest moves back to it.
#[test]
fn twelve_skipped_releases_are_kept_and_the_first_still_holds() {
    let log = tlog!();
    let home = home_at(&log, false, &[("foo", "Foo")], &["retail"], "v0");
    for n in 1..=RUN {
        let tag = tag(n);
        log.step(&format!("{tag} appears, and is skipped"));
        newest(&home, "foo", &tag);
        let ran = json(&log, &home, &["outdated", "-t", "retail"]);
        ran.ok();
        log.assert_eq(
            &format!("{tag} waits"),
            &(&item(&ran)["status"], &item(&ran)["latest"]["version"]),
            &(&json!("update_available"), &json!(tag)),
        );
        quiet(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    }
    let mut kept = held(&marks(&home, "retail", "foo"));
    kept.sort();
    let mut all: Vec<Option<ReleaseKey>> = (1..=RUN).map(|n| gh("foo", &tag(n))).collect();
    all.sort();
    log.assert_eq("twelve marks, none pushed out", &kept, &all);

    log.step("the newest moves back to v1");
    newest(&home, "foo", "v1");
    let ran = json(&log, &home, &["outdated", "-t", "retail"]);
    ran.ok();
    log.assert_eq(
        "still skipped",
        &(
            &item(&ran)["status"],
            &item(&ran)["skipped_release"]["release"],
        ),
        &(
            &json!("skipped_release"),
            &serde_json::to_value(gh("foo", "v1")).unwrap(),
        ),
    );
    let ran = quiet(&log, &home, &["update"]);
    ran.ok();
    log.assert_eq(
        "passed over",
        &item(&ran)["skip_reason"],
        &json!("skipped_release"),
    );
    log.assert_eq("at v0", &at(&home, "retail", "foo"), &"v0".to_owned());
}

/// 12. Duplicates: the same release skipped twice, plainly and by `--release`, is one
///     mark and one transaction.
#[test]
fn the_same_release_skipped_twice_is_one_mark() {
    let log = tlog!();
    let home = behind(&log);
    quiet(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    let written = lines(&home);
    log.step("skip it again, by its id");
    quiet(
        &log,
        &home,
        &["skip", "foo", "--release", "gh:o/foo@v1", "-t", "retail"],
    )
    .ok();
    log.assert_eq(
        "one mark",
        &held(&marks(&home, "retail", "foo")),
        &vec![gh("foo", "v1")],
    );
    log.assert_eq("no transaction", &lines(&home), &written);
}

/// BugGrabber's `v12.1.1` at revision `rev`, installed on retail from WowAce; its
/// record's name.
fn wowace_home(log: &TestLog, rev: u32) -> (Home, String) {
    log.step(&format!("BugGrabber v12.1.1 at revision {rev}, installed"));
    let home = Home::new(&["wow"]);
    log.env(&home);
    serve_recorded(&home);
    tag_at(&home.http, rev);
    json(
        log,
        &home,
        &["install", "wowace:bug-grabber", "-t", "retail"],
    )
    .ok();
    let name = record(&home, "retail")
        .addons
        .values()
        .next()
        .unwrap()
        .name
        .clone();
    (home, name)
}

/// `v12.1.1` re-tagged upstream at `rev`, and the memo aged: the next check sees it.
fn retag(log: &TestLog, home: &mut Home, rev: u32) {
    log.step(&format!("v12.1.1 re-tagged at revision {rev} upstream"));
    let dir = home.scenario(&format!("revision-{rev}"));
    serve_recorded(home);
    tag_at(&dir, rev);
    age_release_memo(home, jiff::SignedDuration::from_hours(24));
}

/// The release BugGrabber's record holds on retail.
fn installed(home: &Home) -> Option<ReleaseKey> {
    ReleaseKey::of_record(record(home, "retail").addons.values().next().unwrap())
}

/// 13. Revision-specific: a mark on one WowAce revision is cleared by `unskip --release
///     --revision` naming that revision, never another, and a mark on another tag stays.
#[test]
fn a_mark_on_one_revision_is_cleared_by_that_revision_alone() {
    let log = tlog!();
    let (mut home, name) = wowace_home(&log, 1390);
    let key = name.to_ascii_lowercase();
    retag(&log, &mut home, 1402);
    json(&log, &home, &["outdated", "-t", "retail"]).ok();

    log.step("skip the re-tag, and another tag by its id");
    quiet(&log, &home, &["skip", &name, "-t", "retail"]).ok();
    quiet(
        &log,
        &home,
        &[
            "skip",
            &name,
            "--release",
            "wowace:bug-grabber@v12.1.0",
            "-t",
            "retail",
        ],
    )
    .ok();
    let tags = |home: &Home| -> Vec<(String, Option<u32>)> {
        let mut tags: Vec<(String, Option<u32>)> = held(&marks(home, "retail", &key))
            .into_iter()
            .flatten()
            .map(|k| (k.tag, k.revision))
            .collect();
        tags.sort();
        tags
    };
    let both = tags(&home);
    log.assert_eq(
        "the re-tag's mark holds revision 1402",
        &both.contains(&("v12.1.1".to_owned(), Some(1402))),
        &true,
    );
    log.assert_eq("two marks", &both.len(), &2);

    log.step("unskip naming revision 1390 clears nothing");
    let unskip = |rev: &str| {
        quiet(
            &log,
            &home,
            &[
                "unskip",
                &name,
                "--release",
                "wowace:bug-grabber@v12.1.1",
                "--revision",
                rev,
                "-t",
                "retail",
            ],
        )
        .ok();
    };
    unskip("1390");
    log.assert_eq("both stay", &tags(&home), &both);

    log.step("unskip naming revision 1402 clears that mark alone");
    unskip("1402");
    let left = tags(&home);
    log.assert_eq(
        "the other tag's mark stays",
        &left.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(),
        &vec!["v12.1.0"],
    );
}

/// Rewrites only the release memo, so that `gh:o/foo`'s newest release reads `tag`.
fn memo_offers(home: &Home, tag: &str) {
    let (mut memo, _) = omaddons::state::memo::load(&paths(&home.env));
    for entry in memo.entries.values_mut() {
        if let Some(set) = &mut entry.set {
            for release in &mut set.releases {
                release.release_id = format!("gh:o/foo@{tag}");
                release.version = tag.to_owned();
            }
        }
        entry.probe = Some(format!("gh:o/foo@{tag}"));
    }
    home.seed_memo(&memo);
}

/// 14. A stale plan: a `skip` previewed while v1 waits, then only the memo moves to v2:
///     `apply` refuses with `plan_stale` and marks nothing.
#[test]
fn a_skip_preview_whose_waiting_release_moved_is_stale() {
    let log = tlog!();
    let home = behind(&log);
    log.step("preview the skip of v1");
    let preview = quiet(&log, &home, &["skip", "foo", "-t", "retail", "--dry-run"]);
    preview.ok();
    let plan = preview.data()["plan_id"].as_str().unwrap().to_owned();

    log.step("only the memo moves: v2 waits now");
    memo_offers(&home, "v2");
    let ran = quiet(&log, &home, &["apply", &plan]);
    ran.fails(1, "plan_stale");
    log.assert_eq("no mark", &marks(&home, "retail", "foo").len(), &0);
}

/// 15. The re-tag lifecycle on one WowAce tag, the release key asserted at every step:
///     the re-tag is offered as an update; `update` installs revision 1402; `undo` goes
///     back to 1390 and marks 1402, not the tag; `outdated` reads `skipped_release`; the
///     redo puts 1402 back and names the mark it cleared; `skip --release --revision`
///     marks it again; `unskip --release --revision` clears exactly that.
///
///     Later slices add steps to this same sequence, in place: RG-55 the re-tag's first
///     sight and `update --auto` holding it, before the update; RG-34 `skip --back`
///     between the two revisions, after the redo.
#[test]
fn the_re_tag_lifecycle_on_one_tag() {
    let log = tlog!();
    let (r1390, r1402) = (wowace("v12.1.1", 1390), wowace("v12.1.1", 1402));
    let as_wire = |key: &Option<ReleaseKey>| serde_json::to_value(key).unwrap();
    let (mut home, name) = wowace_home(&log, 1390);
    let key = name.to_ascii_lowercase();
    log.assert_eq("installed: 1390", &installed(&home), &r1390);

    retag(&log, &mut home, 1402);
    log.step("the re-tag is offered as an update");
    let ran = json(&log, &home, &["outdated", "-t", "retail"]);
    ran.ok();
    let latest = &item(&ran)["latest"];
    log.assert_eq(
        "offered",
        &(
            &item(&ran)["status"],
            &latest["revision"],
            &latest["retagged"],
        ),
        &(&json!("update_available"), &json!(1402), &json!(true)),
    );

    log.step("update installs revision 1402");
    json(&log, &home, &["update", "-t", "retail"]).ok();
    log.assert_eq("installed: 1402", &installed(&home), &r1402);

    log.step("undo goes back to 1390 and marks 1402, not the tag");
    let ran = quiet(&log, &home, &["undo"]);
    ran.ok();
    log.assert_eq(
        "the mark names revision 1402",
        &ran.data()["transactions"][0]["skips_marked"][0]["release"],
        &as_wire(&r1402),
    );
    log.assert_eq("installed: 1390", &installed(&home), &r1390);
    log.assert_eq(
        "on the record",
        &held(&marks(&home, "retail", &key)),
        &vec![r1402.clone()],
    );

    log.step("outdated reads skipped_release");
    let ran = quiet(&log, &home, &["outdated", "-t", "retail"]);
    ran.ok();
    log.assert_eq(
        "skipped",
        &(
            &item(&ran)["status"],
            &item(&ran)["skipped_release"]["release"],
        ),
        &(&json!("skipped_release"), &as_wire(&r1402)),
    );

    log.step("the redo puts 1402 back and names the mark it cleared");
    let ran = quiet(&log, &home, &["undo"]);
    ran.ok();
    log.assert_eq(
        "skips_cleared",
        &ran.data()["transactions"][0]["skips_cleared"][0]["release"],
        &as_wire(&r1402),
    );
    log.assert_eq("installed: 1402", &installed(&home), &r1402);
    log.assert_eq("no mark", &marks(&home, "retail", &key).len(), &0);

    let release = [
        "--release",
        "wowace:bug-grabber@v12.1.1",
        "--revision",
        "1402",
        "-t",
        "retail",
    ];
    log.step("skip marks it again, by its revision");
    let argv: Vec<&str> = ["skip", name.as_str()].into_iter().chain(release).collect();
    quiet(&log, &home, &argv).ok();
    log.assert_eq(
        "marked",
        &held(&marks(&home, "retail", &key)),
        &vec![r1402.clone()],
    );

    log.step("unskip --release --revision clears exactly that");
    let argv: Vec<&str> = ["unskip", name.as_str()]
        .into_iter()
        .chain(release)
        .collect();
    quiet(&log, &home, &argv).ok();
    log.assert_eq("no mark", &marks(&home, "retail", &key).len(), &0);
    log.assert_eq("installed: 1402", &installed(&home), &r1402);
}
