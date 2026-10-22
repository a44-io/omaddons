//! The WowAce source end to end (M3-D15). `tests/fixtures/http/wowace-bug-grabber/` is
//! a recording of the live server: the `PROPFIND` of BugGrabber's tags, the newest tag
//! (v12.1.1, revision 403) and v12.1.0 (401) at their revisions, and trunk at r152. The
//! tests edit copies of it: a poisoned live tag path, moved tags, packaging that needs
//! the packager, collisions, a tree past its bounds.

mod common;

use std::fs;

use common::env::TestEnv;
use common::home::Home;
use common::http::{record as record_http, replay, scenario};
use common::netlog::net_log;
use common::state::record;
use serde_json::Value;
use support::{
    V1211, age_memo, check_recorded, index, moved, retagged_tree, serve, serve_recorded, tried,
    with_wotlk,
};

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{Cursor, Read};
    use std::path::Path;

    use omaddons::limits::DOWNLOAD_MAX_BYTES;
    use omaddons::model::{AssetLocation, Release};
    use omaddons::net::Http;
    use omaddons::sources::{SourceConfigView, SourceRegistry, Wanted, fetch_once, wowace};
    use serde_json::{Value, json};

    use super::common::home::Home;
    use super::common::http::scenario;
    use super::common::trees::CustomRoot;

    /// The newest tag's pinned tree, as recorded.
    pub const V1211: &str = "https://repos.wowace.com/wow/bug-grabber/!svn/bc/403/tags/v12.1.1/";

    /// The recorded scenario, copied into the home's fixtures, where a test edits it.
    pub fn serve_recorded(home: &Home) {
        let from = scenario("wowace-bug-grabber").join("repos.wowace.com");
        let to = home.http.join("repos.wowace.com");
        fs::create_dir_all(&to).unwrap();
        for item in fs::read_dir(from).unwrap() {
            let item = item.unwrap();
            fs::copy(item.path(), to.join(item.file_name())).unwrap();
        }
    }

    /// Serves `body` with `status` for `method url` in `dir`, in place of the recorded
    /// fixture for it when there is one (no key may have two).
    pub fn serve(dir: &Path, method: &str, url: &str, status: u16, body: &[u8]) {
        let host = dir.join("repos.wowace.com");
        fs::create_dir_all(&host).unwrap();
        let recorded = fs::read_dir(&host).unwrap().find_map(|item| {
            let path = item.unwrap().path();
            let text = fs::read_to_string(&path).ok()?;
            let doc: Value = serde_json::from_str(&text).ok()?;
            let same = doc["request"]["method"] == method && doc["request"]["url"] == url;
            same.then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
        });
        let n = fs::read_dir(&host).unwrap().count();
        let stem = recorded.unwrap_or_else(|| format!("edited-{n}"));
        fs::write(host.join(format!("{stem}.body")), body).unwrap();
        let doc = json!({
            "schema_version": "1.0",
            "request": {"method": method, "url": url},
            "responses": [{"status": status, "headers": [], "body_file": format!("{stem}.body")}],
        });
        fs::write(host.join(format!("{stem}.json")), doc.to_string()).unwrap();
    }

    /// The recorded tag list with `tag`'s `version-name` moved to `to`: a re-tag
    /// upstream, or (another tag) a commit that is no update here.
    pub fn moved(tag: &str, to: u32) -> String {
        let dir = scenario("wowace-bug-grabber").join("repos.wowace.com");
        let path = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                let name = p.file_name().unwrap().to_string_lossy();
                name.starts_with("propfind-") && name.ends_with(".body")
            })
            .unwrap();
        let body = fs::read_to_string(path).unwrap();
        let at = body.find(&format!("/tags/{tag}/</D:href>")).unwrap();
        let (head, tail) = body.split_at(at);
        let open = tail.find("version-name>").unwrap() + "version-name>".len();
        let close = open + tail[open..].find('<').unwrap();
        format!("{head}{}{to}{}", &tail[..open], &tail[close..])
    }

    /// v12.1.1's tree as of revision `rev`, a re-tag whose `BugGrabber.lua` says so.
    pub fn retagged_tree(dir: &Path, rev: u32) {
        let base = format!("https://repos.wowace.com/wow/bug-grabber/!svn/bc/{rev}/tags/v12.1.1/");
        let names = ["!BugGrabber.toc", ".pkgmeta", "BugGrabber.lua", "load.xml"];
        serve(dir, "GET", &base, 200, index(&names).as_bytes());
        for (name, body) in [
            (
                "!BugGrabber.toc",
                "## Interface: 120105\n## Version: @project-version@\nload.xml\n",
            ),
            (".pkgmeta", "package-as: \"!BugGrabber\"\n"),
            ("BugGrabber.lua", "-- re-tagged\n"),
            ("load.xml", "<Ui></Ui>\n"),
        ] {
            serve(dir, "GET", &format!("{base}{name}"), 200, body.as_bytes());
        }
    }

    /// The release memo aged by a day, so the next check asks WowAce again.
    pub fn age_memo(home: &Home) {
        let paths = super::common::http::paths(&home.env);
        let (mut memo, _) = omaddons::state::memo::load(&paths);
        for entry in memo.entries.values_mut() {
            entry.fetched_at -= jiff::SignedDuration::from_hours(24);
        }
        home.seed_memo(&memo);
    }

    /// The id of the home's 3.3.5a client, registered as a custom target.
    pub fn with_wotlk(home: &mut Home) -> String {
        let root = CustomRoot::at(home.env.root().join("wow-335")).version(3, 3, 5, 12_340);
        home.with_custom(root);
        let targets = home.run(&["targets"]).expect(0);
        targets["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["family"] != "mainline")
            .and_then(|t| t["id"].as_str())
            .unwrap()
            .to_owned()
    }

    /// `tried[]` of a refused install.
    pub fn tried(envelope: &Value) -> Vec<String> {
        envelope["error"]["details"]["tried"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["reason"].as_str().unwrap().to_owned())
            .collect()
    }

    /// A mod_dav_svn HTML index naming `entries` (a trailing `/` marks a folder).
    pub fn index(entries: &[&str]) -> String {
        let items: String = entries
            .iter()
            .map(|e| format!("  <li><a href=\"{e}\">{e}</a></li>\n"))
            .collect();
        format!(
            "<html><head><title>bug-grabber - Revision 403: /tags/v12.1.1</title></head>\n\
             <body>\n <h2>bug-grabber - Revision 403: /tags/v12.1.1</h2>\n <ul>\n\
             \x20 <li><a href=\"../\">..</a></li>\n{items} </ul>\n</body></html>\n"
        )
    }

    /// A zip's entries by name, as text.
    pub fn entries(zip: &[u8]) -> BTreeMap<String, String> {
        let mut archive = zip::ZipArchive::new(Cursor::new(zip.to_vec())).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut file = archive.by_index(i).unwrap();
                let mut text = String::new();
                file.read_to_string(&mut text).unwrap();
                (file.name().unwrap().into_owned(), text)
            })
            .collect()
    }

    /// The tree of `release`, assembled.
    pub fn assembled(http: &dyn Http, release: &Release) -> BTreeMap<String, String> {
        let AssetLocation::WowAceTree { base, revision } = &release.assets[0].location else {
            unreachable!("{release:?}");
        };
        entries(&wowace::assemble(http, base, *revision, DOWNLOAD_MAX_BYTES).unwrap())
    }

    /// What the recorded scenario shows, live or replayed: BugGrabber's newest stable tag
    /// and v12.1.0 (revision 401), each tree packaged as `!BugGrabber/` with the TOC's
    /// version line filled in; trunk at r152, the 3.3.5a build; and trunk at r130, the
    /// 2.4.3 build. Every body goes
    /// through C1's parsers on the way (the tag list, each listing, `.pkgmeta`, the
    /// keyword scan). Returns the newest tag's release.
    pub fn check_recorded(http: &dyn Http) -> Release {
        let registry = SourceRegistry::builtin(&SourceConfigView::all_enabled());
        let newest = Wanted::stable("wowace:bug-grabber".parse().unwrap());
        let named = Wanted::stable("wowace:bug-grabber#v12.1.0".parse().unwrap());
        let peg = Wanted::stable("wowace:bug-grabber#r152".parse().unwrap());
        let mut results = fetch_once(
            [newest.clone(), named.clone(), peg.clone()],
            &registry,
            http,
        );
        let latest = results.remove(&newest).unwrap().unwrap().releases.remove(0);
        let toc = &assembled(http, &latest)["!BugGrabber/!BugGrabber.toc"];
        assert!(
            toc.contains(&format!("## Version: {}\n", latest.version)),
            "{toc}"
        );

        let tag = results.remove(&named).unwrap().unwrap().releases.remove(0);
        assert_eq!(
            (tag.release_id.as_str(), tag.revision),
            ("wowace:bug-grabber@v12.1.0", Some(401))
        );
        let files = assembled(http, &tag);
        assert_eq!(
            files.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "!BugGrabber/!BugGrabber.toc",
                "!BugGrabber/BugGrabber.lua",
                "!BugGrabber/load.xml"
            ]
        );
        let toc = &files["!BugGrabber/!BugGrabber.toc"];
        assert!(toc.contains("## Version: v12.1.0\n"), "{toc}");
        assert!(toc.contains("120105"), "{toc}");

        // The catalog's 2.4.3 variant: the last trunk revision declaring it, within the
        // packaging subset (its libraries are committed, not externals).
        let tbc = Wanted::stable("wowace:bug-grabber#r130".parse().unwrap());
        let mut sets = fetch_once([tbc.clone()], &registry, http);
        let release = sets.remove(&tbc).unwrap().unwrap().releases.remove(0);
        let files = assembled(http, &release);
        let toc = &files["!BugGrabber/!BugGrabber.toc"];
        assert!(toc.contains("## Interface: 20400"), "{toc}");
        assert!(
            files.contains_key("!BugGrabber/libs/LibStub/LibStub.lua"),
            "{files:?}"
        );

        let trunk = results.remove(&peg).unwrap().unwrap().releases.remove(0);
        assert_eq!(
            (trunk.release_id.as_str(), trunk.revision),
            ("wowace:bug-grabber@r152", Some(152))
        );
        let files = assembled(http, &trunk);
        let toc = &files["!BugGrabber/!BugGrabber.toc"];
        assert!(toc.contains("## Interface: 30300"), "{toc}");
        assert!(
            files.contains_key("!BugGrabber/BugGrabber.lua"),
            "{files:?}"
        );
        latest
    }
}

