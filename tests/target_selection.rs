//! e2e suite: target selection: family and mode fan-out, -t all, the reserved id all.
//!
//! `-t` across several clients (M2-D24): read commands fan a family or mode selector out
//! to every matching client, while `install` still refuses to guess one. `-t all`
//! (M4-D13) selects every client, test clients included; a single-target command asks
//! which; a client already named `all` wins with `target_id_reserved`; `all` cannot be a
//! new id.

use crate::common;

use std::fs;
use std::path::Path;

use common::env::TestEnv;
use common::home::Home;
use common::tlog::{Ran, TestLog, tlog};
use common::trees::{BattlenetRoot, CustomRoot, Prefix};
use serde_json::Value;
use support::{
    foo_zip, ids, json, rewrite_id, selected, three_clients, two_vanilla, warnings_coded,
};

#[cfg(test)]
mod support {
    use super::*;

    /// `args` and `--json`.
    pub fn json(args: &[&str]) -> Vec<String> {
        args.iter()
            .copied()
            .chain(["--json"])
            .map(str::to_owned)
            .collect()
    }

    /// A `Foo` zip under `root`'s `dl/` that loads on retail, ptr and era; its path.
    pub fn foo_zip(root: &Path) -> String {
        let dl = root.join("dl");
        fs::create_dir_all(&dl).unwrap();
        let zip = dl.join("Foo.zip");
        let toc = "## Interface: 120105, 120100, 11509\n";
        fs::write(&zip, common::archives::zip_text(&[("Foo/Foo.toc", toc)])).unwrap();
        zip.display().to_string()
    }

    /// `retail`, the test client `ptr` and `era`, each registered and managing `Foo`.
    pub fn three_clients(log: &TestLog) -> Home {
        let home = Home::new(&["wow", "wowt", "wow_classic_era"]);
        log.env(&home);
        let zip = foo_zip(home.env.root());
        for id in ["retail", "ptr", "era"] {
            log.run(
                &home,
                &json(&["install", "gh:o/foo", "--file", &zip, "-t", id]),
            )
            .ok();
        }
        let targets = log.run(&home, &json(&["targets"]));
        log.json("targets", targets.ok().data());
        home
    }

