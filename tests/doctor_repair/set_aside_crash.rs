//! F2 s6 (omad-m4-agent-surface-doctor-c6cm.6.2.6): the real set-aside (`doctor fix --only
//! <row> --force --yes`) and its real reactivation (`doctor undo <run>`), each killed
//! (`abort`) at every boundary on its path, then recovered by the next lock-taking
//! command. After each, the journal's whole set is in one state, never a mixture: the
//! repair's before when the crash preceded its durable `done`, its after from `done` on.
//! A reactivation never lets the folder pass see the suspended journal before the undo
//! has settled (E14's barrier). Recovery itself is crashed and repeated, and run by other
//! lock-taking commands and by the doctor's own delegation. An injected I/O failure rolls
//! back in process.
//!
//! Each case plants its own home (a journal names its home's absolute paths, so a copy
//! cannot stand in), and independent cases run on parallel threads. Every crash is checked
//! against the boundary it names: the repair journal's phase and the primitives its run
//! logged, read from what the crash left. `fsx.mutate.checked` is hit by untracked writes
//! too (the run's own directory, cleanup), so it is swept, not addressed, and a hit is
//! attributed to primitive `k` only when the journal holds step `k` and `k - 1` primitives
//! were logged.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use omaddons::engine::repair::RepairPhase;
use omaddons::engine::repair::journal::parse;

use super::set_aside::{
    STEP, UNEXPECTED, change_on, hashes, planted, row_of, set_aside_ok, stuck_state,
};
use super::*;

/// The most `fsx.mutate.checked` hits a sweep tries before it calls the path unbounded.
const CHECKED_MAX: usize = 64;

/// The most cases run at once.
const THREADS_MAX: usize = 8;

const PENDING: &str = "fm-concurrency_primitives-journal-pending";

type Hashes = BTreeMap<PathBuf, Option<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The crash preceded the repair's durable `done`: it rolls back.
    Before,
    /// From `done` on: it rolls forward.
    After,
}

/// The repair a matrix crashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    SetAside,
    Reactivate,
}

impl Op {
    /// The command's own failpoints before its repair journal.
    fn own(self) -> &'static [&'static str] {
        match self {
            Op::SetAside => &["doctor.fix.planned"],
            Op::Reactivate => &[],
        }
    }
}

/// The plan's name for a row.
fn label(fm: &str) -> &'static str {
    if fm == STEP { "row-6" } else { "row-5" }
}

// ---------------------------------------------------------------------------------------
// Boundaries.

/// Every addressed boundary of `op`'s repair of `n` primitives: the run's points and the
/// command's own, then the repair journal's `prepared`, each primitive's step line and
/// tracked fsx boundaries, and the journal's last three lines.
fn boundaries(n: usize, op: Op) -> Vec<String> {
    let mut out: Vec<String> = ["doctor.run.staged", "doctor.run.published"]
        .iter()
        .chain(op.own())
        .map(|p| format!("{p}=abort"))
        .collect();
    out.push("doctor.repair.prepared=abort".to_owned());
    for k in 1..=n {
        out.push(format!("doctor.repair.step=abort@{k}"));
        for boundary in ["backed_up", "executed", "logged"] {
            out.push(format!("fsx.mutate.{boundary}=abort@{k}"));
        }
    }
    for p in ["done", "history", "committed"] {
        out.push(format!("doctor.repair.{p}=abort"));
    }
    out
}

/// Every point a matrix must see fire: each boundary of [`boundaries`] and each
/// primitive's `checked`.
fn points(n: usize, op: Op) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = boundaries(n, op).iter().map(|s| point(s)).collect();
    out.extend((1..=n).map(|k| format!("checked@{k}")));
    out
}

/// A failpoint spec's name and its `@N`.
fn split(spec: &str) -> (&str, Option<usize>) {
    let (name, rest) = spec.split_once('=').unwrap_or((spec, ""));
    (name, rest.split_once('@').and_then(|(_, n)| n.parse().ok()))
}

/// A spec's point, as `crashes_exercised` names it: `executed@3`, `prepared`,
/// `run.staged`.
fn point(spec: &str) -> String {
    let (name, at) = split(spec);
    let short = name
        .strip_prefix("doctor.repair.")
        .or_else(|| name.strip_prefix("fsx.mutate."))
        .or_else(|| name.strip_prefix("doctor."))
        .unwrap_or(name);
    match at {
        Some(n) => format!("{short}@{n}"),
        None => short.to_owned(),
    }
}

/// The phase a crash at `spec` must leave; `None` for the swept `checked`, whose hit may
/// fall on either side.
fn expected(spec: &str) -> Option<Phase> {
    match split(spec).0 {
        "fsx.mutate.checked" => None,
        "doctor.repair.done" | "doctor.repair.history" | "doctor.repair.committed" => {
            Some(Phase::After)
        }
        _ => Some(Phase::Before),
    }
}

#[test]
fn boundaries_cover_every_point() {
    let mut want: Vec<String> = [
        "doctor.run.staged",
        "doctor.run.published",
        "doctor.fix.planned",
        "doctor.repair.prepared",
    ]
    .map(|p| format!("{p}=abort"))
    .into();
    for k in 1..=3 {
        want.push(format!("doctor.repair.step=abort@{k}"));
        want.push(format!("fsx.mutate.backed_up=abort@{k}"));
        want.push(format!("fsx.mutate.executed=abort@{k}"));
        want.push(format!("fsx.mutate.logged=abort@{k}"));
    }
    for p in ["done", "history", "committed"] {
        want.push(format!("doctor.repair.{p}=abort"));
    }
    assert_eq!(boundaries(3, Op::SetAside), want);
    let undo = boundaries(3, Op::Reactivate);
    assert_eq!(undo.len(), want.len() - 1);
    assert!(!undo.iter().any(|s| s.starts_with("doctor.fix.")));
    let points = points(3, Op::SetAside);
    for p in [
        "run.staged",
        "fix.planned",
        "prepared",
        "checked@3",
        "logged@3",
        "committed",
    ] {
        assert!(points.contains(p), "{p} in {points:?}");
    }
    assert_eq!(points.len(), want.len() + 3);
}

