//! `omaddons backup create` end to end: a fixture `WTF/` of two accounts and three
//! characters copied exactly (contents, symlinks, an empty and a read-only directory),
//! described by a manifest that is a stat walk of the live tree, owner-only at rest, in
//! history, in the inventory projection and on the doorbell; reflinks where the
//! filesystem clones; capture consistency over two attempts (failpoint pauses change the
//! tree mid-copy); `--verify`; every live client at once, under one run; `--dry-run`
//! writing nothing; the free-space warning; undo stepping over a backup; and the `backup
//! list` hint that offers one. Every run is checked for the fake account, realm and
//! character names, on both streams, progress included.

use crate::common;

use std::fs;
use std::path::Path;

use common::home::{Home, wait};
use common::pause::Pause;
use common::privacy::assert_private_all;
use common::wtf::{Wtf, writable_again};
use omaddons::backup::tree::walk;
use omaddons::backup::{EntryKind, Manifest};
use omaddons::state::store;
use serde_json::{Value, json};
use support::{
    backup_dir, codes, fixture_wtf, history, leftovers, local_addon, mode, one_count, progress,
    run, tree_contents, tree_state,
};

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};

    use omaddons::backup::EntryKind;
    use omaddons::backup::tree::walk;
    use omaddons::model::BackupId;
    use serde_json::Value;

    use super::common::addons::{addon_folder, toc};
    use super::common::home::{Home, Run};
    use super::common::privacy::assert_private_all;
    use super::common::wtf::Wtf;

    /// `omaddons <args> --json --progress json` with `extra` set, private on both
    /// streams; the free space is pinned at 1024 MiB unless `extra` sets it.
    pub fn run(home: &Home, args: &[&str], extra: &[(&str, &str)]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let mut env = vec![("OMADDONS_FS_FREE_MB", "1024")];
        env.extend_from_slice(extra);
        let r = home.exec_with(&argv, &env);
        assert_private_all(&r.stdout, &r.stderr);
        r
    }

    /// Two accounts and three characters: 49 SavedVariables and `Config.wtf`, plus a
    /// symlink, an empty directory and a `0500` one holding one more file (50 in all).
    pub fn fixture_wtf(client: &Path) -> PathBuf {
        let mut wtf = Wtf::at(client).config_wtf("SET locale \"enUS\"\n");
        for i in 0..12 {
            wtf = wtf.saved_variables(&format!("Main{i}"), 200 + i);
        }
        wtf = wtf.character("Second");
        for i in 0..6 {
            wtf = wtf.saved_variables(&format!("Alt{i}"), 120);
        }
        wtf = wtf.account("OTHERACCT").realm("Other").character("Third");
        for i in 0..6 {
            wtf = wtf.saved_variables(&format!("Other{i}"), 80);
        }
        wtf.symlink("Config.link", "Config.wtf")
            .mode("empty", 0o755)
            .file("readonly/kept.txt", b"kept\n")
            .mode("readonly", 0o500)
            .build()
    }

    /// `<root>/src/<name>/`: one folder that loads on retail, version 1.0.0; returns
    /// its path for `install`.
    pub fn local_addon(home: &Home, name: &str) -> String {
        let src = home.env.root().join("src");
        let toc_name = format!("{name}.toc");
        let toc_text = toc("120105", &["## Version: 1.0.0"]);
        addon_folder(&src, name, &[(&toc_name, &toc_text)], &[])
            .display()
            .to_string()
    }

    /// `<client>/Interface/.omaddons/backups/<id>`.
    pub fn backup_dir(client: &Path, id: &str) -> PathBuf {
        let id: BackupId = id.parse().unwrap();
        client
            .join("Interface/.omaddons/backups")
            .join(id.to_string())
    }

    /// Every entry's path, kind, symlink target and file contents (not modes).
    pub fn tree_contents(dir: &Path) -> Vec<(String, EntryKind, Option<String>, Vec<u8>)> {
        walk(dir, usize::MAX, usize::MAX)
            .unwrap()
            .entries
            .into_iter()
            .map(|e| {
                let bytes = match e.kind {
                    EntryKind::File => fs::read(dir.join(&e.path)).unwrap(),
                    _ => Vec::new(),
                };
                (e.path, e.kind, e.link, bytes)
            })
            .collect()
    }

    /// Every entry below `dir` with its mode, size, mtime and contents: unchanged means
    /// nothing was written there.
    pub fn tree_state(dir: &Path) -> BTreeMap<PathBuf, (u32, u64, i64, Vec<u8>)> {
        let mut state = BTreeMap::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(at) = pending.pop() {
            let Ok(listing) = fs::read_dir(&at) else {
                continue;
            };
            for entry in listing {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                let bytes = if meta.is_file() {
                    fs::read(&path).unwrap()
                } else {
                    Vec::new()
                };
                if meta.is_dir() {
                    pending.push(path.clone());
                }
                state.insert(
                    path,
                    (
                        meta.mode(),
                        meta.len(),
                        meta.mtime_nsec() + meta.mtime() * 1_000_000_000,
                        bytes,
                    ),
                );
            }
        }
        state
    }

    pub fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    /// `WTF/` with one character's SavedVariables, for scenes about selection.
    pub fn small_wtf(client: &Path) -> PathBuf {
        Wtf::at(client).saved_variables("Foo", 64).build()
    }

    /// No `WTF/` at all, as before the client's first launch (the synthetic Battle.net
    /// tree gives every client an empty one).
    pub fn never_launched(client: &Path) {
        fs::remove_dir_all(client.join("WTF")).unwrap();
    }

    /// The JSON lines of `history.jsonl`.
    pub fn history(home: &Home) -> Vec<Value> {
        fs::read_to_string(home.state().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// The entries of a capture's scratch places: `<meta>/stage` and `<state>/inflight`.
    pub fn leftovers(home: &Home, client: &Path) -> Vec<String> {
        [
            client.join("Interface/.omaddons/stage"),
            home.state().join("inflight"),
        ]
        .iter()
        .flat_map(|dir| {
            fs::read_dir(dir)
                .map(|l| {
                    l.map(|e| e.unwrap().path().display().to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect()
    }

    /// The warning codes of an envelope, in order.
    pub fn codes(envelope: &Value) -> Vec<&str> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap())
            .collect()
    }

    /// `(message, current, total)` of each stderr progress event of `phase`, in order.
    pub fn progress(r: &Run, phase: &str) -> Vec<(String, u64, u64)> {
        r.stderr
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| e["event"] == "progress" && e["phase"] == phase)
            .map(|e| {
                (
                    e["message"].as_str().unwrap().to_owned(),
                    e["current"].as_u64().unwrap(),
                    e["total"].as_u64().unwrap(),
                )
            })
            .collect()
    }

    /// Asserts `events` are one count of `total` under `label`: `current` never steps
    /// back, never passes `total`, and reaches it.
    pub fn one_count(events: &[(String, u64, u64)], label: &str, total: u64, r: &Run) {
        assert!(!events.is_empty(), "no events\n{}", r.context());
        let mut last = 0;
        for (message, current, of) in events {
            assert_eq!((message.as_str(), *of), (label, total), "{}", r.context());
            assert!(
                *current >= last && *current <= total,
                "{events:?}\n{}",
                r.context()
            );
            last = *current;
        }
        assert_eq!(last, total, "{events:?}\n{}", r.context());
    }
}

#[test]
fn a_backup_is_the_live_wtf_and_says_what_it_is() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let wtf = fixture_wtf(&retail);
    let foo = local_addon(&home, "Foo");
    home.run(&["install", &foo, "-t", "retail"]).expect(0);
    let bell = home.env.cache().join("omaddons/doorbell.json");
    fs::remove_file(&bell).unwrap();

    let r = run(
        &home,
        &[
            "backup",
            "create",
            "-t",
            "retail",
            "--reason",
            "before the raid",
        ],
        &[],
    );
    let envelope = r.expect(0);
    assert_eq!(envelope["command"], "backup create");
    assert_eq!(envelope["partial"], false);
    let data = &envelope["data"];
    assert_eq!(data["dry_run"], false);
    let t = &data["targets"][0];
    assert_eq!(data["targets"].as_array().unwrap().len(), 1);
    assert_eq!(t["target"]["id"], "retail");
    assert!(t["target"]["key"].is_string());
    assert_eq!((&t["skipped"], &t["error"]), (&Value::Null, &Value::Null));
    assert!(t.get("would").is_none(), "only for a dry run");
    let b = &t["backup"];
    assert_eq!(
        (
            &b["kind"],
            &b["trigger"],
            &b["actor"],
            &b["reason"],
            &b["files"],
            &b["consistency"],
            &b["integrity"],
            &b["unsettled"],
            &b["client_running"],
        ),
        (
            &json!("manual"),
            &Value::Null,
            &json!("agent"),
            &json!("before the raid"),
            &json!(50),
            &json!("stable"),
            &json!("metadata_only"),
            &json!(0),
            &json!(false),
        ),
        "{envelope}"
    );
    assert_eq!(
        (&b["restorable"], &b["qualifying"], &b["busy"]),
        (&json!(true), &json!(true), &json!(false))
    );
    assert_eq!(
        (&b["verified_at"], &b["baseline"]),
        (&Value::Null, &Value::Null)
    );
    let id = b["id"].as_str().unwrap();
    let dir = backup_dir(&retail, id);

    // The copy: the live tree's contents and symlink targets, its manifest a stat walk
    // of the live tree (exact modes included), stored directories owner-writable.
    assert_eq!(tree_contents(&dir.join("WTF")), tree_contents(&wtf));
    let manifest: Manifest = store::load(&dir.join("manifest.json"))
        .unwrap()
        .unwrap()
        .value;
    let fresh = walk(&wtf, usize::MAX, usize::MAX).unwrap().entries;
    assert_eq!(manifest.entries, fresh);
    let readonly = fresh.iter().find(|e| e.path == "readonly").unwrap();
    assert_eq!(readonly.mode, 0o500, "the manifest keeps the exact mode");
    assert_eq!(mode(&dir.join("WTF/readonly")) & 0o700, 0o700);
    assert!(dir.join("WTF/empty").is_dir());
    assert_eq!(
        fs::read_link(dir.join("WTF/Config.link")).unwrap(),
        Path::new("Config.wtf")
    );
    for owner_only in [retail.join("Interface/.omaddons/backups"), dir.clone()] {
        assert_eq!(mode(&owner_only), 0o700, "{}", owner_only.display());
    }

    // What it says about itself.
    let meta: Value = serde_json::from_slice(&fs::read(dir.join("backup.json")).unwrap()).unwrap();
    assert_eq!(
        (&meta["files"], &meta["symlinks"], &meta["dirs"]),
        (&json!(50), &json!(1), &json!(15))
    );
    assert_eq!(meta["target"], t["target"]["key"]);
    assert_eq!(meta["client_version"], "12.1.5.70391");
    assert_eq!(meta["wtf_outside_client"], false);
    let addons: Value =
        serde_json::from_slice(&fs::read(dir.join("addons.json")).unwrap()).unwrap();
    let recorded = home.run(&["list", "-t", "retail"]).expect(0);
    let record = &recorded["data"]["targets"][0]["addons"][0];
    let source = &addons["addons"][0]["source"];
    assert_eq!(addons["addons"].as_array().unwrap().len(), 1, "{addons}");
    assert_eq!(
        (
            &addons["addons"][0]["name"],
            &addons["addons"][0]["folders"],
            &addons["addons"][0]["version"],
            &source["kind"],
            &source["ref"],
        ),
        (
            &json!("Foo"),
            &json!(["Foo"]),
            &json!("1.0.0"),
            &json!("local"),
            &record["ref"],
        ),
        "{record}"
    );
    let r = record["ref"].as_str().unwrap();
    assert!(r.ends_with("/src/Foo"), "{foo}: {record}");
    assert!(
        source["release_id"].as_str().unwrap().starts_with(r),
        "{addons}"
    );

    // History, the doorbell and the projection.
    let lines = history(&home);
    let line = lines.last().unwrap();
    assert_eq!(
        (&line["kind"], &line["ops"], &line["tx"], &line["reason"]),
        (
            &json!("backup"),
            &json!([]),
            &t["tx"],
            &json!("before the raid")
        )
    );
    assert_eq!(line["run"], data["run"]);
    assert_eq!(
        line["backup"],
        json!({"id": id, "kind": "manual", "files": 50, "bytes": b["bytes"]})
    );
    let rung: Value = serde_json::from_slice(&fs::read(&bell).unwrap()).unwrap();
    assert_eq!(
        (&rung["command"], &rung["actor"]),
        (&json!("backup create"), &json!("agent"))
    );
    let projection: Value =
        serde_json::from_slice(&fs::read(home.env.cache().join("omaddons/backups.json")).unwrap())
            .unwrap();
    let key = t["target"]["key"].as_str().unwrap();
    assert_eq!(projection["targets"][key]["covers"]["backups"], json!([id]));
    assert_eq!(projection["targets"][key]["latest_qualifying"]["id"], id);
    assert!(
        leftovers(&home, &retail).is_empty(),
        "{:?}",
        leftovers(&home, &retail)
    );

    // `backup list` shows it, as `backup create` described it.
    let listed = run(&home, &["backup", "list", "-t", "retail"], &[]).expect(0);
    assert_eq!(listed["data"]["targets"][0]["backups"][0], *b);
    assert_eq!(listed["commands"], json!([]), "a fresh qualifying backup");

    // `history` names it.
    let shown = home.exec(&["history".to_owned()]);
    let short = format!("{}…", &id[..16]);
    assert!(
        shown
            .stdout
            .contains(&format!("WTF/ as {short} (50 files)")),
        "{}",
        shown.stdout
    );
    let listed = home.run(&["history", "--limit", "1"]).expect(0);
    assert_eq!(listed["data"]["entries"][0]["backup"], line["backup"]);
    writable_again(&wtf);
}

#[test]
fn copies_reflink_where_the_filesystem_clones_and_byte_copy_elsewhere() {
    // Under the target dir: btrfs on the development machine, unlike /tmp.
    let base = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let home = Home::at(base.path(), &["wow"]);
    let retail = home.client("_retail_");
    Wtf::at(&retail).saved_variables("Foo", 64 * 1024).build();
    let btrfs = i128::from(rustix::fs::statfs(base.path()).unwrap().f_type) == 0x9123_683E;

    let envelope = run(&home, &["backup", "create", "-t", "retail"], &[]).expect(0);
    let reflinked = &envelope["data"]["targets"][0]["backup"]["reflinked"];
    if btrfs {
        assert_eq!(reflinked, "all", "{envelope}");
        assert!(!codes(&envelope).contains(&"backup_copied"), "{envelope}");
    }

    let envelope = run(
        &home,
        &["backup", "create", "-t", "retail"],
        &[("OMADDONS_FAILPOINTS", "fsx.reflink.unsupported")],
    )
    .expect(0);
    let b = &envelope["data"]["targets"][0]["backup"];
    assert_eq!(b["reflinked"], "none", "{envelope}");
    let copied: Vec<&Value> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "backup_copied")
        .collect();
    assert_eq!(copied.len(), 1, "{envelope}");
    let message = copied[0]["message"].as_str().unwrap();
    assert!(
        message.starts_with("retail: ") && message.contains(&format!("{} bytes", b["bytes"])),
        "{message}"
    );
    assert_eq!(copied[0]["fix"], Value::Null);
    let human = home.exec(&[
        "backup".into(),
        "create".into(),
        "-t".into(),
        "retail".into(),
    ]);
    assert_eq!(human.code, 0, "{}", human.context());
    assert!(
        human.stdout.starts_with("retail: backed up WTF/ as ")
            && human.stdout.trim_end().ends_with(", stable)"),
        "{}",
        human.stdout
    );
}

#[test]
fn a_file_rewritten_during_its_copy_is_copied_again() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let wtf = Wtf::at(&retail)
        .saved_variables("Aaa", 100)
        .saved_variables("Bbb", 100)
        .build();
    let first = walk(&wtf, usize::MAX, usize::MAX)
        .unwrap()
        .entries
        .into_iter()
        .find(|e| e.kind == EntryKind::File)
        .unwrap()
        .path;
    let pause = Pause::new(home.env.root());
    let argv = ["backup", "create", "-t", "retail", "--progress", "json"];
    let child = home.spawn(
        &argv,
        &[
            ("OMADDONS_FAILPOINTS", "backup.capture.copied.1=pause"),
            pause.env(),
        ],
    );
    pause.reached("backup.capture.copied.1");
    fs::write(wtf.join(&first), "rewritten while it was copied\n").unwrap();
    pause.resume("backup.capture.copied.1");
    let r = wait(child, &argv);
    assert_private_all(&r.stdout, &r.stderr);
    let envelope = r.expect(0);
    let b = &envelope["data"]["targets"][0]["backup"];
    assert_eq!(
        (&b["consistency"], &b["unsettled"]),
        (&json!("stable"), &json!(0))
    );
    let dir = backup_dir(&retail, b["id"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(dir.join("WTF").join(&first)).unwrap(),
        "rewritten while it was copied\n",
        "the second copy is the one kept"
    );
    let unsettled_left: Vec<_> = walk(&dir.join("WTF"), usize::MAX, usize::MAX)
        .unwrap()
        .entries
        .into_iter()
        .filter(|e| e.path.contains(".omaddons-settle-"))
        .collect();
    assert!(unsettled_left.is_empty(), "{unsettled_left:?}");
}

#[test]
fn a_tree_changing_on_both_tries_is_best_effort_and_on_one_try_stable() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let wtf = Wtf::at(&retail).saved_variables("Foo", 100).build();
    let file = wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua");
    let argv = ["backup", "create", "-t", "retail", "--progress", "json"];

    // Changed after its own copy on both attempts: kept, and marked.
    let pause = Pause::new(home.env.root());
    let child = home.spawn(
        &argv,
        &[
            (
                "OMADDONS_FAILPOINTS",
                "backup.capture.done.1=pause,backup.capture.done.2=pause",
            ),
            pause.env(),
        ],
    );
    for (n, step) in ["backup.capture.done.1", "backup.capture.done.2"]
        .into_iter()
        .enumerate()
    {
        pause.reached(step);
        fs::write(&file, "x".repeat(200 + n)).unwrap();
        pause.resume(step);
    }
    let r = wait(child, &argv);
    assert_private_all(&r.stdout, &r.stderr);
    let envelope = r.expect(0);
    let b = &envelope["data"]["targets"][0]["backup"];
    assert_eq!(
        (&b["consistency"], &b["qualifying"]),
        (&json!("best_effort"), &json!(false))
    );
    let unsettled: Vec<&Value> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "backup_unsettled")
        .collect();
    assert_eq!(unsettled.len(), 1, "{envelope}");
    let message = unsettled[0]["message"].as_str().unwrap();
    assert!(
        message.contains("1 changed, 0 added and 0 removed"),
        "{message}"
    );
    assert_eq!(
        unsettled[0]["fix"],
        "make another backup after the game exits"
    );
    assert_eq!(unsettled[0]["fix_argv"], Value::Null);
    let human = home.exec_with(
        &["backup".into(), "list".into(), "-t".into(), "retail".into()],
        &[],
    );
    assert!(human.stdout.contains("best effort"), "{}", human.stdout);

    // Changed on the first attempt only: the second is stable, and the only backup.
    let pause = Pause::new(home.env.root());
    let child = home.spawn(
        &argv,
        &[
            ("OMADDONS_FAILPOINTS", "backup.capture.done.1=pause"),
            pause.env(),
        ],
    );
    pause.reached("backup.capture.done.1");
    fs::write(&file, "y".repeat(300)).unwrap();
    pause.resume("backup.capture.done.1");
    let envelope = wait(child, &argv).expect(0);
    let b = &envelope["data"]["targets"][0]["backup"];
    assert_eq!(b["consistency"], "stable", "{envelope}");
    assert!(!codes(&envelope).contains(&"backup_unsettled"));
    let dir = backup_dir(&retail, b["id"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(dir.join("WTF/Account/FAKEACCT/SavedVariables/Foo.lua")).unwrap(),
        "y".repeat(300)
    );
    let backups = fs::read_dir(retail.join("Interface/.omaddons/backups"))
        .unwrap()
        .count();
    assert_eq!(backups, 2, "one per run, never one per attempt");
    assert!(leftovers(&home, &retail).is_empty());
}

