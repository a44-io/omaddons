//! e2e suite: previews bound by `--if-match`: M3's five commands, `install`, `update`,
//! `sync` and `copy` (D2), and `pin`, `unpin` and `adopt` (D3).
//!
//! Preview binding (M3-D46, M4-D19) end to end: a `--dry-run` of `remove`, `undo`,
//! `backup prune`, `install`, `pin`, `unpin` or an adopting `adopt` (and the others)
//! returns `data.precondition`, and the real run with `--if-match <it>` does exactly what
//! the preview showed or nothing. Each scenario changes the world between preview and run
//! (a commit on the target, before the run plans or between its plan and the lock; a
//! SavedVariables file the undo would restore; a new backup, or a victim's metadata
//! rewritten; a file edited in a folder `adopt` records or `pin --version` replaces) and
//! checks the refusal (`plan_stale`, not retryable, `details.changed`, the preview as
//! `fix_argv`) and that nothing moved. The D2 and D3 scenarios log through `TestLog`.

mod common;

use common::binding as b;
use common::engine::Scene;
use common::graph::Folder;
use common::pause::Pause;
use prune_support::{at_noon, backups, prune_preview, three_days};
use restore_support::{restorable, restore_args};
use serde_json::{Value, json};
use support::{argv, bound_dry, exact_foo, preview, publish_foo, token};

#[cfg(test)]
mod support {
    use serde_json::Value;

    use super::common::addons::{toc, unpack};
    use super::common::engine::Scene;
    use super::common::graph::folder_toc;
    use super::common::home::Home;
    use super::common::http::{Asset, gh_release};

    pub fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    /// `<args> --dry-run --json`'s token, checked well formed.
    pub fn preview(scene: &Scene, args: &[&str]) -> String {
        let mut run = argv(args);
        run.extend(["--dry-run", "--json"].map(str::to_owned));
        let envelope = scene.run(&run, "").expect(0);
        assert_eq!(envelope["data"]["dry_run"], true);
        token(&envelope)
    }

    /// An envelope's `data.precondition`, which must be a token.
    pub fn token(envelope: &Value) -> String {
        let token = envelope["data"]["precondition"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            omaddons::app::precondition::is_token(&token),
            "{token}: {envelope}"
        );
        token
    }

    /// `<args> --dry-run --if-match <token> --json`: a bound dry run.
    pub fn bound_dry(args: &[&str], token: &str) -> Vec<String> {
        let mut run = argv(args);
        run.extend(["--dry-run", "--if-match", token, "--json"].map(str::to_owned));
        run
    }

    /// Release `tag` of `gh:o/foo` as its latest, as `tests/pin.rs`'s `Rel` builds it:
    /// one asset `Foo.zip` with a TOC per (file suffix, interface), so each client reads
    /// its own, and a `core.lua` that differs per tag at the same size. Its release.json
    /// names no flavor, so the TOCs decide.
    pub fn publish_foo(home: &Home, tag: &str, tocs: &[(&str, &str)]) {
        let mut files: Vec<(String, String)> = tocs
            .iter()
            .map(|(suffix, interface)| {
                (
                    format!("Foo/Foo{suffix}.toc"),
                    toc(interface, &["## Title: Foo", &format!("## Version: {tag}")]),
                )
            })
            .collect();
        files.push(("Foo/core.lua".into(), format!("-- Foo {tag}")));
        let entries = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let assets: [Asset<'_>; 1] = [("Foo.zip", &[], entries)];
        gh_release(&home.http, "https://github.com/o/foo", tag, &assets);
    }

    /// Foo v2 released (`Foo` and `Foo_Options`) and listed in the catalog, and the same
    /// files unpacked by hand, as `tests/adopt.rs`'s `foo_home` builds them: an `exact`
    /// group.
    pub fn exact_foo(scene: &Scene) {
        let folders = [
            super::retail_folder("Foo"),
            super::retail_folder("Foo_Options"),
        ];
        scene.release("foo", "v2", &folders);
        scene.catalog(&[("Foo", "foo", &["Foo", "Foo_Options"])]);
        let files: Vec<(String, String)> = folders
            .iter()
            .map(|f| (format!("{0}/{0}.toc", f.name), folder_toc(f, "v2")))
            .collect();
        let entries: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        unpack(&scene.addons(), &entries);
    }
}

/// `args` with `--if-match <token> --json`.
fn bound(args: &[&str], token: &str) -> Vec<String> {
    let mut run = argv(args);
    run.extend(["--if-match", token, "--json"].map(str::to_owned));
    run
}

/// A refusal of a bound run: `plan_stale`, not retryable, naming `changed`, with the
/// preview as its fix.
fn assert_stale(envelope: &Value, changed: &[&str], preview: &[&str]) {
    let error = &envelope["error"];
    assert_eq!(error["code"], "plan_stale", "{envelope}");
    assert_eq!(error["retryable"], false, "{envelope}");
    assert_eq!(error["details"]["changed"], json!(changed), "{envelope}");
    assert_eq!(error["fix_argv"], json!(preview), "{envelope}");
}

const REMOVE: &[&str] = &["remove", "Foo", "-t", "retail"];
const REMOVE_PREVIEW: &[&str] = &["omaddons", "remove", "Foo", "-t", "retail", "--dry-run"];

#[test]
fn a_remove_runs_as_previewed() {
    let scene = Scene::new();
    scene.install("gh:o/foo", "v1");
    let token = preview(&scene, REMOVE);
    assert_eq!(
        preview(&scene, REMOVE),
        token,
        "an unchanged home, the same token"
    );
    let envelope = scene.run(&bound(REMOVE, &token), "").expect(0);
    assert_eq!(envelope["data"]["items"][0]["status"], "removed");
    assert_eq!(
        envelope["data"]["precondition"],
        Value::Null,
        "a real run's"
    );
    assert!(!scene.addons().join("Foo").exists());
}

#[test]
fn a_remove_after_a_concurrent_install_changes_nothing() {
    let scene = Scene::new();
    scene.install("gh:o/foo", "v1");
    let token = preview(&scene, REMOVE);
    scene.install("gh:o/bar", "bar");
    let (folders, generation) = (scene.folders(), scene.generation());
    let envelope = scene.run(&bound(REMOVE, &token), "").expect(1);
    assert_stale(&envelope, &["targets"], REMOVE_PREVIEW);
    assert_eq!((scene.folders(), scene.generation()), (folders, generation));

    // The fix previews again, and that token runs.
    let token = preview(&scene, REMOVE);
    scene.run(&bound(REMOVE, &token), "").expect(0);
    assert!(!scene.addons().join("Foo").exists());
}

#[test]
fn a_commit_between_a_bound_removes_plan_and_its_lock_changes_nothing() {
    let scene = Scene::new();
    scene.install("gh:o/foo", "v1");
    let token = preview(&scene, REMOVE);
    let pause = Pause::new(scene.home.env.root());
    let run = bound(REMOVE, &token);
    let args: Vec<&str> = run
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json")
        .collect();
    let child = scene.home.spawn(
        &args,
        &[
            ("OMADDONS_FAILPOINTS", "engine.snapshot.prestaged=pause"),
            pause.env(),
        ],
    );
    pause.reached("engine.snapshot.prestaged");
    scene.install("gh:o/bar", "bar");
    let (folders, generation) = (scene.folders(), scene.generation());
    pause.resume("engine.snapshot.prestaged");
    let envelope = common::home::wait(child, &args).expect(1);
    assert_stale(&envelope, &["targets"], REMOVE_PREVIEW);
    assert_eq!((scene.folders(), scene.generation()), (folders, generation));
    // Without --if-match the same race commits, as before: binding is opt-in.
    let pause = Pause::new(scene.home.env.root());
    let child = scene.home.spawn(
        REMOVE,
        &[
            ("OMADDONS_FAILPOINTS", "engine.snapshot.prestaged=pause"),
            pause.env(),
        ],
    );
    pause.reached("engine.snapshot.prestaged");
    scene
        .run(&argv(&["pin", "Bar", "-t", "retail", "--json"]), "")
        .expect(0);
    pause.resume("engine.snapshot.prestaged");
    common::home::wait(child, REMOVE).expect(0);
    assert!(!scene.addons().join("Foo").exists());
}

#[test]
fn an_undo_after_a_later_commit_changes_nothing() {
    let scene = Scene::new();
    scene.install("gh:o/foo", "v1");
    let token = preview(&scene, &["undo"]);
    assert_eq!(preview(&scene, &["undo"]), token);
    // Bare undo would now revert the newer install: another selection.
    scene.install("gh:o/bar", "bar");
    let (folders, generation) = (scene.folders(), scene.generation());
    let envelope = scene.run(&bound(&["undo"], &token), "").expect(1);
    assert_stale(
        &envelope,
        &["targets", "selection", "observed"],
        &["omaddons", "undo", "--dry-run"],
    );
    assert_eq!((scene.folders(), scene.generation()), (folders, generation));
    assert!(
        scene.addons().join("Bar").is_dir(),
        "the newer install stands"
    );
}

#[test]
fn an_undo_of_one_transaction_binds_to_its_targets_records() {
    let scene = Scene::new();
    let tx = scene.install("gh:o/foo", "v1")["data"]["tx"]
        .as_str()
        .unwrap()
        .to_owned();
    let undo = ["undo", tx.as_str()];
    let preview_argv = ["omaddons", "undo", tx.as_str(), "--dry-run"];
    let token = preview(&scene, &undo);
    scene.install("gh:o/bar", "bar");
    let envelope = scene.run(&bound(&undo, &token), "").expect(1);
    assert_stale(&envelope, &["targets"], &preview_argv);
    assert!(scene.addons().join("Foo").is_dir());

    // Previewed again, it runs; its own undo line is the newest.
    let token = preview(&scene, &undo);
    let envelope = scene.run(&bound(&undo, &token), "").expect(0);
    assert_eq!(envelope["data"]["reverted"]["tx"], tx.as_str());
    assert_eq!(envelope["data"]["precondition"], Value::Null);
    assert!(!scene.addons().join("Foo").exists());
}

#[test]
fn an_undo_whose_saved_variables_changed_since_its_preview_changes_nothing() {
    let scene = Scene::new().with_saved_variables(&["Foo"]);
    scene.install("gh:o/foo", "v1");
    scene.install("gh:o/foo", "v2");
    scene.play(&["Foo"]);
    let token = preview(&scene, &["undo"]);
    // The game wrote them again: the files the undo would restore are not the ones seen.
    scene.play(&["Foo"]);
    let (folders, wtf) = (scene.folders(), scene.wtf());
    let envelope = scene.run(&bound(&["undo"], &token), "").expect(1);
    assert_stale(&envelope, &["observed"], &["omaddons", "undo", "--dry-run"]);
    assert_eq!((scene.folders(), scene.wtf()), (folders, wtf));
}

#[test]
fn a_commit_between_a_bound_undos_plan_and_its_lock_changes_nothing() {
    let scene = Scene::new();
    scene.install("gh:o/foo", "v1");
    let token = preview(&scene, &["undo"]);
    let pause = Pause::new(scene.home.env.root());
    let args = ["undo", "--if-match", token.as_str()];
    let child = scene.home.spawn(
        &args,
        &[
            ("OMADDONS_FAILPOINTS", "engine.snapshot.prestaged=pause"),
            pause.env(),
        ],
    );
    pause.reached("engine.snapshot.prestaged");
    scene.install("gh:o/bar", "bar");
    let (folders, generation) = (scene.folders(), scene.generation());
    pause.resume("engine.snapshot.prestaged");
    let envelope = common::home::wait(child, &args).expect(1);
    assert_stale(&envelope, &["targets"], &["omaddons", "undo", "--dry-run"]);
    assert_eq!((scene.folders(), scene.generation()), (folders, generation));
}

#[test]
fn a_token_for_another_command_is_stale() {
    let scene = Scene::new();
    scene.install("gh:o/foo", "v1");
    let token = preview(&scene, REMOVE);
    let envelope = scene.run(&bound(&["undo"], &token), "").expect(1);
    let changed = envelope["error"]["details"]["changed"].as_array().unwrap();
    assert!(changed.contains(&json!("command")), "{envelope}");
    assert!(scene.addons().join("Foo").is_dir());
    // A dismissal takes no other command's token either.
    let dismiss = bound(&["errors", "dismiss", "-t", "retail"], &token);
    let envelope = scene.run(&dismiss, "").expect(1);
    let changed = envelope["error"]["details"]["changed"].as_array().unwrap();
    assert!(changed.contains(&json!("command")), "{envelope}");
}

#[test]
fn if_match_goes_only_with_the_binding_commands_and_takes_only_tokens() {
    let scene = Scene::new();
    scene.install("gh:o/foo", "v1");
    let token = preview(&scene, REMOVE);
    // `backup create` never binds (M4-D22).
    let unbound = ["backup", "create", "-t", "retail"];
    let envelope = scene.run(&bound(&unbound, &token), "").expect(2);
    let error = &envelope["error"];
    assert_eq!(error["code"], "usage");
    let message = error["message"].as_str().unwrap();
    for name in omaddons::app::precondition::BINDING_COMMANDS {
        assert!(message.contains(name), "{message}");
    }
    assert_eq!(
        error["fix_argv"],
        json!(["omaddons", "help", "backup", "create"])
    );
    // A command that only reads: its fix drops the flag.
    let envelope = scene.run(&bound(&["list"], &token), "").expect(2);
    assert_eq!(envelope["error"]["fix_argv"], json!(["omaddons", "list"]));

    let envelope = scene.run(&bound(REMOVE, "pc1:nope"), "").expect(2);
    assert_eq!(envelope["error"]["code"], "usage");
    assert_eq!(envelope["error"]["fix_argv"], json!(REMOVE_PREVIEW));
    assert!(scene.addons().join("Foo").is_dir());
}

#[test]
fn a_dry_run_with_if_match_previews_afresh() {
    let scene = Scene::new();
    scene.install("gh:o/foo", "v1");
    let stale = preview(&scene, REMOVE);
    scene.install("gh:o/bar", "bar");
    let mut run = bound(REMOVE, &stale);
    run.push("--dry-run".to_owned());
    let envelope = scene.run(&run, "").expect(0);
    let fresh = token(&envelope);
    assert_ne!(fresh, stale);
    assert_eq!(fresh, preview(&scene, REMOVE));
    assert!(scene.addons().join("Foo").is_dir());
}

// ---- `install` (D2): an engine command's token ----------------------------------------

/// The update to Foo 2, offline: `install gh:o/foo --file <v2.zip> -t retail --offline`.
fn install_v2(scene: &Scene) -> Vec<String> {
    let v2 = scene.zip("v2");
    argv(&[
        "install",
        "gh:o/foo",
        "--file",
        &v2,
        "-t",
        "retail",
        "--offline",
    ])
}

/// `run` with `extra`, then `--json`.
fn with(run: &[String], extra: &[&str]) -> Vec<String> {
    let mut run = run.to_vec();
    run.extend(extra.iter().map(|a| (*a).to_owned()));
    run.push("--json".to_owned());
    run
}

/// The fix of a refused `run`: its preview, `--dry-run` last.
fn preview_of(run: &[String]) -> Vec<String> {
    let mut preview = argv(&["omaddons"]);
    preview.extend(run.iter().cloned());
    preview.push("--dry-run".to_owned());
    preview
}

fn strs(argv: &[String]) -> Vec<&str> {
    argv.iter().map(String::as_str).collect()
}

#[test]
fn an_install_binds_to_its_preview() {
    let log = common::tlog::tlog!();
    let scene = Scene::new();
    log.env(&scene);
    let state = scene.home.state();
    log.step("install Foo 1");
    log.run(&scene, &scene.install_args("gh:o/foo", "v1")).ok();
    let install = install_v2(&scene);
    let fix = preview_of(&install);

    log.step("preview");
    let dry = log.run(&scene, &with(&install, &["--dry-run"]));
    let previewed = token(dry.ok().envelope());
    log.json("token", &previewed);

    log.step("preview again");
    let again = log.run(&scene, &with(&install, &["--dry-run"]));
    log.assert_eq(
        "an unchanged home, the same token",
        &token(again.ok().envelope()),
        &previewed,
    );

    log.step("another install commits on the client");
    log.run(&scene, &scene.install_args("gh:o/bar", "bar")).ok();
    log.tree("addons", &scene.addons());
    log.tree("state", &state);

    log.step("bound run, stale");
    let stale = log.run(&scene, &with(&install, &["--if-match", &previewed]));
    assert_stale(
        stale.fails(1, "plan_stale").envelope(),
        &["targets"],
        &strs(&fix),
    );
    log.assert_unchanged("addons", &scene.addons());
    log.assert_unchanged("state", &state);

    log.step("preview again, then the bound run");
    let dry = log.run(&scene, &with(&install, &["--dry-run"]));
    let fresh = token(dry.ok().envelope());
    log.json("token", &fresh);
    let ran = log.run(&scene, &with(&install, &["--if-match", &fresh]));
    ran.ok();
    log.assert_eq(
        "a real run's precondition",
        &ran.data()["precondition"],
        &Value::Null,
    );
    log.assert_eq(
        "the items the preview showed",
        &ran.data()["items"],
        &dry.data()["items"],
    );
    let expected = Scene::new();
    expected.install("gh:o/foo", "v1");
    expected.install("gh:o/foo", "v2");
    log.assert_eq(
        "Foo's folders, as an unbound update leaves them",
        &scene.folders(),
        &expected.folders(),
    );

    log.step("the same token again");
    log.tree("addons", &scene.addons());
    log.tree("state", &state);
    let again = log.run(&scene, &with(&install, &["--if-match", &fresh]));
    assert_stale(
        again.fails(1, "plan_stale").envelope(),
        &["targets", "selection", "observed"],
        &strs(&fix),
    );
    log.assert_unchanged("addons", &scene.addons());
    log.assert_unchanged("state", &state);
}

#[test]
fn a_bound_install_dry_run_compares() {
    let log = common::tlog::tlog!();
    let scene = Scene::new();
    log.env(&scene);
    let state = scene.home.state();
    log.step("install Foo 1");
    log.run(&scene, &scene.install_args("gh:o/foo", "v1")).ok();
    let install = install_v2(&scene);
    let dry_run = with(&install, &["--dry-run"]);

    log.step("preview");
    let previewed = token(log.run(&scene, &dry_run).ok().envelope());
    log.json("token", &previewed);
    log.tree("addons", &scene.addons());
    log.tree("state", &state);

    log.step("the human preview names the token under -v only");
    let human = [install.as_slice(), &argv(&["--dry-run"])].concat();
    let plain = log.run(&scene, &human);
    let verbose = log.run(&scene, &[human.as_slice(), &argv(&["-v"])].concat());
    let line = format!("Run it bound to this preview: --if-match {previewed}\n");
    log.assert_eq(
        "(without -v, with -v) contains the token line",
        &(
            plain.ok().stdout().contains(&line),
            verbose.ok().stdout().contains(&line),
        ),
        &(false, true),
    );
    for (name, output) in [("plain", plain.ok()), ("verbose", verbose.ok())] {
        let text = common::plans::redact_ulids(output.stdout());
        log.assert_eq(
            &format!("{name} preview ends with the stored-plan line"),
            &text.lines().last(),
            &Some("Plan <ulid> stored for 24 hours."),
        );
    }

    log.step("bound dry run, matching");
    let bound_dry = [dry_run.as_slice(), &argv(&["--if-match", &previewed])].concat();
    let matching = log.run(&scene, &bound_dry);
    log.assert_eq(
        "the same token",
        &token(matching.ok().envelope()),
        &previewed,
    );
    log.assert_unchanged("addons", &scene.addons());
    log.assert_unchanged("state", &state);

    log.step("another install commits on the client");
    log.run(&scene, &scene.install_args("gh:o/bar", "bar")).ok();
    log.tree("addons", &scene.addons());
    log.tree("state", &state);

    log.step("bound dry run, stale");
    let stale = log.run(&scene, &bound_dry);
    assert_stale(
        stale.fails(1, "plan_stale").envelope(),
        &["targets"],
        &strs(&preview_of(&install)),
    );
    log.assert_unchanged("addons", &scene.addons());
    log.assert_unchanged("state", &state);
}

/// `install` asks its sources again (M4-D20): a release published after the preview
/// makes the bound run stale, and the preview that fix runs shows the new release.
#[test]
fn a_release_since_the_preview_stales_a_bound_install() {
    use common::home::Home;
    use common::http::{Asset, gh_release};

    let log = common::tlog::tlog!();
    let home = Home::new(&["wow"]);
    log.env(&home);
    let release = |tag: &str, version: &str| {
        let toc = format!("## Interface: 120105\n## Version: {version}\n");
        let foo: [Asset<'_>; 1] = [(
            "Foo.zip",
            &[("mainline", 120_105)],
            vec![("Foo/Foo.toc", toc.as_str())],
        )];
        gh_release(&home.http, "https://github.com/o/foo", tag, &foo);
    };
    let install = argv(&["install", "gh:o/foo", "-t", "retail"]);
    log.step("v1 is the latest release");
    release("v1", "1");