#[test]
fn phase_of_each_boundary() {
    for (spec, phase) in [
        ("doctor.run.staged=abort", Some(Phase::Before)),
        ("doctor.repair.prepared=abort", Some(Phase::Before)),
        ("doctor.repair.step=abort@3", Some(Phase::Before)),
        ("fsx.mutate.logged=abort@3", Some(Phase::Before)),
        ("fsx.mutate.checked=abort@9", None),
        ("doctor.repair.done=abort", Some(Phase::After)),
        ("doctor.repair.history=abort", Some(Phase::After)),
        ("doctor.repair.committed=abort", Some(Phase::After)),
    ] {
        assert_eq!(expected(spec), phase, "{spec}");
    }
    assert_eq!(point("fsx.mutate.executed=abort@3"), "executed@3");
    assert_eq!(point("doctor.run.published=abort"), "run.published");
}

// ---------------------------------------------------------------------------------------
// Running cases in parallel.

fn threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(THREADS_MAX)
}

/// `f` over `items` on up to [`threads`] threads, each its own home: the results in
/// order. The first panic stops the rest from starting and is re-raised.
fn par<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let panicked = Mutex::new(None);
    let out: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..threads().min(items.len()) {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    match catch_unwind(AssertUnwindSafe(|| f(item))) {
                        Ok(r) => out.lock().unwrap()[i] = Some(r),
                        Err(payload) => {
                            stop.store(true, Ordering::Relaxed);
                            panicked.lock().unwrap().get_or_insert(payload);
                        }
                    }
                }
            });
        }
    });
    if let Some(payload) = panicked.into_inner().unwrap() {
        resume_unwind(payload);
    }
    out.into_inner()
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect()
}

/// `fsx.mutate.checked=abort@N` for N = 1, 2, …, a batch of threads at a time, until a
/// case runs to its end without crashing. `case` says whether it crashed and what it
/// fired; the crashed cases' results.
fn sweep<R: Send>(case: impl Fn(&str) -> (bool, R) + Sync) -> Vec<R> {
    let mut out = Vec::new();
    let mut from = 1;
    while from <= CHECKED_MAX {
        let to = (from + threads()).min(CHECKED_MAX + 1);
        let batch: Vec<String> = (from..to)
            .map(|n| format!("fsx.mutate.checked=abort@{n}"))
            .collect();
        let mut ended = false;
        for (spec, (crashed, r)) in batch.iter().zip(par(&batch, |s| case(s))) {
            assert!(
                !(crashed && ended),
                "{spec} crashed after an earlier hit ran through"
            );
            if crashed {
                out.push(r);
            } else {
                ended = true;
            }
        }
        if ended {
            return out;
        }
        from = to;
    }
    panic!("a run still crashed at fsx.mutate.checked@{CHECKED_MAX}"); // ubs:ignore — fails the test
}

// ---------------------------------------------------------------------------------------
// What a crash left.

/// The txs of history's repair-family lines, oldest first: a repair's (`kind: repair`)
/// and a repair's undo (`kind: undo` with `repairs`), never an undo of an install.
fn repair_txs(home: &Home) -> Vec<String> {
    fs::read_to_string(home.state().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| {
            let repairs = v["repairs"].as_array().is_some_and(|r| !r.is_empty());
            // ubs:ignore — comparing history kinds, not secrets
            v["kind"] == "repair" || (v["kind"] == "undo" && repairs)
        })
        .map(|v| text(&v["tx"]).to_owned())
        .collect()
}

/// The repair journals in `<state>/journal/`: their txs.
fn repair_journals(home: &Home) -> Vec<String> {
    files_in(&home.state().join("journal"))
        .into_iter()
        .filter_map(|name| name.strip_suffix(".repair.jsonl").map(str::to_owned))
        .collect()
}

/// What was there before a command: the repair txs and the published runs.
struct Known {
    txs: BTreeSet<String>,
    runs: BTreeSet<String>,
}

fn known(home: &Home) -> Known {
    let mut txs: BTreeSet<String> = repair_txs(home).into_iter().collect();
    txs.extend(repair_journals(home));
    Known {
        txs,
        runs: plant::runs(home).into_iter().collect(),
    }
}

/// What a crashed command left of its own.
#[derive(Debug)]
struct Left {
    /// Its repair journal: the transaction and how far it got.
    repair: Option<(String, RepairPhase)>,
    /// The primitives its published run logged (`actions.jsonl`).
    actions: usize,
    /// Its repair line in history.
    history: Option<String>,
}

impl Left {
    fn read(home: &Home, known: &Known) -> Left {
        let journal = home.state().join("journal");
        let repair = repair_journals(home)
            .into_iter()
            .find(|tx| !known.txs.contains(tx))
            .map(|tx| {
                let text = fs::read_to_string(journal.join(format!("{tx}.repair.jsonl")))
                    .unwrap_or_default();
                let phase = parse(&text).phase;
                (tx, phase)
            });
        let run = plant::runs(home)
            .into_iter()
            .find(|r| !known.runs.contains(r));
        let actions = run.as_ref().map_or(0, |r| {
            let log = home
                .state()
                .join("doctor/runs")
                .join(r)
                .join("actions.jsonl");
            fs::read_to_string(log).map_or(0, |t| t.lines().count())
        });
        let history = repair_txs(home)
            .into_iter()
            .find(|tx| !known.txs.contains(tx));
        Left {
            repair,
            actions,
            history,
        }
    }