#[test]
fn a_tree_losing_files_on_both_tries_fails_with_a_redacted_path_and_publishes_nothing() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let wtf = Wtf::at(&retail)
        .saved_variables("Aaa", 100)
        .saved_variables("Bbb", 100)
        .saved_variables("Ccc", 100)
        .build();
    let sv = wtf.join("Account/FAKEACCT/SavedVariables");
    let first = walk(&wtf, usize::MAX, usize::MAX)
        .unwrap()
        .entries
        .into_iter()
        .find(|e| e.kind == EntryKind::File)
        .unwrap()
        .path;
    assert!(
        first.as_str() < "Account/FAKEACCT/SavedVariables/Bbb.lua",
        "{first} is copied before the files that go"
    );
    let pause = Pause::new(home.env.root());
    let argv = ["backup", "create", "-t", "retail", "--progress", "json"];
    let child = home.spawn(
        &argv,
        &[
            (
                "OMADDONS_FAILPOINTS",
                "backup.capture.copied.1=pause,backup.capture.copied.2=pause",
            ),
            pause.env(),
        ],
    );
    pause.reached("backup.capture.copied.1");
    fs::remove_file(sv.join("Bbb.lua")).unwrap();
    fs::rename(sv.join("Ccc.lua"), sv.join("Ddd.lua")).unwrap();
    pause.resume("backup.capture.copied.1");
    pause.reached("backup.capture.copied.2");
    fs::remove_file(sv.join("Ddd.lua")).unwrap();
    pause.resume("backup.capture.copied.2");
    let r = wait(child, &argv);
    assert_private_all(&r.stdout, &r.stderr);
    let envelope = r.expect(1);
    let error = &envelope["error"];
    assert_eq!(
        (&error["code"], &error["retryable"]),
        (&json!("io"), &json!(true))
    );
    let path = error["details"]["path"].as_str().unwrap();
    assert!(path.ends_with("SavedVariables/Ddd.lua"), "{path}");
    assert!(
        error["message"].as_str().unwrap().contains(path),
        "{envelope}"
    );
    assert_eq!(error["fix_argv"], Value::Null);
    let backups = retail.join("Interface/.omaddons/backups");
    assert!(
        fs::read_dir(&backups).map_or(true, |mut l| l.next().is_none()),
        "nothing published"
    );
    assert!(
        leftovers(&home, &retail).is_empty(),
        "{:?}",
        leftovers(&home, &retail)
    );
    assert!(
        history(&home).iter().all(|l| l["kind"] != "backup"),
        "no history line for what was not made"
    );
}