/// Re-records `tests/fixtures/http/wowace-bug-grabber/` from the live server. Review the
/// diff (fixtures are public).
/// `OMADDONS_RECORD=1 cargo test --test sources_wowace -- --ignored record`
#[test]
#[ignore = "records live fixtures: OMADDONS_RECORD=1"]
fn record_wowace() {
    // A recorder run on purpose, not a checkpoint check: unset, it has nothing to do.
    if std::env::var("OMADDONS_RECORD").as_deref() != Ok("1") {
        return;
    }
    let env = TestEnv::new();
    let http = record_http(&env, &scenario("wowace-bug-grabber"));
    check_recorded(http.as_ref());
}

#[test]
fn the_recorded_server_decodes() {
    let env = TestEnv::new();
    let http = replay(&env, &scenario("wowace-bug-grabber"));
    let latest = check_recorded(http.as_ref());
    assert_eq!(
        (latest.release_id.as_str(), latest.revision),
        ("wowace:bug-grabber@v12.1.1", Some(403)),
        "the newest stable tag when recorded"
    );
}

/// `install wowace:bug-grabber` on retail: `!BugGrabber` from the newest tag's tree at
/// its revision, TOC-verified, its version line filled in; every request to WowAce
/// under a `wowace-*` purpose, every tree request pinned, no HEAD, no GitHub.
#[test]
fn bug_grabber_installs_on_retail_from_its_pinned_tree() {
    let home = Home::new(&["wow"]);
    serve_recorded(&home);
    let r = home.run(&["install", "wowace:bug-grabber", "-t", "retail"]);
    let envelope = r.expect(0);
    let item = &envelope["data"]["items"][0];
    assert_eq!(item["status"], "installed", "{}", r.context());
    let toc =
        fs::read_to_string(home.addons("_retail_").join("!BugGrabber/!BugGrabber.toc")).unwrap();
    assert!(toc.contains("## Version: v12.1.1\n"), "{toc}");
    let addon = record(&home, "retail").addons.into_values().next().unwrap();
    assert_eq!(
        (addon.release_id.as_deref(), addon.revision),
        (Some("wowace:bug-grabber@v12.1.1"), Some(403))
    );
    assert_eq!(
        addon.folders.iter().map(|f| f.as_str()).collect::<Vec<_>>(),
        ["!BugGrabber"]
    );
    let warnings = envelope["warnings"].as_array().cloned().unwrap_or_default();
    assert!(
        !warnings.iter().any(|w| w["code"] == "download_unverified"),
        "{warnings:?}"
    );

    let log = net_log(&home);
    for e in &log.entries {
        assert!(
            e.host == "repos.wowace.com" && e.purpose.starts_with("wowace-") && e.method != "HEAD",
            "{}",
            log.context()
        );
        if e.purpose != "wowace-tags" {
            assert!(
                e.path.starts_with("/wow/bug-grabber/!svn/bc/403/"),
                "{}",
                log.context()
            );
        }
    }
    let methods: Vec<(&str, &str)> = log
        .entries
        .iter()
        .map(|e| (e.method.as_str(), e.purpose.as_str()))
        .collect();
    assert_eq!(
        methods,
        [
            ("PROPFIND", "wowace-tags"),
            ("GET", "wowace-tree"),
            ("GET", "wowace-file"),
            ("GET", "wowace-file"),
            ("GET", "wowace-file"),
            ("GET", "wowace-file"),
        ],
        "the tag list, one listing, .pkgmeta and three files: {}",
        log.context()
    );
}

