//! `omaddons copy --from --to` end to end, from Classic Era to Anniversary: what loads
//! there installs in one transaction, unpinned; dependencies resolve on the destination
//! from its own TOCs; an addon already there, a local file and an addon with no tbc build
//! are skipped, never failed; a network failure makes the run partial and `--strict`
//! exit 6; named addons, `--no-deps`, `--dry-run`, undo, and the usage errors.

use crate::common;

use common::home::Home;
use common::netlog::{assert_no_github_rest, net_log};
use common::state::record;
use common::trees::tree_hash;
use omaddons::model::AddonReason;
use serde_json::{Value, json};
use support::{BOTH, era_and_anniversary, fail_latest, items, publish, triples, warned};

#[cfg(test)]
mod support {
    use std::fs;
    use std::path::Path;

    use serde_json::Value;

    use super::common::addons::toc;
    use super::common::home::Home;
    use super::common::http::{Asset, fixture, gh_latest, gh_release_v};

    /// Builds for Classic Era and Anniversary, or for Classic Era alone.
    pub const BOTH: &[(&str, u32)] = &[("classic", 11_509), ("bcc", 20_506)];
    pub const ERA_ONLY: &[(&str, u32)] = &[("classic", 11_509)];

    /// Release `tag` of `gh:o/<repo>` as its latest: one asset `<name>.zip` holding
    /// `<name>/<name>.toc` for `flavors`' interfaces.
    pub fn publish(
        scenario: &Path,
        repo: &str,
        name: &str,
        tag: &str,
        flavors: &'static [(&str, u32)],
    ) {
        let interfaces: Vec<String> = flavors.iter().map(|(_, i)| i.to_string()).collect();
        let toc = toc(&interfaces.join(", "), &[&format!("## Version: {tag}")]);
        let (file, path) = (format!("{name}.zip"), format!("{name}/{name}.toc"));
        let assets: [Asset<'_>; 1] =
            [(file.as_str(), flavors, vec![(path.as_str(), toc.as_str())])];
        let base = format!("https://github.com/o/{repo}");
        gh_release_v(scenario, &base, tag, &assets);
        gh_latest(scenario, &base, tag);
    }

    /// `gh:o/<repo>`'s `releases/latest` answers 500 from now on.
    pub fn fail_latest(scenario: &Path, repo: &str) {
        let url = format!("https://github.com/o/{repo}/releases/latest");
        for item in fs::read_dir(scenario.join("github.com")).unwrap() {
            let path = item.unwrap().path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let doc: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            // ubs:ignore — comparing fixture URLs, not secrets
            if doc["request"]["url"] == url.as_str() {
                fs::remove_file(&path).unwrap();
            }
        }
        fixture(
            scenario,
            &format!("latest-{repo}-down"),
            &url,
            500,
            &[],
            None,
        );
    }

    /// Writes `<config>/omaddons/catalog.d/50-copy.toml`: one entry per (name, repo under
    /// `o/`, folders).
    pub fn catalog(home: &Home, entries: &[(&str, &str, &[&str])]) {
        let mut text = String::from("schema_version = \"1.0\"\n");
        for (name, repo, folders) in entries {
            let folders: Vec<String> = folders.iter().map(|f| format!("{f:?}")).collect();
            text.push_str(&format!(
                "\n[[addon]]\nname = {name:?}\nref = \"gh:o/{repo}\"\nfolders = [{}]\n",
                folders.join(", ")
            ));
        }
        let dir = home.env.config().join("omaddons/catalog.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("50-copy.toml"), text).unwrap();
    }

    /// `(name, status, skip_reason)` per item of the one target.
    pub fn items(envelope: &Value) -> Vec<(String, String, Option<String>)> {
        envelope["data"]["targets"][0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["name"]
                        .as_str()
                        .or(i["request"].as_str())
                        .unwrap()
                        .to_owned(),
                    i["status"].as_str().unwrap().to_owned(),
                    i["skip_reason"].as_str().map(str::to_owned),
                )
            })
            .collect()
    }

