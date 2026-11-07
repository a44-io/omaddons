//! Details coverage rows: sources, refs and the network: what a source cannot find, the
//! releases and archives that do not load on a client, blocked hosts, rate limits and
//! offline refusals.

use std::fs;

use omaddons::error::ErrorCode;

use super::super::support::{Scene, check};
use super::Row;
use crate::common::archives::zip_text;
use crate::common::graph::{Folder, Node, release_graph};
use crate::common::home::Home;
use crate::common::http::{fixture, fixture_bytes, gh_feed};

pub const ROWS: &[Row] = &[
    NO_CLIENT_LOADS,
    WRONG_FLAVOR_ARCHIVE,
    MISSING_ASSET,
    GIT_REV_NOT_FOUND,
    BLOCKED_HOSTS,
    WOWACE_TAG,
    NO_VARIANT,
    OFFLINE_REINSTALL,
    RATE_LIMITED,
    AMBIGUOUS_NAME,
    MISSING_DEPENDENCY,
];

/// A catalog file under the home's `catalog.d`.
fn catalog(home: &Home, file: &str, text: &str) {
    let dir = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(file), text).unwrap();
}

/// A Classic Era archive in the home's `dl/`, which neither Scene client loads.
fn era_zip(s: &Scene) -> String {
    let path = s.home.env.root().join("dl/Era.zip");
    fs::write(&path, zip_text(&[("Era/Era.toc", "## Interface: 11509\n")])).unwrap();
    path.display().to_string()
}

/// A vanilla-only release, its feed naming no older one to look back to: on retail it
/// is refused with what was tried, and without `-t` naming every live client checked.
const NO_CLIENT_LOADS: Row = Row {
    id: "a_release_no_client_loads_names_what_was_tried_and_checked",
    keys: &[
        (ErrorCode::NoCompatibleRelease, "ref"),
        (ErrorCode::NoCompatibleRelease, "target"),
        (ErrorCode::NoCompatibleRelease, "tried"),
        (ErrorCode::NoCompatibleRelease, "tried[].asset"),
        (ErrorCode::NoCompatibleRelease, "tried[].code"),
        (ErrorCode::NoCompatibleRelease, "tried[].message"),
        (ErrorCode::NoCompatibleRelease, "checked"),
    ],
    run: || {
        let s = Scene::new();
        gh_feed(
            &s.home.http,
            "https://github.com/o/bar",
            &[("v1", "")],
            None,
        );
        vec![
            s.json(&["install", "gh:o/bar", "-t", "retail"], 3),
            s.json(&["install", "gh:o/bar"], 3),
        ]
    },
};

/// The user's own archive, built for Classic Era, given for retail.
const WRONG_FLAVOR_ARCHIVE: Row = Row {
    id: "an_archive_that_does_not_load_on_the_client_is_the_wrong_flavor",
    keys: &[
        (ErrorCode::WrongFlavor, "ref"),
        (ErrorCode::WrongFlavor, "target"),
        (ErrorCode::WrongFlavor, "tried"),
        (ErrorCode::WrongFlavor, "tried[].asset"),
        (ErrorCode::WrongFlavor, "tried[].code"),
        (ErrorCode::WrongFlavor, "tried[].message"),
    ],
    run: || {
        let s = Scene::new();
        let era = era_zip(&s);
        vec![s.json(&["install", "gh:o/era", "--file", &era, "-t", "retail"], 4)]
    },
};

/// A pasted release-asset link to an asset the release does not have.
const MISSING_ASSET: Row = Row {
    id: "an_asset_link_to_no_asset_of_its_release_names_the_assets",
    keys: &[
        (ErrorCode::NotFound, "asset"),
        (ErrorCode::NotFound, "assets"),
    ],
    run: || {
        let s = Scene::new();
        let link = "https://github.com/o/foo/releases/download/v1/Nope.zip";
        vec![s.json(&["install", link, "-t", "retail"], 3)]
    },
};

