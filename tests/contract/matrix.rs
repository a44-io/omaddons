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

#[test]
fn install_dry_run_stored() {
    use std::os::unix::fs::PermissionsExt;

    use omaddons::state::plans;

    let s = Scene::new();
    let log = crate::common::tlog::tlog!();
    let now = "2026-10-27T18:00:00Z";
    let argv = ["install", "gh:o/foo", "-t", "retail", "--dry-run", "--json"];
    let run = || {
        support::check(
            log.cmd(&s.home, &argv).env("OMADDONS_NOW", now).run().run,
            0,
        )
    };
    log.step("warm the source metadata and candidate facts");
    run();
    for path in common::plans::plan_files(&s.home.env) {
        fs::remove_file(path).unwrap();
    }
    let doorbell = s.home.env.cache().join("omaddons/doorbell.json");
    let before = fs::read(&doorbell).ok();
    log.tree("client before preview", &s.home.root);

    log.step("the completed preview stores a typed plan and rings no doorbell");
    let c = run();
    let data = &c.envelope["data"];
    let id = data["plan_id"].as_str().unwrap();
    assert!(id.parse::<ulid::Ulid>().is_ok(), "{data}");
    let files = common::plans::plan_files(&s.home.env);
    log.json("stored plan paths", &files);
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].file_stem().unwrap().to_str(), Some(id));
    let plan = plans::load(&files[0]).unwrap();
    log.json("stored plan", &plan);
    assert_eq!(plan.precondition.token, data["precondition"]);
    assert_eq!(
        serde_json::to_value(&plan.approval).unwrap(),
        data["approval"]
    );
    assert_eq!(plan.plan_id.to_string(), id);
    assert_eq!(
        serde_json::to_value(plan.expires_at).unwrap(),
        data["plan_expires_at"]
    );
    assert!(plan.display_argv_agrees(), "{plan:?}");
    assert_eq!(
        plan.display_argv,
        ["omaddons", "install", "gh:o/foo", "-t", "retail"]
    );
    assert_eq!(plan.created_at, now.parse::<jiff::Timestamp>().unwrap());
    assert_eq!(
        plan.expires_at.duration_since(plan.created_at),
        jiff::SignedDuration::from_hours(24)
    );
    assert_eq!(data["approval"]["selection"][0]["addon"], "Foo");
    assert_eq!(data["approval"]["selection"][0]["target_id"], "retail");
    assert_eq!(
        fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    log.assert_eq("doorbell unchanged", &fs::read(&doorbell).ok(), &before);
    log.assert_unchanged("client before preview", &s.home.root);
    log.assert_eq(
        "the stored plan's apply, first",
        &c.envelope["commands"][0],
        &apply_hint(&c.envelope, false),
    );
    insta::assert_json_snapshot!("install_dry_run_stored", s.stable(&c.envelope));
}

#[test]
fn update_dry_run_nothing() {
    let s = Scene::new();
    let log = crate::common::tlog::tlog!();
    let now = "2026-10-27T18:00:00Z";
    log.step("install the current upstream release");
    support::check(
        log.cmd(&s.home, &["install", "gh:o/foo", "-t", "retail", "--json"])
            .env("OMADDONS_NOW", now)
            .run()
            .run,
        0,
    );
    log.step("a preview with everything current stores nothing and warns nothing");
    let c = support::check(
        log.cmd(&s.home, &["update", "--dry-run", "--json"])
            .env("OMADDONS_NOW", now)
            .run()
            .run,
        0,
    );
    let data = &c.envelope["data"];
    assert_eq!(data["plan_id"], Value::Null, "{data}");
    assert_eq!(data["plan_expires_at"], Value::Null, "{data}");
    assert_eq!(data["approval"]["selection"], serde_json::json!([]));
    assert_eq!(c.envelope["warnings"], serde_json::json!([]));
    let files = common::plans::plan_files(&s.home.env);
    log.assert_eq("plan files", &files, &Vec::<std::path::PathBuf>::new());
    assert!(omaddons::app::precondition::is_token(
        data["precondition"].as_str().unwrap()
    ));
    insta::assert_json_snapshot!("update_dry_run_nothing", s.stable(&c.envelope));
}

#[test]
fn install_dry_run_not_stored() {
    use sha2::{Digest, Sha256};

    let s = Scene::new();
    let log = crate::common::tlog::tlog!();
    let now = "2026-10-27T18:00:00Z";
    log.step("fill the live-plan quota with a different intent");
    let seed = support::check(
        log.cmd(
            &s.home,
            &[
                "install",
                "gh:o/foo",
                "-t",
                "retail",
                "--force",
                "--dry-run",
                "--json",
            ],
        )
        .env("OMADDONS_NOW", now)
        .run()
        .run,
        0,
    );
    let seed_path = common::plans::plan_files(&s.home.env).pop().unwrap();
    let template: Value = serde_json::from_slice(&fs::read(&seed_path).unwrap()).unwrap();
    fs::remove_file(seed_path).unwrap();
    common::plans::plant_live(&s.home.env, &template, omaddons::limits::PLANS_MAX_FILES, 0);
    log.json("quota template", &seed.envelope["data"]);
    let plan_hashes = || {
        common::plans::plan_files(&s.home.env)
            .into_iter()
            .map(|path| {
                let hash: String = Sha256::digest(fs::read(&path).unwrap())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                (path, hash)
            })
            .collect::<Vec<_>>()
    };
    let before = plan_hashes();
    log.json("live plan hashes", &before);
    log.step("the ordinary preview and token survive declined admission");
    let c = support::check(
        log.cmd(
            &s.home,
            &["install", "gh:o/foo", "-t", "retail", "--dry-run", "--json"],
        )
        .env("OMADDONS_NOW", now)
        .run()
        .run,
        0,
    );
    let data = &c.envelope["data"];
    assert_eq!(data["plan_id"], Value::Null, "{data}");
    assert_eq!(data["plan_expires_at"], Value::Null, "{data}");
    assert!(data["approval"]["selection"].is_array(), "{data}");
    assert!(omaddons::app::precondition::is_token(
        data["precondition"].as_str().unwrap()
    ));
    let warnings = c.envelope["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0]["code"], "plan_not_stored");
    assert!(
        warnings[0]["message"]
            .as_str()
            .unwrap()
            .contains("store is full")
    );
    assert_eq!(
        c.run
            .stderr
            .lines()
            .filter(|line| line.contains("plan_not_stored"))
            .count(),
        1,
        "{}",
        c.run.context()
    );
    log.assert_eq("live plan files unchanged", &plan_hashes(), &before);
    insta::assert_json_snapshot!("install_dry_run_not_stored", s.stable(&c.envelope));
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
    // `auth` requires its service: a one-sentence usage error under `auth github`.
    let c = s.json(&["auth"], 2);
    insta::assert_json_snapshot!("auth_usage", s.stable(&c.envelope));
    // A mistyped service is corrected in place.
    let c = s.json(&["auth", "gihub"], 2);
    assert_eq!(c.envelope["command"], "auth github");
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "auth", "github"])
    );
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
    // `adopt apply` is the flag the user meant (M4-D27), not a folder named `apply`.
    let c = s.json(&["adopt", "apply", "-t", "retail"], 2);
    assert_eq!(
        support::strings(&c.envelope["error"]["fix_argv"]),
        ["omaddons", "adopt", "--apply", "-t", "retail"]
    );
    insta::assert_json_snapshot!("adopt_apply_as_folder", s.stable(&c.envelope));
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
        // ubs:ignore — comparing an action name, not a secret
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