    log.step("preview");
    let dry = log.run(&home, &with(&install, &["--dry-run"]));
    let previewed = token(dry.ok().envelope());
    log.json("token", &previewed);
    log.assert_eq(
        "the preview installs v1",
        &dry.data()["items"][0]["to_version"],
        &json!("v1"),
    );

    log.step("v2 is published");
    release("v2", "2");
    let addons = home.addons("_retail_");
    log.tree("addons", &addons);

    log.step("bound run, stale");
    let stale = log.run(&home, &with(&install, &["--if-match", &previewed]));
    let changed = &stale.fails(1, "plan_stale").envelope()["error"]["details"]["changed"];
    log.assert_eq(
        "the release changed the selection",
        &changed
            .as_array()
            .is_some_and(|c| c.contains(&json!("selection"))),
        &true,
    );
    log.assert_unchanged("addons", &addons);

    log.step("the fix previews v2");
    let dry = log.run(&home, &with(&install, &["--dry-run"]));
    log.assert_eq(
        "the new preview installs v2",
        &dry.ok().data()["items"][0]["to_version"],
        &json!("v2"),
    );
}

#[test]
fn a_bound_install_without_a_client_binds_the_client_the_probe_chose() {
    use common::home::Home;
    use common::http::{Asset, gh_release};

    let log = common::tlog::tlog!();
    let home = Home::new(&["wow", "wow_classic"]);
    log.env(&home);
    log.step("seed a retail-only release");
    let foo: [Asset<'_>; 1] = [(
        "Foo.zip",
        &[("mainline", 120_105)],
        vec![("Foo/Foo.toc", "## Interface: 120105\n## Version: 1\n")],
    )];
    gh_release(&home.http, "https://github.com/o/foo", "v1", &foo);
    let classic = home.addons("_classic_");
    log.tree("classic addons", &classic);

    log.step("preview without -t");
    let dry = log.run(&home, &["install", "gh:o/foo", "--dry-run", "--json"]);
    let previewed = token(dry.ok().envelope());
    log.json("token", &previewed);
    log.assert_eq(
        "the probe chose retail",
        &dry.data()["target"]["id"],
        &json!("retail"),
    );

    log.step("bound run without -t");
    let ran = log.run(
        &home,
        &["install", "gh:o/foo", "--if-match", &previewed, "--json"],
    );
    ran.ok();
    log.assert_eq(
        "installed on retail",
        &(
            &ran.data()["target"]["id"],
            &ran.data()["items"][0]["status"],
        ),
        &(&json!("retail"), &json!("installed")),
    );
    log.assert_eq(
        "Foo on retail",
        &home.addons("_retail_").join("Foo/Foo.toc").is_file(),
        &true,
    );
    log.assert_unchanged("classic addons", &classic);

    log.step("the same token again: the run compared it");
    let retail = home.addons("_retail_");
    log.tree("retail addons", &retail);
    let again = log.run(
        &home,
        &["install", "gh:o/foo", "--if-match", &previewed, "--json"],
    );
    let changed = &again.fails(1, "plan_stale").envelope()["error"]["details"]["changed"];
    log.assert_eq(
        "the target moved since the preview",
        &changed
            .as_array()
            .is_some_and(|c| c.contains(&json!("targets"))),
        &true,
    );
    log.assert_unchanged("retail addons", &retail);
    log.assert_unchanged("classic addons", &classic);
}

// ---- `update` and `sync` (D2): the memo's releases, skip decisions, the file's bytes --

