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
    // A failure still reports what the run found before it: the pending journal.
    let c = s.json(&["list", "-t", "nosuch"], 3);
    let codes: Vec<&str> = c.envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["journal_pending"], "{}", c.envelope);
}

#[test]
fn status_running_unknown() {
    use std::os::unix::fs::PermissionsExt;

    if common::procfs::running_as_root() {
        return; // chmod 000 hides nothing from root
    }
    // A process table omaddons cannot read: `running` is null, and a warning says why.
    let s = Scene::new();
    let locked = s.home.env.root().join("proc-locked");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let env = [("OMADDONS_PROC_ROOT", locked.to_str().unwrap())];
    let c = s.json_env(&["status"], &env, 0);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    insta::assert_json_snapshot!("status_running_unknown", s.stable(&c.envelope));
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
    // --locked: an entry without a version warns not_locked; a second run is free.
    s.json(&["remove", "gh:o/foo", "-t", "retail"], 0);
    s.home
        .write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    let c = s.json(&["sync", "--locked", "-t", "retail"], 0);
    insta::assert_json_snapshot!("sync_locked", s.stable(&c.envelope));
    let c = s.json(&["sync", "--locked", "-t", "retail"], 0);
    assert_eq!(c.envelope["data"]["summary"]["changed"], 0);
    // --prune refuses a section for no known client.
    s.home
        .write_desired("[target.\"retial\"]\naddons = [\"gh:o/foo\"]\n");
    let c = s.json(&["sync", "--prune"], 2);
    insta::assert_json_snapshot!("sync_prune_refused", s.stable(&c.envelope));
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

/// The M2 flags and aliases the per-command rows leave out, each with a success and a
/// failure (the contract audit, omad-m2-updates-adoption-2hp.8.1).
#[test]
fn m2_surface() {
    let s = Scene::new();
    // An empty export is never written.
    let c = s.json(&["export", "--write"], 2);
    assert_eq!(c.envelope["error"]["details"]["reason"], "empty");
    insta::assert_json_snapshot!("export_empty", s.stable(&c.envelope));

    s.json(&["install", "weakauras", "-t", "retail"], 0);
    // install --reinstall and --refresh.
    let c = s.json(&["install", "weakauras", "-t", "retail", "--reinstall"], 0);
    assert_eq!(c.envelope["data"]["items"][0]["status"], "updated");
    let c = s.json(&["install", "gh:o/nope", "-t", "retail", "--reinstall"], 3);
    assert_eq!(c.envelope["error"]["code"], "not_managed");
    s.json(&["install", "weakauras", "-t", "classic", "--refresh"], 0);
    s.json(&["install", "gh:", "--refresh"], 2);
    // status --deep and list --deep.
    s.json(&["status", "--deep"], 0);
    s.json(&["status", "--deep", "-t", "nosuch"], 3);
    s.json(&["list", "--deep"], 0);
    s.json(&["list", "--deep", "-t", "nosuch"], 3);
    // history --by.
    let c = s.json(&["history", "--by", "human"], 0);
    assert!(c.envelope["data"]["entries"].is_array());
    let c = s.json(&["history", "--by", "robot"], 2);
    insta::assert_json_snapshot!("history_bad_by", s.stable(&c.envelope));

    // A newer WeakAuras with release notes.
    let both = "## Interface: 120105, 50504\n## Version: 4\n";
    let wa: [common::http::Asset<'_>; 1] = [(
        "WeakAuras.zip",
        &[("mainline", 120_105), ("mists", 50_504)],
        vec![("WeakAuras/WeakAuras.toc", both)],
    )];
    let base = "https://github.com/WeakAuras/WeakAuras2";
    common::http::gh_release_v(&s.home.http, base, "5.20.0", &wa);
    common::http::gh_latest(&s.home.http, base, "5.20.0");
    common::http::gh_feed(
        &s.home.http,
        base,
        &[("5.20.0", "<p>Faster</p>"), ("5.19.2", "<p>Older</p>")],
        None,
    );
    // info --refresh and --changelog.
    s.json(&["info", "weakauras", "--refresh"], 0);
    s.json(&["info", "nosuchaddon", "--refresh"], 3);
    let c = s.json(&["info", "weakauras", "--changelog"], 0);
    insta::assert_json_snapshot!("info_changelog", s.stable(&c.envelope));
    s.json(&["info", "nosuchaddon", "--changelog"], 3);
    // outdated --changelog, and the check alias.
    let c = s.json(&["outdated", "--changelog"], 0);
    insta::assert_json_snapshot!("outdated_changelog", s.stable(&c.envelope));
    s.json(&["outdated", "--changelog", "-t", "nosuch"], 3);
    let c = s.json(&["check"], 0);
    assert_eq!(c.envelope["command"], "outdated");
    // update: names with --all, and a version, are usage errors with fixes.
    let c = s.json(&["update", "--all", "weakauras"], 2);
    insta::assert_json_snapshot!("update_all_with_names", s.stable(&c.envelope));
    let c = s.json(&["update", "weakauras", "--version", "5.19.2"], 2);
    insta::assert_json_snapshot!("update_with_version", s.stable(&c.envelope));
    // The upgrade alias.
    let c = s.json(&["upgrade"], 0);
    assert_eq!(c.envelope["command"], "update");
    // The find alias fails like search.
    let c = s.json(&["find", "weakauras", "--limit", "0"], 2);
    assert_eq!(c.envelope["command"], "search");

    // auth github set from standard input, then clear.
    let c = s.json_input(&["auth", "github", "set"], b"ghp_contract\n", 0);
    insta::assert_json_snapshot!("auth_set", s.stable(&c.envelope));
    let c = s.json(&["auth", "github", "clear"], 0);
    insta::assert_json_snapshot!("auth_clear", s.stable(&c.envelope));
}

