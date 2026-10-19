//! The Tukui source end to end, over synthetic fixtures: ElvUI installs on retail,
//! classic, era and forever, verified by its TOCs and checked against the download's
//! `ETag` MD5; a bad `ETag`; a token that dies before its download; one list, one
//! resolve and one download for a whole `sync`; warm reinstalls with no request;
//! mixed-case slugs; an unknown slug; the memo's list; and no token anywhere it could be
//! stored or shown. Request counts come from `net.log`. `tests/fixtures/http/tukui/`
//! is a recording of the live list and one resolve (its token long dead).

mod common;

use common::archives::zip_text;
use common::env::TestEnv;
use common::home::Home;
use common::http::{record as record_http, replay, scenario};
use common::netlog::net_log;
use common::state::record;
use common::trees::tree_hash;
use common::tukui::{elvui, etag, tukui_addon, tukui_dead_token, tukui_download, tukui_list};
use serde_json::{Value, json};
use support::{check_recorded, elvui_zip, everything_under, folders, purposes};

#[cfg(test)]
mod support {
    use std::path::Path;

    use omaddons::net::Http;
    use omaddons::sources::{SourceConfigView, SourceId, SourceRegistry, Wanted, fetch_once};

    use super::common::home::Home;
    use super::common::netlog::net_log;

    /// What the recorded scenario shows, live or replayed: ElvUI and Tukui listed, ElvUI
    /// one release ranked for the clients its `patch[]` names, and `/v1/addon/elvui`
    /// handing out a download token with the canonical URL beside it.
    pub fn check_recorded(http: &dyn Http) {
        let registry = SourceRegistry::builtin(&SourceConfigView::all_enabled());
        let elvui = Wanted::stable("tukui:elvui".parse().unwrap());
        let tukui = Wanted::stable("tukui:tukui".parse().unwrap());
        let mut results = fetch_once([elvui.clone(), tukui.clone()], &registry, http);
        assert!(results.remove(&tukui).unwrap().is_ok());
        let set = results.remove(&elvui).unwrap().unwrap();
        let release = &set.releases[0];
        assert!(
            release.release_id.starts_with("tukui:elvui@"),
            "{release:?}"
        );
        let flavors: Vec<&str> = release.assets[0]
            .flavors
            .iter()
            .map(|f| f.flavor.as_str())
            .collect();
        assert!(flavors.contains(&"mainline"), "{flavors:?}");
        let source = registry.get(SourceId::Tukui).unwrap();
        let resolved = source.resolve_asset("elvui", http).unwrap();
        assert!(
            resolved
                .url
                .starts_with("https://api.tukui.org/v1/download/elvui/"),
            "{}",
            resolved.url
        );
        assert_eq!(
            resolved.canonical_url.as_deref(),
            Some("https://api.tukui.org/v1/addon/elvui")
        );
    }

    /// ElvUI's three folders as its zip has them: each with one TOC naming every client
    /// it loads on and the Tukui project headers, and a `core.lua` per version.
    pub fn elvui_zip(version: &str) -> Vec<u8> {
        let toc = "## Interface: 11509, 16001, 20506, 38002, 50504, 120100, 120105\n\
                   ## Title: ElvUI\n\
                   ## X-Tukui-ProjectID: -2\n\
                   ## X-Tukui-ProjectFolders: ElvUI, ElvUI_Libraries, ElvUI_Options\n";
        let core = format!("-- ElvUI {version}");
        super::zip_text(&[
            ("ElvUI/ElvUI.toc", toc),
            ("ElvUI/core.lua", &core),
            ("ElvUI_Libraries/ElvUI_Libraries.toc", toc),
            ("ElvUI_Options/ElvUI_Options.toc", toc),
        ])
    }

    /// `(tukui-list, tukui-addon, asset)` lines in `net.log` after the first `before`.
    pub fn purposes(home: &Home, before: usize) -> (usize, usize, usize) {
        let log = net_log(home);
        let new: Vec<&str> = log
            .entries
            .iter()
            .skip(before)
            .map(|e| e.purpose.as_str())
            .collect();
        let count = |p: &str| new.iter().filter(|x| **x == p).count();
        (count("tukui-list"), count("tukui-addon"), count("asset"))
    }

    /// The folders in `client`'s AddOns, sorted.
    pub fn folders(home: &Home, client: &str) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(home.addons(client))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Every file under `dir`, as text, with its path.
    pub fn everything_under(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(listing) = std::fs::read_dir(dir) else {
            return;
        };
        for item in listing {
            let path = item.unwrap().path();
            if path.is_dir() {
                everything_under(&path, out);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                out.push((
                    path.display().to_string(),
                    String::from_utf8_lossy(&bytes).into_owned(),
                ));
            }
        }
    }
}