/// A release's folder, built for retail.
const fn retail_folder(name: &'static str) -> Folder<'static> {
    Folder {
        name,
        interface: "120105",
        deps: &[],
        optional: &[],
    }
}

/// A retail client with Foo v1 installed from `gh:o/foo`, v2 published since, and the
/// memo aged, so the next `update` sees v2.
fn foo_behind(log: &common::tlog::TestLog) -> Scene {
    let scene = Scene::new();
    log.env(&scene);
    scene.release("foo", "v1", &[retail_folder("Foo")]);
    log.run(&scene, &["install", "gh:o/foo", "-t", "retail", "--json"])
        .ok();
    scene.release("foo", "v2", &[retail_folder("Foo")]);
    b::age_release_memo(&scene.home, jiff::SignedDuration::from_hours(1));
    scene
}

/// Another install commits on the client: Bar v1, current, from `gh:o/bar`.
fn install_bar(log: &common::tlog::TestLog, scene: &Scene) {
    scene.release("bar", "v1", &[retail_folder("Bar")]);
    log.run(scene, &["install", "gh:o/bar", "-t", "retail", "--json"])
        .ok();
}

/// What a stale run must leave alone, recorded: the AddOns and `<state>/targets` trees,
/// and the transactions `history.jsonl` lists (returned).
fn record_before(log: &common::tlog::TestLog, scene: &Scene) -> Vec<String> {
    log.tree("addons", &scene.addons());
    log.tree("records", &scene.home.state().join("targets"));
    let history = scene.history_txs();
    log.json("history", &history);
    history
}

fn assert_left_alone(log: &common::tlog::TestLog, scene: &Scene, history: &[String]) {
    log.assert_unchanged("addons", &scene.addons());
    log.assert_unchanged("records", &scene.home.state().join("targets"));
    log.assert_eq("history", &scene.history_txs(), &history.to_vec());
}

#[test]
fn an_update_binds_to_its_preview() {
    let log = common::tlog::tlog!();
    let scene = foo_behind(&log);
    let update = argv(&["update", "Foo", "-t", "retail"]);

    log.step("preview");
    let dry = log.run(&scene, &with(&update, &["--dry-run"]));
    let previewed = token(dry.ok().envelope());
    log.json("token", &previewed);
    log.assert_eq(
        "the preview updates Foo to v2",
        &dry.data()["targets"][0]["items"][0]["to_version"],
        &json!("v2"),
    );

    log.step("preview again, from the memo");
    let again = log.run(&scene, &with(&update, &["--dry-run"]));
    log.assert_eq(
        "an unchanged home, the same token",
        &token(again.ok().envelope()),
        &previewed,
    );

    log.step("bound run");
    let ran = log.run(&scene, &with(&update, &["--if-match", &previewed]));
    ran.ok();
    log.assert_eq(
        "a real run's precondition",
        &ran.data()["precondition"],
        &Value::Null,
    );
    log.assert_eq(
        "the items the preview showed",
        &ran.data()["targets"][0]["items"],
        &dry.data()["targets"][0]["items"],
    );
    log.assert_eq(
        "Foo's record",
        // Records are keyed by the lowercase name.
        &scene.record("foo")["version"],
        &json!("v2"),
    );

    log.step("stale, on a fresh home in the same state");
    let scene = foo_behind(&log);
    let previewed = token(
        log.run(&scene, &with(&update, &["--dry-run"]))
            .ok()
            .envelope(),
    );
    log.json("token", &previewed);
    install_bar(&log, &scene);
    let history = record_before(&log, &scene);
    let stale = log.run(&scene, &with(&update, &["--if-match", &previewed]));
    assert_stale(
        stale.fails(1, "plan_stale").envelope(),
        &["targets"],
        &strs(&preview_of(&update)),
    );
    assert_left_alone(&log, &scene, &history);
}

/// M4-D20: inside the memo's window a bound `update` does what its preview showed, a
/// release published since notwithstanding, and asks no source.
#[test]
fn a_release_inside_the_memo_window_keeps_a_bound_update_to_its_preview() {
    let log = common::tlog::tlog!();
    let scene = foo_behind(&log);
    let update = argv(&["update", "Foo", "-t", "retail"]);
    log.step("preview: v2");
    let previewed = token(
        log.run(&scene, &with(&update, &["--dry-run"]))
            .ok()
            .envelope(),
    );
    log.json("token", &previewed);

    log.step("v3 is published");
    scene.release("foo", "v3", &[retail_folder("Foo")]);
    let latest = |s: &Scene| common::netlog::net_log(&s.home).count("github.com", "release-latest");
    let asked = latest(&scene);
    log.assert_eq(
        "the aged memo made the preview ask the source",
        &(asked > 0),
        &true,
    );

    log.step("the bound run installs v2, from the memo");
    let ran = log.run(&scene, &with(&update, &["--if-match", &previewed]));
    log.assert_eq(
        "(installed, releases/latest requests since the preview)",
        &(
            &ran.ok().data()["targets"][0]["items"][0]["to_version"],
            latest(&scene) - asked,
        ),
        &(&json!("v2"), 0),
    );
    log.assert_eq(
        "Foo's record: v2, not v3",
        &scene.record("foo")["version"],
        &json!("v2"),
    );
}

#[test]
fn a_bound_update_refused_under_the_lock_is_not_retryable() {
    let log = common::tlog::tlog!();
    let scene = foo_behind(&log);
    let update = argv(&["update", "Foo", "-t", "retail"]);
    log.step("preview");
    let previewed = token(
        log.run(&scene, &with(&update, &["--dry-run"]))
            .ok()
            .envelope(),
    );
    log.json("token", &previewed);

    log.step("the bound run stops before its lock");
    let pause = Pause::new(scene.home.env.root());
    let child = log
        .cmd(&scene, &with(&update, &["--if-match", &previewed]))
        .failpoints("engine.snapshot.prestaged=pause")
        .pause(&pause)
        .spawn();
    log.reached(&pause, "engine.snapshot.prestaged");

    log.step("another install commits meanwhile");
    install_bar(&log, &scene);
    let history = record_before(&log, &scene);

    log.step("the bound run goes on");
    log.resume(&pause, "engine.snapshot.prestaged");
    let ran = child.wait();
    assert_stale(
        ran.fails(1, "plan_stale").envelope(),
        &["targets"],
        &strs(&preview_of(&update)),
    );
    assert_left_alone(&log, &scene, &history);
}

#[test]
fn a_sync_binds_to_its_file() {
    let log = common::tlog::tlog!();
    let scene = Scene::new();
    log.env(&scene);
    scene.release("foo", "v1", &[retail_folder("Foo")]);
    scene.release("bar", "v1", &[retail_folder("Bar")]);
    let desired = |entries: &str, tail: &str| {
        let text = format!("[family.mainline]\naddons = [{entries}]\n{tail}");
        log.json("addons.toml", &text);
        scene.home.write_desired(&text);
    };
    let sync = argv(&["sync"]);

    log.step("a file naming Foo: preview, then the bound run");
    desired("\"gh:o/foo\"", "");
    let previewed = token(
        log.run(&scene, &with(&sync, &["--dry-run"]))
            .ok()
            .envelope(),
    );
    log.json("token", &previewed);
    let ran = log.run(&scene, &with(&sync, &["--if-match", &previewed]));
    log.assert_eq(
        "Foo installed",
        &(
            &ran.ok().data()["precondition"],
            scene.addons().join("Foo").is_dir(),
        ),
        &(&Value::Null, true),
    );

    log.step("Bar added: preview, then a comment added to the file");
    desired("\"gh:o/foo\", \"gh:o/bar\"", "");
    let previewed = token(
        log.run(&scene, &with(&sync, &["--dry-run"]))
            .ok()
            .envelope(),
    );
    log.json("token", &previewed);
    desired("\"gh:o/foo\", \"gh:o/bar\"", "# a note\n");
    let history = record_before(&log, &scene);

    log.step("the bound run");
    let stale = log.run(&scene, &with(&sync, &["--if-match", &previewed]));
    assert_stale(
        stale.fails(1, "plan_stale").envelope(),
        &["observed"],
        &strs(&preview_of(&sync)),
    );
    assert_left_alone(&log, &scene, &history);
    log.assert_eq(
        "Bar not installed",
        &scene.addons().join("Bar").exists(),
        &false,
    );

    log.step("preview again, then another install commits on the client");
    let previewed = token(
        log.run(&scene, &with(&sync, &["--dry-run"]))
            .ok()
            .envelope(),
    );
    log.json("token", &previewed);
    // Baz is not in the file: only the client's record moves.
    scene.release("baz", "v1", &[retail_folder("Baz")]);
    log.run(&scene, &["install", "gh:o/baz", "-t", "retail", "--json"])
        .ok();
    let history = record_before(&log, &scene);
    let stale = log.run(&scene, &with(&sync, &["--if-match", &previewed]));
    assert_stale(
        stale.fails(1, "plan_stale").envelope(),
        &["targets"],
        &strs(&preview_of(&sync)),
    );
    assert_left_alone(&log, &scene, &history);

    log.step("the fix's preview binds: Bar, exactly as previewed");
    let dry = log.run(&scene, &with(&sync, &["--dry-run"]));
    let previewed = token(dry.ok().envelope());
    log.json("token", &previewed);
    let ran = log.run(&scene, &with(&sync, &["--if-match", &previewed]));
    log.assert_eq(
        "the items the preview showed",
        &ran.ok().data()["targets"][0]["items"],
        &dry.data()["targets"][0]["items"],
    );
    log.assert_eq("Bar installed", &scene.addons().join("Bar").is_dir(), &true);
}

#[test]
fn a_bound_noop_update_is_stale_after_a_change() {
    let log = common::tlog::tlog!();
    let scene = Scene::new();
    log.env(&scene);
    scene.release("foo", "v1", &[retail_folder("Foo")]);
    log.run(&scene, &["install", "gh:o/foo", "-t", "retail", "--json"])
        .ok();
    let update = argv(&["update", "Foo", "-t", "retail"]);

    log.step("preview: nothing to do");
    let dry = log.run(&scene, &with(&update, &["--dry-run"]));
    let previewed = token(dry.ok().envelope());
    log.json("token", &previewed);
    log.assert_eq(
        "nothing changes",
        &dry.data()["summary"]["changed"],
        &json!(0),
    );

    log.step("another install commits");
    install_bar(&log, &scene);
    let history = record_before(&log, &scene);

    log.step("the bound run: stale, not an unchanged success");
    let stale = log.run(&scene, &with(&update, &["--if-match", &previewed]));
    assert_stale(
        stale.fails(1, "plan_stale").envelope(),
        &["targets"],
        &strs(&preview_of(&update)),
    );
    assert_left_alone(&log, &scene, &history);

    log.step("a matching bound run with nothing to do");
    let previewed = token(
        log.run(&scene, &with(&update, &["--dry-run"]))
            .ok()
            .envelope(),
    );
    log.json("token", &previewed);
    let ran = log.run(&scene, &with(&update, &["--if-match", &previewed]));
    log.assert_eq(
        "an unchanged success",
        &(
            &ran.ok().data()["summary"]["changed"],
            &ran.data()["precondition"],
        ),
        &(&json!(0), &Value::Null),
    );
    assert_left_alone(&log, &scene, &history);
}

// ---- `copy` (D2): its source client in the token ---------------------------------------

/// `copy`'s two clients: Classic Era (the source) and Anniversary.
const ERA: &str = "_classic_era_";
const ANNIVERSARY: &str = "_anniversary_";

/// Release `tag` of `gh:o/<repo>` as its latest, built for both clients.
fn publish_for_both(home: &common::home::Home, repo: &str, name: &str, tag: &str) {
    b::publish(home, repo, name, tag, b::ERA_AND_TBC);
}

/// Foo installed on Classic Era; Foo and Bar published for both clients.
fn era_with_foo(log: &common::tlog::TestLog) -> common::home::Home {
    let home = common::home::Home::new(&["wow_classic_era", "wow_anniversary"]);
    log.env(&home);
    publish_for_both(&home, "foo", "Foo", "v1");
    publish_for_both(&home, "bar", "Bar", "v1");
    log.run(&home, &["install", "gh:o/foo", "-t", "era", "--json"])
        .ok();
    home
}

/// Both clients' AddOns and the records, recorded; the transactions history lists,
/// returned.
fn record_both(log: &common::tlog::TestLog, home: &common::home::Home) -> Vec<String> {
    log.tree("era addons", &home.addons(ERA));
    log.tree("anniversary addons", &home.addons(ANNIVERSARY));
    log.tree("records", &home.state().join("targets"));
    let history = history_txs(home);
    log.json("history", &history);
    history
}

fn assert_both_alone(log: &common::tlog::TestLog, home: &common::home::Home, history: &[String]) {
    log.assert_unchanged("era addons", &home.addons(ERA));
    log.assert_unchanged("anniversary addons", &home.addons(ANNIVERSARY));
    log.assert_unchanged("records", &home.state().join("targets"));
    log.assert_eq("history", &history_txs(home), &history.to_vec());
}

/// The transaction ids `history.jsonl` lists, oldest first.
fn history_txs(home: &common::home::Home) -> Vec<String> {
    std::fs::read_to_string(home.state().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| v["tx"].as_str().unwrap_or_default().to_owned())
        .collect()
}

const COPY_FOO: &[&str] = &["copy", "--from", "era", "--to", "anniversary", "Foo"];

#[test]
fn a_copy_binds_to_its_preview() {
    let log = common::tlog::tlog!();
    let home = era_with_foo(&log);
    let copy = argv(COPY_FOO);
    let fix = preview_of(&copy);
    // Registered before the preview: registering `--to` in between would rename it in
    // every part of the token, not only `targets`.
    log.step("anniversary has Bar");
    log.run(
        &home,
        &["install", "gh:o/bar", "-t", "anniversary", "--json"],
    )
    .ok();

    log.step("preview");
    let dry = log.run(&home, &with(&copy, &["--dry-run"]));
    let previewed = token(dry.ok().envelope());
    log.json("token", &previewed);
    log.step("preview again");
    let again = log.run(&home, &with(&copy, &["--dry-run"]));
    log.assert_eq(
        "an unchanged home, the same token",
        &token(again.ok().envelope()),
        &previewed,
    );

    log.step("another change commits on --to");
    log.run(&home, &["remove", "Bar", "-t", "anniversary", "--json"])
        .ok();
    let history = record_both(&log, &home);
    let stale = log.run(&home, &with(&copy, &["--if-match", &previewed]));
    assert_stale(
        stale.fails(1, "plan_stale").envelope(),
        &["targets"],
        &strs(&fix),
    );
    assert_both_alone(&log, &home, &history);

    log.step("the fix's preview binds");
    let dry = log.run(&home, &with(&copy, &["--dry-run"]));
    let fresh = token(dry.ok().envelope());
    log.json("token", &fresh);
    let ran = log.run(&home, &with(&copy, &["--if-match", &fresh]));
    log.assert_eq(
        "the items the preview showed, and a real run's precondition",
        &(
            &ran.ok().data()["targets"][0]["items"],
            &ran.data()["precondition"],
        ),
        &(&dry.data()["targets"][0]["items"], &Value::Null),
    );
    log.assert_eq(
        "Foo on anniversary",
        &home.addons(ANNIVERSARY).join("Foo/Foo.toc").is_file(),
        &true,
    );

    log.step("nothing left to copy, then another commit on --to");
    let dry = log.run(&home, &with(&copy, &["--dry-run"]));
    let idle = token(dry.ok().envelope());
    log.json("token", &idle);
    log.assert_eq(
        "Foo is already there",
        &dry.data()["summary"]["changed"],
        &json!(0),
    );
    log.run(
        &home,
        &["install", "gh:o/bar", "-t", "anniversary", "--json"],
    )
    .ok();
    let history = record_both(&log, &home);
    let stale = log.run(&home, &with(&copy, &["--if-match", &idle]));
    assert_stale(
        stale.fails(1, "plan_stale").envelope(),
        &["targets"],
        &strs(&fix),
    );
    assert_both_alone(&log, &home, &history);
}

/// `copy` asks its sources again (M4-D20), as `install` does: a release published after
/// the preview makes the bound run stale.
#[test]
fn a_release_since_the_preview_stales_a_bound_copy() {
    let log = common::tlog::tlog!();
    let home = era_with_foo(&log);
    let copy = argv(COPY_FOO);
    log.step("preview: v1");
    let previewed = token(log.run(&home, &with(&copy, &["--dry-run"])).ok().envelope());
    log.json("token", &previewed);

    log.step("v2 is published");
    publish_for_both(&home, "foo", "Foo", "v2");
    let history = record_both(&log, &home);

    log.step("the bound run");
    let stale = log.run(&home, &with(&copy, &["--if-match", &previewed]));
    let changed = &stale.fails(1, "plan_stale").envelope()["error"]["details"]["changed"];
    log.assert_eq(
        "the release changed the selection",
        &changed
            .as_array()
            .is_some_and(|c| c.contains(&json!("selection"))),
        &true,
    );
    assert_both_alone(&log, &home, &history);
}

#[test]
fn a_copy_whose_source_folder_changed_is_stale() {
    let log = common::tlog::tlog!();
    let home = era_with_foo(&log);
    let copy = argv(COPY_FOO);
    log.step("preview");
    let previewed = token(log.run(&home, &with(&copy, &["--dry-run"])).ok().envelope());
    log.json("token", &previewed);

    log.step("Foo's folder on era is edited by hand; its record is not");
    let toc = home.addons(ERA).join("Foo/Foo.toc");
    let mut text = std::fs::read_to_string(&toc).unwrap();
    text.push_str("## X-Edited: by hand\n");
    std::fs::write(&toc, text).unwrap();
    let history = record_both(&log, &home);

    log.step("the bound run");
    let stale = log.run(&home, &with(&copy, &["--if-match", &previewed]));
    assert_stale(
        stale.fails(1, "plan_stale").envelope(),
        &["observed"],
        &strs(&preview_of(&copy)),
    );
    assert_both_alone(&log, &home, &history);
}

#[test]
fn a_copy_whose_source_client_committed_is_stale() {
    let log = common::tlog::tlog!();
    let home = era_with_foo(&log);
    let copy = argv(COPY_FOO);
    log.step("preview a copy of Foo alone");
    let previewed = token(log.run(&home, &with(&copy, &["--dry-run"])).ok().envelope());
    log.json("token", &previewed);

    log.step("Bar, which the copy does not name, installs on era");
    log.run(&home, &["install", "gh:o/bar", "-t", "era", "--json"])
        .ok();
    let history = record_both(&log, &home);

    log.step("the bound run: the source's generation moved");
    let stale = log.run(&home, &with(&copy, &["--if-match", &previewed]));
    assert_stale(
        stale.fails(1, "plan_stale").envelope(),
        &["observed"],
        &strs(&preview_of(&copy)),
    );
    assert_both_alone(&log, &home, &history);
}

// ---- D2's acceptance rows, end to end (checkpoint criterion 6) --------------------------
//
// The smoke scenarios above prove each command binds; these prove every row of D2 for
// each command, each on fresh homes: what moves which part of the token, and that a
// stale run changes nothing. Releases are replayed `gh:` fixtures (`common::binding`).

/// The engine commands D2 binds.
const ENGINE_COMMANDS: [&str; 4] = ["install", "update", "sync", "copy"];

/// One command on a fresh home where it has work on `client` (registered, so a commit
/// there moves only its generation), with Bar published as a current release nothing
/// installs yet.
struct Case {
    home: common::home::Home,
    args: Vec<String>,
    client: &'static str,
}

impl Case {
    fn new(log: &common::tlog::TestLog, command: &str) -> Self {
        use common::home::Home;
        let mm = b::MAINLINE_AND_MISTS;
        let install = |home: &Home, r: &str, client: &str| {
            log.run(home, &["install", r, "-t", client, "--json"]).ok();
        };
        let (home, args, client) = match command {
            "install" => {
                let home = Home::new(&["wow"]);
                log.env(&home);
                b::publish(&home, "foo", "Foo", "v1", mm);
                install(&home, "gh:o/foo", "retail");
                b::publish(&home, "foo", "Foo", "v2", mm);
                (
                    home,
                    b::argv(&["install", "gh:o/foo", "-t", "retail"]),
                    "retail",
                )
            }
            "update" => {
                let home = Home::new(&["wow"]);
                log.env(&home);
                b::foo_behind(log, &home, "retail", ("v1", "v2"), mm);
                (home, b::argv(&["update", "Foo", "-t", "retail"]), "retail")
            }
            "sync" => {
                let home = Home::new(&["wow"]);
                log.env(&home);
                b::publish(&home, "foo", "Foo", "v1", mm);
                b::publish(&home, "baz", "Baz", "v1", mm);
                install(&home, "gh:o/baz", "retail");
                home.write_desired("[family.mainline]\naddons = [\"gh:o/baz\", \"gh:o/foo\"]\n");
                (home, b::argv(&["sync"]), "retail")
            }
            "copy" => {
                let home = Home::new(&["wow_classic_era", "wow_anniversary"]);
                log.env(&home);
                b::publish(&home, "foo", "Foo", "v1", b::ERA_AND_TBC);
                b::publish(&home, "baz", "Baz", "v1", b::ERA_AND_TBC);
                install(&home, "gh:o/foo", "era");
                install(&home, "gh:o/baz", "anniversary");
                (home, b::argv(COPY_FOO), "anniversary")
            }
            other => panic!("no case for {other}"), // ubs:ignore — the test's own failure
        };
        let flavors = if client == "anniversary" {
            b::ERA_AND_TBC
        } else {
            mm
        };
        b::publish(&home, "bar", "Bar", "v1", flavors);
        Self { home, args, client }
    }

    fn args(&self) -> Vec<&str> {
        self.args.iter().map(String::as_str).collect()
    }

    /// The flavor folder of the client the run changes.
    fn folder(&self) -> &'static str {
        if self.client == "anniversary" {
            ANNIVERSARY
        } else {
            "_retail_"
        }
    }

    /// Another install commits on the client the run changes, outside the run's scope,
    /// so only that client's generation moves (`targets`).
    fn commit_between(&self, log: &common::tlog::TestLog) {
        log.note(format!(
            "another install commits on {}: only its generation moves (targets)",
            self.client
        ));
        log.run(
            &self.home,
            &["install", "gh:o/bar", "-t", self.client, "--json"],
        )
        .ok();
    }

    /// A refusal's fix: the run's preview.
    fn fix(&self) -> Vec<String> {
        b::preview_argv(&self.args())
    }
}

