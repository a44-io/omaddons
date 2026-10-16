//! `omaddons install` end to end: the binary over synthetic clients in a Wine prefix,
//! GitHub releases replayed from synthetic fixtures (`OMADDONS_HTTP_FIXTURES`) and, for
//! running clients, a synthetic `/proc` (`OMADDONS_PROC_ROOT`).

mod common;

use std::fs;
use std::path::Path;

use common::addons::{addon_folder, toc};
use common::archives::zip_text;
use common::home::{Home, Run};
use common::http::{Asset, gh_release};
use common::netlog::net_log;
use common::procfs::FakeProc;
use common::trees::{CustomRoot, listing, tree_hash};
use omaddons::discover::registry::marker_path;
use serde_json::Value;
use support::{argv_of, foo_release, items, mainline, state_files, warning_codes};

#[cfg(test)]
mod support {
    use super::*;

    pub const FOO: &str = "https://github.com/o/foo";
    pub const BAR: &str = "https://github.com/o/bar";

    /// One asset of gh:o/foo whose TOC loads on `interfaces` (a TOC Interface value).
    pub fn foo_release(scenario: &Path, tag: &str, flavors: &[(&str, u32)], interfaces: &str) {
        let version = format!("## Version: {}", tag.trim_start_matches('v'));
        let toc_text = toc(interfaces, &["## Title: Foo", &version]);
        let options = toc(interfaces, &["## Title: Foo Options"]);
        let assets: [Asset<'_>; 1] = [(
            "Foo.zip",
            flavors,
            vec![
                ("Foo/Foo.toc", toc_text.as_str()),
                ("Foo/core.lua", tag),
                ("FooOptions/FooOptions.toc", options.as_str()),
            ],
        )];
        gh_release(scenario, FOO, tag, &assets);
    }

    /// Every file under `<state>/omaddons` except the request log, which records even
    /// a dry run's downloads (D13). Directories are left out: planning holds its
    /// in-flight lock under `inflight/` (vision §5.5) and removes only the file.
    pub fn state_files(home: &Home) -> Vec<String> {
        listing(&home.state())
            .lines()
            .filter(|p| !p.starts_with("net.log") && !p.is_empty())
            .filter(|p| home.state().join(p).is_file())
            .map(str::to_owned)
            .collect()
    }

    /// The envelope with its volatile values replaced, for snapshots.
    pub fn stable(envelope: &Value) -> Value {
        let mut text = envelope.to_string();
        for (value, placeholder) in [
            (&envelope["data"]["tx"], "<tx>"),
            (&envelope["data"]["target"]["key"], "<key>"),
        ] {
            if let Some(v) = value.as_str() {
                text = text.replace(v, placeholder);
            }
        }
        let mut stable: Value = serde_json::from_str(&text).unwrap();
        stable["meta"] = Value::from("<meta>");
        stable["tool_version"] = Value::from("<version>");
        stable
    }

    pub fn mainline() -> &'static [(&'static str, u32)] {
        &[("mainline", 120_105)]
    }

    pub fn items(envelope: &Value) -> &Vec<Value> {
        envelope["data"]["items"].as_array().unwrap()
    }

    pub fn warning_codes(envelope: &Value) -> Vec<&str> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w["code"].as_str())
            .collect()
    }

    pub fn argv_of(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_owned())
            .collect()
    }

    /// The only target record's file.
    pub fn record_file(home: &Home) -> std::path::PathBuf {
        fs::read_dir(home.state().join("targets"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                let name = p.file_name().unwrap().to_string_lossy();
                name.ends_with(".json") && !name.ends_with(".files.json")
            })
            .unwrap()
    }

    /// The record of addon `key` (its lowercase name).
    pub fn addon_record(home: &Home, key: &str) -> Value {
        let record: Value = serde_json::from_slice(&fs::read(record_file(home)).unwrap()).unwrap();
        record["addons"][key].clone()
    }

    /// The stderr NDJSON events of `r`.
    pub fn events(r: &Run) -> Vec<Value> {
        r.stderr
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// A zip of gh:o/foo at `version` in `dir`.
    pub fn foo_zip(dir: &Path, version: &str) -> String {
        let text = toc("120105", &[&format!("## Version: {version}")]);
        let path = dir.join(format!("Foo-{version}.zip"));
        fs::write(
            &path,
            zip_text(&[("Foo/Foo.toc", &text), ("Foo/core.lua", version)]),
        )
        .unwrap();
        path.display().to_string()
    }
}

