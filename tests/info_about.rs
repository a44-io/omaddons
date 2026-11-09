//! e2e suite: `info`'s `about` and `installed[].toc` (omad-widget-user-first-1n2p.3).
//!
//! What an addon is, from each source with project pages (GitHub's repository, a
//! WoWInterface file's details, the Tukui list, WowAce's page alone), asked at most
//! once a day and kept in the about memo; offline, disabled, unknown, rate-limited and
//! failing sources said in `about_unavailable`, never as a warning or a partial run;
//! and the TOC an installed addon's own folder loads, made safe to show.

use crate::common;

use std::fs;

use common::addons::{addon_folder, toc};
use common::home::Home;
use common::http::{Asset, RepoMeta, Resp, gh_release, gh_repo, gh_repo_responses};
use common::netlog::net_log;
use common::tlog::{TestLog, tlog};
use serde_json::{Value, json};

const WA: &str = "https://github.com/WeakAuras/WeakAuras2";

/// WeakAuras 5.19.2 on retail, and what GitHub says of its repository: a description
/// with a bidirectional override and a control character in it.
fn weakauras(home: &Home) {
    let toc = "## Interface: 120105\n## Version: 5.19.2\n";
    let assets: [Asset<'_>; 1] = [(
        "WeakAuras-5.19.2.zip",
        &[("mainline", 120_105)],
        vec![("WeakAuras/WeakAuras.toc", toc)],
    )];
    gh_release(&home.http, WA, "5.19.2", &assets);
    gh_repo(
        &home.http,
        &RepoMeta {
            owner: "WeakAuras",
            name: "WeakAuras2",
            description: Some("World of Warcraft addon\u{202e} that \u{7}displays auras"),
            stars: 1_234,
            pushed_at: "2026-10-28T09:30:00Z",
        },
    );
}

/// `omaddons <args> --json`, logged; it must exit 0. Its envelope.
fn info(log: &TestLog, home: &Home, args: &[&str]) -> Value {
    let mut argv = args.to_vec();
    argv.push("--json");
    log.run(home, &argv).ok().envelope().clone()
}

fn about(envelope: &Value) -> (Value, Value) {
    (
        envelope["data"]["about"].clone(),
        envelope["data"]["about_unavailable"].clone(),
    )
}

/// `about` with only the page the ref gives.
fn page_only(homepage: &str) -> Value {
    json!({
        "summary": null,
        "author": null,
        "homepage": homepage,
        "updated_at": null,
        "popularity": null,
    })
}

/// GitHub: the repository's description (made safe), owner, stars and last push, with
/// its page; asked once, then answered from the memo for a day, offline too;
/// `--refresh` asks again.
#[test]
fn github_says_what_the_repository_is_once_a_day() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    weakauras(&home);

    log.step("the first info asks GitHub's API once");
    let envelope = info(&log, &home, &["info", "weakauras"]);
    let want = json!({
        "summary": "World of Warcraft addon that displays auras",
        "author": "WeakAuras",
        "homepage": "https://github.com/WeakAuras/WeakAuras2",
        "updated_at": "2026-10-28T09:30:00Z",
        "popularity": {"stars": 1234},
    });
    log.assert_eq("about", &about(&envelope), &(want.clone(), Value::Null));
    log.assert_eq("no warning", &envelope["warnings"], &json!([]));
    log.assert_eq(
        "one repository request",
        &net_log(&home).count("api.github.com", "rest-repo"),
        &1,
    );

    log.step("a second info answers from the memo");
    let again = info(&log, &home, &["info", "weakauras"]);
    log.assert_eq("about", &about(&again), &(want.clone(), Value::Null));
    log.assert_eq(
        "no second request",
        &net_log(&home).count_purpose("rest-repo"),
        &1,
    );

    log.step("offline, the memo answers");
    let offline = info(&log, &home, &["info", "weakauras", "--offline"]);
    log.assert_eq("about", &about(&offline), &(want, Value::Null));

    log.step("--refresh asks again");
    info(&log, &home, &["info", "weakauras", "--refresh"]);
    log.assert_eq(
        "asked again",
        &net_log(&home).count_purpose("rest-repo"),
        &2,
    );
}

