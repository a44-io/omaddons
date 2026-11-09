//! `search` (alias `find`), end to end: the catalog, a WoWInterface index, a Tukui list
//! and GitHub's search from fixtures; ranking, merging, compatibility per client, the
//! install hints (which run), `--offline`, a spent GitHub search, `--source`, a disabled
//! source, `--limit`, progress and determinism. Only GitHub receives the query.

use crate::common;

use std::fs;

use common::archives::zip_text;
use common::home::Home;
use common::http::{Resp, fixture_seq, github_search, github_search_spent, replay};
use common::netlog::net_log;
use common::tukui::{LIST as TUKUI_LIST, elvui, tukui_list};
use common::wowi::{FileEntry, details, wowi_details, wowi_file, wowi_filelist};
use serde_json::{Value, json};
use support::{catalog_home, names, refs, results};

#[cfg(test)]
mod support {
    use std::fs;

    use super::common::addons::toc;
    use super::common::home::Home;
    use super::common::http::{Asset, gh_release};
    use super::common::trees::CustomRoot;
    use serde_json::Value;

    /// `data.results`.
    pub fn results(envelope: &Value) -> &Vec<Value> {
        envelope["data"]["results"].as_array().unwrap()
    }

    /// The results' refs, in order.
    pub fn refs(envelope: &Value) -> Vec<&str> {
        results(envelope)
            .iter()
            .map(|r| r["ref"].as_str().unwrap())
            .collect()
    }

    /// The results' names, in order.
    pub fn names(envelope: &Value) -> Vec<&str> {
        results(envelope)
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect()
    }

    /// Retail, Classic Era and a 3.3.5a server (`wotlk`); a `catalog.d` entry Foo
    /// (`gh:o/foo`, mainline) whose release v1 the fixtures serve (`search --catalog`,
    /// 1n2p.6).
    pub fn catalog_home() -> Home {
        let mut home = Home::new(&["wow", "wow_classic_era"]);
        home.with_custom(CustomRoot::at(home.env.root().join("wow-335")));
        let dir = home.env.config().join("omaddons/catalog.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("mine.toml"),
            "schema_version = \"1.0\"\n\n[[addon]]\nname = \"Foo\"\nref = \"gh:o/foo\"\nfamilies = [\"mainline\"]\nfolders = [\"Foo\"]\n",
        )
        .unwrap();
        let toc_text = toc("120105", &["## Title: Foo"]);
        let assets: [Asset<'_>; 1] = [(
            "Foo.zip",
            &[("mainline", 120_105)],
            vec![("Foo/Foo.toc", toc_text.as_str())],
        )];
        gh_release(&home.http, "https://github.com/o/foo", "v1", &assets);
        home
    }
}

/// Retail and Classic Era; a WoWInterface index with Details! (2020), Recount (recent,
/// declared for 12.0.5) and WeakAuras; Tukui's ElvUI; GitHub answering "damage meter"
/// with Skada and "weakauras" with WeakAuras2.
fn meters() -> Home {
    let home = Home::new(&["wow", "wow_classic_era"]);
    let mut details_meter = FileEntry::new(23056, "Details! Damage Meter", &["Details"]);
    details_meter.date_ms = 1_590_000_000_000;
    details_meter.downloads = 515_870;
    let mut recount = FileEntry::new(7000, "Recount Damage Meter", &["Recount"]);
    recount.downloads = 2_100_000;
    recount.compat = vec![("12.0.5".into(), "Midnight".into())];
    let mut wa = FileEntry::new(
        24910,
        "WeakAuras",
        &[
            "WeakAuras",
            "WeakAurasArchive",
            "WeakAurasModelPaths",
            "WeakAurasOptions",
            "WeakAurasTemplates",
        ],
    );
    wa.downloads = 9_000_000;
    wowi_filelist(&home.http, &[details_meter, recount, wa]);
    tukui_list(&home.http, &[elvui()]);
    github_search(
        &home.http,
        "damage meter",
        &[(
            "Skada-Damage-Meter",
            "Skada",
            "A modular damage meter for World of Warcraft",
            412,
            "2026-09-01T00:00:00Z",
        )],
    );
    github_search(
        &home.http,
        "weakauras",
        &[(
            "WeakAuras",
            "WeakAuras2",
            "A powerful framework to display World of Warcraft information",
            1300,
            "2026-10-01T00:00:00Z",
        )],
    );
    home
}

