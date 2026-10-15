//! Live probes of the keyless M1 endpoints, run at milestone checkpoints only:
//! `OMADDONS_LIVE=1 cargo test --test live_probes -- --ignored`. Each probe makes one
//! request (two for the conditional atom check) through the production stack against
//! `gh:WeakAuras/WeakAuras2` and checks raw shape only; source beads add decoder-level
//! probes here.

mod common;

use std::io::{Read, Write};

use std::collections::{BTreeMap, BTreeSet};

use common::env::TestEnv;
use common::http::paths;
use common::netlog::{assert_no_github_rest, read_net_log};
use common::trees::{BattlenetRoot, CustomRoot, Prefix};
use omaddons::model::{AssetLocation, Ref};
use omaddons::net::{CacheState, Http, NetConfig, NetLog, Request, stack};
use omaddons::resolve::catalog::Catalog;
use omaddons::rules::{Family, RulesTable};
use omaddons::sources::git::GitSource;
use omaddons::sources::git_hosts::GitRunner;
use omaddons::sources::github::GithubSource;
use omaddons::sources::{Source, Wanted};
use omaddons::state::paths::Paths;
use serde_json::Value;
use support::{declared_family, latest_tag, live, outcomes};

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

    /// The family a release.json entry `(flavor, interface)` declares: by the rules
    /// table's interface ranges first (Questie declares `wrath:38002` for Titan), then by
    /// the flavor name; `None` when neither names one.
    pub fn declared_family(
        table: &RulesTable,
        flavor: &str,
        interface: Option<u32>,
    ) -> Option<Family> {
        interface
            .and_then(|i| {
                Family::ALL
                    .into_iter()
                    .find(|f| table.family_range(*f).is_some_and(|r| r.contains(i)))
            })
            .or_else(|| Family::from_release_flavor(flavor))
    }

    /// Each item of an install envelope by request: `Ok(folders)` when installed,
    /// `Err((code, message))` when it failed (also from an all-failed error envelope).
    pub fn outcomes(envelope: &Value) -> BTreeMap<String, Result<Value, (String, String)>> {
        let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
        let mut out = BTreeMap::new();
        if let Some(items) = envelope["data"]["items"].as_array() {
            for item in items {
                let result = if item["status"] == "failed" {
                    Err((
                        text(&item["error"]["code"]),
                        text(&item["error"]["message"]),
                    ))
                } else {
                    Ok(item["folders"].clone())
                };
                out.insert(text(&item["request"]), result);
            }
        } else if let Some(items) = envelope["error"]["details"]["items"].as_array() {
            for item in items {
                out.insert(
                    text(&item["request"]),
                    Err((text(&item["code"]), text(&item["message"]))),
                );
            }
        }
        out
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

/// The probe below maps what a publisher declares by interface first, then flavor name.
#[test]
fn declared_families_map_by_interface_first() {
    let table = RulesTable::builtin();
    for (flavor, interface, family) in [
        ("wrath", Some(38_002), Some(Family::Titan)),
        ("bcc", Some(20_506), Some(Family::Tbc)),
        ("classic", Some(11_509), Some(Family::Vanilla)),
        ("forever", Some(16_001), Some(Family::Forever)),
        ("mainline", Some(120_105), Some(Family::Mainline)),
        ("wrath", Some(30_405), Some(Family::Wrath)),
        ("mists", None, Some(Family::Mists)),
        ("plunderstorm", Some(120_105), Some(Family::Mainline)),
        ("plunderstorm", None, None),
    ] {
        assert_eq!(
            declared_family(&table, flavor, interface),
            family,
            "{flavor}:{interface:?}"
        );
    }
}

/// Every catalog entry agrees with what its publisher declares
/// (omad-classic-forever-gaps-xud.7), on synthetic copies of every live client and every
/// legacy generation: a GitHub entry installs exactly on the families its release.json
/// declares (interface first, then flavor name) and is refused with
/// `no_compatible_release` everywhere else; a legacy git entry likewise against its
/// catalog `families`. Prints the matrix (✓ installs as declared, · refused as declared,
/// ✗ mismatch, ~ known publisher mismatch, - no release.json), then asserts no mismatch
/// and zero REST. A standing milestone-checkpoint check.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn catalog_matches_declared_flavors() {
    // (entry, family, why), dated: a publisher's release.json and its own TOCs disagree,
    // and omaddons follows the TOCs (D5).
    const KNOWN_PUBLISHER_MISMATCHES: &[(&str, &str, &str)] = &[
        (
            "Plater",
            "titan",
            "2026-10-15, v661: declares titan:38001, but no TOC lists a 38xxx interface \
             (Plater.toc 120105, _Mainline, _TBC, _Vanilla, _Mists, _Camelot)",
        ),
        (
            "RaiderIO",
            "vanilla",
            "2026-10-15: declares only mainline, but ships RaiderIO_Vanilla.toc, which \
             Classic Era loads",
        ),
        (
            "RaiderIO",
            "mists",
            "2026-10-15: declares only mainline, but ships RaiderIO_Classic.toc, which \
             Mists Classic loads",
        ),
    ];
    let live_families = [
        Family::Mainline,
        Family::Vanilla,
        Family::Tbc,
        Family::Mists,
        Family::Titan,
        Family::Forever,
    ];
    let legacy_families = [
        Family::VanillaLegacy,
        Family::TbcLegacy,
        Family::WrathLegacy,
        Family::CataLegacy,
        Family::MistsLegacy,
    ];
    let Some((env, http)) = live() else { return };
    let mut root = BattlenetRoot::at(Prefix::new(&env, "battlenet").wow_root());
    for code in [
        "wow",
        "wow_classic_era",
        "wow_anniversary",
        "wow_classic",
        "wow_classic_titan",
        "wowf",
    ] {
        root = root.product(code);
    }
    root.build();
    for (dir, (major, minor, patch, build)) in [
        ("legacy-112", (1, 12, 1, 5875)),
        ("legacy-243", (2, 4, 3, 8606)),
        ("legacy-335", (3, 3, 5, 12_340)),
        ("legacy-434", (4, 3, 4, 15_595)),
        ("legacy-548", (5, 4, 8, 18_414)),
    ] {
        CustomRoot::at(env.home().join("Games").join(dir))
            .version(major, minor, patch, build)
            .build();
    }
    let run = |args: &[&str]| -> Value {
        let out = env.command().args(args).arg("--json").output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let targets = run(&["targets"]);
    let id_of: BTreeMap<Family, String> = targets["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| {
            let family = Family::from_name(t["family"].as_str()?)?;
            Some((family, t["id"].as_str()?.to_owned()))
        })
        .collect();
    for family in live_families.iter().chain(&legacy_families) {
        assert!(
            id_of.contains_key(family),
            "no {family} client: {targets:#}"
        );
    }

    // What each entry declares: release.json for GitHub entries, catalog families for
    // legacy git entries. `None`: no release.json (the REST fallback; skipped).
    let table = RulesTable::builtin();
    let (catalog, _) = Catalog::load(&paths(&env));
    let mut rows: Vec<(String, bool, Option<BTreeSet<Family>>, String)> = Vec::new();
    for entry in catalog.entries() {
        match &entry.r#ref {
            Ref::GitHub { .. } => {
                let fetched = GithubSource::new()
                    .fetch(&[Wanted::stable(entry.r#ref.clone())], http.as_ref())
                    .pop()
                    .and_then(Result::ok);
                let assets: Vec<_> = fetched
                    .iter()
                    .flat_map(|set| set.releases.first())
                    .flat_map(|r| r.assets.iter())
                    .collect();
                let flavors: Vec<(String, Option<u32>)> = assets
                    .iter()
                    .flat_map(|a| a.flavors.iter())
                    .map(|f| (f.flavor.clone(), f.interface))
                    .collect();
                let listed: Vec<String> = flavors
                    .iter()
                    .map(|(f, i)| i.map_or_else(|| f.clone(), |i| format!("{f}:{i}")))
                    .collect();
                let declared = (!assets.is_empty() && assets.iter().all(|a| !a.flavors.is_empty()))
                    .then(|| {
                        flavors
                            .iter()
                            .filter_map(|(f, i)| declared_family(&table, f, *i))
                            .collect()
                    });
                rows.push((entry.name.clone(), false, declared, listed.join(", ")));
            }
            _ => {
                let declared: BTreeSet<Family> = entry.families.iter().copied().collect();
                let listed: Vec<&str> = entry.families.iter().map(|f| f.as_str()).collect();
                rows.push((entry.name.clone(), true, Some(declared), listed.join(", ")));
            }
        }
    }

    // One dry run per client with every entry of its kind.
    let mut results: BTreeMap<(String, Family), Result<Value, (String, String)>> = BTreeMap::new();
    for (families, legacy) in [(&live_families[..], false), (&legacy_families[..], true)] {
        let names: Vec<&str> = rows
            .iter()
            .filter(|r| r.1 == legacy && r.2.is_some())
            .map(|r| r.0.as_str())
            .collect();
        for family in families {
            let mut args = vec!["install"];
            args.extend(&names);
            args.extend(["-t", id_of[family].as_str(), "--dry-run"]);
            for (request, result) in outcomes(&run(&args)) {
                results.insert((request, *family), result);
            }
        }
    }

    // The matrix, then the mismatches.
    let mut matrix = String::new();
    let mut mismatches = Vec::new();
    for (name, legacy, declared, listed) in &rows {
        let families = if *legacy {
            &legacy_families[..]
        } else {
            &live_families[..]
        };
        let mut cells = Vec::new();
        for family in families {
            let Some(declared) = declared else {
                cells.push(format!("{family}:-"));
                continue;
            };
            let result = results.get(&(name.clone(), *family));
            let expected = declared.contains(family);
            let known = KNOWN_PUBLISHER_MISMATCHES
                .iter()
                .any(|(e, f, _)| e == name && *f == family.as_str());
            let cell = match (result, expected) {
                (Some(Ok(folders)), true) => {
                    let unexplained = folders["skipped"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|s| s["reason"].as_str().is_none_or(str::is_empty));
                    if unexplained {
                        Err(format!("skipped a folder without a reason: {folders}"))
                    } else {
                        Ok("✓")
                    }
                }
                (Some(Err((code, _))), false) if code == "no_compatible_release" => Ok("·"),
                (Some(Ok(_)), false) => Err("installed on a family it does not declare".to_owned()),
                (Some(Err((code, message))), _) => Err(format!("{code}: {message}")),
                (None, _) => Err("no result".to_owned()),
            };
            match cell {
                Ok(mark) => cells.push(format!("{family}:{mark}")),
                Err(_) if known => cells.push(format!("{family}:~")),
                Err(why) => {
                    cells.push(format!("{family}:✗"));
                    mismatches.push(format!(
                        "{name} on {} ({family}): declares [{listed}]; {why}",
                        id_of[family]
                    ));
                }
            }
        }
        matrix.push_str(&format!("{name:<24} {}\n", cells.join("  ")));
    }
    let _ = writeln!(std::io::stderr(), "catalog x declared flavors:\n{matrix}");
    assert!(
        mismatches.is_empty(),
        "omaddons disagrees with what publishers declare:\n{}",
        mismatches.join("\n")
    );
    assert_no_github_rest(&read_net_log(&paths(&env)));
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