/// A `git:` rev whose feed and archive are both 404, then a plain URL to that archive.
const GIT_REV_NOT_FOUND: Row = Row {
    id: "a_git_rev_that_names_nothing_is_not_found_and_its_archive_link_a_404",
    keys: &[
        (ErrorCode::NotFound, "repository"),
        (ErrorCode::NotFound, "rev"),
        (ErrorCode::HttpStatus, "status"),
    ],
    run: || {
        let home = Home::new(&["wow"]);
        let feed = "https://github.com/o/r/commits/nope.atom";
        fixture(&home.http, "feed", feed, 404, &[], None);
        let zip = "https://codeload.github.com/o/r/zip/refs/tags/nope";
        fixture(&home.http, "zip", zip, 404, &[], Some("404: Not Found"));
        let url = format!("url:{zip}");
        vec![
            check(
                home.run(&["install", "git:https://github.com/o/r#nope", "-t", "retail"]),
                3,
            ),
            check(home.run(&["install", &url, "-t", "retail"]), 1),
        ]
    },
};

/// Hosts omaddons never reaches: an `http:` ref, a URL to a host it does not download
/// from, and a download redirected off the allowlist.
const BLOCKED_HOSTS: Row = Row {
    id: "a_host_omaddons_never_reaches_is_refused_by_ref_request_and_redirect",
    keys: &[
        (ErrorCode::RefInvalid, "host"),
        (ErrorCode::HostBlocked, "host"),
        (ErrorCode::HostBlocked, "hop"),
        (ErrorCode::HostBlocked, "url"),
    ],
    run: || {
        let home = Home::new(&["wow"]);
        let start = "https://cdn.wowinterface.com/downloads/x.zip";
        let away = [("location", "https://example.com/x.zip")];
        fixture(&home.http, "wowi-away", start, 302, &away, None);
        let redirected = format!("url:{start}");
        let run = |args: &[&str], code| check(home.run(args), code);
        vec![
            run(&["install", "http://example.com/a.zip", "-t", "retail"], 2),
            run(
                &["install", "url:https://example.com/a.zip", "-t", "retail"],
                4,
            ),
            run(&["install", &redirected, "-t", "retail", "--dry-run"], 4),
        ]
    },
};

/// A WowAce tag the recorded tag list does not hold.
const WOWACE_TAG: Row = Row {
    id: "a_wowace_tag_the_project_does_not_have_is_not_found",
    keys: &[
        (ErrorCode::NotFound, "ref"),
        (ErrorCode::NotFound, "version"),
    ],
    run: || {
        let home = Home::new(&["wow"]);
        home.serve_recorded("wowace-bug-grabber");
        vec![check(
            home.run(&["install", "wowace:bug-grabber#v99.9.9", "-t", "retail"]),
            3,
        )]
    },
};

/// A catalog name whose variants cover mainline and vanilla, asked for on Mists.
const NO_VARIANT: Row = Row {
    id: "a_catalog_name_with_no_variant_for_the_client_is_not_found",
    keys: &[
        (ErrorCode::NotFound, "name"),
        (ErrorCode::NotFound, "family"),
    ],
    run: || {
        let s = Scene::new();
        catalog(
            &s.home,
            "50-duo.toml",
            "schema_version = \"1.0\"\n\n\
             [[addon]]\nname = \"Duo\"\nvariant = \"modern\"\nref = \"gh:o/foo\"\n\
             families = [\"mainline\"]\nfolders = [\"Foo\"]\n\n\
             [[addon]]\nname = \"Duo\"\nvariant = \"era\"\nref = \"gh:o/bar\"\n\
             families = [\"vanilla\"]\nfolders = [\"Bar\"]\n",
        );
        vec![s.json(&["install", "duo", "-t", "classic"], 3)]
    },
};

/// An offline reinstall whose archive is gone from the cache.
const OFFLINE_REINSTALL: Row = Row {
    id: "an_offline_reinstall_without_its_cached_archive_is_offline",
    keys: &[(ErrorCode::Offline, "reason"), (ErrorCode::Offline, "ref")],
    run: || {
        let s = Scene::new();
        let installed = s.json(&["install", "weakauras", "-t", "retail"], 0);
        fs::remove_dir_all(s.home.env.cache().join("omaddons/blobs")).unwrap();
        let args = [
            "install",
            "weakauras",
            "-t",
            "retail",
            "--reinstall",
            "--offline",
        ];
        vec![installed, s.json(&args, 1)]
    },
};

