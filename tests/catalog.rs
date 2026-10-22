//! Every GitHub and `git:` catalog entry resolves through its source, replayed from
//! upstream responses recorded in `tests/fixtures/http/catalog/` (the Tukui entries are
//! `tests/sources_tukui.rs`'s).

mod common;

use common::addons::{toc, unpack};
use common::env::TestEnv;
use common::home::Home;
use common::http::{record, replay, scenario};
use common::netlog::{assert_each_url_once, assert_no_github_rest, read_net_log};
use common::state::record as target_record;
use omaddons::model::Ref;
use omaddons::resolve::catalog::Catalog;
use omaddons::sources::{SourceConfigView, SourceRegistry, Wanted, fetch_once};
use serde_json::{Value, json};
use support::{legacy, wanted};

#[cfg(test)]
mod support {
    use super::common::trees::CustomRoot;
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

    /// Registers a custom client of `version` (major, minor, patch, build) in `home`;
    /// returns its target id.
    pub fn legacy(home: &mut Home, dir: &str, version: (u16, u16, u16, u16)) -> String {
        let (major, minor, patch, build) = version;
        let root = CustomRoot::at(home.env.root().join(dir)).version(major, minor, patch, build);
        let client = home.with_custom(root);
        let targets = home.run(&["targets"]).expect(0);
        targets["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| {
                t["client_dir"]
                    .as_str()
                    .is_some_and(|d| std::path::Path::new(d) == client)
            })
            .and_then(|t| t["id"].as_str())
            .unwrap()
            .to_owned()
    }
}

fn github(r: &Ref) -> bool {
    matches!(r, Ref::GitHub { .. })
}

fn git(r: &Ref) -> bool {
    matches!(r, Ref::Git { .. })
}

fn wowi(r: &Ref) -> bool {
    matches!(r, Ref::Wowi { .. })
}

