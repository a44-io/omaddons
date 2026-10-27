//! `omaddons backup prune` end to end (M3-D32, M3-D48): retention thins automatic
//! backups by local day and keeps the user's; named backups go whatever their age, never
//! the newest checkpoint; `--dry-run` deletes nothing; a backup a reader holds (a child
//! process with a shared reservation, then killed) is skipped as busy and pruned once
//! free; the reclamation pass under the free-space seam; retention after `backup create`,
//! and its failure as `retention_failed`. Every prune rings the doorbell when it deleted
//! something, appends no history line, keeps the inventory projection true and leaves no
//! trash behind. Every run is checked for the fake account, realm and character names.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::backups::{backup_dir, meta, seed_backup, seed_meta};
use common::home::{Home, Run};
use common::privacy::assert_private_all;
use omaddons::backup::{BackupKind, Trigger};
use omaddons::model::BackupId;
use serde_json::{Value, json};
use support::{at, auto, on_disk, pruned, reclaim, run, run_argv, spawn_reader, trash, wait_for};

/// The frozen clock every run reads.
const NOW: &str = "2026-10-10T21:00:00Z";

#[cfg(test)]
mod support {
    use super::*;

    pub fn at(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    /// `omaddons <args> --json --progress json` at [`NOW`], checked for privacy.
    pub fn run(home: &Home, args: &[&str], extra: &[(&str, &str)]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        run_argv(home, &argv, extra)
    }

    /// `omaddons <argv>` verbatim at [`NOW`], checked for privacy.
    pub fn run_argv(home: &Home, argv: &[String], extra: &[(&str, &str)]) -> Run {
        let mut env = vec![("OMADDONS_NOW", NOW), ("OMADDONS_FS_FREE_MB", "1024")];
        env.extend_from_slice(extra);
        let r = home.exec_with(argv, &env);
        assert_private_all(&r.stdout, &r.stderr);
        r
    }

    /// An automatic backup of `trigger`, captured at `captured`.
    pub fn auto(client: &Path, trigger: Trigger, captured: &str) -> BackupId {
        let mut m = meta(BackupKind::Auto, at(captured), 1);
        m.trigger = Some(trigger);
        seed_meta(client, &m)
    }

    /// The backups under `client`'s `<meta>/backups/`.
    pub fn on_disk(client: &Path) -> BTreeSet<BackupId> {
        fs::read_dir(client.join("Interface/.omaddons/backups"))
            .map(|l| {
                l.filter_map(|e| e.unwrap().file_name().to_str()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The ids one target's `pruned` lists.
    pub fn pruned(target: &Value) -> BTreeSet<BackupId> {
        target["pruned"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap().parse().unwrap())
            .collect()
    }

    /// Waits (at most 20 s) for `path` to exist.
    pub fn wait_for(path: &Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// A child process holding a shared reservation on one backup, as a restore or an
    /// export does, until killed.
    pub fn spawn_reader(home: &Home, dir: &Path) -> std::process::Child {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "child_reader",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("OMADDONS_READER_DIR", dir)
            .env("OMADDONS_READER_MARKER", home.env.root().join("held"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Runs `reclaim` on retail with `OMADDONS_FS_FREE_MB` saying `free_mb`, needing
    /// `need_mb`, in a child (the seam is read from the environment).
    pub fn reclaim(home: &Home, free_mb: &str, need_mb: u64) -> Value {
        let out = home.env.root().join("reclaimed.json");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "child_reclaim",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("OMADDONS_RECLAIM_HOME", home.env.root())
            .env("OMADDONS_RECLAIM_CLIENT", home.client("_retail_"))
            .env("OMADDONS_RECLAIM_NEED_MB", need_mb.to_string())
            .env("OMADDONS_FS_FREE_MB", free_mb)
            .env("OMADDONS_NOW", NOW)
            .env("TZ", "UTC")
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        serde_json::from_slice(&fs::read(out).unwrap()).unwrap()
    }

    /// What `client`'s backup trash holds: nothing once a prune finished.
    pub fn trash(client: &Path) -> Vec<PathBuf> {
        fs::read_dir(client.join("Interface/.omaddons/trash"))
            .map(|l| l.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default()
    }
}

#[test]
fn retention_keeps_each_days_first_and_last_and_the_users_backups() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    // Registered, so the inventory projection has a key to keep true.
    run(&home, &["targets", "default", "retail"], &[]).expect(0);
    let mut kept = BTreeSet::new();
    let mut all = BTreeSet::new();
    for day in 1..=10 {
        let d = |t: &str| format!("2026-10-{day:02}T{t}:00Z");
        let first = auto(&retail, Trigger::PreChange, &d("08:00"));
        let pre = auto(&retail, Trigger::PreChange, &d("12:00"));
        let posts = ["10:00", "14:00", "20:00"].map(|t| auto(&retail, Trigger::PostSession, &d(t)));
        all.extend([first, pre]);
        all.extend(posts);
        match day {
            // Within 24 h of NOW: every one.
            10 => kept.extend([first, pre, posts[0], posts[1], posts[2]]),
            // The newest 7 days with automatic backups: the first and the last.
            4..=9 => kept.extend([first, posts[2]]),
            _ => {}
        }
    }
    for captured in ["2026-09-01T10:00:00Z", "2026-10-05T10:00:00Z"] {
        let manual = seed_backup(&retail, BackupKind::Manual, at(captured), 1);
        kept.insert(manual);
        all.insert(manual);
    }
    // One victim holds a directory a byte copy kept read-only.
    let victim = all.difference(&kept).next().copied().unwrap();
    let sealed = backup_dir(&retail, &victim).join("WTF/Account");
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o500)).unwrap();
    let bell = home.env.cache().join("omaddons/doorbell.json");
    let _ = fs::remove_file(&bell);
    let history = fs::read(home.state().join("history.jsonl")).ok();

    let envelope = run(&home, &["backup", "prune", "-t", "retail"], &[]).expect(0);
    let t = &envelope["data"]["targets"][0];
    assert_eq!(t["target"]["id"], "retail");
    assert_eq!(pruned(t), &all - &kept);
    assert_eq!(
        (t["kept"].clone(), t["busy"].clone()),
        (json!(19), json!([]))
    );
    assert_eq!(t["reclaimed_for_space"], false);
    assert_eq!(on_disk(&retail), kept, "the 0500 directory is no obstacle");
    assert!(trash(&retail).is_empty(), "{:?}", trash(&retail));
    let rung: Value = serde_json::from_slice(&fs::read(&bell).unwrap()).unwrap();
    assert_eq!(rung["command"], "backup prune");
    assert_eq!(
        fs::read(home.state().join("history.jsonl")).ok(),
        history,
        "no history line"
    );
    let projection: Value =
        serde_json::from_slice(&fs::read(home.env.cache().join("omaddons/backups.json")).unwrap())
            .unwrap();
    let key = t["target"]["key"].as_str().unwrap();
    let covered: BTreeSet<BackupId> = projection["targets"][key]["covers"]["backups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().parse().unwrap())
        .collect();
    assert_eq!(covered, kept);

    // Again: nothing to prune, nothing written, no ring.
    fs::remove_file(&bell).unwrap();
    let envelope = run(&home, &["backup", "prune"], &[]).expect(0);
    assert_eq!(envelope["data"]["targets"][0]["pruned"], json!([]));
    assert!(!bell.exists());
}

#[test]
fn named_backups_go_whatever_their_age_but_never_the_newest_checkpoint() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let old = seed_backup(&retail, BackupKind::Manual, at("2026-10-01T10:00:00Z"), 1);
    let fresh = auto(&retail, Trigger::PostSession, "2026-10-10T20:00:00Z");
    let newest = seed_backup(&retail, BackupKind::Manual, at("2026-10-10T20:30:00Z"), 1);
    let other = seed_backup(&retail, BackupKind::Manual, at("2026-10-02T10:00:00Z"), 1);
    // Retention alone keeps all four.
    let envelope = run(&home, &["backup", "prune", "--dry-run"], &[]).expect(0);
    assert_eq!(envelope["data"]["targets"][0]["pruned"], json!([]));
    assert!(envelope["data"]["plan_id"].is_null(), "{envelope}");
    assert!(envelope["data"]["plan_expires_at"].is_null(), "{envelope}");
    assert_eq!(envelope["data"]["approval"]["selection"], json!([]));
    assert!(
        common::plans::plan_files(&home.env).is_empty(),
        "{envelope}"
    );
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            // ubs:ignore — warning codes are public enum names, not secrets
            .all(|warning| warning["code"] != "plan_not_stored"),
        "{envelope}",
    );

    // Ids after `--`, the template's form: one by prefix, one whole. Seeded in one
    // millisecond, the ids share their first ten characters.
    let prefix = &fresh.to_string()[..16];
    let argv: Vec<String> = ["backup", "prune", "-t", "retail", "--json", "--"]
        .iter()
        .map(|a| (*a).to_owned())
        .chain([prefix.to_owned(), old.to_string()])
        .collect();
    let envelope = run_argv(&home, &argv, &[]).expect(0);
    let t = &envelope["data"]["targets"][0];
    assert_eq!(
        pruned(t),
        BTreeSet::from([old, fresh]),
        "younger than a day too"
    );
    assert_eq!(t["kept"], 2);
    assert_eq!(on_disk(&retail), BTreeSet::from([newest, other]));

    // The newest checkpoint: refused, nothing deleted.
    let id = newest.to_string();
    let envelope = run(&home, &["backup", "prune", &id, &other.to_string()], &[]).expect(2);
    assert_eq!(envelope["error"]["code"], "usage");
    assert_eq!(
        envelope["error"]["fix_argv"],
        json!(["omaddons", "backup", "create", "-t", "retail"])
    );
    assert_eq!(on_disk(&retail), BTreeSet::from([newest, other]));
    let envelope = run(&home, &["backup", "prune", "01ZZZZZZ"], &[]).expect(3);
    assert_eq!(envelope["error"]["code"], "not_found");
    let envelope = run(&home, &["backup", "prune", &id, "--keep", "1"], &[]).expect(2);
    assert_eq!(
        envelope["error"]["code"], "usage",
        "ids or a pass, not both"
    );
}

#[test]
fn keep_and_older_than_reach_the_users_backups() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let manuals: Vec<BackupId> = ["01", "03", "05", "07"]
        .map(|d| {
            seed_backup(
                &retail,
                BackupKind::Manual,
                at(&format!("2026-10-{d}T10:00:00Z")),
                1,
            )
        })
        .to_vec();
    let imported = seed_backup(&retail, BackupKind::Imported, at("2026-08-01T10:00:00Z"), 1);
    let envelope = run(&home, &["backup", "prune", "--older-than", "6d"], &[]).expect(0);
    assert_eq!(
        pruned(&envelope["data"]["targets"][0]),
        BTreeSet::from([manuals[0], manuals[1], imported]),
        "captured before the 4th at 21:00"
    );
    let envelope = run(&home, &["backup", "prune", "--keep", "1"], &[]).expect(0);
    assert_eq!(
        pruned(&envelope["data"]["targets"][0]),
        BTreeSet::from([manuals[2]])
    );
    assert_eq!(on_disk(&retail), BTreeSet::from([manuals[3]]));
    let envelope = run(&home, &["backup", "prune", "--older-than", "soon"], &[]).expect(2);
    assert_eq!(envelope["error"]["code"], "usage");
}

#[test]
#[ignore = "child process for the busy tests"]
fn child_reader() {
    // A child process of the busy tests, not a check: run on its own it does nothing.
    let Some(dir) = std::env::var_os("OMADDONS_READER_DIR").map(PathBuf::from) else {
        return;
    };
    let _reservation = omaddons::fsx::DirLock::shared(&dir).unwrap();
    fs::write(std::env::var_os("OMADDONS_READER_MARKER").unwrap(), b"").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(120));
}

#[test]
fn a_backup_being_read_is_skipped_busy_and_pruned_once_its_reader_dies() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let held = auto(&retail, Trigger::PostSession, "2026-10-01T08:00:00Z");
    let free = auto(&retail, Trigger::PostSession, "2026-10-01T12:00:00Z");
    let last = auto(&retail, Trigger::PostSession, "2026-10-01T20:00:00Z");
    let mut reader = spawn_reader(&home, &backup_dir(&retail, &held));
    wait_for(&home.env.root().join("held"));

    let envelope = run(&home, &["backup", "prune", "-t", "retail"], &[]).expect(0);
    let t = &envelope["data"]["targets"][0];
    assert_eq!(pruned(t), BTreeSet::from([free]));
    assert_eq!(t["busy"], json!([{"id": held.to_string()}]));
    assert_eq!(t["kept"], 2, "the busy one stays");
    let envelope = run(&home, &["backup", "prune", &held.to_string()], &[]).expect(0);
    assert_eq!(
        envelope["data"]["targets"][0]["busy"],
        json!([{"id": held.to_string()}]),
        "named, it is still in use"
    );
    assert_eq!(on_disk(&retail), BTreeSet::from([held, last]));

    // SIGKILL: the kernel frees the reservation with the process.
    reader.kill().unwrap();
    reader.wait().unwrap();
    let envelope = run(&home, &["backup", "prune", "-t", "retail"], &[]).expect(0);
    assert_eq!(
        pruned(&envelope["data"]["targets"][0]),
        BTreeSet::from([held])
    );
    assert_eq!(on_disk(&retail), BTreeSet::from([last]));
    assert!(trash(&retail).is_empty());
}

#[test]
#[ignore = "child process for the reclamation test"]
fn child_reclaim() {
    use omaddons::backup::inventory::Ring;
    use omaddons::backup::prune::{self, SpaceNeed};
    use omaddons::fsx::ScopeTable;
    use omaddons::state::paths::{ClientPaths, Paths};
    // A child process of the reclamation test, not a check.
    let Some(root) = std::env::var_os("OMADDONS_RECLAIM_HOME").map(PathBuf::from) else {
        return;
    };
    let client_dir = PathBuf::from(std::env::var_os("OMADDONS_RECLAIM_CLIENT").unwrap());
    let need: u64 = std::env::var("OMADDONS_RECLAIM_NEED_MB")
        .unwrap()
        .parse()
        .unwrap();
    let paths = Paths::with_roots(
        root.join("home"),
        root.join("config"),
        root.join("state"),
        root.join("cache"),
    );
    let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client_dir), None).unwrap();
    let ring = Ring {
        command: "backup create",
        actor: omaddons::model::Actor::Agent,
    };
    let done = prune::reclaim(
        &paths,
        &scopes,
        &ClientPaths::new(&client_dir),
        None,
        SpaceNeed::Free(need << 20),
        &BTreeSet::new(),
        ring,
    )
    .unwrap();
    let ids: Vec<String> = done
        .report
        .pruned
        .iter()
        .map(|p| p.id.to_string())
        .collect();
    let result = json!({"pruned": ids, "met": done.met});
    fs::write(root.join("reclaimed.json"), result.to_string()).unwrap();
}

#[test]
fn reclamation_takes_eligible_automatic_backups_oldest_first_and_never_the_users() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let oldest = auto(&retail, Trigger::PreChange, "2026-09-01T08:00:00Z");
    let older = auto(&retail, Trigger::PostSession, "2026-10-01T08:00:00Z");
    let checkpoint = auto(&retail, Trigger::PostSession, "2026-10-08T08:00:00Z");
    let fresh = auto(&retail, Trigger::PostSession, "2026-10-10T20:00:00Z");
    let manual = seed_backup(&retail, BackupKind::Manual, at("2026-08-01T10:00:00Z"), 1);
    // Enough free: nothing goes.
    let done = reclaim(&home, "1024", 512);
    assert_eq!(done, json!({"pruned": [], "met": true}));
    // Never enough: every eligible one, oldest first, and the need is still unmet. The
    // newest checkpoint is the fresh one; the one before it is eligible.
    let done = reclaim(&home, "1024", 2048);
    assert_eq!(
        done,
        json!({"pruned": [oldest.to_string(), older.to_string(), checkpoint.to_string()], "met": false})
    );
    assert_eq!(on_disk(&retail), BTreeSet::from([fresh, manual]));
    // Only protected backups left: nothing more.
    let done = reclaim(&home, "1024", 2048);
    assert_eq!(done, json!({"pruned": [], "met": false}));
}

