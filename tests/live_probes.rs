//! Live probes of the keyless M1 endpoints, run at milestone checkpoints only:
//! `OMADDONS_LIVE=1 cargo test --test live_probes -- --ignored`. Each probe makes one
//! request (two for the conditional atom check) through the production stack against
//! `gh:WeakAuras/WeakAuras2` and checks raw shape only; source beads add decoder-level
//! probes here.

mod common;

use std::io::{Read, Write};

use common::env::TestEnv;
use common::http::paths;
use common::netlog::{assert_no_github_rest, read_net_log};
use common::trees::{BattlenetRoot, Prefix};
use omaddons::model::AssetLocation;
use omaddons::net::{CacheState, Http, NetConfig, NetLog, Request, stack};
use omaddons::sources::git::GitSource;
use omaddons::sources::git_hosts::GitRunner;
use omaddons::sources::github::GithubSource;
use omaddons::sources::{Source, Wanted};
use omaddons::state::paths::Paths;
use serde_json::Value;
use support::{latest_tag, live};

const REPO: &str = "https://github.com/WeakAuras/WeakAuras2";

#[cfg(test)]
mod support {
    use super::*;

    /// The production stack in an isolated environment, when `OMADDONS_LIVE=1`.
    pub fn live() -> Option<(TestEnv, Box<dyn Http>)> {
        if std::env::var("OMADDONS_LIVE").as_deref() != Ok("1") {
            return None;
        }
        let env = TestEnv::new();
        let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
        let http = stack(&NetConfig::default(), NetLog::new(&paths).unwrap(), &paths);
        Some((env, http))
    }

    /// The tag `releases/latest` redirects to.
    pub fn latest_tag(http: &dyn Http) -> String {
        let resp = http
            .send(Request::get(format!("{REPO}/releases/latest"), "probe").manual_redirects())
            .unwrap();
        assert_eq!(resp.status, 302);
        let location = resp.location().unwrap().to_owned();
        assert!(location.contains("/releases/tag/"), "{location}");
        location.rsplit('/').next().unwrap().to_owned()
    }
}

#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn latest_release_redirects_to_a_tag() {
    let Some((_env, http)) = live() else { return };
    assert!(!latest_tag(http.as_ref()).is_empty());
}

#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn the_releases_feed_revalidates() {
    let Some((_env, http)) = live() else { return };
    let mut req = Request::get(format!("{REPO}/releases.atom"), "probe");
    req.validators = true;
    let first = http.send(req.clone()).unwrap();
    assert_eq!(first.status, 200);
    assert!(first.header("etag").is_some(), "the feed carries an ETag");
    assert!(
        String::from_utf8(first.bytes().unwrap())
            .unwrap()
            .contains("<feed")
    );
    let second = http.send(req).unwrap();
    assert_eq!(
        (second.status, second.cache),
        (200, CacheState::Revalidated)
    );
}

#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn release_json_decodes() {
    let Some((_env, http)) = live() else { return };
    let tag = latest_tag(http.as_ref());
    let resp = http
        .send(Request::get(
            format!("{REPO}/releases/download/{tag}/release.json"),
            "probe",
        ))
        .unwrap();
    assert_eq!(resp.status, 200);
    let json: serde_json::Value = serde_json::from_slice(&resp.bytes().unwrap()).unwrap();
    assert!(json["releases"].is_array(), "{json}");
}

#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn commits_feed_and_codeload_zip() {
    let Some((_env, http)) = live() else { return };
    let feed = http
        .send(Request::get(format!("{REPO}/commits/main.atom"), "probe"))
        .unwrap();
    assert_eq!(feed.status, 200);
    let text = String::from_utf8(feed.bytes().unwrap()).unwrap();
    let sha: String = text
        .split("Grit::Commit/")
        .nth(1)
        .unwrap()
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    assert_eq!(sha.len(), 40, "{sha}");
    let zip = http
        .send(
            Request::get(
                format!("https://codeload.github.com/WeakAuras/WeakAuras2/zip/{sha}"),
                "probe",
            )
            .download(),
        )
        .unwrap();
    assert_eq!(zip.status, 200);
    let mut magic = [0u8; 4];
    zip.into_reader().read_exact(&mut magic).unwrap();
    assert_eq!(&magic, b"PK\x03\x04");
}

