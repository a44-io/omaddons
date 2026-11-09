//! `git:` refs to GitHub repositories: heads from the commits Atom feed, archives from
//! codeload, never REST. Recorded: fondlez/GroupBulletinBoard (a 3.3.5a addon with no
//! releases) in `tests/fixtures/http/git-github/`; synthetic scenarios cover tags, shas
//! and a moved branch. The checkpoint case installs it into a 3.3.5a client.

use crate::common;

use std::fs;
use std::path::Path;

use common::archives::zip_text;
use common::env::TestEnv;
use common::home::Home;
use common::http::{fixture, fixture_bytes, paths, record, replay, scenario};
use common::netlog::{assert_no_github_rest, read_net_log};
use common::trees::CustomRoot;
use omaddons::model::{AssetLocation, ReleaseSet};
use omaddons::net::Request;
use omaddons::sources::{SourceConfigView, SourceRegistry, Wanted, fetch_once};
use serde_json::Value;
use support::{GBB, GBB_SHA, GBB_TAG, fetch_with};

#[cfg(test)]
mod support {
    use super::*;

    pub const GBB: &str = "git:https://github.com/fondlez/GroupBulletinBoard";
    /// The recorded head of `main` (and of the default branch).
    pub const GBB_SHA: &str = "fbf8ed267da1d86e5fa812cc5d7629aedabbe17a";
    /// A recorded tag (the repository's newest, 2026-10-15).
    pub const GBB_TAG: &str = "v1.2.3";

    /// One fetch through the production stack over `scenario`.
    pub fn fetch_with(
        env: &TestEnv,
        scenario: &Path,
        r: &str,
        version: Option<&str>,
    ) -> omaddons::error::Result<ReleaseSet> {
        let http = replay(env, scenario);
        let mut wanted = Wanted::stable(r.parse().unwrap());
        wanted.version = version.map(str::to_owned);
        fetch_once(
            [wanted.clone()],
            &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
            http.as_ref(),
        )
        .remove(&wanted)
        .unwrap()
    }
}