    /// How many warnings carry `code`.
    pub fn warned(envelope: &Value, code: &str) -> usize {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] == code)
            .count()
    }

    pub fn triples(
        expected: &[(&str, &str, Option<&str>)],
    ) -> Vec<(String, String, Option<String>)> {
        expected
            .iter()
            .map(|(n, s, r)| ((*n).to_owned(), (*s).to_owned(), r.map(str::to_owned)))
            .collect()
    }

    /// Classic Era with Foo (both families), Bar (era only), Qux (also on Anniversary
    /// already), Pin (pinned at v1 while v2 is out), Req (requiring Lib, a dependency
    /// there) and Loc (a local file); Anniversary with Qux.
    pub fn era_and_anniversary() -> Home {
        use super::common::archives::zip_text;
        use super::common::graph::{Folder, Node, release_graph};
        use super::common::state::record;
        use omaddons::model::AddonReason;

        let home = Home::new(&["wow_classic_era", "wow_anniversary"]);
        publish(&home.http, "foo", "Foo", "v1", BOTH);
        publish(&home.http, "bar", "Bar", "v1", ERA_ONLY);
        publish(&home.http, "qux", "Qux", "v1", BOTH);
        publish(&home.http, "pin", "Pin", "v1", BOTH);
        let lib = [Folder {
            name: "Lib",
            interface: "11509, 20506",
            deps: &[],
            optional: &[],
        }];
        let req = [Folder {
            name: "Req",
            interface: "11509, 20506",
            deps: &["Lib"],
            optional: &[],
        }];
        release_graph(
            &home.http,
            &[
                Node {
                    base: "https://github.com/o/req",
                    tag: "v1",
                    folders: &req,
                },
                Node {
                    base: "https://github.com/o/lib",
                    tag: "v1",
                    folders: &lib,
                },
            ],
        );
        catalog(&home, &[("Lib", "lib", &["Lib"])]);
        for r in ["gh:o/foo", "gh:o/bar", "gh:o/qux", "gh:o/req"] {
            home.run(&["install", r, "-t", "era"]).expect(0);
        }
        home.run(&["install", "gh:o/pin", "--version", "v1", "-t", "era"])
            .expect(0);
        publish(&home.http, "pin", "Pin", "v2", BOTH);
        let zip = home.env.root().join("Loc.zip");
        fs::write(&zip, zip_text(&[("Loc/Loc.toc", "## Interface: 11509\n")])).unwrap();
        home.run(&["install", &format!("file:{}", zip.display()), "-t", "era"])
            .expect(0);
        home.run(&["install", "gh:o/qux", "-t", "anniversary"])
            .expect(0);
        let era = record(&home, "era");
        assert_eq!(era.addons["lib"].reason, AddonReason::Dependency);
        assert!(era.addons["pin"].pinned);
        home
    }
}

/// `copy` takes no `--force`: a refusal the engine would retry with it is fixed by the
/// install of that addon on `--to`, which runs.
#[test]
fn a_takeover_refusal_names_an_install_that_runs() {
    let home = Home::new(&["wow_classic_era", "wow_anniversary"]);
    publish(&home.http, "foo", "Foo", "v1", BOTH);
    publish(&home.http, "fork", "Foo", "v1", BOTH);
    home.run(&["install", "gh:o/foo", "-t", "era"]).expect(0);
    home.run(&["install", "gh:o/fork", "-t", "anniversary"])
        .expect(0);
    let r = home.run(&["copy", "--from", "era", "--to", "anniversary"]);
    let envelope = r.expect(4);
    assert_eq!(
        (&envelope["error"]["code"], &envelope["error"]["fix_argv"]),
        (
            &json!("ownership_conflict"),
            &json!([
                "omaddons",
                "install",
                "gh:o/foo",
                "--force",
                "-t",
                "anniversary"
            ])
        ),
        "{}",
        r.context()
    );
    let fix: Vec<String> = envelope["error"]["fix_argv"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|a| a.as_str().unwrap().to_owned())
        .collect();
    let r = home.exec(&[fix, vec!["--json".to_owned()]].concat());
    r.expect(0);
}