    fn phase(&self) -> Phase {
        match (&self.repair, &self.history) {
            (Some((_, RepairPhase::Done | RepairPhase::Committed)), _) | (None, Some(_)) => {
                Phase::After
            }
            _ => Phase::Before,
        }
    }

    /// Its transaction, from the journal or the history line.
    fn tx(&self) -> Option<String> {
        self.repair
            .as_ref()
            .map(|(tx, _)| tx.clone())
            .or_else(|| self.history.clone())
    }

    /// Whether this is where `spec` stops a repair. Any state is possible at a swept
    /// `checked` hit.
    fn at(&self, spec: &str) -> bool {
        let phase = self.repair.as_ref().map(|(_, p)| *p);
        // ubs:ignore — comparing step numbers, not secrets
        let stepped = |k: usize| matches!(phase, Some(RepairPhase::Stepped(s)) if s as usize == k);
        let done = matches!(phase, Some(RepairPhase::Done));
        match split(spec) {
            ("fsx.mutate.checked", _) => true,
            ("doctor.repair.prepared", None) => matches!(phase, Some(RepairPhase::Prepared)),
            ("doctor.repair.step" | "fsx.mutate.backed_up" | "fsx.mutate.executed", Some(k)) => {
                stepped(k) && self.actions + 1 == k
            }
            ("fsx.mutate.logged", Some(k)) => stepped(k) && self.actions == k,
            ("doctor.repair.done", None) => done && self.history.is_none(),
            ("doctor.repair.history", None) => done && self.history.is_some(),
            ("doctor.repair.committed", None) => matches!(phase, Some(RepairPhase::Committed)),
            _ => phase.is_none() && self.history.is_none(),
        }
    }

    /// The point a crash at `spec` fired: its own, or for a swept `checked` hit the
    /// primitive it fell on (none for an untracked write's).
    fn fired(&self, spec: &str) -> Option<String> {
        if split(spec).0 != "fsx.mutate.checked" {
            return Some(point(spec));
        }
        match &self.repair {
            Some((_, RepairPhase::Stepped(k))) if self.actions + 1 == *k as usize => {
                Some(format!("checked@{k}"))
            }
            _ => None,
        }
    }
}

/// A command killed at `spec`, and a reader after it.
struct Crashed {
    crashed: bool,
    left: Left,
}

impl Crashed {
    fn fired(&self, spec: &str) -> Option<String> {
        if self.crashed {
            self.left.fired(spec)
        } else {
            None
        }
    }
}

/// `argv` run with `spec` armed; what it left, checked against `spec`; then a reader,
/// which changes nothing and reports the pending repair it left.
fn crash(log: &TestLog, home: &Home, argv: &[&str], spec: &str) -> Crashed {
    let known = known(home);
    log.step(&format!("crash at {spec}"));
    log.note(format!("armed: OMADDONS_FAILPOINTS={spec}"));
    let ran = checked(cmd(log, home, argv).failpoints(spec).run());
    let crashed = ran.signal.is_some();
    if !crashed {
        ran.ok();
    }
    let left = Left::read(home, &known);
    log.note(format!(
        "{spec}: crashed {crashed}, exit {}, left {left:?}",
        ran.run.code
    ));
    if crashed {
        assert!(
            left.at(spec),
            "{spec} stops the repair where it names: {left:?}"
        );
        if let Some(phase) = expected(spec) {
            assert_eq!(left.phase(), phase, "{spec}: the phase it leaves");
        }
        // A committed journal only awaits its prune: nothing is pending.
        let pending = left
            .repair
            .as_ref()
            // ubs:ignore — comparing journal phases, not secrets
            .is_some_and(|(_, p)| *p != RepairPhase::Committed);
        reader_changes_nothing(log, home, pending);
    }
    Crashed { crashed, left }
}

/// The directories a reader must leave alone.
fn watched(home: &Home) -> Vec<(String, PathBuf)> {
    let state = home.state();
    let mut dirs: Vec<(String, PathBuf)> = ["journal", "inflight", "snapshots", "doctor/runs"]
        .iter()
        .map(|d| ((*d).to_owned(), state.join(d)))
        .collect();
    for client in ["_retail_", "_classic_"] {
        dirs.push((
            format!("{client} meta"),
            home.client(client).join("Interface/.omaddons"),
        ));
        dirs.push((format!("{client} AddOns"), home.addons(client)));
    }
    dirs.retain(|(_, dir)| dir.exists());
    dirs
}

fn reader_changes_nothing(log: &TestLog, home: &Home, pending: bool) {
    log.step("status changes nothing");
    let dirs = watched(home);
    for (label, dir) in &dirs {
        log.tree(label, dir);
    }
    let history = fs::read(home.state().join("history.jsonl")).ok();
    let ran = run(log, home, &["status"]);
    log.json("status warnings", &ran.envelope()["warnings"]);
    for (label, dir) in &dirs {
        log.assert_unchanged(label, dir);
    }
    assert_eq!(
        fs::read(home.state().join("history.jsonl")).ok(),
        history,
        "status appends nothing to history"
    );
    if pending {
        assert!(
            // ubs:ignore — comparing warning codes, not secrets
            ran.warnings().iter().any(|c| c == "journal_pending"),
            "status reports the pending repair: {:?}",
            ran.warnings()
        );
    }
}