/// Re-records the GroupBulletinBoard scenario from live GitHub. Delete the directory
/// first, then trim the Atom bodies to their newest entry with the author replaced by
/// "contributor" (fixtures are public).
/// `OMADDONS_RECORD=1 cargo test --test integration -- --ignored git_github::record`
#[test]
#[ignore = "records live fixtures: OMADDONS_RECORD=1"]
fn record_git_github() {
    // A recorder run on purpose, not a checkpoint check: unset, it has nothing to do.
    if std::env::var("OMADDONS_RECORD").as_deref() != Ok("1") {
        return;
    }
    let env = TestEnv::new();
    let http = record(&env, &scenario("git-github"));
    let wanted = [
        Wanted::stable(GBB.parse().unwrap()),
        Wanted::stable(format!("{GBB}#main").parse().unwrap()),
    ];
    for (w, result) in fetch_once(
        wanted,
        &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
        http.as_ref(),
    ) {
        assert!(result.is_ok(), "{}: {result:?}", w.r#ref);
        let set = result.unwrap();
        let location = &set.releases[0].assets[0].location;
        let url = match location {
            AssetLocation::Url(url) => Some(url.clone()),
            _ => None,
        };
        assert!(url.is_some(), "not a URL asset: {location:?}");
        http.send(Request::get(url.unwrap(), "asset")).unwrap();
    }
    // A tag: GitHub serves its commits feed too (resolution only; no archive recorded).
    let tag = Wanted::stable(format!("{GBB}#{GBB_TAG}").parse().unwrap());
    fetch_once(
        [tag],
        &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
        http.as_ref(),
    );
}

/// GitHub serves `commits/<tag>.atom` like a branch's feed, headed by the tag's commit,
/// so a tag resolves to that commit and its archive is pinned to it.
#[test]
fn a_recorded_tag_resolves_through_its_commits_feed() {
    let env = TestEnv::new();
    let set = fetch_with(
        &env,
        &scenario("git-github"),
        &format!("{GBB}#{GBB_TAG}"),
        None,
    )
    .unwrap();
    let release = &set.releases[0];
    assert_eq!(
        (release.release_id.as_str(), release.version.as_str()),
        (
            "git:https://github.com/fondlez/GroupBulletinBoard@95653f8e6a9d9b18ffecb5f14cdcee3f0c5d93a0",
            "95653f8"
        )
    );
    assert_eq!(
        release.assets[0].location,
        AssetLocation::Url(
            "https://codeload.github.com/fondlez/GroupBulletinBoard/zip/95653f8e6a9d9b18ffecb5f14cdcee3f0c5d93a0".into()
        )
    );
    let log = read_net_log(&paths(&env));
    assert_eq!(
        log.iter()
            .map(|e| (e.host.as_str(), e.path.as_str(), e.status))
            .collect::<Vec<_>>(),
        [(
            "github.com",
            "/fondlez/GroupBulletinBoard/commits/v1.2.3.atom",
            Some(200)
        )],
        "{log:?}"
    );
}

#[test]
fn the_default_branch_and_an_explicit_branch_resolve_to_the_head() {
    let env = TestEnv::new();
    for r in [GBB.to_owned(), format!("{GBB}#main")] {
        let set = fetch_with(&env, &scenario("git-github"), &r, None).unwrap();
        let release = &set.releases[0];
        assert_eq!(
            release.release_id,
            format!("git:https://github.com/fondlez/GroupBulletinBoard@{GBB_SHA}"),
            "{r}"
        );
        assert_eq!(release.version, &GBB_SHA[..7]);
        assert_eq!(
            release.assets[0].location,
            AssetLocation::Url(format!(
                "https://codeload.github.com/fondlez/GroupBulletinBoard/zip/{GBB_SHA}"
            ))
        );
    }
    let log = read_net_log(&paths(&env));
    assert_no_github_rest(&log);
    assert!(
        log.iter().all(|e| e.host == "github.com"),
        "resolution only reads the Atom feed: {log:?}"
    );
}

/// A synthetic GitHub repository `o/r`: branch `main` at `head`, and a tag `v1.0` whose
/// feed is not served (the by-name fallback).
fn synthetic(dir: &Path, head: &str) {
    let atom = format!(
        "<?xml version=\"1.0\"?><feed><entry><id>tag:github.com,2008:Grit::Commit/{head}</id><title>x</title></entry></feed>"
    );
    fixture(
        dir,
        "main",
        "https://github.com/o/r/commits/main.atom",
        200,
        &[],
        Some(&atom),
    );
    fixture(
        dir,
        "default",
        "https://github.com/o/r/commits.atom",
        200,
        &[],
        Some(&atom),
    );
    fixture(
        dir,
        "tag-is-no-branch",
        "https://github.com/o/r/commits/v1.0.atom",
        404,
        &[],
        None,
    );
    let zip = zip_text(&[("r-main/R/R.toc", "## Interface: 30300\n")]);
    for (name, at) in [
        ("zip-head", head.to_owned()),
        ("zip-tag", "refs/tags/v1.0".to_owned()),
    ] {
        fixture_bytes(
            dir,
            name,
            &format!("https://codeload.github.com/o/r/zip/{at}"),
            200,
            &[],
            Some(&zip),
        );
    }
}

#[test]
fn tags_after_a_branch_404_shas_without_a_request_and_a_moved_branch() {
    let env = TestEnv::new();
    let a = "a".repeat(40);
    let b = "b".repeat(40);
    let first = env.root().join("first");
    synthetic(&first, &a);
    let moved = env.root().join("moved");
    synthetic(&moved, &b);

    let tag = fetch_with(&env, &first, "git:https://github.com/o/r#v1.0", None).unwrap();
    assert_eq!(
        tag.releases[0].release_id,
        "git:https://github.com/o/r@v1.0"
    );
    assert_eq!(
        tag.releases[0].assets[0].location,
        AssetLocation::Url("https://codeload.github.com/o/r/zip/refs/tags/v1.0".into())
    );

    let before = read_net_log(&paths(&env)).len();
    let sha = fetch_with(
        &env,
        &first,
        &format!("git:https://github.com/o/r#{a}"),
        None,
    )
    .unwrap();
    assert_eq!(sha.releases[0].version, "aaaaaaa");
    assert_eq!(
        read_net_log(&paths(&env)).len(),
        before,
        "a full sha needs no request"
    );

    let one = fetch_with(&env, &first, "git:https://github.com/o/r#main", None).unwrap();
    let same = fetch_with(&env, &first, "git:https://github.com/o/r#main", None).unwrap();
    let other = fetch_with(&env, &moved, "git:https://github.com/o/r#main", None).unwrap();
    assert_eq!(
        one.releases[0].release_id, same.releases[0].release_id,
        "unchanged branch"
    );
    assert_ne!(
        one.releases[0].release_id, other.releases[0].release_id,
        "moved branch"
    );

    // --version is a rev, and must agree with #rev.
    let by_version = fetch_with(&env, &first, "git:https://github.com/o/r", Some("v1.0")).unwrap();
    assert_eq!(
        by_version.releases[0].release_id,
        tag.releases[0].release_id
    );
    let err = fetch_with(
        &env,
        &first,
        "git:https://github.com/o/r#main",
        Some("v1.0"),
    )
    .unwrap_err();
    assert_eq!(err.code.wire(), "usage");
    assert_no_github_rest(&read_net_log(&paths(&env)));
}

#[test]
fn a_legacy_addon_installs_into_a_335_client_from_its_repository() {
    let mut home = Home::new(&[]);
    home.http = scenario("git-github");
    let root = CustomRoot::at(home.env.home().join("Games/wow-335"))
        .version(3, 3, 5, 12_340)
        .build();
    home.run(&[
        "targets",
        "add",
        &root.display().to_string(),
        "--name",
        "wotlk",
    ])
    .expect(0);

    let r = home.run(&["install", "gbb", "-t", "wotlk"]);
    let envelope: Value = r.expect(0);
    let item = &envelope["data"]["items"][0];
    assert_eq!(
        (
            &item["ref"],
            &item["name"],
            &item["status"],
            &item["to_version"]
        ),
        (
            &Value::from("git:https://github.com/fondlez/GroupBulletinBoard#main"),
            &Value::from("GroupBulletinBoard"),
            &Value::from("installed"),
            &Value::from(&GBB_SHA[..7])
        ),
        "{}",
        r.context()
    );
    let toc =
        fs::read_to_string(root.join("Interface/AddOns/GroupBulletinBoard/GroupBulletinBoard.toc"))
            .unwrap();
    assert!(toc.contains("## Interface: 30300"), "{toc}");

    // Again: the branch has not moved, so nothing changes.
    let again = home.run(&["install", "gbb", "-t", "wotlk"]).expect(0);
    assert_eq!(again["data"]["items"][0]["status"], "unchanged");
    let log = read_net_log(&paths(&home.env));
    assert_no_github_rest(&log);
    let hosts: std::collections::BTreeSet<&str> = log.iter().map(|e| e.host.as_str()).collect();
    assert!(
        hosts
            .iter()
            .all(|h| matches!(*h, "github.com" | "codeload.github.com")),
        "{hosts:?}"
    );

    // --version on a git ref is a rev; disagreeing with #rev is usage.
    let r = home.run(&[
        "install",
        "git:https://github.com/fondlez/GroupBulletinBoard#main",
        "--version",
        "v9",
        "-t",
        "wotlk",
    ]);
    assert_eq!(r.expect(2)["error"]["code"], "usage", "{}", r.context());
}

/// omad-t8m3: codeload serves `git archive`'s zip of one commit, the same bytes every
/// time, so an exported lock's `sha256` holds on a machine whose cache never saw it; the
/// same entry locked to other bytes installs nothing.
#[test]
fn a_locked_sync_verifies_a_commits_archive_on_an_empty_cache() {
    let wotlk = |home: &Home| {
        let root = CustomRoot::at(home.env.home().join("Games/wow-335"))
            .version(3, 3, 5, 12_340)
            .build();
        let root = root.display().to_string();
        home.run(&["targets", "add", &root, "--name", "wotlk"])
            .expect(0);
    };
    let mut old = Home::new(&[]);
    old.http = scenario("git-github");
    wotlk(&old);
    old.run(&["install", "gbb", "-t", "wotlk"]).expect(0);
    let toml = old.run(&["export"]).expect(0)["data"]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    let lock = toml
        .lines()
        .find_map(|l| l.strip_prefix("sha256 = \""))
        .map(|l| l.trim_end_matches('"').to_owned())
        .unwrap_or_else(|| panic!("the export locks the archive: {toml}"));

    for (name, text, installs) in [
        ("the exported lock", toml.clone(), true),
        (
            "a lock on other bytes",
            toml.replace(&lock, &"0".repeat(64)),
            false,
        ),
    ] {
        let mut new = Home::new(&[]);
        new.http = scenario("git-github");
        wotlk(&new);
        let file = new.write_desired(&text);
        let r = new.run(&["sync", &file.display().to_string(), "--locked"]);
        let addons = new.env.home().join("Games/wow-335/Interface/AddOns");
        if installs {
            r.expect(0);
            assert!(
                addons.join("GroupBulletinBoard").is_dir(),
                "{name}: {}",
                r.context()
            );
        } else {
            let envelope = r.expect(1);
            assert_eq!(
                (
                    &envelope["error"]["code"],
                    &envelope["error"]["details"]["reason"]
                ),
                (&Value::from("hash_mismatch"), &Value::from("lock_mismatch")),
                "{name}: {}",
                r.context()
            );
            assert!(!addons.join("GroupBulletinBoard").exists(), "{name}");
        }
    }
}

/// A rev that names nothing (its feed is a 404, then so is the archive by tag name) says
/// so, while a plain URL to the same archive keeps the bare download error.
#[test]
fn a_rev_that_names_nothing_is_not_found() {
    let home = Home::new(&["wow"]);
    fixture(
        &home.http,
        "feed",
        "https://github.com/o/r/commits/nope.atom",
        404,
        &[],
        None,
    );
    fixture(
        &home.http,
        "zip",
        "https://codeload.github.com/o/r/zip/refs/tags/nope",
        404,
        &[],
        Some("404: Not Found"),
    );
    let r = home.run(&["install", "git:https://github.com/o/r#nope", "-t", "retail"]);
    let envelope = r.expect(3);
    let error = &envelope["error"];
    assert_eq!(error["code"], "not_found", "{}", r.context());
    let message = error["message"].as_str().unwrap_or("");
    assert!(
        message.contains("nope")
            && message.contains("https://github.com/o/r")
            && message.contains("private"),
        "{message}"
    );

    let r = home.run(&[
        "install",
        "url:https://codeload.github.com/o/r/zip/refs/tags/nope",
        "-t",
        "retail",
    ]);
    assert_eq!(
        r.expect(1)["error"]["code"],
        "http_status",
        "{}",
        r.context()
    );
}

#[test]
fn an_unchanged_commits_feed_is_answered_through_a_304() {
    let env = TestEnv::new();
    let head = "0123456789abcdef0123456789abcdef01234567";
    let atom = format!(
        "<?xml version=\"1.0\"?><feed><entry><id>tag:github.com,2008:Grit::Commit/{head}</id><title>x</title></entry></feed>"
    );
    let feed = "https://github.com/o/r/commits/main.atom";
    // The first run stores the feed and its ETag; the second is told it did not change.
    let first = env.root().join("first");
    fixture(
        &first,
        "main",
        feed,
        200,
        &[("etag", "\"feed-1\"")],
        Some(&atom),
    );
    let second = env.root().join("second");
    fixture(&second, "main", feed, 304, &[], None);
    let r = "git:https://github.com/o/r#main";
    let a = fetch_with(&env, &first, r, None).unwrap();
    let b = fetch_with(&env, &second, r, None).unwrap();
    assert_eq!(a, b, "the same set from the cached feed");
    let log = read_net_log(&paths(&env));
    let statuses: Vec<Option<u16>> = log
        .iter()
        .filter(|e| e.purpose == "commits-atom")
        .map(|e| e.status)
        .collect();
    assert_eq!(statuses, [Some(200), Some(304)]);
}