/// The newest tag does not load on 3.3.5a, so the TOCs refuse it there; trunk at r152
/// installs.
#[test]
fn wotlk_installs_the_r152_peg_and_refuses_the_newest_tag() {
    let mut home = Home::new(&["wow"]);
    let wotlk = with_wotlk(&mut home);
    serve_recorded(&home);
    let r = home.run(&["install", "wowace:bug-grabber", "-t", &wotlk]);
    let envelope = r.expect(3);
    assert_eq!(
        envelope["error"]["code"],
        "no_compatible_release",
        "{}",
        r.context()
    );
    assert!(
        tried(&envelope)[0].starts_with("wrong_flavor: !BugGrabber"),
        "{}",
        r.context()
    );

    let r = home.run(&["install", "wowace:bug-grabber#r152", "-t", &wotlk]);
    r.expect(0);
    let addons = home.env.root().join("wow-335/Interface/AddOns");
    let toc = fs::read_to_string(addons.join("!BugGrabber/!BugGrabber.toc")).unwrap();
    assert!(toc.contains("## Interface: 30300"), "{toc}");
    let addon = record(&home, &wotlk).addons.into_values().next().unwrap();
    assert_eq!(
        (
            addon.release_id.as_deref(),
            addon.version.as_str(),
            addon.revision
        ),
        (Some("wowace:bug-grabber@r152"), "r152", Some(152))
    );

    // The same pin pasted as a repository link: the same ref, already installed.
    let link = "https://repos.wowace.com/wow/bug-grabber/!svn/bc/152/trunk/";
    let r = home.run(&["install", link, "-t", &wotlk]);
    let item = &r.expect(0)["data"]["items"][0];
    assert_eq!(
        (&item["ref"], &item["status"]),
        (
            &Value::from("wowace:bug-grabber#r152"),
            &Value::from("unchanged")
        ),
        "{}",
        r.context()
    );
}