/// Re-records the scenario from live GitHub. Delete the directory first, then review
/// the diff (fixtures are public; trim contributor names from changelogs).
/// `OMADDONS_RECORD=1 cargo test --test catalog -- --ignored record`
#[test]
#[ignore = "records live fixtures: OMADDONS_RECORD=1"]
fn record_catalog() {
    // A recorder run on purpose, not a checkpoint check: unset, it has nothing to do.
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
    // A recorder run on purpose, not a checkpoint check: unset, it has nothing to do.
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

/// Re-records the WoWInterface entries' file details into the same scenario, without
/// touching the others: one `filedetails` request names every id. Then empty each
/// file's `UIChangeLog` and `UIDescription` in the gzipped body: nothing reads them
/// here, and they can name contributors.
/// `OMADDONS_RECORD=1 cargo test --test catalog -- --ignored record_catalog_wowi`
#[test]
#[ignore = "records live fixtures: OMADDONS_RECORD=1"]
fn record_catalog_wowi() {
    // A recorder run on purpose, not a checkpoint check: unset, it has nothing to do.
    if std::env::var("OMADDONS_RECORD").as_deref() != Ok("1") {
        return;
    }
    let env = TestEnv::new();
    let http = record(&env, &scenario("catalog"));
    let results = fetch_once(
        wanted(wowi),
        &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
        http.as_ref(),
    );
    for (w, result) in results {
        assert!(result.is_ok(), "{}: {result:?}", w.r#ref);
    }
}

#[test]
fn every_wowi_entry_resolves_in_one_request_with_its_md5() {
    let env = TestEnv::new();
    let http = replay(&env, &scenario("catalog"));
    let wanted = wanted(wowi);
    assert_eq!(wanted.len(), 9, "the WoWInterface entries");
    let results = fetch_once(
        wanted,
        &SourceRegistry::builtin(&SourceConfigView::all_enabled()),
        http.as_ref(),
    );
    for (w, result) in &results {
        assert!(result.is_ok(), "{}: {result:?}", w.r#ref);
        let set = result.as_ref().unwrap();
        assert!(
            set.releases
                .first()
                .and_then(|r| r.assets.first())
                .is_some_and(|a| a.expected_hash.is_some()),
            "{}: the file and its published MD5: {set:?}",
            w.r#ref
        );
    }
    let log = read_net_log(&common::http::paths(&env));
    assert_eq!(log.len(), 1, "one filedetails request: {log:?}");
    assert_eq!(log[0].purpose, "wowi-details");
}

#[test]
fn every_github_entry_resolves_without_rest() {
    let env = TestEnv::new();
    let http = replay(&env, &scenario("catalog"));
    let wanted = wanted(github);
    assert_eq!(wanted.len(), 46, "the release.json publishers");
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

// ---- BugGrabber: one name, each client its own build (M3-D16) --------------------------

/// The ref and status `install` reports for its one item.
fn installed(run: &common::home::Run) -> (Value, Value) {
    let envelope = run.expect(0);
    let item = &envelope["data"]["items"][0];
    (item["ref"].clone(), item["status"].clone())
}

/// `install buggrabber` installs each client's build (replayed from the WowAce
/// recording); `pin`, `unpin` and `update` find it by its name or either alias; `info`
/// shows the client's variant and the others' families.
#[test]
fn buggrabber_installs_each_clients_own_build() {
    let mut home = Home::new(&["wow"]);
    let wotlk = legacy(&mut home, "wow-335", (3, 3, 5, 12_340));
    let tbc = legacy(&mut home, "wow-243", (2, 4, 3, 8606));
    home.serve_recorded("wowace-bug-grabber");
    for (target, r) in [
        ("retail", "wowace:bug-grabber"),
        (wotlk.as_str(), "wowace:bug-grabber#r152"),
        (tbc.as_str(), "wowace:bug-grabber#r130"),
    ] {
        let run = home.run(&["install", "buggrabber", "-t", target]);
        assert_eq!(
            installed(&run),
            (json!(r), json!("installed")),
            "{}",
            run.context()
        );
        let addon = target_record(&home, target)
            .addons
            .into_values()
            .next()
            .unwrap();
        assert_eq!(
            (addon.name.as_str(), addon.r#ref.to_string()),
            ("BugGrabber", r.to_owned())
        );
    }
    for target in ["retail", wotlk.as_str()] {
        home.run(&["pin", "bug-grabber", "-t", target]).expect(0);
        assert!(target_record(&home, target).addons["buggrabber"].pinned);
        home.run(&["unpin", "!BugGrabber", "-t", target]).expect(0);
        assert!(!target_record(&home, target).addons["buggrabber"].pinned);
        let run = home.run(&["update", "buggrabber", "-t", target]);
        let envelope = run.expect(0);
        assert_eq!(
            envelope["data"]["summary"]["failed"],
            0,
            "{}",
            run.context()
        );
    }

    let info = home.run(&["info", "buggrabber", "-t", &wotlk]).expect(0);
    let catalog = &info["data"]["catalog"];
    assert_eq!(
        (
            &info["data"]["ref"],
            &catalog["variant"],
            &catalog["families"]
        ),
        (
            &json!("wowace:bug-grabber#r152"),
            &json!("r152"),
            &json!(["wrath-legacy"])
        )
    );
    assert_eq!(
        catalog["variants"],
        json!([
            {"variant": "modern", "families": ["mainline", "vanilla", "tbc", "wrath", "titan", "cata", "mists", "forever"]},
            {"variant": "r130", "families": ["tbc-legacy"]}
        ])
    );
    let info = home.run(&["info", "buggrabber"]).expect(0);
    assert_eq!(
        (&info["data"]["ref"], &info["data"]["catalog"]["variant"]),
        (&json!("wowace:bug-grabber"), &json!("modern")),
        "several clients and no default: the modern build"
    );
}

/// Without `-t`: the default target's build; with no default, the probe skips a client
/// no build is known for instead of offering it another family's.
#[test]
fn without_t_buggrabber_goes_where_its_build_is_known() {
    let mut home = Home::new(&[]);
    let wotlk = legacy(&mut home, "wow-335", (3, 3, 5, 12_340));
    legacy(&mut home, "wow-243", (2, 4, 3, 8606));
    home.serve_recorded("wowace-bug-grabber");
    home.run(&["config", "set", "default_target", &wotlk])
        .expect(0);
    let run = home.run(&["install", "buggrabber"]);
    assert_eq!(
        installed(&run),
        (json!("wowace:bug-grabber#r152"), json!("installed")),
        "{}",
        run.context()
    );
    assert_eq!(run.envelope()["data"]["target"]["id"], json!(wotlk));

    let mut home = Home::new(&["wow"]);
    legacy(&mut home, "wow-112", (1, 12, 1, 5875));
    home.serve_recorded("wowace-bug-grabber");
    let run = home.run(&["install", "buggrabber"]);
    assert_eq!(
        installed(&run),
        (json!("wowace:bug-grabber"), json!("installed")),
        "{}",
        run.context()
    );
    assert_eq!(run.envelope()["data"]["target"]["id"], "retail");
}

/// `sync` resolves the name on every client: each gets its own build, and a client no
/// build is known for fails that entry alone, as a name the catalog does not know.
#[test]
fn sync_gives_each_client_its_build_and_fails_alone_where_none_is_known() {
    let mut home = Home::new(&["wow"]);
    let wotlk = legacy(&mut home, "wow-335", (3, 3, 5, 12_340));
    let vanilla = legacy(&mut home, "wow-112", (1, 12, 1, 5875));
    home.serve_recorded("wowace-bug-grabber");
    home.write_desired(&format!(
        "schema_version = \"1.0\"\n\n[target.\"retail\"]\naddons = [\"buggrabber\"]\n\n\
         [target.\"{wotlk}\"]\naddons = [\"!buggrabber\"]\n\n[target.\"{vanilla}\"]\naddons = [\"buggrabber\"]\n"
    ));
    let run = home.run(&["sync"]);
    let envelope = run.envelope();
    let targets = envelope["data"]["targets"].as_array().unwrap();
    let of = |id: &str| targets.iter().find(|t| t["target"]["id"] == id).unwrap();
    for (id, r) in [
        ("retail", "wowace:bug-grabber"),
        (wotlk.as_str(), "wowace:bug-grabber#r152"),
    ] {
        let item = &of(id)["items"][0];
        assert_eq!(
            (&item["ref"], &item["status"]),
            (&json!(r), &json!("installed")),
            "{}",
            run.context()
        );
    }
    let item = &of(&vanilla)["items"][0];
    assert_eq!(
        (&item["status"], &item["error"]["code"]),
        (&json!("failed"), &json!("not_found")),
        "{}",
        run.context()
    );
    assert!(
        item["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no BugGrabber build for this client is known"),
        "{}",
        run.context()
    );
    assert!(target_record(&home, &vanilla).addons.is_empty());
}

/// Adoption guesses a 3.3.5a `!BugGrabber` folder as the 3.3.5a build at most, never
/// the modern one.
#[test]
fn adoption_never_guesses_another_familys_build() {
    let mut home = Home::new(&[]);
    let wotlk = legacy(&mut home, "wow-335", (3, 3, 5, 12_340));
    home.serve_recorded("wowace-bug-grabber");
    let addons = home.env.root().join("wow-335/Interface/AddOns");
    unpack(
        &addons,
        &[(
            "!BugGrabber/!BugGrabber.toc",
            &toc("30300", &["## Title: BugGrabber"]),
        )],
    );
    let run = home.run(&["adopt", "-t", &wotlk, "--no-verify"]);
    let envelope = run.expect(0);
    let text = envelope.to_string();
    assert!(
        !text.contains("\"wowace:bug-grabber\""),
        "the modern build is never offered: {}",
        run.context()
    );
    assert!(
        text.contains("wowace:bug-grabber#r152"),
        "{}",
        run.context()
    );
}

/// A broken `catalog.d` override is reported by a command that then fails too (it may
/// be why the name is unknown): once in the envelope's `warnings`, and as a stderr
/// `warning` event before the `error` event. A command that succeeds reports it once.
#[test]
fn a_broken_override_is_reported_by_a_failing_command_too() {
    let home = Home::new(&["wow"]);
    let catalog_d = home.env.config().join("omaddons/catalog.d");
    std::fs::create_dir_all(&catalog_d).unwrap();
    std::fs::write(
        catalog_d.join("90-broken.toml"),
        "schema_version = \"1.0\"\nbogus_key = 1\n",
    )
    .unwrap();
    let once = |run: &common::home::Run, envelope: &Value| {
        let codes: Vec<&Value> = envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| &w["code"])
            .filter(|c| *c == "catalog_override_invalid")
            .collect();
        assert_eq!(codes.len(), 1, "{}", run.context());
    };
    for args in [
        &["info", "nosuchaddonxyz"][..],
        &["install", "nosuchaddonxyz", "-t", "retail"],
        &["pin", "nosuchaddonxyz", "-t", "retail"],
        &["unpin", "nosuchaddonxyz", "-t", "retail"],
        &["update", "nosuchaddonxyz", "-t", "retail"],
        &["adopt", "nosuchaddonxyz", "-t", "retail"],
    ] {
        let mut argv: Vec<&str> = args.to_vec();
        argv.push("--offline");
        let run = home.run(&argv);
        assert_ne!(run.code, 0, "{}", run.context());
        let envelope: Value = serde_json::from_str(&run.stdout).unwrap();
        once(&run, &envelope);
        let events: Vec<String> = run
            .stderr
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|e| match e["event"].as_str() {
                Some("warning") if e["code"] == "catalog_override_invalid" => {
                    Some("warning".to_owned())
                }
                Some("error") => Some("error".to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(events, ["warning", "error"], "{}", run.context());
    }
    let run = home.run(&["info", "weakauras", "--offline"]);
    let envelope = run.expect(0);
    once(&run, &envelope);
}
