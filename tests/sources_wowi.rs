//! The WoWInterface source end to end, over synthetic `filedetails` and CDN fixtures:
//! install by ref and by pasted link with the MD5 checked against `UIMD5`, the content
//! identity `wowi:<id>@<md5>`, a tampered body, unknown and vanished projects, the
//! no-op re-run and an MD5 change through `outdated`, pins, and the `sources.wowi`
//! enable flag. Request counts come from `net.log`. `tests/fixtures/http/wowi/` is a
//! recording of the live API and CDN (gzipped as served) that the source decodes and
//! installs from.

use crate::common;

use common::addons::toc;
use common::archives::zip_text;
use common::env::TestEnv;
use common::home::Home;
use common::http::{record as record_http, replay, scenario};
use common::netlog::net_log;
use common::state::record;
use common::trees::tree_hash;
use common::wowi::{details, wowi_details, wowi_file, wowi_unknown};
use serde_json::{Value, json};
use support::{bugsack, check_recorded, items, only_new, state_files};

const UID: u64 = 5995;

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::path::Path;

    use md5::{Digest, Md5};
    use omaddons::error::ErrorCode;
    use omaddons::model::AssetLocation;
    use omaddons::net::{Http, Request};
    use omaddons::sources::{SourceConfigView, SourceRegistry, Wanted, fetch_once};
    use serde_json::Value;

    use super::common::netlog::NetLogView;
    use super::{UID, details, toc, wowi_details, wowi_file, zip_text};

    /// What the recorded scenario shows, live or replayed: BugSack's one release, its
    /// identity the MD5 its file really has, with plain-text notes and its name; and an
    /// id that does not exist is `not_found`. Returns the release id.
    pub fn check_recorded(http: &dyn Http) -> String {
        let registry = SourceRegistry::builtin(&SourceConfigView::all_enabled());
        let bugsack = Wanted::stable(format!("wowi:{UID}").parse().unwrap());
        let mut results = fetch_once([bugsack.clone()], &registry, http);
        let set = results.remove(&bugsack).unwrap().unwrap();
        let release = &set.releases[0];
        let md5 = release
            .release_id
            .strip_prefix("wowi:5995@")
            .unwrap_or_default();
        assert_eq!(md5.len(), 32, "{release:?}");
        assert_eq!(release.name_hint.as_deref(), Some("BugSack"));
        let notes = release.notes.as_deref().unwrap_or_default();
        assert!(
            !notes.is_empty() && !omaddons::text::has_markup(notes) && !notes.contains('\r'),
            "{notes:?}"
        );
        let asset = &release.assets[0];
        assert_eq!(asset.expected_hash.as_ref().map(|h| h.hex()), Some(md5));
        let url = match &asset.location {
            AssetLocation::Url(url) => Some(url.clone()),
            _ => None,
        };
        assert!(url.is_some(), "{asset:?}");
        let body = http
            .send(Request::get(url.unwrap(), "asset"))
            .unwrap()
            .error_for_status("asset")
            .unwrap()
            .bytes()
            .unwrap();
        let got: String = Md5::digest(&body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(got, md5, "the file's MD5 is UIMD5");

        let unknown = Wanted::stable("wowi:999999999".parse().unwrap());
        let mut results = fetch_once([unknown.clone()], &registry, http);
        let error = results.remove(&unknown).unwrap().unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound, "{error:?}");
        release.release_id.clone()
    }

    /// BugSack `version` as WoWInterface serves it: its zip (one folder whose TOC
    /// carries `## X-WoWI-ID`, a `core.lua` that differs per version) at the CDN, and
    /// its `filedetails` entry; returns the zip's MD5.
    pub fn bugsack(scenario: &Path, version: &str) -> String {
        let toc = toc(
            "120105",
            &[
                "## Title: BugSack",
                "## X-WoWI-ID: 5995",
                &format!("## Version: {version}"),
            ],
        );
        let core = format!("-- BugSack {version}");
        let zip = zip_text(&[("BugSack/BugSack.toc", &toc), ("BugSack/core.lua", &core)]);
        let md5 = wowi_file(scenario, UID, &zip);
        wowi_details(scenario, &[UID], &[details(UID, "BugSack", version, &md5)]);
        md5
    }

    /// `(target, name, status)` per item of an `outdated` envelope.
    pub fn items(envelope: &Value) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for t in envelope["data"]["targets"].as_array().unwrap() {
            for i in t["items"].as_array().unwrap() {
                out.push((
                    t["target"]["id"].as_str().unwrap().to_owned(),
                    i["name"].as_str().unwrap().to_owned(),
                    i["status"].as_str().unwrap().to_owned(),
                ));
            }
        }
        out
    }

    /// The `net.log` lines after the first `before`.
    pub fn only_new(log: NetLogView, before: usize) -> NetLogView {
        NetLogView {
            entries: log.entries.into_iter().skip(before).collect(),
        }
    }

    /// Every file under `dir` but `net.log` (and its rotation), with its bytes; empty when
    /// `dir` does not exist.
    pub fn state_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            let Ok(listing) = std::fs::read_dir(dir) else {
                return;
            };
            for item in listing {
                let path = item.unwrap().path();
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                if rel.starts_with("net.log") {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    out.insert(rel, std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }
}

fn triples(expected: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    expected
        .iter()
        .map(|(t, n, s)| ((*t).to_owned(), (*n).to_owned(), (*s).to_owned()))
        .collect()
}

/// Re-records `tests/fixtures/http/wowi/` from the live API and CDN: BugSack's
/// `filedetails`, its file, and a batch of an id that does not exist. Delete the
/// directory first, then review the diff (fixtures are public; trim contributor names
/// from changelogs).
/// `OMADDONS_RECORD=1 cargo test --test integration -- --ignored sources_wowi::record`
#[test]
#[ignore = "records live fixtures: OMADDONS_RECORD=1"]
fn record_wowi() {
    // A recorder run on purpose, not a checkpoint check: unset, it has nothing to do.
    if std::env::var("OMADDONS_RECORD").as_deref() != Ok("1") {
        return;
    }
    let env = TestEnv::new();
    let http = record_http(&env, &scenario("wowi"));
    check_recorded(http.as_ref());
}

#[test]
fn the_recorded_api_decodes_and_its_file_installs() {
    let env = TestEnv::new();
    let http = replay(&env, &scenario("wowi"));
    let release_id = check_recorded(http.as_ref());

    let mut home = Home::new(&["wow"]);
    home.http = scenario("wowi");
    let r = home.run(&["install", "wowi:5995", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["items"][0]["name"],
        "BugSack",
        "{}",
        r.context()
    );
    let addon = &record(&home, "retail").addons["bugsack"];
    assert_eq!(addon.release_id.as_deref(), Some(release_id.as_str()));
}

#[test]
fn install_by_ref_checks_the_md5_and_records_the_content_identity() {
    let home = Home::new(&["wow"]);
    let md5 = bugsack(&home.http, "v12.1.7");

    let r = home.run(&["install", "wowi:5995", "-t", "retail"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["items"][0];
    assert_eq!(
        (&item["status"], &item["name"], &item["ref"]),
        (
            &Value::from("installed"),
            &Value::from("BugSack"),
            &Value::from("wowi:5995")
        ),
        "{}",
        r.context()
    );
    let addon = &record(&home, "retail").addons["bugsack"];
    assert_eq!(
        addon.release_id.as_deref(),
        Some(format!("wowi:5995@{md5}").as_str())
    );
    assert_eq!(addon.version, "v12.1.7");
    assert!(home.addons("_retail_").join("BugSack/core.lua").is_file());
    let log = net_log(&home);
    assert_eq!(
        (
            log.count_purpose("wowi-details"),
            log.count_purpose("asset")
        ),
        (1, 1),
        "{}",
        log.context()
    );
    assert_eq!(log.count("cdn.wowinterface.com", "asset"), 1);

    // The changelog is remembered as plain text with the release.
    let (memo, _) = omaddons::state::memo::load(&common::http::paths(&home.env));
    let notes: Vec<&str> = memo
        .entries
        .values()
        .filter_map(|e| e.set.as_ref())
        .flat_map(|s| &s.releases)
        .filter_map(|r| r.notes.as_deref())
        .collect();
    assert_eq!(notes, ["BugSack\n\n- v12.1.7: fixed things"]);

    // Again: current, and nothing downloaded.
    let before = net_log(&home).len();
    let r = home.run(&["install", "wowi:5995", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["items"][0]["status"],
        "unchanged",
        "{}",
        r.context()
    );
    let new = only_new(net_log(&home), before);
    assert_eq!(new.count_purpose("asset"), 0, "{}", new.context());

    let r = home.run(&["outdated", "-v"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[("retail", "BugSack", "current")]),
        "{}",
        r.context()
    );
}

#[test]
fn pasted_links_install_the_same_release() {
    let home = Home::new(&["wow"]);
    let md5 = bugsack(&home.http, "v12.1.7");
    let r = home.run(&[
        "install",
        "https://www.wowinterface.com/downloads/info5995-BugSack.html",
        "-t",
        "retail",
    ]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["items"][0]["ref"],
        "wowi:5995",
        "{}",
        r.context()
    );
    let addon = &record(&home, "retail").addons["bugsack"];
    assert_eq!(
        addon.release_id.as_deref(),
        Some(format!("wowi:5995@{md5}").as_str())
    );
    for link in [
        "https://www.wowinterface.com/downloads/fileinfo.php?id=5995",
        "https://cdn.wowinterface.com/downloads/getfile.php?id=5995&d=1760300000&minion",
    ] {
        let r = home.run(&["install", link, "-t", "retail"]);
        let envelope = r.expect(0);
        let item = &envelope["data"]["items"][0];
        assert_eq!(
            (&item["ref"], &item["status"]),
            (&Value::from("wowi:5995"), &Value::from("unchanged")),
            "{}",
            r.context()
        );
    }
}

#[test]
fn a_tampered_body_fails_hash_mismatch_and_changes_nothing() {
    let home = Home::new(&["wow"]);
    bugsack(&home.http, "v12.1.7");
    // The API promises the real file's MD5; the CDN serves other bytes.
    let other = zip_text(&[("BugSack/BugSack.toc", &toc("120105", &["## Title: Evil"]))]);
    wowi_file(&home.http, UID, &other);
    home.run(&["status"]).expect(0);
    let addons = tree_hash(&home.addons("_retail_"));
    let state = state_files(&home.state());

    let r = home.run(&["install", "wowi:5995", "-t", "retail"]);
    let envelope = r.expect(1);
    let error = &envelope["error"];
    assert_eq!(error["code"], "hash_mismatch", "{}", r.context());
    assert_eq!(
        net_log(&home).count_purpose("asset"),
        2,
        "one retry, then the failure"
    );
    let blobs = common::http::paths(&home.env).blobs_dir();
    let stored = std::fs::read_dir(&blobs).map_or(0, |d| {
        d.filter(|e| e.as_ref().unwrap().path().is_file()).count()
    });
    assert_eq!(stored, 0, "no blob stored");
    assert_eq!(tree_hash(&home.addons("_retail_")), addons);
    assert_eq!(state_files(&home.state()), state, "no state changed");
}

/// omad-t8m3: a WoWInterface file is checked against its published MD5 and, under
/// `sync --locked`, against the lock as well. On a fresh machine the exported lock holds;
/// a file re-uploaded under the same version, whose new MD5 the API publishes, still
/// fails the lock and installs nothing.
#[test]
fn a_locked_sync_checks_the_lock_besides_the_published_md5() {
    let old = Home::new(&["wow"]);
    bugsack(&old.http, "v12.1.7");
    old.run(&["install", "wowi:5995", "-t", "retail"]).expect(0);
    let toml = old.run(&["export"]).expect(0)["data"]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        toml.contains("sha256 = ") && toml.contains("version = \"v12.1.7\""),
        "{toml}"
    );

    let same = Home::new(&["wow"]);
    bugsack(&same.http, "v12.1.7");
    let file = same.write_desired(&toml).display().to_string();
    let r = same.run(&["sync", &file, "--locked"]);
    r.expect(0);
    assert!(
        same.addons("_retail_").join("BugSack").is_dir(),
        "the published MD5 and the lock both hold: {}",
        r.context()
    );

    let reuploaded = Home::new(&["wow"]);
    let zip = zip_text(&[
        (
            "BugSack/BugSack.toc",
            &toc("120105", &["## Title: BugSack", "## Version: v12.1.7"]),
        ),
        ("BugSack/core.lua", "-- BugSack v12.1.7, packaged again"),
    ]);
    let md5 = wowi_file(&reuploaded.http, UID, &zip);
    wowi_details(
        &reuploaded.http,
        &[UID],
        &[details(UID, "BugSack", "v12.1.7", &md5)],
    );
    let file = reuploaded.write_desired(&toml).display().to_string();
    let r = reuploaded.run(&["sync", &file, "--locked"]);
    let envelope = r.expect(1);
    let error = &envelope["error"];
    assert_eq!(
        (&error["code"], &error["details"]["reason"]),
        (&json!("hash_mismatch"), &json!("lock_mismatch")),
        "the published MD5 holds, the lock does not: {}",
        r.context()
    );
    assert_eq!(error["fix_argv"], json!(["omaddons", "sync", file]));
    assert!(!reuploaded.addons("_retail_").join("BugSack").exists());
}

#[test]
fn an_unknown_id_is_not_found_with_an_argv_fix() {
    let home = Home::new(&["wow"]);
    wowi_unknown(&home.http, &[404_404]);
    let r = home.run(&["install", "wowi:404404", "-t", "retail"]);
    let envelope = r.expect(3);
    let error = &envelope["error"];
    assert_eq!(error["code"], "not_found", "{}", r.context());
    assert_eq!(
        error["message"],
        "wowi:404404 does not exist or was removed from WoWInterface"
    );
    assert_eq!(error["fix_argv"], json!(["omaddons", "help", "install"]));
}

#[test]
fn a_vanished_project_is_failed_and_remembered() {
    let mut home = Home::new(&["wow"]);
    bugsack(&home.http, "v12.1.7");
    home.run(&["install", "wowi:5995", "-t", "retail"])
        .expect(0);
    let gone = home.scenario("http-gone");
    wowi_unknown(&gone, &[UID]);

    let r = home.run(&["outdated", "--refresh"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[("retail", "BugSack", "failed")]),
        "{}",
        r.context()
    );
    let error = &envelope["data"]["targets"][0]["items"][0]["error"];
    assert_eq!(error["code"], "not_found");
    assert_eq!(
        error["fix_argv"],
        json!(["omaddons", "remove", "BugSack", "-t", "retail"])
    );

    let before = net_log(&home).len();
    let r = home.run(&["outdated"]);
    let again = r.expect(0);
    assert_eq!(
        items(&again),
        triples(&[("retail", "BugSack", "failed")]),
        "{}",
        r.context()
    );
    assert_eq!(
        again["data"]["targets"][0]["items"][0]["error"]["fix_argv"],
        error["fix_argv"]
    );
    assert_eq!(net_log(&home).len(), before, "remembered: no request");
}

#[test]
fn a_new_file_is_an_update_and_a_pin_matches_the_current_version_only() {
    let mut home = Home::new(&["wow"]);
    bugsack(&home.http, "v12.1.7");
    home.run(&["install", "wowi:5995", "-t", "retail"])
        .expect(0);

    let r = home.run(&[
        "install",
        "wowi:5995",
        "--version",
        "12.1.6",
        "-t",
        "retail",
    ]);
    let envelope = r.expect(3);
    let error = &envelope["error"];
    assert_eq!(error["code"], "not_found", "{}", r.context());
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("WoWInterface serves only its current file (v12.1.7)"),
        "{}",
        r.context()
    );
    assert_eq!(
        error["fix_argv"],
        json!(["omaddons", "install", "wowi:5995"])
    );

    // A new file with a new MD5, checked once the remembered answer is a day old.
    let next = home.scenario("http-v2");
    let md5 = bugsack(&next, "v12.1.8");
    let paths = common::http::paths(&home.env);
    let (mut memo, _) = omaddons::state::memo::load(&paths);
    for entry in memo.entries.values_mut() {
        entry.fetched_at -= jiff::SignedDuration::from_hours(24);
    }
    home.seed_memo(&memo);
    let r = home.run(&["outdated"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[("retail", "BugSack", "update_available")]),
        "{}",
        r.context()
    );
    let item = &envelope["data"]["targets"][0]["items"][0];
    assert_eq!(
        (&item["latest"]["version"], &item["confidence"]),
        (&Value::from("v12.1.8"), &Value::from("likely")),
        "{}",
        r.context()
    );

    let r = home.run(&["update"]);
    r.expect(0);
    let addon = &record(&home, "retail").addons["bugsack"];
    assert_eq!(
        addon.release_id.as_deref(),
        Some(format!("wowi:5995@{md5}").as_str()),
        "{}",
        r.context()
    );
    assert_eq!(addon.version, "v12.1.8");
}

#[test]
fn the_enable_flag_turns_the_source_off_and_its_fix_turns_it_on() {
    let home = Home::new(&["wow"]);
    bugsack(&home.http, "v12.1.7");
    home.run(&["install", "wowi:5995", "-t", "retail"])
        .expect(0);
    home.run(&["config", "set", "sources.wowi.enabled", "false"])
        .expect(0);

    let before = net_log(&home).len();
    let r = home.run(&["outdated", "--refresh", "-v"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[("retail", "BugSack", "unchecked")]),
        "{}",
        r.context()
    );
    assert_eq!(
        envelope["data"]["targets"][0]["items"][0]["unchecked_reason"],
        "source_disabled"
    );

    let r = home.run(&["install", "wowi:5995", "-t", "retail"]);
    let envelope = r.expect(1);
    let error = &envelope["error"];
    assert_eq!(error["code"], "source_disabled", "{}", r.context());
    assert_eq!(
        error["message"],
        "wowi:5995: the WoWInterface source is disabled in config.toml"
    );
    let fix: Vec<&str> = error["fix_argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(
        fix,
        ["omaddons", "config", "set", "sources.wowi.enabled", "true"]
    );
    assert_eq!(
        net_log(&home).len(),
        before,
        "a disabled source makes no request"
    );

    home.run(&fix[1..]).expect(0);
    let r = home.run(&["install", "wowi:5995", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["items"][0]["ref"],
        "wowi:5995",
        "{}",
        r.context()
    );
}

/// A file whose details publish no usable MD5 (none, or a malformed one) installs, and
/// says it was not checked: in JSON, to a human, in the release memo, and again when a
/// reinstall reuses its cached blob without a request. Its identity is its date.
#[test]
fn a_file_without_a_usable_md5_installs_and_says_it_was_not_checked() {
    for published in ["", "not-an-md5"] {
        let home = Home::new(&["wow"]);
        let toc = toc("120105", &["## Title: BugSack", "## Version: v1"]);
        let zip = zip_text(&[("BugSack/BugSack.toc", &toc)]);
        wowi_file(&home.http, UID, &zip);
        wowi_details(
            &home.http,
            &[UID],
            &[details(UID, "BugSack", "v1", published)],
        );
        let expected =
            "BugSack v1: its download was not checked: WoWInterface published no usable MD5 for it";

        let r = home.run(&["install", "wowi:5995", "-t", "retail"]);
        let envelope = r.expect(0);
        let warned: Vec<&str> = envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] == "download_unverified")
            .filter_map(|w| w["message"].as_str())
            .collect();
        assert_eq!(warned, [expected], "{published:?}: {}", r.context());
        let addon = &record(&home, "retail").addons["bugsack"];
        assert_eq!(
            addon.release_id.as_deref(),
            Some("wowi:5995@d1760300000000"),
            "{published:?}"
        );
        let (memo, _) = omaddons::state::memo::load(&common::http::paths(&home.env));
        assert!(
            memo.candidates
                .iter()
                .any(|f| f.release_id == "wowi:5995@d1760300000000" && f.unverified),
            "{published:?}"
        );

        // A reinstall takes the cached blob: no download, and still not checked.
        let before = net_log(&home).len();
        let argv: Vec<String> = ["install", "wowi:5995", "-t", "retail", "--reinstall"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        let human = home.exec(&argv);
        assert_eq!(human.code, 0, "{}", human.context());
        assert!(human.stderr.contains(expected), "{}", human.context());
        let new = only_new(net_log(&home), before);
        assert_eq!(new.count_purpose("asset"), 0, "{}", new.context());
    }
}