/// A stored destructive preview keeps the original M3 binding and the approval the
/// command actually rendered; storing it never changes the client or durable state.
#[test]
fn remove_dry_run_stored() {
    let s = Scene::new();
    let log = common::tlog::tlog!();
    let now = "2026-10-27T18:00:00Z";
    log.step("install the addon to remove");
    let zip = s.zip("foo");
    support::check(
        log.cmd(
            &s.home,
            &[
                "install", "gh:o/foo", "--file", &zip, "-t", "retail", "--json",
            ],
        )
        .env("OMADDONS_NOW", now)
        .run()
        .run,
        0,
    );
    log.tree("client before removal preview", &s.home.root);
    log.tree("state before removal preview", &s.home.state());
    log.step("preview the removal and inspect its stored intent and approval");
    let c = support::check(
        log.cmd(
            &s.home,
            &["remove", "Foo", "-t", "retail", "--dry-run", "--json"],
        )
        .env("OMADDONS_NOW", now)
        .run()
        .run,
        0,
    );
    let plan = stored_preview(&s, &c.envelope);
    log.json("stored removal plan", &plan);
    log.assert_eq("stored intent", &plan.intent.command_path(), &"remove");
    log.assert_eq("destructive approval", &plan.approval.routine, &false);
    // Reasons retain the approval registry's canonical order.
    log.assert_eq(
        "approval reasons",
        &c.envelope["data"]["approval"]["reasons"],
        &serde_json::json!(["folders_removed", "destructive"]),
    );
    log.assert_unchanged("client before removal preview", &s.home.root);
    log.assert_unchanged("state before removal preview", &s.home.state());
    log.assert_eq(
        "the stored plan's apply, first, as destructive as a remove",
        &c.envelope["commands"][0],
        &apply_hint(&c.envelope, true),
    );
    insta::assert_json_snapshot!("remove_dry_run_stored", s.stable(&c.envelope));
}