#[test]
fn backup_create_runs_retention_and_a_failure_only_warns() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    common::wtf::Wtf::at(retail.clone())
        .config_wtf("SET locale \"enUS\"\n")
        .saved_variables("Foo", 2)
        .build();
    let first = auto(&retail, Trigger::PreChange, "2026-10-01T08:00:00Z");
    auto(&retail, Trigger::PostSession, "2026-10-01T12:00:00Z");
    let last = auto(&retail, Trigger::PostSession, "2026-10-01T20:00:00Z");
    let retention_failed = |envelope: &Value| {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| matches!(w["code"].as_str(), Some("retention_failed")))
            .cloned()
    };
    let envelope = run(&home, &["backup", "create", "-t", "retail"], &[]).expect(0);
    let made: BackupId = envelope["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(retention_failed(&envelope), None);
    assert_eq!(
        on_disk(&retail),
        BTreeSet::from([first, last, made]),
        "the day's extra went"
    );

    // A victim whose directory cannot be opened for its reservation: the backup stands.
    let stuck = auto(&retail, Trigger::PostSession, "2026-10-01T14:00:00Z");
    let dir = backup_dir(&retail, &stuck);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o300)).unwrap();
    let envelope = run(&home, &["backup", "create", "-t", "retail"], &[]).expect(0);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let warning = retention_failed(&envelope);
    assert!(warning.is_some(), "{envelope}");
    let warning = warning.unwrap();
    assert_eq!(
        warning["fix_argv"],
        json!(["omaddons", "backup", "prune", "-t", "retail"])
    );
    assert!(envelope["data"]["targets"][0]["backup"]["id"].is_string());
    assert!(on_disk(&retail).contains(&stuck));
    run(&home, &["backup", "prune", "-t", "retail"], &[]).expect(0);
    assert!(!on_disk(&retail).contains(&stuck), "the fix prunes it");
}