#[test]
fn auth() {
    let s = Scene::new();
    // No token configured: status needs no request offline.
    let c = s.json(&["auth", "github", "--offline"], 0);
    insta::assert_json_snapshot!("auth_status_offline", s.stable(&c.envelope));
    // Standard input is not a terminal here, and empty: nothing to store.
    let c = s.json(&["auth", "github", "set"], 2);
    insta::assert_json_snapshot!("auth_set_nothing", s.stable(&c.envelope));
    let c = s.json(&["auth", "github", "clear", "--from-gh"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
}

#[test]
fn copy() {
    let s = Scene::new();
    s.install_foo();
    s.json(&["install", "weakauras", "-t", "retail"], 0);
    // WeakAuras has a Mists build, Foo does not: one install, one skip.
    let c = s.json(&["copy", "--from", "retail", "--to", "classic"], 0);
    assert_eq!(c.envelope["data"]["summary"]["changed"], 1);
    assert_eq!(c.envelope["data"]["summary"]["skipped"], 1);
    insta::assert_json_snapshot!("copy_ok", s.stable(&c.envelope));
    let c = s.json(
        &[
            "copy",
            "--from",
            "retail",
            "--to",
            "classic",
            "weakauras",
            "nosuch",
        ],
        0,
    );
    assert_eq!(c.envelope["partial"], true);
    insta::assert_json_snapshot!("copy_partial", s.stable(&c.envelope));
    let c = s.json(&["copy", "--from", "retail", "--to", "retail"], 2);
    insta::assert_json_snapshot!("copy_same_client", s.stable(&c.envelope));
    let c = s.json(
        &["copy", "--from", "retail", "--to", "classic", "-t", "x"],
        2,
    );
    assert_eq!(c.envelope["error"]["code"], "usage");
    // The same copy without -t.
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "copy", "--from", "retail", "--to", "classic"])
    );
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
fn reinstall() {
    let s = Scene::new();
    s.json(&["install", "weakauras", "-t", "retail"], 0);
    // The verb: install with --reinstall, and that runs.
    let c = s.json(&["reinstall", "weakauras", "-t", "retail"], 2);
    insta::assert_json_snapshot!("reinstall_verb", s.stable(&c.envelope));
    let fix = crate::support::strings(&c.envelope["error"]["fix_argv"]);
    let args: Vec<&str> = fix[1..].iter().map(String::as_str).collect();
    let c = s.json(&args, 0);
    assert_eq!(c.envelope["data"]["items"][0]["status"], "updated");
    // Offline, with the archive gone from the cache: nothing to reinstall from.
    std::fs::remove_dir_all(s.home.env.cache().join("omaddons/blobs")).unwrap();
    let c = s.json(
        &[
            "install",
            "weakauras",
            "-t",
            "retail",
            "--reinstall",
            "--offline",
        ],
        1,
    );
    insta::assert_json_snapshot!("reinstall_offline", s.stable(&c.envelope));
}