/// `apply` (D5): a stored install applied, previewed again, applied twice, refused for
/// an unknown id and for `--if-match`; and its data's wrapper union holding each
/// command to its own result.
#[test]
fn apply() {
    let s = Scene::new();
    let c = s.json(&["apply", "01ZZZZZZZZZZZZZZZZZZZZZZZZ"], 3);
    assert_eq!(c.envelope["error"]["code"], "plan_not_found");
    insta::assert_json_snapshot!("apply_plan_not_found", s.stable(&c.envelope));

    let zip = s.zip("foo");
    let preview = s.json(
        &[
            "install",
            "gh:o/foo",
            "--file",
            &zip,
            "-t",
            "retail",
            "--dry-run",
        ],
        0,
    );
    let plan = stored_preview(&s, &preview.envelope);
    let id = plan.plan_id.to_string();
    let token = format!("pc1:{}", "0".repeat(64));
    let c = s.json(&["apply", &id, "--if-match", &token], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
    insta::assert_json_snapshot!("apply_if_match", s.stable(&c.envelope));

    let c = s.json(&["apply", &id, "--dry-run"], 0);
    let data = &c.envelope["data"];
    assert_eq!(data["applied"]["command"], "install", "{data}");
    assert_eq!(
        data["approved"],
        serde_json::to_value(&plan.approval).unwrap()
    );
    assert_eq!(data["result"]["dry_run"], true, "{data}");
    assert_eq!(data["outcomes"][0]["status"], "unchanged", "{data}");
    let hint = c.envelope["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["action"] == "apply"); // ubs:ignore — an action name, not a secret
    assert_eq!(
        hint.map(|h| &h["argv"]),
        Some(&serde_json::json!(["omaddons", "apply", id])),
        "{}",
        c.envelope["commands"]
    );
    insta::assert_json_snapshot!("apply_dry_run", s.stable(&c.envelope));

    let c = s.json(&["apply", &id], 0);
    let data = &c.envelope["data"];
    assert_eq!(data["applied"]["command"], "install", "{data}");
    assert_eq!(data["approved"], Value::Null, "{data}");
    assert_eq!(data["outcomes"][0]["status"], "committed", "{data}");
    assert_eq!(data["outcomes"][0]["tx"], data["result"]["tx"], "{data}");
    insta::assert_json_snapshot!("apply_ok", s.stable(&c.envelope));
    let applied = c.envelope.clone();

    let c = s.json(&["apply", &id], 1);
    assert_eq!(c.envelope["error"]["code"], "plan_stale");
    assert_eq!(c.envelope["error"]["details"]["reason"], "already_applied");
    assert_eq!(
        c.envelope["error"]["details"]["txs"],
        serde_json::json!([applied["data"]["result"]["tx"]])
    );
    insta::assert_json_snapshot!("apply_already_applied", s.stable(&c.envelope));

    // The wrapper union: each command with its own result only.
    assert_eq!(validate::validate_envelope(&applied, ""), Ok(()));
    let mut mismatched = applied.clone();
    mismatched["data"]["applied"]["command"] = serde_json::json!("update");
    assert!(
        validate::validate_envelope(&mismatched, "").is_err(),
        "an install's result under applied.command update validates: {mismatched}"
    );
    let mut unknown = applied.clone();
    unknown["data"]["result"]["bogus"] = serde_json::json!(true);
    let refused = validate::validate_envelope(&unknown, "").unwrap_err();
    assert!(refused.contains("/result"), "{refused}");

    // An applied update validates too.
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
    let preview = s.json(
        &[
            "update",
            "WeakAuras",
            "-t",
            "retail",
            "--refresh",
            "--dry-run",
        ],
        0,
    );
    let id = stored_preview(&s, &preview.envelope).plan_id.to_string();
    let c = s.json(&["apply", &id], 0);
    assert_eq!(c.envelope["data"]["applied"]["command"], "update");
    assert_eq!(c.envelope["data"]["outcomes"][0]["status"], "committed");
    assert_eq!(validate::validate_envelope(&c.envelope, ""), Ok(()));
}

/// What `apply` is mistaken for (M4-D27): bare `apply` and a folder's name point at
/// `adopt`, help first; a word that is neither a plan nor such a folder keeps `apply`'s
/// own refusal; and none of them, nor `adopt apply` or `aply`, changes anything.
#[test]
fn apply_hints() {
    let log = common::tlog::tlog!();
    let s = Scene::new();
    // Foo unpacked by hand into retail: a folder omaddons did not install.
    let addons = s.home.addons("_retail_");
    std::fs::create_dir_all(addons.join("Foo")).unwrap();
    std::fs::write(
        addons.join("Foo/Foo.toc"),
        "## Interface: 120105\n## Version: 1\n",
    )
    .unwrap();
    let trees = [
        ("state", s.home.env.state()),
        ("plans", s.home.env.cache().join("omaddons/plans")),
        ("retail", s.home.client("_retail_")),
        ("classic", s.home.client("_classic_")),
    ];
    for (label, root) in &trees {
        log.tree(label, root);
    }
    let fix_and_alternatives = |c: &support::Checked| {
        let error = &c.envelope["error"];
        let alternatives: Vec<Vec<String>> = error["alternatives"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| support::strings(&a["argv"]))
            .collect();
        (
            error["code"].as_str().unwrap().to_owned(),
            support::strings(&error["fix_argv"]),
            alternatives,
        )
    };
    let words = |w: &[&str]| -> Vec<String> { w.iter().map(|a| (*a).to_owned()).collect() };
    let help = words(&["omaddons", "help", "apply"]);
    let adopt_foo = words(&["omaddons", "adopt", "Foo", "-t", "retail"]);

    log.step("bare apply: the help, then adopt --apply");
    let c = s.json(&["apply"], 2);
    log.assert_eq(
        "bare apply",
        &fix_and_alternatives(&c),
        &(
            "usage".to_owned(),
            help.clone(),
            vec![words(&["omaddons", "adopt", "--apply"])],
        ),
    );
    insta::assert_json_snapshot!("apply_bare", s.stable(&c.envelope));

    log.step("apply <an unmanaged folder>, in any case: adopt it where it is");
    let c = s.json(&["apply", "Foo"], 3);
    log.assert_eq(
        "apply Foo",
        &fix_and_alternatives(&c),
        &(
            "not_found".to_owned(),
            help.clone(),
            vec![adopt_foo.clone()],
        ),
    );
    insta::assert_json_snapshot!("apply_folder_hint", s.stable(&c.envelope));
    let c = s.json(&["apply", "foo"], 3);
    log.assert_eq(
        "apply foo",
        &fix_and_alternatives(&c),
        &("not_found".to_owned(), help.clone(), vec![adopt_foo]),
    );

    log.step("a word that is no plan and no folder: apply's own refusal");
    let c = s.json(&["apply", "Nothing"], 2);
    log.assert_eq(
        "short word",
        &fix_and_alternatives(&c).0,
        &"usage".to_owned(),
    );
    let c = s.json(&["apply", "NoSuchFolder"], 3);
    log.assert_eq(
        "long word",
        &fix_and_alternatives(&c).0,
        &"plan_not_found".to_owned(),
    );

    log.step("adopt apply and aply: their fixes, nothing run");
    let c = s.json(&["adopt", "apply"], 2);
    log.assert_eq(
        "adopt apply",
        &fix_and_alternatives(&c).1,
        &words(&["omaddons", "adopt", "--apply"]),
    );
    let c = s.json(&["aply", "01M8Q2VA"], 2);
    log.assert_eq("aply", &fix_and_alternatives(&c).1, &help);

    log.step("no wrong invocation changed a tree");
    for (label, root) in &trees {
        log.assert_unchanged(label, root);
    }
}

/// The `apply` hint a stored preview's envelope must lead its `commands` with.
#[cfg(test)]
fn apply_hint(envelope: &Value, destructive: bool) -> Value {
    let id = envelope["data"]["plan_id"].as_str().unwrap_or_default();
    serde_json::json!({
        "action": "apply",
        "command": format!("omaddons apply {id}"),
        "argv": ["omaddons", "apply", id],
        "destructive": destructive,
    })
}

#[cfg(test)]
fn stored_preview(s: &Scene, envelope: &Value) -> omaddons::state::plans::StoredPlan {
    use omaddons::state::plans;
    let data = &envelope["data"];
    let id: omaddons::model::PlanId = data["plan_id"].as_str().unwrap().parse().unwrap();
    let paths = common::http::paths(&s.home.env);
    let stored = plans::load(&paths.plan(&id)).unwrap();
    assert_eq!(
        stored.precondition.token, data["precondition"],
        "{data}: {stored:?}"
    );
    assert_eq!(
        serde_json::to_value(&stored.approval).unwrap(),
        data["approval"],
        "{data}: {stored:?}"
    );
    assert_eq!(
        serde_json::to_value(stored.expires_at).unwrap(),
        data["plan_expires_at"],
        "{data}: {stored:?}"
    );
    assert!(stored.display_argv_agrees(), "{stored:?}");
    assert_eq!(
        stored.expires_at.duration_since(stored.created_at),
        jiff::SignedDuration::from_hours(24),
        "{stored:?}"
    );
    stored
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
    let preview = s.json(&["undo", "--dry-run"], 0);
    let plan = stored_preview(&s, &preview.envelope);
    assert_eq!(plan.intent.command_path(), "undo", "{plan:?}");
    assert!(plan.intent.destructive(), "{plan:?}");
    assert_eq!(
        serde_json::to_value(&plan.intent).unwrap()["selection"]["run"],
        preview.envelope["data"]["reverted_run"],
        "{plan:?}"
    );
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

/// `plans show` (D9): an applied plan is `completed`, an unknown id `plan_not_found`, and
/// bare `plans` the usage error whose fix is its help.
#[test]
fn plans_show() {
    let s = Scene::new();
    s.install_foo();
    let foo2 = s.zip("foo2");
    let preview = s.json(
        &[
            "install",
            "gh:o/foo",
            "--file",
            &foo2,
            "-t",
            "retail",
            "--dry-run",
        ],
        0,
    );
    let id = stored_preview(&s, &preview.envelope).plan_id.to_string();
    s.json(&["apply", &id], 0);
    let c = s.json(&["plans", "show", &id], 0);
    let data = &c.envelope["data"];
    assert_eq!(data["state"], "completed", "{data}");
    assert_eq!(data["evidence"]["targets"][0]["target"], "retail", "{data}");
    assert_eq!(validate::validate_envelope(&c.envelope, ""), Ok(()));
    insta::assert_json_snapshot!("plans_show_completed", s.stable(&c.envelope));

    let c = s.json(&["plans", "show", "01M8Q2VA7K3N5P8R1T4W6X9Y0Z"], 3);
    assert_eq!(c.envelope["error"]["code"], "plan_not_found");
    insta::assert_json_snapshot!("plans_show_not_found", s.stable(&c.envelope));

    let c = s.json(&["plans"], 2);
    assert_eq!(c.envelope["command"], "plans show");
    assert_eq!(
        support::strings(&c.envelope["error"]["fix_argv"]),
        ["omaddons", "help", "plans"]
    );
    insta::assert_json_snapshot!("plans_usage", s.stable(&c.envelope));
}

/// A repair line (M4-D31), planted before any command writes one: `target: null`, its
/// clients in `targets`, its doctor run and its operations.
#[test]
fn history_repair_line() {
    use crate::common::history::{chmod, plant, repair_line};
    let s = Scene::new();
    s.install_foo();
    let retail = crate::common::state::key_of(&s.home, "retail");
    let credential = crate::common::http::paths(&s.home.env).credential_file("github");
    let line = repair_line(
        "repair",
        &[retail],
        "2026-10-30T18-03-40Z__01M8S000000000000000000000",
        vec![chmod(1, &credential, "0644", "0600")],
    );
    plant(&s.home, &line);
    let c = s.json(&["history"], 0);
    insta::assert_json_snapshot!("history_repair_line", s.stable(&c.envelope));
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

/// The doctor (E3): its report at the envelope's top level, its exits from its own
/// dictionary (M4-D34, M4-D40).
#[test]
fn doctor() {
    // A refusal comes before the run: no run directory, and its fix and alternative run.
    let r = Scene::new();
    let c = r.json(&["doctor", "--budget", "1ms"], 4);
    let error = &c.envelope["error"];
    assert_eq!(
        (&error["code"], &error["details"]["reason"]),
        (&json!("doctor_refused"), &json!("over_budget"))
    );
    assert!(
        !r.home.state().join("doctor").exists(),
        "a refusal records nothing"
    );
    let alternatives = error["alternatives"].as_array().unwrap();
    let argvs = std::iter::once(&error["fix_argv"]).chain(alternatives.iter().map(|a| &a["argv"]));
    for argv in argvs {
        let words: Vec<&str> = argv
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        let ran = r.home.run(&words[1..]);
        assert_eq!(ran.code, 0, "{words:?}\n{}", ran.context());
    }
    // The Scene's two clients, nothing installed: nothing to find.
    let s = Scene::new();
    let c = s.json(&["doctor"], 0);
    assert_eq!(
        (&c.envelope["command"], &c.envelope["state"]),
        (&json!("doctor diagnose"), &json!("DONE_OK"))
    );
    insta::assert_json_snapshot!("doctor_ok", s.stable(&c.envelope));
    let c = s.json(&["doctor", "--only", "fm-nope"], 64);
    insta::assert_json_snapshot!("doctor_unknown_only", s.stable(&c.envelope));
    let c = s.json(&["doctor", "diagnose"], 0);
    insta::assert_json_snapshot!("doctor_diagnose_ok", s.stable(&c.envelope));
    let c = s.json(
        &[
            "doctor",
            "diagnose",
            "--since",
            "2026-01-01T00-00-00Z__01M8R000000000000000000000",
        ],
        66,
    );
    insta::assert_json_snapshot!("doctor_diagnose_since_unknown", s.stable(&c.envelope));
    // `doctor explain`: the failure mode in full and its instances now (none here).
    let c = s.json(
        &["doctor", "explain", "fm-userland_state-target-vanished"],
        0,
    );
    insta::assert_json_snapshot!("doctor_explain_ok", s.stable(&c.envelope));
    let c = s.json(&["doctor", "explain", "fm-nope"], 64);
    insta::assert_json_snapshot!("doctor_explain_unknown", s.stable(&c.envelope));
}

/// A journal a newer omaddons started, on retail (F1): `status`'s issue and `doctor`'s
/// finding in full, and every remediation and explain argv either offers runs.
#[test]
fn status_and_doctor_with_a_journal_too_new() {
    let s = Scene::new();
    common::doctor::plant::journal_too_new(&s.home, "retail");
    let status = s.json(&["status"], 0);
    insta::assert_json_snapshot!("status_with_a_journal_too_new", s.stable(&status.envelope));
    let doctor = s.json(&["doctor"], 1);
    insta::assert_json_snapshot!(
        "doctor_diagnose_with_a_journal_too_new",
        s.stable(&doctor.envelope)
    );
    let retail = status.envelope["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "retail") // ubs:ignore — a client id, not a secret
        .unwrap();
    let finding = &doctor.envelope["findings"][0];
    let argvs = [
        &retail["issues"][0]["remediation"]["argv"],
        &finding["remediation"]["argv"],
        &finding["remediation"]["explain_argv"],
    ];
    for argv in argvs {
        let words: Vec<&str> = argv
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        s.json(&words[1..], 0);
    }
}

/// A damaged cached archive (F6): the deep diagnosis's finding in full. Its digests are
/// deterministic (the fixture's archive, one byte flipped), and `support::check` parses
/// every argv the report offers.
#[test]
fn doctor_deep_damaged_blob() {
    let s = Scene::new();
    common::doctor::plant::damaged_blob(&s.home, "retail");
    let doctor = s.json(&["doctor", "--include-deep"], 1);
    insta::assert_json_snapshot!("doctor_deep_damaged_blob", s.stable(&doctor.envelope));
}

/// `doctor fix` and `doctor diff` (E6) on the Scene's clean home: nothing to repair, the
/// force gate, a stale token, and diff's two modes.
#[test]
fn doctor_fix() {
    let s = Scene::new();
    let c = s.json(&["doctor", "fix", "--dry-run"], 0);
    insta::assert_json_snapshot!("doctor_fix_dry_run_ok", s.stable(&c.envelope));
    let c = s.json(&["doctor", "fix"], 0);
    insta::assert_json_snapshot!("doctor_fix_ok", s.stable(&c.envelope));
    let c = s.json(&["doctor", "fix", "--force"], 64);
    assert_eq!(c.envelope["error"]["details"]["flag"], json!("--force"));
    insta::assert_json_snapshot!("doctor_fix_force_without_yes", s.stable(&c.envelope));
    let stale = format!("pc1:{}", "0".repeat(64));
    let c = s.json(&["doctor", "fix", "--if-match", &stale], 4);
    assert_eq!(
        (
            &c.envelope["error"]["code"],
            &c.envelope["error"]["fix_argv"]
        ),
        (
            &json!("plan_stale"),
            &json!(["omaddons", "doctor", "fix", "--dry-run"])
        )
    );
    insta::assert_json_snapshot!("doctor_fix_plan_stale", s.stable(&c.envelope));
    let c = s.json(&["doctor", "diff"], 0);
    insta::assert_json_snapshot!("doctor_diff_ok", s.stable(&c.envelope));
    let c = s.json(
        &[
            "doctor",
            "diff",
            "2026-01-01T00-00-00Z__01M8R000000000000000000000",
        ],
        66,
    );
    insta::assert_json_snapshot!("doctor_diff_unknown_run", s.stable(&c.envelope));
    // Its fix names the newest finished run, and runs.
    let fix: Vec<&str> = c.envelope["error"]["fix_argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    let ran = s.home.run(&fix[1..]);
    assert_eq!(ran.code, 0, "{fix:?}\n{}", ran.context());
}

/// `undo` of a repair (E7): a committed `fm-test-write` repair (the test fixers, a debug
/// build), its preview, the undo refused once the subject changed, and the undo. The
/// data's `repairs` hold no identity or run id, so nothing beyond the matrix's own
/// redaction varies.
#[test]
fn undo_of_a_repair() {
    use crate::common::doctor::test_fixers::{ENV, REPAIRED, WRITE, plant_write};
    let s = Scene::new();
    let file = plant_write(&s.home, "a.write", b"broken\n");
    let fixed = s.json_env(&["doctor", "fix", "--only", WRITE], &[ENV], 0);
    let tx = fixed.envelope["tx"].as_str().unwrap().to_owned();

    let c = s.json(&["undo", &tx, "--dry-run"], 0);
    assert_eq!(c.envelope["data"]["kind"], json!("repair"));
    insta::assert_json_snapshot!("undo_repair_dry_run", s.stable(&c.envelope));

    fs::write(&file, b"edited\n").unwrap();
    let c = s.json(&["undo", &tx], 4);
    assert_eq!(
        (
            &c.envelope["error"]["code"],
            &c.envelope["error"]["details"]["reason"]
        ),
        (&json!("undo_conflict"), &json!("changed_since_repair"))
    );
    insta::assert_json_snapshot!("undo_repair_changed_since", s.stable(&c.envelope));
    assert_eq!(fs::read(&file).unwrap(), b"edited\n", "nothing moved");

    fs::write(&file, REPAIRED).unwrap();
    let c = s.json(&["undo", &tx], 0);
    assert_eq!(fs::read(&file).unwrap(), b"broken\n", "undone");
    insta::assert_json_snapshot!("undo_repair_ok", s.stable(&c.envelope));
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
    // `backup create` never binds (M4-D22); `update` starts to with D2.
    let c = s.json(
        &["backup", "create", "-t", "retail", "--if-match", &token],
        2,
    );
    insta::assert_json_snapshot!("if_match_unbound_command", s.stable(&c.envelope));
}

/// `install` binds to its preview (M4-D19): a dry run's token, a bound run refused after
/// another commit on the client, and one that runs exactly what a fresh preview showed.
#[test]
fn install_binding() {
    let s = Scene::new();
    s.install_foo();
    let foo2 = s.zip("foo2");
    let install = [
        "install",
        "gh:o/foo",
        "--file",
        foo2.as_str(),
        "-t",
        "retail",
    ];
    let preview = |snapshot: Option<&str>| {
        let c = s.json(&[&install[..], &["--dry-run"]].concat(), 0);
        if let Some(name) = snapshot {
            insta::assert_json_snapshot!(name, s.stable(&c.envelope));
        }
        c.envelope["data"]["precondition"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let token = preview(Some("install_dry_run"));
    // Another install commits on the client between the preview and the run.
    s.json(&["install", &s.zip("both"), "-t", "retail"], 0);
    let c = s.json(&[&install[..], &["--if-match", &token]].concat(), 1);
    insta::assert_json_snapshot!("install_if_match_stale", s.stable(&c.envelope));
    let token = preview(None);
    let c = s.json(&[&install[..], &["--if-match", &token]].concat(), 0);
    insta::assert_json_snapshot!("install_if_match_ok", s.stable(&c.envelope));
}

/// `data.precondition` of a dry run; a token.
#[cfg(test)]
fn precondition_of(c: &support::Checked) -> String {
    c.envelope["data"]["precondition"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// `update` binds to its preview: a commit on the client in between stales it, and a
/// fresh preview's token runs.
#[test]
fn update_binding() {
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
    // Named, so the addon installed in between is no skip decision of this update.
    let update = ["update", "weakauras", "-t", "retail", "--refresh"];
    let c = s.json(&[&update[..], &["--dry-run"]].concat(), 0);
    insta::assert_json_snapshot!("update_dry_run", s.stable(&c.envelope));
    let token = precondition_of(&c);
    s.install_foo();
    let c = s.json(&[&update[..], &["--if-match", &token]].concat(), 1);
    insta::assert_json_snapshot!("update_if_match_stale", s.stable(&c.envelope));
    let token = precondition_of(&s.json(&[&update[..], &["--dry-run"]].concat(), 0));
    let c = s.json(&[&update[..], &["--if-match", &token]].concat(), 0);
    insta::assert_json_snapshot!("update_if_match_ok", s.stable(&c.envelope));
}

/// `copy` binds to its preview and to its source: a file edited in a copied source
/// folder in between stales it (`observed`), and a fresh preview's token runs.
#[test]
fn copy_binding() {
    let s = Scene::new();
    s.json(&["install", "weakauras", "-t", "retail"], 0);
    let copy = ["copy", "--from", "retail", "--to", "classic"];
    let c = s.json(&[&copy[..], &["--dry-run"]].concat(), 0);
    insta::assert_json_snapshot!("copy_dry_run", s.stable(&c.envelope));
    let token = precondition_of(&c);
    // The source's record stays as it was: only its folder changed.
    let toc = s.home.addons("_retail_").join("WeakAuras/WeakAuras.toc");
    let mut text = std::fs::read_to_string(&toc).unwrap();
    text.push_str("## X-Edited: by hand\n");
    std::fs::write(&toc, text).unwrap();
    let c = s.json(&[&copy[..], &["--if-match", &token]].concat(), 1);
    insta::assert_json_snapshot!("copy_if_match_stale", s.stable(&c.envelope));
    let token = precondition_of(&s.json(&[&copy[..], &["--dry-run"]].concat(), 0));
    let c = s.json(&[&copy[..], &["--if-match", &token]].concat(), 0);
    insta::assert_json_snapshot!("copy_if_match_ok", s.stable(&c.envelope));
}

/// `sync` binds to its preview and to the file's bytes: a comment added in between
/// stales it, and a fresh preview's token runs.
#[test]
fn sync_binding() {
    let s = Scene::new();
    let desired = "[family.mainline]\naddons = [\"gh:o/foo\"]\n";
    s.home.write_desired(desired);
    let c = s.json(&["sync", "--dry-run"], 0);
    insta::assert_json_snapshot!("sync_dry_run", s.stable(&c.envelope));
    let token = precondition_of(&c);
    s.home.write_desired(&format!("{desired}# a note\n"));
    let c = s.json(&["sync", "--if-match", &token], 1);
    insta::assert_json_snapshot!("sync_if_match_stale", s.stable(&c.envelope));
    let token = precondition_of(&s.json(&["sync", "--dry-run"], 0));
    let c = s.json(&["sync", "--if-match", &token], 0);
    insta::assert_json_snapshot!("sync_if_match_ok", s.stable(&c.envelope));
}

/// `pin`, `unpin` and an adopting `adopt` bind to their previews (M4-D19): a concurrent
/// pin stales a bound pin, an edit in a folder the adoption records stales it, and the
/// report refuses a token with a fix that runs.
#[test]
fn pin_unpin_and_adopt_bind() {
    let s = Scene::new();
    s.install_foo();
    let pin = ["pin", "Foo", "-t", "retail"];
    let c = s.json(&[&pin[..], &["--dry-run"]].concat(), 0);
    insta::assert_json_snapshot!("pin_dry_run", s.stable(&c.envelope));
    let token = precondition_of(&c);
    s.json(&pin, 0);
    let c = s.json(&[&pin[..], &["--if-match", &token]].concat(), 1);
    insta::assert_json_snapshot!("pin_stale", s.stable(&c.envelope));
    let c = s.json(&["unpin", "Foo", "-t", "retail", "--dry-run"], 0);
    assert!(omaddons::app::precondition::is_token(&precondition_of(&c)));

    // Mystery unpacked by hand, and a catalog entry that names its folder.
    let mystery = s.home.addons("_retail_").join("Mystery");
    std::fs::create_dir_all(&mystery).unwrap();
    std::fs::write(mystery.join("Mystery.toc"), "## Interface: 120105\n").unwrap();
    std::fs::write(mystery.join("Mystery.lua"), "-- mine\n").unwrap();
    let catalog = s.home.env.config().join("omaddons/catalog.d");
    std::fs::create_dir_all(&catalog).unwrap();
    std::fs::write(
        catalog.join("50-mystery.toml"),
        "schema_version = \"1.0\"\n\n[[addon]]\nname = \"Mystery\"\nref = \"gh:o/mystery\"\nfolders = [\"Mystery\"]\n",
    )
    .unwrap();
    let apply = ["adopt", "--apply", "-t", "retail", "--no-verify"];
    let c = s.json(&[&apply[..], &["--dry-run"]].concat(), 0);
    insta::assert_json_snapshot!("adopt_apply_dry_run", s.stable(&c.envelope));
    let token = precondition_of(&c);
    std::fs::write(mystery.join("Mystery.lua"), "-- edited\n").unwrap();
    let c = s.json(&[&apply[..], &["--if-match", &token]].concat(), 1);
    insta::assert_json_snapshot!("adopt_stale", s.stable(&c.envelope));

    let report = ["adopt", "-t", "retail", "--no-verify", "--if-match", &token];
    let c = s.json(&report, 2);
    insta::assert_json_snapshot!("adopt_report_if_match", s.stable(&c.envelope));
    let fix: Vec<&str> = c.envelope["error"]["fix_argv"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .filter_map(Value::as_str)
        .collect();
    s.json(&fix, 0);
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
    let plan = stored_preview(&s, &c.envelope);
    assert_eq!(plan.intent.command_path(), "backup restore", "{plan:?}");
    assert_eq!(
        serde_json::to_value(&plan.intent).unwrap()["backup"],
        id,
        "{plan:?}"
    );
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
    let preview = s.json(
        &[
            "backup",
            "restore",
            &id,
            "-t",
            "retail",
            "--addon",
            "Addon0",
            "--dry-run",
        ],
        0,
    );
    let plan = stored_preview(&s, &preview.envelope);
    assert_eq!(
        serde_json::to_value(&plan.intent).unwrap()["addons"],
        serde_json::json!(["Addon0"]),
        "{plan:?}"
    );
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
    let plan = stored_preview(&s, &c.envelope);
    assert_eq!(plan.intent.command_path(), "backup prune", "{plan:?}");
    assert!(plan.intent.destructive(), "{plan:?}");
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
    // A fixed bound: `archive_too_large` with the number and the constant's name, as
    // every `archive_too_large` gives them (`config_key` instead when config raises it).
    let mut entries = tar_entries(&std::fs::read(&file).unwrap());
    let last = entries.len() - 1;
    let long = format!(
        "WTF/{}",
        "a".repeat(omaddons::limits::BACKUP_IMPORT_MAX_PATH as usize)
    );
    entries.insert(last, TarEntry::file(&long, b"x"));
    let deep = dir.join("long.tar.zst");
    std::fs::write(&deep, tar_zst(&entries)).unwrap();
    let c = s.json(
        &[
            "backup",
            "import",
            &deep.display().to_string(),
            "-t",
            "retail",
        ],
        4,
    );
    insta::assert_json_snapshot!("backup_import_archive_too_large", s.stable(&c.envelope));
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
fn omarchy() {
    let s = Scene::new();
    let root = s.home.env.root().to_path_buf();
    // `PATH` holds a stub `omarchy-shell` alone: no real Omarchy tool is reachable.
    let bin = root.join("omarchy-bin");
    fs::create_dir_all(&bin).unwrap();
    let shell = bin.join("omarchy-shell");
    crate::common::exec::write_executable(&shell, "#!/bin/sh\n");
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

    // Installed, on no section of the bar, the stub shell answering no ping.
    let c = s.json_env(&["omarchy"], &env, 0);
    assert_eq!(c.envelope["command"], "omarchy status");
    let mut stable = s.stable(&c.envelope);
    stable["data"]["plugin"]["binary"] = "<binary>".into();
    stable["data"]["plugin"]["version"] = "<version>".into();
    insta::assert_json_snapshot!("omarchy_status_ok", stable);
    let c = s.json_env(&["omarchy", "status", "--bogus"], &env, 2);
    insta::assert_json_snapshot!("omarchy_status_usage", s.stable(&c.envelope));

    let c = s.json_env(&["omarchy", "remove"], &env, 0);
    insta::assert_json_snapshot!("omarchy_remove_ok", s.stable(&c.envelope));
    let c = s.json_env(&["omarchy", "remove"], &env, 3);
    insta::assert_json_snapshot!("omarchy_remove_missing", s.stable(&c.envelope));
}

/// `-t all` (M4-D13): every client on a command that covers several, `target_ambiguous`
/// on one that lands on one, the reserved id, and a client named `all` winning with
/// `target_id_reserved`.
#[test]
fn target_all() {
    let s = Scene::new();
    let log = crate::common::tlog::tlog!();
    s.install_foo();
    let ids = |envelope: &Value| -> Vec<String> {
        envelope["data"]["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| t["id"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    let reserved = |envelope: &Value| -> Vec<Value> {
        envelope["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|w| w["code"] == "target_id_reserved")
            .cloned()
            .collect()
    };

    log.step("status -t all");
    let c = s.json(&["status", "-t", "all"], 0);
    log.json("envelope", &c.envelope);
    let every = s.json(&["status"], 0);
    assert_eq!(
        ids(&c.envelope),
        ["classic", "retail"],
        "{}",
        c.run.context()
    );
    assert_eq!(ids(&c.envelope), ids(&every.envelope), "the same as no -t");
    insta::assert_json_snapshot!("target_all_status", s.stable(&c.envelope));

    log.step("list -t ALL");
    let c = s.json(&["list", "-t", "ALL"], 0);
    log.json("envelope", &c.envelope);
    assert_eq!(
        ids(&c.envelope),
        ["classic", "retail"],
        "{}",
        c.run.context()
    );

    log.step("install on one target with -t all");
    let zip = s.zip("foo");
    let c = s.json(&["install", "gh:o/foo", "--file", &zip, "-t", "all"], 3);
    log.json("envelope", &c.envelope);
    let ctx = c.run.context();
    let error = &c.envelope["error"];
    assert_eq!(error["code"], "target_ambiguous", "{ctx}");
    assert_eq!(
        error["details"]["candidates"],
        serde_json::json!(["classic", "retail"]),
        "{ctx}"
    );
    let fix = support::strings(&error["fix_argv"]);
    assert!(fix.ends_with(&["-t".into(), "classic".into()]), "{ctx}");
    let alternatives = error["alternatives"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(alternatives.len(), 1, "{ctx}");
    let alternative = support::strings(&alternatives[0]["argv"]);
    assert!(
        alternative.ends_with(&["-t".into(), "retail".into()]),
        "{ctx}"
    );
    insta::assert_json_snapshot!("target_all_on_one_target", s.stable(&c.envelope));

    log.step("targets rename retail all");
    let registry = s.home.state().join("registry.json");
    let before = fs::read(&registry).unwrap();
    let c = s.json(&["targets", "rename", "retail", "all"], 2);
    log.json("envelope", &c.envelope);
    let ctx = c.run.context();
    assert_eq!(c.envelope["error"]["code"], "usage", "{ctx}");
    let message = c.envelope["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("reserved"), "{ctx}");
    insta::assert_json_snapshot!("target_id_all_refused", s.stable(&c.envelope));
    assert_eq!(
        fs::read(&registry).unwrap(),
        before,
        "the registry was written"
    );

    log.step("a client named all, as an older build left it");
    s.json(&["targets", "default", "classic"], 0);
    let text = fs::read_to_string(&registry).unwrap();
    let mut doc: Value = serde_json::from_str(&text).unwrap();
    log.json("registry before", &doc);
    for target in doc["targets"].as_array_mut().into_iter().flatten() {
        // ubs:ignore — comparing a target id, not a secret
        if target["id"] == "classic" {
            target["id"] = Value::from("all");
        }
    }
    log.json("registry after", &doc);
    fs::write(&registry, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    let c = s.json(&["status", "-t", "all"], 0);
    log.json("envelope", &c.envelope);
    let ctx = c.run.context();
    assert_eq!(ids(&c.envelope), ["all"], "{ctx}");
    let warned = reserved(&c.envelope);
    assert_eq!(warned.len(), 1, "{ctx}");
    assert_eq!(
        warned[0]["message"],
        "a client has the id \"all\", so -t all selected that client, not every client",
        "{ctx}"
    );
    let display = warned[0]["fix"].as_str().unwrap_or_default();
    assert!(
        display.contains("omaddons targets rename all <new id>"),
        "{ctx}"
    );
    assert!(warned[0]["fix_argv"].is_null(), "{ctx}");
    insta::assert_json_snapshot!("target_id_reserved", s.stable(&c.envelope));

    log.step("status");
    let c = s.json(&["status"], 0);
    log.json("envelope", &c.envelope);
    assert!(reserved(&c.envelope).is_empty(), "{}", c.run.context());
}

/// A mistyped selector (M4-D14): the closest word that selects a client here, in the
/// user's own command, with the listing as the last alternative.
#[test]
fn target_typos() {
    let s = Scene::new();
    let log = crate::common::tlog::tlog!();
    let listing =
        serde_json::json!({ "display": "omaddons targets", "argv": ["omaddons", "targets"] });
    for (args, fix) in [
        (
            &["status", "-t", "retial"][..],
            &["status", "-t", "retail"][..],
        ),
        (&["list", "--target=Retial"], &["list", "--target=retail"]),
        // A command that changes addons keeps its verb.
        (
            &["install", "weakauras", "-tretial"],
            &["install", "weakauras", "-tretail"],
        ),
        (
            &["copy", "--from", "retial", "--to", "classic"],
            &["copy", "--from", "retail", "--to", "classic"],
        ),
        // A mode alias that selects the Mists Classic client.
        (
            &["status", "-t", "mop-clasic"],
            &["status", "-t", "mop-classic"],
        ),
    ] {
        log.step(&args.join(" "));
        let c = s.json(args, 3);
        log.json("envelope", &c.envelope);
        let ctx = c.run.context();
        let error = &c.envelope["error"];
        assert_eq!(error["code"], "target_not_found", "{ctx}");
        let want: Vec<String> = std::iter::once("omaddons")
            .chain(fix.iter().copied())
            .map(str::to_owned)
            .collect();
        assert_eq!(support::strings(&error["fix_argv"]), want, "{ctx}");
        assert_eq!(error["alternatives"], serde_json::json!([listing]), "{ctx}");
        if matches!(*args, ["status", "-t", "retial"]) {
            assert_eq!(
                error["message"],
                "no target matches \"retial\"; the closest selector is \"retail\"",
                "{ctx}"
            );
            assert_eq!(
                error["details"],
                serde_json::json!({ "selector": "retial" }),
                "{ctx}"
            );
            insta::assert_json_snapshot!("status_target_typo", s.stable(&c.envelope));
        }
    }

    log.step("status -t sodd: no Classic Era client, no guess");
    let c = s.json(&["status", "-t", "sodd"], 3);
    log.json("envelope", &c.envelope);
    let ctx = c.run.context();
    let error = &c.envelope["error"];
    assert_eq!(
        support::strings(&error["fix_argv"]),
        ["omaddons", "targets"],
        "{ctx}"
    );
    assert_eq!(error["alternatives"], serde_json::json!([]), "{ctx}");
    assert_eq!(error["message"], "no target matches \"sodd\"", "{ctx}");

    log.step("the fixes run");
    for fix in [
        &["status", "-t", "retail"][..],
        &["list", "--target=retail"],
    ] {
        let c = s.json(fix, 0);
        log.json("envelope", &c.envelope);
    }
}

/// `completions <SHELL>` (M4-D16): the script as data, a mistyped shell corrected in place,
/// no shell, and `--if-match` refused. The scripts themselves are pinned by
/// `tests/completions.rs`; the success snapshot pins every other key.
#[test]
fn completions() {
    let s = Scene::new();
    let c = s.json(&["completions", "bash"], 0);
    assert_eq!(c.envelope["command"], "completions");
    assert_eq!(c.envelope["data"]["shell"], "bash");
    let script = c.envelope["data"]["script"].as_str().unwrap_or_default();
    let head: String = script.chars().take(200).collect();
    assert!(
        script.starts_with("_omaddons() {") && script.contains("complete -F _omaddons"),
        "the bash script, {} bytes: {head:?}",
        script.len()
    );
    let mut stable = s.stable(&c.envelope);
    stable["data"]["script"] = "<script: tests/snapshots/completions__bash.snap>".into();
    insta::assert_json_snapshot!("completions_bash", stable);

    let c = s.json(&["completions", "bsh"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "completions", "bash"])
    );
    // Alternatives: completions zsh, then completions fish (Levenshtein 1, then 2).
    insta::assert_json_snapshot!("completions_unknown_shell", s.stable(&c.envelope));

    let c = s.json(&["completions"], 2);
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "help", "completions"])
    );
    let token = format!("pc1:{}", "0".repeat(64));
    let c = s.json(&["completions", "bash", "--if-match", &token], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "completions", "bash"])
    );
}

#[test]
fn robot_docs() {
    use common::tlog::tlog;
    let log = tlog!();
    let s = Scene::new();
    let run = |args: &[&str], code: i32| {
        let mut argv = args.to_vec();
        argv.push("--json");
        support::check(log.run(&s.home, &argv).to_run(), code)
    };

    log.step("one schema");
    let c = run(&["robot-docs", "schema", "config", "path"], 0);
    assert_eq!(c.envelope["command"], "robot-docs schema");
    let data = &c.envelope["data"];
    assert!(
        data["envelope"].is_null() && data["events"].is_null() && data["files"].is_null(),
        "{data}"
    );
    assert_eq!(data["commands"]["config path"]["title"], "config path");
    insta::assert_json_snapshot!("robot_docs_schema_one", s.stable(&c.envelope));

    log.step("the full listing");
    let c = run(&["robot-docs", "schema"], 0);
    let data = &c.envelope["data"];
    let keys = |value: &Value| -> Vec<String> {
        value
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default()
    };
    let mut expected: Vec<String> = omaddons::app::COMMANDS
        .iter()
        .map(|row| row.name.to_owned())
        .collect();
    expected.sort();
    assert_eq!(
        keys(&data["commands"]),
        expected,
        "commands are the COMMANDS paths"
    );
    let events: Vec<String> = omaddons::contract::events::EventName::ALL
        .iter()
        .map(|name| name.wire().to_owned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(keys(&data["events"]), events);
    let files: Vec<String> = omaddons::contract::schema::file_schemas()
        .keys()
        .map(|kind| (*kind).to_owned())
        .collect();
    assert_eq!(keys(&data["files"]), files);
    assert!(
        data["envelope"]["properties"].is_object(),
        "the envelope's schema"
    );
    log.json(
        "counts",
        &json!({
            "commands": data["commands"].as_object().map(serde_json::Map::len),
            "events": data["events"].as_object().map(serde_json::Map::len),
            "files": data["files"].as_object().map(serde_json::Map::len),
            "stdout_bytes": c.run.stdout.len(),
        }),
    );

    log.step("unknown path");
    let c = run(&["robot-docs", "schema", "nosuch"], 2);
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        json!(["omaddons", "help", "robot-docs", "schema"])
    );
    insta::assert_json_snapshot!("robot_docs_schema_unknown", s.stable(&c.envelope));
    let c = run(&["robot-docs", "schema", "instal"], 2);
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        json!(["omaddons", "robot-docs", "schema", "install"])
    );

    log.step("the catalog");
    let c = run(&["robot-docs", "errors"], 0);
    let data = &c.envelope["data"];
    let errors = data["error_codes"].as_array().unwrap();
    let warnings = data["warning_codes"].as_array().unwrap();
    assert_eq!(errors.len(), ErrorCode::ALL.len());
    assert_eq!(warnings.len(), omaddons::error::WarningCode::ALL.len());
    log.json(
        "counts",
        &json!({"error_codes": errors.len(), "warning_codes": warnings.len()}),
    );
    let mut projection = s.stable(&c.envelope);
    projection["data"]["error_codes"] = json!(
        errors
            .iter()
            .filter(|row| row["code"] == "network")
            .collect::<Vec<_>>()
    );
    projection["data"]["warning_codes"] = json!(
        warnings
            .iter()
            .filter(|row| row["code"] == "reload_required")
            .collect::<Vec<_>>()
    );
    log.json("projection", &projection);
    insta::assert_json_snapshot!("robot_docs_errors", projection);

    log.step("catalog usage");
    let c = run(&["robot-docs", "errors", "extra"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
    insta::assert_json_snapshot!("robot_docs_errors_usage", s.stable(&c.envelope));

    log.step("bare");
    let c = run(&["robot-docs"], 0);
    assert_eq!(c.envelope["command"], "robot-docs guide");
    assert_eq!(c.envelope["data"]["format"], "markdown");

    log.step("usage at the parent");
    let c = run(&["robot-docs", "--bogus"], 2);
    assert_eq!(c.envelope["command"], "robot-docs");
    assert_eq!(c.envelope["error"]["code"], "usage");
}

#[test]
fn robot_docs_guide() {
    use common::tlog::tlog;
    let log = tlog!();
    let s = Scene::new();
    let run = |args: &[&str], code: i32| {
        let mut argv = args.to_vec();
        argv.push("--json");
        support::check(log.run(&s.home, &argv).to_run(), code)
    };
    let without_meta = |envelope: &Value| {
        let mut envelope = envelope.clone();
        if let Some(object) = envelope.as_object_mut() {
            object.remove("meta");
        }
        envelope
    };

    log.step("the guide");
    let guide = run(&["robot-docs", "guide"], 0);
    assert_eq!(guide.envelope["command"], "robot-docs guide");
    assert_eq!(guide.envelope["data"]["format"], "markdown");
    let content = guide.envelope["data"]["content"].as_str().unwrap_or("");
    assert!(content.starts_with("---\nname: omaddons\n"), "{content}");
    // tests/skill.rs pins the content whole; here it is redacted, so a guide change
    // re-accepts no contract snapshot.
    let mut redacted = s.stable(&guide.envelope);
    redacted["data"]["content"] = json!("<guide>");
    insta::assert_json_snapshot!("robot_docs_guide_ok", redacted);

    log.step("bare robot-docs is the guide");
    let bare = run(&["robot-docs"], 0);
    assert_eq!(without_meta(&bare.envelope), without_meta(&guide.envelope));

    log.step("an extra argument");
    let c = run(&["robot-docs", "guide", "extra"], 2);
    assert_eq!(c.envelope["error"]["code"], "usage");
    insta::assert_json_snapshot!("robot_docs_guide_extra_argument", s.stable(&c.envelope));
}

/// `doctor undo` (E7 s4) on a home with one committed `fm-test-write` repair (the test
/// fixers, a debug build): its dry run, `latest` refused, `undo --run` of a doctor run id
/// refused, and the undo.
#[test]
fn doctor_undo() {
    use crate::common::doctor::test_fixers::{ENV, WRITE, plant_write};
    let s = Scene::new();
    let file = plant_write(&s.home, "a.write", b"broken\n");
    let fixed = s.json_env(&["doctor", "fix", "--only", WRITE], &[ENV], 0);
    let run = fixed.envelope["run_id"].as_str().unwrap().to_owned();
    let tx = fixed.envelope["tx"].as_str().unwrap().to_owned();

    let c = s.json(&["doctor", "undo", "--dry-run"], 0);
    assert_eq!(c.envelope["tx"], json!(tx));
    assert_eq!(
        c.envelope["commands"][0]["argv"],
        json!(["omaddons", "undo", tx, "--dry-run"])
    );
    insta::assert_json_snapshot!("doctor_undo_dry_run", s.stable(&c.envelope));

    let c = s.json(&["doctor", "undo", "latest"], 64);
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        json!(["omaddons", "doctor", "undo"])
    );
    insta::assert_json_snapshot!("doctor_undo_latest", s.stable(&c.envelope));

    let c = s.json(&["undo", "--run", &run], 2);
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        json!(["omaddons", "doctor", "undo", run])
    );
    insta::assert_json_snapshot!("undo_run_doctor_run_id", s.stable(&c.envelope));

    let c = s.json(&["doctor", "undo"], 0);
    assert_eq!(c.envelope["reverted_run"], json!(run));
    assert_eq!(fs::read(&file).unwrap(), b"broken\n", "undone");
    insta::assert_json_snapshot!("doctor_undo_ok", s.stable(&c.envelope));
}