// ---------------------------------------------------------------------------------------
// Recovering.

/// How a case recovers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum By {
    /// `config set check_interval_hours 6`.
    Config,
    /// An install on Classic.
    Install,
    /// `omaddons undo` of an earlier install on Classic.
    UndoEarlier,
    /// `config set` with `doctor.recover.step=abort@j`, then four plain ones.
    CrashedAt(usize),
    /// `doctor fix --only <journal-pending>`.
    Delegation,
}

impl By {
    fn label(self) -> String {
        match self {
            By::Config => "config set".to_owned(),
            By::Install => "an install on classic".to_owned(),
            By::UndoEarlier => "undo of an earlier install".to_owned(),
            By::CrashedAt(j) => format!("recovery crashed at recover.step@{j}"),
            By::Delegation => "the delegation".to_owned(),
        }
    }
}

const CONFIG: &[&str] = &["config", "set", "check_interval_hours", "6"];

/// Recovers as `by` says: each recovering command's envelope, in order (none for a
/// recovery that crashed).
fn recover(log: &TestLog, home: &Home, by: By, earlier: Option<&str>) -> Vec<Value> {
    log.step(&format!("recover by {}", by.label()));
    let ran = match by {
        By::Config => run(log, home, CONFIG),
        By::Install => change_on(log, home, "classic", "recovering"),
        By::UndoEarlier => run(log, home, &["undo", earlier.unwrap()]),
        By::Delegation => run(log, home, &["doctor", "fix", "--only", PENDING]),
        By::CrashedAt(j) => {
            let spec = format!("doctor.recover.step=abort@{j}");
            log.note(format!("armed: OMADDONS_FAILPOINTS={spec}"));
            let crashed = checked(cmd(log, home, CONFIG).failpoints(&spec).run());
            log.note(format!(
                "the recovery crashed: {}",
                crashed.signal.is_some()
            ));
            let mut out: Vec<Value> = (crashed.signal.is_none())
                .then(|| crashed.envelope().clone())
                .into_iter()
                .collect();
            for round in 1..=4 {
                log.step(&format!("recover again, round {round}"));
                let again = run(log, home, CONFIG);
                again.ok();
                out.push(again.envelope().clone());
            }
            return out;
        }
    };
    ran.ok();
    log.json("warnings", &ran.envelope()["warnings"]);
    vec![ran.envelope().clone()]
}

/// The warnings of `envelopes` with `code`.
fn warnings_of<'a>(envelopes: &'a [Value], code: &str) -> Vec<&'a Value> {
    envelopes
        .iter()
        .flat_map(|e| e["warnings"].as_array().into_iter().flatten())
        // ubs:ignore — comparing warning codes, not secrets
        .filter(|w| w["code"] == code)
        .collect()
}

/// Every warning, error and delegated item of `envelopes` naming `tx`.
fn naming<'a>(envelopes: &'a [Value], tx: &str) -> Vec<&'a Value> {
    envelopes
        .iter()
        .flat_map(|e| {
            let warnings = e["warnings"].as_array().into_iter().flatten();
            let delegated = e["delegated"].as_array().into_iter().flatten();
            warnings
                .chain(delegated)
                .chain(std::iter::once(&e["error"]))
        })
        .filter(|v| v.to_string().contains(tx))
        .collect()
}

/// The recovering commands reported the crashed repair recovered, once, in its
/// direction, when it left an unfinished journal (a committed one is only pruned). The
/// delegation reports it in `delegated` instead.
fn recovered_reported(log: &TestLog, envelopes: &[Value], left: &Left, by: By, context: &str) {
    let Some((tx, phase)) = &left.repair else {
        return;
    };
    if matches!(phase, RepairPhase::Committed) || matches!(by, By::Delegation) {
        return;
    }
    let reported: Vec<&Value> = warnings_of(envelopes, "journal_recovered")
        .into_iter()
        .filter(|w| text(&w["message"]).contains(tx.as_str()))
        .collect();
    log.json("journal_recovered", &reported);
    assert_eq!(reported.len(), 1, "{context}: its recovery reported once");
    let direction = match left.phase() {
        Phase::Before => "rolled back",
        Phase::After => "rolled forward",
    };
    assert!(
        text(&reported[0]["message"]).contains(direction),
        "{context}: {direction}: {}",
        reported[0]
    );
}

// ---------------------------------------------------------------------------------------
// The set-aside, crashed.

/// Where the set-aside put each artifact of `set`: `<run>/quarantine/state/` for a state
/// artifact, `<meta>/snapshots/<repair tx>.repair/quarantine/` for a client's, at its
/// path relative to `<state>/omaddons` or its client dir.
fn destinations(home: &Home, run: &str, repair: &str, set: &[PathBuf]) -> Vec<PathBuf> {
    let state = home.state();
    let quarantine = state.join("doctor/runs").join(run).join("quarantine/state");
    set.iter()
        .map(|path| match path.strip_prefix(&state) {
            Ok(rel) => quarantine.join(rel),
            Err(_) => {
                let client = home.client("_retail_");
                client
                    .join(format!(
                        "Interface/.omaddons/snapshots/{repair}.repair/quarantine"
                    ))
                    .join(path.strip_prefix(&client).unwrap())
            }
        })
        .collect()
}

/// The values of `found` in `paths`' order.
fn values(found: &Hashes, paths: &[PathBuf]) -> Vec<Option<String>> {
    paths.iter().map(|p| found[p].clone()).collect()
}

fn set_aside_argv(fm: &str) -> Vec<&str> {
    vec![
        "doctor", "fix", "--only", fm, "-t", "retail", "--force", "--yes",
    ]
}