#[test]
fn copies_what_loads_skips_the_rest_and_undoes_byte_for_byte() {
    let home = era_and_anniversary();
    let addons = home.addons("_anniversary_");
    let before = (tree_hash(&addons), record(&home, "anniversary").addons);

    // A dry run says the same and changes nothing.
    let r = home.run(&["copy", "--from", "era", "--to", "anniversary", "--dry-run"]);
    let dry = r.expect(0);
    assert_eq!(
        dry["data"]["targets"][0]["tx"],
        Value::Null,
        "{}",
        r.context()
    );
    assert_eq!(
        (tree_hash(&addons), record(&home, "anniversary").addons),
        before
    );
    let human = home
        .exec(&["copy", "--from", "era", "--to", "anniversary", "--dry-run"].map(str::to_owned));
    assert_eq!(human.code, 0, "{}", human.context());
    insta::assert_snapshot!(
        "copy_dry_run_human",
        common::plans::redact_ulids(&human.stdout)
    );

    // Skips are not failures: --strict still exits 0.
    let r = home.run(&["copy", "--from", "era", "--to", "anniversary", "--strict"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[
            ("Bar", "skipped", Some("no_build_for_target")),
            ("Foo", "installed", None),
            ("Loc", "skipped", Some("local_source")),
            ("Pin", "installed", None),
            ("Qux", "skipped", Some("already_installed")),
            ("Req", "installed", None),
            ("Lib", "installed", None),
        ]),
        "{}",
        r.context()
    );
    assert_eq!(items(&dry), items(&envelope), "the dry run said the same");
    assert_eq!(envelope["partial"], false);
    let t = &envelope["data"]["targets"][0];
    assert_eq!(
        (&t["target"]["id"], &t["kind"]),
        (&json!("anniversary"), &json!("install"))
    );
    let lib = &t["items"][6];
    assert_eq!(
        (&lib["reason"], &lib["required_by"]),
        (&json!("dependency"), &json!(["Req"]))
    );
    let summary = &envelope["data"]["summary"];
    assert_eq!(
        (&summary["changed"], &summary["skipped"], &summary["failed"]),
        (&json!(4), &json!(3), &json!(0))
    );
    // Pins are not copied; dependency records follow by resolution.
    let after = record(&home, "anniversary");
    assert!(!after.addons["pin"].pinned);
    assert_eq!(after.addons["pin"].version, "v2");
    assert_eq!(after.addons["lib"].reason, AddonReason::Dependency);
    assert_no_github_rest(&net_log(&home).entries);

    // The one transaction undoes byte for byte.
    let tx = t["tx"].as_str().unwrap();
    let undo = &envelope["commands"][0]["argv"];
    assert_eq!(undo, &json!(["omaddons", "undo", tx]), "{envelope}");
    home.run(&["undo", tx]).expect(0);
    assert_eq!(
        (tree_hash(&addons), record(&home, "anniversary").addons),
        before
    );
}