#[test]
fn verify_hashes_every_file_it_copied() {
    use sha2::{Digest, Sha256};
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let wtf = Wtf::at(&retail)
        .config_wtf("SET a 1\n")
        .saved_variables("Foo", 300)
        .symlink("Config.link", "Config.wtf")
        .build();
    let r = run(
        &home,
        &["backup", "create", "-t", "retail", "--verify"],
        &[],
    );
    let envelope = r.expect(0);
    // Progress: the copy, then the hashing, each in its phase under a fixed label.
    let progress: Vec<(String, String, u64)> = r
        .stderr
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|e| e["event"] == "progress")
        .map(|e| {
            (
                e["phase"].as_str().unwrap().to_owned(),
                e["message"].as_str().unwrap().to_owned(),
                e["current"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        progress,
        [
            ("copy".to_owned(), "WTF/ files".to_owned(), 3),
            ("verify".to_owned(), "verifying".to_owned(), 3),
        ],
        "{}",
        r.stderr
    );
    let b = &envelope["data"]["targets"][0]["backup"];
    assert_eq!(
        (&b["integrity"], &b["baseline"]),
        (&json!("verified"), &json!("capture"))
    );
    assert!(b["verified_at"].is_string());
    let dir = backup_dir(&retail, b["id"].as_str().unwrap());
    let manifest: Manifest = store::load(&dir.join("manifest.json"))
        .unwrap()
        .unwrap()
        .value;
    let mut files = 0;
    for entry in &manifest.entries {
        if entry.kind == EntryKind::File {
            files += 1;
            let digest = Sha256::digest(fs::read(wtf.join(&entry.path)).unwrap());
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(
                entry.sha256.as_deref(),
                Some(hex.as_str()),
                "{}",
                entry.path
            );
        } else {
            assert_eq!(entry.sha256, None, "{}", entry.path);
        }
    }
    assert_eq!(files, 3);
    let human = home.exec(&[
        "backup".into(),
        "create".into(),
        "-t".into(),
        "retail".into(),
        "--verify".into(),
    ]);
    // Never "verified" alone: what its hashes were recorded at (M3-D47).
    assert!(
        human.stdout.contains(", verified (capture))"),
        "{}",
        human.stdout
    );
    let listed = home.exec(&["backup".into(), "list".into()]);
    assert!(
        listed.stdout.contains("stable · verified"),
        "{}",
        listed.stdout
    );
}

#[test]
fn no_global_lock_is_held_while_copying() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    Wtf::at(&retail).saved_variables("Foo", 100).build();
    let foo = local_addon(&home, "Foo");
    home.run(&["install", &foo, "-t", "retail"]).expect(0);
    let pause = Pause::new(home.env.root());
    let argv = ["backup", "create", "-t", "retail"];
    let child = home.spawn(
        &argv,
        &[
            ("OMADDONS_FAILPOINTS", "backup.capture.done.1=pause"),
            pause.env(),
        ],
    );
    pause.reached("backup.capture.done.1");
    // While the copy waits, another change commits.
    let pinned = home.run(&["pin", "Foo", "-t", "retail"]);
    pinned.expect(0);
    pause.resume("backup.capture.done.1");
    wait(child, &argv).expect(0);
    let kinds: Vec<Value> = history(&home).iter().map(|l| l["kind"].clone()).collect();
    assert_eq!(kinds, [json!("install"), json!("pin"), json!("backup")]);
}

#[test]
fn bare_create_backs_up_every_live_client_with_a_wtf_under_one_run() {
    let home = Home::new(&["wow", "wow_classic_era", "wowt", "wow_classic"]);
    for folder in ["_retail_", "_classic_era_", "_ptr_"] {
        support::small_wtf(&home.client(folder));
    }
    support::never_launched(&home.client("_classic_"));
    let envelope = run(&home, &["backup", "create"], &[]).expect(0);
    let targets = envelope["data"]["targets"].as_array().unwrap();
    let ids: Vec<&str> = targets
        .iter()
        .map(|t| t["target"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["era", "retail"], "live clients with a WTF/, by id");
    assert!(
        targets
            .iter()
            .all(|t| t["backup"]["consistency"] == "stable")
    );
    let run_id = &envelope["data"]["run"];
    let lines = history(&home);
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .iter()
            .all(|l| l["kind"] == "backup" && &l["run"] == run_id)
    );
    for (t, line) in targets.iter().zip(&lines) {
        assert_eq!(t["tx"], line["tx"]);
        assert_eq!(t["target"]["key"], line["target"]);
    }
    assert!(
        !home
            .client("_ptr_")
            .join("Interface/.omaddons/backups")
            .exists()
    );
    assert!(
        !home
            .client("_classic_")
            .join("Interface/.omaddons/backups")
            .exists()
    );

    // A test client is backed up when -t names it.
    let envelope = run(&home, &["backup", "create", "-t", "ptr"], &[]).expect(0);
    assert_eq!(envelope["data"]["targets"][0]["target"]["id"], "ptr");

    // -t naming several: a client without WTF/ fails on its own.
    let envelope = run(&home, &["backup", "create", "-t", "mists"], &[]);
    assert_eq!(
        envelope.expect(3)["error"]["code"],
        "not_found",
        "only Mists Classic, without WTF/"
    );
    let home = Home::new(&["wow", "wowt"]);
    support::small_wtf(&home.client("_retail_"));
    support::never_launched(&home.client("_ptr_"));
    let envelope = run(&home, &["backup", "create", "-t", "mainline"], &[]).expect(0);
    assert_eq!(envelope["partial"], true);
    let targets = envelope["data"]["targets"].as_array().unwrap();
    assert_eq!(targets[0]["target"]["id"], "ptr");
    assert_eq!(targets[0]["error"]["code"], "not_found");
    assert_eq!(targets[0]["backup"], Value::Null);
    assert_eq!(targets[1]["target"]["id"], "retail");
    assert_eq!(targets[1]["backup"]["kind"], "manual");
    let failed: Vec<&Value> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "not_found")
        .collect();
    assert_eq!(failed.len(), 1, "{envelope}");
    assert!(failed[0]["message"].as_str().unwrap().starts_with("ptr: "));
    let human = home.exec(&[
        "backup".into(),
        "create".into(),
        "-t".into(),
        "mainline".into(),
    ]);
    assert!(
        human
            .stdout
            .contains("ptr: not backed up: ptr has no WTF/ yet"),
        "{}",
        human.stdout
    );
}