#[test]
fn the_catalog_entry_comes_first_with_its_duplicates_merged() {
    let home = meters();
    let r = home.run(&["search", "weakauras"]);
    let envelope = r.expect(0);
    let first = &results(&envelope)[0];
    assert_eq!(
        (&first["name"], &first["ref"], &first["source"]),
        (
            &json!("WeakAuras"),
            &json!("gh:WeakAuras/WeakAuras2"),
            &json!("catalog")
        ),
        "{}",
        r.context()
    );
    // The WoWInterface project (same name, its folders) and the GitHub repo (same ref)
    // merged into it: their popularity and date are its own now.
    assert_eq!(first["popularity"], json!({"downloads": 9_000_000}));
    assert_eq!(
        refs(&envelope),
        ["gh:WeakAuras/WeakAuras2"],
        "no wowi:24910, no second gh:weakauras/weakauras2"
    );
    // The catalog's families: Classic Era yes, retail no.
    assert_eq!(first["compat"], json!({"era": "yes", "retail": "no"}));
    assert_eq!(
        envelope["commands"][0]["argv"],
        json!([
            "omaddons",
            "install",
            "gh:WeakAuras/WeakAuras2",
            "-t",
            "era"
        ])
    );
    assert_eq!(envelope["partial"], false);
}

