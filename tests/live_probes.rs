//! Live probes of the keyless M1 endpoints, run at milestone checkpoints only:
//! `OMADDONS_LIVE=1 cargo test --test live_probes -- --ignored`. Each probe makes one
//! request (two for the conditional atom check) through the production stack against
//! `gh:WeakAuras/WeakAuras2` and checks raw shape only; source beads add decoder-level
//! probes here. Without `OMADDONS_LIVE=1` a probe prints `SKIP <probe>: …` and fails
//! unless `OMADDONS_ALLOW_SKIP` names it (`common::checkpoint`).

mod common;

use std::io::{Read, Write};

use std::collections::{BTreeMap, BTreeSet};

use common::env::TestEnv;
use common::http::paths;
use common::netlog::{assert_no_github_rest, read_net_log};
use common::trees::{BattlenetRoot, CustomRoot, Prefix};
use omaddons::limits::DOWNLOAD_MAX_BYTES;
use omaddons::model::{AssetLocation, Ref};
use omaddons::net::{CacheState, Http, NetConfig, NetLog, Request, stack};
use omaddons::resolve::catalog::Catalog;
use omaddons::rules::{Family, RulesTable};
use omaddons::sources::git::GitSource;
use omaddons::sources::git_hosts::GitRunner;
use omaddons::sources::github::GithubSource;
use omaddons::sources::wowace::{self, WowAceSource};
use omaddons::sources::wowi::WowiSource;
use omaddons::sources::{Source, Wanted};
use omaddons::state::paths::Paths;
use serde_json::Value;
use support::{declared_family, latest_tag, live, outcomes};

const REPO: &str = "https://github.com/WeakAuras/WeakAuras2";

#[cfg(test)]
mod support {
    use super::*;