/// The live tag path serves other bytes; nothing ever asks for it, so the install is the
/// pinned tree's.
#[test]
fn a_live_tag_path_is_never_requested() {
    let home = Home::new(&["wow"]);
    serve_recorded(&home);
    let live = "https://repos.wowace.com/wow/bug-grabber/tags/v12.1.1/";
    serve(
        &home.http,
        "GET",
        live,
        200,
        index(&["!BugGrabber.toc", "Evil.lua"]).as_bytes(),
    );
    serve(
        &home.http,
        "GET",
        &format!("{live}!BugGrabber.toc"),
        200,
        b"## Interface: 120105\n",
    );
    serve(
        &home.http,
        "GET",
        &format!("{live}Evil.lua"),
        200,
        b"evil()\n",
    );
    home.run(&["install", "wowace:bug-grabber", "-t", "retail"])
        .expect(0);
    let folder = home.addons("_retail_").join("!BugGrabber");
    assert!(!folder.join("Evil.lua").exists());
    let toc = fs::read_to_string(folder.join("!BugGrabber.toc")).unwrap();
    assert!(toc.contains("## Version: v12.1.1\n"), "{toc}");
    let log = net_log(&home);
    assert!(
        log.entries
            .iter()
            .all(|e| !e.path.starts_with("/wow/bug-grabber/tags/v")),
        "{}",
        log.context()
    );
}