#[test]
fn install_then_unchanged_then_update() {
    let mut home = Home::new(&["wow"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");

    let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["ok"], true, "{}", r.context());
    let tx = envelope["data"]["tx"].as_str().unwrap().to_owned();
    let item = &items(&envelope)[0];
    assert_eq!(
        (
            &item["status"],
            &item["to_version"],
            &item["version_confidence"]
        ),
        (
            &Value::from("installed"),
            &Value::from("v1.0.0"),
            &Value::from("release")
        ),
        "{}",
        r.context()
    );
    assert_eq!(envelope["data"]["kind"], "install");
    for folder in ["Foo", "FooOptions"] {
        assert!(
            home.addons("_retail_").join(folder).is_dir(),
            "{folder}\n{}",
            listing(&home.addons("_retail_"))
        );
    }
    assert_eq!(
        envelope["commands"][0]["argv"],
        serde_json::json!(["omaddons", "undo", tx]),
        "{}",
        r.context()
    );
    insta::assert_json_snapshot!("install_success", support::stable(&envelope));
    // Snapshots sort keys; the wire keeps the documented order.
    for keys in [
        &[
            "\"dry_run\"",
            "\"target\"",
            "\"tx\"",
            "\"kind\"",
            "\"items\"",
            "\"reload_required\"",
            "\"atomic_swap\"",
        ][..],
        &[
            "\"request\"",
            "\"ref\"",
            "\"name\"",
            "\"status\"",
            "\"from_version\"",
            "\"to_version\"",
            "\"version_confidence\"",
            "\"channel\"",
            "\"folders\"",
            "\"error\"",
        ][..],
    ] {
        let at: Vec<usize> = keys.iter().map(|k| r.stdout.find(k).unwrap()).collect();
        assert!(at.is_sorted(), "{keys:?} out of order\n{}", r.stdout);
    }

    // Again: nothing to do, nothing written.
    let addons = tree_hash(&home.client("_retail_"));
    let state = tree_hash(&home.state().join("targets"));
    let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(
        (&envelope["data"]["tx"], &items(&envelope)[0]["status"]),
        (&Value::Null, &Value::from("unchanged")),
        "{}",
        r.context()
    );
    assert!(envelope["commands"].as_array().is_none_or(Vec::is_empty));
    assert_eq!(tree_hash(&home.client("_retail_")), addons);
    assert_eq!(tree_hash(&home.state().join("targets")), state);
    insta::assert_json_snapshot!("install_unchanged", support::stable(&envelope));

    // A newer release: installing by the installed name updates it.
    let next = home.scenario("http-v2");
    foo_release(&next, "v2.0.0", mainline(), "120105");
    let r = home.run(&["install", "Foo", "-t", "retail"]);
    let envelope = r.expect(0);
    let item = &items(&envelope)[0];
    assert_eq!(
        (
            &envelope["data"]["kind"],
            &item["status"],
            &item["from_version"],
            &item["to_version"],
            &item["ref"]
        ),
        (
            &Value::from("update"),
            &Value::from("updated"),
            &Value::from("v1.0.0"),
            &Value::from("v2.0.0"),
            &Value::from("gh:o/foo")
        ),
        "{}",
        r.context()
    );
    let core = fs::read_to_string(home.addons("_retail_").join("Foo/core.lua")).unwrap();
    assert_eq!(core, "v2.0.0");
}

#[test]
fn the_first_install_registers_the_target() {
    let home = Home::new(&["wow"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    let client = home.client("_retail_");
    assert!(!marker_path(&client).exists());

    let envelope = home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let key = envelope["data"]["target"]["key"]
        .as_str()
        .unwrap()
        .to_owned();
    let marker = fs::read_to_string(marker_path(&client)).unwrap();
    assert!(marker.contains(&key), "{marker}");

    let targets = home.run(&["targets"]).expect(0);
    let retail = targets["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "retail")
        .unwrap()
        .clone();
    assert_eq!(retail["key"], key.as_str());

    let again = home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    assert_eq!(
        again["data"]["target"],
        serde_json::json!({"id": "retail", "key": key})
    );
}

#[test]
fn two_compatible_live_targets_are_ambiguous_and_each_fix_works() {
    let home = Home::new(&["wow", "wow_classic"]);
    foo_release(
        &home.http,
        "v1.0.0",
        &[("mainline", 120_105), ("mists", 50_504)],
        "120105, 50504",
    );
    let r = home.run(&["install", "gh:o/foo"]);
    let envelope = r.expect(3);
    let error = &envelope["error"];
    assert_eq!(error["code"], "target_ambiguous", "{}", r.context());
    assert_eq!(
        argv_of(&error["fix_argv"]),
        ["omaddons", "install", "gh:o/foo", "-t", "classic"]
    );
    let alternatives = error["alternatives"].as_array().unwrap();
    assert_eq!(alternatives.len(), 1, "{}", r.context());
    assert_eq!(
        argv_of(&alternatives[0]["argv"]),
        ["omaddons", "install", "gh:o/foo", "-t", "retail"]
    );
    insta::assert_json_snapshot!("install_ambiguous", support::stable(&envelope));
    assert!(
        !home.addons("_classic_").join("Foo").exists()
            && !home.addons("_retail_").join("Foo").exists()
    );

    let fix: Vec<String> = argv_of(&error["fix_argv"])[1..].to_vec();
    let r = home.exec(&[fix, vec!["--json".to_owned()]].concat());
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["target"]["id"],
        "classic",
        "{}",
        r.context()
    );
    assert!(home.addons("_classic_").join("Foo").is_dir());
}

#[test]
fn the_single_compatible_live_target_is_chosen() {
    let home = Home::new(&["wow", "wow_classic"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    let r = home.run(&["install", "gh:o/foo"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["target"]["id"],
        "retail",
        "{}",
        r.context()
    );
    assert!(home.addons("_retail_").join("Foo").is_dir());
    assert!(!home.addons("_classic_").join("Foo").exists());
    // The probe tried the zip on both clients and the real run installed it: one
    // download (M2-D28).
    let log = net_log(&home);
    assert_eq!(log.count_purpose("asset"), 1, "{}", log.context());
}

#[test]
fn test_clients_are_only_chosen_with_t() {
    let home = Home::new(&["wow", "wowt"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105, 120100");
    let envelope = home.run(&["install", "gh:o/foo"]).expect(0);
    assert_eq!(envelope["data"]["target"]["id"], "retail");

    let home = Home::new(&["wowt"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105, 120100");
    let r = home.run(&["install", "gh:o/foo"]);
    let envelope = r.expect(3);
    assert_eq!(
        envelope["error"]["code"],
        "target_not_found",
        "{}",
        r.context()
    );
    let fix = argv_of(&envelope["error"]["fix_argv"]);
    assert_eq!(fix, ["omaddons", "install", "gh:o/foo", "-t", "ptr"]);
    let r = home.exec(&[fix[1..].to_vec(), vec!["--json".to_owned()]].concat());
    assert_eq!(
        r.expect(0)["data"]["target"]["id"],
        "ptr",
        "{}",
        r.context()
    );
}

#[test]
fn some_items_failing_is_partial_and_strict_makes_it_exit_6() {
    let home = Home::new(&["wow"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    let vanilla = toc("11509", &["## Title: Bar"]);
    let assets: [Asset<'_>; 1] = [(
        "Bar.zip",
        &[("vanilla", 11_509)],
        vec![("Bar/Bar_Vanilla.toc", vanilla.as_str())],
    )];
    gh_release(&home.http, support::BAR, "v1", &assets);

    let r = home.run(&["install", "gh:o/foo", "gh:o/bar", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(
        (&envelope["ok"], &envelope["partial"]),
        (&Value::Bool(true), &Value::Bool(true)),
        "{}",
        r.context()
    );
    assert_eq!(items(&envelope).len(), 2, "{}", r.context());
    let (foo, bar) = (&items(&envelope)[0], &items(&envelope)[1]);
    assert_eq!(foo["status"], "installed");
    assert_eq!(
        (&bar["status"], &bar["error"]["code"]),
        (
            &Value::from("failed"),
            &Value::from("no_compatible_release")
        ),
        "{}",
        r.context()
    );
    assert!(
        warning_codes(&envelope).contains(&"no_compatible_release"),
        "{}",
        r.context()
    );
    insta::assert_json_snapshot!("install_partial", support::stable(&envelope));

    let r = home.run(&[
        "install", "gh:o/foo", "gh:o/bar", "-t", "retail", "--strict",
    ]);
    let envelope = r.expect(6);
    assert_eq!(
        (&envelope["ok"], &envelope["error"]["code"]),
        (&Value::Bool(false), &Value::from("check_failed")),
        "{}",
        r.context()
    );
    assert_eq!(items(&envelope).len(), 2, "data stays populated");
}

#[test]
fn every_item_failing_is_the_first_items_error() {
    let home = Home::new(&["wow"]);
    let vanilla = toc("11509", &[]);
    let assets: [Asset<'_>; 1] = [(
        "Bar.zip",
        &[("vanilla", 11_509)],
        vec![("Bar/Bar_Vanilla.toc", vanilla.as_str())],
    )];
    gh_release(&home.http, support::BAR, "v1", &assets);

    let r = home.run(&["install", "gh:o/bar", "gh:", "-t", "retail"]);
    let envelope = r.expect(3);
    assert_eq!(
        envelope["error"]["code"],
        "no_compatible_release",
        "{}",
        r.context()
    );
    let listed: Vec<&str> = envelope["error"]["details"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["request"].as_str().unwrap())
        .collect();
    assert_eq!(listed, ["gh:o/bar", "gh:"]);

    let r = home.run(&["install", "gh:", "gh:o/bar", "-t", "retail"]);
    let envelope = r.expect(2);
    assert_eq!(envelope["error"]["code"], "ref_invalid", "{}", r.context());
}

#[test]
fn a_dry_run_writes_nothing_and_registers_nothing() {
    let home = Home::new(&["wow"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    let client = tree_hash(&home.client("_retail_"));
    let state = state_files(&home);

    let r = home.run(&["install", "gh:o/foo", "-t", "retail", "--dry-run"]);
    let envelope = r.expect(0);
    let data = &envelope["data"];
    assert_eq!(
        (&data["dry_run"], &data["tx"], &data["target"]["key"]),
        (&Value::Bool(true), &Value::Null, &Value::Null),
        "{}",
        r.context()
    );
    let item = &items(&envelope)[0];
    assert_eq!(item["status"], "installed", "the same schema as a real run");
    assert_eq!(
        item["folders"]["added"],
        serde_json::json!(["Foo", "FooOptions"])
    );
    assert_eq!(
        tree_hash(&home.client("_retail_")),
        client,
        "the client dir is untouched"
    );
    assert_eq!(state_files(&home), state, "{}", listing(&home.state()));
    assert!(!marker_path(&home.client("_retail_")).exists());
    assert!(!home.state().join("registry.json").exists());
}

#[test]
fn a_downloaded_archive_installs_under_the_given_ref() {
    let home = Home::new(&["wow"]);
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("Foo-3.1.4.zip");
    let text = toc("120105", &["## Version: 3.1.4"]);
    fs::write(&zip, zip_text(&[("Foo/Foo.toc", &text)])).unwrap();
    let path = zip.display().to_string();

    let r = home.run(&["install", "gh:o/foo", "--file", &path, "-t", "retail"]);
    let envelope = r.expect(0);
    let item = &items(&envelope)[0];
    assert_eq!(
        (
            &item["ref"],
            &item["to_version"],
            &item["version_confidence"]
        ),
        (
            &Value::from("gh:o/foo"),
            &Value::from("3.1.4"),
            &Value::from("toc")
        ),
        "{}",
        r.context()
    );
    assert!(home.addons("_retail_").join("Foo/Foo.toc").is_file());

    let r = home.run(&["install", "gh:o/foo", "gh:o/bar", "--file", &path]);
    assert_eq!(r.expect(2)["error"]["code"], "usage", "{}", r.context());
    let r = home.run(&["install", &path, "--version", "1.0", "-t", "retail"]);
    assert_eq!(r.expect(2)["error"]["code"], "usage", "{}", r.context());
}

#[test]
fn refusals_exit_4_and_force_resolves_only_ownership_and_unmanaged() {
    let mut home = Home::new(&["wow"]);
    let addons = home.addons("_retail_");
    let retail_toc = toc("120105", &[]);
    addon_folder(
        &addons,
        "Foo",
        &[("Foo.toc", &retail_toc)],
        &[("mine.lua", b"x")],
    );
    addon_folder(&addons, "baz", &[("baz.toc", &retail_toc)], &[]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let archive = |name: &str, entries: &[(&str, &str)]| {
        let path = dl.join(name);
        fs::write(&path, zip_text(entries)).unwrap();
        path.display().to_string()
    };
    let refused = |r: &Run, code: &str| {
        let envelope = r.expect(4);
        assert_eq!(envelope["error"]["code"], code, "{}", r.context());
        assert!(
            envelope["error"]["fix"]
                .as_str()
                .is_some_and(|f| !f.is_empty()),
            "{}",
            r.context()
        );
        envelope
    };

    // unmanaged_folder: adopt it (see below), or --force replaces it.
    let envelope = refused(
        &home.run(&["install", "gh:o/foo", "-t", "retail"]),
        "unmanaged_folder",
    );
    assert_eq!(
        argv_of(&envelope["error"]["fix_argv"]),
        [
            "omaddons", "adopt", "Foo", "--ref", "gh:o/foo", "-t", "retail"
        ]
    );
    assert_eq!(
        argv_of(&envelope["error"]["alternatives"][0]["argv"])
            .last()
            .unwrap(),
        "--force"
    );
    assert!(addons.join("Foo/mine.lua").exists());
    home.run(&["install", "gh:o/foo", "-t", "retail", "--force"])
        .expect(0);
    assert!(!addons.join("Foo/mine.lua").exists());

    // ownership_conflict, then --force transfers the folder.
    let bar = toc("120105", &[]);
    let path = archive(
        "Bar.zip",
        &[("Bar/Bar.toc", &bar), ("FooOptions/FooOptions.toc", &bar)],
    );
    refused(
        &home.run(&["install", "gh:o/bar", "--file", &path, "-t", "retail"]),
        "ownership_conflict",
    );
    let r = home.run(&[
        "install", "gh:o/bar", "--file", &path, "-t", "retail", "--force",
    ]);
    let envelope = r.expect(0);
    assert!(
        warning_codes(&envelope).contains(&"ownership_transferred"),
        "{}",
        r.context()
    );

    // case_collision and protected_folder, with or without --force.
    let path = archive("Baz.zip", &[("Baz/Baz.toc", &retail_toc)]);
    for force in [false, true] {
        let mut args = vec!["install", "gh:o/baz", "--file", &path, "-t", "retail"];
        if force {
            args.push("--force");
        }
        refused(&home.run(&args), "case_collision");
    }
    let path = archive(
        "Blizz.zip",
        &[("Blizzard_AuctionUI/Blizzard_AuctionUI.toc", &retail_toc)],
    );
    for force in [false, true] {
        let mut args = vec!["install", "gh:o/blizz", "--file", &path, "-t", "retail"];
        if force {
            args.push("--force");
        }
        refused(&home.run(&args), "protected_folder");
    }
    assert!(addons.join("baz").is_dir() && !addons.join("Baz").exists());

    // wrong_flavor: an archive whose only TOC is for another client.
    let path = archive("Old.zip", &[("Old/Old_Vanilla.toc", &toc("11509", &[]))]);
    refused(
        &home.run(&["install", "gh:o/old", "--file", &path, "-t", "retail"]),
        "wrong_flavor",
    );

    // game_running: a running 3.3.5a client takes no change under the safe policy.
    let legacy = CustomRoot::at(home.env.home().join("Games/wow-335"))
        .version(3, 3, 5, 12_340)
        .build();
    let added = home.run(&[
        "targets",
        "add",
        &legacy.display().to_string(),
        "--name",
        "wotlk",
    ]);
    added.expect(0);
    let exe = legacy.join("Wow.exe").display().to_string();
    let proc = FakeProc::new(home.env.root().join("proc")).process(
        9,
        true,
        "Wow.exe",
        &["/usr/bin/wine", &exe],
        &[],
    );
    home.proc = Some(proc.root().to_path_buf());
    let path = archive("Legacy.zip", &[("Legacy/Legacy.toc", &toc("30300", &[]))]);
    let envelope = refused(
        &home.run(&["install", "gh:o/legacy", "--file", &path, "-t", "wotlk"]),
        "game_running",
    );
    assert_eq!(
        argv_of(&envelope["error"]["fix_argv"]).last().unwrap(),
        "--while-running"
    );
}

#[test]
fn installing_over_an_unmanaged_folder_offers_adopt_which_records_it_as_it_is() {
    let home = Home::new(&["wow"]);
    let addons = home.addons("_retail_");
    let retail_toc = toc("120105", &[]);
    addon_folder(
        &addons,
        "Foo",
        &[("Foo.toc", &retail_toc)],
        &[("mine.lua", b"x")],
    );
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    let tree = tree_hash(&addons);
    let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
    let fix = argv_of(&r.expect(4)["error"]["fix_argv"]);
    let r = home.exec(&[&fix[1..], &["--json".to_owned()]].concat());
    r.expect(0);
    assert_eq!(tree_hash(&addons), tree, "adopted as it is");
    let record = common::state::record(&home, "retail");
    let foo = record.addons.values().find(|a| a.name == "Foo");
    assert_eq!(
        foo.map(|a| a.r#ref.to_string()).as_deref(),
        Some("gh:o/foo"),
        "{}",
        r.context()
    );
}

#[test]
fn an_addon_of_the_same_name_from_another_ref_is_replaced_only_as_its_rule_says() {
    let home = Home::new(&["wow"]);
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = |version: &str| {
        let path = dl.join(format!("Bar-{version}.zip"));
        let toc = format!("## Interface: 120105\n## Version: {version}\n");
        fs::write(&path, zip_text(&[("Bar/Bar.toc", &toc)])).unwrap();
        path.display().to_string()
    };
    let (old, new) = (zip("2.1.0"), zip("2.2.0"));
    let addons = home.addons("_retail_");
    let bar = |home: &Home| {
        common::state::record(home, "retail")
            .addons
            .get("bar")
            .map(|a| (a.r#ref.to_string(), a.version.clone()))
    };
    home.run(&["install", &old, "-t", "retail"]).expect(0);
    let first = bar(&home);
    let tree = tree_hash(&addons);

    // A newer local archive of a local install: the same addon, updated.
    let r = home.run(&["install", &new, "-t", "retail"]);
    let envelope = r.expect(0);
    let item = &items(&envelope)[0];
    assert_eq!(
        (
            &item["status"],
            &item["from_version"],
            &item["to_version"],
            &item["folders"]["replaced"]
        ),
        (
            &Value::from("updated"),
            &Value::from("2.1.0"),
            &Value::from("2.2.0"),
            &serde_json::json!(["Bar"])
        ),
        "{}",
        r.context()
    );
    assert!(warning_codes(&envelope).is_empty(), "{}", r.context());
    assert_eq!(
        bar(&home),
        Some((format!("file:{new}"), "2.2.0".to_owned()))
    );
    home.run(&["undo"]).expect(0);
    assert_eq!(
        (bar(&home), tree_hash(&addons)),
        (first.clone(), tree.clone())
    );

    // Another (not local) ref of the same name: a takeover, only with --force.
    let takeover = [
        "install",
        "gh:demo/bar",
        "--file",
        &new,
        "-t",
        "retail",
        "--offline",
    ];
    let r = home.run(&takeover);
    let envelope = r.expect(4);
    let error = &envelope["error"];
    assert_eq!(error["code"], "ownership_conflict", "{}", r.context());
    assert_eq!(argv_of(&error["fix_argv"]).last().unwrap(), "--force");
    assert_eq!(
        argv_of(&error["alternatives"][0]["argv"]),
        ["omaddons", "remove", "Bar", "-t", "retail"]
    );
    assert_eq!(
        (bar(&home), tree_hash(&addons)),
        (first.clone(), tree.clone())
    );

    let forced: Vec<&str> = takeover.iter().copied().chain(["--force"]).collect();
    let r = home.run(&forced);
    let envelope = r.expect(0);
    let item = &items(&envelope)[0];
    assert_eq!(
        (
            &item["status"],
            &item["from_version"],
            &item["folders"]["replaced"]
        ),
        (
            &Value::from("updated"),
            &Value::from("2.1.0"),
            &serde_json::json!(["Bar"])
        ),
        "{}",
        r.context()
    );
    assert!(
        warning_codes(&envelope).contains(&"ownership_transferred"),
        "{}",
        r.context()
    );
    assert_eq!(bar(&home).map(|(r, _)| r).as_deref(), Some("gh:demo/bar"));

    // Undo restores the original record, ref and folder.
    home.run(&["undo"]).expect(0);
    assert_eq!((bar(&home), tree_hash(&addons)), (first, tree));
}

#[test]
fn a_running_client_needs_a_reload() {
    let mut home = Home::new(&["wow"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    home.retail_running();
    let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["reload_required"],
        "reload",
        "{}",
        r.context()
    );
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "reload_required");
    assert!(warning.is_some(), "{}", r.context());
    let warning = warning.unwrap();
    assert_eq!(
        (&warning["fix"], &warning["fix_argv"]),
        (&Value::from("/reload in game"), &Value::Null)
    );
}

#[test]
fn human_output_and_the_lock_log() {
    let home = Home::new(&["wow"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    let args: Vec<String> = ["install", "gh:o/foo", "-t", "retail", "-v"]
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
    let r = home.exec(&args);
    assert_eq!(r.code, 0, "{}", r.context());
    let lines: Vec<&str> = r.stdout.lines().collect();
    assert_eq!(
        lines[0],
        "retail: installed Foo v1.0.0 (2 folders)",
        "{}",
        r.context()
    );
    assert!(
        lines[1].starts_with("tx ") && lines[1].contains(" · undo: omaddons undo "),
        "{}",
        r.context()
    );
    assert!(r.stderr.contains("lock released"), "{}", r.context());
}

#[test]
fn a_catalog_addon_is_recorded_under_the_catalog_name() {
    let catalog = "schema_version = \"1.0\"\n\n[[addon]]\nname = \"FooSuite\"\naliases = [\"fs\"]\nref = \"gh:o/foo\"\n";
    for request in ["fs", "gh:o/foo", "https://github.com/o/foo"] {
        let home = Home::new(&["wow"]);
        let dir = home.env.config().join("omaddons/catalog.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("mine.toml"), catalog).unwrap();
        foo_release(&home.http, "v1.0.0", mainline(), "120105");
        let r = home.run(&["install", request, "-t", "retail"]);
        let envelope = r.expect(0);
        assert_eq!(
            (&items(&envelope)[0]["name"], &items(&envelope)[0]["ref"]),
            (&Value::from("FooSuite"), &Value::from("gh:o/foo")),
            "{}",
            r.context()
        );
    }
}

#[test]
fn verbose_adds_the_client_dir_and_asset_url() {
    let home = Home::new(&["wow"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    let r = home.run(&["install", "gh:o/foo", "-t", "retail", "--dry-run"]);
    let envelope = r.expect(0);
    assert!(
        envelope["data"].get("client_dir").is_none()
            && items(&envelope)[0].get("asset_url").is_none(),
        "token-frugal by default\n{}",
        r.context()
    );
    let r = home.run(&["install", "gh:o/foo", "-t", "retail", "-v"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["client_dir"],
        home.client("_retail_").display().to_string(),
        "{}",
        r.context()
    );
    assert_eq!(
        items(&envelope)[0]["asset_url"],
        format!("{}/releases/download/v1.0.0/Foo.zip", support::FOO)
    );
}

/// A target whose AddOns cannot be read fails every item with the reason; nothing is
/// reported unchanged.
#[test]
fn an_unreadable_addons_folder_fails_every_item() {
    let home = Home::new(&["wow"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    let bar = toc("120105", &["## Title: Bar"]);
    let assets: [Asset<'_>; 1] = [("Bar.zip", mainline(), vec![("Bar/Bar.toc", bar.as_str())])];
    gh_release(&home.http, support::BAR, "v1", &assets);
    let addons = home.addons("_retail_");
    fs::remove_dir_all(&addons).unwrap();
    fs::write(&addons, "not a folder").unwrap();

    let r = home.run(&["install", "gh:o/foo", "-t", "retail"]);
    let envelope = r.expect(1);
    assert_eq!(
        (&envelope["ok"], &envelope["error"]["code"]),
        (&Value::Bool(false), &Value::from("io")),
        "{}",
        r.context()
    );

    let r = home.run(&["install", "gh:o/foo", "gh:o/bar", "-t", "retail"]);
    let envelope = r.expect(1);
    let listed: Vec<(&str, &str)> = envelope["error"]["details"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["request"].as_str().unwrap(), i["code"].as_str().unwrap()))
        .collect();
    assert_eq!(
        listed,
        [("gh:o/foo", "io"), ("gh:o/bar", "io")],
        "{}",
        r.context()
    );
    assert_eq!(fs::read_to_string(&addons).unwrap(), "not a folder");
}

#[test]
fn a_pinned_addon_stays_until_a_version_moves_it() {
    let mut home = Home::new(&["wow"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    let r = home.run(&["install", "gh:o/foo", "--version", "v1.0.0", "-t", "retail"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope)[0]["status"],
        "installed",
        "{}",
        r.context()
    );
    assert_eq!(support::addon_record(&home, "foo")["pinned"], true);

    // A newer release: a plain install of the pinned addon changes nothing.
    let next = home.scenario("http-v2");
    foo_release(&next, "v2.0.0", mainline(), "120105");
    let tree = tree_hash(&home.client("_retail_"));
    let record = fs::read(support::record_file(&home)).unwrap();
    let requests = || {
        fs::read_to_string(home.state().join("net.log"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let before = requests();
    let r = home.run(&["install", "Foo", "-t", "retail"]);
    let envelope = r.expect(0);
    let item = &items(&envelope)[0];
    assert_eq!(
        (
            &envelope["data"]["tx"],
            &item["status"],
            &item["to_version"]
        ),
        (
            &Value::Null,
            &Value::from("unchanged"),
            &Value::from("v1.0.0")
        ),
        "{}",
        r.context()
    );
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "addon_pinned");
    assert!(warning.is_some(), "no addon_pinned\n{}", r.context());
    let warning = warning.unwrap().clone();
    assert_eq!(
        warning["message"],
        "Foo is pinned at v1.0.0 on retail; nothing was installed. Pass --version to move it"
    );
    let fix = argv_of(&warning["fix_argv"]);
    assert_eq!(fix, ["omaddons", "unpin", "Foo", "-t", "retail"]);
    let mirrored = support::events(&r)
        .into_iter()
        .find(|e| e["event"] == "warning" && e["code"] == "addon_pinned");
    assert_eq!(
        mirrored.map(|e| e["fix_argv"].clone()),
        Some(warning["fix_argv"].clone()),
        "mirrored on stderr with its argv\n{}",
        r.context()
    );
    assert_eq!(tree_hash(&home.client("_retail_")), tree, "files untouched");
    assert_eq!(
        fs::read(support::record_file(&home)).unwrap(),
        record,
        "record untouched"
    );
    assert!(before > 0, "the first install was logged");
    assert_eq!(
        requests(),
        before,
        "a pinned addon's releases are not fetched"
    );
    let r = home.exec(&[fix[1..].to_vec(), vec!["--json".to_owned()]].concat());
    r.expect(0);

    // --version moves it, and it stays pinned.
    let r = home.run(&["install", "Foo", "--version", "v2.0.0", "-t", "retail"]);
    let envelope = r.expect(0);
    let item = &items(&envelope)[0];
    assert_eq!(
        (&item["status"], &item["from_version"], &item["to_version"]),
        (
            &Value::from("updated"),
            &Value::from("v1.0.0"),
            &Value::from("v2.0.0")
        ),
        "{}",
        r.context()
    );
    let record = support::addon_record(&home, "foo");
    assert_eq!(
        (&record["pinned"], &record["version"]),
        (&Value::Bool(true), &Value::from("v2.0.0"))
    );
    let core = fs::read_to_string(home.addons("_retail_").join("Foo/core.lua")).unwrap();
    assert_eq!(core, "v2.0.0");
}

#[test]
fn the_recorded_channel_survives_an_update() {
    let home = Home::new(&["wow"]);
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let (v1, v2) = (support::foo_zip(&dl, "1.0"), support::foo_zip(&dl, "2.0"));
    let install = |zip: &str, extra: &[&str]| {
        let mut args = vec![
            "install",
            "gh:o/foo",
            "--file",
            zip,
            "-t",
            "retail",
            "--offline",
        ];
        args.extend_from_slice(extra);
        home.run(&args)
    };
    let r = install(&v1, &["--channel", "beta"]);
    assert_eq!(items(&r.expect(0))[0]["channel"], "beta", "{}", r.context());
    let r = install(&v2, &[]);
    let envelope = r.expect(0);
    assert_eq!(
        (
            &items(&envelope)[0]["status"],
            &items(&envelope)[0]["channel"]
        ),
        (&Value::from("updated"), &Value::from("beta")),
        "{}",
        r.context()
    );
    let record = support::addon_record(&home, "foo");
    assert_eq!(
        (&record["channel"], &record["pinned"], &record["version"]),
        (
            &Value::from("beta"),
            &Value::Bool(false),
            &Value::from("2.0")
        )
    );
}

#[test]
fn installing_a_dependency_by_name_makes_it_explicit() {
    let home = Home::new(&["wow"]);
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let (v1, v2) = (support::foo_zip(&dl, "1.0"), support::foo_zip(&dl, "2.0"));
    let install = |zip: &str| {
        home.exec(
            &[
                "install",
                "gh:o/foo",
                "--file",
                zip,
                "-t",
                "retail",
                "--offline",
                "--json",
                "-v",
            ]
            .map(String::from),
        )
    };
    install(&v1).expect(0);
    // Foo became a dependency of Bar (as the resolver will record it).
    let file = support::record_file(&home);
    let demote = || {
        let mut record: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        record["addons"]["foo"]["reason"] = "dependency".into();
        record["addons"]["foo"]["required_by"] = serde_json::json!(["Bar"]);
        fs::write(&file, record.to_string()).unwrap();
    };
    demote();

    // The same release: a records-only install that promotes it.
    let tree = tree_hash(&home.addons("_retail_"));
    let r = install(&v1);
    let envelope = r.expect(0);
    let item = &items(&envelope)[0];
    assert_eq!(
        (
            &envelope["data"]["kind"],
            &item["status"],
            &item["fields"],
            &item["folders"]["recorded"]
        ),
        (
            &Value::from("install"),
            &Value::from("edited"),
            &serde_json::json!(["reason"]),
            &serde_json::json!(["Foo"])
        ),
        "{}",
        r.context()
    );
    assert!(envelope["data"]["tx"].is_string(), "{}", r.context());
    let record = support::addon_record(&home, "foo");
    assert_eq!(
        (
            &record["reason"],
            &record["required_by"],
            &record["version"]
        ),
        (
            &Value::from("explicit"),
            &serde_json::json!(["Bar"]),
            &Value::from("1.0")
        )
    );
    assert_eq!(tree_hash(&home.addons("_retail_")), tree, "no folder moved");
    let again = install(&v1).expect(0);
    assert_eq!(
        (&again["data"]["tx"], &items(&again)[0]["status"]),
        (&Value::Null, &Value::from("unchanged")),
        "a repeat changes nothing"
    );

    // A newer release: installed, promoted, still required by Bar.
    demote();
    let r = install(&v2);
    assert_eq!(
        items(&r.expect(0))[0]["status"],
        "updated",
        "{}",
        r.context()
    );
    let record = support::addon_record(&home, "foo");
    assert_eq!(
        (&record["reason"], &record["required_by"]),
        (&Value::from("explicit"), &serde_json::json!(["Bar"]))
    );
}

/// `install gh:o/foo --file <zip> -t retail --offline` plus `extra`, under `--json`.
fn file_install(home: &Home, zip: &str, extra: &[&str]) -> Run {
    let mut args = vec![
        "install",
        "gh:o/foo",
        "--file",
        zip,
        "-t",
        "retail",
        "--offline",
    ];
    args.extend_from_slice(extra);
    home.run(&args)
}

#[test]
fn installing_the_installed_release_with_a_version_pins_it_as_a_pin_transaction() {
    let home = Home::new(&["wow"]);
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let v1 = support::foo_zip(&dl, "1.0");
    file_install(&home, &v1, &[]).expect(0);
    let tree = tree_hash(&home.addons("_retail_"));

    let r = file_install(&home, &v1, &["--version", "1.0"]);
    let envelope = r.expect(0);
    let item = &items(&envelope)[0];
    assert_eq!(
        (
            &envelope["data"]["kind"],
            &item["status"],
            &item["from_version"],
            &item["to_version"],
            &item["folders"]["recorded"]
        ),
        (
            &Value::from("pin"),
            &Value::from("pinned"),
            &Value::from("1.0"),
            &Value::from("1.0"),
            &serde_json::json!(["Foo"])
        ),
        "{}",
        r.context()
    );
    assert!(item.get("fields").is_none(), "{}", r.context());
    let tx = envelope["data"]["tx"].as_str().unwrap().to_owned();
    assert_eq!(
        argv_of(&envelope["commands"][0]["argv"]),
        ["omaddons", "undo", tx.as_str()]
    );
    assert_eq!(support::addon_record(&home, "foo")["pinned"], true);
    assert_eq!(tree_hash(&home.addons("_retail_")), tree, "byte-identical");
    let r = home.run(&["history", "-v"]);
    let entry = &r.expect(0)["data"]["entries"][0];
    assert_eq!(
        (&entry["kind"], &entry["ops"]),
        (
            &Value::from("pin"),
            &serde_json::json!([{"op": "record", "folder": "Foo", "addon": "Foo"}])
        ),
        "{}",
        r.context()
    );
    // Pinned already: nothing to do.
    let again = file_install(&home, &v1, &["--version", "1.0"]).expect(0);
    assert_eq!(
        (&again["data"]["tx"], &items(&again)[0]["status"]),
        (&Value::Null, &Value::from("unchanged"))
    );

    // Undo unpins it, folders untouched.
    let r = home.run(&["undo", &tx]);
    r.expect(0);
    assert_eq!(support::addon_record(&home, "foo")["pinned"], false);
    assert_eq!(tree_hash(&home.addons("_retail_")), tree);
}

#[test]
fn a_channel_switch_whose_release_is_installed_records_the_channel() {
    let home = Home::new(&["wow"]);
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let v1 = support::foo_zip(&dl, "1.0");
    file_install(&home, &v1, &[]).expect(0);

    let r = file_install(&home, &v1, &["--channel", "beta"]);
    let envelope = r.expect(0);
    let item = &items(&envelope)[0];
    assert_eq!(
        (
            &envelope["data"]["kind"],
            &item["status"],
            &item["fields"],
            &item["channel"]
        ),
        (
            &Value::from("install"),
            &Value::from("edited"),
            &serde_json::json!(["channel"]),
            &Value::from("beta")
        ),
        "{}",
        r.context()
    );
    assert_eq!(support::addon_record(&home, "foo")["channel"], "beta");
    let again = file_install(&home, &v1, &["--channel", "beta"]).expect(0);
    assert_eq!(
        (&again["data"]["tx"], &items(&again)[0]["status"]),
        (&Value::Null, &Value::from("unchanged")),
        "an immediate repeat creates no transaction"
    );
}

/// The `progress` events of a run, as `(phase, message, current, total)`.
fn progress(r: &Run) -> Vec<(String, String, u64, u64)> {
    support::events(r)
        .into_iter()
        .filter(|e| e["event"] == "progress")
        .map(|e| {
            (
                e["phase"].as_str().unwrap_or_default().to_owned(),
                e["message"].as_str().unwrap_or_default().to_owned(),
                e["current"].as_u64().unwrap_or_default(),
                e["total"].as_u64().unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn a_long_install_reports_progress_only_when_asked() {
    let home = Home::new(&["wow"]);
    foo_release(&home.http, "v1.0.0", mainline(), "120105");
    let bar = toc("120105", &["## Title: Bar"]);
    let assets: [Asset<'_>; 1] = [("Bar.zip", mainline(), vec![("Bar/Bar.toc", bar.as_str())])];
    gh_release(&home.http, support::BAR, "v1", &assets);

    let r = home.run(&[
        "install",
        "gh:o/foo",
        "gh:o/bar",
        "-t",
        "retail",
        "--progress",
        "json",
    ]);
    let envelope = r.expect(0);
    assert_eq!(envelope["data"]["kind"], "install", "{}", r.context());
    assert!(
        r.stderr
            .lines()
            .all(|l| serde_json::from_str::<Value>(l).is_ok_and(|e| e["event"].is_string())),
        "stderr is NDJSON events only\n{}",
        r.context()
    );
    let events = progress(&r);
    let phases: std::collections::BTreeSet<&str> =
        events.iter().map(|(p, ..)| p.as_str()).collect();
    assert_eq!(
        phases,
        ["commit", "download", "fetch", "stage", "verify"].into(),
        "{}",
        r.context()
    );
    for phase in &phases {
        let of: Vec<&(String, String, u64, u64)> =
            events.iter().filter(|(p, ..)| p == phase).collect();
        let total = of[0].3;
        let mut items = std::collections::BTreeSet::new();
        for (n, (_, item, current, t)) in of.iter().enumerate() {
            assert_eq!(
                (*current, *t),
                (n as u64 + 1, total),
                "{phase}: monotonic, fixed total"
            );
            assert!(*current <= total, "{phase}");
            assert!(items.insert(item.clone()), "{phase}: {item} twice");
        }
    }
    let fetched: Vec<&str> = events
        .iter()
        .filter(|(p, ..)| p == "fetch")
        .map(|(_, item, ..)| item.as_str())
        .collect();
    assert_eq!(fetched, ["gh:o/bar", "gh:o/foo"]);

    // Not asked for: --json alone, or --quiet even with --progress json.
    let next = home.run(&["remove", "Foo", "Bar", "-t", "retail"]);
    next.expect(0);
    for extra in [&[][..], &["--quiet", "--progress", "json"][..]] {
        let mut args = vec!["install", "gh:o/foo", "-t", "retail"];
        args.extend_from_slice(extra);
        let r = home.run(&args);
        r.expect(0);
        assert!(progress(&r).is_empty(), "{extra:?}\n{}", r.context());
        home.run(&["remove", "Foo", "-t", "retail"]).expect(0);
    }
    // Human output with a stderr that is not a terminal (every test run): no progress.
    let argv: Vec<String> = ["install", "gh:o/bar", "-t", "retail"]
        .map(str::to_owned)
        .to_vec();
    let r = home.exec(&argv);
    assert_eq!(r.code, 0, "{}", r.context());
    assert!(
        progress(&r).is_empty() && !r.stderr.contains('\r'),
        "{}",
        r.context()
    );
}

/// Retail with gh:o/foo v1 installed; returns the Foo folder.
fn installed_foo() -> (Home, std::path::PathBuf) {
    let home = Home::new(&["wow"]);
    foo_release(&home.http, "v1", mainline(), "120105");
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let folder = home.addons("_retail_").join("Foo");
    (home, folder)
}

#[test]
fn reinstall_repairs_from_the_blob_cache_with_no_request_and_undo_brings_the_edit_back() {
    let (home, foo) = installed_foo();
    let fresh = tree_hash(&foo);
    let before = support::addon_record(&home, "foo");
    fs::write(foo.join("core.lua"), "-- edited by hand").unwrap();
    let requests = net_log(&home).len();

    let r = home.run(&["install", "gh:o/foo", "-t", "retail", "--reinstall"]);
    let envelope = r.expect(0);
    let item = &items(&envelope)[0];
    assert_eq!(
        (
            &item["status"],
            &item["from_version"],
            &item["to_version"],
            &envelope["data"]["kind"]
        ),
        (
            &Value::from("updated"),
            &Value::from("v1"),
            &Value::from("v1"),
            &Value::from("update")
        ),
        "{}",
        r.context()
    );
    assert_eq!(
        item["folders"]["replaced"],
        serde_json::json!(["Foo", "FooOptions"])
    );
    assert_eq!(net_log(&home).len(), requests, "no request");
    assert_eq!(tree_hash(&foo), fresh, "repaired");
    let after = support::addon_record(&home, "foo");
    assert_eq!(
        (&after["release_id"], &after["blob_sha256"]),
        (&before["release_id"], &before["blob_sha256"])
    );
    assert_ne!(after["tx_id"], before["tx_id"], "restamped");

    home.run(&["undo"]).expect(0);
    assert_eq!(
        fs::read_to_string(foo.join("core.lua")).unwrap(),
        "-- edited by hand",
        "undo brings the edit back"
    );

    // Said as a reinstall from the local cache.
    let human = home.exec(&argv_of(&serde_json::json!([
        "install",
        "gh:o/foo",
        "-t",
        "retail",
        "--reinstall"
    ])));
    assert_eq!(human.code, 0, "{}", human.context());
    assert!(
        human
            .stdout
            .contains("retail: reinstalled Foo v1 from the local cache"),
        "{}",
        human.context()
    );
}

#[test]
fn reinstall_keeps_the_pin_and_the_recorded_release() {
    let (home, foo) = installed_foo();
    home.run(&["pin", "Foo"]).expect(0);
    // A newer release exists; the folder is gone.
    foo_release(&home.http, "v2", mainline(), "120105");
    fs::remove_dir_all(&foo).unwrap();
    let requests = net_log(&home).len();
    let r = home.run(&["install", "gh:o/foo", "-t", "retail", "--reinstall"]);
    let envelope = r.expect(0);
    assert_eq!(items(&envelope)[0]["to_version"], "v1", "{}", r.context());
    assert!(foo.join("Foo.toc").is_file(), "the folder is back");
    let record = support::addon_record(&home, "foo");
    assert_eq!(
        (&record["version"], &record["pinned"]),
        (&Value::from("v1"), &Value::Bool(true))
    );
    assert_eq!(net_log(&home).len(), requests, "from the cache");
}

#[test]
fn without_its_blob_a_reinstall_fetches_the_recorded_release() {
    let (home, foo) = installed_foo();
    let record = support::addon_record(&home, "foo");
    let blob = home
        .env
        .cache()
        .join("omaddons/blobs")
        .join(record["blob_sha256"].as_str().unwrap());
    fs::remove_file(&blob).unwrap();
    foo_release(&home.http, "v2", mainline(), "120105");
    fs::write(foo.join("core.lua"), "-- edited").unwrap();

    // Offline: nothing to install it from.
    let r = home.run(&[
        "install",
        "gh:o/foo",
        "-t",
        "retail",
        "--reinstall",
        "--offline",
    ]);
    let envelope = r.expect(1);
    assert_eq!(envelope["error"]["code"], "offline", "{}", r.context());
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not in the blob cache"),
        "{}",
        r.context()
    );
    assert_eq!(
        argv_of(&envelope["error"]["fix_argv"]),
        [
            "omaddons",
            "install",
            "gh:o/foo",
            "-t",
            "retail",
            "--reinstall"
        ]
    );

    let assets = net_log(&home).count_purpose("asset");
    let r = home.run(&["install", "gh:o/foo", "-t", "retail", "--reinstall"]);
    let envelope = r.expect(0);
    assert_eq!(items(&envelope)[0]["to_version"], "v1", "{}", r.context());
    assert_eq!(
        net_log(&home).count_purpose("asset"),
        assets + 1,
        "downloaded again"
    );
    assert!(blob.is_file(), "and cached again");
    assert_eq!(fs::read_to_string(foo.join("core.lua")).unwrap(), "v1");

    // Offline with the blob back: no request.
    let requests = net_log(&home).len();
    home.run(&[
        "install",
        "gh:o/foo",
        "-t",
        "retail",
        "--reinstall",
        "--offline",
    ])
    .expect(0);
    assert_eq!(net_log(&home).len(), requests);
}

#[test]
fn reinstall_needs_an_installed_addon_and_a_version_pins_it() {
    let (home, _) = installed_foo();
    let r = home.run(&["install", "gh:o/bar", "-t", "retail", "--reinstall"]);
    let envelope = r.expect(3);
    assert_eq!(envelope["error"]["code"], "not_managed", "{}", r.context());
    assert_eq!(
        argv_of(&envelope["error"]["fix_argv"]),
        ["omaddons", "install", "gh:o/bar", "-t", "retail"]
    );

    foo_release(&home.http, "v2", mainline(), "120105");
    let r = home.run(&[
        "install",
        "gh:o/foo",
        "-t",
        "retail",
        "--reinstall",
        "--version",
        "v2",
    ]);
    let envelope = r.expect(0);
    assert_eq!(items(&envelope)[0]["to_version"], "v2", "{}", r.context());
    let record = support::addon_record(&home, "foo");
    assert_eq!(
        (&record["version"], &record["pinned"]),
        (&Value::from("v2"), &Value::Bool(true))
    );
}

#[test]
fn a_reinstalled_dependency_stays_a_dependency() {
    let (home, foo) = installed_foo();
    let path = support::record_file(&home);
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["addons"]["foo"]["reason"] = Value::from("dependency");
    fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    fs::write(foo.join("core.lua"), "-- edited").unwrap();
    home.run(&["install", "gh:o/foo", "-t", "retail", "--reinstall"])
        .expect(0);
    assert_eq!(support::addon_record(&home, "foo")["reason"], "dependency");
}

#[test]
fn the_reinstall_verb_is_install_with_reinstall() {
    let (home, foo) = installed_foo();
    fs::write(foo.join("core.lua"), "-- edited").unwrap();
    let r = home.run(&["reinstall", "gh:o/foo", "-t", "retail"]);
    let envelope = r.expect(2);
    let fix = argv_of(&envelope["error"]["fix_argv"]);
    assert_eq!(
        fix,
        [
            "omaddons",
            "install",
            "gh:o/foo",
            "-t",
            "retail",
            "--reinstall"
        ],
        "{}",
        r.context()
    );
    let args: Vec<&str> = fix[1..].iter().map(String::as_str).collect();
    home.run(&args).expect(0);
    assert_eq!(fs::read_to_string(foo.join("core.lua")).unwrap(), "v1");
}

#[test]
fn a_release_that_loads_only_as_declared_is_reinstalled_from_its_source_metadata() {
    // Its publisher declares retail, and it ships a folder only Classic Era loads: the
    // cached archive alone (no rank) is refused, so the exact release is asked for, and
    // its archive still comes from the cache.
    let home = Home::new(&["wow"]);
    let foo = toc("120105", &["## Version: 1"]);
    let era = toc("11507", &[]);
    let assets: [Asset<'_>; 1] = [(
        "Foo.zip",
        mainline(),
        vec![
            ("Foo/Foo.toc", foo.as_str()),
            ("FooEra/FooEra.toc", era.as_str()),
        ],
    )];
    gh_release(&home.http, support::FOO, "v1", &assets);
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let addons = home.addons("_retail_");
    fs::write(addons.join("Foo/Foo.toc"), "-- edited").unwrap();
    let downloads = net_log(&home).count_purpose("asset");
    let requests = net_log(&home).len();

    let r = home.run(&["install", "gh:o/foo", "-t", "retail", "--reinstall"]);
    let envelope = r.expect(0);
    assert_eq!(items(&envelope)[0]["status"], "updated", "{}", r.context());
    assert_eq!(fs::read_to_string(addons.join("Foo/Foo.toc")).unwrap(), foo);
    assert_eq!(
        net_log(&home).count_purpose("asset"),
        downloads,
        "the archive came from the cache"
    );
    assert!(
        net_log(&home).len() > requests,
        "the release was asked for: the cached archive alone was refused"
    );
}
