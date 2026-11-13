//! e2e suite: two processes never lose or revive ledger evidence
//!
//! The errors ledger's store (design 7.1.6) between real processes. Each scenario runs
//! the test binary as two child processes (`child_session`) over one synthetic cache.
//! Each child looks first (it loads the ledger for display, as a command does before it
//! reads BugGrabber's files), then opens a ledger session, applies what it saw as input,
//! and commits. The children are held at barriers: their own before `open` (they have
//! looked and wait to record), or the store's failpoints `errors.ledger.loaded` and
//! `errors.ledger.store`, with the lock held. No command records evidence yet (`errors`
//! and the light read before a change will), so the children drive the library the way
//! those commands will.
#![cfg(test)]
#![cfg(debug_assertions)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use omaddons::errors::buggrabber::{TimeEvidence, TimeMeaning, TzEvidence};
use omaddons::errors::ledger::{
    ChangeMark, ClientNow, DbRead, DbSighting, EntrySighting, ErrorsLedger, FamilyFirst,
    LedgerUnavailable, Restored, SightMode, load, open,
};
use omaddons::errors::observe::Coverage;
use omaddons::errors::suspect::{JudgmentGap, JudgmentState, Verdict};
use omaddons::fsx::ScopeTable;
use omaddons::limits::{LEDGER_LOCK_TIMEOUT, SUSPECT_ALGORITHM};
use omaddons::state::paths::Paths;
use serde::{Deserialize, Serialize};

use crate::common;
use common::pause::Pause;
use common::tlog::tlog;

/// The child's root, and its name (its script and outcome files are named after it).
const ROOT_ENV: &str = "OMADDONS_LEDGER_TEST_ROOT";
const NAME_ENV: &str = "OMADDONS_LEDGER_TEST_NAME";
/// The client every scenario records, but the first.
const TARGET: &str = "k";
const DB: &str = "d:a";

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + seconds).unwrap()
}

fn paths_in(root: &Path) -> Paths {
    Paths::with_roots(
        root.join("home"),
        root.join("config"),
        root.join("state"),
        root.join("cache"),
    )
}

/// One proved read a child saw: database `key` of client `target` in state `stamp`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Seen {
    target: String,
    key: String,
    stamp: String,
    written: i64,
    observed: i64,
    session: Option<u32>,
    families: Vec<String>,
}

impl Seen {
    fn new(stamp: &str, written: i64, observed: i64, session: u32, families: &[&str]) -> Self {
        Self {
            target: TARGET.to_owned(),
            key: DB.to_owned(),
            stamp: stamp.to_owned(),
            written,
            observed,
            session: Some(session),
            families: families.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    fn sighting(&self) -> DbSighting {
        let entries = self
            .families
            .iter()
            .map(|family| EntrySighting {
                family: family.clone(),
                fingerprint: format!("e:{}", family.trim_start_matches("a:")),
                session: self.session,
                time: TimeEvidence {
                    at: None,
                    meaning: TimeMeaning::Unknown,
                    tz: TzEvidence::SystemAssumed,
                },
                counter: 1,
            })
            .collect();
        DbSighting {
            key: self.key.clone(),
            provider: "!BugGrabber".to_owned(),
            stamp: self.stamp.clone(),
            written_at: Some(at(self.written)),
            observed_at: at(self.observed),
            session: self.session,
            read: Some(DbRead {
                coverage: Coverage::Proved,
                entries,
            }),
        }
    }
}

/// What a child does.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Script {
    /// A barrier of the child's own, before `open`: it has looked, and waits to record.
    before_open: Option<String>,
    sightings: Vec<Seen>,
    /// Whole `WTF/` restores at these times, read from history before the session.
    restores: Vec<i64>,
    light: bool,
    now: i64,
    /// Change marks: the stamp it saw before its change, and its run. The generation is
    /// the one its first look found.
    marks: Vec<(String, String)>,
    /// A complete verdict for the cluster this family had at its first look.
    attach: Option<String>,
}

/// What a child came to.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Outcome {
    /// Why it recorded nothing (`LedgerUnavailable`, as debug text).
    refused: Option<String>,
    waited_ms: u64,
    committed: bool,
    revision: Option<u64>,
    marks_added: Vec<bool>,
    attached: Option<bool>,
}

/// One child of a scenario: its script and outcome live in the scenario's root.
struct Run {
    child: Child,
    root: PathBuf,
    name: String,
}