#[test]
fn with_nothing_to_back_up_bare_create_says_why() {
    // Only a test client has a WTF/: name it.
    let home = Home::new(&["wow", "wowt"]);
    support::small_wtf(&home.client("_ptr_"));
    support::never_launched(&home.client("_retail_"));
    let envelope = run(&home, &["backup", "create", "--verify"], &[]).expect(3);
    assert_eq!(envelope["error"]["code"], "target_not_found");
    assert_eq!(
        envelope["error"]["fix_argv"],
        json!(["omaddons", "backup", "create", "-t", "ptr", "--verify"])
    );
    // No client has one.
    let home = Home::new(&["wow"]);
    support::never_launched(&home.client("_retail_"));
    let envelope = run(&home, &["backup", "create"], &[]).expect(3);
    assert_eq!(envelope["error"]["code"], "not_found");
    assert_eq!(envelope["error"]["fix_argv"], Value::Null);
    // -t names one without: nothing is registered or written.
    let envelope = run(&home, &["backup", "create", "-t", "retail"], &[]).expect(3);
    assert_eq!(envelope["error"]["code"], "not_found");
    assert_eq!(
        envelope["error"]["message"],
        "retail has no WTF/ yet: it has not been launched"
    );
    assert_eq!(envelope["error"]["fix_argv"], Value::Null);
    assert!(!home.state().join("registry.json").exists());
    assert!(!home.state().join("history.jsonl").exists());
}

