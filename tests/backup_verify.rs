//! `omaddons backup verify` end to end (M3-D47, M3-D48): a byte flipped in a backup that
//! recorded hashes makes it `damaged` (the run `partial`, exit 0), which `backup list`
//! shows and `backup restore` refuses; a backup without hashes gets a `later` baseline,
//! said to prove nothing of the capture, that then catches a change; an imported backup
//! keeps its `import` baseline; `--dry-run` records nothing and establishes nothing; a
//! prune meanwhile finds the backup busy; a crash between the manifest and `backup.json`
//! leaves hashes the next verify compares against; no history line and no global lock;
//! the doorbell rings only for a recorded result. Every run is checked for the fake
//! account, realm and character names on both streams.

mod common;

use std::fs;
use std::path::PathBuf;

use common::home::{Home, Run};
use common::privacy::assert_private_all;
use common::wtf::Wtf;
use serde_json::{Value, json};
use support::{backup_dir, create, described, flip, hashed, item, listed, run, verify};

#[cfg(test)]
mod support {
    use super::*;

    /// `omaddons <args> --json --progress json` with `extra` set, checked for privacy.
    pub fn run(home: &Home, args: &[&str], extra: &[(&str, &str)]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let r = home.exec_with(&argv, extra);
        assert_private_all(&r.stdout, &r.stderr);
        r
    }