#[test]
fn damage_meter_ranks_by_tier_and_popularity_and_never_suggests_the_abandoned() {
    let home = meters();
    let r = home.run(&["search", "damage", "meter", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["data"]["query"], "damage meter");
    assert_eq!(
        refs(&envelope),
        ["wowi:7000", "wowi:23056", "gh:Skada-Damage-Meter/Skada"],
        "{}",
        r.context()
    );
    let compat: Vec<&Value> = results(&envelope).iter().map(|r| &r["compat"]).collect();
    assert_eq!(
        compat,
        [
            &json!({"retail": "likely"}),
            &json!({"retail": "abandoned"}),
            &json!({"retail": "unknown"})
        ]
    );
    let skada = &results(&envelope)[2];
    assert_eq!(
        (&skada["popularity"], &skada["summary"], &skada["author"]),
        (
            &json!({"stars": 412}),
            &json!("A modular damage meter for World of Warcraft"),
            &json!("Skada-Damage-Meter")
        )
    );
    let argv: Vec<&Value> = envelope["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["argv"])
        .collect();
    assert_eq!(
        argv,
        [
            &json!(["omaddons", "install", "wowi:7000", "-t", "retail"]),
            // Unknown on every client: info judges each, search guesses none (bmcx.6).
            &json!(["omaddons", "info", "gh:Skada-Damage-Meter/Skada"])
        ],
        "the abandoned Details! is never suggested"
    );

    // The first hint runs.
    let toc = "## Interface: 120105\n## Title: Recount\n## Version: 1.0\n";
    let zip = zip_text(&[("Recount/Recount.toc", toc)]);
    let md5 = wowi_file(&home.http, 7000, &zip);
    wowi_details(
        &home.http,
        &[7000],
        &[details(7000, "Recount", "1.0", &md5)],
    );
    let hint: Vec<&str> = envelope["commands"][0]["argv"].as_array().unwrap()[1..]
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let r = home.run(&hint);
    assert_eq!(
        r.expect(0)["data"]["items"][0]["status"],
        "installed",
        "{}",
        r.context()
    );
}

#[test]
fn only_github_receives_the_query_and_progress_is_reported() {
    let home = meters();
    let r = home.run(&["search", "damage", "meter", "--progress", "json"]);
    r.expect(0);
    let log = net_log(&home);
    let mut asked: Vec<(String, String)> = log
        .entries
        .iter()
        .map(|e| (e.host.clone(), e.purpose.clone()))
        .collect();
    asked.sort();
    assert_eq!(
        asked,
        [
            ("api.github.com".to_owned(), "github-search".to_owned()),
            ("api.mmoui.com".to_owned(), "wowi-filelist".to_owned()),
            ("api.tukui.org".to_owned(), "tukui-list".to_owned()),
        ],
        "{}",
        log.context()
    );
    assert!(
        log.entries
            .iter()
            .all(|e| !e.path.contains("damage") && !e.path.contains("meter")),
        "no path carries the query: {}",
        log.context()
    );
    let fetches = r
        .stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["event"] == "progress" && e["phase"] == "fetch")
        .count();
    assert_eq!(fetches, 3, "{}", r.context());
}

#[test]
fn offline_answers_from_the_catalog_and_the_stored_index_without_a_request() {
    let home = meters();
    home.run(&["search", "meter"]).expect(0);
    let before = net_log(&home).len();
    let r = home.run(&["search", "damage", "meter", "--offline"]);
    let envelope = r.expect(0);
    assert_eq!(net_log(&home).len(), before, "no request");
    assert_eq!(
        refs(&envelope),
        ["wowi:7000", "wowi:23056"],
        "{}",
        r.context()
    );
    assert_eq!(
        envelope["data"]["sources_skipped"],
        json!([{"source": "github", "reason": "offline"}])
    );
    // Offline and nothing stored: WoWInterface, like Tukui, was not asked.
    let fresh = Home::new(&["wow"]);
    let r = fresh.run(&["search", "weakauras", "--offline"]);
    let envelope = r.expect(0);
    assert_eq!(refs(&envelope), ["gh:WeakAuras/WeakAuras2"], "the catalog");
    let skipped: Vec<(&Value, &Value)> = envelope["data"]["sources_skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (&s["source"], &s["reason"]))
        .collect();
    assert_eq!(
        skipped,
        [
            (&json!("github"), &json!("offline")),
            (&json!("wowi"), &json!("offline")),
            (&json!("tukui"), &json!("offline"))
        ],
        "{}",
        r.context()
    );
    assert_eq!(envelope["partial"], false, "{}", r.context());
}

#[test]
fn a_spent_github_search_is_skipped_with_its_reset_and_the_rest_answers() {
    let home = meters();
    github_search_spent(&home.http, "recount", 1_760_652_120);
    let r = home.run(&["search", "recount"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], false, "{}", r.context());
    assert_eq!(refs(&envelope), ["wowi:7000"]);
    assert_eq!(
        envelope["data"]["sources_skipped"],
        json!([{"source": "github", "reason": "rate_limited", "reset_at": "2025-10-16T22:02:00Z"}])
    );
}

#[test]
fn the_search_budget_is_not_the_core_api_budget() {
    use omaddons::sources::github_rest::GithubRest;
    use omaddons::sources::github_search::GithubSearch;
    let home = meters();
    github_search_spent(&home.http, "recount", 1_760_652_120);
    let http = replay(&home.env, &home.http);
    let search = GithubSearch::with_token(None);
    let rest = GithubRest::with_token(None);
    let err = search.search("recount", http.as_ref()).unwrap_err();
    assert_eq!(err.code, omaddons::error::ErrorCode::RateLimited);
    assert_eq!(search.budget().remaining, Some(0));
    assert!(rest.budget().allows(), "the core budget is untouched");
    // A spent search budget refuses the next search without a request.
    let before = net_log(&home).len();
    let again = search.search("recount", http.as_ref()).unwrap_err();
    assert_eq!(again.code, omaddons::error::ErrorCode::RateLimited);
    assert_eq!(net_log(&home).len(), before);
}

#[test]
fn find_is_search_and_sources_filter_and_report_as_asked() {
    let home = meters();
    let a = home.run(&["search", "weakauras"]).expect(0);
    let b = home.run(&["find", "weakauras"]).expect(0);
    assert_eq!(a["data"], b["data"]);

    let before = net_log(&home).len();
    let r = home.run(&["search", "meter", "--source", "wowi"]);
    let envelope = r.expect(0);
    let sources: Vec<&Value> = results(&envelope).iter().map(|r| &r["source"]).collect();
    assert!(!sources.is_empty(), "{}", r.context());
    assert!(
        sources.iter().all(|s| matches!(s.as_str(), Some("wowi"))),
        "{sources:?}"
    );
    assert_eq!(envelope["data"]["sources_skipped"], json!([]));
    let new = &net_log(&home).entries[before..];
    let purposes: Vec<&str> = new.iter().map(|e| e.purpose.as_str()).collect();
    assert!(!purposes.contains(&"github-search"), "{purposes:?}");

    home.run(&["config", "set", "sources.github.enabled", "false"])
        .expect(0);
    let r = home.run(&["search", "meter"]);
    let skipped = &r.expect(0)["data"]["sources_skipped"];
    assert_eq!(
        skipped,
        &json!([{"source": "github", "reason": "disabled"}]),
        "{}",
        r.context()
    );
}

#[test]
fn limit_is_bounded_and_the_output_is_deterministic() {
    let home = meters();
    for (limit, bound) in [("0", "1"), ("51", "50")] {
        let r = home.run(&["search", "meter", "--limit", limit]);
        let envelope = r.expect(2);
        assert_eq!(
            envelope["error"]["fix_argv"],
            json!(["omaddons", "search", "meter", "--limit", bound]),
            "{}",
            r.context()
        );
    }
    let one = home.run(&["search", "meter", "--limit", "1"]).expect(0);
    assert_eq!(results(&one).len(), 1);
    let a = home.run(&["search", "damage", "meter"]).expect(0);
    let b = home.run(&["search", "damage", "meter"]).expect(0);
    assert_eq!(a["data"], b["data"]);
    assert_eq!(a["commands"], b["commands"]);
}

/// WoWInterface's names are HTML-escaped and untrusted: shown decoded, and nothing a
/// terminal would obey (an escape, raw or as an entity) ever reaches it. An index an
/// older build stored as sent is decoded once on load.
#[test]
fn source_names_are_decoded_and_never_reach_the_terminal_raw() {
    let home = Home::new(&["wow"]);
    let mut korean = FileEntry::new(1, "DBM &#54392;&#52852; (DBM-VPPukaKR)", &["DBM-VPPukaKR"]);
    korean.author = "Some&amp;One".into();
    let quoted = FileEntry::new(2, "DBM Voicepack &quot;Yike&quot; &#x41;", &["DBM-VPYike"]);
    let entity_escape = FileEntry::new(3, "DBM evil &#27;[31mRED", &["DBM-Evil"]);
    let raw_escape = FileEntry::new(4, "DBM raw \u{1b}[31mRED\u{1b}[0m\u{7}", &["DBM-Raw"]);
    wowi_filelist(&home.http, &[korean, quoted, entity_escape, raw_escape]);
    let names = |envelope: &Value| -> Vec<String> {
        let mut names: Vec<String> = results(envelope)
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_owned())
            .collect();
        names.sort();
        names
    };
    let expected = [
        "DBM Voicepack \"Yike\" A",
        "DBM evil [31mRED",
        "DBM raw [31mRED[0m",
        "DBM \u{d478}\u{ce74} (DBM-VPPukaKR)",
    ];

    let r = home.run(&["search", "dbm", "--source", "wowi"]);
    let envelope = r.expect(0);
    assert_eq!(names(&envelope), expected, "{}", r.context());
    let first_author = |envelope: &Value| {
        results(envelope)
            .iter()
            .find(|r| matches!(r["ref"].as_str(), Some("wowi:1")))
            .map(|r| r["author"].clone())
    };
    assert_eq!(first_author(&envelope), Some(json!("Some&One")));
    let human = home.exec(&["search", "dbm", "--source", "wowi"].map(str::to_owned));
    assert_eq!(human.code, 0, "{}", human.context());
    assert!(
        !human.stdout.chars().any(|c| c.is_control() && c != '\n'),
        "{:?}",
        human.stdout
    );

    // An index written by an older build (1.0) holds the API's text as sent.
    let index = common::http::paths(&home.env).wowi_index();
    let mut stored: Value = serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
    stored["schema_version"] = json!("1.0");
    for entry in stored["entries"].as_array_mut().unwrap() {
        if entry["uid"] == 1 {
            entry["name"] = json!("DBM &#54392;&#52852; (DBM-VPPukaKR)");
            entry["author"] = json!("Some&amp;amp;One");
        }
        if entry["uid"] == 4 {
            entry["name"] = json!("DBM raw \u{1b}[31mRED\u{7}");
        }
    }
    std::fs::write(&index, serde_json::to_vec(&stored).unwrap()).unwrap();
    let r = home.run(&["search", "dbm", "--source", "wowi", "--offline"]);
    let envelope = r.expect(0);
    let migrated = names(&envelope);
    assert!(
        migrated.contains(&"DBM \u{d478}\u{ce74} (DBM-VPPukaKR)".to_owned())
            && migrated.contains(&"DBM raw [31mRED".to_owned()),
        "{migrated:?}"
    );
    assert_eq!(
        first_author(&envelope),
        Some(json!("Some&amp;One")),
        "decoded exactly once"
    );
}

/// `search dbm`: GitHub's database tools never show; WoWInterface's Deadly Boss Mods
/// merges into the catalog's DBM, which then shows its downloads and comes first; the
/// voice packs follow.
#[test]
fn dbm_finds_deadly_boss_mods_first_and_no_database_tools() {
    let home = Home::new(&["wow"]);
    let mut dbm = FileEntry::new(
        8814,
        "Deadly Boss Mods",
        &["DBM-Core", "DBM-GUI", "DBM-StatusBarTimers"],
    );
    dbm.downloads = 3_000_000;
    let mut yike = FileEntry::new(22000, "DBM Voicepack Yike", &["DBM-VPYike"]);
    yike.downloads = 20_000;
    let mut kyle = FileEntry::new(22001, "DBM Voicepack Kyle", &["DBM-VPKyleW"]);
    kyle.downloads = 10_000;
    wowi_filelist(&home.http, &[dbm, yike, kyle]);
    let at = "2026-09-01T00:00:00Z";
    github_search(
        &home.http,
        "dbm",
        &[
            (
                "amacneil",
                "dbmate",
                "A lightweight, framework-agnostic database migration tool.",
                6000,
                at,
            ),
            ("holistics", "dbml", "Database Markup Language", 3000, at),
            (
                "DeadlyBossMods",
                "DeadlyBossMods",
                "Raid encounter alerts for World of Warcraft",
                400,
                at,
            ),
            ("someone", "DBM-VPKyle", "A voice pack addon for DBM", 5, at),
        ],
    );
    let argv: Vec<String> = [
        "search", "dbm", "--source", "catalog", "--source", "wowi", "--source", "github", "-t",
        "retail", "-v", "--json",
    ]
    .iter()
    .map(|a| (*a).to_owned())
    .collect();
    let r = home.exec(&argv);
    let envelope = r.expect(0);
    let found = refs(&envelope);
    assert_eq!(
        found[0],
        "gh:DeadlyBossMods/DeadlyBossMods",
        "{}",
        r.context()
    );
    assert_eq!(
        results(&envelope)[0]["popularity"],
        json!({"downloads": 3_000_000}),
        "the WoWInterface project merged into it"
    );
    assert!(
        !found
            .iter()
            .any(|f| f.contains("dbmate") || f.contains("dbml")),
        "{found:?}"
    );
    assert!(!found.contains(&"wowi:8814"), "{found:?}");
    for pack in ["wowi:22000", "wowi:22001", "gh:someone/DBM-VPKyle"] {
        assert!(found.contains(&pack), "{pack}: {found:?}");
    }
    assert!(
        r.stderr
            .contains("GitHub: 2 repositories without a WoW marker dropped"),
        "{}",
        r.context()
    );
}

/// The `code: message` of each warning.
fn warning_texts(envelope: &Value) -> Vec<String> {
    envelope["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|w| {
            format!(
                "{}: {}",
                w["code"].as_str().unwrap_or_default(),
                w["message"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

/// A WoWInterface index that cannot be fetched: with nothing stored, the run is partial
/// and says why. With a stored index past its window, its results stand, the run is not
/// partial, and `stale_check` names the failed refresh.
#[test]
fn a_failed_wowi_fetch_is_partial_unless_a_stored_index_answers() {
    let home = Home::new(&["wow"]);
    let r = home.run(&["search", "bugsack", "--source", "wowi"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    assert_eq!(
        envelope["data"]["sources_skipped"],
        json!([{"source": "wowi", "reason": "no_index"}])
    );
    let texts = warning_texts(&envelope);
    assert!(
        texts.iter().any(|t| t.contains("the WoWInterface index: ")),
        "{texts:?}"
    );

    wowi_filelist(&home.http, &[FileEntry::new(1, "BugSack", &["BugSack"])]);
    home.run(&["search", "bugsack", "--source", "wowi"])
        .expect(0);
    // Past its window, and the file list is gone.
    let index = home.env.cache().join("omaddons/wowi/index.json");
    let mut stored: Value = serde_json::from_str(&fs::read_to_string(&index).unwrap()).unwrap();
    stored["fetched_at"] = json!("2026-01-01T00:00:00Z");
    fs::write(&index, stored.to_string()).unwrap();
    fs::remove_file(home.http.join("api.mmoui.com/wowi-filelist.json")).unwrap();
    let r = home.run(&["search", "bugsack", "--source", "wowi"]);
    let envelope = r.expect(0);
    assert_eq!(
        (&envelope["partial"], refs(&envelope)),
        (&json!(false), vec!["wowi:1"]),
        "{}",
        r.context()
    );
    let texts = warning_texts(&envelope);
    assert!(
        texts.iter().any(|t| t.starts_with(
            "stale_check: the WoWInterface index is from 2026-01-01T00:00:00Z; refreshing it failed: "
        )),
        "{texts:?}"
    );
}

/// A rate-limited Tukui list is a failure, unlike a spent GitHub search: the run is
/// partial, and the list is skipped `rate_limited` with when the limit lifts.
#[test]
fn a_rate_limited_tukui_list_makes_the_run_partial() {
    let home = Home::new(&["wow"]);
    let limited = Resp::status(429).header("retry-after", "3600");
    fixture_seq(&home.http, "tukui-list", TUKUI_LIST, &[limited]);
    let r = home.run(&["search", "elvui", "--source", "tukui"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    let skipped = &envelope["data"]["sources_skipped"][0];
    assert_eq!(
        (&skipped["source"], &skipped["reason"]),
        (&json!("tukui"), &json!("rate_limited")),
        "{}",
        r.context()
    );
    assert!(skipped["reset_at"].is_string(), "{}", r.context());
    let texts = warning_texts(&envelope);
    assert!(
        texts.iter().any(|t| t.contains("the Tukui list: ")),
        "{texts:?}"
    );
}

/// No client is guessed from no evidence (bmcx.6). A GitHub hit nothing judges is
/// `unknown` everywhere and gets an `info` hint, which judges each client; once the
/// release memo remembers its newest release, that release judges each client at no
/// request, and the install hint goes to the preferred `likely` client.
#[test]
fn a_github_hit_gets_info_until_the_memo_judges_it() {
    let home = Home::new(&["wow", "wowf", "wow_classic_era"]);
    github_search(
        &home.http,
        "ellesmere",
        &[(
            "EllesmereGaming",
            "EllesmereUI",
            "A user interface for World of Warcraft",
            50,
            "2026-10-01T00:00:00Z",
        )],
    );
    let search = || home.run(&["search", "ellesmere", "--source", "github"]);

    // No memo: unknown everywhere, and info rather than a guessed client.
    let r = search();
    let envelope = r.expect(0);
    let first = &results(&envelope)[0];
    assert_eq!(
        first["compat"],
        json!({ "era": "unknown", "forever": "unknown", "retail": "unknown" }),
        "{}",
        r.context()
    );
    assert_eq!(
        envelope["commands"],
        json!([{
            "action": "info",
            "command": "omaddons info gh:EllesmereGaming/EllesmereUI",
            "argv": ["omaddons", "info", "gh:EllesmereGaming/EllesmereUI"],
            "destructive": false
        }]),
        "{}",
        r.context()
    );
    let human = home.exec(&["search", "ellesmere", "--source", "github"].map(str::to_owned));
    assert_eq!(human.code, 0, "{}", human.context());
    assert!(
        human
            .stdout
            .contains("Details: omaddons info gh:EllesmereGaming/EllesmereUI"),
        "{}",
        human.context()
    );

    // A remembered newest release declaring mainline and forever judges each client.
    home.seed_memo(&common::memo::of(vec![common::memo::entry_with_flavors(
        "gh:EllesmereGaming/EllesmereUI",
        "v1.0.0",
        common::memo::ago(60 * 24 * 3),
        &[("mainline", 120_105), ("forever", 16_001)],
    )]));
    let before = net_log(&home).len();
    let r = search();
    let envelope = r.expect(0);
    assert_eq!(
        results(&envelope)[0]["compat"],
        json!({ "era": "unlikely", "forever": "likely", "retail": "likely" }),
        "{}",
        r.context()
    );
    let log = net_log(&home);
    let purposes: Vec<&str> = log.entries[before..]
        .iter()
        .map(|e| e.purpose.as_str())
        .collect();
    assert_eq!(
        purposes,
        ["github-search"],
        "only the search request:\n{}",
        log.context()
    );
    assert_eq!(
        envelope["commands"][0]["argv"],
        json!([
            "omaddons",
            "install",
            "gh:EllesmereGaming/EllesmereUI",
            "-t",
            "retail"
        ]),
        "the preferred of retail and forever: {}",
        r.context()
    );

    // A remembered release with no flavor metadata says nothing either way.
    home.seed_memo(&common::memo::of(vec![common::memo::entry_with_flavors(
        "gh:EllesmereGaming/EllesmereUI",
        "v1.0.0",
        common::memo::ago(60 * 24 * 3),
        &[],
    )]));
    let r = search();
    let envelope = r.expect(0);
    assert_eq!(
        results(&envelope)[0]["compat"],
        json!({ "era": "unknown", "forever": "unknown", "retail": "unknown" }),
        "{}",
        r.context()
    );
    assert_eq!(envelope["commands"][0]["action"], "info", "{}", r.context());
}

/// An entry the catalog says is `yes` on retail and another client: the hint goes to
/// the default target, else the client with the most addons, else retail, never just
/// the first id (`classic` sorts before `retail`, `titan` after).
#[test]
fn an_install_hint_prefers_the_default_then_the_played_client() {
    let home_with = |other: &str, family: &str| {
        let home = Home::new(&["wow", other]);
        let dir = home.env.config().join("omaddons/catalog.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("50-both.toml"),
            format!("schema_version = \"1.0\"\n\n[[addon]]\nname = \"Both\"\nref = \"gh:o/both\"\nfamilies = [\"mainline\", \"{family}\"]\nfolders = [\"Both\"]\n"),
        )
        .unwrap();
        home
    };
    let hinted = |home: &Home| -> Value {
        let r = home.run(&["search", "both", "--offline"]);
        let envelope = r.expect(0);
        envelope["commands"][0]["argv"][4].clone()
    };
    let classic = home_with("wow_classic", "mists");
    assert_eq!(
        hinted(&classic),
        json!("retail"),
        "no default, no addons: retail"
    );

    let titan = home_with("wow_classic_titan", "titan");
    assert_eq!(hinted(&titan), json!("retail"));
    titan.run(&["targets", "default", "titan"]).expect(0);
    assert_eq!(hinted(&titan), json!("titan"), "the default target");

    let titan = home_with("wow_classic_titan", "titan");
    let addon = titan.env.root().join("addons/Mine");
    fs::create_dir_all(&addon).unwrap();
    fs::write(addon.join("Mine.toc"), "## Interface: 38002\n").unwrap();
    let addon = addon.to_string_lossy().into_owned();
    titan.run(&["install", &addon, "-t", "titan"]).expect(0);
    assert_eq!(hinted(&titan), json!("titan"), "the client with addons");
}

// ---------------------------------------------------------------- --catalog (1n2p.6)

#[test]
fn catalog_lists_every_entry_that_fits_a_client_by_name_and_asks_no_source() {
    let home = catalog_home();
    let r = home.run(&["search", "--catalog"]);
    let envelope = r.expect(0);
    let listed = names(&envelope);
    let mut sorted = listed.clone();
    sorted.sort_by_key(|n| n.to_lowercase());
    assert_eq!(listed, sorted, "by name: {}", r.context());
    // Retail's, Classic Era's and 3.3.5a's builds are listed, and no cap applies; the
    // 1.12.1, 2.4.3, 4.3.4 and 5.4.8 servers' addons fit no client here.
    for kept in [
        "Foo",
        "Questie",
        "GroupBulletinBoard",
        "BugGrabber",
        "WeakAuras",
    ] {
        assert!(listed.contains(&kept), "{kept}: {listed:?}");
    }
    for gone in ["pfUI", "PlateBuffer", "MikScrollingBattleText", "InFlight"] {
        assert!(!listed.contains(&gone), "{gone}: {listed:?}");
    }
    assert!(listed.len() > 50, "no default cap: {}", listed.len());
    for row in results(&envelope) {
        let hints = row["compat"].as_object().unwrap();
        assert_eq!(
            hints.keys().collect::<Vec<_>>(),
            ["era", "retail", "wotlk"],
            "{row}"
        );
        assert!(hints.values().any(|c| c != "no"), "{row}");
        assert_eq!(row["source"], json!("catalog"), "{row}");
    }
    assert_eq!(
        (
            &envelope["data"]["query"],
            &envelope["data"]["sources_skipped"]
        ),
        (&json!(""), &json!([]))
    );
    assert_eq!(net_log(&home).len(), 0, "no request");
    // `--offline` answers the same.
    let offline = home.run(&["search", "--catalog", "--offline"]).expect(0);
    assert_eq!(offline["data"], envelope["data"]);
    // `--limit` caps it, from the top.
    let five = home.run(&["search", "--catalog", "--limit", "5"]).expect(0);
    assert_eq!(names(&five), listed[..5].to_vec());
}

#[test]
fn catalog_words_filter_the_catalog_alone_with_the_hints_plain_search_gives() {
    let home = catalog_home();
    let catalog = home.run(&["search", "--catalog", "dbm"]).expect(0);
    assert_eq!(names(&catalog), ["DBM", "DBM-Vanilla"]);
    assert_eq!(net_log(&home).len(), 0, "no request");
    // `search dbm` is unchanged, and its catalog hits carry the same hints.
    let plain = home.run(&["search", "dbm", "--offline"]).expect(0);
    for row in results(&catalog) {
        let same = results(&plain)
            .iter()
            .find(|p| p["ref"] == row["ref"])
            .unwrap_or_else(|| panic!("{} in plain search: {plain}", row["ref"]));
        assert_eq!(same["compat"], row["compat"], "{}", row["name"]);
    }
    // `--source catalog` goes with it; another source does not, and the fix runs.
    home.run(&["search", "--catalog", "--source", "catalog", "dbm"])
        .expect(0);
    let refused = home.run(&["search", "--catalog", "--source", "github", "dbm"]);
    let envelope = refused.expect(2);
    assert_eq!(envelope["error"]["code"], json!("usage"));
    let fix: Vec<String> = serde_json::from_value(envelope["error"]["fix_argv"].clone()).unwrap();
    assert_eq!(fix, ["omaddons", "search", "--catalog", "--", "dbm"]);
    // `--json` before the command: after `--` it would be a word of the query.
    let mut argv = vec!["--json".to_owned()];
    argv.extend(fix[1..].iter().cloned());
    let fixed = home.exec(&argv).expect(0);
    assert_eq!(names(&fixed), ["DBM", "DBM-Vanilla"]);
    // Without `--catalog`, words are still required: a usage error before anything is
    // read, whose fix is search's help.
    let before = net_log(&home).len();
    let bare = home.run(&["search"]);
    let envelope = bare.expect(2);
    assert_eq!(
        (&envelope["error"]["code"], &envelope["error"]["fix_argv"]),
        (&json!("usage"), &json!(["omaddons", "help", "search"])),
        "{envelope}"
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--catalog"),
        "{envelope}"
    );
    assert_eq!(net_log(&home).len(), before, "no request");
}

// Variants resolve per client as `install <name>` does (M3-D16): one client selected,
// a name's row is that client's build.
#[test]
fn catalog_names_each_clients_own_build_and_lists_what_fits_it() {
    let home = catalog_home();
    let wotlk = home
        .run(&["search", "--catalog", "-t", "wotlk", "buggrabber"])
        .expect(0);
    let row = &results(&wotlk)[0];
    assert_eq!(
        (&row["name"], &row["ref"], &row["compat"]),
        (
            &json!("BugGrabber"),
            &json!("wowace:bug-grabber#r152"),
            &json!({"wotlk": "yes"})
        )
    );
    let era = home
        .run(&["search", "--catalog", "-t", "era", "buggrabber"])
        .expect(0);
    assert_eq!(
        (&results(&era)[0]["ref"], &results(&era)[0]["compat"]),
        (&json!("wowace:bug-grabber"), &json!({"era": "yes"}))
    );
    // Plain search names the same build for one client.
    let plain = home
        .run(&["search", "-t", "wotlk", "buggrabber", "--offline"])
        .expect(0);
    assert_eq!(results(&plain)[0]["ref"], json!("wowace:bug-grabber#r152"));
    // What fits 3.3.5a: its two builds, nothing else.
    let listing = home.run(&["search", "--catalog", "-t", "wotlk"]).expect(0);
    assert_eq!(names(&listing), ["BugGrabber", "GroupBulletinBoard"]);
}

// A row's ref, on a client whose hint is `yes`, installs: its preview plans it cleanly.
#[test]
fn a_catalog_rows_ref_previews_cleanly_where_its_hint_is_yes() {
    let home = catalog_home();
    let listing = home
        .run(&["search", "--catalog", "-t", "retail", "foo"])
        .expect(0);
    let foo = results(&listing)
        .iter()
        .find(|r| r["name"] == "Foo")
        .unwrap();
    assert_eq!(foo["compat"], json!({"retail": "yes"}));
    let r = foo["ref"].as_str().unwrap();
    let preview = home.run(&["install", r, "-t", "retail", "--dry-run"]);
    let envelope = preview.expect(0);
    let item = &envelope["data"]["items"][0];
    assert_eq!(
        (&item["name"], &item["status"], &item["to_version"]),
        (&json!("Foo"), &json!("installed"), &json!("v1")),
        "{}",
        preview.context()
    );
}
