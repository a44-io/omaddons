//! Every `status` recommendation runs (the M2 contract audit): for each scenario,
//! `status --json` recommends commands in order, and each one, run on a home freshly
//! built from the same scenario (the registry stores absolute client paths, so a copied
//! home would act on the original's clients), exits 0 and keeps its promise:
//! `reinstall` clears the drift, `update` leaves nothing to update, `remove_orphans`
//! leaves no orphan and the explicit addons alone, `forget_target` forgets the client.
//! Each issue's remediation command runs the same way and clears its issue.

use std::collections::BTreeSet;

use scenarios::{
    Builder, adoptable, behind, degraded, edited, orphaned, target, unchecked, unmanaged, unplugged,
};
use serde_json::json;

/// One builder per recommendation; each builds the same home every time.
#[cfg(test)]
mod scenarios {
    use super::super::*;

    pub type Builder = fn() -> Scene;

    /// WeakAuras on retail, one file changed by hand.
    pub fn edited() -> Scene {
        let s = Scene::new();
        s.json(&["install", "weakauras", "-t", "retail"], 0);
        let toc = s.home.addons("_retail_").join("WeakAuras/WeakAuras.toc");
        fs::write(&toc, "## Interface: 120105\n## Title: edited by hand\n").unwrap();
        s
    }

    /// WeakAuras behind on retail and classic, after a check.
    pub fn behind() -> Scene {
        let s = Scene::new();
        for target in ["retail", "classic"] {
            s.json(&["install", "weakauras", "-t", target], 0);
        }
        let both = "## Interface: 120105, 50504\n## Version: 4\n";
        let wa: [Asset<'_>; 1] = [(
            "WeakAuras.zip",
            &[("mainline", 120_105), ("mists", 50_504)],
            vec![("WeakAuras/WeakAuras.toc", both)],
        )];
        let base = "https://github.com/WeakAuras/WeakAuras2";
        common::http::gh_release_v(&s.home.http, base, "5.20.0", &wa);
        common::http::gh_latest(&s.home.http, base, "5.20.0");
        s.json(&["outdated", "--refresh"], 0);
        s
    }

    /// An unmanaged folder the catalog names (Masque).
    pub fn adoptable() -> Scene {
        let s = Scene::new();
        s.dependencies();
        unmanaged_folder(&s, "Masque");
        s
    }

    /// WeakAuras installed, and the release memo gone: its status is unknown.
    pub fn unchecked() -> Scene {
        let s = Scene::new();
        s.json(&["install", "weakauras", "-t", "retail"], 0);
        fs::remove_file(s.home.env.cache().join("omaddons/releases.json")).unwrap();
        s
    }

    /// BigWigs left behind by LittleWigs' removal.
    pub fn orphaned() -> Scene {
        let s = Scene::new();
        s.dependencies();
        s.json(&["install", "littlewigs", "-t", "retail"], 0);
        s.json(&["install", "weakauras", "-t", "retail"], 0);
        s.json(&["remove", "LittleWigs", "-t", "retail"], 0);
        s
    }

    /// The classic client's directory gone after an install there.
    pub fn unplugged() -> Scene {
        let s = Scene::new();
        s.json(&["install", "weakauras", "-t", "classic"], 0);
        fs::rename(
            s.home.client("_classic_"),
            s.home.env.root().join("unplugged"),
        )
        .unwrap();
        s
    }

    /// Foo's only folder taken over with `--force` by an archive that also ships it.
    pub fn degraded() -> Scene {
        let s = Scene::new();
        s.install_foo();
        let zip = s.home.env.root().join("dl/barfoo.zip");
        let toc = "## Interface: 120105\n";
        fs::write(
            &zip,
            zip_text(&[("Bar/Bar.toc", toc), ("Foo/Foo.toc", toc)]),
        )
        .unwrap();
        let zip = zip.display().to_string();
        s.json(
            &[
                "install", "gh:o/bar", "--file", &zip, "-t", "retail", "--force",
            ],
            0,
        );
        s
    }

    /// A folder no catalog entry names.
    pub fn unmanaged() -> Scene {
        let s = Scene::new();
        unmanaged_folder(&s, "Mystery");
        s
    }

    fn unmanaged_folder(s: &Scene, name: &str) {
        let dir = s.home.addons("_retail_").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{name}.toc")), "## Interface: 120105\n").unwrap();
    }

    /// `status`'s target `id`.
    pub fn target<'a>(status: &'a Value, id: &str) -> Option<&'a Value> {
        status["data"]["targets"]
            .as_array()
            .and_then(|ts| ts.iter().find(|t| t["id"] == id))
    }
}