/// ElvUI 16.00 on Tukui: the list, two tokens (token A serves the zip with its real
/// `ETag`), returning the zip.
fn serve_elvui(home: &Home) -> Vec<u8> {
    let zip = elvui_zip("16.00");
    tukui_list(&home.http, &[elvui()]);
    tukui_addon(&home.http, &elvui(), &["TOKEN-A", "TOKEN-B"]);
    tukui_download(&home.http, "elvui", "TOKEN-A", &zip, Some(&etag(&zip)));
    zip
}

const ELVUI: [&str; 3] = ["ElvUI", "ElvUI_Libraries", "ElvUI_Options"];

/// Re-records `tests/fixtures/http/tukui/` from the live API: the list and one
/// `/v1/addon/elvui`. The download is not recorded (3 MB; the live probe checks it), and
/// the recorded token dies within seconds. Delete the directory first, then review the
/// diff (fixtures are public).
/// `OMADDONS_RECORD=1 cargo test --test sources_tukui -- --ignored record`
#[test]
#[ignore = "records live fixtures: OMADDONS_RECORD=1"]
fn record_tukui() {
    // A recorder run on purpose, not a checkpoint check: unset, it has nothing to do.
    if std::env::var("OMADDONS_RECORD").as_deref() != Ok("1") {
        return;
    }
    let env = TestEnv::new();
    let http = record_http(&env, &scenario("tukui"));
    check_recorded(http.as_ref());
}

#[test]
fn the_recorded_api_decodes() {
    let env = TestEnv::new();
    let http = replay(&env, &scenario("tukui"));
    check_recorded(http.as_ref());
}

