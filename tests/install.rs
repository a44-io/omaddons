//! `omaddons install` end to end: the binary over synthetic clients in a Wine prefix,
//! GitHub releases replayed from synthetic fixtures (`OMADDONS_HTTP_FIXTURES`) and, for
//! running clients, a synthetic `/proc` (`OMADDONS_PROC_ROOT`).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::addons::{addon_folder, toc};
use common::archives::zip_text;
use common::env::TestEnv;
use common::http::{Asset, gh_release};
use common::procfs::FakeProc;
use common::trees::{BattlenetRoot, CustomRoot, Prefix, listing, tree_hash};
use omaddons::discover::registry::marker_path;
use serde_json::Value;
use support::{Home, Run, argv_of, foo_release, items, mainline, state_files, warning_codes};

#[cfg(test)]
mod support {
    use super::*;

    pub const FOO: &str = "https://github.com/o/foo";
    pub const BAR: &str = "https://github.com/o/bar";

    /// Synthetic clients in one Wine prefix, a fixture scenario and, optionally, a
    /// fake `/proc`.
    pub struct Home {
        pub env: TestEnv,
        pub root: PathBuf,
        pub prefix: PathBuf,
        pub http: PathBuf,
        pub proc: Option<PathBuf>,
    }

    impl Home {
        /// A Battle.net root holding `products` (product codes).
        pub fn new(products: &[&str]) -> Self {
            let env = TestEnv::new();
            let prefix = Prefix::new(&env, "battlenet");
            let mut root = BattlenetRoot::at(prefix.wow_root());
            for code in products {
                root = root.product(code);
            }
            let root = root.build();
            let http = env.root().join("http");
            fs::create_dir_all(&http).unwrap();
            Self {
                root,
                prefix: prefix.path().to_path_buf(),
                http,
                proc: None,
                env,
            }
        }

        /// A product's client dir (`_retail_`, `_classic_`, …).
        pub fn client(&self, folder: &str) -> PathBuf {
            self.root.join(folder)
        }

        pub fn addons(&self, folder: &str) -> PathBuf {
            self.client(folder).join("Interface/AddOns")
        }

        /// `<state>/omaddons`.
        pub fn state(&self) -> PathBuf {
            self.env.state().join("omaddons")
        }

        /// A fresh fixture scenario; later runs replay it.
        pub fn scenario(&mut self, name: &str) -> PathBuf {
            self.http = self.env.root().join(name);
            fs::create_dir_all(&self.http).unwrap();
            self.http.clone()
        }

        /// A retail client process in the fake `/proc`.
        pub fn retail_running(&mut self) {
            let prefix = self.prefix.display().to_string();
            let proc = FakeProc::new(self.env.root().join("proc")).process(
                4242,
                true,
                "Wow.exe",
                &["C:\\Program Files (x86)\\World of Warcraft\\_retail_\\Wow.exe"],
                &[("WINEPREFIX", &prefix)],
            );
            self.proc = Some(proc.root().to_path_buf());
        }

        /// Runs `omaddons <args> --json`.
        pub fn run(&self, args: &[&str]) -> Run {
            let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            argv.push("--json".to_owned());
            self.exec(&argv)
        }

        /// Runs `omaddons <args>` verbatim.
        pub fn exec(&self, args: &[String]) -> Run {
            let mut cmd = self.env.command();
            cmd.env("OMADDONS_HTTP_FIXTURES", &self.http);
            if let Some(proc) = &self.proc {
                cmd.env("OMADDONS_PROC_ROOT", proc);
            }
            let output = cmd.args(args).output().unwrap();
            Run {
                argv: args.to_vec(),
                code: output.status.code().unwrap(),
                stdout: String::from_utf8(output.stdout).unwrap(),
                stderr: String::from_utf8(output.stderr).unwrap(),
            }
        }
    }

    pub struct Run {
        pub argv: Vec<String>,
        pub code: i32,
        pub stdout: String,
        pub stderr: String,
    }

    impl Run {
        /// The one stdout envelope; every stderr line must be NDJSON.
        pub fn envelope(&self) -> Value {
            assert_eq!(
                self.stdout.lines().count(),
                1,
                "one envelope\n{}",
                self.context()
            );
            for line in self.stderr.lines() {
                assert!(
                    serde_json::from_str::<Value>(line).is_ok(),
                    "stderr is NDJSON\n{}",
                    self.context()
                );
            }
            serde_json::from_str(&self.stdout).unwrap()
        }

        /// The envelope, asserting the exit code.
        pub fn expect(&self, code: i32) -> Value {
            assert_eq!(self.code, code, "{}", self.context());
            self.envelope()
        }

        pub fn context(&self) -> String {
            format!(
                "argv: {:?}\nexit: {}\nstdout: {}\nstderr: {}",
                self.argv, self.code, self.stdout, self.stderr
            )
        }
    }

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

    // unmanaged_folder, then --force replaces it.
    let envelope = refused(
        &home.run(&["install", "gh:o/foo", "-t", "retail"]),
        "unmanaged_folder",
    );
    assert_eq!(
        argv_of(&envelope["error"]["fix_argv"]).last().unwrap(),
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
