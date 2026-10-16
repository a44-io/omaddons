//! The release memo end to end: request counts in `net.log` across commands, the
//! 15-minute window and `--refresh`, seeded entries of chosen ages, remembered and
//! retried failures, a corrupt memo, two writers at once, and `install`'s probe and real
//! runs sharing one fetch.

mod common;

use std::fs;

use common::home::{Home, wait};
use common::http::{Asset, fixture, gh_release};
use common::memo;
use serde_json::Value;
use support::{foo, requests};

#[cfg(test)]
mod support {
    use super::*;

    /// Release `tag` of gh:o/foo, loading on retail.
    pub fn foo(home: &Home, tag: &str) {
        let assets: [Asset<'_>; 1] = [(
            "Foo.zip",
            &[("mainline", 120_105)],
            vec![("Foo/Foo.toc", "## Interface: 120105\n")],
        )];
        gh_release(&home.http, "https://github.com/o/foo", tag, &assets);
    }

    /// The requests so far.
    pub fn requests(home: &Home) -> usize {
        fs::read_to_string(home.state().join("net.log"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// The paths requested so far that end with `suffix`.
    pub fn requested(home: &Home, suffix: &str) -> usize {
        fs::read_to_string(home.state().join("net.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| e["path"].as_str().is_some_and(|p| p.ends_with(suffix)))
            .count()
    }
}

#[test]
fn info_inside_the_window_makes_no_request_and_refresh_does() {
    let home = Home::new(&["wow"]);
    foo(&home, "v1");
    let r = home.run(&["info", "gh:o/foo"]);
    assert_eq!(
        r.expect(0)["data"]["latest"]["version"],
        "v1",
        "{}",
        r.context()
    );
    let once = requests(&home);
    assert!(once > 0);

    let r = home.run(&["info", "gh:o/foo"]);
    assert_eq!(
        r.expect(0)["data"]["latest"]["version"],
        "v1",
        "{}",
        r.context()
    );
    assert_eq!(requests(&home), once, "answered from the memo");

    let r = home.run(&["info", "gh:o/foo", "--refresh"]);
    r.expect(0);
    assert_eq!(requests(&home), 2 * once, "--refresh fetches again");
    let memo: Value = serde_json::from_slice(&fs::read(home.memo()).unwrap()).unwrap();
    assert_eq!(memo["schema_version"], "1.0");
    assert!(memo["entries"]["gh:o/foo|stable"].is_object(), "{memo}");
}

#[test]
fn a_remembered_answer_is_fresh_for_fifteen_minutes() {
    let home = Home::new(&["wow"]);
    foo(&home, "v1");
    let seeded = |minutes| memo::of(vec![memo::entry("gh:o/foo", "v0", memo::ago(minutes))]);

    home.seed_memo(&seeded(5));
    let r = home.run(&["info", "gh:o/foo"]);
    assert_eq!(
        r.expect(0)["data"]["latest"]["version"],
        "v0",
        "{}",
        r.context()
    );
    assert_eq!(requests(&home), 0, "five minutes old: no request");

    home.seed_memo(&seeded(20));
    let r = home.run(&["info", "gh:o/foo"]);
    assert_eq!(
        r.expect(0)["data"]["latest"]["version"],
        "v1",
        "{}",
        r.context()
    );
    let fetched = requests(&home);
    assert!(fetched > 0, "twenty minutes old: fetched");

    home.seed_memo(&seeded(-60));
    let r = home.run(&["info", "gh:o/foo"]);
    assert_eq!(
        r.expect(0)["data"]["latest"]["version"],
        "v1",
        "{}",
        r.context()
    );
    assert!(requests(&home) > fetched, "fetched in the future: stale");
}

#[test]
fn a_missing_repository_is_remembered_and_a_server_error_is_not() {
    let home = Home::new(&["wow"]);
    fixture(
        &home.http,
        "missing",
        "https://github.com/o/missing/releases/latest",
        404,
        &[],
        None,
    );
    fixture(
        &home.http,
        "down",
        "https://github.com/o/down/releases/latest",
        503,
        &[("retry-after", "0")],
        None,
    );
    let r = home.run(&["info", "gh:o/missing"]);
    assert_eq!(r.expect(3)["error"]["code"], "not_found", "{}", r.context());
    let after_missing = requests(&home);
    let r = home.run(&["info", "gh:o/missing"]);
    let envelope = r.expect(3);
    assert_eq!(envelope["error"]["code"], "not_found", "{}", r.context());
    assert_eq!(
        envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "info", "gh:o/missing", "--refresh"])
    );
    assert_eq!(requests(&home), after_missing, "remembered");

    let r = home.run(&["info", "gh:o/down"]);
    assert_eq!(
        r.expect(1)["error"]["code"],
        "http_status",
        "{}",
        r.context()
    );
    let after_down = requests(&home);
    home.run(&["info", "gh:o/down"]).expect(1);
    assert!(
        requests(&home) > after_down,
        "a 5xx is retried, never remembered"
    );
    let memo: Value = serde_json::from_slice(&fs::read(home.memo()).unwrap()).unwrap();
    assert!(memo["entries"]["gh:o/down|stable"].is_null(), "{memo}");
}

#[test]
fn a_corrupt_memo_is_ignored_and_rewritten() {
    let home = Home::new(&["wow"]);
    foo(&home, "v1");
    fs::create_dir_all(home.memo().parent().unwrap()).unwrap();
    fs::write(home.memo(), "{ not json").unwrap();
    let args = ["info", "gh:o/foo", "-v", "--json"].map(str::to_owned);
    let r = home.exec(&args);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["latest"]["version"],
        "v1",
        "{}",
        r.context()
    );
    assert!(
        r.stderr.contains("release memo ignored"),
        "a -v note, no warning\n{}",
        r.context()
    );
    assert_eq!(
        envelope["warnings"],
        serde_json::json!([]),
        "{}",
        r.context()
    );
    let memo: Value = serde_json::from_slice(&fs::read(home.memo()).unwrap()).unwrap();
    assert!(
        memo["entries"]["gh:o/foo|stable"].is_object(),
        "rewritten valid"
    );
}

#[test]
fn two_info_processes_at_once_leave_a_valid_memo() {
    let home = Home::new(&["wow"]);
    foo(&home, "v1");
    let assets: [Asset<'_>; 1] = [(
        "Bar.zip",
        &[("mainline", 120_105)],
        vec![("Bar/Bar.toc", "## Interface: 120105\n")],
    )];
    gh_release(&home.http, "https://github.com/o/bar", "v2", &assets);
    let (a, b) = (
        ["info", "gh:o/foo", "--json"],
        ["info", "gh:o/bar", "--json"],
    );
    let first = home.spawn(&a, &[]);
    let second = home.spawn(&b, &[]);
    wait(first, &a).expect(0);
    wait(second, &b).expect(0);
    let memo: Value = serde_json::from_slice(&fs::read(home.memo()).unwrap()).unwrap();
    let entries = memo["entries"].as_object().unwrap();
    assert!(
        entries.contains_key("gh:o/foo|stable") || entries.contains_key("gh:o/bar|stable"),
        "at least one writer's entry: {memo}"
    );
}