    /// The production stack in an isolated environment, when `OMADDONS_LIVE=1`; else
    /// `test` skips (`common::checkpoint`), which fails it unless the run accepts that.
    pub fn live(test: &str) -> Option<(TestEnv, Box<dyn Http>)> {
        if !super::common::checkpoint::require(test, &["OMADDONS_LIVE"]) {
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
/// `no_compatible_release` everywhere else; a Tukui or WoWInterface entry (live) and a
/// legacy git entry likewise against their catalog `families`. Prints the matrix (✓ installs as declared, · refused as declared,
/// ✗ mismatch, ~ known publisher mismatch, - no release.json), then asserts no mismatch
/// and zero REST. A standing milestone-checkpoint check.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn catalog_matches_declared_flavors() {
    let Some((env, http)) = live("catalog_matches_declared_flavors") else {
        return;
    };
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
    // legacy git entries. `None`: no release.json (the REST fallback; skipped). A row is
    // (label, request, legacy, declared, listed): a name with variants is requested by
    // each variant's ref, so each is proven by its own TOCs against its own families.
    type Row = (String, String, bool, Option<BTreeSet<Family>>, String);
    let table = RulesTable::builtin();
    let (catalog, _) = Catalog::load(&paths(&env));
    let mut rows: Vec<Row> = Vec::new();
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
                let name = entry.name.clone();
                rows.push((name.clone(), name, false, declared, listed.join(", ")));
            }
            // Live entries whose TOCs are the only declaration (Tukui's patch[] and
            // WoWInterface's compatibility tags lag): judged by their catalog families,
            // read from those TOCs.
            Ref::Tukui { .. } | Ref::Wowi { .. } => {
                let declared: BTreeSet<Family> = entry.families.iter().copied().collect();
                let listed: Vec<&str> = entry.families.iter().map(|f| f.as_str()).collect();
                let name = entry.name.clone();
                rows.push((name.clone(), name, false, Some(declared), listed.join(", ")));
            }
            Ref::Git { .. } => {
                let declared: BTreeSet<Family> = entry.families.iter().copied().collect();
                let listed: Vec<&str> = entry.families.iter().map(|f| f.as_str()).collect();
                let name = entry.name.clone();
                rows.push((name.clone(), name, true, Some(declared), listed.join(", ")));
            }
            // WowAce variants: each by its ref, on the generation its families are of.
            Ref::WowAce { .. } => {
                let declared: BTreeSet<Family> = entry.families.iter().copied().collect();
                let listed: Vec<&str> = entry.families.iter().map(|f| f.as_str()).collect();
                rows.push((
                    entry.identity(),
                    entry.r#ref.to_string(),
                    entry.of_generation(true),
                    Some(declared),
                    listed.join(", "),
                ));
            }
            _ => {}
        }
    }

    // One dry run per client with every entry of its kind.
    let mut results: BTreeMap<(String, Family), Result<Value, (String, String)>> = BTreeMap::new();
    for (families, legacy) in [(&live_families[..], false), (&legacy_families[..], true)] {
        let names: Vec<&str> = rows
            .iter()
            .filter(|r| r.2 == legacy && r.3.is_some())
            .map(|r| r.1.as_str())
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
    for (name, request, legacy, declared, listed) in &rows {
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
            let result = results.get(&(request.clone(), *family));
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
    let Some((_env, http)) = live("latest_release_redirects_to_a_tag") else {
        return;
    };
    assert!(!latest_tag(http.as_ref()).is_empty());
}

#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn the_releases_feed_revalidates() {
    let Some((_env, http)) = live("the_releases_feed_revalidates") else {
        return;
    };
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

/// WowAce (M3-D15): BugGrabber's newest stable tag assembles at its revision within the
/// packaging subset (a new `.pkgmeta` key or keyword fails it) and its TOC names a
/// modern client; trunk at r152 is the 3.3.5a build; and the newest `BugGrabber.lua`
/// still builds entries as B3's decoders and M3-D19's session watermarks read them
/// (`message`, `session`, `time = time()`, `counter = 1`): the schema-drift tripwire.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn wowace_bug_grabber_decodes() {
    let Some((_env, http)) = live("wowace_bug_grabber_decodes") else {
        return;
    };
    let wanted = [
        Wanted::stable("wowace:bug-grabber".parse().unwrap()),
        Wanted::stable("wowace:bug-grabber#r152".parse().unwrap()),
    ];
    let sets = WowAceSource.fetch(&wanted, http.as_ref());
    let tree = |i: usize| {
        let release = sets[i].as_ref().unwrap().releases[0].clone();
        let AssetLocation::WowAceTree { base, revision } = &release.assets[0].location else {
            unreachable!("{release:?}");
        };
        let zip = wowace::assemble(http.as_ref(), base, *revision, DOWNLOAD_MAX_BYTES).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
        let mut files = BTreeMap::new();
        for n in 0..archive.len() {
            let mut file = archive.by_index(n).unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            let name = file.name().unwrap().into_owned();
            files.insert(name, String::from_utf8_lossy(&bytes).into_owned());
        }
        (release, files)
    };

    let (latest, files) = tree(0);
    let toc = &files["!BugGrabber/!BugGrabber.toc"];
    assert!(
        toc.contains(&format!("## Version: {}\n", latest.version)),
        "{toc}"
    );
    let interfaces: Vec<u32> = toc
        .lines()
        .find_map(|l| l.strip_prefix("## Interface:"))
        .unwrap()
        .split(',')
        .filter_map(|i| i.trim().parse().ok())
        .collect();
    assert!(interfaces.iter().any(|i| *i >= 120_000), "{interfaces:?}");
    let lua = &files["!BugGrabber/BugGrabber.lua"];
    for built in ["message =", "session =", "time = time()", "counter = 1"] {
        assert!(
            lua.contains(built),
            "BugGrabber.lua no longer builds {built:?}"
        );
    }

    let (_, files) = tree(1);
    assert!(
        files["!BugGrabber/!BugGrabber.toc"].contains("## Interface: 30300"),
        "{files:?}"
    );
    let _ = writeln!(
        std::io::stderr(),
        "wowace: newest {} at r{}",
        latest.release_id,
        latest.revision.unwrap_or_default()
    );
}