fn spawn(root: &Path, name: &str, script: &Script, pause: &Pause, failpoints: &str) -> Run {
    fs::write(
        root.join(format!("{name}.script.json")),
        serde_json::to_vec(script).unwrap(),
    )
    .unwrap();
    let (dir_var, dir) = pause.env();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "errors_ledger_procs::child_session",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROOT_ENV, root)
        .env(NAME_ENV, name)
        .env(dir_var, dir)
        .env("OMADDONS_FAILPOINTS", failpoints)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    Run {
        child,
        root: root.to_owned(),
        name: name.to_owned(),
    }
}

impl Run {
    /// Waits for the child, which must succeed, and reads what it came to.
    fn finish(self) -> Outcome {
        let output = self.child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "child {} failed: {}",
            self.name,
            String::from_utf8_lossy(&output.stderr)
        );
        let outcome = self.root.join(format!("{}.outcome.json", self.name));
        serde_json::from_slice(&fs::read(outcome).unwrap()).unwrap()
    }
}

/// The child's own barrier, by the failpoints' pause protocol: `<name>.reached`, then
/// waiting for `<name>.resume`.
fn barrier(name: &str) {
    let dir = PathBuf::from(std::env::var("OMADDONS_FAILPOINT_DIR").unwrap());
    fs::write(
        dir.join(format!("{name}.reached")),
        std::process::id().to_string(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !dir.join(format!("{name}.resume")).exists() {
        assert!(Instant::now() < deadline, "never resumed from {name}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "child process for the errors ledger's process tests"]
fn child_session() {
    let Some(root) = std::env::var_os(ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    let name = std::env::var(NAME_ENV).unwrap();
    let script: Script =
        serde_json::from_slice(&fs::read(root.join(format!("{name}.script.json"))).unwrap())
            .unwrap();
    let paths = paths_in(&root);
    let scopes = ScopeTable::new(&paths, &[], None).unwrap();
    // The first look, before the lock: what the process will hold as input.
    let first_look = load(&paths).unwrap_or_default();
    let looked_at = first_look.targets.get(TARGET);
    let generation = looked_at
        .and_then(|t| t.databases.get(DB))
        .map(|db| db.generation.clone());
    let cluster =
        script
            .attach
            .as_ref()
            .and_then(|family| match &looked_at?.families.get(family)?.first {
                FamilyFirst::Observed { cluster } => Some(cluster.clone()),
                _ => None,
            });
    if let Some(name) = &script.before_open {
        barrier(name);
    }
    let started = Instant::now();
    let opened = open(&paths, &scopes, "child");
    let mut outcome = Outcome {
        waited_ms: u64::try_from(started.elapsed().as_millis()).unwrap(),
        ..Outcome::default()
    };
    match opened {
        Err(refused) => outcome.refused = Some(format!("{refused:?}")),
        Ok(mut session) => {
            let client = ClientNow {
                version: Some("12.1.5.70391".to_owned()),
                now: at(script.now),
            };
            let restores: Vec<Restored> = script
                .restores
                .iter()
                .map(|&t| Restored {
                    at: at(t),
                    whole: true,
                    providers: Vec::new(),
                })
                .collect();
            let mode = if script.light {
                SightMode::Light
            } else {
                SightMode::Full
            };
            let dbs: Vec<DbSighting> = script.sightings.iter().map(Seen::sighting).collect();
            let target = script
                .sightings
                .first()
                .map_or(TARGET, |seen| seen.target.as_str());
            session.sight(target, &dbs, &restores, &client, true, mode);
            for (stamp, run) in &script.marks {
                let added = generation.as_ref().is_some_and(|generation| {
                    session.add_mark(
                        TARGET,
                        DB,
                        ChangeMark {
                            run: run.parse().unwrap(),
                            at: at(script.now),
                            generation: generation.clone(),
                            stamp: stamp.clone(),
                            session: None,
                            running: Some(false),
                            settled: true,
                        },
                    )
                });
                outcome.marks_added.push(added);
            }
            if let Some(cluster) = &cluster {
                let verdict = Verdict {
                    algorithm: SUSPECT_ALGORITHM,
                    state: JudgmentState::Complete,
                    judged_at: at(script.now),
                    suspect: None,
                    gap: None,
                };
                outcome.attached = Some(session.attach(TARGET, cluster, verdict, None));
            }
            match session.commit() {
                Ok(accepted) => {
                    outcome.committed = true;
                    outcome.revision = Some(accepted.ledger.revision);
                }
                Err(refused) => outcome.refused = Some(format!("{refused:?}")),
            }
        }
    }
    fs::write(
        root.join(format!("{name}.outcome.json")),
        serde_json::to_vec(&outcome).unwrap(),
    )
    .unwrap();
}

/// A scenario's root, with `seen` already recorded, one session each, in this process.
fn seeded(seen: &[Seen]) -> (tempfile::TempDir, Pause) {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let scopes = ScopeTable::new(&paths, &[], None).unwrap();
    for s in seen {
        let mut session = open(&paths, &scopes, "seed").unwrap();
        let client = ClientNow {
            version: Some("12.1.5.70391".to_owned()),
            now: at(s.observed),
        };
        session.sight(
            &s.target,
            &[s.sighting()],
            &[],
            &client,
            true,
            SightMode::Full,
        );
        session.commit().unwrap();
    }
    let pause = Pause::new(dir.path());
    (dir, pause)
}

fn ledger(root: &Path) -> ErrorsLedger {
    load(&paths_in(root)).unwrap()
}

fn script(before_open: Option<&str>, sightings: Vec<Seen>, now: i64) -> Script {
    Script {
        before_open: before_open.map(str::to_owned),
        sightings,
        now,
        ..Script::default()
    }
}

#[test]
fn two_processes_recording_two_clients_keep_both() {
    let log = tlog!();
    log.step("both look at an empty ledger, then record one after the other");
    let (dir, pause) = seeded(&[]);
    let root = dir.path();
    let client = |target: &str, key: &str, family: &str| Seen {
        target: target.to_owned(),
        key: key.to_owned(),
        ..Seen::new("s1", 100, 110, 3, &[family])
    };
    let x = spawn(
        root,
        "x",
        &script(Some("x.ready"), vec![client("x", "d:x", "a:x")], 110),
        &pause,
        "",
    );
    pause.reached("x.ready");
    let y = spawn(
        root,
        "y",
        &script(Some("y.ready"), vec![client("y", "d:y", "a:y")], 110),
        &pause,
        "",
    );
    pause.reached("y.ready");
    pause.resume("x.ready");
    let first = x.finish();
    pause.resume("y.ready");
    let second = y.finish();
    log.json("outcomes", &(&first, &second));
    let after = ledger(root);
    log.json("ledger", &after);
    log.assert_eq(
        "clients recorded",
        &after.targets.keys().cloned().collect::<Vec<_>>(),
        &vec!["x".to_owned(), "y".to_owned()],
    );
    log.assert_eq("revision", &after.revision, &2);
}

#[test]
fn an_older_state_never_becomes_the_newest_whichever_commits_first() {
    let log = tlog!();
    let older = Seen::new("s1", 150, 160, 4, &["a:1"]);
    let newer = Seen::new("s2", 250, 260, 5, &["a:1"]);
    let mut results = Vec::new();
    for (order, first) in [("older first", "old"), ("newer first", "new")] {
        log.step(order);
        let (dir, pause) = seeded(&[Seen::new("s0", 50, 60, 3, &["a:1"])]);
        let root = dir.path();
        let old = spawn(
            root,
            "old",
            &script(Some("old.ready"), vec![older.clone()], 160),
            &pause,
            "",
        );
        pause.reached("old.ready");
        let new = spawn(
            root,
            "new",
            &script(Some("new.ready"), vec![newer.clone()], 260),
            &pause,
            "",
        );
        pause.reached("new.ready");
        let (a, b) = if first == "old" {
            (old, new)
        } else {
            (new, old)
        };
        let a_name = a.name.clone();
        pause.resume(&format!("{a_name}.ready"));
        let a_outcome = a.finish();
        pause.resume(&format!("{}.ready", b.name));
        let b_outcome = b.finish();
        log.json("outcomes", &(&a_outcome, &b_outcome));
        let after = ledger(root);
        let db = &after.targets[TARGET].databases[DB];
        log.assert_eq(
            &format!("{order}: the newest read"),
            &db.reads.last().map(|r| r.stamp.clone()),
            &Some("s2".to_owned()),
        );
        results.push(after);
    }
    log.step("one knowledge either way: families, clusters, generation and newest state");
    let knowledge = |l: &ErrorsLedger| {
        let t = &l.targets[TARGET];
        let db = &t.databases[DB];
        (
            t.families.keys().cloned().collect::<Vec<_>>(),
            t.clusters.clone(),
            db.generation.clone(),
            db.reads.last().cloned(),
        )
    };
    log.assert_eq(
        "the same knowledge",
        &knowledge(&results[0]),
        &knowledge(&results[1]),
    );
}

#[test]
fn a_reset_committed_first_keeps_no_read_or_mark_of_the_lifetime_it_ended() {
    let log = tlog!();
    log.step("generation 1: s1, then s2");
    let (dir, pause) = seeded(&[
        Seen::new("s1", 50, 60, 5, &["a:1"]),
        Seen::new("s2", 150, 160, 6, &["a:1"]),
    ]);
    let root = dir.path();
    let g1 = ledger(root).targets[TARGET].databases[DB]
        .generation
        .clone();
    log.step("an older writer has looked at s3 and changed something; a reset commits first");
    let mut older = script(
        Some("old.ready"),
        vec![Seen::new("s3", 290, 300, 7, &["a:1"])],
        300,
    );
    older.marks = vec![("s3".to_owned(), "01M51EX04HHWM46XE9F2B9JXED".to_owned())];
    let old = spawn(root, "old", &older, &pause, "");
    pause.reached("old.ready");
    let reset = spawn(
        root,
        "reset",
        &script(None, vec![Seen::new("s4", 400, 410, 1, &["a:1"])], 410),
        &pause,
        "",
    );
    let reset_outcome = reset.finish();
    pause.resume("old.ready");
    let old_outcome = old.finish();
    log.json("outcomes", &(&reset_outcome, &old_outcome));
    let after = ledger(root);
    log.json("ledger", &after);
    let db = &after.targets[TARGET].databases[DB];
    assert_ne!(db.generation, g1, "the reset started a generation");
    log.assert_eq(
        "the reads",
        &db.reads.iter().map(|r| r.stamp.clone()).collect::<Vec<_>>(),
        &vec!["s4".to_owned()],
    );
    log.assert_eq("the marks", &db.marks.len(), &0);
    log.assert_eq(
        "the older mark was refused",
        &old_outcome.marks_added,
        &vec![false],
    );
}

#[test]
fn a_verdict_for_an_ended_generation_is_never_attached() {
    let log = tlog!();
    log.step("generation 1 watched a:2 appear");
    let (dir, pause) = seeded(&[
        Seen::new("s1", 50, 60, 5, &["a:1"]),
        Seen::new("s2", 150, 160, 6, &["a:1", "a:2"]),
    ]);
    let root = dir.path();
    let id = match &ledger(root).targets[TARGET].families["a:2"].first {
        FamilyFirst::Observed { cluster } => cluster.clone(),
        other => panic!("a:2 is {other:?}"),
    };
    log.step("a judge computed a verdict for it; generation 2 begins first");
    let mut judge = script(Some("judge.ready"), Vec::new(), 300);
    judge.attach = Some("a:2".to_owned());
    let judging = spawn(root, "judge", &judge, &pause, "");
    pause.reached("judge.ready");
    let reset = spawn(
        root,
        "reset",
        &script(
            None,
            vec![Seen::new("s3", 250, 260, 1, &["a:1", "a:2"])],
            260,
        ),
        &pause,
        "",
    );
    reset.finish();
    pause.resume("judge.ready");
    let judged = judging.finish();
    log.json("judge", &judged);
    let after = ledger(root);
    log.json("ledger", &after);
    log.assert_eq("attached", &judged.attached, &Some(false));
    log.assert_eq(
        "the cluster's judgment",
        &after.targets[TARGET].clusters[&id].judgment,
        &Some(Verdict {
            algorithm: SUSPECT_ALGORITHM,
            state: JudgmentState::Unsupported,
            judged_at: at(260),
            suspect: None,
            gap: Some(JudgmentGap::GenerationEnded),
        }),
    );
    let complete = after.targets[TARGET]
        .clusters
        .values()
        .filter(|c| {
            c.judgment
                .as_ref()
                .is_some_and(|v| v.state == JudgmentState::Complete)
        })
        .count();
    log.assert_eq("clusters with a complete verdict", &complete, &0);
}

#[test]
fn a_newer_file_that_appears_before_open_is_left_alone() {
    let log = tlog!();
    let (dir, pause) = seeded(&[]);
    let root = dir.path();
    let run = spawn(
        root,
        "late",
        &script(
            Some("late.ready"),
            vec![Seen::new("s1", 100, 110, 3, &["a:1"])],
            110,
        ),
        &pause,
        "",
    );
    pause.reached("late.ready");
    log.step("a newer omaddons writes the ledger between the look and the open");
    let paths = paths_in(root);
    fs::create_dir_all(paths.cache_dir()).unwrap();
    let newer = r#"{"schema_version":"9.0","targets":"a newer shape"}"#;
    fs::write(paths.errors_ledger(), newer).unwrap();
    pause.resume("late.ready");
    let outcome = run.finish();
    log.json("outcome", &outcome);
    log.assert_eq(
        "refused",
        &outcome.refused,
        &Some(format!("{:?}", LedgerUnavailable::Newer)),
    );
    log.assert_eq(
        "the file",
        &fs::read_to_string(paths.errors_ledger()).unwrap(),
        &newer.to_owned(),
    );
}

#[test]
fn a_lock_held_past_its_timeout_costs_the_other_process_its_evidence_and_nothing_else() {
    let log = tlog!();
    for failpoint in ["errors.ledger.loaded", "errors.ledger.store"] {
        log.step(&format!("one process holds the lock at {failpoint}"));
        let (dir, pause) = seeded(&[]);
        let root = dir.path();
        let holder = spawn(
            root,
            "holder",
            &script(None, vec![Seen::new("s1", 100, 110, 3, &["a:1"])], 110),
            &pause,
            &format!("{failpoint}=pause"),
        );
        pause.reached(failpoint);
        let other = Seen {
            target: "other".to_owned(),
            ..Seen::new("t1", 100, 120, 3, &["a:9"])
        };
        let waiting = spawn(root, "other", &script(None, vec![other], 120), &pause, "").finish();
        log.json("the other process", &waiting);
        log.assert_eq(
            "refused",
            &waiting.refused,
            &Some(format!("{:?}", LedgerUnavailable::Busy)),
        );
        let timeout = u64::try_from(LEDGER_LOCK_TIMEOUT.as_millis()).unwrap();
        assert!(
            (timeout..timeout + 1_500).contains(&waiting.waited_ms),
            "waited {} ms",
            waiting.waited_ms
        );
        pause.resume(failpoint);
        let held = holder.finish();
        log.assert_eq("the holder committed", &held.committed, &true);
        let after = ledger(root);
        log.assert_eq(
            "clients recorded",
            &after.targets.keys().cloned().collect::<Vec<_>>(),
            &vec![TARGET.to_owned()],
        );
    }
}

#[test]
fn a_slow_reader_of_the_file_before_a_restore_cannot_start_the_generation_from_it() {
    let log = tlog!();
    log.step("the ledger's newest read is s1");
    let (dir, pause) = seeded(&[Seen::new("s1", 50, 60, 5, &["a:1"])]);
    let root = dir.path();
    let before = ledger(root);
    let g1 = before.targets[TARGET].databases[DB].generation.clone();
    log.step("a slow process read s1 at 150; a restore commits at 160 and is recorded first");
    let slow = spawn(
        root,
        "slow",
        &script(
            Some("slow.ready"),
            vec![Seen::new("s1", 50, 150, 5, &["a:1"])],
            150,
        ),
        &pause,
        "",
    );
    pause.reached("slow.ready");
    let mut restore = script(None, Vec::new(), 165);
    restore.restores = vec![160];
    restore.light = true;
    restore.sightings = Vec::new();
    spawn(root, "restore", &restore, &pause, "").finish();
    pause.resume("slow.ready");
    let slow_outcome = slow.finish();
    log.json("the slow process", &slow_outcome);
    let after = ledger(root);
    log.json("ledger", &after);
    let db = &after.targets[TARGET].databases[DB];
    log.assert_eq("the generation", &db.generation, &g1);
    log.assert_eq("invalidated_at", &db.invalidated_at, &Some(at(160)));
    log.assert_eq(
        "the reads",
        &db.reads,
        &before.targets[TARGET].databases[DB].reads,
    );
    log.step("the next read of the restored file starts the generation");
    let next = spawn(
        root,
        "next",
        &script(None, vec![Seen::new("s9", 170, 180, 5, &["a:1"])], 180),
        &pause,
        "",
    );
    next.finish();
    let restored = ledger(root);
    let db = &restored.targets[TARGET].databases[DB];
    assert_ne!(db.generation, g1);
    log.assert_eq("invalidated_at", &db.invalidated_at, &None);
}