/// A row planted for a set-aside case: the home, its set and the set's hashes, and the
/// earlier install on Classic when the case recovers by undoing it.
struct Planted {
    home: Home,
    set: Vec<PathBuf>,
    before: Hashes,
    earlier: Option<String>,
}

fn plant_set(log: &TestLog, fm: &str, by: By) -> Planted {
    let row = row_of(fm);
    let home = planted(log, &row);
    let earlier = matches!(by, By::UndoEarlier).then(|| {
        let ran = change_on(log, &home, "classic", "earlier");
        ran.ok();
        let tx = text(&ran.envelope()["data"]["tx"]).to_owned();
        assert!(
            !tx.is_empty(),
            "the earlier install's tx: {}",
            ran.envelope()
        );
        tx
    });
    let set = row.fixture.subject(&home);
    let before = hashes(&set);
    log.json("set", &set);
    Planted {
        home,
        set,
        before,
        earlier,
    }
}

/// The in-flight locks outside `set`: none, except that a repair whose crash left no
/// journal of its own (killed before its `prepared` line, or in cleanup after its
/// journal was pruned) leaves its own lock, held from planning to cleanup (§6.14.2), which
/// no recovery owns and the orphan sweep takes once it is old. That one lock, and nothing
/// else, may be new, as E14's oracle allows.
fn strays(log: &TestLog, home: &Home, set: &[PathBuf], left: Option<&Left>, context: &str) {
    let inflight = home.state().join("inflight");
    let strays: Vec<String> = files_in(&inflight)
        .into_iter()
        .filter(|n| !set.contains(&inflight.join(n)))
        .collect();
    let orphaned = left.is_some_and(|l| l.repair.is_none());
    if orphaned && strays.len() == 1 {
        log.note(format!(
            "{context}: {} is the crashed repair's own lock, the orphan sweep's",
            strays[0]
        ));
        return;
    }
    assert!(
        strays.is_empty(),
        "{context}: no lock but the set's: {strays:?}"
    );
}

/// The set-aside's whole before or whole after, then what follows from it: the same
/// stuck state, or a change on the client that succeeds. `left` is what a crash left
/// (none for an in-process rollback).
fn set_aside_oracle(
    log: &TestLog,
    p: &Planted,
    stuck: &(String, String),
    phase: Phase,
    left: Option<&Left>,
    context: &str,
) {
    log.step(&format!("oracle: the whole {phase:?}"));
    let home = &p.home;
    for (label, dir) in watched(home) {
        log.tree(&label, &dir);
    }
    assert_eq!(
        repair_journals(home),
        Vec::<String>::new(),
        "{context}: no repair journal left"
    );
    let lines = repair_txs(home);
    match phase {
        Phase::Before => {
            log.assert_eq(
                &format!("{context}: the set as planted"),
                &hashes(&p.set),
                &p.before,
            );
            assert!(lines.is_empty(), "{context}: no repair line: {lines:?}");
            strays(log, home, &p.set, left, context);
            log.assert_eq(
                &format!("{context}: stuck as before"),
                &stuck_state(log, home, "retail"),
                stuck,
            );
        }
        Phase::After => {
            assert_eq!(
                lines.len(),
                1,
                "{context}: the repair's line once: {lines:?}"
            );
            strays(log, home, &[], left, context);
            log.assert_eq(
                &format!("{context}: every source moved"),
                &values(&hashes(&p.set), &p.set),
                &vec![None; p.set.len()],
            );
            let runs = plant::runs(home);
            assert_eq!(runs.len(), 1, "{context}: the set-aside's run: {runs:?}");
            let to = destinations(home, &runs[0], &lines[0], &p.set);
            log.assert_eq(
                &format!("{context}: the set, whole, in quarantine"),
                &values(&hashes(&to), &to),
                &values(&p.before, &p.set),
            );
            change_on(log, home, "retail", "unblocked").ok();
        }
    }
}

/// One set-aside case: `fm` planted, its set-aside killed at `spec`, a reader, recovery
/// by `by`, the oracle. Whether it crashed, and the point it fired.
fn set_aside_case(
    log: &TestLog,
    fm: &'static str,
    stuck: &(String, String),
    spec: &str,
    by: By,
    name: &str,
) -> (bool, Option<String>) {
    let log = log.case(name);
    let p = plant_set(&log, fm, by);
    let c = crash(&log, &p.home, &set_aside_argv(fm), spec);
    if !c.crashed {
        return (false, None);
    }
    let context = format!("{} {} by {}", label(fm), point(spec), by.label());
    let envelopes = recover(&log, &p.home, by, p.earlier.as_deref());
    recovered_reported(&log, &envelopes, &c.left, by, &context);
    set_aside_oracle(&log, &p, stuck, c.left.phase(), Some(&c.left), &context);
    log.note(format!("{context}: whole {:?}", c.left.phase()));
    (true, c.fired(spec))
}

/// A row's facts from a reference home: its set's size and the stuck state it reports.
fn reference(log: &TestLog, fm: &str) -> (usize, (String, String)) {
    let log = log.case(&format!("{} reference", label(fm)));
    let row = row_of(fm);
    let home = planted(&log, &row);
    let n = row.fixture.subject(&home).len();
    let stuck = stuck_state(&log, &home, "retail");
    log.json("reference", &json!({"primitives": n, "stuck": stuck}));
    (n, stuck)
}