/// The catalog's BugGrabber legacy variants (M3-D16), re-checked on trunk: r130 is the
/// last revision declaring 2.4.3 (r131 declares 3.0) and r152 the 3.3.5a pin (r155
/// declares 4.0; r153–r154 still say 3.3 but begin the 4.0 port). The last 4.3.4 and
/// 5.4.8 revisions (r186, r203) need the packager (`@localization` keywords), as every
/// revision of those eras does, so those families have no variant.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn wowace_bug_grabber_legacy_revisions() {
    let Some((_env, http)) = live("wowace_bug_grabber_legacy_revisions") else {
        return;
    };
    let base = |rev: u32| format!("https://repos.wowace.com/wow/bug-grabber/!svn/bc/{rev}/trunk/");
    // The 1.12-era tree outside trunk, the catalog's 1.12.1 variant.
    let vanilla = |rev: u32| {
        format!("https://repos.wowace.com/wow/bug-grabber/!svn/bc/{rev}/tmp/trunk/!BugGrabber/")
    };
    let toc_interface = |at: String| -> String {
        let resp = http
            .send(Request::get(format!("{at}!BugGrabber.toc"), "probe"))
            .unwrap();
        let toc = String::from_utf8_lossy(&resp.bytes().unwrap()).into_owned();
        toc.lines()
            .find_map(|l| {
                l.trim_start_matches('\u{feff}')
                    .strip_prefix("## Interface:")
            })
            .map(|i| i.trim().to_owned())
            .unwrap_or_default()
    };
    let interface = |rev: u32| toc_interface(base(rev));
    // r43 is the last 1.12-era revision declaring 11200, and within the subset.
    assert_eq!(toc_interface(vanilla(43)), "11200", "tmp/trunk r43");
    assert_eq!(toc_interface(vanilla(44)), "20000", "tmp/trunk r44");
    assert!(
        wowace::assemble(http.as_ref(), &vanilla(43), 43, DOWNLOAD_MAX_BYTES).is_ok(),
        "tmp/trunk r43 needs the packager"
    );
    for (rev, declares) in [
        (130, "20400"),
        (131, "30000"),
        (152, "30300"),
        (155, "40000"),
        (186, "40300"),
        (187, "50001"),
        (203, "50400"),
        (204, "60000"),
    ] {
        assert_eq!(interface(rev), declares, "r{rev}");
    }
    for rev in [186, 203] {
        let error =
            wowace::assemble(http.as_ref(), &base(rev), rev, DOWNLOAD_MAX_BYTES).unwrap_err();
        let refusal = wowace::packaging_refusal(&error).unwrap_or_default();
        assert!(refusal.contains("@localization"), "r{rev}: {error:?}");
    }
}

#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn release_json_decodes() {
    let Some((_env, http)) = live("release_json_decodes") else {
        return;
    };
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
    let Some((_env, http)) = live("commits_feed_and_codeload_zip") else {
        return;
    };
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
    let Some((env, http)) = live("github_source_decodes_live") else {
        return;
    };
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
    let Some((env, _http)) = live("keyless_installs_into_a_synthetic_forever_target") else {
        return;
    };
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
    let test = "boss_mods_install_their_shared_folders_on_classic_and_forever";
    let Some((env, _http)) = live(test) else {
        return;
    };
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
    let Some((env, _http)) = live("git_hosts_resolve_and_check_out_live") else {
        return;
    };
    let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
    let net = NetConfig {
        git_hosts: vec!["codeberg.org".to_owned()],
        ..NetConfig::default()
    };
    let git = GitRunner::new(&net, NetLog::new(&paths).unwrap(), &paths);
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
    let Some((env, http)) = live("git_source_decodes_live") else {
        return;
    };
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

/// WoWInterface's `filedetails` decodes, and BugSack's file, downloaded from the CDN
/// link it gives, has exactly the MD5 it promises (M2-D11's content identity).
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn wowi_details_decode_and_the_file_has_its_md5() {
    use md5::{Digest, Md5};

    let Some((env, http)) = live("wowi_details_decode_and_the_file_has_its_md5") else {
        return;
    };
    let wanted = Wanted::stable("wowi:5995".parse().unwrap());
    let set = WowiSource
        .fetch(&[wanted], http.as_ref())
        .pop()
        .unwrap()
        .unwrap();
    let release = &set.releases[0];
    let md5 = release
        .release_id
        .strip_prefix("wowi:5995@")
        .unwrap_or_default();
    assert_eq!(md5.len(), 32, "{release:?}");
    assert!(release.notes.is_some() && release.name_hint.as_deref() == Some("BugSack"));
    let url = match &release.assets[0].location {
        AssetLocation::Url(url) => Some(url.clone()),
        _ => None,
    };
    assert!(
        url.as_deref()
            .is_some_and(|u| u.starts_with("https://cdn.wowinterface.com/")),
        "{release:?}"
    );
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
    let notes = release.notes.as_deref().unwrap_or_default();
    assert!(
        !notes.is_empty() && !omaddons::text::has_markup(notes),
        "UIChangeLog as text: {notes:?}"
    );
    let purposes: Vec<String> = read_net_log(&paths(&env))
        .into_iter()
        .map(|e| e.purpose)
        .collect();
    assert_eq!(purposes, ["wowi-details", "asset"]);
}