/// `path` with its first `v1` replaced by `x1`: the same size, another byte.
#[cfg(test)]
fn same_size_edit(path: &std::path::Path) {
    let text = std::fs::read_to_string(path).unwrap();
    let edited = text.replacen("v1", "x1", 1);
    assert!(edited.len() == text.len() && edited != text, "{text}");
    std::fs::write(path, edited).unwrap();
}

#[test]
fn a_commit_on_an_affected_client_stales_each_command() {
    let log = common::tlog::tlog!();
    for command in ENGINE_COMMANDS {
        log.step(&format!("{command}: setup"));
        let case = Case::new(&log, command);
        log.step(&format!("{command}: preview"));
        let (_, token) = b::preview(&log, &case.home, command, &case.args());
        log.step(&format!("{command}: a commit on {}", case.client));
        case.commit_between(&log);
        let guard = b::guard(&log, &case.home, command);
        log.step(&format!("{command}: the bound run"));
        let ran = log.run(&case.home, &b::bound(&case.args(), &token));
        b::assert_stale(&log, &ran, &["targets"], &case.fix());
        guard.unchanged(&log);
    }
}

/// M4-D20: `sync` answers from the memo for 15 minutes, like `update`.
#[test]
fn a_release_inside_the_memo_window_keeps_a_bound_sync_to_its_preview() {
    let log = common::tlog::tlog!();
    let home = common::home::Home::new(&["wow"]);
    log.env(&home);
    log.step("setup: Foo v1 published, named in addons.toml, not installed");
    b::publish(&home, "foo", "Foo", "v1", b::MAINLINE_AND_MISTS);
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    let latest =
        |h: &common::home::Home| common::netlog::net_log(h).count("github.com", "release-latest");

    log.step("preview: v1");
    let (dry, token) = b::preview(&log, &home, "sync", &["sync"]);
    log.assert_eq(
        "(the preview installs, it asked the source)",
        &(&b::items(dry.data())[0]["to_version"], latest(&home) > 0),
        &(&json!("v1"), true),
    );

    log.step("v2 is published");
    b::publish(&home, "foo", "Foo", "v2", b::MAINLINE_AND_MISTS);
    let asked = latest(&home);

    log.step("the bound run installs v1, from the memo");
    let ran = log.run(&home, &b::bound(&["sync"], &token));
    log.assert_eq(
        "(installed, releases/latest requests since the preview)",
        &(
            &b::items(ran.ok().data())[0]["to_version"],
            latest(&home) - asked,
        ),
        &(&json!("v1"), 0),
    );
    log.assert_eq(
        "Foo's record",
        &common::state::record(&home, "retail").addons["foo"].version,
        &"v1".to_owned(),
    );
}

/// `--refresh` asks the source again, so a newer release stales the bound run.
#[test]
fn a_bound_update_with_refresh_revalidates() {
    let log = common::tlog::tlog!();
    log.step("setup");
    let case = Case::new(&log, "update");
    let mut args = case.args();
    args.push("--refresh");
    log.step("preview with --refresh: v2");
    let (_, token) = b::preview(&log, &case.home, "update", &args);
    log.step("v3 is published");
    b::publish(&case.home, "foo", "Foo", "v3", b::MAINLINE_AND_MISTS);
    let guard = b::guard(&log, &case.home, "update");
    log.step("the bound run asks again");
    let ran = log.run(&case.home, &b::bound(&args, &token));
    b::assert_stale(&log, &ran, &["selection"], &b::preview_argv(&args));
    guard.unchanged(&log);
}

#[test]
fn a_hand_edit_in_a_replaced_folder_stales_the_bound_run() {
    let log = common::tlog::tlog!();
    for command in ["install", "update"] {
        log.step(&format!("{command}: setup"));
        let case = Case::new(&log, command);
        log.step(&format!("{command}: preview"));
        let (_, token) = b::preview(&log, &case.home, command, &case.args());
        log.step(&format!("{command}: Foo.toc edited by hand, the same size"));
        log.note("clean at the preview, in a folder the run replaces: observed");
        same_size_edit(&case.home.addons(case.folder()).join("Foo/Foo.toc"));
        let guard = b::guard(&log, &case.home, command);
        log.step(&format!("{command}: the bound run"));
        let ran = log.run(&case.home, &b::bound(&case.args(), &token));
        b::assert_stale(&log, &ran, &["observed"], &case.fix());
        guard.unchanged(&log);
    }

    log.step("sync --prune: setup, Bar installed and not in the file");
    let home = common::home::Home::new(&["wow"]);
    log.env(&home);
    for (repo, name) in [("foo", "Foo"), ("bar", "Bar")] {
        b::publish(&home, repo, name, "v1", b::MAINLINE_AND_MISTS);
        log.run(
            &home,
            &["install", &format!("gh:o/{repo}"), "-t", "retail", "--json"],
        )
        .ok();
    }
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    let prune = ["sync", "--prune"];
    log.step("sync --prune: preview, which removes Bar");
    let (dry, token) = b::preview(&log, &home, "sync --prune", &prune);
    log.assert_eq(
        "the preview removes Bar",
        &dry.data()["summary"]["changed"],
        &json!(1),
    );
    log.step("sync --prune: Bar.toc edited by hand, the same size");
    log.note("in the folder the run removes: observed");
    same_size_edit(&home.addons("_retail_").join("Bar/Bar.toc"));
    let guard = b::guard(&log, &home, "sync --prune");
    log.step("sync --prune: the bound run");
    let ran = log.run(&home, &b::bound(&prune, &token));
    b::assert_stale(&log, &ran, &["observed"], &b::preview_argv(&prune));
    guard.unchanged(&log);
}

#[test]
fn a_name_the_install_creates_appearing_stales_it() {
    let log = common::tlog::tlog!();
    let home = common::home::Home::new(&["wow"]);
    log.env(&home);
    log.step("setup: Foo installed, Bar published");
    for (repo, name) in [("foo", "Foo"), ("bar", "Bar")] {
        b::publish(&home, repo, name, "v1", b::MAINLINE_AND_MISTS);
    }
    log.run(&home, &["install", "gh:o/foo", "-t", "retail", "--json"])
        .ok();
    let install = ["install", "gh:o/bar", "-t", "retail"];
    log.step("preview: Bar's folder would be added");
    let (_, token) = b::preview(&log, &home, "install", &install);
    log.step("a folder named Bar is made by hand");
    let mine = home.addons("_retail_").join("Bar");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(
        mine.join("Bar.toc"),
        "## Interface: 120105\n## Title: mine\n",
    )
    .unwrap();
    let guard = b::guard(&log, &home, "install");
    log.step("the bound run plans around it, so it is not the preview");
    let ran = log.run(&home, &b::bound(&install, &token));
    b::assert_stale_with(&log, &ran, "selection", &b::preview_argv(&install));
    guard.unchanged(&log);
}

#[test]
fn an_edited_sync_file_stales_a_bound_sync() {
    let log = common::tlog::tlog!();
    let home = common::home::Home::new(&["wow"]);
    log.env(&home);
    log.step("setup: Baz installed, addons.toml naming Baz and Foo");
    for (repo, name) in [("foo", "Foo"), ("bar", "Bar"), ("baz", "Baz")] {
        b::publish(&home, repo, name, "v1", b::MAINLINE_AND_MISTS);
    }
    log.run(&home, &["install", "gh:o/baz", "-t", "retail", "--json"])
        .ok();
    home.write_desired("[family.mainline]\naddons = [\"gh:o/baz\", \"gh:o/foo\"]\n");
    log.step("preview");
    let (_, token) = b::preview(&log, &home, "sync", &["sync"]);
    log.step("an entry is added to the file");
    log.note("a new install (selection) and other bytes (observed)");
    home.write_desired("[family.mainline]\naddons = [\"gh:o/baz\", \"gh:o/foo\", \"gh:o/bar\"]\n");
    let guard = b::guard(&log, &home, "sync");
    log.step("the bound run");
    let ran = log.run(&home, &b::bound(&["sync"], &token));
    b::assert_stale(
        &log,
        &ran,
        &["selection", "observed"],
        &b::preview_argv(&["sync"]),
    );
    guard.unchanged(&log);
}

/// A home where `command` has nothing to do, with Bar installed on the client it would
/// change: (the home, the run's argv, that client).
fn idle_case(
    log: &common::tlog::TestLog,
    command: &str,
) -> (common::home::Home, Vec<String>, &'static str) {
    use common::home::Home;
    let install = |home: &Home, r: &str, client: &str| {
        log.run(home, &["install", r, "-t", client, "--json"]).ok();
    };
    if command == "copy" {
        let home = Home::new(&["wow_classic_era", "wow_anniversary"]);
        log.env(&home);
        for (repo, name) in [("foo", "Foo"), ("bar", "Bar")] {
            b::publish(&home, repo, name, "v1", b::ERA_AND_TBC);
        }
        install(&home, "gh:o/foo", "era");
        install(&home, "gh:o/foo", "anniversary");
        install(&home, "gh:o/bar", "anniversary");
        return (home, b::argv(COPY_FOO), "anniversary");
    }
    let home = Home::new(&["wow"]);
    log.env(&home);
    for (repo, name) in [("foo", "Foo"), ("bar", "Bar")] {
        b::publish(&home, repo, name, "v1", b::MAINLINE_AND_MISTS);
        install(&home, &format!("gh:o/{repo}"), "retail");
    }
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\", \"gh:o/bar\"]\n");
    let args = match command {
        // Already at the newest release: the engine plans it unchanged.
        "install" => b::argv(&["install", "gh:o/foo", "-t", "retail"]),
        "update" => b::argv(&["update", "Foo", "-t", "retail"]),
        _ => b::argv(&["sync"]),
    };
    (home, args, "retail")
}

#[test]
fn a_bound_noop_run_with_a_stale_token_is_stale() {
    let log = common::tlog::tlog!();
    for command in ENGINE_COMMANDS {
        log.step(&format!("{command}: setup, nothing to do"));
        let (home, args, client) = idle_case(&log, command);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        log.step(&format!("{command}: preview"));
        let (dry, token) = b::preview(&log, &home, command, &args);
        // ubs:ignore — comparing a command name and an item status, not secrets
        let idle = if command == "install" {
            // ubs:ignore — comparing an item status, not secrets
            dry.data()["items"][0]["status"] == "unchanged"
        } else {
            dry.data()["summary"]["changed"] == 0
        };
        log.assert_eq("the preview changes nothing", &idle, &true);
        log.step(&format!("{command}: Bar is pinned on {client}"));
        log.note("a records-only commit: the client's generation moves");
        log.run(&home, &["pin", "Bar", "-t", client, "--json"]).ok();
        let guard = b::guard(&log, &home, command);
        log.step(&format!(
            "{command}: the bound run, stale, never `nothing to do`"
        ));
        let ran = log.run(&home, &b::bound(&args, &token));
        b::assert_stale_with(&log, &ran, "targets", &b::preview_argv(&args));
        guard.unchanged(&log);
    }
}

#[test]
fn the_second_of_two_runs_holding_one_token_is_stale() {
    let log = common::tlog::tlog!();
    for command in ["update", "copy"] {
        log.step(&format!("{command}: setup"));
        let case = Case::new(&log, command);
        log.step(&format!("{command}: preview, then the bound run"));
        let (_, token) = b::preview(&log, &case.home, command, &case.args());
        log.run(&case.home, &b::bound(&case.args(), &token)).ok();
        let guard = b::guard(&log, &case.home, command);
        log.step(&format!("{command}: the same token again"));
        log.note("its own commit moved the client, and it now has nothing to do");
        let ran = log.run(&case.home, &b::bound(&case.args(), &token));
        b::assert_stale_with(&log, &ran, "targets", &case.fix());
        b::assert_stale_with(&log, &ran, "selection", &case.fix());
        guard.unchanged(&log);
    }
}

#[test]
fn a_bound_dry_run_compares() {
    let log = common::tlog::tlog!();
    for command in ENGINE_COMMANDS {
        log.step(&format!("{command}: setup"));
        let case = Case::new(&log, command);
        let home = &case.home;
        log.step(&format!("{command}: preview"));
        let (_, token) = b::preview(&log, home, command, &case.args());
        let mut dry = case.args();
        dry.push("--dry-run");
        // A dry run writes nothing under `<state>` and takes no global lock.
        let untouched = |label: &str| {
            (
                b::state_without_net_log(&log, home, label),
                b::lock_inode(home),
            )
        };

        log.step(&format!("{command}: a matching bound dry run"));
        let before = untouched("before the matching dry run");
        let ran = log.run(home, &b::bound(&dry, &token));
        log.assert_eq(
            "the same token",
            &ran.ok().data()["precondition"],
            &json!(token),
        );
        log.assert_eq(
            "<state> and the lock file",
            &untouched("after the matching dry run"),
            &before,
        );

        log.step(&format!("{command}: a stale bound dry run"));
        case.commit_between(&log);
        let before = untouched("before the stale dry run");
        let ran = log.run(home, &b::bound(&dry, &token));
        b::assert_stale(&log, &ran, &["targets"], &case.fix());
        log.assert_eq(
            "<state> and the lock file",
            &untouched("after the stale dry run"),
            &before,
        );
    }
}

