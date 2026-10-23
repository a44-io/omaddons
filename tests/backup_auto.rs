//! Automatic backups end to end (M3-D30, M3-D31, M3-D48): a pre-change checkpoint before
//! the day's first change that moves folders, at most once a local day and none when
//! `WTF/` is unchanged since the newest qualifying capture; the after-play checkpoint of
//! `backup create --auto`, skipped with a reason and exit 0; what counts as a checkpoint;
//! changes that make none; the default `daily`, which writes nothing where the filesystem
//! cannot clone; the space guard and the byte-copy budget; one attempt at a time per
//! client; and a failure that never fails the change. Every run is checked for the fake
//! account, realm and character names, on both streams.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use common::archives::zip_text;
use common::backups::{meta, seed_meta};
use common::home::{Home, Run};
use common::privacy::{ACCOUNT, assert_private_all};
use common::wtf::Wtf;
use omaddons::backup::{BackupKind, Consistency, Reflinked, Trigger};
use serde_json::{Value, json};
use support::{at, autos, cloning, history, install, run, set_mode};

/// Day 1, mid-morning, and its evening.
const DAY1: &str = "2026-10-21T10:00:00Z";
const EVENING: &str = "2026-10-21T20:00:00Z";
const DAY2: &str = "2026-10-22T10:00:00Z";
const DAY3: &str = "2026-10-23T10:00:00Z";

#[cfg(test)]
mod support {
    use super::*;