#[test]
fn elvui_installs_on_every_client_its_tocs_declare() {
    let home = Home::new(&["wow", "wow_classic", "wow_classic_era", "wowf"]);
    serve_elvui(&home);

    let r = home.run(&["install", "tukui:elvui", "-t", "retail"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["items"][0];
    assert_eq!(
        (&item["status"], &item["name"], &item["ref"]),
        (
            &Value::from("installed"),
            &Value::from("ElvUI"),
            &Value::from("tukui:elvui")
        ),
        "{}",
        r.context()
    );
    assert_eq!(folders(&home, "_retail_"), ELVUI);
    let addon = &record(&home, "retail").addons["elvui"];
    assert_eq!(
        (addon.release_id.as_deref(), addon.version.as_str()),
        (Some("tukui:elvui@16.00"), "16.00")
    );
    assert_eq!(purposes(&home, 0), (1, 1, 1), "list, resolve, download");

    // Other clients take the blob the first download cached: no resolve, no download.
    for (target, client) in [
        ("classic", "_classic_"),
        ("era", "_classic_era_"),
        ("forever", "_forever_"),
    ] {
        let before = net_log(&home).len();
        let r = home.run(&["install", "tukui:elvui", "-t", target]);
        r.expect(0);
        assert_eq!(folders(&home, client), ELVUI, "{}", r.context());
        assert_eq!(purposes(&home, before), (1, 0, 0), "{target}: warm");
    }

    // A warm reinstall: unchanged, nothing resolved or downloaded.
    let before = net_log(&home).len();
    let r = home.run(&["install", "tukui:elvui", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["data"]["items"][0]["status"], "unchanged");
    assert_eq!(purposes(&home, before), (1, 0, 0));
}

#[test]
fn one_sync_lists_resolves_and_downloads_elvui_once_for_three_clients() {
    let home = Home::new(&["wow", "wow_classic", "wow_classic_era"]);
    serve_elvui(&home);
    home.write_desired(
        "[family.mainline]\naddons = [\"tukui:elvui\"]\n\n\
         [family.mists]\naddons = [\"tukui:elvui\"]\n\n\
         [family.vanilla]\naddons = [\"tukui:elvui\"]\n",
    );
    let r = home.run(&["sync"]);
    r.expect(0);
    for client in ["_retail_", "_classic_", "_classic_era_"] {
        assert_eq!(folders(&home, client), ELVUI, "{client}: {}", r.context());
    }
    assert_eq!(purposes(&home, 0), (1, 1, 1));
}

#[test]
fn a_bad_etag_fails_hash_mismatch_after_one_resolve_and_retry() {
    let home = Home::new(&["wow"]);
    let zip = elvui_zip("16.00");
    let wrong = etag(b"other bytes");
    tukui_list(&home.http, &[elvui()]);
    tukui_addon(&home.http, &elvui(), &["TOKEN-A", "TOKEN-B"]);
    tukui_download(&home.http, "elvui", "TOKEN-A", &zip, Some(&wrong));
    tukui_download(&home.http, "elvui", "TOKEN-B", &zip, Some(&wrong));
    home.run(&["status"]).expect(0);
    let addons = tree_hash(&home.addons("_retail_"));

    let r = home.run(&["install", "tukui:elvui", "-t", "retail"]);
    let envelope = r.expect(1);
    assert_eq!(
        envelope["error"]["code"],
        "hash_mismatch",
        "{}",
        r.context()
    );
    assert_eq!(purposes(&home, 0).1, 2, "resolved again before the retry");
    assert_eq!(purposes(&home, 0).2, 2);
    assert_eq!(
        tree_hash(&home.addons("_retail_")),
        addons,
        "nothing written"
    );
    let blobs = common::http::paths(&home.env).blobs_dir();
    let stored = std::fs::read_dir(&blobs).map_or(0, |d| {
        d.filter(|e| e.as_ref().unwrap().path().is_file()).count()
    });
    assert_eq!(stored, 0, "no blob stored");
}

#[test]
fn a_token_that_died_is_resolved_again_and_no_token_is_ever_kept() {
    let mut home = Home::new(&["wow"]);
    let zip = elvui_zip("16.00");

    // Both tokens dead: the install fails, and its error names neither.
    tukui_list(&home.http, &[elvui()]);
    tukui_addon(&home.http, &elvui(), &["SECRET-TOKEN-1", "SECRET-TOKEN-2"]);
    tukui_dead_token(&home.http, "elvui", "SECRET-TOKEN-1");
    tukui_dead_token(&home.http, "elvui", "SECRET-TOKEN-2");
    let failed = home.run(&["install", "tukui:elvui", "-t", "retail"]);
    assert_ne!(failed.code, 0, "{}", failed.context());
    assert!(!home.addons("_retail_").join("ElvUI").exists());

    // The first token died before its download: resolved again, then installed.
    let live = home.scenario("http-live");
    tukui_list(&live, &[elvui()]);
    tukui_addon(&live, &elvui(), &["SECRET-TOKEN-3", "SECRET-TOKEN-4"]);
    tukui_dead_token(&live, "elvui", "SECRET-TOKEN-3");
    tukui_download(&live, "elvui", "SECRET-TOKEN-4", &zip, Some(&etag(&zip)));
    let before = net_log(&home).len();
    let dry = home.run(&["install", "tukui:elvui", "-t", "retail", "--dry-run", "-v"]);
    let planned = dry.expect(0);
    assert_eq!(
        planned["data"]["items"][0]["asset_url"],
        "https://api.tukui.org/v1/addon/elvui",
        "the canonical URL, never the token: {}",
        dry.context()
    );
    let r = home.run(&["install", "tukui:elvui", "-t", "retail", "-v"]);
    let envelope = r.expect(0);
    assert_eq!(folders(&home, "_retail_"), ELVUI, "{}", r.context());
    assert_eq!(
        purposes(&home, before),
        (2, 2, 2),
        "the dry run: list, resolve, 403, resolve again, zip; the real run: list only"
    );
    // The real run took the blob the dry run cached, so it resolved nothing: no URL.
    assert_eq!(envelope["data"]["items"][0]["asset_url"], Value::Null);

    let mut seen = vec![
        ("dry-run stdout".to_owned(), dry.stdout.clone()),
        ("dry-run stderr".to_owned(), dry.stderr.clone()),
        ("stdout".to_owned(), r.stdout.clone()),
        ("stderr".to_owned(), r.stderr.clone()),
        ("failed stdout".to_owned(), failed.stdout.clone()),
        ("failed stderr".to_owned(), failed.stderr.clone()),
    ];
    for dir in [home.state(), home.env.cache(), home.env.config()] {
        everything_under(&dir, &mut seen);
    }
    assert!(
        seen.iter().any(|(path, _)| path.ends_with("net.log")),
        "net.log is searched too"
    );
    for (what, text) in &seen {
        assert!(!text.contains("SECRET-TOKEN"), "a token in {what}");
    }
}

#[test]
fn mixed_case_slugs_are_one_addon_and_unknown_slugs_are_not_found() {
    let home = Home::new(&["wow"]);
    serve_elvui(&home);
    home.run(&["install", "tukui:ElvUI", "-t", "retail"])
        .expect(0);
    let r = home.run(&["install", "tukui:elvui", "-t", "retail"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["items"][0];
    assert_eq!(
        (&item["status"], &item["ref"]),
        (&Value::from("unchanged"), &Value::from("tukui:elvui")),
        "{}",
        r.context()
    );
    assert_eq!(record(&home, "retail").addons.len(), 1);

    let r = home.run(&["install", "tukui:nosuch", "-t", "retail"]);
    let envelope = r.expect(3);
    assert_eq!(envelope["error"]["code"], "not_found", "{}", r.context());
    assert_eq!(envelope["error"]["message"], "tukui:nosuch is not on Tukui");
    assert_eq!(
        envelope["error"]["fix_argv"],
        json!(["omaddons", "search", "nosuch"])
    );
}

#[test]
fn the_memo_keeps_the_list_without_urls() {
    let home = Home::new(&["wow"]);
    serve_elvui(&home);
    home.run(&["install", "tukui:elvui", "-t", "retail"])
        .expect(0);
    let memo: Value = serde_json::from_slice(&std::fs::read(home.memo()).unwrap()).unwrap();
    let list = &memo["tukui_list"];
    assert!(list["fetched_at"].is_string(), "{memo:#}");
    let entries = list["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let keys: Vec<&str> = entries[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "author",
            "directories",
            "id",
            "last_update",
            "name",
            "patch",
            "slug",
            "version"
        ]
    );
    assert!(!list.to_string().contains("https://"), "{list}");
    assert_eq!(
        entries[0]["patch"],
        json!(["12.1.0", "5.5.4", "1.15.9", "2.5.6"])
    );
}

/// A download without a usable `ETag` MD5 (none, or a weak one) installs, and says it
/// was not checked: in JSON, to a human, in the release memo, and again when another
/// client reuses its cached blob without a request.
#[test]
fn a_download_without_a_usable_etag_installs_and_says_it_was_not_checked() {
    for sent in [None, Some("W/\"abc\"")] {
        let home = Home::new(&["wow", "wow_classic"]);
        let zip = elvui_zip("16.00");
        tukui_list(&home.http, &[elvui()]);
        tukui_addon(&home.http, &elvui(), &["TOKEN-A", "TOKEN-B"]);
        tukui_download(&home.http, "elvui", "TOKEN-A", &zip, sent);
        let unverified = |envelope: &Value| -> Vec<String> {
            envelope["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|w| w["code"] == "download_unverified")
                .map(|w| w["message"].as_str().unwrap().to_owned())
                .collect()
        };

        let r = home.run(&["install", "tukui:elvui", "-t", "retail"]);
        let envelope = r.expect(0);
        let warned = unverified(&envelope);
        assert_eq!(warned.len(), 1, "{sent:?}: {}", r.context());
        assert!(
            warned[0].starts_with("ElvUI 16.00: its download was not checked: https://api.tukui.org/v1/download/elvui/… sent"),
            "{}",
            warned[0]
        );
        assert!(!warned[0].contains("TOKEN"), "the token is redacted");
        assert_eq!(folders(&home, "_retail_"), ELVUI);
        let (memo, _) = omaddons::state::memo::load(&common::http::paths(&home.env));
        let fact = memo
            .candidates
            .iter()
            .find(|f| f.release_id == "tukui:elvui@16.00")
            .unwrap();
        assert!(fact.unverified, "{sent:?}");

        // Classic reuses the cached blob: no download, and still not checked.
        let before = net_log(&home).len();
        let human = home.exec(&["install", "tukui:elvui", "-t", "classic"].map(str::to_owned));
        assert_eq!(human.code, 0, "{}", human.context());
        assert!(
            human.stderr.contains("its download was not checked"),
            "{}",
            human.context()
        );
        assert_eq!(purposes(&home, before), (1, 0, 0), "{sent:?}: warm");
    }

    // A matching ETag: checked, nothing to say.
    let home = Home::new(&["wow"]);
    serve_elvui(&home);
    let envelope = home
        .run(&["install", "tukui:elvui", "-t", "retail"])
        .expect(0);
    assert!(
        !envelope.to_string().contains("download_unverified"),
        "{envelope}"
    );
}

/// The list names retail, but only the library loads there, not the addon's own folder:
/// a list only ranks, so the strict rule refuses it, as for the same file from disk.
#[test]
fn a_listed_client_never_waives_the_own_folder_rule() {
    let home = Home::new(&["wow"]);
    let zip = zip_text(&[
        ("Foo/Foo_Vanilla.toc", "## Interface: 11509\n"),
        ("Foo/Foo.lua", "--"),
        ("FooLib/FooLib.toc", "## Interface: 120105\n"),
        ("FooLib/lib.lua", "--"),
    ]);
    let foo = common::tukui::entry(7, "foo", "Foo", "1.0", &["12.1.5"], &["Foo", "FooLib"]);
    tukui_list(&home.http, std::slice::from_ref(&foo));
    tukui_addon(&home.http, &foo, &["TOKEN1"]);
    tukui_download(&home.http, "foo", "TOKEN1", &zip, Some(&etag(&zip)));
    let r = home.run(&["install", "tukui:foo", "-t", "retail", "--dry-run"]);
    let envelope = r.expect(3);
    assert_eq!(
        envelope["error"]["code"],
        "no_compatible_release",
        "{}",
        r.context()
    );
    let tried = envelope["error"]["details"]["tried"].to_string();
    assert!(
        tried.contains("foo-1.0.zip") && tried.contains("wrong_flavor"),
        "{tried}"
    );
    assert!(folders(&home, "_retail_").is_empty());
}