    /// A retail `WTF/` with two addons' settings and `Config.wtf`, and a backup of it
    /// (`--verify` when `hashed`); returns the backup's id.
    pub fn create(home: &Home, hashed: bool) -> String {
        let foo = "WTF/Account/FAKEACCT/SavedVariables/Foo.lua";
        if !home.client("_retail_").join(foo).is_file() {
            Wtf::at(home.client("_retail_"))
                .config_wtf("SET locale \"enUS\"\n")
                .saved_variables("Foo", 300)
                .saved_variables("Bar", 120)
                .build();
        }
        let mut args = vec!["backup", "create", "-t", "retail"];
        if hashed {
            args.push("--verify");
        }
        let created = run(home, &args, &[]).expect(0);
        created["data"]["targets"][0]["backup"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// `<client>/Interface/.omaddons/backups/<id>`.
    pub fn backup_dir(home: &Home, id: &str) -> PathBuf {
        home.client("_retail_")
            .join("Interface/.omaddons/backups")
            .join(id)
    }

    /// Flips one byte of Foo's account file in backup `id`, keeping its size and time.
    pub fn flip(home: &Home, id: &str) {
        let path = backup_dir(home, id).join("WTF/Account/FAKEACCT/SavedVariables/Foo.lua");
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 0x20;
        fs::write(&path, bytes).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    /// `backup verify <ids…> -t retail` with `more` arguments.
    pub fn verify(home: &Home, ids: &[&str], more: &[&str]) -> Run {
        let mut args = vec!["backup", "verify"];
        args.extend_from_slice(ids);
        args.extend(["-t", "retail"]);
        args.extend_from_slice(more);
        run(home, &args, &[])
    }

    /// The first backup of the first target in a verify envelope.
    pub fn item(envelope: &Value) -> Value {
        envelope["data"]["targets"][0]["backups"][0].clone()
    }

    /// `backup list`'s entry for `id`.
    pub fn listed(home: &Home, id: &str) -> Value {
        let envelope = run(home, &["backup", "list", "-t", "retail"], &[]).expect(0);
        envelope["data"]["targets"][0]["backups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["id"] == id)
            .unwrap()
            .clone()
    }

    /// The bytes of `id`'s `backup.json` and `manifest.json`.
    pub fn described(home: &Home, id: &str) -> (Vec<u8>, Vec<u8>) {
        let dir = backup_dir(home, id);
        (
            fs::read(dir.join("backup.json")).unwrap(),
            fs::read(dir.join("manifest.json")).unwrap(),
        )
    }

    /// Whether every file in `id`'s manifest has a hash.
    pub fn hashed(home: &Home, id: &str) -> bool {
        let manifest: Value =
            serde_json::from_slice(&fs::read(backup_dir(home, id).join("manifest.json")).unwrap())
                .unwrap();
        manifest["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "file")
            .all(|e| e["sha256"].is_string())
    }
}

#[test]
fn a_flipped_byte_in_a_verified_backup_is_damage_and_restore_refuses_it() {
    let home = Home::new(&["wow"]);
    let id = create(&home, true);
    let ok = verify(&home, &[&id], &[]).expect(0);
    assert_eq!(
        (
            &item(&ok)["integrity"],
            &item(&ok)["baseline"],
            &ok["partial"]
        ),
        (&json!("verified"), &json!("capture"), &json!(false)),
        "{ok:#}"
    );
    flip(&home, &id);
    let r = verify(&home, &[&id], &[]);
    let envelope = r.expect(0);
    let b = item(&envelope);
    assert_eq!(
        (
            &envelope["partial"],
            &b["integrity"],
            &b["baseline"],
            &b["mismatched"],
            &b["checked"]
        ),
        (
            &json!(true),
            &json!("damaged"),
            &Value::Null,
            &json!(1),
            &b["files"]
        ),
        "{envelope:#}"
    );
    let note = b["note"].as_str().unwrap();
    assert!(
        note.starts_with(
            "1 files differ from their recorded hashes; the first: Account/account 1/"
        ),
        "{note}"
    );
    let entry = listed(&home, &id);
    assert_eq!(
        (&entry["integrity"], &entry["restorable"]),
        (&json!("damaged"), &json!(false))
    );
    let human = home.exec(&["backup".to_owned(), "list".to_owned()]);
    assert_private_all(&human.stdout, &human.stderr);
    assert!(human.stdout.contains("damaged"), "{}", human.stdout);
    let refused = run(&home, &["backup", "restore", &id, "-t", "retail"], &[]);
    assert_eq!(
        refused.envelope()["error"]["code"],
        "state_corrupt",
        "{}",
        refused.context()
    );
}

/// A backup whose `WTF/` is gone is damaged, every file missing: recorded as such and
/// never restorable, not an I/O error to retry.
#[test]
fn a_backup_without_its_wtf_is_damage() {
    let home = Home::new(&["wow"]);
    let id = create(&home, true);
    fs::remove_dir_all(backup_dir(&home, &id).join("WTF")).unwrap();
    let envelope = verify(&home, &[&id], &[]).expect(0);
    let b = item(&envelope);
    assert_eq!(
        (&envelope["partial"], &b["integrity"]),
        (&json!(true), &json!("damaged")),
        "{envelope:#}"
    );
    let entry = listed(&home, &id);
    assert_eq!(
        (&entry["integrity"], &entry["restorable"]),
        (&json!("damaged"), &json!(false))
    );
}

#[test]
fn a_backup_without_hashes_gets_a_later_baseline_that_then_catches_a_change() {
    let home = Home::new(&["wow"]);
    let id = create(&home, false);
    assert!(!hashed(&home, &id));
    let first = verify(&home, &[&id], &[]).expect(0);
    let b = item(&first);
    assert_eq!(
        (
            &b["integrity"],
            &b["baseline"],
            &b["note"],
            &first["partial"]
        ),
        (
            &json!("verified"),
            &json!("later"),
            &json!(
                "no hashes existed; recorded a baseline (this cannot prove the capture was intact)"
            ),
            &json!(false)
        ),
        "{first:#}"
    );
    assert!(hashed(&home, &id), "the baseline is in the manifest");
    let entry = listed(&home, &id);
    assert_eq!(
        (&entry["integrity"], &entry["baseline"]),
        (&json!("verified"), &json!("later"))
    );
    assert!(entry["verified_at"].is_string());
    let human = home.exec(&["backup".to_owned(), "list".to_owned()]);
    assert!(
        human.stdout.contains("verified (later)"),
        "{}",
        human.stdout
    );

    // Again: the hashes it recorded, matched; then a change caught.
    let again = verify(&home, &[&id], &[]).expect(0);
    assert_eq!(item(&again)["baseline"], "later");
    assert!(
        item(&again)["note"]
            .as_str()
            .unwrap()
            .contains("cannot prove the capture was intact")
    );
    flip(&home, &id);
    let caught = verify(&home, &[&id], &[]).expect(0);
    assert_eq!(item(&caught)["integrity"], "damaged", "{caught:#}");
}

#[test]
fn an_imported_backup_verifies_against_its_import_hashes() {
    let home = Home::new(&["wow"]);
    let id = create(&home, false);
    let usb = home.env.root().join("usb");
    fs::create_dir_all(&usb).unwrap();
    let file = usb.join("retail.tar.zst").display().to_string();
    run(&home, &["backup", "export", &id, &file], &[]).expect(0);
    let imported = run(&home, &["backup", "import", &file, "-t", "retail"], &[]).expect(0);
    let imported = imported["data"]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = verify(&home, &[&imported], &[]).expect(0);
    assert_eq!(
        (
            &item(&done)["integrity"],
            &item(&done)["baseline"],
            &item(&done)["note"]
        ),
        (
            &json!("verified"),
            &json!("import"),
            &json!("every file matched the hashes checked when it was imported")
        ),
        "{done:#}"
    );
    assert_eq!(listed(&home, &imported)["baseline"], "import");
}

#[test]
fn a_dry_run_records_and_establishes_nothing_and_rings_no_doorbell() {
    let home = Home::new(&["wow"]);
    let id = create(&home, false);
    let bell = home.env.cache().join("omaddons/doorbell.json");
    fs::remove_file(&bell).unwrap();
    let history = fs::read_to_string(home.state().join("history.jsonl")).unwrap();
    let before = described(&home, &id);
    let dry = verify(&home, &[&id], &["--dry-run"]).expect(0);
    let b = item(&dry);
    assert_eq!(
        (
            &dry["data"]["dry_run"],
            &b["integrity"],
            &b["baseline"],
            &b["checked"]
        ),
        (
            &json!(true),
            &json!("metadata_only"),
            &Value::Null,
            &b["files"]
        ),
        "{dry:#}"
    );
    assert!(
        b["note"]
            .as_str()
            .unwrap()
            .contains("no baseline is established"),
        "{dry:#}"
    );
    assert_eq!(described(&home, &id), before, "nothing recorded");
    assert!(!bell.exists(), "a dry run rings nothing");

    verify(&home, &[&id], &[]).expect(0);
    let rung: Value = serde_json::from_slice(&fs::read(&bell).unwrap()).unwrap();
    assert_eq!(rung["command"], "backup verify");
    assert_eq!(
        fs::read_to_string(home.state().join("history.jsonl")).unwrap(),
        history,
        "a check appends no history line"
    );
}

#[test]
fn a_prune_meanwhile_finds_the_backup_busy_and_a_global_lock_never_stops_it() {
    use common::pause::Pause;

    let home = Home::new(&["wow"]);
    let id = create(&home, false);
    // A newer checkpoint, so the first is prunable by name.
    create(&home, false);
    let pause = Pause::new(home.env.root());
    let child = home.spawn(
        &["backup", "verify", &id, "-t", "retail"],
        &[
            ("OMADDONS_FAILPOINTS", "fsx.write_atomic.before=pause"),
            pause.env(),
        ],
    );
    pause.reached("fsx.write_atomic.before");
    let prune = run(&home, &["backup", "prune", &id, "-t", "retail"], &[]).expect(0);
    assert_eq!(
        prune["data"]["targets"][0]["busy"],
        json!([{"id": id}]),
        "{prune:#}"
    );
    pause.resume("fsx.write_atomic.before");
    let output = child.wait_with_output().unwrap();
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(item(&envelope)["integrity"], "verified", "{envelope:#}");
    assert!(backup_dir(&home, &id).is_dir());

    // Another command holding the global lock: verify takes none.
    let paths = omaddons::state::paths::Paths::with_roots(
        home.env.home(),
        home.env.config(),
        home.env.state(),
        home.env.cache(),
    );
    let lock = omaddons::fsx::GlobalLock::acquire(&paths, "test").unwrap();
    let again = verify(&home, &[&id], &[]).expect(0);
    assert_eq!(item(&again)["integrity"], "verified");
    drop(lock);
}

/// A crash between the manifest write and `backup.json`'s: the hashes are recorded, the
/// integrity not yet; the next verify compares against them (a `later` baseline), never
/// calling the backup damaged.
#[test]
fn a_crash_between_the_two_writes_leaves_hashes_the_next_verify_compares_against() {
    let mut found = false;
    for n in 1..=6 {
        let home = Home::new(&["wow"]);
        let id = create(&home, false);
        let spec = format!("fsx.write_atomic.before=abort@{n}");
        let argv = ["backup", "verify", &id, "-t", "retail", "--json"].map(str::to_owned);
        let r = home.exec_with(&argv, &[("OMADDONS_FAILPOINTS", &spec)]);
        if r.code != -1 {
            break;
        }
        let meta: Value =
            serde_json::from_slice(&fs::read(backup_dir(&home, &id).join("backup.json")).unwrap())
                .unwrap();
        if !(hashed(&home, &id) && meta["integrity"] == "metadata_only") {
            continue;
        }
        found = true;
        let next = verify(&home, &[&id], &[]).expect(0);
        assert_eq!(
            (&item(&next)["integrity"], &item(&next)["baseline"]),
            (&json!("verified"), &json!("later")),
            "{next:#}"
        );
    }
    assert!(found, "a crash at the backup.json write was reached");
}

#[test]
fn an_unknown_id_refuses_the_run_before_anything_is_hashed() {
    let home = Home::new(&["wow"]);
    let id = create(&home, false);
    let before = described(&home, &id);
    let r = verify(&home, &[&id, "01ZZZZZZZZ"], &[]);
    assert_eq!(
        r.envelope()["error"]["code"],
        "not_found",
        "{}",
        r.context()
    );
    assert_eq!(described(&home, &id), before);
}

/// Every place a backup's result shows is private: a mismatch's path, the human lines.
#[test]
fn nothing_names_an_account_realm_or_character() {
    let home = Home::new(&["wow"]);
    let id = create(&home, true);
    flip(&home, &id);
    for args in [
        vec!["backup", "verify", id.as_str()],
        vec!["backup", "verify", id.as_str(), "--dry-run"],
    ] {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let human = home.exec(&argv);
        assert_private_all(&human.stdout, &human.stderr);
        assert!(human.stdout.contains("damaged"), "{}", human.stdout);
    }
}