#[test]
fn a_network_failure_is_partial_and_strict_makes_it_exit_6() {
    let home = Home::new(&["wow_classic_era", "wow_anniversary"]);
    publish(&home.http, "foo", "Foo", "v1", BOTH);
    publish(&home.http, "net", "Net", "v1", BOTH);
    for r in ["gh:o/foo", "gh:o/net"] {
        home.run(&["install", r, "-t", "era"]).expect(0);
    }
    fail_latest(&home.http, "net");
    // --strict: the same run, Foo committed, exit 6.
    let r = home.run(&["copy", "--from", "era", "--to", "anniversary", "--strict"]);
    let strict = r.expect(6);
    assert_eq!(strict["error"]["code"], "check_failed", "{}", r.context());
    let all = items(&strict);
    assert!(
        all.contains(&("Foo".into(), "installed".into(), None)),
        "{all:?}"
    );
    assert!(
        all.iter().any(|i| i.0 == "Net" && i.1 == "failed"),
        "{all:?}"
    );
    assert_eq!(record(&home, "anniversary").addons.len(), 1);
    // Without it, the same failure is a partial success.
    let r = home.run(&["copy", "--from", "era", "--to", "anniversary"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    assert_eq!(
        items(&envelope),
        triples(&[
            ("Foo", "skipped", Some("already_installed")),
            ("Net", "failed", None),
        ]),
        "{}",
        r.context()
    );
    assert!(!envelope["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn named_addons_unknown_names_and_no_deps() {
    let home = era_and_anniversary();
    // --no-deps: the requester alone, with its missing dependency said.
    let r = home.run(&[
        "copy",
        "--from",
        "era",
        "--to",
        "anniversary",
        "Req",
        "--no-deps",
    ]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[("Req", "installed", None)]),
        "{}",
        r.context()
    );
    let codes: Vec<&Value> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| &w["code"])
        .collect();
    assert_eq!(codes, [&json!("missing_dependency")], "{}", r.context());
    assert!(!record(&home, "anniversary").addons.contains_key("lib"));

    // A named dependency is copied as explicit; an unknown name fails with a fix.
    let r = home.run(&[
        "copy",
        "--from",
        "era",
        "--to",
        "anniversary",
        "lib",
        "Foo",
        "nosuch",
    ]);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    let all = items(&envelope);
    assert!(
        all.contains(&("Lib".into(), "installed".into(), None)),
        "{all:?}"
    );
    assert!(
        all.contains(&("Foo".into(), "installed".into(), None)),
        "{all:?}"
    );
    let nosuch = envelope["data"]["targets"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["request"] == "nosuch")
        .unwrap();
    assert_eq!(
        (&nosuch["error"]["code"], &nosuch["error"]["fix_argv"]),
        (
            &json!("not_managed"),
            &json!(["omaddons", "list", "-t", "era"])
        )
    );
    assert_eq!(
        record(&home, "anniversary").addons["lib"].reason,
        AddonReason::Explicit
    );
}

#[test]
fn each_side_is_one_other_client() {
    let home = era_and_anniversary();
    for args in [
        &["copy", "--from", "era", "--to", "era"][..],
        &["copy", "--from", "era", "--to", "anniversary", "-t", "era"],
    ] {
        let r = home.run(args);
        let envelope = r.expect(2);
        assert_eq!(envelope["error"]["code"], "usage", "{}", r.context());
    }

    // Two vanilla clients: --from vanilla names neither, and each fix names one.
    let home = Home::new(&["wow_classic_era", "wow_anniversary"]);
    common::trees::CustomRoot::at(home.env.home().join("Games/era-custom"))
        .version(1, 15, 9, 60_003)
        .build();
    home.run(&["targets", "rename", "vanilla", "custom"])
        .expect(0);
    let r = home.run(&["copy", "--from", "vanilla", "--to", "anniversary"]);
    let envelope = r.expect(3);
    let error = &envelope["error"];
    assert_eq!(error["code"], "target_ambiguous", "{}", r.context());
    let mut fixes = vec![error["fix_argv"].clone()];
    fixes.extend(
        error["alternatives"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["argv"].clone()),
    );
    assert_eq!(
        fixes,
        [
            json!(["omaddons", "copy", "--from", "era", "--to", "anniversary"]),
            json!([
                "omaddons",
                "copy",
                "--from",
                "custom",
                "--to",
                "anniversary"
            ]),
        ],
        "{}",
        r.context()
    );
}

/// Only `--to`'s client matters: copying from a running client needs nothing, and
/// copying onto one installs (copy only adds) and asks for a reload. Copy never
/// replaces what `--to` has (an addon there is skipped), so it cannot drop a loaded TOC.
#[test]
fn only_the_destination_s_running_client_matters() {
    let mut home = era_and_anniversary();
    let addons = home.addons("_anniversary_");

    let running = |home: &Home| -> Vec<(String, Value)> {
        home.run(&["status"]).expect(0)["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["id"].as_str().unwrap().to_owned(), t["running"].clone()))
            .collect()
    };
    home.client_running("_classic_era_", "WowClassic.exe");
    assert_eq!(
        running(&home),
        [
            ("anniversary".to_owned(), json!(false)),
            ("era".to_owned(), json!(true))
        ]
    );
    let r = home.run(&["copy", "--from", "era", "--to", "anniversary", "Foo"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[("Foo", "installed", None)]),
        "{}",
        r.context()
    );
    assert_eq!(envelope["data"]["targets"][0]["reload_required"], "none");
    assert_eq!(warned(&envelope, "reload_required"), 0, "{}", r.context());

    home.client_running("_anniversary_", "WowClassic.exe");
    let r = home.run(&["copy", "--from", "era", "--to", "anniversary", "Pin"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[("Pin", "installed", None)]),
        "{}",
        r.context()
    );
    let t = &envelope["data"]["targets"][0];
    assert!(t["tx"].is_string(), "{t}");
    assert_eq!(t["reload_required"], "reload");
    assert_eq!(warned(&envelope, "reload_required"), 1, "{}", r.context());
    assert!(addons.join("Pin").is_dir());
}

/// A process table omaddons cannot read counts `--to` as running: the copy installs,
/// asks for a reload, and says once that it could not tell.
#[test]
fn copy_counts_the_destination_as_running_when_the_process_table_cannot_be_read() {
    use std::os::unix::fs::PermissionsExt;

    if common::procfs::running_as_root() {
        return; // chmod 000 hides nothing from root
    }
    let home = era_and_anniversary();
    let locked = home.env.root().join("proc-locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let argv = [
        "copy",
        "--from",
        "era",
        "--to",
        "anniversary",
        "Foo",
        "--json",
    ]
    .map(str::to_owned);
    let r = home.exec_with(&argv, &[("OMADDONS_PROC_ROOT", locked.to_str().unwrap())]);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[("Foo", "installed", None)]),
        "{}",
        r.context()
    );
    assert_eq!(envelope["data"]["targets"][0]["reload_required"], "reload");
    assert_eq!(warned(&envelope, "running_unknown"), 1, "{}", r.context());
}