#[test]
fn a_dry_run_says_what_it_would_copy_and_writes_nothing() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let wtf = Wtf::at(&retail)
        .config_wtf("SET a 1\n")
        .saved_variables("Foo", 1000)
        .build();
    // Discovery's cache is derived anyway: warm it first.
    home.run(&["status"]).expect(0);
    let watched = [home.env.state(), home.env.cache(), retail.clone()];
    let before: Vec<_> = watched.iter().map(|d| tree_state(d)).collect();
    let envelope = run(&home, &["backup", "create", "--dry-run"], &[]).expect(0);
    let after: Vec<_> = watched.iter().map(|d| tree_state(d)).collect();
    assert!(before == after, "a dry run wrote something");
    let data = &envelope["data"];
    assert_eq!(
        (&data["dry_run"], &data["run"]),
        (&json!(true), &Value::Null)
    );
    let t = &data["targets"][0];
    assert_eq!((&t["backup"], &t["tx"]), (&Value::Null, &Value::Null));
    assert_eq!(
        t["would"],
        json!({"files": 3, "bytes": 2008, "copy": "byte_copy", "free_bytes": 1024u64 << 20})
    );
    assert_eq!(
        walk(&wtf, usize::MAX, usize::MAX)
            .unwrap()
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| e.size)
            .sum::<u64>(),
        2008
    );
    let human = home.exec_with(
        &["backup".into(), "create".into(), "--dry-run".into()],
        &[
            ("OMADDONS_FS_FREE_MB", "1024"),
            ("OMADDONS_FS_CLONE", "yes"),
        ],
    );
    assert_eq!(
        human.stdout,
        "retail: would back up 3 files (2.0 kB), copy-on-write · 1.1 GB free\n"
    );
}