/// One pinned tree always packs to the same archive, wherever it is assembled.
#[test]
fn one_pinned_tree_packs_to_one_blob() {
    let blob = || {
        let home = Home::new(&["wow"]);
        serve_recorded(&home);
        home.run(&["install", "wowace:bug-grabber", "-t", "retail"])
            .expect(0);
        let addon = record(&home, "retail").addons.into_values().next().unwrap();
        addon.blob_sha256.unwrap()
    };
    assert_eq!(blob(), blob());
}

/// What needs the packager is refused by name, and nothing is installed.
#[test]
fn a_project_that_needs_the_packager_is_refused_by_name() {
    let home = Home::new(&["wow"]);
    serve_recorded(&home);
    let pkgmeta = format!("{V1211}.pkgmeta");
    let lua = format!("{V1211}BugGrabber.lua");
    for (meta, code, reason) in [
        (
            "package-as: \"!BugGrabber\"\nexternals:\n  Libs/LibStub: svn://x\n",
            "local x = 1\n",
            "packaging_unsupported: .pkgmeta: externals",
        ),
        (
            "package-as: \"!BugGrabber\"\nmove-folders:\n  a: b\nignore:\n  - x\n",
            "local x = 1\n",
            "packaging_unsupported: .pkgmeta: move-folders; .pkgmeta: ignore",
        ),
        (
            "package-as: \"!BugGrabber\"\n",
            "local x = 1\n--@debug@\nprint(x)\n--@end-debug@\n",
            "packaging_unsupported: BugGrabber.lua:2: --@debug@; BugGrabber.lua:4: --@end-debug@",
        ),
        (
            "package-as: \"Bug/Grabber\"\n",
            "local x = 1\n",
            "packaging_unsupported: .pkgmeta: package-as \"Bug/Grabber\" is not a folder name",
        ),
    ] {
        serve(&home.http, "GET", &pkgmeta, 200, meta.as_bytes());
        serve(&home.http, "GET", &lua, 200, code.as_bytes());
        let r = home.run(&["install", "wowace:bug-grabber", "-t", "retail"]);
        let envelope = r.expect(3);
        assert_eq!(
            envelope["error"]["code"],
            "no_compatible_release",
            "{}",
            r.context()
        );
        assert_eq!(tried(&envelope), [reason], "{}", r.context());
        assert!(!home.addons("_retail_").join("!BugGrabber").exists());
    }
}

/// Names equal ignoring case, in one listing, are a broken tree, named as one.
#[test]
fn a_case_fold_collision_or_a_repeated_directory_is_a_tree_collision() {
    let home = Home::new(&["wow"]);
    serve_recorded(&home);
    for listing in [
        index(&["!BugGrabber.toc", "BugGrabber.lua", "load.xml", "Load.XML"]),
        index(&["!BugGrabber.toc", "Libs/", "Libs/"]),
    ] {
        serve(&home.http, "GET", V1211, 200, listing.as_bytes());
        let r = home.run(&["install", "wowace:bug-grabber", "-t", "retail"]);
        let envelope = r.expect(1);
        assert_eq!(
            (
                &envelope["error"]["code"],
                &envelope["error"]["details"]["reason"]
            ),
            (&Value::from("bad_archive"), &Value::from("tree_collision")),
            "{}",
            r.context()
        );
        assert!(!home.addons("_retail_").join("!BugGrabber").exists());
    }
}

