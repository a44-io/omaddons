//! `search` (alias `find`), end to end: the catalog, a WoWInterface index, a Tukui list
//! and GitHub's search from fixtures; ranking, merging, compatibility per client, the
//! install hints (which run), `--offline`, a spent GitHub search, `--source`, a disabled
//! source, `--limit`, progress and determinism. Only GitHub receives the query.

mod common;

use common::archives::zip_text;
use common::home::Home;
use common::http::{github_search, github_search_spent, replay};
use common::netlog::net_log;
use common::tukui::{elvui, tukui_list};
use common::wowi::{FileEntry, details, wowi_details, wowi_file, wowi_filelist};
use serde_json::{Value, json};
use support::{refs, results};

#[cfg(test)]
mod support {
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
            "A modular damage meter",
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
            "A powerful framework",
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
            &json!("A modular damage meter"),
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
            &json!([
                "omaddons",
                "install",
                "gh:Skada-Damage-Meter/Skada",
                "-t",
                "retail"
            ])
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
    // Offline and nothing stored: the index is not there.
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
            (&json!("wowi"), &json!("no_index")),
            (&json!("tukui"), &json!("offline"))
        ],
        "{}",
        r.context()
    );
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