/// Offline with nothing remembered: the page alone, and why the rest is missing; no
/// request. The human view says so.
#[test]
fn offline_with_nothing_remembered_gives_the_page_alone() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    weakauras(&home);
    let envelope = info(&log, &home, &["info", "weakauras", "--offline"]);
    log.assert_eq(
        "the page, offline",
        &about(&envelope),
        &(
            page_only("https://github.com/WeakAuras/WeakAuras2"),
            json!({"reason": "offline"}),
        ),
    );
    log.assert_eq("no request", &net_log(&home).len(), &0);
    let human = log.run(&home, &["info", "weakauras", "--offline"]);
    let stdout = &human.ok().run.stdout;
    log.assert_eq(
        "the human view names the page and why",
        &(
            stdout.contains("https://github.com/WeakAuras/WeakAuras2"),
            stdout.contains("about: not checked (offline)"),
        ),
        &(true, true),
    );
}

/// A repository GitHub does not know is remembered as unknown (no second request); a
/// spent budget and a failed request are said, never as a warning or a partial run,
/// and the release still answers.
#[test]
fn unknown_spent_and_failing_sources_say_why() {
    let log = tlog!();
    let gone = Home::new(&["wow"]);
    weakauras(&gone);
    gh_repo_responses(&gone.http, "WeakAuras", "WeakAuras2", &[Resp::status(404)]);

    log.step("a 404 is not_found, the page still given");
    let envelope = info(&log, &gone, &["info", "weakauras"]);
    log.assert_eq(
        "not found",
        &about(&envelope),
        &(
            page_only("https://github.com/WeakAuras/WeakAuras2"),
            json!({"reason": "not_found"}),
        ),
    );
    log.assert_eq(
        "neither partial nor warned",
        &(&envelope["partial"], &envelope["warnings"]),
        &(&Value::Bool(false), &json!([])),
    );
    info(&log, &gone, &["info", "weakauras"]);
    log.assert_eq("remembered", &net_log(&gone).count_purpose("rest-repo"), &1);

    log.step("a spent budget is rate_limited");
    let spent = Home::new(&["wow"]);
    weakauras(&spent);
    let reset = (jiff::Timestamp::now().as_second() + 3_600).to_string();
    let limited = Resp::status(403)
        .header("x-ratelimit-remaining", "0")
        .header("x-ratelimit-reset", &reset);
    gh_repo_responses(&spent.http, "WeakAuras", "WeakAuras2", &[limited]);
    let envelope = info(&log, &spent, &["info", "weakauras"]);
    log.assert_eq(
        "rate limited",
        &(about(&envelope).1, &envelope["partial"]),
        &(json!({"reason": "rate_limited"}), &Value::Bool(false)),
    );

    log.step("a server error is fetch_failed, and the release still answers");
    let down = Home::new(&["wow"]);
    weakauras(&down);
    gh_repo_responses(&down.http, "WeakAuras", "WeakAuras2", &[Resp::status(500)]);
    let envelope = info(&log, &down, &["info", "weakauras"]);
    log.assert_eq(
        "fetch failed",
        &(about(&envelope).1, &envelope["data"]["latest"]["version"]),
        &(json!({"reason": "fetch_failed"}), &Value::from("5.19.2")),
    );
}

/// WoWInterface: the author, downloads and date its file details give, and its page;
/// its BBCode description makes no summary.
#[test]
fn wowinterface_gives_author_downloads_and_date() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let details = common::wowi::details(7032, "TomTom", "v4.0.3", "0123");
    common::wowi::wowi_details(&home.http, &[7032], &[details]);
    let envelope = info(&log, &home, &["info", "wowi:7032"]);
    let want = json!({
        "summary": null,
        "author": "someone",
        "homepage": "https://www.wowinterface.com/downloads/info7032",
        "updated_at": "2025-10-12T20:13:20Z",
        "popularity": {"downloads": 1000},
    });
    log.assert_eq("about", &about(&envelope), &(want, Value::Null));
}