/// A tree past `WOWACE_TREE_MAX_DIRS` fails before any file is fetched, and nothing is
/// installed. (The request bound, retries included, is the source's unit test: retries
/// sleep.)
#[test]
fn a_tree_past_its_directory_bound_fails_cleanly() {
    let home = Home::new(&["wow"]);
    serve_recorded(&home);
    let dirs: Vec<String> = (0..omaddons::limits::WOWACE_TREE_MAX_DIRS)
        .map(|i| format!("d{i}/"))
        .collect();
    let names: Vec<&str> = dirs.iter().map(String::as_str).collect();
    serve(&home.http, "GET", V1211, 200, index(&names).as_bytes());
    for dir in &dirs {
        serve(
            &home.http,
            "GET",
            &format!("{V1211}{dir}"),
            200,
            index(&[]).as_bytes(),
        );
    }
    let r = home.run(&["install", "wowace:bug-grabber", "-t", "retail"]);
    let envelope = r.expect(4);
    assert_eq!(
        (
            &envelope["error"]["code"],
            &envelope["error"]["details"]["constant"]
        ),
        (
            &Value::from("archive_too_large"),
            &Value::from("WOWACE_TREE_MAX_DIRS")
        ),
        "{}",
        r.context()
    );
    assert!(!home.addons("_retail_").join("!BugGrabber").exists());
    let log = net_log(&home);
    assert_eq!(log.count_purpose("wowace-file"), 0, "{}", log.context());
    assert_eq!(
        log.count_purpose("wowace-tree"),
        omaddons::limits::WOWACE_TREE_MAX_DIRS
    );
}

#[test]
fn offline_with_nothing_cached_and_a_disabled_source_refuse() {
    let home = Home::new(&["wow"]);
    serve_recorded(&home);
    let r = home.run(&["install", "wowace:bug-grabber", "-t", "retail", "--offline"]);
    let envelope = r.expect(r.code);
    assert_eq!(envelope["error"]["code"], "offline", "{}", r.context());
    assert!(net_log(&home).is_empty());

    home.run(&["config", "set", "sources.wowace.enabled", "false"])
        .expect(0);
    let r = home.run(&["install", "wowace:bug-grabber", "-t", "retail"]);
    let envelope = r.expect(r.code);
    assert_eq!(
        envelope["error"]["code"],
        "source_disabled",
        "{}",
        r.context()
    );
    assert_eq!(
        envelope["error"]["fix_argv"],
        serde_json::json!([
            "omaddons",
            "config",
            "set",
            "sources.wowace.enabled",
            "true"
        ])
    );
}

const TAGS: &str = "https://repos.wowace.com/wow/bug-grabber/tags/";

/// The status `outdated` gives the one addon on retail (a current one is only counted).
fn outdated(home: &Home) -> Value {
    let r = home.run(&["outdated", "-t", "retail"]);
    let envelope = r.expect(r.code);
    if envelope["data"]["summary"]["current"] == 1 {
        return serde_json::json!({ "status": "current" });
    }
    let items = envelope["data"]["targets"][0]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(items.len(), 1, "{}", r.context());
    items[0].clone()
}