/// Each Before boundary at a primitive `k` with the number of inverses its recovery
/// runs: `k` once primitive `k` ran (`executed`, `logged`), `k - 1` before it.
fn inverse_cases(n: usize) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for k in 1..=n {
        for (boundary, inverses) in [
            ("doctor.repair.step", k - 1),
            ("fsx.mutate.backed_up", k - 1),
            ("fsx.mutate.executed", k),
            ("fsx.mutate.logged", k),
        ] {
            out.extend((1..=inverses).map(|j| (format!("{boundary}=abort@{k}"), j)));
        }
    }
    out
}

/// The set-aside of rows 6 and 5, killed at every boundary: each recovered state whole;
/// recovery crashed at each inverse and repeated; three lock-taking commands recovering
/// alike.
#[test]
fn set_aside_crash_at_every_boundary() {
    let log = tlog!();
    for fm in [STEP, UNEXPECTED] {
        let (n, stuck) = reference(&log, fm);
        let row = label(fm);

        let addressed = boundaries(n, Op::SetAside);
        let mut fired = BTreeSet::new();
        let results = par(&addressed, |spec| {
            set_aside_case(
                &log,
                fm,
                &stuck,
                spec,
                By::Config,
                &format!("{row} {}", point(spec)),
            )
        });
        for (spec, (crashed, point)) in addressed.iter().zip(results) {
            assert!(crashed, "{row}: crash point never hit: {spec}");
            fired.extend(point);
        }
        let swept = sweep(|spec| {
            let hit = split(spec).1.unwrap_or_default();
            set_aside_case(
                &log,
                fm,
                &stuck,
                spec,
                By::Config,
                &format!("{row} sweep checked hit {hit}"),
            )
        });
        log.note(format!("{row}: the sweep crashed {} times", swept.len()));
        fired.extend(swept.into_iter().flatten());
        log.json(&format!("{row} crashes_exercised"), &fired);
        let missing: Vec<String> = points(n, Op::SetAside)
            .difference(&fired)
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "{row}: crash point never hit: {missing:?}"
        );

        log.step(&format!(
            "{row}: recovery crashed at each inverse, then repeated"
        ));
        let inverses = inverse_cases(n);
        par(&inverses, |(spec, j)| {
            let name = format!("{row} {} recover.step@{j}", point(spec));
            let (crashed, _) = set_aside_case(&log, fm, &stuck, spec, By::CrashedAt(*j), &name);
            assert!(crashed, "{name}");
        });

        log.step(&format!("{row}: other lock-taking commands recover alike"));
        let others: Vec<(String, By)> = [
            "fsx.mutate.executed=abort@1".to_owned(),
            format!("fsx.mutate.executed=abort@{n}"),
            "doctor.repair.history=abort".to_owned(),
        ]
        .into_iter()
        .flat_map(|spec| [(spec.clone(), By::Install), (spec, By::UndoEarlier)])
        .collect();
        par(&others, |(spec, by)| {
            let name = format!("{row} {} by {}", point(spec), by.label());
            let (crashed, _) = set_aside_case(&log, fm, &stuck, spec, *by, &name);
            assert!(crashed, "{name}");
        });
    }
}

/// An I/O failure at each primitive of row 6's set-aside rolls it back in process.
#[test]
fn set_aside_io_failure_at_each_primitive() {
    let log = tlog!();
    let (n, stuck) = reference(&log, STEP);
    let cases: Vec<(String, usize)> = (1..=n)
        .flat_map(|k| {
            [
                (format!("fsx.mutate.backed_up=error@{k}"), k - 1),
                (format!("fsx.mutate.executed=error@{k}"), k),
            ]
        })
        .collect();
    par(&cases, |(spec, inverted)| {
        let log = log.case(&format!("row-6 {}", point(spec)));
        let p = plant_set(&log, STEP, By::Config);
        log.step(&format!("fail at {spec}"));
        let ran = checked(
            cmd(&log, &p.home, &set_aside_argv(STEP))
                .failpoints(spec)
                .run(),
        );
        ran.exit(3);
        let report = ran.envelope();
        log.json("summary", &report["summary"]);
        log.assert_eq("state", &report["state"], &json!("DONE_FAILED"));
        log.assert_eq(
            "renames put back",
            &report["summary"]["backups_restored"],
            &json!(inverted),
        );
        set_aside_oracle(&log, &p, &stuck, Phase::Before, None, spec);
    });
}

// ---------------------------------------------------------------------------------------
// The reactivation, crashed.

/// A committed set-aside of a row.
struct Suspended {
    home: Home,
    set: Vec<PathBuf>,
    original: Hashes,
    quarantine: Vec<PathBuf>,
    suspended: Vec<Option<String>>,
    /// The set-aside's run, which `doctor undo` names.
    run: String,
    /// The stuck journal's transaction.
    tx: String,
}

fn suspended(log: &TestLog, fm: &str) -> Suspended {
    let row = row_of(fm);
    let home = planted(log, &row);
    let set = row.fixture.subject(&home);
    let original = hashes(&set);
    let run = set_aside_ok(log, &home, &row);
    let lines = repair_txs(&home);
    assert_eq!(lines.len(), 1, "the set-aside's line: {lines:?}");
    let quarantine = destinations(&home, &run, &lines[0], &set);
    let suspended = values(&hashes(&quarantine), &quarantine);
    let tx = set
        .iter()
        .find_map(|p| {
            let name = p.file_name()?.to_str()?;
            let tx = name
                .strip_suffix(".wtf.jsonl")
                .or_else(|| name.strip_suffix(".jsonl"))?;
            Some(tx.to_owned())
        })
        .unwrap();
    log.json(
        "suspended",
        &json!({"tx": tx, "run": run, "quarantine": quarantine}),
    );
    Suspended {
        home,
        set,
        original,
        quarantine,
        suspended,
        run,
        tx,
    }
}

