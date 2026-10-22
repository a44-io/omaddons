//! `omaddons backup restore` end to end (M3-D24, M3-D26, M3-D29, M3-D47, M3-D48): a whole
//! restore puts the backup's `WTF/` back exactly, modes included, and `undo` returns each
//! state; every refusal (a damaged backup, a copy that fails its hashes, a backup on
//! another client, a running client, a filesystem that cannot exchange, a newer state)
//! comes before anything under the client dir changes; the current `WTF/` is kept as a
//! `before_restore` backup where the filesystem clones, and that backup keeps its history
//! line when the restore fails after it; retention after the restore never prunes its
//! source; a concurrent prune finds the source busy. Every run is checked for the fake
//! account, realm and character names, on both streams.

mod common;

use std::fs;
use std::path::PathBuf;

use common::backups::{backup_dir, seed_backup};
use common::home::{Home, Run};
use common::privacy::assert_private_all;
use common::trees::tree_hash;
use common::wtf::Wtf;
use omaddons::backup::BackupKind;
use serde_json::{Value, json};
use support::{CLONES, backups, cloning_home, fixture, history, leftovers, made, run, wtf_of};

#[cfg(test)]
mod support {
    use super::*;

    /// `omaddons <args> --json --progress json`, checked for privacy on both streams.
    pub fn run(home: &Home, args: &[&str], extra: &[(&str, &str)]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let mut env = vec![("OMADDONS_FS_FREE_MB", "4096")];
        env.extend_from_slice(extra);
        let r = home.exec_with(&argv, &env);
        assert_private_all(&r.stdout, &r.stderr);
        r
    }

    /// A retail `WTF/` with `Config.wtf`, two addons' SavedVariables and a read-only
    /// directory; returns it.
    pub fn fixture(home: &Home) -> PathBuf {
        Wtf::at(home.client("_retail_"))
            .config_wtf("SET locale \"enUS\"\n")
            .saved_variables("Foo", 300)
            .saved_variables("Bar", 120)
            .file("Account/FAKEACCT/Sealed/Kept.lua", b"Kept = 1")
            .mode("Account/FAKEACCT/Sealed", 0o500)
            .build()
    }