/// M4-D61: the guarantee is per client. A two-client bound `update` whose second client
/// receives a commit while the run waits before that client's lock ends partial.
#[test]
fn a_two_client_bound_update_commits_per_client() {
    let log = common::tlog::tlog!();
    let home = b::two_clients_with_foo("v1");
    log.env(&home);
    log.step("setup: Foo v1 on retail and classic, v2 published");
    for client in ["retail", "classic"] {
        log.run(&home, &["install", "gh:o/foo", "-t", client, "--json"])
            .ok();
    }
    b::publish(&home, "foo", "Foo", "v2", b::MAINLINE_AND_MISTS);
    b::age_release_memo(&home, jiff::SignedDuration::from_hours(1));
    let update = ["update", "Foo"];

    log.step("preview: both clients");
    let (dry, token) = b::preview(&log, &home, "update", &update);
    log.assert_eq(
        "both clients update",
        &dry.data()["summary"]["changed"],
        &json!(2),
    );
    // The engine commits registered clients in key order: the second one is raced.
    let mut keyed = [
        ("retail", "_retail_", common::state::key_of(&home, "retail")),
        (
            "classic",
            "_classic_",
            common::state::key_of(&home, "classic"),
        ),
    ];
    keyed.sort_by_key(|(_, _, key)| *key);
    let [(first, _, _), (second, second_folder, _)] = keyed;
    log.json("commit order", &[first, second]);
    let kinds = |h: &common::home::Home| -> Vec<String> {
        common::state::history(h)
            .iter()
            .map(|e| {
                serde_json::to_value(e.kind)
                    .unwrap()
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    };
    let before = kinds(&home);

    log.step("the bound run stops before the second client's lock");
    let pause = Pause::new(home.env.root());
    let child = log
        .cmd(&home, &b::bound(&update, &token))
        .failpoints("engine.snapshot.prestaged=pause@2")
        .pause(&pause)
        .spawn();
    log.reached(&pause, "engine.snapshot.prestaged");
    log.step(&format!("Bar installs on {second} meanwhile"));
    log.run(&home, &["install", "gh:o/bar", "-t", second, "--json"])
        .ok();
    log.tree("second addons", &home.addons(second_folder));

    log.step("the bound run goes on");
    log.resume(&pause, "engine.snapshot.prestaged");
    let ran = child.wait();
    let data = ran.ok().data();
    let target = |id: &str| {
        data["targets"]
            .as_array()
            .unwrap()
            .iter()
            // ubs:ignore — comparing target ids, not secrets
            .find(|t| t["target"]["id"] == id)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let (one, two) = (target(first), target(second));
    log.assert_eq(
        "(partial, the first committed with no error)",
        &(
            &ran.envelope()["partial"],
            one["tx"].is_string(),
            &one["error"],
        ),
        &(&json!(true), true, &Value::Null),
    );
    log.assert_eq(
        "the second refused: (code, retryable, changed, tx)",
        &(
            &two["error"]["code"],
            &two["error"]["retryable"],
            &two["error"]["details"]["changed"],
            &two["tx"],
        ),
        &(
            &json!("plan_stale"),
            &json!(false),
            &json!(["targets"]),
            &Value::Null,
        ),
    );
    let version = |id: &str| {
        common::state::record(&home, id).addons["foo"]
            .version
            .clone()
    };
    log.assert_eq(
        "Foo: (first, second)",
        &(version(first), version(second)),
        &("v2".to_owned(), "v1".to_owned()),
    );
    log.assert_unchanged("second addons", &home.addons(second_folder));
    let after = kinds(&home);
    log.assert_eq(
        "history gains the first client's update and Bar's install",
        &after[before.len()..].to_vec(),
        &["update".to_owned(), "install".to_owned()].to_vec(),
    );
}

#[test]
fn a_bound_run_refused_under_the_lock_is_not_retryable() {
    let log = common::tlog::tlog!();
    for command in ["install", "sync", "copy"] {
        log.step(&format!("{command}: setup"));
        let case = Case::new(&log, command);
        log.step(&format!("{command}: preview"));
        let (_, token) = b::preview(&log, &case.home, command, &case.args());

        log.step(&format!("{command}: the bound run stops before its lock"));
        let pause = Pause::new(case.home.env.root());
        let child = log
            .cmd(&case.home, &b::bound(&case.args(), &token))
            .failpoints("engine.snapshot.prestaged=pause")
            .pause(&pause)
            .spawn();
        log.reached(&pause, "engine.snapshot.prestaged");
        case.commit_between(&log);
        let guard = b::guard(&log, &case.home, command);

        log.step(&format!("{command}: the bound run goes on"));
        log.resume(&pause, "engine.snapshot.prestaged");
        let ran = child.wait();
        b::assert_stale(&log, &ran, &["targets"], &case.fix());
        guard.unchanged(&log);
    }
}

#[test]
fn two_previews_of_an_unchanged_home_agree() {
    let log = common::tlog::tlog!();
    for command in ENGINE_COMMANDS {
        log.step(&format!("{command}: setup"));
        let case = Case::new(&log, command);
        let net = || common::netlog::net_log(&case.home);
        log.step(&format!("{command}: two previews"));
        let (_, first) = b::preview(&log, &case.home, command, &case.args());
        let before = net();
        let (_, second) = b::preview(&log, &case.home, command, &case.args());
        let after = net();
        log.assert_eq("the same token", &second, &first);
        match command {
            // The first preview asked the source (the memo was aged); the second
            // answers from the memo.
            "update" => log.assert_eq(
                "(the first preview asked, releases/latest requests by the second)",
                &(
                    before.count("github.com", "release-latest") > 0,
                    after.count("github.com", "release-latest")
                        - before.count("github.com", "release-latest"),
                ),
                &(true, 0),
            ),
            // `install` asks again, but the archive comes from the blob cache.
            "install" => log.assert_eq(
                "downloads by the second preview",
                &(after.count_purpose("asset") - before.count_purpose("asset")),
                &0,
            ),
            _ => {}
        }
    }
}

#[test]
fn a_token_from_another_command_is_stale_on_each() {
    let log = common::tlog::tlog!();
    let home = b::two_clients_with_foo("v1");
    log.env(&home);
    log.step("setup: Foo on retail, named in addons.toml");
    log.run(&home, &["install", "gh:o/foo", "-t", "retail", "--json"])
        .ok();
    home.write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    log.step("remove's preview");
    let (_, token) = b::preview(&log, &home, "remove", &["remove", "Foo", "-t", "retail"]);
    let guard = b::guard(&log, &home, "every command");
    for args in [
        &["install", "gh:o/foo", "-t", "classic"][..],
        &["update", "Foo", "-t", "retail"],
        &["sync"],
        &["copy", "--from", "retail", "--to", "classic", "Foo"],
    ] {
        log.step(&format!("{}: remove's token", args[0]));
        let ran = log.run(&home, &b::bound(args, &token));
        b::assert_stale_with(&log, &ran, "command", &b::preview_argv(args));
    }
    guard.unchanged(&log);
}

const PRUNE: &[&str] = &["backup", "prune", "-t", "retail", "--keep", "2"];
const PRUNE_PREVIEW: &[&str] = &[
    "omaddons",
    "backup",
    "prune",
    "-t",
    "retail",
    "--keep",
    "2",
    "--dry-run",
];

#[cfg(test)]
mod prune_support {
    use super::common::engine::Scene;
    use super::support::{argv, token};

    /// `omaddons <argv>` with the clock frozen, so a backup's age is the same in the
    /// preview and the run.
    pub fn at_noon(scene: &Scene, argv: &[String]) -> super::common::home::Run {
        scene
            .home
            .exec_with(argv, &[("OMADDONS_NOW", "2026-10-10T12:00:00Z")])
    }

    /// `backup prune`'s `--dry-run` token, and the ids it would delete.
    pub fn prune_preview(scene: &Scene) -> (String, Vec<String>) {
        let mut run = argv(super::PRUNE);
        run.extend(["--dry-run", "--json"].map(str::to_owned));
        let envelope = at_noon(scene, &run).expect(0);
        let ids = envelope["data"]["targets"][0]["pruned"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap().to_owned())
            .collect();
        (token(&envelope), ids)
    }

    /// Three automatic backups on three old days, and the newest checkpoint: `--keep 2`
    /// prunes the oldest automatic one.
    pub fn three_days(scene: &Scene) -> std::path::PathBuf {
        use omaddons::backup::BackupKind;
        let client = scene.home.client("_retail_");
        for day in ["01", "02", "03"] {
            let at = format!("2026-10-{day}T08:00:00Z").parse().unwrap();
            super::common::backups::seed_backup(&client, BackupKind::Auto, at, 1);
        }
        let at = "2026-10-05T08:00:00Z".parse().unwrap();
        super::common::backups::seed_backup(&client, BackupKind::Manual, at, 1);
        client
    }

    /// The backup ids under `client`'s `<meta>/backups/`, sorted.
    pub fn backups(client: &std::path::Path) -> Vec<String> {
        let mut ids: Vec<String> = std::fs::read_dir(client.join("Interface/.omaddons/backups"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        ids.sort();
        ids
    }
}

#[test]
fn a_backup_prune_deletes_exactly_what_it_previewed() {
    let scene = Scene::new();
    let client = three_days(&scene);
    let (token, ids) = prune_preview(&scene);
    assert_eq!(ids.len(), 1);
    assert_eq!(
        prune_preview(&scene).0,
        token,
        "an unchanged home, the same token"
    );
    let envelope = at_noon(&scene, &bound(PRUNE, &token)).expect(0);
    let pruned: Vec<&str> = envelope["data"]["targets"][0]["pruned"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(pruned, ids);
    assert_eq!(envelope["data"]["precondition"], Value::Null);
    assert!(!backups(&client).contains(&ids[0]));
}

#[test]
fn a_backup_prune_after_a_new_backup_deletes_nothing() {
    use omaddons::backup::BackupKind;
    let scene = Scene::new();
    let client = three_days(&scene);
    let (stale, _) = prune_preview(&scene);
    // A new automatic backup: --keep 2 now deletes a different set.
    let at = "2026-10-04T08:00:00Z".parse().unwrap();
    common::backups::seed_backup(&client, BackupKind::Auto, at, 1);
    let before = backups(&client);
    let envelope = at_noon(&scene, &bound(PRUNE, &stale)).expect(1);
    assert_stale(&envelope, &["selection", "observed"], PRUNE_PREVIEW);
    assert_eq!(backups(&client), before, "nothing was deleted");

    // A preview given the stale token previews afresh; the fix's token runs.
    let mut run = bound(PRUNE, &stale);
    run.push("--dry-run".to_owned());
    let fresh = token(&at_noon(&scene, &run).expect(0));
    let (token, ids) = prune_preview(&scene);
    assert_eq!(fresh, token);
    assert_eq!(ids.len(), 2);
    at_noon(&scene, &bound(PRUNE, &token)).expect(0);
    assert_eq!(backups(&client).len(), before.len() - 2);
}

#[test]
fn a_backup_prune_whose_victim_was_rewritten_deletes_nothing() {
    let scene = Scene::new();
    let client = three_days(&scene);
    let (token, ids) = prune_preview(&scene);
    // Its metadata rewritten since the preview (a verify records what it hashed).
    let meta = client
        .join("Interface/.omaddons/backups")
        .join(&ids[0])
        .join("backup.json");
    let text = std::fs::read_to_string(&meta).unwrap();
    std::fs::write(&meta, text.replace("metadata_only", "verified")).unwrap();
    let envelope = at_noon(&scene, &bound(PRUNE, &token)).expect(1);
    assert_stale(&envelope, &["observed"], PRUNE_PREVIEW);
    assert!(backups(&client).contains(&ids[0]));
}

#[cfg(test)]
mod restore_support {
    use super::common::engine::Scene;
    use super::support::argv;

    /// A live retail `WTF/` and a manual backup to restore; the backup's id and the live
    /// tree.
    pub fn restorable(scene: &Scene) -> (String, std::path::PathBuf) {
        use omaddons::backup::BackupKind;
        let client = scene.home.client("_retail_");
        let wtf = super::common::wtf::Wtf::at(client.clone())
            .saved_variables("Foo", 64)
            .build();
        let at = "2026-10-05T08:00:00Z".parse().unwrap();
        let id = super::common::backups::seed_backup(&client, BackupKind::Manual, at, 2);
        (id.to_string(), wtf)
    }

    /// `backup restore <id> -t retail`, and its preview's argv.
    pub fn restore_args(id: &str) -> ([&str; 5], Vec<String>) {
        let args = ["backup", "restore", id, "-t", "retail"];
        let mut preview = argv(&["omaddons"]);
        preview.extend(argv(&args));
        preview.push("--dry-run".to_owned());
        (args, preview)
    }
}

#[test]
fn a_backup_restore_restores_exactly_what_it_previewed() {
    let scene = Scene::new();
    let (id, _) = restorable(&scene);
    let (args, _) = restore_args(&id);
    let token = preview(&scene, &args);
    assert_eq!(
        preview(&scene, &args),
        token,
        "an unchanged home, the same token"
    );
    let envelope = scene.run(&bound(&args, &token), "").expect(0);
    assert_eq!(envelope["data"]["kind"], "restore");
    assert_eq!(envelope["data"]["precondition"], Value::Null);
}

#[test]
fn a_backup_restore_after_a_play_session_changes_nothing() {
    let scene = Scene::new();
    let (id, wtf) = restorable(&scene);
    let (args, preview_argv) = restore_args(&id);
    let token = preview(&scene, &args);
    // The game wrote settings since the preview, touching no generation.
    let sv = wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua");
    std::fs::write(&sv, "Foo = { played = true }").unwrap();
    let before = common::trees::tree_hash(&wtf);
    let envelope = scene.run(&bound(&args, &token), "").expect(1);
    let preview_argv: Vec<&str> = preview_argv.iter().map(String::as_str).collect();
    assert_stale(&envelope, &["observed"], &preview_argv);
    assert_eq!(common::trees::tree_hash(&wtf), before);
}

#[test]
fn a_backup_restore_whose_backup_was_rewritten_changes_nothing() {
    let scene = Scene::new();
    let (id, wtf) = restorable(&scene);
    let (args, preview_argv) = restore_args(&id);
    let token = preview(&scene, &args);
    // Re-verified since the preview: its backup.json says so.
    let meta = scene
        .home
        .client("_retail_")
        .join("Interface/.omaddons/backups")
        .join(&id)
        .join("backup.json");
    let text = std::fs::read_to_string(&meta).unwrap();
    std::fs::write(
        &meta,
        text.replace("\"actor\"", "\"reason\": \"checked\", \"actor\""),
    )
    .unwrap();
    let before = common::trees::tree_hash(&wtf);
    let envelope = scene.run(&bound(&args, &token), "").expect(1);
    let preview_argv: Vec<&str> = preview_argv.iter().map(String::as_str).collect();
    assert_stale(&envelope, &["observed"], &preview_argv);
    assert_eq!(common::trees::tree_hash(&wtf), before);
}

#[test]
fn a_backup_restore_after_a_commit_on_its_target_changes_nothing() {
    let scene = Scene::new();
    let (id, wtf) = restorable(&scene);
    scene.install("gh:o/foo", "v1");
    let (args, preview_argv) = restore_args(&id);
    let token = preview(&scene, &args);
    scene.install("gh:o/bar", "bar");
    let before = common::trees::tree_hash(&wtf);
    let envelope = scene.run(&bound(&args, &token), "").expect(1);
    let preview_argv: Vec<&str> = preview_argv.iter().map(String::as_str).collect();
    assert_stale(&envelope, &["targets"], &preview_argv);
    assert_eq!(common::trees::tree_hash(&wtf), before);
}

#[test]
fn a_commit_between_a_bound_restores_stage_and_its_lock_changes_nothing() {
    let scene = Scene::new();
    let (id, wtf) = restorable(&scene);
    scene.install("gh:o/foo", "v1");
    let (args, preview_argv) = restore_args(&id);
    let token = preview(&scene, &args);
    let pause = Pause::new(scene.home.env.root());
    let run = bound(&args, &token);
    let run: Vec<&str> = run
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json")
        .collect();
    let child = scene.home.spawn(
        &run,
        &[
            ("OMADDONS_FAILPOINTS", "engine.restore.staged=pause"),
            pause.env(),
        ],
    );
    pause.reached("engine.restore.staged");
    scene.install("gh:o/bar", "bar");
    let before = common::trees::tree_hash(&wtf);
    pause.resume("engine.restore.staged");
    let envelope = common::home::wait(child, &run).expect(1);
    let preview_argv: Vec<&str> = preview_argv.iter().map(String::as_str).collect();
    assert_stale(&envelope, &["targets"], &preview_argv);
    assert_eq!(common::trees::tree_hash(&wtf), before);
}

// ---- `pin` and `unpin` (D3, M4-D19): records only, and `pin --version` ------------------
//
// A client unregistered at the preview and registered by a commit before the bound run
// is stale in `targets`, `selection` and `observed` (its token names it by provisional
// id, then by key; D1). So every scenario that expects one part alone registers the
// client first, by installing Foo.

const PIN: &[&str] = &["pin", "Foo", "-t", "retail"];
const UNPIN: &[&str] = &["unpin", "Foo", "-t", "retail"];
/// An adopting `adopt` with no download: what `shapes::adopt` returns, without `--json`.
const ADOPT_APPLY: &[&str] = &["adopt", "--apply", "-t", "retail", "--no-verify"];
/// One TOC, `Foo.toc`, for retail.
const RETAIL_TOC: &[(&str, &str)] = &[("", "120105")];
/// Builds for retail and Classic Era.
const MAINLINE_AND_VANILLA: &[(&str, u32)] = &[("mainline", 120_105), ("classic", 11_509)];

/// A retail scene with Foo 1 (`Foo`, `FooOld`) installed from `gh:o/foo`: the client is
/// registered.
fn foo_installed(log: &common::tlog::TestLog) -> Scene {
    let scene = Scene::new();
    log.env(&scene);
    log.step("seed home: Foo installed");
    log.run(&scene, &scene.install_args("gh:o/foo", "v1")).ok();
    scene
}

/// Another install commits on the client: Bar, offline from its fixture, so only the
/// client's generation moves.
fn commit_bar(log: &common::tlog::TestLog, scene: &Scene) {
    log.note("Bar installs: the client's generation moves (targets)");
    log.run(scene, &scene.install_args("gh:o/bar", "bar")).ok();
}

/// `args` run unbound, which must succeed.
fn unbound(log: &common::tlog::TestLog, scene: &Scene, args: &[&str]) {
    log.run(scene, &with(&argv(args), &[])).ok();
}

#[test]
fn a_pin_runs_as_previewed() {
    let log = common::tlog::tlog!();
    let scene = foo_installed(&log);

    log.step("preview");
    let (dry, previewed) = b::preview(&log, &scene, "pin", PIN);
    let target = &dry.data()["targets"][0];
    log.assert_eq(
        "the preview: (Foo's item, no transaction)",
        &(&target["items"][0]["status"], &target["tx"]),
        &(&json!("pinned"), &Value::Null),
    );

    log.step("preview again");
    let (_, again) = b::preview(&log, &scene, "pin again", PIN);
    log.assert_eq("an unchanged home, the same token", &again, &previewed);

    log.step("trees");
    let history = record_before(&log, &scene);

    log.step("bound run");
    let ran = log.run(&scene, &b::bound(PIN, &previewed));
    let target = &ran.ok().data()["targets"][0];
    log.assert_eq(
        "(Foo's item, a transaction, a real run's precondition)",
        &(
            &target["items"][0]["status"],
            target["tx"].is_string(),
            &ran.data()["precondition"],
        ),
        &(&json!("pinned"), true, &Value::Null),
    );
    log.assert_eq("Foo pinned", &scene.record("foo")["pinned"], &json!(true));
    log.assert_eq(
        "history gains one line, a pin",
        &scene.history()[history.len()..].to_vec(),
        &vec!["pin".to_owned()],
    );
    log.assert_unchanged("addons", &scene.addons());
}

#[test]
fn an_unpin_runs_as_previewed() {
    let log = common::tlog::tlog!();
    let scene = foo_installed(&log);
    unbound(&log, &scene, PIN);

    log.step("preview");
    let (dry, previewed) = b::preview(&log, &scene, "unpin", UNPIN);
    log.assert_eq(
        "the preview unpins Foo",
        &dry.data()["targets"][0]["items"][0]["status"],
        &json!("unpinned"),
    );

    log.step("preview again");
    let (_, again) = b::preview(&log, &scene, "unpin again", UNPIN);
    log.assert_eq("an unchanged home, the same token", &again, &previewed);

    log.step("trees");
    let history = record_before(&log, &scene);

    log.step("bound run");
    let ran = log.run(&scene, &b::bound(UNPIN, &previewed));
    let target = &ran.ok().data()["targets"][0];
    log.assert_eq(
        "(Foo's item, a transaction, a real run's precondition)",
        &(
            &target["items"][0]["status"],
            target["tx"].is_string(),
            &ran.data()["precondition"],
        ),
        &(&json!("unpinned"), true, &Value::Null),
    );
    log.assert_eq(
        "Foo not pinned",
        &scene.record("foo")["pinned"],
        &json!(false),
    );
    log.assert_eq(
        "history gains one line, a pin transaction",
        &scene.history()[history.len()..].to_vec(),
        &vec!["pin".to_owned()],
    );
    log.assert_unchanged("addons", &scene.addons());
}

/// The plan's acceptance: the bound run finds nothing left to change, and its no-op
/// check sees the moved generation.
#[test]
fn a_concurrent_pin_makes_a_bound_pin_stale_on_its_noop_path() {
    let log = common::tlog::tlog!();
    let scene = foo_installed(&log);

    log.step("preview");
    let (_, previewed) = b::preview(&log, &scene, "pin", PIN);

    log.step("change the world: a concurrent pin");
    unbound(&log, &scene, PIN);

    log.step("trees");
    let history = record_before(&log, &scene);

    log.step("bound run");
    let ran = log.run(&scene, &b::bound(PIN, &previewed));
    b::assert_stale(&log, &ran, &["targets", "selection"], &b::preview_argv(PIN));

    log.step("assert unchanged");
    assert_left_alone(&log, &scene, &history);

    log.step("fix: preview the no-op, then run it bound");
    let (dry, fresh) = b::preview(&log, &scene, "pin, a no-op", PIN);
    log.assert_eq(
        "(another token, Foo unchanged)",
        // ubs:ignore — comparing preview tokens, which are no credential
        &(fresh != previewed, &dry.data()["summary"]["unchanged"]),
        &(true, &json!(1)),
    );
    let ran = log.run(&scene, &b::bound(PIN, &fresh));
    log.assert_eq(
        "a no-op bound to its own no-op preview: (unchanged, precondition)",
        &(
            &ran.ok().data()["summary"]["unchanged"],
            &ran.data()["precondition"],
        ),
        &(&json!(1), &Value::Null),
    );
    assert_left_alone(&log, &scene, &history);
}

#[test]
fn an_unpin_after_a_concurrent_unpin_is_stale() {
    let log = common::tlog::tlog!();
    let scene = foo_installed(&log);
    unbound(&log, &scene, PIN);

    log.step("preview");
    let (_, previewed) = b::preview(&log, &scene, "unpin", UNPIN);

    log.step("change the world: a concurrent unpin");
    unbound(&log, &scene, UNPIN);

    log.step("trees");
    let history = record_before(&log, &scene);

    log.step("bound run");
    let ran = log.run(&scene, &b::bound(UNPIN, &previewed));
    b::assert_stale(
        &log,
        &ran,
        &["targets", "selection"],
        &b::preview_argv(UNPIN),
    );

    log.step("assert unchanged");
    assert_left_alone(&log, &scene, &history);
    log.assert_eq(
        "Foo not pinned",
        &scene.record("foo")["pinned"],
        &json!(false),
    );
}

#[test]
fn a_commit_before_a_bound_pins_lock_changes_nothing() {
    let log = common::tlog::tlog!();
    let scene = foo_installed(&log);

    log.step("preview");
    let (_, previewed) = b::preview(&log, &scene, "pin", PIN);

    log.step("the bound run stops before its lock");
    let pause = Pause::new(scene.home.env.root());
    let child = log
        .cmd(&scene, &b::bound(PIN, &previewed))
        .failpoints("engine.snapshot.prestaged=pause")
        .pause(&pause)
        .spawn();
    log.reached(&pause, "engine.snapshot.prestaged");

    log.step("change the world: a concurrent commit");
    commit_bar(&log, &scene);

    log.step("trees");
    let history = record_before(&log, &scene);

    log.step("the bound run goes on");
    log.resume(&pause, "engine.snapshot.prestaged");
    let ran = child.wait();
    log.json("bound run (child)", ran.envelope());
    b::assert_stale(&log, &ran, &["targets"], &b::preview_argv(PIN));
    log.assert_eq(
        "the refused item beside it, in details.items",
        &ran.envelope()["error"]["details"]["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        &true,
    );

    log.step("assert unchanged");
    assert_left_alone(&log, &scene, &history);
    log.assert_eq(
        "Foo not pinned",
        &scene.record("foo")["pinned"],
        &json!(false),
    );

    log.step("the same race without --if-match commits: binding is opt-in");
    let pause = Pause::new(scene.home.env.root());
    let child = log
        .cmd(&scene, &with(&argv(PIN), &[]))
        .failpoints("engine.snapshot.prestaged=pause")
        .pause(&pause)
        .spawn();
    log.reached(&pause, "engine.snapshot.prestaged");
    unbound(&log, &scene, &["pin", "Bar", "-t", "retail"]);
    log.resume(&pause, "engine.snapshot.prestaged");
    child.wait().ok();
    log.assert_eq("Foo pinned", &scene.record("foo")["pinned"], &json!(true));
}

/// The set-up of `command` on a scene with Foo installed: Foo unpinned for `pin`, pinned
/// for `unpin`, Echo and Fox unpacked by hand for `adopt`; returns its argv.
fn records_only_case(
    log: &common::tlog::TestLog,
    scene: &Scene,
    command: &str,
) -> &'static [&'static str] {
    match command {
        "pin" => PIN,
        "unpin" => {
            unbound(log, scene, PIN);
            UNPIN
        }
        _ => {
            common::engine::shapes::adopt(scene);
            ADOPT_APPLY
        }
    }
}

/// A bound dry run compares its token on these commands (D2's rule): stale, it is
/// `plan_stale` and writes nothing; matching, it returns the token it was given.
#[test]
fn a_bound_dry_run_of_pin_unpin_or_adopt_compares_its_token() {
    let log = common::tlog::tlog!();
    for command in ["pin", "unpin", "adopt"] {
        log.step(&format!("row: {command}"));
        let scene = foo_installed(&log);
        let args = records_only_case(&log, &scene, command);
        let records = scene.home.state().join("targets");

        log.step(&format!("{command}: preview"));
        let (_, previewed) = b::preview(&log, &scene, command, args);

        log.step(&format!("{command}: change the world"));
        commit_bar(&log, &scene);
        log.tree("records", &records);

        log.step(&format!("{command}: a stale bound dry run"));
        let ran = log.run(&scene, &bound_dry(args, &previewed));
        b::assert_stale(&log, &ran, &["targets"], &b::preview_argv(args));
        log.assert_unchanged("records", &records);

        log.step(&format!("{command}: a matching bound dry run"));
        let (_, fresh) = b::preview(&log, &scene, command, args);
        let ran = log.run(&scene, &bound_dry(args, &fresh));
        log.assert_eq(
            "the token it was given",
            &ran.ok().data()["precondition"],
            &json!(fresh),
        );
    }
}

/// A commit on the client between the preview and the bound run: refused before the run
/// stages anything.
#[test]
fn a_commit_on_the_client_stales_each_bound_run() {
    let log = common::tlog::tlog!();
    for command in ["pin", "unpin", "adopt"] {
        log.step(&format!("row: {command}"));
        let scene = foo_installed(&log);
        let args = records_only_case(&log, &scene, command);

        log.step(&format!("{command}: preview"));
        let (_, previewed) = b::preview(&log, &scene, command, args);

        log.step(&format!("{command}: change the world"));
        commit_bar(&log, &scene);
        let guard = b::guard(&log, &scene.home, command);

        log.step(&format!("{command}: bound run"));
        let ran = log.run(&scene, &b::bound(args, &previewed));
        b::assert_stale(&log, &ran, &["targets"], &b::preview_argv(args));
        guard.unchanged(&log);
    }
}

/// A commit on the client while the bound run waits before its lock: refused under the
/// lock, not retryable, with the refused items beside. `pin`'s row, with the opt-in
/// tail, is `a_commit_before_a_bound_pins_lock_changes_nothing`.
#[test]
fn a_commit_before_the_lock_stales_a_bound_unpin_or_adopt() {
    let log = common::tlog::tlog!();
    for command in ["unpin", "adopt"] {
        log.step(&format!("row: {command}"));
        let scene = foo_installed(&log);
        let args = records_only_case(&log, &scene, command);

        log.step(&format!("{command}: preview"));
        let (_, previewed) = b::preview(&log, &scene, command, args);

        log.step(&format!("{command}: the bound run stops before its lock"));
        let pause = Pause::new(scene.home.env.root());
        let child = log
            .cmd(&scene, &b::bound(args, &previewed))
            .failpoints("engine.snapshot.prestaged=pause")
            .pause(&pause)
            .spawn();
        log.reached(&pause, "engine.snapshot.prestaged");

        log.step(&format!("{command}: change the world"));
        commit_bar(&log, &scene);
        let guard = b::guard(&log, &scene.home, command);

        log.step(&format!("{command}: the bound run goes on"));
        log.resume(&pause, "engine.snapshot.prestaged");
        let ran = child.wait();
        log.json("bound run (child)", ran.envelope());
        b::assert_stale(&log, &ran, &["targets"], &b::preview_argv(args));
        log.assert_eq(
            "the refused items beside it, in details.items",
            &ran.envelope()["error"]["details"]["items"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            &true,
        );
        guard.unchanged(&log);
    }
}

/// Retail with Foo v2 installed from `gh:o/foo` while v1, its previous release, can
/// still be asked for by its tag.
fn foo_v2_installed(log: &common::tlog::TestLog) -> Scene {
    let scene = Scene::new();
    log.env(&scene);
    log.step("seed home: Foo v1 and v2 released, v2 installed");
    for tag in ["v1", "v2"] {
        publish_foo(&scene.home, tag, RETAIL_TOC);
    }
    log.run(&scene, &["install", "gh:o/foo", "-t", "retail", "--json"])
        .ok();
    scene
}

const PIN_V1: &[&str] = &["pin", "foo", "--version", "v1", "-t", "retail"];

#[test]
fn a_pin_version_previews_once_from_download_and_cache_and_binds_its_folders() {
    let log = common::tlog::tlog!();
    let scene = foo_v2_installed(&log);
    let assets = |s: &Scene| common::netlog::net_log(&s.home).count_purpose("asset");

    log.step("preview 1: downloads v1");
    let before = assets(&scene);
    let (_, previewed) = b::preview(&log, &scene, "pin --version", PIN_V1);
    let downloaded = assets(&scene) - before;
    log.json("asset requests", &downloaded);

    log.step("preview 2: from the download cache");
    let (_, again) = b::preview(&log, &scene, "pin --version again", PIN_V1);
    log.json("asset requests", &(assets(&scene) - before - downloaded));
    log.assert_eq(
        "(the same token, the first preview downloaded, the second did not)",
        &(
            // ubs:ignore — comparing preview tokens, which are no credential
            again == previewed,
            downloaded > 0,
            assets(&scene) - before - downloaded,
        ),
        &(true, true, 0),
    );

    log.step("bound run");
    let ran = log.run(&scene, &b::bound(PIN_V1, &previewed));
    log.assert_eq(
        "(a real run's precondition; Foo's record: version, pinned)",
        &(
            &ran.ok().data()["precondition"],
            &scene.record("foo")["version"],
            &scene.record("foo")["pinned"],
        ),
        &(&Value::Null, &json!("v1"), &json!(true)),
    );
    let expected = Scene::new();
    publish_foo(&expected.home, "v1", RETAIL_TOC);
    log.run(
        &expected,
        &["install", "gh:o/foo", "-t", "retail", "--json"],
    )
    .ok();
    log.assert_eq(
        "Foo/, as installing v1 leaves it",
        &common::trees::tree_hash(&scene.addons().join("Foo")),
        &common::trees::tree_hash(&expected.addons().join("Foo")),
    );

    log.step("stale, on a fresh home in the same state");
    let scene = foo_v2_installed(&log);
    log.step("preview");
    let (_, previewed) = b::preview(&log, &scene, "pin --version", PIN_V1);
    log.step("change the world: Foo/core.lua edited by hand, the same size");
    log.note("in a folder the run replaces: observed");
    let core = scene.addons().join("Foo/core.lua");
    let text = std::fs::read_to_string(&core).unwrap();
    let edited = text.replace("v2", "x2");
    assert!(edited.len() == text.len() && edited != text, "{text}");
    std::fs::write(&core, edited).unwrap();
    let guard = b::guard(&log, &scene.home, "pin --version");
    log.step("bound run");
    let ran = log.run(&scene, &b::bound(PIN_V1, &previewed));
    b::assert_stale(&log, &ran, &["observed"], &b::preview_argv(PIN_V1));
    log.step("assert unchanged");
    guard.unchanged(&log);
}

/// M4-D61: the guarantee is per client. A two-client bound `pin` whose second client
/// receives a commit while the run waits before the first client's lock commits the
/// first and refuses the second.
#[test]
fn a_bound_pin_over_two_clients_commits_one_and_refuses_the_changed_one() {
    let log = common::tlog::tlog!();
    let home = common::home::Home::new(&["wow", "wow_classic_era"]);
    log.env(&home);
    log.step("seed home: Foo and Bar on retail and era");
    publish_foo(
        &home,
        "v1",
        &[("_Mainline", "120105"), ("_Vanilla", "11509")],
    );
    b::publish(&home, "bar", "Bar", "v1", MAINLINE_AND_VANILLA);
    for client in ["retail", "era"] {
        log.run(
            &home,
            &["install", "gh:o/foo", "gh:o/bar", "-t", client, "--json"],
        )
        .ok();
    }
    // The engine commits registered clients in key order: the second one is raced.
    let mut keyed = [
        ("retail", "_retail_", common::state::key_of(&home, "retail")),
        ("era", "_classic_era_", common::state::key_of(&home, "era")),
    ];
    keyed.sort_by_key(|(_, _, key)| *key);
    let [(first, _, _), (second, second_folder, _)] = keyed;
    log.json("commit order", &[first, second]);
    let pin = ["pin", "foo"];

    log.step("preview: both clients");
    let (dry, previewed) = b::preview(&log, &home, "pin", &pin);
    log.assert_eq(
        "both clients pin Foo",
        &dry.data()["summary"]["changed"],
        &json!(2),
    );

    log.step("the bound run stops before the first client's lock");
    let pause = Pause::new(home.env.root());
    let child = log
        .cmd(&home, &b::bound(&pin, &previewed))
        .failpoints("engine.snapshot.prestaged=pause")
        .pause(&pause)
        .spawn();
    log.reached(&pause, "engine.snapshot.prestaged");

    log.step(&format!("change the world: Bar is pinned on {second}"));
    log.run(&home, &["pin", "bar", "-t", second, "--json"]).ok();
    let record = |id: &str| serde_json::to_value(common::state::record(&home, id)).unwrap();
    let raced = record(second);
    log.tree("second addons", &home.addons(second_folder));

    log.step("the bound run goes on");
    log.resume(&pause, "engine.snapshot.prestaged");
    let ran = child.wait();
    let data = ran.ok().data();
    let target = |id: &str| {
        data["targets"]
            .as_array()
            .unwrap()
            .iter()
            // ubs:ignore — comparing target ids, not secrets
            .find(|t| t["target"]["id"] == id)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let (one, two) = (target(first), target(second));
    log.assert_eq(
        "(partial; the first: committed, its item, no error)",
        &(
            &ran.envelope()["partial"],
            one["tx"].is_string(),
            &one["items"][0]["status"],
            &one["error"],
        ),
        &(&json!(true), true, &json!("pinned"), &Value::Null),
    );
    log.assert_eq(
        "the second refused: (code, retryable, changed, its item, tx)",
        &(
            &two["error"]["code"],
            &two["error"]["retryable"],
            &two["error"]["details"]["changed"],
            &two["items"][0]["status"],
            &two["tx"],
        ),
        &(
            &json!("plan_stale"),
            &json!(false),
            &json!(["targets"]),
            &json!("failed"),
            &Value::Null,
        ),
    );
    let pinned = |id: &str| common::state::record(&home, id).addons["foo"].pinned;
    log.assert_eq(
        "Foo pinned: (first, second)",
        &(pinned(first), pinned(second)),
        &(true, false),
    );
    log.assert_eq(
        "the second's record, as the concurrent pin left it",
        &record(second),
        &raced,
    );
    log.assert_unchanged("second addons", &home.addons(second_folder));
}

// ---- `adopt` (D3, M4-D19): the folders it records, file by file -------------------------

/// `(name, status)` of a run's first target's items, sorted.
fn adopt_items(data: &Value) -> Vec<(String, String)> {
    let mut items: Vec<(String, String)> = data["targets"][0]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| {
            (
                i["name"].as_str().unwrap_or_default().to_owned(),
                i["status"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    items.sort();
    items
}

/// Echo and Fox, each with `status`.
fn echo_and_fox(status: &str) -> Vec<(String, String)> {
    ["Echo", "Fox"]
        .map(|name| (name.to_owned(), status.to_owned()))
        .to_vec()
}

/// A retail scene with Echo and Fox unpacked by hand and in the catalog, and
/// `Echo/Echo.lua` holding `aaaa`.
#[cfg(test)]
fn adoptable(log: &common::tlog::TestLog) -> Scene {
    let scene = Scene::new();
    log.env(&scene);
    log.step("seed home: Echo and Fox unpacked by hand");
    common::engine::shapes::adopt(&scene);
    std::fs::write(scene.addons().join("Echo/Echo.lua"), "aaaa").unwrap();
    scene
}

/// Whether neither Echo nor Fox has a record.
fn nothing_recorded(log: &common::tlog::TestLog, scene: &Scene) {
    log.assert_eq(
        "no record: (Echo, Fox)",
        &(scene.record("echo"), scene.record("fox")),
        &(Value::Null, Value::Null),
    );
}

#[test]
fn an_adopt_runs_as_previewed() {
    let log = common::tlog::tlog!();
    let scene = adoptable(&log);

    log.step("preview");
    let (dry, previewed) = b::preview(&log, &scene, "adopt", ADOPT_APPLY);
    log.assert_eq(
        "the preview adopts Echo and Fox",
        &adopt_items(dry.data()),
        &echo_and_fox("adopted"),
    );

    log.step("preview again");
    let (_, again) = b::preview(&log, &scene, "adopt again", ADOPT_APPLY);
    log.assert_eq("an unchanged home, the same token", &again, &previewed);

    log.step("trees");
    log.tree("addons", &scene.addons());

    log.step("bound run");
    let ran = log.run(&scene, &b::bound(ADOPT_APPLY, &previewed));
    log.assert_eq(
        "(Echo and Fox adopted, a real run's precondition)",
        &(adopt_items(ran.ok().data()), &ran.data()["precondition"]),
        &(echo_and_fox("adopted"), &Value::Null),
    );
    log.assert_eq(
        "recorded: (Echo, Fox)",
        &(
            scene.record("echo").is_object(),
            scene.record("fox").is_object(),
        ),
        &(true, true),
    );
    log.assert_unchanged("addons", &scene.addons());
}

/// One adopting run on a scene with Foo installed and Echo and Fox unpacked: its argv,
/// the items it reports, and the record whose ref it leaves as `r#ref`.
struct AdoptKind {
    row: &'static str,
    args: &'static [&'static str],
    items: Vec<(String, String)>,
    record: &'static str,
    r#ref: &'static str,
}

/// The other adopting modes bind as `--apply` does: folder names, and `--ref` on an
/// unmanaged and on a managed folder.
#[test]
fn a_bound_adopt_of_each_kind_runs_as_previewed() {
    let log = common::tlog::tlog!();
    let kinds = [
        AdoptKind {
            row: "folder names",
            args: &["adopt", "Echo", "Fox", "-t", "retail", "--no-verify"],
            items: echo_and_fox("adopted"),
            record: "echo",
            r#ref: "gh:o/echo",
        },
        AdoptKind {
            row: "--ref on an unmanaged folder",
            args: &[
                "adopt",
                "Echo",
                "--ref",
                "gh:o/echo-fork",
                "-t",
                "retail",
                "--no-verify",
            ],
            items: vec![("Echo".to_owned(), "adopted".to_owned())],
            record: "echo",
            r#ref: "gh:o/echo-fork",
        },
        AdoptKind {
            row: "--ref on a managed folder",
            args: &["adopt", "Foo", "--ref", "gh:o/bar", "-t", "retail"],
            items: vec![("Foo".to_owned(), "edited".to_owned())],
            record: "foo",
            r#ref: "gh:o/bar",
        },
    ];
    for AdoptKind {
        row,
        args,
        items,
        record,
        r#ref,
    } in kinds
    {
        log.step(&format!("row: {row}"));
        let scene = foo_installed(&log);
        common::engine::shapes::adopt(&scene);

        log.step(&format!("{row}: preview"));
        let (dry, previewed) = b::preview(&log, &scene, row, args);
        log.assert_eq("the preview's items", &adopt_items(dry.data()), &items);

        log.step(&format!("{row}: preview again"));
        let (_, again) = b::preview(&log, &scene, row, args);
        log.assert_eq("an unchanged home, the same token", &again, &previewed);

        log.step(&format!("{row}: trees"));
        log.tree("addons", &scene.addons());

        log.step(&format!("{row}: bound run"));
        let ran = log.run(&scene, &b::bound(args, &previewed));
        log.assert_eq(
            "(the preview's items, a real run's precondition)",
            &(adopt_items(ran.ok().data()), &ran.data()["precondition"]),
            &(items, &Value::Null),
        );
        log.assert_eq(
            &format!("{record}'s ref"),
            &scene.record(record)["ref"],
            &json!(r#ref),
        );
        log.assert_unchanged("addons", &scene.addons());
    }
}

/// An edit by hand in a client's AddOns directory.
type HandEdit = fn(&std::path::Path);

#[test]
fn an_edited_folder_makes_a_bound_adopt_stale_observed() {
    let log = common::tlog::tlog!();
    let rows: [(&str, HandEdit); 3] = [
        ("same-size edit", |addons| {
            std::fs::write(addons.join("Echo/Echo.lua"), "bbbb").unwrap();
        }),
        ("longer write", |addons| {
            std::fs::write(addons.join("Echo/Echo.lua"), "bbbbbbbb").unwrap();
        }),
        ("empty directory added", |addons| {
            std::fs::create_dir(addons.join("Fox/Media")).unwrap();
        }),
    ];
    for (row, edit) in rows {
        log.step(&format!("row: {row}"));
        let scene = adoptable(&log);

        log.step(&format!("{row}: preview"));
        let (_, previewed) = b::preview(&log, &scene, row, ADOPT_APPLY);

        log.step(&format!("{row}: change the world"));
        log.note(format!("{row} in a folder the run records: observed"));
        edit(&scene.addons());

        log.step(&format!("{row}: trees"));
        let history = record_before(&log, &scene);

        log.step(&format!("{row}: bound run"));
        let ran = log.run(&scene, &b::bound(ADOPT_APPLY, &previewed));
        b::assert_stale(&log, &ran, &["observed"], &b::preview_argv(ADOPT_APPLY));

        log.step(&format!("{row}: assert unchanged"));
        assert_left_alone(&log, &scene, &history);
        nothing_recorded(&log, &scene);

        log.step(&format!("{row}: fix"));
        let (_, fresh) = b::preview(&log, &scene, row, ADOPT_APPLY);
        let ran = log.run(&scene, &b::bound(ADOPT_APPLY, &fresh));
        log.assert_eq(
            "the fix's token adopts",
            &adopt_items(ran.ok().data()),
            &echo_and_fox("adopted"),
        );
    }
}

/// The witness is the folders' content, not their stamps (D1; M4-D60).
#[test]
fn a_touch_alone_leaves_an_adopt_preview_valid() {
    let log = common::tlog::tlog!();
    let scene = adoptable(&log);

    log.step("preview");
    let (_, previewed) = b::preview(&log, &scene, "adopt", ADOPT_APPLY);

    log.step("change the world: Echo/Echo.lua touched, a minute later, its bytes kept");
    let path = scene.addons().join("Echo/Echo.lua");
    let file = std::fs::File::open(&path).unwrap();
    let later = file.metadata().unwrap().modified().unwrap() + std::time::Duration::from_secs(60);
    file.set_modified(later).unwrap();
    log.tree("addons", &scene.addons());

    log.step("bound run");
    let ran = log.run(&scene, &b::bound(ADOPT_APPLY, &previewed));
    log.assert_eq(
        "adopted",
        &adopt_items(ran.ok().data()),
        &echo_and_fox("adopted"),
    );
    log.assert_unchanged("addons", &scene.addons());
}

#[test]
fn an_adopt_whose_folders_were_adopted_meanwhile_is_stale_on_its_noop_path() {
    let log = common::tlog::tlog!();
    let scene = adoptable(&log);

    log.step("preview");
    let (_, previewed) = b::preview(&log, &scene, "adopt", ADOPT_APPLY);

    log.step("change the world: Echo and Fox adopted by name");
    unbound(
        &log,
        &scene,
        &["adopt", "Echo", "Fox", "-t", "retail", "--no-verify"],
    );

    log.step("trees");
    let history = record_before(&log, &scene);

    log.step("bound run: nothing left to adopt, and stale");
    log.note("the preview witnessed two folders; the no-op's witness is bare");
    let ran = log.run(&scene, &b::bound(ADOPT_APPLY, &previewed));
    b::assert_stale(
        &log,
        &ran,
        &["targets", "selection", "observed"],
        &b::preview_argv(ADOPT_APPLY),
    );

    log.step("assert unchanged");
    assert_left_alone(&log, &scene, &history);
}

#[test]
fn a_bound_adopt_with_verification_previews_once_from_download_and_cache() {
    let log = common::tlog::tlog!();
    let scene = Scene::new();
    log.env(&scene);
    log.step("seed home: Foo v2 released, in the catalog, and unpacked by hand");
    exact_foo(&scene);
    let apply = ["adopt", "--apply", "-t", "retail"];
    let assets = |s: &Scene| common::netlog::net_log(&s.home).count_purpose("asset");

    log.step("preview 1: downloads the release to check the folders against it");
    let (dry, previewed) = b::preview(&log, &scene, "adopt", &apply);
    let downloaded = assets(&scene);
    log.json("asset requests", &downloaded);
    log.assert_eq(
        "the preview adopts Foo",
        &adopt_items(dry.data()),
        &vec![("Foo".to_owned(), "adopted".to_owned())],
    );

    log.step("preview 2: from the download cache");
    let (_, again) = b::preview(&log, &scene, "adopt again", &apply);
    log.json("asset requests", &(assets(&scene) - downloaded));
    log.assert_eq(
        "(the same token, the first preview downloaded, the second did not)",
        &(
            // ubs:ignore — comparing preview tokens, which are no credential
            again == previewed,
            downloaded > 0,
            assets(&scene) - downloaded,
        ),
        &(true, true, 0),
    );

    log.step("bound run");
    let ran = log.run(&scene, &b::bound(&apply, &previewed));
    log.assert_eq(
        "(a real run's precondition, Foo recorded as exactly its release)",
        &(
            &ran.ok().data()["precondition"],
            &scene.record("foo")["version_confidence"],
        ),
        &(&Value::Null, &json!("release")),
    );
}

#[test]
fn a_bound_adopt_ref_repoint_is_stale_after_a_commit() {
    let log = common::tlog::tlog!();
    let scene = foo_installed(&log);
    let repoint = ["adopt", "Foo", "--ref", "gh:o/bar", "-t", "retail"];

    log.step("preview");
    let (dry, previewed) = b::preview(&log, &scene, "adopt --ref", &repoint);
    log.assert_eq(
        "the preview re-points Foo",
        &adopt_items(dry.data()),
        &vec![("Foo".to_owned(), "edited".to_owned())],
    );

    log.step("change the world: Foo pinned");
    log.note("a records-only commit on the client: its generation moves");
    unbound(&log, &scene, PIN);

    log.step("trees");
    let history = record_before(&log, &scene);

    log.step("bound run");
    let ran = log.run(&scene, &b::bound(&repoint, &previewed));
    b::assert_stale(&log, &ran, &["targets"], &b::preview_argv(&repoint));

    log.step("assert unchanged");
    assert_left_alone(&log, &scene, &history);
    log.assert_eq(
        "Foo still from gh:o/foo",
        &scene.record("foo")["ref"],
        &json!("gh:o/foo"),
    );
}

/// The edit lands after the bound run's own check, so the approval witness checked again
/// under the lock (D8, `omad-m4-agent-surface-doctor-c6cm.4.8`) refuses it, as the
/// non-retryable `observed` naming the folder, before adopt's own guard is reached; and
/// running the same bound command again is `observed` too. Foo is installed first: a
/// refusal under the lock comes after the commit window registers a new client, which
/// would rename it in the re-run's token.
#[test]
fn a_bound_adopt_edited_before_its_lock_records_nothing_and_converges() {
    let log = common::tlog::tlog!();
    let scene = foo_installed(&log);
    common::engine::shapes::adopt(&scene);
    std::fs::write(scene.addons().join("Echo/Echo.lua"), "aaaa").unwrap();

    log.step("preview");
    let (_, previewed) = b::preview(&log, &scene, "adopt", ADOPT_APPLY);

    log.step("the bound run stops before its lock");
    let pause = Pause::new(scene.home.env.root());
    let child = log
        .cmd(&scene, &b::bound(ADOPT_APPLY, &previewed))
        .failpoints("engine.snapshot.prestaged=pause")
        .pause(&pause)
        .spawn();
    log.reached(&pause, "engine.snapshot.prestaged");

    log.step("change the world: Echo/Echo.lua grows");
    std::fs::write(scene.addons().join("Echo/Echo.lua"), "aaaaaaaa").unwrap();

    log.step("trees");
    let history = record_before(&log, &scene);

    log.step("the bound run goes on");
    log.resume(&pause, "engine.snapshot.prestaged");
    let ran = child.wait();
    log.json("bound run (child)", ran.envelope());
    b::assert_stale(&log, &ran, &["observed"], &b::preview_argv(ADOPT_APPLY));
    let message = ran.envelope()["error"]["message"]
        .as_str()
        .unwrap_or_default();
    log.assert_eq(
        "the refusal names the folder that changed",
        &message.contains("echo changed after the preview"),
        &true,
    );
    assert_left_alone(&log, &scene, &history);
    nothing_recorded(&log, &scene);

    log.step("rerun the same bound command");
    let ran = log.run(&scene, &b::bound(ADOPT_APPLY, &previewed));
    b::assert_stale(&log, &ran, &["observed"], &b::preview_argv(ADOPT_APPLY));
    assert_left_alone(&log, &scene, &history);
    nothing_recorded(&log, &scene);
}

#[test]
fn adopts_report_refuses_if_match_and_its_fix_runs() {
    let log = common::tlog::tlog!();
    let scene = adoptable(&log);
    let state = scene.home.state();

    log.step("preview");
    let (_, previewed) = b::preview(&log, &scene, "adopt", ADOPT_APPLY);
    log.tree("state", &state);

    log.step("the report, given the token");
    let report = ["adopt", "-t", "retail", "--no-verify"];
    let ran = log.run(&scene, &b::bound(&report, &previewed));
    let fix = &ran.fails(2, "usage").envelope()["error"]["fix_argv"];
    log.assert_eq(
        "the fix: the report without the token",
        fix,
        &json!(["omaddons", "adopt", "-t", "retail", "--no-verify"]),
    );
    log.assert_unchanged("state", &state);

    log.step("fix");
    let fix: Vec<&str> = fix.as_array().unwrap()[1..]
        .iter()
        .filter_map(Value::as_str)
        .chain(["--json"])
        .collect();
    let ran = log.run(&scene, &fix);
    let mut groups: Vec<&str> = ran.ok().data()["targets"][0]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|g| g["name"].as_str())
        .collect();
    groups.sort_unstable();
    log.assert_eq(
        "the report lists Echo and Fox",
        &groups,
        &vec!["Echo", "Fox"],
    );
}

// ---- every path (D3): a no-op preview binds too -------------------------------------------

/// A bound run with nothing to do never reports "unchanged" over a world its preview did
/// not see.
#[test]
fn a_noop_preview_goes_stale_when_its_target_moves() {
    let log = common::tlog::tlog!();
    for command in ["pin", "unpin", "adopt"] {
        log.step(&format!("row: {command}"));
        let scene = foo_installed(&log);
        // Foo already pinned; Foo not pinned; nothing unmanaged.
        let args = match command {
            "pin" => {
                unbound(&log, &scene, PIN);
                PIN
            }
            "unpin" => UNPIN,
            _ => ADOPT_APPLY,
        };

        log.step(&format!("{command}: the no-op preview"));
        let (dry, previewed) = b::preview(&log, &scene, command, args);
        log.assert_eq(
            "nothing changes",
            &dry.data()["summary"]["changed"],
            &json!(0),
        );

        log.step(&format!("{command}: change the world"));
        commit_bar(&log, &scene);
        let guard = b::guard(&log, &scene.home, command);

        log.step(&format!("{command}: bound run"));
        let ran = log.run(&scene, &b::bound(args, &previewed));
        b::assert_stale(&log, &ran, &["targets"], &b::preview_argv(args));
        guard.unchanged(&log);

        log.step(&format!("{command}: fix"));
        let (_, fresh) = b::preview(&log, &scene, command, args);
        let ran = log.run(&scene, &b::bound(args, &fresh));
        log.assert_eq(
            "a no-op bound to its own preview: (changed, precondition)",
            &(
                &ran.ok().data()["summary"]["changed"],
                &ran.data()["precondition"],
            ),
            &(&json!(0), &Value::Null),
        );
        guard.unchanged(&log);
    }
}

// ---- `remove` checked again under the lock (D8, M4-D60) -------------------------------------

/// `run` stopped at `point`, `act` done while it waits, then resumed; returns the run.
#[cfg(test)]
fn paused_at(
    log: &common::tlog::TestLog,
    scene: &Scene,
    run: &[String],
    point: &str,
    act: impl FnOnce(),
) -> common::tlog::Ran {
    log.step("the run stops at its pause");
    let pause = Pause::new(scene.home.env.root());
    let child = log
        .cmd(scene, run)
        .failpoints(&format!("{point}=pause"))
        .pause(&pause)
        .spawn();
    log.reached(&pause, point);
    act();
    log.step("the run goes on");
    log.resume(&pause, point);
    child.wait()
}

/// Foo 1 installed, then both its folders deleted by hand: `remove Foo` forgets it, a
/// records-only change that moves no folder.
#[cfg(test)]
fn foo_forgettable(log: &common::tlog::TestLog) -> Scene {
    let scene = foo_installed(log);
    log.note("Foo's folders deleted by hand");
    for folder in common::engine::FOLDERS {
        std::fs::remove_dir_all(scene.addons().join(folder)).unwrap();
    }
    scene
}

/// `Foo/Foo.toc` put back by hand.
#[cfg(test)]
fn put_foo_back(scene: &Scene) {
    let folder = scene.addons().join("Foo");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("Foo.toc"), "## Interface: 120105\n").unwrap();
}

/// A forget whose folder is put back after the bound run's own check no longer applies:
/// the operations re-judged under the lock are not the approved ones (`selection`). A
/// forget moves no folder, so the witness has nothing to refuse first. Unbound, the same
/// race changes nothing either; what it reports is `omad-z60g`.
#[test]
fn a_bound_remove_whose_forget_no_longer_applies_changes_nothing() {
    let log = common::tlog::tlog!();
    let scene = foo_forgettable(&log);
    log.step("preview: Foo forgotten");
    let (dry, token) = b::preview(&log, &scene, "remove", REMOVE);
    log.assert_eq(
        "the preview's item: (status, folders it moves)",
        &(
            &dry.data()["items"][0]["status"],
            &dry.data()["items"][0]["folders"],
        ),
        &(&json!("removed"), &json!([])),
    );
    let generation = scene.generation();
    let mut guard = None;
    let ran = paused_at(
        &log,
        &scene,
        &b::bound(REMOVE, &token),
        "engine.snapshot.prestaged",
        || {
            log.step("Foo/Foo.toc put back by hand");
            put_foo_back(&scene);
            guard = Some(b::guard(&log, &scene.home, "after the edit"));
        },
    );
    b::assert_stale(&log, &ran, &["selection"], &argv(REMOVE_PREVIEW));
    guard.unwrap().unchanged(&log);
    log.assert_eq(
        "(Foo recorded, the generation)",
        &(scene.record("foo").is_object(), scene.generation()),
        &(true, generation),
    );

    log.step("unbound, the same race");
    let scene = foo_forgettable(&log);
    let generation = scene.generation();
    let mut unbound = argv(REMOVE);
    unbound.push("--json".to_owned());
    let mut guard = None;
    let ran = paused_at(&log, &scene, &unbound, "engine.snapshot.prestaged", || {
        log.step("Foo/Foo.toc put back by hand");
        put_foo_back(&scene);
        guard = Some(b::guard(&log, &scene.home, "unbound, after the edit"));
    });
    log.json("the unbound run's envelope", ran.envelope());
    log.step("nothing changed: Foo still recorded, its folder as put back");
    guard.unwrap().unchanged(&log);
    log.assert_eq(
        "(Foo recorded, the generation)",
        &(scene.record("foo").is_object(), scene.generation()),
        &(true, generation),
    );
}

/// Foo's last folder (in the swap's folder-key order, after Bar's and Foo's) replaced by
/// an exact copy after both under-lock checks: its rename's identity check refuses, the
/// two removals before it are rolled back, and the bound run is `observed`.
#[test]
fn a_bound_remove_whose_folder_was_replaced_at_the_last_moment_changes_nothing() {
    let log = common::tlog::tlog!();
    let scene = foo_installed(&log);
    commit_bar(&log, &scene);
    let remove = ["remove", "Foo", "Bar", "-t", "retail"];
    log.step("preview");
    let (_, token) = b::preview(&log, &scene, "remove", &remove);
    let generation = scene.generation();
    let guard = b::guard(&log, &scene.home, "before the run");
    let ran = paused_at(
        &log,
        &scene,
        &b::bound(&remove, &token),
        "engine.revalidated",
        || {
            log.step("FooOld replaced by an exact copy: another identity");
            let live = scene.addons().join("FooOld");
            let aside = scene.home.env.root().join("FooOld-aside");
            std::fs::rename(&live, &aside).unwrap();
            common::trees::copy_tree_exact(&aside, &live);
        },
    );
    b::assert_stale(&log, &ran, &["observed"], &b::preview_argv(&remove));
    log.step("rolled back: every folder in place, records and history as before");
    guard.unchanged(&log);
    log.assert_eq("the generation", &scene.generation(), &generation);
    log.assert_eq(
        "(journals, orphan snapshots)",
        &(scene.journals().len(), scene.orphan_snapshots().len()),
        &(0, 0),
    );
}

/// `remove`'s token has no witness: only its operations are compared under the lock, so
/// a bound remove logs no cost note.
#[test]
fn a_bound_remove_logs_no_approval_check() {
    let log = common::tlog::tlog!();
    let scene = foo_installed(&log);
    log.step("preview");
    let (_, token) = b::preview(&log, &scene, "remove", REMOVE);
    log.step("bound run, -v");
    let mut run = b::bound(REMOVE, &token);
    run.push("-v".to_owned());
    let ran = log.run(&scene, &run);
    ran.ok();
    let notes = ran
        .events()
        .iter()
        .filter(|e| e["event"] == "log")
        .filter_map(|e| e["message"].as_str())
        .filter(|m| m.contains("approval checked under the lock"))
        .count();
    log.assert_eq(
        "(Foo removed, cost notes)",
        &(&ran.data()["items"][0]["status"], notes),
        &(&json!("removed"), 0),
    );
}