#[test]
fn info() {
    let s = Scene::new();
    let c = s.json(&["info", "weakauras"], 0);
    insta::assert_json_snapshot!("info_ok", s.stable(&c.envelope));
    let c = s.json(&["info", "nosuchaddon"], 3);
    insta::assert_json_snapshot!("info_not_found", s.stable(&c.envelope));
}

/// Dependency items, fields, warnings and hints: an install with a dependency, with one
/// that nothing ships, and with a bridge requirement, resolved or not.
#[test]
fn install_with_dependencies() {
    use super::common::graph::{Folder, Node, release_graph};
    let s = Scene::new();
    let folder = |name, deps| Folder {
        name,
        interface: "120105",
        deps,
        optional: &[],
    };
    let node = |base, folders| Node {
        base,
        tag: "v1",
        folders,
    };
    release_graph(
        &s.home.http,
        &[
            node(
                "https://github.com/o/littlewigs",
                &[folder("LittleWigs", &["BigWigs", "Nowhere"])],
            ),
            node("https://github.com/o/bigwigs", &[folder("BigWigs", &[])]),
            node(
                "https://github.com/o/plug",
                &[folder("Plug", &[]), folder("Plug_Masque", &["Masque"])],
            ),
            node("https://github.com/o/masque", &[folder("Masque", &[])]),
            node(
                "https://github.com/o/lone",
                &[folder("Lone", &[]), folder("Lone_Nowhere", &["Nowhere"])],
            ),
        ],
    );
    // Tukui and WoWInterface are consulted for Nowhere, and neither ships it.
    common::tukui::tukui_list(&s.home.http, &[common::tukui::elvui()]);
    common::wowi::wowi_filelist(
        &s.home.http,
        &[common::wowi::FileEntry::new(5995, "BugSack", &["BugSack"])],
    );
    let catalog = s.home.env.config().join("omaddons/catalog.d");
    std::fs::create_dir_all(&catalog).unwrap();
    let mut text = String::from("schema_version = \"1.0\"\n");
    for name in ["LittleWigs", "BigWigs", "Masque"] {
        text.push_str(&format!(
            "\n[[addon]]\nname = \"{name}\"\nref = \"gh:o/{}\"\nfolders = [\"{name}\"]\n",
            name.to_lowercase()
        ));
    }
    std::fs::write(catalog.join("50-deps.toml"), text).unwrap();
    let c = s.json(&["install", "littlewigs", "-t", "retail"], 0);
    let requests: Vec<&serde_json::Value> = c.envelope["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| &i["request"])
        .collect();
    assert_eq!(requests, ["littlewigs", "BigWigs"]);
    insta::assert_json_snapshot!("install_dependencies", s.stable(&c.envelope));
    let c = s.json(&["install", "gh:o/plug", "-t", "retail"], 0);
    let codes: Vec<&serde_json::Value> = c.envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| &w["code"])
        .collect();
    assert_eq!(codes, ["bridge_dependency"]);
    // Every hint runs.
    for hint in c.envelope["commands"].as_array().unwrap() {
        let argv = support::strings(&hint["argv"]);
        if hint["action"] == "install" {
            let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
            s.json(&args, 0);
        }
    }
    // A bridge requirement nothing resolves is searched for, and its hint says so.
    let c = s.json(&["install", "gh:o/lone", "-t", "retail"], 0);
    let bridge = c.envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "bridge_dependency")
        .unwrap();
    assert_eq!(
        bridge["fix_argv"],
        serde_json::json!(["omaddons", "search", "Nowhere"])
    );
    let hints: Vec<(&serde_json::Value, &serde_json::Value)> = c.envelope["commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["action"] != "undo")
        .map(|h| (&h["action"], &h["argv"]))
        .collect();
    assert_eq!(
        hints,
        [(
            &serde_json::json!("search"),
            &serde_json::json!(["omaddons", "search", "Nowhere"])
        )],
        "{}",
        c.envelope
    );
}

