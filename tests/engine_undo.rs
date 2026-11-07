//! Undo checkpoints across processes (vision §7 M1), complementing the engine's own
//! undo tests (src/engine/undo.rs: the SavedVariables policy matrix, deferral and the
//! follow-up, undo of undo, --last-by) and tests/undo_history.rs (the CLI surface): an
//! update reverted byte for byte, and `undo_conflict` across transaction kinds.

use crate::common;

use common::engine::Scene;
use common::trees::tree_hash;

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

#[test]
fn removing_an_addon_deleted_by_hand_forgets_it_and_undo_brings_the_record_back() {
    let s = Scene::new();
    let installed = s.install("gh:o/foo", "v1");
    let install_tx = installed["data"]["tx"].as_str().unwrap().to_owned();
    for folder in ["Foo", "FooOld"] {
        std::fs::remove_dir_all(s.addons().join(folder)).unwrap();
    }
    let list = || {
        let args = ["list", "-t", "retail", "--json"].map(str::to_owned);
        let envelope = s.run(&args, "").expect(0);
        envelope["data"]["targets"][0]["addons"].clone()
    };
    assert_eq!(list().as_array().map(Vec::len), Some(1));

    let remove = ["remove", "Foo", "-t", "retail", "--offline", "--json"].map(str::to_owned);
    let r = s.run(&remove, "");
    let removed = r.expect(0);
    let item = &removed["data"]["items"][0];
    assert_eq!(
        (&item["status"], &item["folders"]),
        (&serde_json::json!("removed"), &serde_json::json!([])),
        "nothing was left to move\n{}",
        r.context()
    );
    assert_eq!(s.history().last().map(String::as_str), Some("remove"));
    let history = ["history", "-v", "--json"].map(str::to_owned);
    let entry = s.run(&history, "").expect(0)["data"]["entries"][0].clone();
    assert_eq!(
        entry["ops"],
        serde_json::json!([
            {"op": "record", "folder": "Foo", "addon": "Foo"},
            {"op": "record", "folder": "FooOld", "addon": "Foo"},
        ]),
        "{entry}"
    );
    assert_eq!(list().as_array().map(Vec::len), Some(0), "forgotten");

    // The forget is undoable, and it conflicts with undoing the install beneath it.
    let undo_install = ["undo".to_owned(), install_tx, "--json".to_owned()];
    assert_eq!(
        s.run(&undo_install, "").expect(4)["error"]["code"],
        "undo_conflict"
    );
    let undo = ["undo", "--offline", "--json"].map(str::to_owned);
    let envelope = s.run(&undo, "").expect(0);
    assert_eq!(envelope["data"]["reverted"]["kind"], "remove", "{envelope}");
    let addons = list();
    assert_eq!(addons[0]["name"], "Foo", "the record is back: {addons}");
    assert!(
        !s.addons().join("Foo").exists(),
        "no folder comes back: none was there"
    );
}

/// A hand-made folder replaced with `--force` exists only in the snapshot, and comes
/// back as it was: directory modes, a 0600 file, a symlink and an empty directory.
#[test]
fn undoing_a_forced_replace_restores_modes_links_and_empty_dirs() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    let s = Scene::new();
    let hand = s.addons().join("Hand");
    for sub in ["empty", "libs", "private"] {
        fs::create_dir_all(hand.join(sub)).unwrap();
    }
    fs::write(hand.join("Hand.toc"), "## Interface: 120105\n").unwrap();
    fs::write(hand.join("libs/lib.lua"), "-- lib").unwrap();
    std::os::unix::fs::symlink("libs/lib.lua", hand.join("link.lua")).unwrap();
    fs::write(hand.join("private/p.lua"), "-- private").unwrap();
    for (path, mode) in [
        ("libs", 0o775),
        ("private/p.lua", 0o600),
        ("private", 0o700),
    ] {
        fs::set_permissions(hand.join(path), fs::Permissions::from_mode(mode)).unwrap();
    }
    let before = tree_hash(&s.addons());

    let packaged = s.home.env.root().join("packaged/Hand");
    fs::create_dir_all(&packaged).unwrap();
    fs::write(
        packaged.join("Hand.toc"),
        "## Interface: 120105\n## Notes: new\n",
    )
    .unwrap();
    let packaged = packaged.to_string_lossy();
    let install = [
        "install",
        "gh:o/hand",
        "--file",
        &packaged,
        "-t",
        "retail",
        "--force",
        "--offline",
        "--json",
    ]
    .map(str::to_owned);
    s.run(&install, "").expect(0);
    assert_ne!(tree_hash(&s.addons()), before);
    let undo = ["undo", "--offline", "--json"].map(str::to_owned);
    let envelope = s.run(&undo, "").expect(0);
    assert_eq!(
        envelope["data"]["reverted"]["kind"], "install",
        "{envelope}"
    );
    assert_eq!(tree_hash(&s.addons()), before, "Hand back as it was");
}