#[test]
fn low_free_space_warns_and_still_backs_up() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    support::small_wtf(&retail);
    let envelope = run(
        &home,
        &["backup", "create", "-t", "retail"],
        &[("OMADDONS_FS_FREE_MB", "100")],
    )
    .expect(0);
    assert_eq!(envelope["data"]["targets"][0]["backup"]["kind"], "manual");
    let low: Vec<&Value> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "backup_space_low")
        .collect();
    assert_eq!(low.len(), 1, "{envelope}");
    assert!(
        low[0]["message"]
            .as_str()
            .unwrap()
            .contains("limits.backup_min_free_mb"),
        "{envelope}"
    );
    assert_eq!(low[0]["fix_argv"], Value::Null);
    // The default (2,048 MiB) against 1,024 MiB free; 0 turns the check off.
    let envelope = run(&home, &["backup", "create", "-t", "retail"], &[]).expect(0);
    assert!(codes(&envelope).contains(&"backup_space_low"));
    home.run(&["config", "set", "limits.backup_min_free_mb", "0"])
        .expect(0);
    let envelope = run(
        &home,
        &["backup", "create", "-t", "retail"],
        &[("OMADDONS_FS_FREE_MB", "1")],
    )
    .expect(0);
    assert!(
        !codes(&envelope).contains(&"backup_space_low"),
        "{envelope}"
    );
}

