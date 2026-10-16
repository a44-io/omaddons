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
use support::{facts, fetched_at, foo, requests, verdicts};

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

    /// The memo's `fetched_at` of gh:o/`name` on stable.
    pub fn fetched_at(home: &Home, name: &str) -> Value {
        let memo: Value = serde_json::from_slice(&fs::read(home.memo()).unwrap()).unwrap();
        memo["entries"][format!("gh:o/{name}|stable")]["fetched_at"].clone()
    }

    /// The memo's candidate facts.
    pub fn facts(home: &Home) -> Vec<Value> {
        let memo: Value = serde_json::from_slice(&fs::read(home.memo()).unwrap()).unwrap();
        memo["candidates"].as_array().cloned().unwrap_or_default()
    }

    /// The verdicts remembered for interface `interface`, as `(loads, out_of_date,
    /// primary_interface)`.
    pub fn verdicts(home: &Home, interface: u32) -> Vec<(bool, bool, Value)> {
        facts(home)
            .iter()
            .flat_map(|f| f["verdicts"].as_array().cloned().unwrap_or_default())
            .filter(|v| v["interface"] == interface)
            .map(|v| {
                (
                    v["loads"].as_bool().unwrap(),
                    v["out_of_date"].as_bool().unwrap(),
                    v["primary_interface"].clone(),
                )
            })
            .collect()
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

#[test]
fn a_cached_archive_is_never_downloaded_again_while_its_blob_is_kept() {
    let home = Home::new(&["wow"]);
    foo(&home, "v1");
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    assert_eq!(support::requested(&home, "/Foo.zip"), 1);
    let fact = &facts(&home)[0];
    assert_eq!(
        (&fact["release_id"], &fact["asset"]),
        (&Value::from("gh:o/foo@v1"), &Value::from("Foo.zip"))
    );
    let sha256 = fact["sha256"].as_str().unwrap().to_owned();
    assert!(verdicts(&home, 120_105)[0].0, "loads on retail");

    home.run(&["remove", "Foo", "-t", "retail"]).expect(0);
    let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
    assert_eq!(r.expect(0)["data"]["items"][0]["status"], "installed");
    assert_eq!(
        support::requested(&home, "/Foo.zip"),
        1,
        "the remembered blob, no second download\n{}",
        r.context()
    );

    // Evicted: downloaded again, once, and remembered again.
    let blob = home.env.cache().join("omaddons/blobs").join(&sha256);
    assert!(blob.is_file(), "{}", blob.display());
    fs::remove_file(&blob).unwrap();
    home.run(&["remove", "Foo", "-t", "retail"]).expect(0);
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    assert_eq!(support::requested(&home, "/Foo.zip"), 2);
    assert!(blob.is_file(), "back in the cache");
    assert_eq!(facts(&home).len(), 1, "the same fact, refreshed");
}

#[test]
fn verdicts_remember_what_loads_and_how_current_it_is() {
    let home = Home::new(&["wow"]);
    // Only a Classic Era build: wrong flavor on retail.
    let assets: [Asset<'_>; 1] = [(
        "Era.zip",
        &[],
        vec![("Era/Era.toc", "## Interface: 11509\n")],
    )];
    gh_release(&home.http, "https://github.com/o/era", "v1", &assets);
    let r = home.run(&["install", "gh:o/era", "-t", "retail"]);
    assert_eq!(r.expect(3)["error"]["code"], "no_compatible_release");
    assert_eq!(
        verdicts(&home, 120_105),
        [(false, false, Value::Null)],
        "it does not load on retail\n{}",
        r.context()
    );

    // An older retail interface: it loads, out of date, at that interface; a dry run
    // learns it too.
    let home = Home::new(&["wow"]);
    let assets: [Asset<'_>; 1] = [(
        "Old.zip",
        &[("mainline", 120_000)],
        vec![("Old/Old.toc", "## Interface: 120000, 11509\n")],
    )];
    gh_release(&home.http, "https://github.com/o/old", "v1", &assets);
    let r = home.run(&["install", "gh:o/old", "-t", "retail", "--dry-run"]);
    r.expect(0);
    assert_eq!(
        verdicts(&home, 120_105),
        [(true, true, Value::from(120_000))],
        "{}",
        r.context()
    );
}