    /// The id of the backup `backup create` made.
    pub fn made(envelope: &Value) -> String {
        envelope["data"]["targets"][0]["backup"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// `<client>/WTF`.
    pub fn wtf_of(home: &Home) -> PathBuf {
        home.client("_retail_").join("WTF")
    }

    /// Each backup of retail, with its kind.
    pub fn backups(home: &Home) -> Vec<(String, String)> {
        let envelope = run(home, &["backup", "list", "-t", "retail"], &[]).expect(0);
        envelope["data"]["targets"][0]["backups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b["id"].as_str().unwrap().to_owned(),
                    b["kind"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// The history lines, oldest first.
    pub fn history(home: &Home) -> Vec<Value> {
        fs::read_to_string(home.state().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// A home under the target dir (btrfs on the development machine), where
    /// [`CLONES`] holds; `None`, with a `SKIP` line, elsewhere.
    pub fn cloning_home(test: &str) -> Option<(tempfile::TempDir, Home)> {
        let base = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let btrfs = i128::from(rustix::fs::statfs(base.path()).unwrap().f_type) == 0x9123_683E;
        if !btrfs {
            super::common::checkpoint::skip(
                test,
                "the target dir is not on btrfs, so nothing clones",
            );
            return None;
        }
        let home = Home::at(base.path(), &["wow"]);
        Some((base, home))
    }

    /// The clone hint a cloning home's runs take.
    pub const CLONES: (&str, &str) = ("OMADDONS_FS_CLONE", "yes");

    /// What a restore's scratch places hold: `<meta>/stage` and `<state>/inflight`.
    pub fn leftovers(home: &Home) -> Vec<PathBuf> {
        [
            home.client("_retail_").join("Interface/.omaddons/stage"),
            home.state().join("inflight"),
        ]
        .iter()
        .flat_map(|dir| {
            fs::read_dir(dir)
                .map(|l| l.map(|e| e.unwrap().path()).collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .collect()
    }
}

#[test]
fn a_restore_puts_the_whole_backup_back_and_undo_returns_each_state() {
    let home = Home::new(&["wow"]);
    let wtf = fixture(&home);
    let created = run(
        &home,
        &["backup", "create", "-t", "retail", "--verify"],
        &[],
    )
    .expect(0);
    let id = made(&created);
    let backed_up = tree_hash(&wtf);
    // A session since: one file rewritten, one added.
    let sv = wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua");
    fs::write(&sv, "Foo = { changed = true }").unwrap();
    fs::write(
        wtf.join("Account/FAKEACCT/SavedVariables/New.lua"),
        "New = 1",
    )
    .unwrap();
    let played = tree_hash(&wtf);

    let envelope = run(&home, &["backup", "restore", &id, "--dry-run"], &[]).expect(0);
    let data = &envelope["data"];
    assert_eq!(
        (&data["dry_run"], &data["tx"], &data["before_restore"]),
        (&json!(true), &Value::Null, &Value::Null)
    );
    assert_eq!(
        (&data["kind"], &data["mode"], &data["exchange"]),
        (&json!("restore"), &json!("whole"), &json!("unverified"))
    );
    assert_eq!(data["files"]["differed"], 1, "{envelope}");
    assert_eq!(data["files"]["created_since"], 1);
    assert!(omaddons::app::precondition::is_token(
        data["precondition"].as_str().unwrap()
    ));
    assert_eq!(tree_hash(&wtf), played, "a dry run writes nothing");
    let argv: Vec<String> = ["backup", "restore", id.as_str(), "--dry-run"]
        .map(str::to_owned)
        .to_vec();
    let human = home.exec_with(&argv, &[]);
    assert_private_all(&human.stdout, &human.stderr);
    for said in [
        "retail: would restore WTF/ from backup",
        "1 differed, 1 created since",
        "its files will be checked against capture-time hashes",
        "Run it bound to this preview: --if-match pc1:",
    ] {
        assert!(human.stdout.contains(said), "{said}: {}", human.stdout);
    }

    let envelope = run(&home, &["backup", "restore", &id, "-t", "retail"], &[]).expect(0);
    let data = &envelope["data"];
    let tx = data["tx"].as_str().unwrap().to_owned();
    assert_eq!(
        (&data["mode"], &data["exchange"], &data["reload_required"]),
        (&json!("whole"), &json!("verified"), &json!("restart"))
    );
    assert_eq!(
        data["backup"]["integrity"], "verified",
        "its hashes were checked"
    );
    assert_eq!(data["precondition"], Value::Null);
    // tmpfs cannot clone: no before_restore backup, and the warning says so.
    assert_eq!(data["before_restore"], Value::Null);
    let codes: Vec<&str> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"before_restore_skipped"), "{envelope}");
    assert_eq!(
        envelope["commands"][0]["argv"],
        json!(["omaddons", "undo", tx])
    );
    assert_eq!(
        tree_hash(&wtf),
        backed_up,
        "exactly the backup, modes included"
    );
    assert!(leftovers(&home).is_empty(), "{:?}", leftovers(&home));
    let line = history(&home).pop().unwrap();
    assert_eq!(
        (&line["kind"], &line["tx"]),
        (&json!("restore"), &json!(tx))
    );

    run(&home, &["undo"], &[]).expect(0);
    assert_eq!(tree_hash(&wtf), played, "undo puts the played-on WTF/ back");
    run(&home, &["undo"], &[]).expect(0);
    assert_eq!(
        tree_hash(&wtf),
        backed_up,
        "and undoing that restores again"
    );
    common::wtf::writable_again(&wtf);
}

#[test]
fn a_damaged_backup_or_one_whose_copy_fails_its_hashes_changes_nothing() {
    let home = Home::new(&["wow"]);
    let wtf = fixture(&home);
    let id = made(
        &run(
            &home,
            &["backup", "create", "-t", "retail", "--verify"],
            &[],
        )
        .expect(0),
    );
    fs::write(
        wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua"),
        "Foo = 2",
    )
    .unwrap();
    let before = tree_hash(&wtf);
    let dir = home
        .client("_retail_")
        .join("Interface/.omaddons/backups")
        .join(&id);

    // One byte flipped, the size kept: only the hashes see it.
    let copy = dir.join("WTF/Account/FAKEACCT/SavedVariables/Bar.lua");
    let mut bytes = fs::read(&copy).unwrap();
    bytes[0] ^= 1;
    fs::write(&copy, &bytes).unwrap();
    let envelope = run(&home, &["backup", "restore", &id], &[]).expect(1);
    assert_eq!(envelope["error"]["code"], "hash_mismatch", "{envelope}");
    assert_eq!(
        envelope["error"]["retryable"], false,
        "a damaged backup stays damaged"
    );
    assert_eq!(tree_hash(&wtf), before);
    assert!(leftovers(&home).is_empty(), "{:?}", leftovers(&home));

    fs::write(dir.join("backup.json"), "{").unwrap();
    let envelope = run(&home, &["backup", "restore", &id], &[]).expect(1);
    assert_eq!(envelope["error"]["code"], "state_corrupt", "{envelope}");
    assert_eq!(
        envelope["error"]["fix_argv"],
        json!(["omaddons", "backup", "list"])
    );
    assert_eq!(tree_hash(&wtf), before);
    common::wtf::writable_again(&wtf);
}

#[test]
fn a_backup_on_another_client_names_its_own() {
    let home = Home::new(&["wow", "wow_classic"]);
    fixture(&home);
    let id = made(&run(&home, &["backup", "create", "-t", "retail"], &[]).expect(0));
    let envelope = run(&home, &["backup", "restore", &id, "-t", "classic"], &[]).expect(3);
    assert_eq!(envelope["error"]["code"], "not_found");
    assert_eq!(
        envelope["error"]["fix_argv"],
        json!(["omaddons", "backup", "restore", id, "-t", "retail"])
    );
    let envelope = run(&home, &["backup", "restore", "01ZZZZZZ"], &[]).expect(3);
    assert_eq!(envelope["error"]["code"], "not_found");
    let envelope = run(&home, &["backup", "restore", "01ZZZZZ"], &[]).expect(2);
    assert_eq!(envelope["error"]["code"], "usage", "seven characters");
    // The widget's template: the id after `--`.
    let argv: Vec<String> = [
        "backup",
        "restore",
        "-t",
        "retail",
        "--dry-run",
        "--json",
        "--",
    ]
    .iter()
    .map(|a| (*a).to_owned())
    .chain([id.clone()])
    .collect();
    let r = home.exec_with(&argv, &[]);
    assert_eq!(r.expect(0)["data"]["backup"]["id"], json!(id));
    common::wtf::writable_again(&wtf_of(&home));
}

#[test]
fn a_running_client_is_refused_before_anything_is_staged() {
    let mut home = Home::new(&["wow"]);
    let wtf = fixture(&home);
    let id = made(&run(&home, &["backup", "create", "-t", "retail"], &[]).expect(0));
    let before = tree_hash(&wtf);
    home.retail_running();
    for args in [
        vec!["backup", "restore", id.as_str()],
        vec!["backup", "restore", id.as_str(), "--while-running"],
        vec!["backup", "restore", id.as_str(), "--dry-run"],
    ] {
        let envelope = run(&home, &args, &[]).expect(4);
        assert_eq!(envelope["error"]["code"], "game_running", "{args:?}");
        assert_eq!(envelope["error"]["details"]["reason"], "saved_variables");
    }
    assert_eq!(tree_hash(&wtf), before);
    assert!(leftovers(&home).is_empty(), "{:?}", leftovers(&home));
    common::wtf::writable_again(&wtf);
}

#[test]
fn a_filesystem_that_cannot_exchange_is_refused_before_anything_is_copied() {
    let home = Home::new(&["wow"]);
    let wtf = fixture(&home);
    let id = made(&run(&home, &["backup", "create", "-t", "retail"], &[]).expect(0));
    let before = tree_hash(&wtf);
    let envelope = run(
        &home,
        &["backup", "restore", &id],
        &[("OMADDONS_FAILPOINTS", "fsx.renameat2.einval")],
    )
    .expect(4);
    let error = &envelope["error"];
    assert_eq!(
        (
            &error["code"],
            &error["details"]["reason"],
            &error["retryable"]
        ),
        (
            &json!("filesystem_unsupported"),
            &json!("no_rename_exchange"),
            &json!(false)
        )
    );
    assert!(
        error["details"]["backup_dir"]
            .as_str()
            .unwrap()
            .ends_with(&format!("{id}/WTF"))
    );
    assert_eq!(tree_hash(&wtf), before);
    assert!(leftovers(&home).is_empty(), "{:?}", leftovers(&home));
    assert_eq!(backups(&home).len(), 1, "no before_restore backup");
    common::wtf::writable_again(&wtf);
}

#[test]
fn a_newer_record_is_refused_before_any_file_of_the_client_changes() {
    let home = Home::new(&["wow"]);
    let wtf = fixture(&home);
    let id = made(&run(&home, &["backup", "create", "-t", "retail"], &[]).expect(0));
    run(&home, &["backup", "restore", &id], &[]).expect(0);
    let key = common::state::key_of(&home, "retail");
    let record = home.state().join(format!("targets/{key}.json"));
    let mut doc: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    doc["schema_version"] = json!("1.99");
    fs::write(&record, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    fs::write(
        wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua"),
        "Foo = 3",
    )
    .unwrap();
    let before = tree_hash(&wtf);
    let listed = backups(&home);
    let envelope = run(&home, &["backup", "restore", &id], &[]).expect(4);
    assert_eq!(envelope["error"]["code"], "state_too_new", "{envelope}");
    assert_eq!(tree_hash(&wtf), before);
    assert_eq!(backups(&home), listed);
    assert!(leftovers(&home).is_empty(), "{:?}", leftovers(&home));
    common::wtf::writable_again(&wtf);
}

#[test]
fn retention_after_a_restore_never_prunes_its_source() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    fixture(&home);
    // Two automatic backups of an old day: retention keeps only the later one, but the
    // restore's source is the earlier.
    let at = |t: &str| t.parse().unwrap();
    let source = seed_backup(&retail, BackupKind::Auto, at("2026-10-01T08:00:00Z"), 2);
    let later = seed_backup(&retail, BackupKind::Auto, at("2026-10-01T20:00:00Z"), 2);
    let env = [("OMADDONS_NOW", "2026-10-10T12:00:00Z")];
    let id = source.to_string();
    let human = home.exec_with(&["backup", "restore", id.as_str()].map(str::to_owned), &env);
    assert_private_all(&human.stdout, &human.stderr);
    assert_eq!(human.code, 0, "{}", human.context());
    for said in [
        "retail: restored WTF/ from backup",
        "auto (after play)): 2 files",
        "metadata checked; content not verified",
        "· undo: omaddons undo ",
    ] {
        assert!(human.stdout.contains(said), "{said}: {}", human.stdout);
    }
    let listed: Vec<String> = backups(&home).into_iter().map(|(id, _)| id).collect();
    assert!(listed.contains(&id), "the source is pinned: {listed:?}");
    assert!(listed.contains(&later.to_string()));
    // Without the restore, the next pass prunes it.
    let envelope = run(&home, &["backup", "prune", "-t", "retail"], &env).expect(0);
    assert_eq!(envelope["data"]["targets"][0]["pruned"][0]["id"], json!(id));
    common::wtf::writable_again(&wtf_of(&home));
}

#[test]
fn a_concurrent_prune_finds_the_source_busy() {
    use common::pause::Pause;
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    fixture(&home);
    let at = |t: &str| t.parse().unwrap();
    let source = seed_backup(&retail, BackupKind::Manual, at("2026-10-01T08:00:00Z"), 2);
    // A newer checkpoint, so naming the source is a prune that may go ahead.
    seed_backup(&retail, BackupKind::Manual, at("2026-10-02T08:00:00Z"), 2);
    let id = source.to_string();
    let pause = Pause::new(home.env.root());
    let args = ["backup", "restore", id.as_str()];
    let child = home.spawn(
        &args,
        &[
            ("OMADDONS_FAILPOINTS", "engine.restore.staged=pause"),
            pause.env(),
        ],
    );
    pause.reached("engine.restore.staged");
    let envelope = run(&home, &["backup", "prune", "-t", "retail", &id], &[]).expect(0);
    assert_eq!(
        envelope["data"]["targets"][0]["busy"],
        json!([{"id": id}]),
        "{envelope}"
    );
    pause.resume("engine.restore.staged");
    common::home::wait(child, &args).expect(0);
    assert!(backup_dir(&retail, &source).is_dir());
    common::wtf::writable_again(&wtf_of(&home));
}

#[test]
fn the_current_wtf_is_kept_as_a_backup_where_the_filesystem_clones() {
    let Some((_base, home)) =
        cloning_home("the_current_wtf_is_kept_as_a_backup_where_the_filesystem_clones")
    else {
        return;
    };
    let wtf = fixture(&home);
    let id = made(&run(&home, &["backup", "create", "-t", "retail"], &[CLONES]).expect(0));
    fs::write(
        wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua"),
        "Foo = 4",
    )
    .unwrap();
    let played = tree_hash(&wtf);
    let dry = run(&home, &["backup", "restore", &id, "--dry-run"], &[CLONES]).expect(0);
    assert_eq!(dry["data"]["before_restore"], Value::Null);
    let envelope = run(&home, &["backup", "restore", &id], &[CLONES]).expect(0);
    let kept = envelope["data"]["before_restore"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        backups(&home).contains(&(kept.clone(), "before_restore".to_owned())),
        "{envelope}"
    );
    // Its history line, then the restore's.
    let lines = history(&home);
    let n = lines.len();
    assert_eq!(lines[n - 2]["backup"]["id"], json!(kept));
    assert_eq!(lines[n - 1]["kind"], "restore");
    // It holds the played-on WTF/: restoring it puts that back, modes included.
    run(&home, &["backup", "restore", &kept], &[CLONES]).expect(0);
    assert_eq!(tree_hash(&wtf), played);
    common::wtf::writable_again(&wtf);
}

#[test]
fn a_refusal_at_the_preflight_makes_no_backup_and_a_failure_after_one_keeps_its_line() {
    let Some((_base, home)) = cloning_home(
        "a_refusal_at_the_preflight_makes_no_backup_and_a_failure_after_one_keeps_its_line",
    ) else {
        return;
    };
    let wtf = fixture(&home);
    let id = made(&run(&home, &["backup", "create", "-t", "retail"], &[CLONES]).expect(0));
    let before = tree_hash(&wtf);
    run(
        &home,
        &["backup", "restore", &id],
        &[CLONES, ("OMADDONS_FAILPOINTS", "fsx.renameat2.einval")],
    )
    .expect(4);
    assert_eq!(backups(&home).len(), 1, "no before_restore backup");
    assert!(leftovers(&home).is_empty(), "{:?}", leftovers(&home));

    run(
        &home,
        &["backup", "restore", &id],
        &[
            CLONES,
            ("OMADDONS_FAILPOINTS", "engine.restore.staged=error"),
        ],
    )
    .expect(1);
    assert_eq!(tree_hash(&wtf), before, "nothing restored");
    let kept: Vec<String> = backups(&home)
        .into_iter()
        .filter(|(_, kind)| kind == "before_restore")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(kept.len(), 1);
    let line = history(&home).pop().unwrap();
    assert_eq!(line["backup"]["id"], json!(kept[0]), "listed with its line");
    assert!(leftovers(&home).is_empty(), "{:?}", leftovers(&home));
    common::wtf::writable_again(&wtf);
}