/// GitHub's release list for `o/<name>`, one beta release, with `limits` as its
/// rate-limit headers, and its zip (`sources_github_rest::beta_listing`).
fn beta_listing(dir: &std::path::Path, name: &str, limits: &[(&str, &str)]) {
    let upper = name.to_uppercase();
    let zip_url = format!("https://github.com/o/{name}/releases/download/v1-beta/{upper}.zip");
    let zip = zip_text(&[(&format!("{upper}/{upper}.toc"), "## Interface: 120105\n")]);
    let listing = format!(
        r#"[{{"tag_name": "v1-beta", "prerelease": true, "published_at": "2026-10-01T00:00:00Z",
            "assets": [{{"name": "{upper}.zip", "browser_download_url": "{zip_url}", "size": {}}}]}}]"#,
        zip.len()
    );
    let api = format!("https://api.github.com/repos/o/{name}/releases?per_page=20");
    fixture(dir, name, &api, 200, limits, Some(&listing));
    fixture_bytes(dir, &format!("{name}-zip"), &zip_url, 200, &[], Some(&zip));
}

/// Two beta refs in one command: the REST reserve stops the second.
const RATE_LIMITED: Row = Row {
    id: "a_second_beta_ref_past_the_rest_reserve_is_rate_limited",
    keys: &[(ErrorCode::RateLimited, "reset_at")],
    run: || {
        let home = Home::new(&["wow"]);
        let limits = [
            ("x-ratelimit-remaining", "5"),
            ("x-ratelimit-reset", "1800000000"),
        ];
        for name in ["a", "b"] {
            beta_listing(&home.http, name, &limits);
        }
        let args = [
            "install",
            "gh:o/a",
            "gh:o/b",
            "--channel",
            "beta",
            "-t",
            "retail",
        ];
        vec![check(home.run(&args), 0)]
    },
};

/// `foo` names an installed addon and another project's catalog entry.
const AMBIGUOUS_NAME: Row = Row {
    id: "a_name_installed_and_catalogued_as_two_refs_is_ambiguous",
    keys: &[
        (ErrorCode::RefAmbiguous, "input"),
        (ErrorCode::RefAmbiguous, "matches"),
        (ErrorCode::RefAmbiguous, "matches[].from"),
        (ErrorCode::RefAmbiguous, "matches[].ref"),
    ],
    run: || {
        let s = Scene::new();
        s.install_foo();
        catalog(
            &s.home,
            "50-foo.toml",
            "schema_version = \"1.0\"\n[[addon]]\nname = \"foo\"\nref = \"gh:b/foo\"\n\
             families = [\"mainline\"]\n",
        );
        vec![s.json(&["install", "foo", "-t", "retail", "--offline"], 3)]
    },
};

/// DepA requires DepB, whose only release is Classic Era's: DepB fails alone.
const MISSING_DEPENDENCY: Row = Row {
    id: "a_dependency_without_a_release_for_the_client_names_what_required_it",
    keys: &[(ErrorCode::NoCompatibleRelease, "required_by")],
    run: || {
        let home = Home::new(&["wow"]);
        let folder = |name, interface, deps| Folder {
            name,
            interface,
            deps,
            optional: &[],
        };
        let node = |base, folders| Node {
            base,
            tag: "v1",
            folders,
        };
        release_graph(
            &home.http,
            &[
                node(
                    "https://github.com/o/depa",
                    &[folder("DepA", "120105", &["DepB"])],
                ),
                node("https://github.com/o/depb", &[folder("DepB", "11509", &[])]),
            ],
        );
        catalog(
            &home,
            "50-deps.toml",
            "schema_version = \"1.0\"\n\n\
             [[addon]]\nname = \"DepA\"\nref = \"gh:o/depa\"\nfolders = [\"DepA\"]\n\n\
             [[addon]]\nname = \"DepB\"\nref = \"gh:o/depb\"\nfolders = [\"DepB\"]\n",
        );
        vec![check(home.run(&["install", "depa", "-t", "retail"]), 0)]
    },
};