/// A commit that moves another tag is no update; the installed tag moved is a re-tag,
/// which `update` installs at its new revision.
#[test]
fn only_a_re_tag_of_the_installed_tag_is_an_update() {
    let mut home = Home::new(&["wow"]);
    serve_recorded(&home);
    home.run(&["install", "wowace:bug-grabber", "-t", "retail"])
        .expect(0);

    let unrelated = home.scenario("unrelated");
    serve_recorded(&home);
    serve(
        &unrelated,
        "PROPFIND",
        TAGS,
        207,
        moved("v12.0.9", 404).as_bytes(),
    );
    age_memo(&home);
    let item = outdated(&home);
    assert_eq!(item["status"], "current", "{item}");
    assert_eq!(
        net_log(&home).count_purpose("wowace-tags"),
        2,
        "asked again"
    );

    let retag = home.scenario("retag");
    serve_recorded(&home);
    serve(
        &retag,
        "PROPFIND",
        TAGS,
        207,
        moved("v12.1.1", 404).as_bytes(),
    );
    retagged_tree(&retag, 404);
    age_memo(&home);
    let item = outdated(&home);
    assert_eq!(
        (
            &item["status"],
            &item["latest"]["revision"],
            &item["latest"]["retagged"]
        ),
        (
            &Value::from("update_available"),
            &Value::from(404),
            &Value::from(true)
        ),
        "{item}"
    );
    let human = home.exec(&["outdated".to_owned(), "-t".to_owned(), "retail".to_owned()]);
    assert!(
        human.stdout.contains("v12.1.1 (re-tagged upstream)"),
        "{}",
        human.context()
    );

    let r = home.run(&["update", "--all", "-t", "retail"]);
    r.expect(0);
    let addon = record(&home, "retail").addons.into_values().next().unwrap();
    assert_eq!(
        (addon.release_id.as_deref(), addon.revision),
        (Some("wowace:bug-grabber@v12.1.1"), Some(404)),
        "{}",
        r.context()
    );
    let lua = home.addons("_retail_").join("!BugGrabber/BugGrabber.lua");
    assert_eq!(fs::read_to_string(lua).unwrap(), "-- re-tagged\n");
    assert_eq!(outdated(&home)["status"], "current");
}

/// `export` records the revision; `sync --locked` elsewhere installs exactly that tree
/// after the tag moved upstream, without asking for the tag list, and with the archive
/// cached reinstalls it offline with no request at all.
#[test]
fn a_locked_sync_installs_the_recorded_revision() {
    let home = Home::new(&["wow"]);
    serve_recorded(&home);
    home.run(&["install", "wowace:bug-grabber", "-t", "retail"])
        .expect(0);
    let r = home.exec(&["export".to_owned()]);
    assert_eq!(r.code, 0, "{}", r.context());
    let toml = r.stdout;
    assert!(
        toml.contains("version = \"v12.1.1\"") && toml.contains("revision = 403"),
        "{toml}"
    );

    // Elsewhere, after v12.1.1 was re-tagged at 404.
    let mut other = Home::new(&["wow"]);
    other.write_desired(&toml);
    let moved_on = other.scenario("moved-on");
    serve_recorded(&other);
    serve(
        &moved_on,
        "PROPFIND",
        TAGS,
        207,
        moved("v12.1.1", 404).as_bytes(),
    );
    retagged_tree(&moved_on, 404);
    let r = other.run(&["sync", "--locked", "-t", "retail"]);
    r.expect(0);
    let addon = record(&other, "retail")
        .addons
        .into_values()
        .next()
        .unwrap();
    assert_eq!(addon.revision, Some(403), "{}", r.context());
    let lua = other.addons("_retail_").join("!BugGrabber/BugGrabber.lua");
    assert_ne!(fs::read_to_string(&lua).unwrap(), "-- re-tagged\n");
    let log = net_log(&other);
    assert_eq!(log.count_purpose("wowace-tags"), 0, "{}", log.context());
    assert!(
        log.entries.iter().all(|e| e.path.contains("/!svn/bc/403/")),
        "{}",
        log.context()
    );

    // The archive is cached: offline, gone and back with no request.
    other
        .run(&["remove", "!BugGrabber", "-t", "retail"])
        .expect(0);
    let before = net_log(&other).len();
    let r = other.run(&["sync", "--locked", "--offline", "-t", "retail"]);
    r.expect(0);
    assert_eq!(net_log(&other).len(), before, "{}", r.context());
    let addon = record(&other, "retail")
        .addons
        .into_values()
        .next()
        .unwrap();
    assert_eq!(
        (addon.release_id.as_deref(), addon.revision),
        (Some("wowace:bug-grabber@v12.1.1"), Some(403))
    );
}

#[test]
fn the_changelog_is_not_supported() {
    let home = Home::new(&["wow"]);
    serve_recorded(&home);
    let r = home.run(&["info", "wowace:bug-grabber", "--changelog"]);
    let envelope = r.expect(0);
    assert_eq!(
        (
            &envelope["data"]["latest"]["release_id"],
            &envelope["data"]["changelog_unavailable"]
        ),
        (
            &Value::from("wowace:bug-grabber@v12.1.1"),
            &Value::from("not_supported")
        ),
        "{}",
        r.context()
    );
}
