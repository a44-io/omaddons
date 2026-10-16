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