/// The `net.log` lines after the first `before`, as `(host, purpose)`.
fn lines_after(home: &Home, before: usize) -> Vec<(String, String)> {
    common::netlog::net_log(home)
        .entries
        .into_iter()
        .skip(before)
        .map(|e| (e.host, e.purpose))
        .collect()
}

#[test]
fn reinstalling_unchanged_refs_costs_one_request_each() {
    let home = Home::new(&["wow"]);
    let names = ["foo", "bar", "baz"];
    for name in names {
        let folder = format!("{}/{}.toc", name.to_uppercase(), name.to_uppercase());
        let assets: [Asset<'_>; 1] = [(
            "A.zip",
            &[("mainline", 120_105)],
            vec![(folder.as_str(), "## Interface: 120105\n")],
        )];
        gh_release(
            &home.http,
            &format!("https://github.com/o/{name}"),
            "v1",
            &assets,
        );
    }
    let args = [
        "install", "gh:o/foo", "gh:o/bar", "gh:o/baz", "-t", "retail",
    ];
    home.run(&args).expect(0);
    let before = requests(&home);
    let r = home.run(&args);
    r.expect(0);
    let lines = lines_after(&home, before);
    assert_eq!(
        lines,
        vec![("github.com".to_owned(), "release-latest".to_owned()); 3],
        "{}",
        r.context()
    );
}

#[test]
fn an_aged_entry_is_confirmed_by_one_request_and_fresh_again() {
    let home = Home::new(&["wow"]);
    foo(&home, "v1");
    home.seed_memo(&memo::of(vec![memo::entry(
        "gh:o/foo",
        "v1",
        memo::ago(20),
    )]));
    let seeded = fetched_at(&home, "foo");
    let r = home.run(&["info", "gh:o/foo"]);
    assert_eq!(r.expect(0)["data"]["latest"]["version"], "v1");
    assert_eq!(
        lines_after(&home, 0),
        [("github.com".to_owned(), "release-latest".to_owned())],
        "{}",
        r.context()
    );
    assert_ne!(fetched_at(&home, "foo"), seeded, "fetched_at bumped");
    home.run(&["info", "gh:o/foo"]).expect(0);
    assert_eq!(requests(&home), 1, "fresh again: no request");

    // A moved tag: one manifest, and `releases/latest` still once.
    home.seed_memo(&memo::of(vec![memo::entry(
        "gh:o/foo",
        "v0",
        memo::ago(20),
    )]));
    let before = requests(&home);
    let r = home.run(&["info", "gh:o/foo"]);
    assert_eq!(r.expect(0)["data"]["latest"]["version"], "v1");
    assert_eq!(
        lines_after(&home, before),
        [
            ("github.com".to_owned(), "release-latest".to_owned()),
            ("github.com".to_owned(), "release-json".to_owned())
        ],
        "{}",
        r.context()
    );
}

#[test]
fn a_probe_404_is_remembered_and_a_probe_5xx_is_not() {
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
    home.seed_memo(&memo::of(vec![
        memo::entry("gh:o/missing", "v1", memo::ago(20)),
        memo::entry("gh:o/down", "v1", memo::ago(20)),
    ]));
    let seeded = fetched_at(&home, "down");
    let r = home.run(&["info", "gh:o/missing"]);
    assert_eq!(r.expect(3)["error"]["code"], "not_found", "{}", r.context());
    let memo: Value = serde_json::from_slice(&fs::read(home.memo()).unwrap()).unwrap();
    let missing = &memo["entries"]["gh:o/missing|stable"];
    assert_eq!(
        (&missing["set"], &missing["failure"]["code"]),
        (&Value::Null, &Value::from("not_found")),
        "{memo}"
    );
    let before = requests(&home);
    home.run(&["info", "gh:o/missing"]).expect(3);
    assert_eq!(requests(&home), before, "remembered");

    home.run(&["info", "gh:o/down"]);
    assert_eq!(
        fetched_at(&home, "down"),
        seeded,
        "a 5xx probe is never stored"
    );
}