#[test]
fn undo_steps_over_a_backup() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    support::small_wtf(&retail);
    let foo = local_addon(&home, "Foo");
    let installed = home.run(&["install", &foo, "-t", "retail"]).expect(0);
    let backed_up = run(&home, &["backup", "create", "-t", "retail"], &[]).expect(0);
    let undone = home.run(&["undo"]).expect(0);
    assert_eq!(undone["data"]["reverted"]["tx"], installed["data"]["tx"]);
    assert!(!home.addons("_retail_").join("Foo").exists());
    let id = backed_up["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap();
    assert!(backup_dir(&retail, id).is_dir(), "a backup is not undone");
    // A backup's own line is no transaction undo reverts.
    let tx = backed_up["data"]["targets"][0]["tx"].as_str().unwrap();
    let refused = home.run(&["undo", tx]).expect(3);
    assert_eq!(refused["error"]["code"], "not_found");
    assert_eq!(
        refused["error"]["message"],
        format!("transaction {tx} made a backup, which undo does not revert")
    );
    assert_eq!(
        refused["error"]["fix_argv"],
        json!(["omaddons", "backup", "list"])
    );
}

#[test]
fn backup_list_offers_a_backup_until_a_fresh_qualifying_one_exists() {
    let home = Home::new(&["wow", "wow_classic_era"]);
    let retail = home.client("_retail_");
    support::small_wtf(&retail);
    let hint = json!([{
        "action": "backup_create",
        "command": "omaddons backup create -t retail",
        "argv": ["omaddons", "backup", "create", "-t", "retail"],
        "destructive": false
    }]);
    // era has no WTF/: nothing to offer there.
    let listed = run(&home, &["backup", "list", "-t", "retail"], &[]).expect(0);
    assert_eq!(listed["commands"], hint);
    let listed = run(&home, &["backup", "list", "-t", "era"], &[]).expect(0);
    assert_eq!(listed["commands"], json!([]));

    // The hint runs, and then is gone.
    let argv: Vec<String> = hint[0]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|a| a.as_str().unwrap().to_owned())
        .chain(["--json".to_owned()])
        .collect();
    home.exec(&argv).expect(0);
    let listed = run(&home, &["backup", "list", "-t", "retail"], &[]).expect(0);
    assert_eq!(listed["commands"], json!([]));
    // Eight days on, it is stale again.
    let later = (jiff::Timestamp::now() + jiff::SignedDuration::from_hours(8 * 24)).to_string();
    let listed = run(
        &home,
        &["backup", "list", "-t", "retail"],
        &[("OMADDONS_NOW", &later)],
    )
    .expect(0);
    assert_eq!(listed["commands"], hint);
}

