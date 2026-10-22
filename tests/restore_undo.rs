//! Undo around a whole `WTF/` restore, end to end through `backup restore` (M3-D28): a
//! restore, its undo and the undo of that each give back the tree before them; an older
//! transaction whose undo would restore SavedVariables waits for a later restore to be
//! undone first, or reverts its folders only with `--saved-variables keep`; and the undo
//! of a restore counts the files play changed since (`changed_since`). Every run is
//! checked for the fake account, realm and character names, on both streams.

mod common;

use std::fs;

use common::archives::zip_text;
use common::home::{Home, Run};
use common::privacy::{ACCOUNT, assert_private_all};
use common::trees::tree_hash;
use serde_json::{Value, json};
use support::{install, made, run, settings};

#[cfg(test)]
mod support {
    use super::*;

    /// `omaddons <args> --json --progress json`, checked for privacy on both streams.
    pub fn run(home: &Home, args: &[&str]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let r = home.exec_with(&argv, &[("OMADDONS_FS_FREE_MB", "4096")]);
        assert_private_all(&r.stdout, &r.stderr);
        r
    }

    /// Installs a one-folder `Foo` from a local zip; returns its transaction.
    pub fn install(home: &Home) -> String {
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        let zip = dl.join("Foo.zip");
        let toc = "## Interface: 120105\n## Version: 1\n";
        fs::write(&zip, zip_text(&[("Foo/Foo.toc", toc)])).unwrap();
        let path = zip.display().to_string();
        let envelope = run(
            home,
            &["install", "gh:o/foo", "--file", &path, "-t", "retail"],
        )
        .expect(0);
        envelope["data"]["tx"].as_str().unwrap().to_owned()
    }

    /// Foo's account-wide SavedVariables file.
    pub fn settings(home: &Home) -> std::path::PathBuf {
        home.client("_retail_").join(ACCOUNT).join("Foo.lua")
    }

    /// The id of the backup `backup create` made.
    pub fn made(envelope: &Value) -> String {
        envelope["data"]["targets"][0]["backup"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

#[test]
fn an_older_undo_that_would_restore_settings_waits_for_a_later_restore() {
    let home = Home::new(&["wow"]);
    let sv = settings(&home);
    fs::create_dir_all(sv.parent().unwrap()).unwrap();
    fs::write(&sv, "-- before").unwrap();
    // The install keeps Foo's settings in its snapshot: its undo would put them back.
    let installed = install(&home);
    fs::write(&sv, "-- played").unwrap();
    let id = made(&run(&home, &["backup", "create", "-t", "retail"]).expect(0));
    fs::write(&sv, "-- later").unwrap();
    let restored = run(&home, &["backup", "restore", &id]).expect(0)["data"]["tx"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(fs::read_to_string(&sv).unwrap(), "-- played");

    let r = run(&home, &["undo", &installed]);
    let envelope = r.expect(4);
    let error = &envelope["error"];
    assert_eq!(error["code"], "undo_conflict", "{}", r.context());
    assert_eq!(error["details"]["conflicting"][0]["tx"], json!(restored));
    assert_eq!(error["fix_argv"], json!(["omaddons", "undo", restored]));
    assert_eq!(
        error["alternatives"][0]["argv"],
        json!(["omaddons", "undo", installed, "--saved-variables", "keep"])
    );
    assert_eq!(
        fs::read_to_string(&sv).unwrap(),
        "-- played",
        "nothing changed"
    );

    // Folders only: the restored settings stay.
    run(&home, &["undo", &installed, "--saved-variables", "keep"]).expect(0);
    assert!(!home.addons("_retail_").join("Foo").exists());
    assert_eq!(fs::read_to_string(&sv).unwrap(), "-- played");
}

#[test]
fn the_undo_of_a_restore_counts_the_play_since_and_its_undo_restores_again() {
    let home = Home::new(&["wow"]);
    let sv = settings(&home);
    fs::create_dir_all(sv.parent().unwrap()).unwrap();
    fs::write(&sv, "-- backed up").unwrap();
    let id = made(&run(&home, &["backup", "create", "-t", "retail"]).expect(0));
    fs::write(&sv, "-- replaced").unwrap();
    let wtf = home.client("_retail_").join("WTF");
    let replaced = tree_hash(&wtf);
    run(&home, &["backup", "restore", &id]).expect(0);
    let restored = tree_hash(&wtf);
    // Play since the restore: one file rewritten, one created.
    fs::write(&sv, "-- played since").unwrap();
    fs::write(sv.with_file_name("New.lua"), "New = 1").unwrap();
    let played = tree_hash(&wtf);

    let envelope = run(&home, &["undo", "--dry-run"]).expect(0);
    assert_eq!(envelope["data"]["changed_since"], 2, "{envelope}");
    assert_eq!(tree_hash(&wtf), played, "a dry run writes nothing");
    let r = run(&home, &["undo", "--saved-variables", "keep"]);
    assert_eq!(r.expect(2)["error"]["code"], "usage", "{}", r.context());
    let envelope = run(&home, &["undo"]).expect(0);
    assert_eq!(envelope["data"]["changed_since"], 2);
    assert_eq!(tree_hash(&wtf), replaced, "the WTF/ the restore replaced");
    run(&home, &["undo"]).expect(0);
    assert_eq!(tree_hash(&wtf), played, "the undo of the undo: as played");
    assert_ne!(played, restored);
}