/// The reactivation's whole before (the set suspended, no journal of the transaction
/// under any name) or whole after (the set back, the journal under its own name). `left`
/// is what the crash left.
fn reactivation_whole(log: &TestLog, s: &Suspended, phase: Phase, left: &Left, context: &str) {
    let journal = s.home.state().join("journal");
    log.tree("journal", &journal);
    let back: &[PathBuf] = match phase {
        Phase::Before => &[],
        Phase::After => &s.set,
    };
    strays(log, &s.home, back, Some(left), context);
    assert_eq!(
        repair_journals(&s.home),
        Vec::<String>::new(),
        "{context}: no repair journal left"
    );
    let names = files_in(&journal);
    assert!(
        !names.iter().any(|n| n.ends_with(".reactivating.jsonl")),
        "{context}: no provisional journal: {names:?}"
    );
    let lines = repair_txs(&s.home);
    match phase {
        Phase::Before => {
            log.assert_eq(
                &format!("{context}: the set, suspended"),
                &values(&hashes(&s.quarantine), &s.quarantine),
                &s.suspended,
            );
            log.assert_eq(
                &format!("{context}: nothing at the set's paths"),
                &values(&hashes(&s.set), &s.set),
                &vec![None; s.set.len()],
            );
            assert!(
                !names.iter().any(|n| n.starts_with(&s.tx)),
                "{context}: no journal of {}: {names:?}",
                s.tx
            );
            assert_eq!(
                lines.len(),
                1,
                "{context}: the set-aside's line alone: {lines:?}"
            );
        }
        Phase::After => {
            log.assert_eq(
                &format!("{context}: the set, back"),
                &hashes(&s.set),
                &s.original,
            );
            assert_eq!(lines.len(), 2, "{context}: the undo's line: {lines:?}");
        }
    }
}

/// [`reactivation_whole`], then what follows from it: a Before undoes again to the whole
/// after; either way the journal then reports the stuck state it reported before.
fn reactivation_oracle(
    log: &TestLog,
    s: &Suspended,
    stuck: &(String, String),
    phase: Phase,
    left: &Left,
    context: &str,
) {
    log.step(&format!("oracle: the whole {phase:?}"));
    reactivation_whole(log, s, phase, left, context);
    if matches!(phase, Phase::Before) {
        log.step("undo again");
        run(log, &s.home, &["doctor", "undo", &s.run]).ok();
        let again = format!("{context}, undone again");
        reactivation_whole(log, s, Phase::After, left, &again);
    }
    log.assert_eq(
        &format!("{context}: stuck as before the set-aside"),
        &stuck_state(log, &s.home, "retail"),
        stuck,
    );
}

/// Once the undo rolled forward, the recovering command's folder pass meets the journal
/// under its own name again (`journal_pending` naming `tx`), after the undo's own
/// `journal_recovered` when there was one: the barrier's order. A folder journal is
/// reported with the stop reason it had before the set-aside. A `WTF/` journal waits
/// instead (`saved_variables`): `config set` writes no SavedVariables. Either way, the
/// oracle's next change on the client reports exactly the stop state from before.
fn folder_pass_after_the_undo(
    envelope: &Value,
    s: &Suspended,
    left: &Left,
    stuck: &(String, String),
    context: &str,
) {
    let tx = s.tx.as_str();
    let wtf = s
        .set
        .iter()
        .any(|p| p.to_string_lossy().ends_with(".wtf.jsonl"));
    let reason = if wtf {
        "saved_variables"
    } else {
        stuck.1.as_str()
    };
    let warnings: Vec<&Value> = envelope["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let reported = warnings.iter().position(|w| {
        let message = text(&w["message"]);
        // ubs:ignore — comparing warning codes, not secrets
        w["code"] == "journal_pending"
            && message.contains(tx)
            && message.contains(&format!("({reason})"))
    });
    let Some(reported) = reported else {
        panic!("{context}: the folder pass reports {tx} ({reason}): {warnings:?}"); // ubs:ignore — fails the test
    };
    let undo = left.tx().unwrap();
    let recovered = warnings
        .iter()
        // ubs:ignore — comparing warning codes, not secrets
        .position(|w| w["code"] == "journal_recovered" && text(&w["message"]).contains(&undo));
    if let Some(recovered) = recovered {
        assert!(
            recovered < reported,
            "{context}: the undo settles before the folder pass: {warnings:?}"
        );
    }
}

/// The plan's name for a reactivation boundary of `m` primitives (the activation last).
/// Keyed on the point fired (`logged@3`, `checked@6`), so a swept `checked` hit is named
/// by the primitive it fell on.
fn named(point: &str, m: usize) -> Option<&'static str> {
    let (boundary, k) = point.split_once('@')?;
    let k: usize = k.parse().ok()?;
    match boundary {
        "logged" if k < m => Some("after_each_artifact_restore"),
        "step" | "checked" | "backed_up" if k == m => Some("immediately_before_the_activation"),
        "executed" | "logged" if k == m => Some("between_the_activation_and_done"),
        _ => None,
    }
}

/// Where a named case's crash left the suspended journal: under its provisional name
/// (`<tx>….reactivating.jsonl`) after each restore and immediately before the
/// activation, under its own name from the activation on (E14's rule 3: provisional
/// first, every other artifact, the activation last).
fn journal_where_named(log: &TestLog, s: &Suspended, point: &str, m: usize, context: &str) {
    let Some(name) = named(point, m) else {
        return;
    };
    let journal = s.home.state().join("journal");
    let names = files_in(&journal);
    let provisional = names
        .iter()
        .any(|n| n.starts_with(&s.tx) && n.contains(".reactivating."));
    let own = s.set.iter().any(|p| p.starts_with(&journal) && p.exists());
    log.json(&format!("{name}: the journal dir"), &names);
    let want = match name {
        "between_the_activation_and_done" => (false, true),
        _ => (true, false),
    };
    assert_eq!(
        (provisional, own),
        want,
        "{context} ({name}): (provisional, own name): {names:?}"
    );
}