/// Three live clients of 10, 20 and 30 files, backed up in one run: one `copy` count of
/// 60 files under the one label, each client's from where the ones before it ended;
/// with `--verify`, one `verify` count of 60 too. One client reports as it always has.
#[test]
fn several_clients_report_one_run_wide_count() {
    use omaddons::backup::tree::COPY_LABEL;

    let home = Home::new(&["wow", "wow_classic_era", "wow_classic"]);
    for (folder, files) in [("_retail_", 10), ("_classic_era_", 20), ("_classic_", 30)] {
        Wtf::at(home.client(folder)).big(files * 64, files).build();
    }
    let r = run(&home, &["backup", "create"], &[]);
    r.expect(0);
    one_count(&progress(&r, "copy"), COPY_LABEL, 60, &r);
    let r = run(&home, &["backup", "create", "--verify"], &[]);
    r.expect(0);
    one_count(&progress(&r, "copy"), COPY_LABEL, 60, &r);
    one_count(&progress(&r, "verify"), "verifying", 60, &r);

    // A retried capture (the first client's tree changed after its first try) never
    // steps back.
    let pause = Pause::new(home.env.root());
    let argv = ["backup", "create", "--json", "--progress", "json"];
    let child = home.spawn(
        &argv,
        &[
            ("OMADDONS_FAILPOINTS", "backup.capture.done.1=pause@1"),
            ("OMADDONS_FS_FREE_MB", "1024"),
            pause.env(),
        ],
    );
    pause.reached("backup.capture.done.1");
    // classic is first, by id.
    let changed = home
        .client("_classic_")
        .join("WTF/Account/FAKEACCT/FakeRealm/Char0/SavedVariables/Addon0.lua");
    fs::write(&changed, "changed during the copy").unwrap();
    pause.resume("backup.capture.done.1");
    let r = wait(child, &argv);
    assert_private_all(&r.stdout, &r.stderr);
    let envelope = r.expect(0);
    one_count(&progress(&r, "copy"), COPY_LABEL, 60, &r);
    // It did retry: the second copy, the one kept, holds the change.
    let first = &envelope["data"]["targets"][0];
    assert_eq!(first["target"]["id"], "classic");
    let kept = backup_dir(
        &home.client("_classic_"),
        first["backup"]["id"].as_str().unwrap(),
    )
    .join("WTF/Account/FAKEACCT/FakeRealm/Char0/SavedVariables/Addon0.lua");
    assert_eq!(fs::read_to_string(kept).unwrap(), "changed during the copy");

    // One client: its own count, as before.
    let r = run(&home, &["backup", "create", "-t", "retail"], &[]);
    r.expect(0);
    one_count(&progress(&r, "copy"), COPY_LABEL, 10, &r);
}

/// A client whose `WTF/` cannot be walked adds nothing to the run's total, fails on its
/// own, and the others' count still never steps back or passes it.
#[test]
fn an_unreadable_client_leaves_the_others_count_right() {
    use omaddons::backup::tree::COPY_LABEL;

    let home = Home::new(&["wow", "wow_classic_era", "wow_classic"]);
    for (folder, files) in [("_retail_", 10), ("_classic_era_", 20), ("_classic_", 30)] {
        Wtf::at(home.client(folder)).big(files * 64, files).build();
    }
    let locked = home.client("_classic_").join("WTF/Account");
    fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    let r = run(&home, &["backup", "create"], &[]);
    writable_again(&locked);
    let envelope = r.expect(0);
    assert_eq!(envelope["partial"], true, "{}", r.context());
    one_count(&progress(&r, "copy"), COPY_LABEL, 30, &r);
}
