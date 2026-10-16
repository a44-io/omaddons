//! The command matrix: at least one success and one failure per M1 command, each run
//! under `--json` and checked against the whole contract, with envelope snapshots
//! (meta removed, ULIDs, times and the tempdir redacted).

use super::*;

#[test]
fn bare_and_help() {
    let s = Scene::new();
    let bare = s.home.exec(&[]);
    assert_eq!(bare.code, 0, "{}", bare.context());
    assert!(
        bare.stdout.contains("Usage: omaddons"),
        "{}",
        bare.context()
    );
    let c = s.json(&[], 0);
    assert_eq!(c.envelope["command"], "help");
    let c = s.json(&["--bogus"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
    let c = s.json(&["help", "install"], 0);
    assert_eq!(c.envelope["command"], "help");
    let c = s.json(&["help", "nosuch"], 2);
    insta::assert_json_snapshot!("help_unknown", s.stable(&c.envelope));
}

#[test]
fn status() {
    let s = Scene::new();
    let c = s.json(&["status"], 0);
    insta::assert_json_snapshot!("status_ok", s.stable(&c.envelope));
    let c = s.json(&["status", "-t", "nosuch"], 3);
    insta::assert_json_snapshot!("status_unknown_target", s.stable(&c.envelope));
    s.install_foo();
    let zip = s.zip("foo2");
    let crashed = s.home.exec_with(
        &[
            "install", "gh:o/foo", "--file", &zip, "-t", "retail", "--json",
        ]
        .map(str::to_owned),
        &[("OMADDONS_FAILPOINTS", "engine.journal.prepared=abort")],
    );
    assert_eq!(crashed.code, -1, "{}", crashed.context());
    let c = s.json(&["status", "--check"], 6);
    assert_eq!(c.envelope["error"]["code"], "check_failed");
    insta::assert_json_snapshot!("status_check_failed", s.stable(&c.envelope));
}

#[test]
fn targets() {
    let s = Scene::new();
    let c = s.json(&["targets"], 0);
    insta::assert_json_snapshot!("targets_ok", s.stable(&c.envelope));
    let root = common::trees::CustomRoot::at(s.home.env.root().join("elsewhere/wow-335"))
        .version(3, 3, 5, 12_340)
        .build();
    let c = s.json(&["targets", "add", &root.display().to_string()], 0);
    insta::assert_json_snapshot!("targets_add", s.stable(&c.envelope));
    let empty = s.home.env.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let c = s.json(&["targets", "add", &empty.display().to_string()], 3);
    assert_eq!(c.envelope["error"]["code"], "target_not_found");
    let c = s.json(&["targets", "rename", "retail", "Bad Name"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
}

#[test]
fn list() {
    let s = Scene::new();
    s.install_foo();
    let c = s.json(&["list"], 0);
    insta::assert_json_snapshot!("list_ok", s.stable(&c.envelope));
    let c = s.json(&["list", "-t", "nosuch"], 3);
    assert_eq!(c.envelope["error"]["code"], "target_not_found");
}

#[test]
fn pin_and_unpin() {
    let s = Scene::new();
    s.json(&["install", "weakauras", "-t", "retail"], 0);
    let c = s.json(&["pin", "weakauras"], 0);
    insta::assert_json_snapshot!("pin_ok", s.stable(&c.envelope));
    let c = s.json(&["unpin", "weakauras"], 0);
    assert_eq!(
        c.envelope["data"]["targets"][0]["items"][0]["status"],
        "unpinned"
    );
    let c = s.json(&["pin", "nosuch"], 3);
    insta::assert_json_snapshot!("pin_not_managed", s.stable(&c.envelope));
    s.json(&["unpin", "nosuch"], 3);
}

#[test]
fn sync() {
    let s = Scene::new();
    let c = s.json(&["sync"], 3);
    insta::assert_json_snapshot!("sync_not_found", s.stable(&c.envelope));
    s.home
        .write_desired("[family.mainline]\naddons = [\"weakauras\", \"gh:o/foo\"]\n");
    // A dry run is deterministic, and the same twice.
    let dry = || support::without_meta(&s.json(&["sync", "--dry-run"], 0).run.stdout);
    assert_eq!(dry(), dry());
    let c = s.json(&["sync"], 0);
    assert_eq!(c.envelope["data"]["summary"]["changed"], 2);
    insta::assert_json_snapshot!("sync_ok", s.stable(&c.envelope));
    let c = s.json(&["sync"], 0);
    assert_eq!(c.envelope["data"]["summary"]["changed"], 0);
    s.home
        .write_desired("[family.mainline]\naddons = \"weakauras\"\n");
    let c = s.json(&["sync"], 2);
    insta::assert_json_snapshot!("sync_invalid", s.stable(&c.envelope));
}

#[test]
fn update() {
    let s = Scene::new();
    s.json(&["install", "weakauras", "-t", "retail"], 0);
    let both = "## Interface: 120105, 50504\n## Version: 4\n";
    let wa: [common::http::Asset<'_>; 1] = [(
        "WeakAuras.zip",
        &[("mainline", 120_105), ("mists", 50_504)],
        vec![("WeakAuras/WeakAuras.toc", both)],
    )];
    let base = "https://github.com/WeakAuras/WeakAuras2";
    common::http::gh_release_v(&s.home.http, base, "5.20.0", &wa);
    common::http::gh_latest(&s.home.http, base, "5.20.0");
    let c = s.json(&["update", "--refresh"], 0);
    assert_eq!(c.envelope["data"]["summary"]["changed"], 1);
    insta::assert_json_snapshot!("update_ok", s.stable(&c.envelope));
    let c = s.json(&["update", "--no-deps"], 0);
    assert_eq!(c.envelope["data"]["summary"]["changed"], 0);
    let c = s.json(&["update", "--channel", "beta"], 2);
    insta::assert_json_snapshot!("update_channel_without_names", s.stable(&c.envelope));
    let c = s.json(&["upgrade", "nosuch"], 3);
    assert_eq!(c.envelope["error"]["code"], "not_managed");
}

#[test]
fn outdated() {
    let s = Scene::new();
    s.install_foo();
    s.json(&["install", "weakauras", "-t", "retail"], 0);
    let c = s.json(&["outdated"], 0);
    insta::assert_json_snapshot!("outdated_ok", s.stable(&c.envelope));
    let both = "## Interface: 120105, 50504\n## Version: 4\n";
    let wa: [common::http::Asset<'_>; 1] = [(
        "WeakAuras.zip",
        &[("mainline", 120_105), ("mists", 50_504)],
        vec![("WeakAuras/WeakAuras.toc", both)],
    )];
    let base = "https://github.com/WeakAuras/WeakAuras2";
    common::http::gh_release_v(&s.home.http, base, "5.20.0", &wa);
    common::http::gh_latest(&s.home.http, base, "5.20.0");
    let c = s.json(&["outdated", "--refresh", "--check"], 6);
    assert_eq!(c.envelope["error"]["code"], "check_failed");
    insta::assert_json_snapshot!("outdated_check_failed", s.stable(&c.envelope));
    let c = s.json(&["check", "-t", "nosuch"], 3);
    assert_eq!(c.envelope["command"], "outdated");
    assert_eq!(c.envelope["error"]["code"], "target_not_found");
}

#[test]
fn adopt() {
    let s = Scene::new();
    // Foo v1 unpacked by hand into retail, and a catalog entry naming its repository.
    let addons = s.home.addons("_retail_");
    std::fs::create_dir_all(addons.join("Foo")).unwrap();
    std::fs::write(
        addons.join("Foo/Foo.toc"),
        "## Interface: 120105\n## Version: 1\n",
    )
    .unwrap();
    let catalog = s.home.env.config().join("omaddons/catalog.d");
    std::fs::create_dir_all(&catalog).unwrap();
    std::fs::write(
        catalog.join("50-foo.toml"),
        "schema_version = \"1.0\"\n\n[[addon]]\nname = \"Foo\"\nref = \"gh:o/foo\"\nfolders = [\"Foo\"]\n",
    )
    .unwrap();
    let c = s.json(&["adopt", "-t", "retail"], 0);
    assert_eq!(
        c.envelope["data"]["targets"][0]["groups"][0]["tier"],
        "exact"
    );
    insta::assert_json_snapshot!("adopt_ok", s.stable(&c.envelope));
    let c = s.json(&["adopt", "--no-verify"], 0);
    assert_eq!(c.envelope["data"]["verify"], serde_json::Value::Null);
    let c = s.json(&["adopt", "--apply", "Foo"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
    let c = s.json(&["adopt", "--ref", "gh:o/foo"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
    // A folder nothing identifies: the fix is the --ref text.
    std::fs::create_dir_all(addons.join("Mystery")).unwrap();
    std::fs::write(addons.join("Mystery/Mystery.toc"), "## Interface: 120105\n").unwrap();
    let c = s.json(&["adopt", "Mystery", "-t", "retail", "--no-verify"], 3);
    insta::assert_json_snapshot!("adopt_unknown_folder", s.stable(&c.envelope));
    let c = s.json(&["adopt", "--apply", "-t", "retail"], 0);
    insta::assert_json_snapshot!("adopt_apply", s.stable(&c.envelope));
    let c = s.json(&["adopt", "Foo", "--ref", "gh:o/bar", "-t", "retail"], 0);
    assert_eq!(
        c.envelope["data"]["targets"][0]["items"][0]["status"],
        "edited"
    );
}

#[test]
fn export() {
    let s = Scene::new();
    s.install_foo();
    let c = s.json(&["export"], 0);
    insta::assert_json_snapshot!("export_ok", s.stable(&c.envelope));
    let c = s.json(&["export", "--write"], 0);
    assert!(c.envelope["data"]["path"].is_string());
    // Edited by hand since: refused until --overwrite.
    let file = s.home.env.config().join("omaddons/addons.toml");
    std::fs::write(&file, "# mine\n").unwrap();
    let c = s.json(&["export", "--write"], 2);
    insta::assert_json_snapshot!("export_edited", s.stable(&c.envelope));
    let c = s.json(&["export", "--overwrite"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
}

#[test]
fn info() {
    let s = Scene::new();
    let c = s.json(&["info", "weakauras"], 0);
    insta::assert_json_snapshot!("info_ok", s.stable(&c.envelope));
    let c = s.json(&["info", "nosuchaddon"], 3);
    insta::assert_json_snapshot!("info_not_found", s.stable(&c.envelope));
}

#[test]
fn install() {
    let s = Scene::new();
    let zip = s.zip("foo");
    let c = s.json(&["install", "gh:o/foo", "--file", &zip, "-t", "retail"], 0);
    insta::assert_json_snapshot!("install_file", s.stable(&c.envelope));
    let c = s.json(&["install", "weakauras", "-t", "classic"], 0);
    insta::assert_json_snapshot!("install_catalog", s.stable(&c.envelope));
    let c = s.json(&["install", "gh:"], 2);
    assert_eq!(c.envelope["error"]["code"], "ref_invalid");
    let both = s.zip("both");
    let c = s.json(&["install", "gh:o/both", "--file", &both], 3);
    insta::assert_json_snapshot!("install_ambiguous", s.stable(&c.envelope));
    let blizz = s.zip("blizz");
    let c = s.json(
        &["install", "gh:o/blizz", "--file", &blizz, "-t", "retail"],
        4,
    );
    assert_eq!(c.envelope["error"]["code"], "protected_folder");
    let c = s.json(
        &[
            "install", "gh:o/foo", "gh:o/bar", "-t", "retail", "--strict",
        ],
        6,
    );
    insta::assert_json_snapshot!("install_strict", s.stable(&c.envelope));
}

#[test]
fn install_of_a_pinned_addon() {
    let s = Scene::new();
    let (foo, foo2) = (s.zip("foo"), s.zip("foo2"));
    let pin = [
        "install",
        "gh:o/foo",
        "--file",
        &foo,
        "--version",
        "1.0",
        "-t",
        "retail",
    ];
    s.json(&pin, 0);
    let c = s.json(&["install", "gh:o/foo", "--file", &foo2, "-t", "retail"], 0);
    let warning = c.envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "addon_pinned")
        .cloned();
    assert!(warning.is_some(), "{}", c.run.context());
    let mirrored = c
        .run
        .stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["event"] == "warning" && e["code"] == "addon_pinned");
    assert_eq!(
        mirrored.map(|e| e["fix_argv"].clone()),
        warning.map(|w| w["fix_argv"].clone()),
        "mirrored on stderr with the same argv\n{}",
        c.run.context()
    );
    insta::assert_json_snapshot!("install_pinned", s.stable(&c.envelope));
}

#[test]
fn remove() {
    let s = Scene::new();
    s.install_foo();
    let c = s.json(&["remove", "Foo", "-t", "retail"], 0);
    insta::assert_json_snapshot!("remove_ok", s.stable(&c.envelope));
    let c = s.json(&["remove", "Nope", "-t", "retail"], 3);
    insta::assert_json_snapshot!("remove_not_managed", s.stable(&c.envelope));
}

#[test]
fn undo() {
    let s = Scene::new();
    let c = s.json(&["undo"], 3);
    insta::assert_json_snapshot!("undo_nothing", s.stable(&c.envelope));
    let first = s.install_foo();
    let zip = s.zip("foo2");
    s.json(&["install", "gh:o/foo", "--file", &zip, "-t", "retail"], 0);
    let c = s.json(&["undo", &first], 4);
    insta::assert_json_snapshot!("undo_conflict", s.stable(&c.envelope));
    let c = s.json(&["undo"], 0);
    insta::assert_json_snapshot!("undo_ok", s.stable(&c.envelope));
}

#[test]
fn history() {
    let s = Scene::new();
    s.install_foo();
    let c = s.json(&["history"], 0);
    insta::assert_json_snapshot!("history_ok", s.stable(&c.envelope));
    let c = s.json(&["history", "--since", "nonsense"], 2);
    insta::assert_json_snapshot!("history_bad_since", s.stable(&c.envelope));
}

#[test]
fn runs() {
    let s = Scene::new();
    s.install_foo();
    let c = s.json(&["history"], 0);
    let run = c.envelope["data"]["entries"][0]["run"]
        .as_str()
        .unwrap()
        .to_owned();
    let c = s.json(&["history", "--run", &run], 0);
    assert_eq!(c.envelope["data"]["entries"].as_array().unwrap().len(), 1);
    let c = s.json(&["history", "--run", "01ZZZZZZZZ"], 3);
    insta::assert_json_snapshot!("history_unknown_run", s.stable(&c.envelope));
    let c = s.json(&["undo", "--run", "01ZZZZZZZZ"], 3);
    assert_eq!(c.envelope["error"]["code"], "not_found");
    let c = s.json(&["undo", "--run", &run], 0);
    assert_eq!(c.envelope["data"]["reverted_run"], run.as_str());
    insta::assert_json_snapshot!("undo_run", s.stable(&c.envelope));
}

#[test]
fn config() {
    let s = Scene::new();
    s.json(&["config", "get", "default_channel"], 0);
    let c = s.json(&["config", "set", "default_channel", "beta"], 0);
    insta::assert_json_snapshot!("config_set", s.stable(&c.envelope));
    s.json(&["config", "path"], 0);
    let c = s.json(&["config", "set", "nosuch.key", "1"], 2);
    insta::assert_json_snapshot!("config_unknown_key", s.stable(&c.envelope));
}

#[test]
fn net_log() {
    let s = Scene::new();
    s.json(&["install", "gh:o/foo", "-t", "retail"], 0);
    let c = s.json(&["net-log"], 0);
    insta::assert_json_snapshot!("net_log_ok", s.stable(&c.envelope));
    let c = s.json(&["net-log", "--since", "nonsense"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
}