/// source-github-stable: the production decoder on WeakAuras2's live `release.json`,
/// with zero REST requests.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn github_source_decodes_live() {
    let Some((env, http)) = live() else { return };
    let wanted = Wanted::stable("gh:WeakAuras/WeakAuras2".parse().unwrap());
    let set = GithubSource::new()
        .fetch(&[wanted], http.as_ref())
        .pop()
        .unwrap()
        .unwrap();
    let release = &set.releases[0];
    assert!(!release.assets.is_empty(), "{release:?}");
    assert!(
        release.assets.iter().all(|a| !a.flavors.is_empty()),
        "{release:?}"
    );
    assert_no_github_rest(&read_net_log(&paths(&env)));

    let recent = GithubSource::recent_tags("WeakAuras", "WeakAuras2", http.as_ref()).unwrap();
    assert!(
        recent
            .tags
            .iter()
            .any(|t| release.release_id.ends_with(&format!("@{}", t.tag))),
        "{recent:?}"
    );
    let again = GithubSource::recent_tags("WeakAuras", "WeakAuras2", http.as_ref()).unwrap();
    assert!(again.revalidated && again.tags == recent.tags);
}

/// The M1 checkpoint's synthetic Forever target (1.60.x, `wowf`): the five keyless
/// publishers through the binary. Each installs, verified by the client's TOC rules, or is
/// refused with `no_compatible_release`; nothing installed is invalid afterwards, and the
/// stable channel never touches the REST API.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn keyless_installs_into_a_synthetic_forever_target() {
    let Some((env, _http)) = live() else { return };
    BattlenetRoot::at(Prefix::new(&env, "battlenet").wow_root())
        .product("wowf")
        .build();
    let run = |args: &[&str]| -> Value {
        let out = env.command().args(args).arg("--json").output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let installed = run(&[
        "install",
        "weakauras",
        "dbm",
        "bigwigs",
        "plater",
        "bartender4",
        "-t",
        "forever",
    ]);
    let items = installed["data"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let summary: Vec<String> = items
        .iter()
        .map(|i| {
            format!(
                "{}: {} {}",
                i["request"],
                i["status"],
                i["to_version"]
                    .as_str()
                    .or(i["error"]["code"].as_str())
                    .unwrap_or("")
            )
        })
        .collect();
    let _ = writeln!(std::io::stderr(), "forever: {summary:#?}");
    assert_eq!(
        (installed["exit_code"].as_i64(), items.len()),
        (Some(0), 5),
        "{installed:#}"
    );
    for item in &items {
        let refused =
            item["status"] == "failed" && item["error"]["code"] == "no_compatible_release";
        assert!(item["status"] == "installed" || refused, "{item:#}");
    }
    let status = run(&["status"]);
    let forever = &status["data"]["targets"][0];
    assert_eq!(
        (&forever["family"], &forever["folders"]["invalid"]),
        (&Value::from("forever"), &Value::from(0)),
        "{status:#}"
    );
    assert_no_github_rest(&read_net_log(&paths(&env)));
}

/// DBM and BigWigs ship one zip for every client, with retail-only boss modules gated by
/// `AllowLoadGameType: standard`. On each synthetic Classic client and Forever the
/// current releases install their shared folders (DBM-Core among them) and skip only
/// gated folders; retail skips none. Nothing installed is unmanaged or invalid, and
/// undo empties AddOns.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn boss_mods_install_their_shared_folders_on_classic_and_forever() {
    let Some((env, _http)) = live() else { return };
    let products = [
        ("wow_classic_era", "era"),
        ("wow_anniversary", "anniversary"),
        ("wow_classic", "classic"),
        ("wowf", "forever"),
        ("wow", "retail"),
    ];
    let mut root = BattlenetRoot::at(Prefix::new(&env, "battlenet").wow_root());
    for (code, _) in products {
        root = root.product(code);
    }
    root.build();
    let run = |args: &[&str]| -> Value {
        let out = env.command().args(args).arg("--json").output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let names = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .map(|n| n.as_str().unwrap_or_default().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    for (_, id) in products {
        let installed = run(&["install", "dbm", "bigwigs", "-t", id]);
        assert_eq!(
            installed["exit_code"].as_i64(),
            Some(0),
            "{id}: {installed:#}"
        );
        for (item, core) in installed["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .zip(["DBM-Core", "BigWigs_Core"])
        {
            let folders = &item["folders"];
            let added = names(&folders["added"]);
            let skipped: Vec<(String, String)> = folders["skipped"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| {
                    (
                        s["name"].as_str().unwrap_or_default().to_owned(),
                        s["reason"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect();
            let _ = writeln!(
                std::io::stderr(),
                "{id}: {} {} added {added:?} skipped {skipped:?}",
                item["name"],
                item["to_version"]
            );
            assert_eq!(item["status"], "installed", "{id}: {item:#}");
            assert!(added.iter().any(|a| a == core), "{id}: {item:#}");
            assert_eq!(
                skipped.is_empty(),
                id == "retail",
                "{id}: only retail loads the retail modules: {item:#}"
            );
            assert!(
                skipped
                    .iter()
                    .all(|(_, reason)| reason == "game_type_excluded"),
                "{id}: {item:#}"
            );
        }
        let status = run(&["status"]);
        let target = status["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap();
        assert_eq!(
            (
                &target["folders"]["unmanaged"],
                &target["folders"]["invalid"]
            ),
            (&Value::from(0), &Value::from(0)),
            "{id}: {status:#}"
        );
        let undone = run(&["undo"]);
        assert_eq!(undone["exit_code"].as_i64(), Some(0), "{id}: {undone:#}");
    }
    assert_no_github_rest(&read_net_log(&paths(&env)));
}

/// git refs on another host through the git binary: `ls-remote` for the default branch of
/// tullamods/Dominos on Codeberg, then a shallow checkout, each logged `via: git`.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn git_hosts_resolve_and_check_out_live() {
    let Some((env, _http)) = live() else { return };
    let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
    let net = NetConfig {
        git_hosts: vec!["codeberg.org".to_owned()],
        ..NetConfig::default()
    };
    let git = GitRunner::new(&net, NetLog::new(&paths).unwrap());
    let url = "https://codeberg.org/tullamods/Dominos";
    let set = git
        .fetch(&Wanted::stable(format!("git:{url}").parse().unwrap()))
        .unwrap();
    let location = &set.releases[0].assets[0].location;
    let checkout = match location {
        AssetLocation::GitCheckout { rev, sha, .. } => Some((rev.clone(), sha.clone())),
        _ => None,
    };
    assert!(checkout.is_some(), "{location:?}");
    let (rev, sha) = checkout.unwrap();
    let dest = env.root().join("checkout");
    std::fs::create_dir_all(&dest).unwrap();
    git.materialize(url, &rev, &sha, &dest).unwrap();
    assert!(dest.join("Dominos").is_dir() || dest.join("Dominos.toc").is_file());
    let log = read_net_log(&paths);
    let purposes: Vec<&str> = log
        .iter()
        .filter(|e| e.host == "codeberg.org" && e.method == "GIT")
        .map(|e| e.purpose.as_str())
        .collect();
    assert_eq!(purposes, ["ls-remote", "clone", "rev-parse"], "{log:#?}");
}

/// git refs for GitHub repositories: the default branch through `commits.atom`, a tag
/// through its own commits feed, both pinned to a commit on codeload; zero REST.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn git_source_decodes_live() {
    let Some((env, http)) = live() else { return };
    let wanted = [
        "git:https://github.com/shagu/pfUI",
        "git:https://github.com/fondlez/GroupBulletinBoard#v1.2.3",
    ]
    .map(|r| Wanted::stable(r.parse().unwrap()));
    for set in GitSource::default().fetch(&wanted, http.as_ref()) {
        let release = set.unwrap().releases.remove(0);
        let (_, sha) = release.release_id.rsplit_once('@').unwrap();
        assert!(
            sha.len() == 40 && release.version == sha[..7],
            "{release:?}"
        );
        assert_eq!(
            release.assets[0].location,
            omaddons::model::AssetLocation::Url(format!(
                "{}/zip/{sha}",
                release
                    .release_id
                    .trim_start_matches("git:https://github.com")
                    .split('@')
                    .next()
                    .map(|path| format!("https://codeload.github.com{path}"))
                    .unwrap()
            )),
        );
    }
    assert_no_github_rest(&read_net_log(&paths(&env)));
}
