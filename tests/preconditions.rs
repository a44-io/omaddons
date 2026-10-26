//! e2e suite: previews bound by `--if-match`, M3's five commands and `install` (D2).
//!
//! Preview binding (M3-D46, M4-D19) end to end: a `--dry-run` of `remove`, `undo`,
//! `backup prune` or `install` returns `data.precondition`, and the real run with
//! `--if-match <it>` does exactly what the preview showed or nothing. Each scenario
//! changes the world between preview and run (a commit on the target, before the run
//! plans or between its plan and the lock; a SavedVariables file the undo would restore;
//! a new backup, or a victim's metadata rewritten) and checks the refusal (`plan_stale`,
//! not retryable, `details.changed`, the preview as `fix_argv`) and that nothing moved.
//! The `install` scenarios log through `TestLog`.

mod common;

use common::binding as b;
use common::engine::Scene;
use common::graph::Folder;
use common::pause::Pause;
use prune_support::{at_noon, backups, prune_preview, three_days};
use restore_support::{restorable, restore_args};
use serde_json::{Value, json};
use support::{argv, preview, token};

#[cfg(test)]
mod support {
    use serde_json::Value;

    use super::common::engine::Scene;

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
        "(without -v, with -v) ends with the token line",
        &(
            plain.ok().stdout().ends_with(&line),
            verbose.ok().stdout().ends_with(&line),
        ),
        &(false, true),
    );

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