/// Tukui: the list decodes, `/v1/addon/elvui` hands out a token that downloads ElvUI at
/// once, its `ETag` is the MD5 of the bytes (M2-D13), and the zip holds ElvUI's three
/// folders, each with one TOC. No token reaches `net.log`.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn tukui_resolves_downloads_and_its_etag_is_the_md5() {
    use md5::{Digest, Md5};
    use omaddons::engine::inspect::{ArchiveLimits, inspect};
    use omaddons::sources::tukui::TukuiSource;

    let Some((env, http)) = live("tukui_resolves_downloads_and_its_etag_is_the_md5") else {
        return;
    };
    let source = TukuiSource::default();
    let wanted = Wanted::stable("tukui:elvui".parse().unwrap());
    let set = source
        .fetch(&[wanted], http.as_ref())
        .pop()
        .unwrap()
        .unwrap();
    let release = &set.releases[0];
    assert!(!release.assets[0].flavors.is_empty(), "{release:?}");
    let resolved = source.resolve_asset("elvui", http.as_ref()).unwrap();
    let resp = http
        .send(Request::get(resolved.url.clone(), "asset").download())
        .unwrap()
        .error_for_status("asset")
        .unwrap();
    let etag = resp.header("etag").map(|e| e.trim_matches('"').to_owned());
    let body = resp.bytes().unwrap();
    let md5: String = Md5::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(etag.as_deref(), Some(md5.as_str()), "the ETag is the MD5");

    let zip = env.root().join("elvui.zip");
    std::fs::write(&zip, &body).unwrap();
    let inspected = inspect(&zip, &ArchiveLimits::default(), &RulesTable::builtin()).unwrap();
    let mut folders: Vec<(String, usize)> = inspected
        .folders
        .iter()
        .map(|f| {
            let tocs = f
                .files
                .iter()
                .filter(|file| !file.rel_path.contains('/') && file.rel_path.ends_with(".toc"))
                .count();
            (f.name.as_str().to_owned(), tocs)
        })
        .collect();
    folders.sort();
    assert_eq!(
        folders,
        [
            ("ElvUI".to_owned(), 1),
            ("ElvUI_Libraries".to_owned(), 1),
            ("ElvUI_Options".to_owned(), 1)
        ]
    );
    let log = read_net_log(&paths(&env));
    let purposes: Vec<&str> = log.iter().map(|e| e.purpose.as_str()).collect();
    assert_eq!(purposes, ["tukui-list", "tukui-addon", "asset"]);
    // Tukui answers HEAD with 404: never sent.
    assert!(log.iter().all(|e| e.method == "GET"), "{log:#?}");
    let token = resolved.url.rsplit('/').next().unwrap();
    assert!(log.iter().all(|e| !e.path.contains(token)), "{log:#?}");
}

/// WoWInterface's file list (M2-D12): one request, gzipped on the wire, still without an
/// `ETag` or `Last-Modified` and with an hour's `max-age` (the index is cached by age
/// alone because of that; a change here is recorded on the vision bead), decoding to more
/// than 5,000 projects whose folders are sanitised.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn wowi_filelist_decodes_gzipped_and_without_validators() {
    use omaddons::model::FolderName;
    use omaddons::sources::wowi::index::{FILELIST_URL, decode_filelist};

    let test = "wowi_filelist_decodes_gzipped_and_without_validators";
    let Some((env, http)) = live(test) else {
        return;
    };
    let resp = http
        .send(Request::get(FILELIST_URL, "wowi-filelist"))
        .unwrap()
        .error_for_status("the file list")
        .unwrap();
    assert_eq!(
        (resp.header("etag"), resp.header("last-modified")),
        (None, None),
        "WoWInterface now sends validators: reconsider conditional requests (M2-D12)"
    );
    let cache_control = resp.header("cache-control").unwrap_or_default().to_owned();
    assert!(cache_control.contains("max-age=3600"), "{cache_control}");
    let body = resp.bytes().unwrap();
    let (entries, _) = decode_filelist(&body).unwrap();
    assert!(entries.len() > 5_000, "{} projects", entries.len());
    for e in &entries {
        let mut seen = BTreeSet::new();
        for dir in &e.dirs {
            let name = dir.as_str();
            let lower = name.to_ascii_lowercase();
            assert!(
                !name.is_empty()
                    && !name.starts_with('.')
                    && !["__macosx", "interface", "wtf"].contains(&lower.as_str())
                    && FolderName::new(name).is_ok()
                    && seen.insert(lower),
                "{}: {:?}",
                e.uid,
                e.dirs
            );
        }
    }
    let log = read_net_log(&paths(&env));
    let [line] = log.as_slice() else {
        unreachable!("one request: {log:#?}")
    };
    let (bytes, wire) = (line.bytes.unwrap_or(0), line.wire_bytes.unwrap_or(u64::MAX));
    assert!(
        wire < bytes / 3,
        "gzip on the wire: {wire} of {bytes} bytes"
    );
}