#[test]
fn install_without_t_fetches_each_ref_once_across_its_probe_and_its_run() {
    let home = Home::new(&["wow", "wow_classic"]);
    foo(&home, "v1");
    let r = home.run(&["install", "gh:o/foo"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["target"]["id"],
        "retail",
        "{}",
        r.context()
    );
    assert_eq!(
        support::requested(&home, "/releases/latest"),
        1,
        "{}",
        r.context()
    );
    assert_eq!(support::requested(&home, "/release.json"), 1);
}

#[test]
fn an_offline_install_plans_from_the_memo_but_downloads_nothing() {
    let home = Home::new(&["wow"]);
    foo(&home, "v1");
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let before = requests(&home);

    // The remembered release is the installed one: unchanged, said to be unchecked.
    let r = home.run(&["install", "gh:o/foo", "-t", "retail", "--offline"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["items"][0]["status"],
        "unchanged",
        "{}",
        r.context()
    );
    let codes: Vec<&str> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["code"].as_str())
        .collect();
    assert_eq!(codes, ["stale_check"], "{}", r.context());

    // A remembered release whose archive is not cached: it fails offline.
    home.seed_memo(&memo::of(vec![memo::entry_with_asset(
        "gh:o/foo",
        "v9",
        memo::ago(60 * 24),
        "https://github.com/o/foo/releases/download/v9/Foo.zip",
    )]));
    let r = home.run(&["install", "gh:o/foo", "-t", "retail", "--offline"]);
    let envelope = r.expect(1);
    assert_eq!(
        (&envelope["error"]["code"], &envelope["error"]["fix"]),
        (
            &Value::from("offline"),
            &Value::from("run without --offline and with OMADDONS_OFFLINE unset")
        ),
        "the remembered v9's archive is not downloaded\n{}",
        r.context()
    );
    assert_eq!(requests(&home), before, "no request either time");
}