    pub fn at(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    /// `omaddons <args> --json --progress json` at `now`, with plenty of space free,
    /// checked for privacy on both streams.
    pub fn run(home: &Home, now: &str, args: &[&str], extra: &[(&str, &str)]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let mut env = vec![("OMADDONS_NOW", now), ("OMADDONS_FS_FREE_MB", "8192")];
        env.extend_from_slice(extra);
        let r = home.exec_with(&argv, &env);
        assert_private_all(&r.stdout, &r.stderr);
        r
    }

    /// `auto_backup = <mode>`.
    pub fn set_mode(home: &Home, mode: &str) {
        run(home, DAY1, &["config", "set", "auto_backup", mode], &[]).expect(0);
    }

    /// Installs a one-folder addon `name` from a local zip at `now`; returns the run.
    pub fn install(home: &Home, now: &str, name: &str, extra: &[(&str, &str)]) -> Run {
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        let zip = dl.join(format!("{name}.zip"));
        let toc = "## Interface: 120105\n## Version: 1\n";
        fs::write(&zip, zip_text(&[(&format!("{name}/{name}.toc"), toc)])).unwrap();
        let r = format!("gh:o/{}", name.to_ascii_lowercase());
        let path = zip.display().to_string();
        run(
            home,
            now,
            &["install", &r, "--file", &path, "-t", "retail"],
            extra,
        )
    }

    /// The automatic backups of retail, oldest first: their trigger.
    pub fn autos(home: &Home) -> Vec<String> {
        let envelope = run(home, DAY3, &["backup", "list", "-t", "retail"], &[]).expect(0);
        let mut found: Vec<(String, String)> = envelope["data"]["targets"][0]["backups"]
            .as_array()
            .map(|b| {
                b.iter()
                    .filter(|b| b["kind"] == "auto")
                    .map(|b| {
                        (
                            b["captured_at"].as_str().unwrap().to_owned(),
                            b["trigger"].as_str().unwrap().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        found.sort();
        found.into_iter().map(|(_, trigger)| trigger).collect()
    }

    /// The history lines' kinds, oldest first.
    pub fn history(home: &Home) -> Vec<String> {
        fs::read_to_string(home.state().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let line: Value = serde_json::from_str(l).unwrap();
                line["kind"].as_str().unwrap().to_owned()
            })
            .collect()
    }

    /// A home with settings under the target dir (btrfs on the development machine),
    /// where clones work; `None`, with a `SKIP` line, elsewhere.
    pub fn cloning(test: &str) -> Option<(tempfile::TempDir, Home)> {
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
        Wtf::at(home.client("_retail_"))
            .saved_variables("Foo", 200)
            .build();
        Some((base, home))
    }

    /// Retail's settings: one SavedVariables file.
    pub fn settings(home: &Home) -> PathBuf {
        home.client("_retail_").join(ACCOUNT).join("Foo.lua")
    }
}

/// A retail client with settings, `auto_backup = <mode>`.
fn client(mode: &str) -> Home {
    let home = Home::new(&["wow"]);
    Wtf::at(home.client("_retail_"))
        .config_wtf("SET locale \"enUS\"\n")
        .saved_variables("Foo", 200)
        .build();
    set_mode(&home, mode);
    home
}

#[test]
fn a_checkpoint_before_the_days_first_change_once_a_day_and_only_when_changed() {
    let home = client("daily-copy");
    install(&home, DAY1, "Foo", &[]).expect(0);
    assert_eq!(autos(&home), ["pre_change"]);
    assert_eq!(
        history(&home)[history(&home).len() - 2..],
        ["backup", "install"],
        "the backup's line just before the change's"
    );
    install(&home, DAY1, "Bar", &[]).expect(0);
    assert_eq!(autos(&home), ["pre_change"], "done today");

    // The next day, after a session that changed settings.
    fs::write(support::settings(&home), "Foo = { played = true }").unwrap();
    install(&home, DAY2, "Baz", &[]).expect(0);
    assert_eq!(autos(&home), ["pre_change", "pre_change"]);
    // The day after, nothing changed since.
    install(&home, DAY3, "Qux", &[]).expect(0);
    assert_eq!(
        autos(&home).len(),
        2,
        "unchanged since the newest checkpoint"
    );
}

/// Nine days of one change a day, settings changed each day: each change's pre-change
/// backup runs backup retention (M3-D32), so the newest 7 days' backups remain. When the
/// pass fails, the change still commits, with `retention_failed`.
#[test]
fn pre_change_backups_are_thinned_by_day_as_they_are_made() {
    let home = client("daily-copy");
    for day in 1..=9 {
        fs::write(support::settings(&home), format!("Foo = {{ day = {day} }}")).unwrap();
        let now = format!("2026-10-0{day}T10:00:00Z");
        install(&home, &now, &format!("Day{day}"), &[]).expect(0);
    }
    let kept: Vec<String> =
        run(&home, DAY3, &["backup", "list", "-t", "retail"], &[]).expect(0)["data"]["targets"][0]
            ["backups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["captured_at"].as_str().unwrap()[..10].to_owned())
            .collect();
    let days: BTreeSet<&str> = kept.iter().map(String::as_str).collect();
    assert_eq!(
        days,
        BTreeSet::from([
            "2026-10-03",
            "2026-10-04",
            "2026-10-05",
            "2026-10-06",
            "2026-10-07",
            "2026-10-08",
            "2026-10-09"
        ]),
        "{kept:?}"
    );

    // A pass that cannot delete: the trash is a file. The change commits anyway.
    let trash = home.client("_retail_").join("Interface/.omaddons/trash");
    fs::remove_dir_all(&trash).unwrap();
    fs::write(&trash, "").unwrap();
    fs::write(support::settings(&home), "Foo = { day = 10 }").unwrap();
    let r = install(&home, "2026-10-10T10:00:00Z", "Day10", &[]).expect(0);
    let codes: Vec<&str> = r["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"retention_failed"), "{r}");
    assert_eq!(r["data"]["items"][0]["status"], "installed", "{r}");
}

#[test]
fn the_after_play_checkpoint_is_made_when_due_and_skipped_with_its_reason() {
    let mut home = client("daily-copy");
    install(&home, DAY1, "Foo", &[]).expect(0);
    let auto = ["backup", "create", "--auto", "-t", "retail"];
    // A session since this morning's checkpoint, over by the evening.
    fs::write(support::settings(&home), "Foo = { played = true }").unwrap();
    let envelope = run(&home, EVENING, &auto, &[]).expect(0);
    let t = &envelope["data"]["targets"][0];
    assert_eq!(t["backup"]["trigger"], "post_session", "{envelope}");
    assert!(t["tx"].is_string(), "with its history line");
    assert_eq!(autos(&home), ["pre_change", "post_session"]);

    let envelope = run(&home, EVENING, &auto, &[]).expect(0);
    assert_eq!(
        envelope["data"]["targets"][0]["skipped"],
        json!({"reason": "unchanged"})
    );
    fs::write(support::settings(&home), "Foo = { again = true }").unwrap();
    home.retail_running();
    let envelope = run(&home, EVENING, &auto, &[]).expect(0);
    assert_eq!(
        envelope["data"]["targets"][0]["skipped"],
        json!({"reason": "running"})
    );
    // A dry run says what it would do, and writes nothing.
    home.nothing_running();
    let envelope = run(&home, EVENING, &[&auto[..], &["--dry-run"]].concat(), &[]).expect(0);
    assert!(
        envelope["data"]["targets"][0]["would"].is_object(),
        "{envelope}"
    );
    assert_eq!(autos(&home).len(), 2);
    // Human output names the reason.
    run(&home, EVENING, &auto, &[]).expect(0);
    let argv: Vec<String> = auto.map(str::to_owned).to_vec();
    let human = home.exec_with(&argv, &[("OMADDONS_NOW", EVENING)]);
    assert_private_all(&human.stdout, &human.stderr);
    assert!(
        human
            .stdout
            .contains("no automatic backup: WTF/ is unchanged since the last checkpoint"),
        "{}",
        human.stdout
    );
}

#[test]
fn an_import_a_partial_recovery_or_a_best_effort_capture_is_no_checkpoint() {
    let home = client("daily-copy");
    let retail = home.client("_retail_");
    let year_ago = meta(BackupKind::Imported, at("2025-10-21T10:00:00Z"), 1);
    let mut partial = meta(BackupKind::Recovered, at("2026-10-21T08:00:00Z"), 1);
    partial.partial = true;
    let mut shaky = meta(BackupKind::Auto, at("2026-10-21T09:00:00Z"), 1);
    shaky.trigger = Some(Trigger::PreChange);
    shaky.consistency = Consistency::BestEffort;
    for m in [&year_ago, &partial, &shaky] {
        seed_meta(&retail, m);
    }
    install(&home, DAY1, "Foo", &[]).expect(0);
    assert_eq!(
        autos(&home),
        ["pre_change", "pre_change"],
        "one more, qualifying"
    );
}

#[test]
fn records_only_changes_dry_runs_and_no_ops_make_none() {
    let home = client("daily-copy");
    install(&home, DAY1, "Foo", &[]).expect(0);
    fs::write(support::settings(&home), "Foo = { played = true }").unwrap();
    // The next day, with settings changed: due, but none of these moves a folder.
    run(&home, DAY2, &["pin", "Foo", "-t", "retail"], &[]).expect(0);
    install(&home, DAY2, "Foo", &[]).expect(0);
    let dl = home.env.root().join("dl/Bar.zip");
    fs::write(
        &dl,
        zip_text(&[("Bar/Bar.toc", "## Interface: 120105\n## Version: 1\n")]),
    )
    .unwrap();
    let bar = dl.display().to_string();
    run(
        &home,
        DAY2,
        &[
            "install",
            "gh:o/bar",
            "--file",
            &bar,
            "-t",
            "retail",
            "--dry-run",
        ],
        &[],
    )
    .expect(0);
    assert_eq!(autos(&home), ["pre_change"]);
    // A change that moves a folder makes it.
    install(&home, DAY2, "Bar", &[]).expect(0);
    assert_eq!(autos(&home), ["pre_change", "pre_change"]);
}

#[test]
fn an_undo_is_a_change_too() {
    let home = client("daily-copy");
    install(&home, DAY1, "Foo", &[]).expect(0);
    fs::write(support::settings(&home), "Foo = { played = true }").unwrap();
    run(&home, DAY2, &["undo"], &[]).expect(0);
    assert_eq!(autos(&home), ["pre_change", "pre_change"]);
    let lines = history(&home);
    assert_eq!(lines[lines.len() - 2..], ["backup", "undo"]);
}

#[test]
fn the_default_writes_nothing_where_the_filesystem_cannot_clone() {
    let home = Home::new(&["wow"]);
    Wtf::at(home.client("_retail_"))
        .saved_variables("Foo", 200)
        .build();
    let retail = home.client("_retail_");
    let wtf = common::trees::tree_hash(&retail.join("WTF"));
    install(&home, DAY1, "Foo", &[]).expect(0);
    assert!(!retail.join("Interface/.omaddons/backups").exists());
    assert!(!home.env.cache().join("omaddons/backups.json").exists());
    assert_eq!(common::trees::tree_hash(&retail.join("WTF")), wtf);
    assert_eq!(history(&home), ["install"]);
}

#[test]
fn a_failure_never_fails_the_change() {
    let home = client("daily-copy");
    let r = install(
        &home,
        DAY1,
        "Foo",
        &[("OMADDONS_FAILPOINTS", "engine.autobackup=error")],
    );
    let envelope = r.expect(0);
    let codes: Vec<&str> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"auto_backup_failed"), "{envelope}");
    assert!(home.addons("_retail_").join("Foo").is_dir());
    assert!(autos(&home).is_empty());
    // Asked for, the same failure is the item's error.
    let envelope = run(
        &home,
        DAY1,
        &["backup", "create", "--auto", "-t", "retail"],
        &[("OMADDONS_FAILPOINTS", "engine.autobackup=error")],
    )
    .expect(1);
    assert_eq!(envelope["error"]["code"], "io", "{envelope}");
}

#[test]
fn below_the_free_space_floor_eligible_automatic_backups_go_then_it_skips() {
    let home = client("daily-copy");
    let retail = home.client("_retail_");
    let old = |captured: &str| {
        let mut m = meta(BackupKind::Auto, at(captured), 1);
        m.trigger = Some(Trigger::PreChange);
        m
    };
    let eligible = seed_meta(&retail, &old("2026-10-10T08:00:00Z"));
    let newest = seed_meta(&retail, &old("2026-10-11T08:00:00Z"));
    let manual = seed_meta(
        &retail,
        &meta(BackupKind::Manual, at("2026-09-01T08:00:00Z"), 1),
    );
    let low = [("OMADDONS_FS_FREE_MB", "100")];
    let envelope = install(&home, DAY1, "Foo", &low).expect(0);
    let codes: Vec<&str> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"backup_space_low"), "{envelope}");
    let left: BTreeSet<String> = fs::read_dir(retail.join("Interface/.omaddons/backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(!left.contains(&eligible.to_string()), "reclaimed");
    assert!(
        left.contains(&newest.to_string()),
        "the newest checkpoint stays"
    );
    assert!(left.contains(&manual.to_string()), "the user's stays");
    assert_eq!(left.len(), 2, "and no new one");
    let listed = run(&home, DAY1, &["backup", "list", "-t", "retail"], &[]).expect(0);
    assert_eq!(
        listed["data"]["targets"][0]["auto"]["last"]["result"],
        "space_low"
    );
}

#[test]
fn byte_copies_stay_within_their_budget() {
    let home = client("daily-copy");
    let retail = home.client("_retail_");
    // About 300 kB of settings; a 1 MiB budget leaves room for about 750 kB of copies.
    fs::write(support::settings(&home), vec![b'x'; 300_000]).unwrap();
    run(
        &home,
        DAY1,
        &["config", "set", "limits.backup_copy_max_mb", "1"],
        &[],
    )
    .expect(0);
    let copy = |captured: &str| {
        let mut m = meta(BackupKind::Auto, at(captured), 1);
        m.trigger = Some(Trigger::PostSession);
        m.reflinked = Reflinked::None;
        m.bytes = 400_000;
        m
    };
    let oldest = seed_meta(&retail, &copy("2026-10-10T08:00:00Z"));
    let older = seed_meta(&retail, &copy("2026-10-11T08:00:00Z"));
    let newest = seed_meta(&retail, &copy("2026-10-12T08:00:00Z"));
    install(&home, DAY1, "Foo", &[]).expect(0);
    let left: BTreeSet<String> = fs::read_dir(retail.join("Interface/.omaddons/backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(!left.contains(&oldest.to_string()), "oldest first");
    assert!(!left.contains(&older.to_string()), "until there is room");
    assert!(
        left.contains(&newest.to_string()),
        "the newest checkpoint stays"
    );
    assert_eq!(autos(&home).len(), 2, "and the new one is made");

    // A budget smaller than the tree itself: skipped.
    run(
        &home,
        DAY1,
        &["config", "set", "limits.backup_copy_max_mb", "0"],
        &[],
    )
    .expect(0);
    fs::write(support::settings(&home), vec![b'y'; 300_000]).unwrap();
    run(
        &home,
        DAY1,
        &["config", "set", "limits.backup_copy_max_mb", "1"],
        &[],
    )
    .expect(0);
    fs::write(support::settings(&home), vec![b'z'; 2_000_000]).unwrap();
    let envelope = run(
        &home,
        DAY2,
        &["backup", "create", "--auto", "-t", "retail"],
        &[],
    )
    .expect(0);
    assert_eq!(
        envelope["data"]["targets"][0]["skipped"],
        json!({"reason": "space_low"}),
        "{envelope}"
    );
}

#[test]
fn two_attempts_at_once_make_one_backup() {
    use common::pause::Pause;
    let home = client("daily-copy");
    install(&home, DAY1, "Foo", &[]).expect(0);
    fs::write(support::settings(&home), "Foo = { played = true }").unwrap();
    let pause = Pause::new(home.env.root());
    let args = ["backup", "create", "--auto", "-t", "retail"];
    let child = home.spawn(
        &args,
        &[
            ("OMADDONS_NOW", EVENING),
            ("OMADDONS_FS_FREE_MB", "8192"),
            ("OMADDONS_FAILPOINTS", "engine.autobackup=pause"),
            pause.env(),
        ],
    );
    pause.reached("engine.autobackup");
    let envelope = run(&home, EVENING, &args, &[]).expect(0);
    assert_eq!(
        envelope["data"]["targets"][0]["skipped"],
        json!({"reason": "in_progress"})
    );
    pause.resume("engine.autobackup");
    let first = common::home::wait(child, &args).expect(0);
    assert_eq!(
        first["data"]["targets"][0]["backup"]["trigger"],
        "post_session"
    );
    assert_eq!(autos(&home), ["pre_change", "post_session"]);
}

#[test]
fn daily_clones_only_and_says_what_it_observed() {
    let Some((_base, home)) = cloning("daily_clones_only_and_says_what_it_observed") else {
        return;
    };
    let clones = ("OMADDONS_FS_CLONE", "yes");
    // Told it can clone, but every clone fails: nothing copied, observed unsupported.
    install(
        &home,
        DAY1,
        "Foo",
        &[clones, ("OMADDONS_FAILPOINTS", "fsx.reflink.unsupported")],
    )
    .expect(0);
    let retail = home.client("_retail_");
    assert!(autos(&home).is_empty());
    assert!(
        fs::read_dir(retail.join("Interface/.omaddons/stage"))
            .map_or(true, |mut d| d.next().is_none()),
        "no stage left"
    );
    let listed = run(&home, DAY1, &["backup", "list", "-t", "retail"], &[clones]).expect(0);
    assert_eq!(
        listed["data"]["targets"][0]["auto"],
        json!({"mode": "daily", "capability": "unsupported", "last": listed["data"]["targets"][0]["auto"]["last"]})
    );
    assert_eq!(
        listed["data"]["targets"][0]["auto"]["last"]["result"],
        "no_reflink"
    );

    // Where clones work: one reflinked backup, observed supported.
    set_mode(&home, "daily-copy");
    fs::write(support::settings(&home), "Foo = { played = true }").unwrap();
    install(&home, DAY2, "Bar", &[clones]).expect(0);
    let listed = run(&home, DAY2, &["backup", "list", "-t", "retail"], &[clones]).expect(0);
    let t = &listed["data"]["targets"][0];
    assert_eq!(t["backups"][0]["reflinked"], "all", "{listed}");
    assert_eq!(
        (&t["auto"]["capability"], &t["auto"]["last"]["result"]),
        (&json!("supported"), &json!("created"))
    );
}
