//! Preview binding (M3-D46) end to end: a `--dry-run` of `remove` or `undo` returns
//! `data.precondition`, and the real run with `--if-match <it>` does exactly what the
//! preview showed or nothing. Each scenario changes the world between preview and run (a
//! commit on the target, before the run plans or between its plan and the lock; a
//! SavedVariables file the undo would restore) and checks the refusal (`plan_stale`, not
//! retryable, `details.changed`, the preview as `fix_argv`) and that nothing moved.

mod common;

use common::engine::Scene;
use common::pause::Pause;
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
}

#[test]
fn if_match_goes_only_with_the_binding_commands_and_takes_only_tokens() {
    let scene = Scene::new();
    scene.install("gh:o/foo", "v1");
    let token = preview(&scene, REMOVE);
    let envelope = scene.run(&bound(&["update"], &token), "").expect(2);
    let error = &envelope["error"];
    assert_eq!(error["code"], "usage");
    let message = error["message"].as_str().unwrap();
    for name in [
        "remove",
        "undo",
        "backup restore",
        "backup prune",
        "errors dismiss",
    ] {
        assert!(message.contains(name), "{message}");
    }
    assert_eq!(error["fix_argv"], json!(["omaddons", "help", "update"]));
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