/// Tukui: its list's one-line description, author and last update, from the list the
/// release fetch decoded: one request serves both.
#[test]
fn tukui_answers_from_the_list_the_release_fetch_read() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    common::tukui::tukui_list(&home.http, &[common::tukui::elvui()]);
    let envelope = info(&log, &home, &["info", "elvui"]);
    let want = json!({
        "summary": "ElvUI does things.",
        "author": "someone",
        "homepage": "https://tukui.org/elvui",
        "updated_at": "2026-10-13T00:00:00Z",
        "popularity": null,
    });
    log.assert_eq("about", &about(&envelope), &(want, Value::Null));
    let sent = net_log(&home);
    log.assert_eq(
        "one list request, nothing else",
        &(sent.count("api.tukui.org", "tukui-list"), sent.len()),
        &(1, 1),
    );
}

/// WowAce describes no project: its page, `not_supported`, and no request for it.
#[test]
fn wowace_gives_its_page_alone() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let envelope = info(&log, &home, &["info", "wowace:bug-grabber", "--offline"]);
    log.assert_eq(
        "about",
        &about(&envelope),
        &(
            page_only("https://www.wowace.com/projects/bug-grabber"),
            json!({"reason": "not_supported"}),
        ),
    );
}

/// A source turned off is not asked, and says so.
#[test]
fn a_disabled_source_is_not_asked() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    weakauras(&home);
    let config = home.env.config().join("omaddons/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "[sources.github]\nenabled = false\n").unwrap();
    let envelope = info(&log, &home, &["info", "weakauras"]);
    log.assert_eq(
        "disabled, not asked",
        &(
            about(&envelope).1,
            net_log(&home).count_purpose("rest-repo"),
        ),
        &(json!({"reason": "source_disabled"}), 0),
    );
}

/// An installed addon's own folder (named like it, not its first): its TOC as the
/// client reads it, colour codes, textures, controls and hidden characters removed, the
/// website kept as text only. A local source names no project page, and a folder
/// deleted by hand has no TOC.
#[test]
fn an_installed_addon_shows_its_own_folders_toc_made_safe() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let src = home.env.root().join("src");
    let text = toc(
        "120105, 50504",
        &[
            "## Title: |cff33ff99Hello|r World",
            "## Notes: Says\u{202e} hello\u{7} |TInterface\\Icons\\x:16|t loudly",
            "## Author: |cffff0000me|r",
            "## Version: 1.2.3",
            "## X-Website: https://example.invalid/hello",
            "## Dependencies: HelloLib",
            "## OptionalDeps: Ace3",
        ],
    );
    let lib = toc("120105", &["## Title: Hello's library"]);
    let lib_dir = addon_folder(&src, "HelloLib", &[("HelloLib.toc", &lib)], &[]);
    let hello = addon_folder(&src, "Hello", &[("Hello.toc", &text)], &[]);

    log.step("install the library, then the addon");
    info(
        &log,
        &home,
        &["install", lib_dir.to_str().unwrap(), "-t", "retail"],
    );
    info(
        &log,
        &home,
        &["install", hello.to_str().unwrap(), "-t", "retail"],
    );

    log.step("info reads the TOC of the folder named like the addon");
    let envelope = info(&log, &home, &["info", "Hello"]);
    log.assert_eq(
        "the TOC, made safe",
        &envelope["data"]["installed"][0]["toc"],
        &json!({
            "folder": "Hello",
            "title_plain": "Hello World",
            "notes": "Says hello loudly",
            "author": "me",
            "version": "1.2.3",
            "interfaces": [120105, 50504],
            "website": "https://example.invalid/hello",
            "dependencies": ["HelloLib"],
            "optional_deps": ["Ace3"],
        }),
    );
    log.assert_eq(
        "a local folder names no project page",
        &about(&envelope),
        &(Value::Null, json!({"reason": "source_has_no_metadata"})),
    );

    log.step("a folder deleted by hand has no TOC");
    fs::remove_dir_all(home.addons("_retail_").join("HelloLib")).unwrap();
    let envelope = info(&log, &home, &["info", "HelloLib"]);
    log.assert_eq(
        "no TOC",
        &envelope["data"]["installed"][0]["toc"],
        &Value::Null,
    );
}