/// One reactivation case: `fm` set aside, its undo killed at `spec`, a reader, recovery
/// by `by`, the barrier and the oracle. Whether it crashed, and the point it fired.
fn reactivation_case(
    log: &TestLog,
    fm: &'static str,
    stuck: &(String, String),
    spec: &str,
    by: By,
    name: &str,
) -> (bool, Option<String>) {
    let log = log.case(name);
    let s = suspended(&log, fm);
    let c = crash(&log, &s.home, &["doctor", "undo", &s.run], spec);
    if !c.crashed {
        return (false, None);
    }
    let phase = c.left.phase();
    let context = format!("{} {} by {}", label(fm), point(spec), by.label());
    if let Some(fired) = c.fired(spec) {
        journal_where_named(&log, &s, &fired, s.set.len() + 1, &context);
    }
    let envelopes = recover(&log, &s.home, by, None);
    recovered_reported(&log, &envelopes, &c.left, by, &context);
    if matches!(by, By::Delegation) {
        let undo = c.left.tx().unwrap();
        log.assert_eq(
            &format!("{context}: the undo's repair, rolled back"),
            &envelopes[0]["delegated"],
            &json!([{"fm": PENDING, "tx": undo, "family": "repair", "outcome": "rolled_back"}]),
        );
    }
    match phase {
        Phase::Before => {
            let seen = naming(&envelopes, &s.tx);
            assert!(
                seen.is_empty(),
                "{context}: the suspended journal is never seen before the undo settles: {seen:?}"
            );
        }
        Phase::After => folder_pass_after_the_undo(&envelopes[0], &s, &c.left, stuck, &context),
    }
    reactivation_oracle(&log, &s, stuck, phase, &c.left, &context);
    (true, c.fired(spec))
}

/// The reactivation of rows 6 and 5, killed at every boundary: the barrier holds, and
/// each recovered state is whole; the three named cases with recovery crashed at each
/// inverse and repeated, and the one between the activation and `done` recovered by the
/// delegation.
#[test]
fn reactivate_crash_at_every_boundary() {
    let log = tlog!();
    for fm in [STEP, UNEXPECTED] {
        let (n, stuck) = reference(&log, fm);
        let (row, m) = (label(fm), n + 1);
        let name = |spec: &str| match named(&point(spec), m) {
            Some(named) => format!("{row} {named} {}", point(spec)),
            None => format!("{row} {}", point(spec)),
        };

        let addressed = boundaries(m, Op::Reactivate);
        let mut fired = BTreeSet::new();
        let results = par(&addressed, |spec| {
            reactivation_case(&log, fm, &stuck, spec, By::Config, &name(spec))
        });
        for (spec, (crashed, point)) in addressed.iter().zip(results) {
            assert!(crashed, "{row}: crash point never hit: {spec}");
            fired.extend(point);
        }
        let swept = sweep(|spec| {
            let hit = split(spec).1.unwrap_or_default();
            reactivation_case(
                &log,
                fm,
                &stuck,
                spec,
                By::Config,
                &format!("{row} sweep checked hit {hit}"),
            )
        });
        fired.extend(swept.into_iter().flatten());
        log.json(&format!("{row} crashes_exercised"), &fired);
        let missing: Vec<String> = points(m, Op::Reactivate)
            .difference(&fired)
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "{row}: crash point never hit: {missing:?}"
        );
        for (named_case, wanted) in [
            (
                "after_each_artifact_restore",
                (1..m).map(|k| format!("logged@{k}")).collect::<Vec<_>>(),
            ),
            (
                "immediately_before_the_activation",
                vec![format!("step@{m}"), format!("checked@{m}")],
            ),
            (
                "between_the_activation_and_done",
                vec![format!("executed@{m}"), format!("logged@{m}")],
            ),
        ] {
            let absent: Vec<&String> = wanted.iter().filter(|p| !fired.contains(*p)).collect();
            assert!(
                absent.is_empty(),
                "{row} {named_case}: never hit {absent:?}"
            );
        }

        log.step(&format!(
            "{row}: the named cases, recovery crashed at each inverse"
        ));
        let crashed_recoveries: Vec<(String, usize)> = [
            (format!("fsx.mutate.logged=abort@{n}"), n),
            (format!("doctor.repair.step=abort@{m}"), n),
            (format!("fsx.mutate.executed=abort@{m}"), m),
        ]
        .into_iter()
        .flat_map(|(spec, inverses)| (1..=inverses).map(move |j| (spec.clone(), j)))
        .collect();
        par(&crashed_recoveries, |(spec, j)| {
            let case = format!("{} recover.step@{j}", name(spec));
            let (crashed, _) = reactivation_case(&log, fm, &stuck, spec, By::CrashedAt(*j), &case);
            assert!(crashed, "{case}");
        });

        log.step(&format!(
            "{row}: between the activation and done, by the delegation"
        ));
        let spec = format!("fsx.mutate.executed=abort@{m}");
        let case = format!("{} recovered_by_the_delegation", name(&spec));
        let (crashed, _) = reactivation_case(&log, fm, &stuck, &spec, By::Delegation, &case);
        assert!(crashed, "{case}");
    }
}