/// `search`: a catalog hit merged with its GitHub duplicate, the offline skips, and the
/// bounded `--limit`.
#[test]
fn search() {
    let s = Scene::new();
    super::common::http::github_search(
        &s.home.http,
        "weakauras",
        &[(
            "WeakAuras",
            "WeakAuras2",
            "A powerful framework to display World of Warcraft information",
            1300,
            "2026-10-01T00:00:00Z",
        )],
    );
    super::common::wowi::wowi_filelist(&s.home.http, &[]);
    super::common::tukui::tukui_list(&s.home.http, &[]);
    let c = s.json(&["search", "weakauras"], 0);
    insta::assert_json_snapshot!("search_ok", s.stable(&c.envelope));
    let c = s.json(&["find", "weakauras", "--offline"], 0);
    assert_eq!(
        c.envelope["data"]["sources_skipped"][0],
        serde_json::json!({"source": "github", "reason": "offline"})
    );
    let c = s.json(&["search", "weakauras", "--limit", "99"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
}

/// Other commands' fixes for an unmanaged folder adopt it, and each runs.
#[test]
fn adopt_fixes() {
    let s = Scene::new();
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
    let zip = s.zip("foo");
    let c = s.json(&["install", "gh:o/foo", "--file", &zip, "-t", "retail"], 4);
    insta::assert_json_snapshot!("install_unmanaged", s.stable(&c.envelope));
    let adopt = [
        "omaddons", "adopt", "Foo", "--ref", "gh:o/foo", "-t", "retail",
    ];
    for args in [&["pin", "Foo"][..], &["remove", "Foo", "-t", "retail"]] {
        let c = s.json(args, 3);
        assert_eq!(
            (
                &c.envelope["error"]["code"],
                support::strings(&c.envelope["error"]["fix_argv"])
            ),
            (
                &serde_json::json!("not_managed"),
                adopt.map(str::to_owned).to_vec()
            ),
            "{args:?}"
        );
    }
    let c = s.json(&adopt[1..], 0);
    assert_eq!(
        c.envelope["data"]["targets"][0]["items"][0]["status"],
        "adopted"
    );
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
        // ubs:ignore — comparing event fields, not secrets
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

/// Persisted formats fail closed (M3-D45): an undo refused for a newer history line this
/// build cannot read, and a change refused up front for a record a newer omaddons wrote.
#[test]
fn fail_closed() {
    let s = Scene::new();
    let first = s.install_foo();
    let key = common::state::key_of(&s.home, "retail");
    let history = s.home.state().join("history.jsonl");
    let mut text = fs::read_to_string(&history).unwrap();
    let line = serde_json::json!({
        "schema_version": "1.0", "tx": "01M5ZZZZZZZZZZZZZZZZZZZZZZ", "kind": "restore",
        "at": "2026-10-21T12:00:00Z", "target": key.to_string(), "actor": "human",
        "reason": null, "argv": ["omaddons"], "result": "committed",
        // An op this build does not know, so the line does not read.
        "ops": [{"op": "merge", "folder": "Foo", "addon": "Foo"}],
    });
    text.push_str(&format!("{line}\n"));
    fs::write(&history, text).unwrap();
    let c = s.json(&["undo", &first], 4);
    insta::assert_json_snapshot!("undo_history_unreadable", s.stable(&c.envelope));

    let record = s.home.state().join(format!("targets/{key}.json"));
    let mut doc: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    doc["schema_version"] = serde_json::json!("1.99");
    fs::write(&record, serde_json::to_vec(&doc).unwrap()).unwrap();
    let zip = s.zip("foo2");
    let c = s.json(&["install", "gh:o/foo", "--file", &zip, "-t", "retail"], 4);
    insta::assert_json_snapshot!("install_state_too_new", s.stable(&c.envelope));
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

#[test]
fn backup_list() {
    use omaddons::backup::BackupKind;
    let s = Scene::new();
    let at = "2026-10-18T21:15:00Z".parse().unwrap();
    common::backups::seed_backup(&s.home.client("_retail_"), BackupKind::Manual, at, 2);
    // Free space as a debug build is told it, so the envelope is the same anywhere.
    let free = [("OMADDONS_FS_FREE_MB", "1024")];
    let c = s.json_env(&["backup", "list"], &free, 0);
    insta::assert_json_snapshot!("backup_list_ok", s.stable(&c.envelope));
    let bare = s.json_env(&["backup"], &free, 0);
    assert_eq!(
        s.stable(&bare.envelope),
        s.stable(&c.envelope),
        "bare backup is backup list"
    );
    let c = s.json_env(&["backup", "list", "01ZZZZZZ"], &free, 3);
    insta::assert_json_snapshot!("backup_list_unknown_id", s.stable(&c.envelope));
    let c = s.json(&["backup", "list", "01ZZZZZ"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage", "seven characters");
}

#[test]
fn preview_binding() {
    let s = Scene::new();
    s.install_foo();
    let c = s.json(&["remove", "Foo", "-t", "retail", "--dry-run"], 0);
    let token = c.envelope["data"]["precondition"]
        .as_str()
        .unwrap()
        .to_owned();
    // The target changes between the preview and the run.
    let foo2 = s.zip("foo2");
    s.json(&["install", "gh:o/foo", "--file", &foo2, "-t", "retail"], 0);
    let c = s.json(&["remove", "Foo", "-t", "retail", "--if-match", &token], 1);
    insta::assert_json_snapshot!("if_match_stale", s.stable(&c.envelope));
    let c = s.json(&["update", "--if-match", &token], 2);
    insta::assert_json_snapshot!("if_match_unbound_command", s.stable(&c.envelope));
}

#[test]
fn backup_create() {
    let s = Scene::new();
    common::wtf::Wtf::at(s.home.client("_retail_"))
        .config_wtf("SET locale \"enUS\"\n")
        .saved_variables("Foo", 100)
        .build();
    // Byte copies and the free space as a debug build is told them, so the envelope is
    // the same on any filesystem.
    let env = [
        ("OMADDONS_FS_FREE_MB", "1024"),
        ("OMADDONS_FAILPOINTS", "fsx.reflink.unsupported"),
    ];
    let c = s.json_env(&["backup", "create", "-t", "retail"], &env, 0);
    insta::assert_json_snapshot!("backup_create_ok", s.stable(&c.envelope));
    let c = s.json_env(&["backup", "create", "-t", "retail", "--dry-run"], &env, 0);
    insta::assert_json_snapshot!("backup_create_dry_run", s.stable(&c.envelope));
    std::fs::remove_dir_all(s.home.client("_classic_").join("WTF")).unwrap();
    let c = s.json_env(&["backup", "create", "-t", "classic"], &env, 3);
    insta::assert_json_snapshot!("backup_create_no_wtf", s.stable(&c.envelope));
}

#[test]
fn backup_restore() {
    use omaddons::backup::BackupKind;
    let mut s = Scene::new();
    let client = s.home.client("_retail_");
    common::wtf::Wtf::at(client.clone())
        .config_wtf("SET locale \"enUS\"\n")
        .saved_variables("Foo", 100)
        .build();
    let at = "2026-10-05T08:00:00Z".parse().unwrap();
    let id = common::backups::seed_backup(&client, BackupKind::Manual, at, 2).to_string();
    let restore = ["backup", "restore", id.as_str(), "-t", "retail"];
    let c = s.json(&[&restore[..], &["--dry-run"]].concat(), 0);
    insta::assert_json_snapshot!("backup_restore_dry_run", s.stable(&c.envelope));
    let token = c.envelope["data"]["precondition"]
        .as_str()
        .unwrap()
        .to_owned();
    // The game wrote settings since the preview.
    let sv = client.join("WTF/Account/FAKEACCT/SavedVariables/Foo.lua");
    std::fs::write(&sv, "Foo = 2").unwrap();
    let c = s.json(&[&restore[..], &["--if-match", &token]].concat(), 1);
    insta::assert_json_snapshot!("backup_restore_stale", s.stable(&c.envelope));
    let c = s.json(&["backup", "restore", "01ZZZZZZ"], 3);
    insta::assert_json_snapshot!("backup_restore_not_found", s.stable(&c.envelope));
    let no_exchange = [("OMADDONS_FAILPOINTS", "fsx.renameat2.einval")];
    let c = s.json_env(&restore, &no_exchange, 4);
    insta::assert_json_snapshot!(
        "backup_restore_filesystem_unsupported",
        s.stable(&c.envelope)
    );
    let c = s.json(&restore, 0);
    insta::assert_json_snapshot!("backup_restore_ok", s.stable(&c.envelope));
    s.home.retail_running();
    let c = s.json(&restore, 4);
    insta::assert_json_snapshot!("backup_restore_game_running", s.stable(&c.envelope));
}

/// `backup restore --addon`: one folder's settings put back, and a capture-time folder
/// another addon owns now refused (`ref_ambiguous`, each folder alone an alternative).
#[test]
fn backup_restore_addon() {
    use common::addons::{addon_folder, toc};
    use omaddons::backup::BackupKind;
    let s = Scene::new();
    let client = s.home.client("_retail_");
    common::wtf::Wtf::at(client.clone())
        .config_wtf("SET locale \"enUS\"\n")
        .saved_variables("Foo", 100)
        .build();
    // Its addon record: WeakAuras owned WeakAuras and WeakAurasOptions; its files:
    // Addon0.lua and Addon1.lua for one character.
    let at = "2026-10-05T08:00:00Z".parse().unwrap();
    let id = common::backups::seed_backup(&client, BackupKind::Manual, at, 2).to_string();
    let c = s.json(
        &[
            "backup", "restore", &id, "-t", "retail", "--addon", "Addon0",
        ],
        0,
    );
    insta::assert_json_snapshot!("backup_restore_addon_ok", s.stable(&c.envelope));
    let src = s.home.env.root().join("src");
    let options = addon_folder(
        &src,
        "WeakAurasOptions",
        &[("WeakAurasOptions.toc", &toc("120105", &[]))],
        &[],
    );
    s.json(
        &["install", &options.display().to_string(), "-t", "retail"],
        0,
    );
    let c = s.json(
        &[
            "backup",
            "restore",
            &id,
            "-t",
            "retail",
            "--addon",
            "WeakAuras",
        ],
        3,
    );
    insta::assert_json_snapshot!("backup_restore_addon_ref_ambiguous", s.stable(&c.envelope));
}

#[test]
fn backup_prune() {
    use common::backups::seed_backup;
    use omaddons::backup::BackupKind;
    let s = Scene::new();
    let client = s.home.client("_retail_");
    let at = |t: &str| t.parse().unwrap();
    // One old day's post-session backups (its last stays) and the newest checkpoint.
    seed_backup(&client, BackupKind::Auto, at("2026-10-01T08:00:00Z"), 1);
    seed_backup(&client, BackupKind::Auto, at("2026-10-01T20:00:00Z"), 1);
    let manual = seed_backup(&client, BackupKind::Manual, at("2026-10-05T10:00:00Z"), 1);
    let env = [("OMADDONS_NOW", "2026-10-15T18:30:00Z")];
    let c = s.json_env(&["backup", "prune", "-t", "retail", "--dry-run"], &env, 0);
    insta::assert_json_snapshot!("backup_prune_dry_run", s.stable(&c.envelope));
    let token = c.envelope["data"]["precondition"]
        .as_str()
        .unwrap()
        .to_owned();
    // Another of that day's backups appears: the run would delete a different set.
    seed_backup(&client, BackupKind::Auto, at("2026-10-01T12:00:00Z"), 1);
    let run = ["backup", "prune", "-t", "retail", "--if-match"];
    let c = s.json_env(&[&run[..], &[token.as_str()]].concat(), &env, 1);
    insta::assert_json_snapshot!("backup_prune_stale", s.stable(&c.envelope));
    let c = s.json_env(&["backup", "prune", "-t", "retail", "--dry-run"], &env, 0);
    let token = c.envelope["data"]["precondition"].as_str().unwrap();
    let c = s.json_env(&[&run[..], &[token]].concat(), &env, 0);
    insta::assert_json_snapshot!("backup_prune_ok", s.stable(&c.envelope));
    let manual = manual.to_string();
    let c = s.json_env(&["backup", "prune", "-t", "retail", &manual], &env, 2);
    insta::assert_json_snapshot!("backup_prune_newest_checkpoint", s.stable(&c.envelope));
}

#[test]
fn backup_export() {
    use omaddons::backup::BackupKind;
    let s = Scene::new();
    let client = s.home.client("_retail_");
    let at = "2026-10-05T08:00:00Z".parse().unwrap();
    let id = common::backups::seed_backup(&client, BackupKind::Manual, at, 2).to_string();
    let dir = s.home.env.root().join("usb");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("retail-wtf.tar.zst").display().to_string();
    let c = s.json(&["backup", "export", &id, &file, "--dry-run"], 0);
    insta::assert_json_snapshot!("backup_export_dry_run", s.stable(&c.envelope));
    let c = s.json(&["backup", "export", &id, &file], 0);
    // The archive holds the backup's id, which each run draws anew.
    let mut envelope = c.envelope.clone();
    let data = &mut envelope["data"];
    assert!(data["sha256"].as_str().is_some_and(|h| h.len() == 64));
    assert!(data["compressed_bytes"].as_u64().is_some_and(|n| n > 0));
    data["sha256"] = "<sha256>".into();
    data["compressed_bytes"] = "<bytes>".into();
    insta::assert_json_snapshot!("backup_export_ok", s.stable(&envelope));
    let c = s.json(&["backup", "export", &id, &file], 4);
    insta::assert_json_snapshot!("backup_export_file_exists", s.stable(&c.envelope));
}

#[test]
fn backup_verify() {
    use omaddons::backup::BackupKind;
    let s = Scene::new();
    let client = s.home.client("_retail_");
    let at = "2026-10-05T08:00:00Z".parse().unwrap();
    let id = common::backups::seed_backup(&client, BackupKind::Manual, at, 2).to_string();
    let c = s.json(&["backup", "verify", &id, "-t", "retail"], 0);
    insta::assert_json_snapshot!("backup_verify_ok", s.stable(&c.envelope));
    let c = s.json(&["backup", "verify", "01ZZZZZZZZ", "-t", "retail"], 3);
    insta::assert_json_snapshot!("backup_verify_not_found", s.stable(&c.envelope));
}

#[test]
fn backup_import() {
    use common::archives::{TarEntry, tar_entries, tar_zst};
    use omaddons::backup::BackupKind;
    let s = Scene::new();
    let client = s.home.client("_retail_");
    let at = "2026-10-05T08:00:00Z".parse().unwrap();
    let id = common::backups::seed_backup(&client, BackupKind::Manual, at, 2).to_string();
    let dir = s.home.env.root().join("usb");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("retail-wtf.tar.zst");
    let arg = file.display().to_string();
    s.json(&["backup", "export", &id, &arg], 0);
    let c = s.json(&["backup", "import", &arg, "-t", "retail"], 0);
    insta::assert_json_snapshot!("backup_import_ok", s.stable(&c.envelope));
    let mut entries = tar_entries(&std::fs::read(&file).unwrap());
    let last = entries.len() - 1;
    entries.insert(last, TarEntry::file("WTF/../evil.txt", b"x"));
    let hostile = dir.join("hostile.tar.zst");
    std::fs::write(&hostile, tar_zst(&entries)).unwrap();
    let c = s.json(
        &[
            "backup",
            "import",
            &hostile.display().to_string(),
            "-t",
            "retail",
        ],
        4,
    );
    insta::assert_json_snapshot!("backup_import_unsafe_archive", s.stable(&c.envelope));
}

#[test]
fn errors() {
    use common::addons::{addon_folder, toc};
    use common::buggrabber::{self, BugDb, BugEntry, NOW, TimeKind, Writer, write_files};

    let s = Scene::new();
    s.install_foo();
    let bug_toc = toc("120105", &["## SavedVariables: BugGrabberDB"]);
    addon_folder(
        &s.home.addons("_retail_"),
        "!BugGrabber",
        &[("!BugGrabber.toc", &bug_toc)],
        &[],
    );
    let at = |file: &str, line| buggrabber::stack_era_c("Foo", file, line);
    let db = BugDb::new(Writer::Modern, 10)
        .entry(BugEntry::new(
            &format!("{} FakeChar pulled", at("boss.lua", 3)),
            9,
            (TimeKind::Unix, NOW - 86_400),
        ))
        .entry(
            BugEntry::new(
                &format!(
                    "{} attempt to index a nil value (field 'x')",
                    at("core.lua", 12)
                ),
                10,
                (TimeKind::Unix, NOW - 60),
            )
            .stack(&[
                format!("{} in function 'Refresh'", at("core.lua", 12)),
                "[C]: in function 'xpcall'".to_owned(),
            ])
            .counter(4),
        );
    let wtf = s.home.client("_retail_").join("WTF");
    let mut file = db.file();
    write_files(&wtf, std::slice::from_ref(&file));
    let env = [("OMADDONS_NOW", "2026-10-15T18:30:00Z")];
    let c = s.json_env(&["errors", "-t", "retail"], &env, 0);
    insta::assert_json_snapshot!("errors_ok", s.stable(&c.envelope));
    let c = s.json_env(&["errors", "-t", "retail", "--check"], &env, 6);
    insta::assert_json_snapshot!("errors_check_failed", s.stable(&c.envelope));
    // A file the game is writing: what came before its end is kept, and the check
    // cannot certify it.
    let cut = file.bytes.len() - 20;
    file.bytes.truncate(cut);
    write_files(&wtf, &[file]);
    let c = s.json_env(&["errors", "-t", "retail", "--check"], &env, 6);
    insta::assert_json_snapshot!("errors_check_incomplete", s.stable(&c.envelope));
    let c = s.json(&["errors", "-t", "nowhere"], 3);
    insta::assert_json_snapshot!("errors_unknown_target", s.stable(&c.envelope));
    let c = s.json(&["errors", "--limit", "0"], 2);
    insta::assert_json_snapshot!("errors_limit_zero", s.stable(&c.envelope));
}

#[test]
fn errors_dismiss() {
    use common::addons::{addon_folder, toc};
    use common::buggrabber::{self, BugDb, BugEntry, NOW, TimeKind, Writer, write_files};

    let s = Scene::new();
    s.install_foo();
    let bug_toc = toc("120105", &["## SavedVariables: BugGrabberDB"]);
    addon_folder(
        &s.home.addons("_retail_"),
        "!BugGrabber",
        &[("!BugGrabber.toc", &bug_toc)],
        &[],
    );
    let db = |session: u32| {
        BugDb::new(Writer::Modern, session).entry(BugEntry::new(
            &format!(
                "{} attempt to call a nil value",
                buggrabber::stack_era_c("Foo", "core.lua", 12)
            ),
            session,
            (TimeKind::Unix, NOW - 60),
        ))
    };
    let wtf = s.home.client("_retail_").join("WTF");
    write_files(&wtf, &[db(10).file()]);
    let env = [("OMADDONS_NOW", "2026-10-15T18:30:00Z")];
    let read = s.json_env(&["errors", "-t", "retail"], &env, 0);
    let token = read.envelope["data"]["targets"][0]["dismiss"]["precondition"]
        .as_str()
        .unwrap()
        .to_owned();
    let c = s.json_env(&["errors", "dismiss", "-t", "retail", "--dry-run"], &env, 0);
    insta::assert_json_snapshot!("errors_dismiss_dry_run", s.stable(&c.envelope));
    // A session ended between the read and the dismissal.
    write_files(&wtf, &[db(11).file()]);
    let bound = ["errors", "dismiss", "-t", "retail", "--if-match", &token];
    let c = s.json_env(&bound, &env, 1);
    insta::assert_json_snapshot!("errors_dismiss_stale", s.stable(&c.envelope));
    let c = s.json_env(&["errors", "dismiss", "-t", "retail"], &env, 0);
    insta::assert_json_snapshot!("errors_dismiss_ok", s.stable(&c.envelope));
    // A file the game is writing is never acknowledged.
    let mut cut = db(12).file();
    let at = cut.bytes.len() - 20;
    cut.bytes.truncate(at);
    write_files(&wtf, &[cut]);
    let c = s.json_env(&["errors", "dismiss", "-t", "retail"], &env, 0);
    insta::assert_json_snapshot!("errors_dismiss_partial", s.stable(&c.envelope));
}

#[test]
fn omarchy_install() {
    use std::os::unix::fs::PermissionsExt;

    let s = Scene::new();
    let root = s.home.env.root().to_path_buf();
    // `PATH` holds a stub `omarchy-shell` alone: no real Omarchy tool is reachable.
    let bin = root.join("omarchy-bin");
    fs::create_dir_all(&bin).unwrap();
    let shell = bin.join("omarchy-shell");
    fs::write(&shell, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).unwrap();
    let (bin, omarchy) = (
        bin.display().to_string(),
        root.join("omarchy").display().to_string(),
    );
    let env = [("PATH", bin.as_str()), ("OMARCHY_PATH", omarchy.as_str())];

    let c = s.json_env(&["omarchy", "install"], &env, 3);
    insta::assert_json_snapshot!("omarchy_install_missing", s.stable(&c.envelope));

    fs::create_dir_all(s.home.env.config().join("omarchy")).unwrap();
    let c = s.json_env(&["omarchy", "install", "--no-enable"], &env, 0);
    let mut stable = s.stable(&c.envelope);
    // The build's own path and version.
    stable["data"]["plugin"]["binary"] = "<binary>".into();
    stable["data"]["plugin"]["version"] = "<version>".into();
    insta::assert_json_snapshot!("omarchy_install_ok", stable);
}