/// GitHub's releases feed carries each release's notes as HTML `<content>`, which a
/// changelog renders as plain text (D6), with no REST request.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn release_notes_decode_from_the_releases_feed() {
    use omaddons::limits::CHANGELOG_MAX_CHARS;
    use omaddons::text::{has_markup, html_to_text};

    let Some((env, http)) = live("release_notes_decode_from_the_releases_feed") else {
        return;
    };
    let feed = GithubSource::recent_tags("WeakAuras", "WeakAuras2", http.as_ref()).unwrap();
    let notes: Vec<String> = feed
        .tags
        .iter()
        .filter_map(|t| t.notes.as_deref())
        .map(|n| html_to_text(n, CHANGELOG_MAX_CHARS))
        .collect();
    assert!(
        !notes.is_empty(),
        "no <content> in {} entries",
        feed.tags.len()
    );
    for text in &notes {
        assert!(
            !text.trim().is_empty() && !has_markup(text),
            "notes as text: {text:?}"
        );
    }
    let log = read_net_log(&paths(&env));
    let purposes: Vec<&str> = log.iter().map(|e| e.purpose.as_str()).collect();
    assert_eq!(purposes, ["releases-atom"]);
    assert_no_github_rest(&log);
}

/// GitHub's repository search, unauthenticated: one request (10 a minute are allowed),
/// counted against the `search` budget, answering the fields the decoder reads (G1).
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn github_search_is_one_request_on_its_own_budget() {
    use omaddons::sources::github_search::GithubSearch;

    let Some((env, http)) = live("github_search_is_one_request_on_its_own_budget") else {
        return;
    };
    let resp = http
        .send(GithubSearch::with_token(None).request("weakauras"))
        .unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.header("x-ratelimit-resource"), Some("search"));
    let page: Value = serde_json::from_slice(&resp.bytes().unwrap()).unwrap();
    let items = page["items"].as_array().cloned().unwrap_or_default();
    assert!(!items.is_empty(), "{page:#}");
    for item in &items {
        assert!(
            item["name"].is_string()
                && item["owner"]["login"].is_string()
                && item["stargazers_count"].is_u64()
                && (item["pushed_at"].is_string() || item["pushed_at"].is_null())
                && (item["description"].is_string() || item["description"].is_null()),
            "{item:#}"
        );
    }
    let log = read_net_log(&paths(&env));
    let lines: Vec<(&str, &str)> = log
        .iter()
        .map(|e| (e.host.as_str(), e.purpose.as_str()))
        .collect();
    assert_eq!(lines, [("api.github.com", "github-search")]);
}

