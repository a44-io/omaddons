//! Every catalog entry resolves through its source, replayed from upstream responses
//! recorded in `tests/fixtures/http/catalog/`.

mod common;

use common::env::TestEnv;
use common::http::{record, replay, scenario};
use common::netlog::{assert_each_url_once, assert_no_github_rest, read_net_log};
use omaddons::model::Ref;
use omaddons::resolve::catalog::Catalog;
use omaddons::sources::{SourceConfigView, SourceRegistry, Wanted, fetch_once};
use support::wanted;

#[cfg(test)]
mod support {
    use super::*;

    /// A stable fetch of every catalog entry whose ref `keep` accepts.
    pub fn wanted(keep: impl Fn(&Ref) -> bool) -> Vec<Wanted> {
        Catalog::builtin()
            .unwrap()
            .entries()
            .iter()
            .filter(|e| keep(&e.r#ref))
            .map(|e| Wanted::stable(e.r#ref.clone()))
            .collect()
    }
}

fn github(r: &Ref) -> bool {
    matches!(r, Ref::GitHub { .. })
}

fn git(r: &Ref) -> bool {
    matches!(r, Ref::Git { .. })
}

/// Re-records the scenario from live GitHub. Delete the directory first, then review
/// the diff (fixtures are public; trim contributor names from changelogs).
/// `OMADDONS_RECORD=1 cargo test --test catalog -- --ignored record`
#[test]
#[ignore = "records live fixtures: OMADDONS_RECORD=1"]
fn record_catalog() {
    if std::env::var("OMADDONS_RECORD").as_deref() != Ok("1") {
        return;
    }
    let env = TestEnv::new();
    let http = record(&env, &scenario("catalog"));
    let results = fetch_once(
        wanted(github),
        &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
        http.as_ref(),
    );
    for (w, result) in results {
        assert!(result.is_ok(), "{}: {result:?}", w.r#ref);
    }
}

/// Re-records the legacy (`git:`) entries' commits feeds into the same scenario, without
/// touching the GitHub ones. Trim each feed to its newest entry with the author
/// replaced by "contributor".
/// `OMADDONS_RECORD=1 cargo test --test catalog -- --ignored record_git`
#[test]
#[ignore = "records live fixtures: OMADDONS_RECORD=1"]
fn record_catalog_git() {
    if std::env::var("OMADDONS_RECORD").as_deref() != Ok("1") {
        return;
    }
    let env = TestEnv::new();
    let http = record(&env, &scenario("catalog"));
    let results = fetch_once(
        wanted(git),
        &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
        http.as_ref(),
    );
    for (w, result) in results {
        assert!(result.is_ok(), "{}: {result:?}", w.r#ref);
    }
}

#[test]
fn every_github_entry_resolves_without_rest() {
    let env = TestEnv::new();
    let http = replay(&env, &scenario("catalog"));
    let wanted = wanted(github);
    assert_eq!(wanted.len(), 13, "the release.json publishers");
    let results = fetch_once(
        wanted,
        &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
        http.as_ref(),
    );
    for (w, result) in &results {
        assert!(result.is_ok(), "{}: {result:?}", w.r#ref);
        let set = result.as_ref().unwrap();
        assert!(
            set.releases.first().is_some_and(|r| !r.assets.is_empty()),
            "{}: {set:?}",
            w.r#ref
        );
    }
    let log = read_net_log(&common::http::paths(&env));
    assert_no_github_rest(&log);
    assert_each_url_once(&log);
}

#[test]
fn every_git_entry_resolves() {
    let env = TestEnv::new();
    let http = replay(&env, &scenario("catalog"));
    let wanted = wanted(git);
    assert_eq!(wanted.len(), 5, "one per legacy family");
    let results = fetch_once(
        wanted,
        &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
        http.as_ref(),
    );
    for (w, result) in &results {
        assert!(result.is_ok(), "{}: {result:?}", w.r#ref);
        let release = &result.as_ref().unwrap().releases[0];
        assert!(
            release.release_id.len() > 40 && release.version.len() == 7,
            "a branch resolves to a commit: {release:?}"
        );
    }
    let log = read_net_log(&common::http::paths(&env));
    assert_no_github_rest(&log);
    assert_each_url_once(&log);
    assert!(
        log.iter().all(|e| e.host == "github.com"),
        "resolution reads only commits feeds: {log:?}"
    );
}