    /// The ids a command covered, wherever its data keeps them: `data.targets` as ids,
    /// as objects with an `id`, or with a `target.id`; else `data.installed[].target.id`.
    pub fn selected(ran: &Ran) -> Vec<String> {
        let data = ran.data();
        let list = data["targets"]
            .as_array()
            .or_else(|| data["installed"].as_array())
            .cloned()
            .unwrap_or_default();
        list.iter()
            .map(|v| {
                v.as_str()
                    .or_else(|| v["id"].as_str())
                    .or_else(|| v["target"]["id"].as_str())
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    /// The envelope's warnings with `code`.
    pub fn warnings_coded(ran: &Ran, code: &str) -> Vec<Value> {
        ran.envelope()["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|w| w["code"] == code)
            .cloned()
            .collect()
    }

    /// Rewrites the registered id `from` to `to` in `registry.json`, as a build that
    /// accepted it would have left it (test code writing a fixture).
    pub fn rewrite_id(log: &TestLog, home: &Home, from: &str, to: &str) {
        let path = home.state().join("registry.json");
        let mut doc: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        log.json("registry before", &doc);
        let mut found = false;
        for target in doc["targets"].as_array_mut().unwrap() {
            if target["id"] == from {
                target["id"] = Value::from(to);
                found = true;
            }
        }
        assert!(found, "no registered {from}: {doc}");
        log.json("registry after", &doc);
        fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    }

    /// A Battle.net Classic Era client and a 1.15.x custom client: two vanilla clients.
    pub fn two_vanilla() -> Home {
        let home = Home::new(&["wow", "wow_classic_era"]);
        CustomRoot::at(home.env.home().join("Games/era-custom"))
            .version(1, 15, 9, 60_003)
            .build();
        // Its default id, `vanilla`, would win over the family name: rename it.
        home.run(&["targets", "rename", "vanilla", "custom"])
            .expect(0);
        let targets = home.run(&["targets"]).expect(0);
        let vanilla: Vec<&str> = targets["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["family"] == "vanilla")
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert_eq!(vanilla.len(), 2, "{targets}");
        home
    }

    /// The `id`s of `values`, where `field` names the id within each.
    pub fn ids(values: &Value, field: &str) -> Vec<String> {
        values
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v[field].as_str().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn read_commands_cover_every_client_a_family_names() {
    let home = two_vanilla();
    let r = home.run(&["status", "-t", "vanilla"]);
    let status = r.expect(0);
    let covered = ids(&status["data"]["targets"], "id");
    assert_eq!(covered.len(), 2, "{}", r.context());
    assert!(covered.contains(&"era".to_owned()), "{covered:?}");
    let mut sorted = covered.clone();
    sorted.sort();
    assert_eq!(covered, sorted, "sorted by id");

    let r = home.run(&["list", "-t", "vanilla"]);
    let list = r.expect(0);
    assert_eq!(
        ids(&list["data"]["targets"], "id"),
        covered,
        "{}",
        r.context()
    );
    let r = home.run(&["list", "-t", "mainline"]);
    assert_eq!(
        ids(&r.expect(0)["data"]["targets"], "id"),
        ["retail"],
        "{}",
        r.context()
    );

    // A single target reads exactly as before.
    let r = home.run(&["status", "-t", "retail"]);
    assert_eq!(ids(&r.expect(0)["data"]["targets"], "id"), ["retail"]);
    // A mode selects the client running it.
    let r = home.run(&["status", "-t", "sod"]);
    assert_eq!(
        ids(&r.expect(0)["data"]["targets"], "id"),
        ["era"],
        "{}",
        r.context()
    );
    // Nothing matching: the same error as before.
    let r = home.run(&["status", "-t", "nosuch"]);
    assert_eq!(r.expect(3)["error"]["code"], "target_not_found");
}

#[test]
fn info_covers_every_client_a_family_names() {
    let home = two_vanilla();
    let dl = home.env.root().join("dl");
    std::fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("Foo.zip");
    let toc = "## Interface: 11509\n## Version: 1\n";
    std::fs::write(&zip, common::archives::zip_text(&[("Foo/Foo.toc", toc)])).unwrap();
    let path = zip.display().to_string();
    let status = home.run(&["status", "-t", "vanilla"]).expect(0);
    for id in ids(&status["data"]["targets"], "id") {
        let r = home.run(&["install", "gh:o/foo", "--file", &path, "-t", &id]);
        r.expect(0);
    }
    let r = home.run(&["info", "gh:o/foo", "-t", "vanilla", "--offline"]);
    let envelope = r.expect(0);
    let installed: Vec<String> = envelope["data"]["installed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["target"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(installed.len(), 2, "{}", r.context());
}

#[test]
fn install_still_refuses_to_guess_among_a_familys_clients() {
    let home = two_vanilla();
    let dl = home.env.root().join("dl");
    std::fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("Foo.zip");
    let toc = "## Interface: 11509\n";
    std::fs::write(&zip, common::archives::zip_text(&[("Foo/Foo.toc", toc)])).unwrap();
    let path = zip.display().to_string();
    let r = home.run(&["install", "gh:o/foo", "--file", &path, "-t", "vanilla"]);
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
    assert_eq!(fixes.len(), 2, "one fix per candidate\n{}", r.context());
    for fix in fixes {
        let argv: Vec<String> = fix
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .map(|a| a.as_str().unwrap().to_owned())
            .chain(["--json".to_owned()])
            .collect();
        let run = home.exec(&argv);
        assert_eq!(run.code, 0, "{}", run.context());
    }
}

// ---- -t all (M4-D13) ----

const ALL_THREE: [&str; 3] = ["era", "ptr", "retail"];

fn owned(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

#[test]
fn all_selects_every_client_test_clients_included() {
    let log = tlog!();
    log.step("home");
    let home = three_clients(&log);

    log.step("every command that takes several");
    for command in [
        &["status"][..],
        &["list"],
        &["outdated", "--offline"],
        &["info", "gh:o/foo", "--offline"],
    ] {
        let label = command.join(" ");
        let all = log.run(&home, &json(&[command, &["-t", "all"]].concat()));
        let bare = log.run(&home, &json(command));
        let (all, bare) = (selected(all.ok()), selected(bare.ok()));
        log.assert_eq(&format!("{label} -t all"), &all, &owned(&ALL_THREE));
        log.assert_eq(&format!("{label}: as without -t"), &all, &bare);
    }
    let all = log.run(&home, &json(&["export", "-t", "all"]));
    let bare = log.run(&home, &json(&["export"]));
    let pick = |ran: &Ran| {
        let data = ran.ok().data();
        (data["targets"].clone(), data["entries"].clone())
    };
    log.assert_eq("export: as without -t", &pick(&all), &pick(&bare));

    log.step("every spelling");
    for flag in [&["-t", "ALL"][..], &["--target=All"], &["--flavor", "all"]] {
        let ran = log.run(&home, &json(&[&["status"][..], flag].concat()));
        log.assert_eq(&flag.join(" "), &selected(ran.ok()), &owned(&ALL_THREE));
    }

    log.step("a default that leaves clients out");
    let bare = log.run(&home, &json(&["backup", "list"]));
    log.assert_eq("backup list", &selected(bare.ok()), &Vec::<String>::new());
    let all = log.run(&home, &json(&["backup", "list", "-t", "all"]));
    log.assert_eq(
        "backup list -t all",
        &selected(all.ok()),
        &owned(&ALL_THREE),
    );
}

#[test]
fn update_all_says_how_many_are_test_clients() {
    let log = tlog!();
    log.step("home");
    let home = three_clients(&log);
    let line = "-t all selects 3 clients, including 1 test client (ptr)";

    log.step("dry run, human");
    let ran = log.run(&home, &["update", "-t", "all", "--dry-run"]);
    ran.exit(0);
    let stdout = ran.stdout();
    log.assert_eq("the first line", &stdout.lines().next(), &Some(line));
    for id in ALL_THREE {
        let later = stdout.lines().skip(1).any(|l| l.contains(id));
        assert!(later, "{id} is in no later line\n{stdout}");
    }

    log.step("dry run, json");
    let all = log.run(&home, &json(&["update", "-t", "all", "--dry-run"]));
    let bare = log.run(&home, &json(&["update", "--dry-run"]));
    log.assert_eq("update -t all", &selected(all.ok()), &owned(&ALL_THREE));
    let keys = |ran: &Ran| -> Vec<String> {
        ran.data()
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    log.assert_eq("the same data keys", &keys(&all), &keys(bare.ok()));
    log.assert_eq(
        "bare update selects them too",
        &selected(&bare),
        &selected(&all),
    );
    log.note(
        "bare update already covers every client at c0ab5e9; -t all selects the same ones and says how many are test clients",
    );

    log.step("real run");
    for (label, folder) in [
        ("retail", "_retail_"),
        ("ptr", "_ptr_"),
        ("era", "_classic_era_"),
    ] {
        log.tree(label, &home.client(folder));
    }
    let ran = log.run(&home, &["update", "-t", "all"]);
    ran.exit(0);
    log.assert_eq("the first line", &ran.stdout().lines().next(), &Some(line));
    for (label, folder) in [
        ("retail", "_retail_"),
        ("ptr", "_ptr_"),
        ("era", "_classic_era_"),
    ] {
        log.assert_unchanged(label, &home.client(folder));
    }

    log.step("no line without the keyword");
    let ran = log.run(&home, &["update", "-t", "era"]);
    ran.exit(0);
    let first = ran.stdout().lines().next().unwrap_or_default();
    assert!(!first.starts_with("-t all"), "{first}");
}

/// `<state>`, `<config>` and the three clients of [`three_clients`].
fn three_roots(home: &Home) -> Vec<(&'static str, std::path::PathBuf)> {
    vec![
        ("state", home.env.state()),
        ("config", home.env.config()),
        ("retail", home.client("_retail_")),
        ("ptr", home.client("_ptr_")),
        ("era", home.client("_classic_era_")),
    ]
}

#[test]
fn single_target_commands_refuse_all_with_one_fix_per_client() {
    let log = tlog!();
    log.step("home");
    let home = three_clients(&log);
    let zip = foo_zip(home.env.root());
    for (label, root) in three_roots(&home) {
        log.tree(label, &root);
    }

    log.step("install");
    let ran = log.run(
        &home,
        &json(&["install", "gh:o/foo", "--file", &zip, "-t", "all"]),
    );
    ran.fails(3, "target_ambiguous");
    let error = &ran.envelope()["error"];
    log.assert_eq(
        "candidates",
        &error["details"]["candidates"],
        &Value::from(ALL_THREE.to_vec()),
    );
    let mut fixes = vec![error["fix_argv"].clone()];
    fixes.extend(
        error["alternatives"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["argv"].clone()),
    );
    let tails: Vec<String> = fixes
        .iter()
        .map(|f| {
            let argv = ids_of(f);
            argv[argv.len() - 2..].join(" ")
        })
        .collect();
    log.assert_eq(
        "each fix's -t",
        &tails,
        &owned(&["-t era", "-t ptr", "-t retail"]),
    );
    for (label, root) in three_roots(&home) {
        log.assert_unchanged(label, &root);
    }

    log.step("every fix runs");
    for fix in &fixes {
        let argv: Vec<String> = ids_of(fix).into_iter().skip(1).collect();
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        log.run(&home, &json(&argv)).ok();
    }
    for (label, root) in three_roots(&home) {
        log.tree(label, &root);
    }

    log.step("other single-target commands");
    for command in [
        &["remove", "foo", "-t", "all"][..],
        &["history", "-t", "all"],
        &["undo", "-t", "all", "--dry-run"],
    ] {
        let ran = log.run(&home, &json(command));
        ran.fails(3, "target_ambiguous");
        log.assert_eq(
            &command.join(" "),
            &ran.envelope()["error"]["details"]["candidates"],
            &Value::from(ALL_THREE.to_vec()),
        );
    }
    for (label, root) in three_roots(&home) {
        log.assert_unchanged(label, &root);
    }

    log.step("one client");
    let one = Home::new(&["wow"]);
    let zip = foo_zip(one.env.root());
    log.run(
        &one,
        &json(&["install", "gh:o/foo", "--file", &zip, "-t", "all"]),
    )
    .ok();
    assert!(one.addons("_retail_").join("Foo").is_dir(), "not on retail");

    log.step("no client");
    let none = TestEnv::new();
    let all = log.run(&none, &json(&["status", "-t", "all"]));
    let bare = log.run(&none, &json(&["status"]));
    log.assert_eq("status -t all", &selected(all.ok()), &selected(bare.ok()));
    log.assert_eq("no target", &selected(&all), &Vec::<String>::new());
    let zip = foo_zip(none.root());
    let ran = log.run(
        &none,
        &json(&["install", "gh:o/foo", "--file", &zip, "-t", "all"]),
    );
    ran.fails(3, "target_not_found");
    let error = &ran.envelope()["error"];
    log.assert_eq(
        "selector",
        &error["details"]["selector"],
        &Value::from("all"),
    );
    log.assert_eq(
        "fix",
        &error["fix_argv"],
        &Value::from(vec!["omaddons", "targets"]),
    );
}

/// The strings of a JSON array (none for anything else).
fn ids_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| v.as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Exactly one `target_id_reserved`, with its message and advice, and no argv.
#[track_caller]
fn reserved_once(log: &TestLog, ran: &Ran) {
    let warned = warnings_coded(ran, "target_id_reserved");
    log.assert_eq("target_id_reserved", &warned.len(), &1);
    let warning = &warned[0];
    log.assert_eq(
        "message",
        &warning["message"],
        &Value::from(
            "a client has the id \"all\", so -t all selected that client, not every client",
        ),
    );
    let fix = warning["fix"].as_str().unwrap_or_default();
    assert!(
        fix.contains("omaddons targets rename all <new id>"),
        "{fix}"
    );
    assert!(warning["fix_argv"].is_null(), "{warning}");
}

#[test]
fn a_target_named_all_wins_with_a_warning() {
    let log = tlog!();
    log.step("home");
    let home = Home::new(&["wow", "wow_classic_era"]);
    log.env(&home);
    log.run(&home, &json(&["targets", "default", "retail"]))
        .ok();
    log.run(&home, &json(&["targets", "default", "era"])).ok();
    rewrite_id(&log, &home, "era", "all");

    log.step("the id wins");
    let ran = log.run(&home, &json(&["status", "-t", "all"]));
    log.assert_eq("status -t all", &selected(ran.ok()), &owned(&["all"]));
    reserved_once(&log, &ran);

    log.step("quiet without the keyword");
    let ran = log.run(&home, &json(&["status"]));
    log.assert_eq("status", &selected(ran.ok()), &owned(&["all", "retail"]));
    assert!(warnings_coded(&ran, "target_id_reserved").is_empty());
    let ran = log.run(&home, &json(&["status", "-t", "retail"]));
    assert!(warnings_coded(ran.ok(), "target_id_reserved").is_empty());

    log.step("a single-target command takes it");
    let zip = foo_zip(home.env.root());
    let ran = log.run(
        &home,
        &json(&["install", "gh:o/foo", "--file", &zip, "-t", "ALL"]),
    );
    ran.ok();
    reserved_once(&log, &ran);
    assert!(
        home.addons("_classic_era_").join("Foo").is_dir(),
        "not on era"
    );
    assert!(!home.addons("_retail_").join("Foo").exists(), "on retail");

    log.step("rename restores the keyword");
    log.run(&home, &json(&["targets", "rename", "all", "era2"]))
        .ok();
    let ran = log.run(&home, &json(&["status", "-t", "all"]));
    log.assert_eq(
        "status -t all",
        &selected(ran.ok()),
        &owned(&["era2", "retail"]),
    );
    assert!(warnings_coded(&ran, "target_id_reserved").is_empty());

    log.step("a flavor folder named _all_");
    let env = TestEnv::new();
    let prefix = Prefix::new(&env, "battlenet");
    BattlenetRoot::at(prefix.wow_root())
        .product("wow")
        .unknown_folder("_all_", None, None)
        .build();
    let targets = log.run(&env, &json(&["targets"]));
    let known = selected(targets.ok());
    assert!(
        known.contains(&"all".to_owned()),
        "the tree builder's _all_ folder became no target named all: {known:?}"
    );
    let ran = log.run(&env, &json(&["status", "-t", "all"]));
    log.assert_eq("status -t all", &selected(ran.ok()), &owned(&["all"]));
    reserved_once(&log, &ran);

    log.step("a registered all whose directory is gone");
    rewrite_id(&log, &home, "era2", "all");
    let era = home.client("_classic_era_");
    fs::rename(&era, era.with_extension("gone")).unwrap();
    let ran = log.run(&home, &json(&["status", "-t", "all"]));
    ran.fails(3, "target_not_found");
    reserved_once(&log, &ran);
}

#[test]
fn all_is_a_reserved_id() {
    let log = tlog!();
    log.step("home");
    let home = Home::new(&["wow", "wow_classic_era"]);
    log.env(&home);
    log.run(&home, &json(&["targets", "default", "retail"]))
        .ok();
    let custom = CustomRoot::at(home.env.root().join("clients/custom")).build();
    let roots = [
        ("state", home.env.state()),
        ("retail", home.client("_retail_")),
        ("era", home.client("_classic_era_")),
        ("custom", custom.clone()),
    ];
    for (label, root) in &roots {
        log.tree(label, root);
    }
    let registry = home.state().join("registry.json");
    let before = fs::read(&registry).unwrap();

    log.step("refusals");
    let path = custom.display().to_string();
    for command in [
        &["targets", "rename", "retail", "all"][..],
        &["targets", "add", &path, "--name", "all"],
        &["targets", "add", &path, "--name", "all", "--dry-run"],
    ] {
        let ran = log.run(&home, &json(command));
        ran.fails(2, "usage");
        let message = ran.envelope()["error"]["message"]
            .as_str()
            .unwrap_or_default();
        assert!(message.contains("reserved"), "{}", ran.run.context());
    }

    log.step("nothing written");
    for (label, root) in &roots {
        log.assert_unchanged(label, root);
    }
    assert_eq!(
        fs::read(&registry).unwrap(),
        before,
        "registry.json changed"
    );

    log.step("near names are fine");
    log.run(&home, &json(&["targets", "rename", "retail", "all-1"]))
        .ok();
    log.run(&home, &json(&["targets", "rename", "all-1", "retail"]))
        .ok();
}

#[test]
fn all_lists_registered_clients_whose_directory_is_gone() {
    let log = tlog!();
    log.step("home");
    let home = three_clients(&log);
    let ptr = home.client("_ptr_");
    fs::rename(&ptr, ptr.with_extension("gone")).unwrap();

    log.step("status");
    let split = |ran: &Ran| {
        let targets = ran.data()["targets"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let (gone, found): (Vec<_>, Vec<_>) = targets.iter().partition(|t| t["missing"] == true);
        let ids = |list: Vec<&Value>| -> Vec<String> {
            list.iter()
                .map(|t| t["id"].as_str().unwrap_or_default().to_owned())
                .collect()
        };
        (ids(gone), ids(found))
    };
    let bare = log.run(&home, &json(&["status"]));
    let all = log.run(&home, &json(&["status", "-t", "all"]));
    let (bare, all) = (split(bare.ok()), split(all.ok()));
    log.assert_eq("missing", &all.0, &owned(&["ptr"]));
    log.assert_eq("as without -t", &all, &bare);

    log.step("export");
    let pick = |ran: &Ran| {
        let data = ran.ok().data();
        let missing = warnings_coded(ran, "export_missing_target");
        (data["targets"].clone(), data["entries"].clone(), missing)
    };
    let bare = log.run(&home, &json(&["export"]));
    let all = log.run(&home, &json(&["export", "-t", "all"]));
    let (bare, all) = (pick(&bare), pick(&all));
    log.assert_eq("export: as without -t", &all, &bare);
    assert_eq!(all.2.len(), 1, "one export_missing_target: {:?}", all.2);
}