/// `outdated` over every GitHub catalog entry on three live clients (B2, B5): the first
/// run never calls the REST API; a second inside 15 minutes asks nothing; once the memo
/// is a day old, each ref whose tag did not move costs one `releases/latest` and no
/// `release.json`. The records are seeded beside one local install per client, so
/// nothing is downloaded.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn outdated_over_the_catalog_asks_little_then_nothing() {
    use omaddons::model::TargetKey;
    use omaddons::state::records::{TargetRecord, load_record};
    use omaddons::state::store::encode;

    let Some((env, _http)) = live("outdated_over_the_catalog_asks_little_then_nothing") else {
        return;
    };
    let mut root = BattlenetRoot::at(Prefix::new(&env, "battlenet").wow_root());
    for code in ["wow", "wow_classic_era", "wow_classic"] {
        root = root.product(code);
    }
    root.build();
    let run = |args: &[&str]| -> Value {
        let out = env.command().args(args).arg("--json").output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let p = paths(&env);
    let ids = |targets: &Value| -> Vec<String> {
        targets["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let targets = run(&["targets"]);
    assert_eq!(ids(&targets).len(), 3, "{targets:#}");
    let zip = env.root().join("seed.zip");
    std::fs::write(
        &zip,
        common::archives::zip_text(&[("Seed/Seed.toc", "## Interface: 11509, 50504, 120105\n")]),
    )
    .unwrap();
    let zip = zip.display().to_string();
    let (catalog, _) = Catalog::load(&p);
    let github: Vec<&omaddons::resolve::catalog::Entry> = catalog
        .entries()
        .iter()
        .filter(|e| matches!(e.r#ref, Ref::GitHub { .. }))
        .collect();
    for id in ids(&targets) {
        let seeded = run(&[
            "install",
            "gh:probe/seed",
            "--file",
            &zip,
            "-t",
            &id,
            "--offline",
        ]);
        assert_eq!(seeded["exit_code"], 0, "{seeded:#}");
    }
    // Registered now, by their first install.
    let registered = run(&["targets"]);
    for t in registered["data"]["targets"].as_array().unwrap() {
        let key: TargetKey = t["key"].as_str().unwrap().parse().unwrap();
        let mut record = load_record(&p, &key).unwrap().value;
        let seed = record.addons.values().next().unwrap().clone();
        for e in &github {
            let mut addon = seed.clone();
            addon.name = e.name.clone();
            addon.r#ref = e.r#ref.clone();
            addon.version = "0".to_owned();
            addon.release_id = Some(format!("{}@0", e.r#ref));
            addon.folders = e.folders.clone();
            record
                .addons
                .insert(TargetRecord::addon_key(&e.name), addon);
        }
        std::fs::write(p.target_record(&key), encode(&record).unwrap()).unwrap();
    }
    // Latest version per ref, as `outdated -v` lists it.
    let latest = |envelope: &Value| -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for t in envelope["data"]["targets"].as_array().unwrap() {
            for i in t["items"].as_array().unwrap() {
                if let Some(v) = i["latest"]["version"].as_str() {
                    out.insert(i["ref"].as_str().unwrap().to_owned(), v.to_owned());
                }
            }
        }
        out
    };

    let first = run(&["outdated", "-v"]);
    assert_eq!(first["exit_code"], 0, "{first:#}");
    let log = read_net_log(&p);
    assert_no_github_rest(&log);
    let after_first = log.len();
    let second = run(&["outdated", "-v"]);
    assert_eq!(second["exit_code"], 0);
    assert_eq!(
        read_net_log(&p).len(),
        after_first,
        "inside 15 minutes: no request"
    );

    // A day later, by the memo.
    let (mut memo, _) = omaddons::state::memo::load(&p);
    for entry in memo.entries.values_mut() {
        entry.fetched_at -= jiff::SignedDuration::from_hours(24);
    }
    std::fs::write(p.release_memo(), encode(&memo).unwrap()).unwrap();
    let third = run(&["outdated", "-v"]);
    assert_eq!(third["exit_code"], 0, "{third:#}");
    let log = read_net_log(&p);
    let aged = &log[after_first..];
    assert_no_github_rest(aged);
    let (before, after) = (latest(&first), latest(&third));
    let mut counts = Vec::new();
    for e in &github {
        let Ref::GitHub { owner, repo } = &e.r#ref else {
            continue;
        };
        let prefix = format!("/{owner}/{repo}/");
        let of = |purpose: &str| {
            aged.iter()
                .filter(|l| l.purpose == purpose && l.path.starts_with(&prefix))
                .count()
        };
        let moved = before.get(&e.r#ref.to_string()) != after.get(&e.r#ref.to_string());
        counts.push(format!(
            "{}: release-latest {} release-json {}{}",
            e.name,
            of("release-latest"),
            of("release-json"),
            if moved { " (moved)" } else { "" }
        ));
        assert_eq!(of("release-latest"), 1, "{}: {aged:#?}", e.name);
        if !moved {
            assert_eq!(of("release-json"), 0, "{}: {aged:#?}", e.name);
        }
    }
    let _ = writeln!(
        std::io::stderr(),
        "outdated over {} refs: first run {after_first} requests, second 0, aged {}:\n{}",
        github.len(),
        aged.len(),
        counts.join("\n")
    );
}
