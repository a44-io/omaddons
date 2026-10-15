//! Undo checkpoints across processes (vision §7 M1), complementing the engine's own
//! undo tests (src/engine/undo.rs: the SavedVariables policy matrix, deferral and the
//! follow-up, undo of undo, --last-by) and tests/undo_history.rs (the CLI surface): an
//! update reverted byte for byte, and `undo_conflict` across transaction kinds.

mod common;

use common::engine::Scene;

#[test]
fn undoing_an_update_restores_every_folder_byte_for_byte() {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    let v1 = s.folders();
    s.install("gh:o/foo", "v2");
    assert_ne!(s.folders(), v1);
    let undo = ["undo", "--offline", "--json"].map(str::to_owned);
    let envelope = s.run(&undo, "").expect(0);
    assert_eq!(envelope["data"]["reverted"]["kind"], "update", "{envelope}");
    assert_eq!(s.folders(), v1, "Foo back to v1 and FooOld back");
    assert_eq!(s.history().last().map(String::as_str), Some("undo"));
}

#[test]
fn a_later_transaction_of_another_kind_conflicts() {
    let s = Scene::new();
    let installed = s.install("gh:o/foo", "v1");
    let install_tx = installed["data"]["tx"].as_str().unwrap().to_owned();
    let remove = ["remove", "Foo", "-t", "retail", "--offline", "--json"].map(str::to_owned);
    let removed = s.run(&remove, "").expect(0);
    let remove_tx = removed["data"]["tx"].as_str().unwrap().to_owned();
    let before = s.folders();

    let undo = ["undo".to_owned(), install_tx, "--json".to_owned()];
    let r = s.run(&undo, "");
    let envelope = r.expect(4);
    assert_eq!(
        envelope["error"]["code"],
        "undo_conflict",
        "{}",
        r.context()
    );
    let conflicting = &envelope["error"]["details"]["conflicting"][0];
    assert_eq!(
        (&conflicting["tx"], &conflicting["kind"]),
        (&serde_json::json!(remove_tx), &serde_json::json!("remove")),
        "{}",
        r.context()
    );
    assert_eq!(s.folders(), before, "nothing changed");
}