#[test]
fn every_status_recommendation_runs_on_a_fresh_home() {
    let scenarios: [(&str, Builder, &str); 8] = [
        ("edited", edited, "reinstall"),
        ("behind", behind, "update"),
        ("adoptable", adoptable, "adopt"),
        ("unchecked", unchecked, "check_updates"),
        ("orphaned", orphaned, "remove_orphans"),
        ("unplugged", unplugged, "forget_target"),
        ("degraded", degraded, "inspect"),
        ("unmanaged", unmanaged, "list_unmanaged"),
    ];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut fixed: BTreeSet<String> = BTreeSet::new();
    for (name, build, expected) in scenarios {
        let s = build();
        let status = s.json(&["status"], 0).envelope;
        let commands: Vec<(String, Vec<String>, bool)> = status["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["action"].as_str().unwrap().to_owned(),
                    serde_json::from_value(c["argv"].clone()).unwrap(),
                    c["destructive"].as_bool().unwrap(),
                )
            })
            .collect();
        assert!(
            commands.iter().any(|(a, ..)| a == expected),
            "{name}: no {expected} in {commands:?}"
        );
        for (action, argv, destructive) in &commands {
            seen.insert(action.clone());
            assert_eq!(
                *destructive,
                matches!(action.as_str(), "remove_orphans" | "forget_target"),
                "{name}: {action}"
            );
            let fresh = build();
            let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
            fresh.json(&args, 0);
            let after = fresh.json(&["status"], 0).envelope;
            match action.as_str() {
                "reinstall" => {
                    let id = &argv[argv.len() - 2];
                    let drift = &target(&after, id).unwrap()["drift"];
                    assert_eq!(drift, &json!([]), "{name}: {after:#}");
                }
                "update" => {
                    for t in after["data"]["targets"].as_array().unwrap() {
                        assert_eq!(t["updates"]["available"], 0, "{name}: {t:#}");
                    }
                }
                "remove_orphans" => {
                    let retail = target(&after, "retail").unwrap();
                    assert_eq!(retail["orphans"], 0, "{name}: {retail:#}");
                    let listed = fresh.json(&["list", "-t", "retail"], 0).envelope;
                    assert!(
                        listed.to_string().contains("WeakAuras"),
                        "the explicit addon stays: {listed:#}"
                    );
                }
                "forget_target" => {
                    let targets = fresh.json(&["targets"], 0).envelope;
                    let ids: Vec<&str> = targets["data"]["targets"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|t| t["id"].as_str())
                        .collect();
                    assert!(!ids.contains(&"classic"), "{name}: {ids:?}");
                }
                _ => {}
            }
        }
        // Every issue's remediation command runs too, and clears its issue unless it
        // is a look (`inspect`).
        for t in status["data"]["targets"].as_array().unwrap() {
            let id = t["id"].as_str().unwrap();
            for issue in t["issues"].as_array().unwrap() {
                let Some(argv) = issue["remediation"]["argv"].as_array() else {
                    continue;
                };
                fixed.insert(issue["id"].as_str().unwrap().to_owned());
                let args: Vec<&str> = argv[1..].iter().map(|a| a.as_str().unwrap()).collect();
                let fresh = build();
                fresh.json(&args, 0);
                let after = fresh.json(&["status"], 0).envelope;
                let left = target(&after, id).is_some_and(|t| {
                    t["issues"].as_array().unwrap().iter().any(|i| {
                        // ubs:ignore — comparing issue ids, not secrets
                        i["id"] == issue["id"] && i["addon"] == issue["addon"]
                    })
                });
                let look = issue["id"] == "fm-external_artifacts-ownership-transferred";
                assert_eq!(left, look, "{name}: {issue} after {args:?}: {after:#}");
            }
        }
    }
    let expected: BTreeSet<String> = [
        "fm-external_artifacts-managed-files-modified",
        "fm-external_artifacts-ownership-transferred",
        "fm-userland_state-target-vanished",
    ]
    .iter()
    .map(|a| (*a).to_owned())
    .collect();
    let missing: Vec<&String> = expected.difference(&fixed).collect();
    assert!(missing.is_empty(), "no remediation ran for: {missing:?}");
    let expected: BTreeSet<String> = [
        "reinstall",
        "update",
        "adopt",
        "check_updates",
        "remove_orphans",
        "forget_target",
        "inspect",
        "list_unmanaged",
    ]
    .iter()
    .map(|a| (*a).to_owned())
    .collect();
    let missing: Vec<&String> = expected.difference(&seen).collect();
    assert!(missing.is_empty(), "never recommended: {missing:?}");
}
