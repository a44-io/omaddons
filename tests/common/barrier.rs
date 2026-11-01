//! E14's barrier scene (omad-m4-agent-surface-doctor-c6cm.5.14.5): a real folder journal F,
//! left on `_retail_` by an update of `foo` killed mid-swap (or after `swapped`), set aside
//! by a real repair (F2's suspension, lowered by E5 from `Fix::Suspend`) and reactivated by
//! another (E7's undo, built from E5's primitives as M4-D38 orders it), each committed by
//! E5's `commit` in a child of the test binary with failpoints armed
//! (`common::repairs::run_child`). F2 and E7 land after this scene; they reuse it for their
//! own fixers' crash cases.
//!
//! What a case compares is [`Trees`]: each tree's content hash (`trees::tree_hash`: paths,
//! modes and bytes, never inodes or times) or `absent`, and the history's transactions.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::archives::zip_text;
use super::engine::{BAR, V1, V2};
use super::home::Home;
use super::privacy::{ACCOUNT, CHARACTER};
use super::repairs::{self, Child, FixSpec, Root};
use super::tlog::TestLog;
use super::trees::tree_hash;

pub const RETAIL: &str = "_retail_";
pub const ERA: &str = "_classic_era_";
/// An addon for the Classic Era client, for a command that commits on the other client.
const BAZ: &[(&str, &str)] = &[("Baz/Baz.toc", "## Interface: 11507\n## Version: 3\n")];

/// Where the update that leaves F was killed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderCrash {
    /// After its first exchange (`engine.swap.after_exchange`): F rolls back.
    MidSwap,
    /// After `swapped` (`engine.swapped`): F rolls forward.
    Swapped,
}

/// Where the set-aside moves F's journal among its artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspendOrder {
    JournalFirst,
    JournalLast,
}

/// Each tree a case compares, by name: its hash, or `absent`; `history`: the
/// transactions of `history.jsonl`, in order.
pub type Trees = BTreeMap<&'static str, String>;

/// A recovering command of a case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovering {
    /// `config set check_interval_hours 6`: recovery under the lock alone.
    ConfigSet,
    /// An install of `bar` on `_retail_`, F's own client.
    InstallOwn,
    /// An install of `baz` on `_classic_era_`, the other client.
    InstallOther,
}

impl Recovering {
    pub fn label(self) -> &'static str {
        match self {
            Self::ConfigSet => "config set",
            Self::InstallOwn => "install on retail",
            Self::InstallOther => "install on era",
        }
    }

    /// Its argv, with `--json` and `--progress json` (so privacy is checked on both
    /// streams).
    pub fn argv(self, scene: &BarrierScene) -> Vec<String> {
        let words: Vec<String> = match self {
            Self::ConfigSet => ["config", "set", "check_interval_hours", "6"]
                .map(str::to_owned)
                .to_vec(),
            Self::InstallOwn => scene.install_args("gh:o/bar", "bar", "retail"),
            Self::InstallOther => scene.install_args("gh:o/baz", "baz", "era"),
        };
        let mut argv = words;
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        argv
    }

    /// The target ids it commits on, for the differential's caller.
    pub fn own(self) -> &'static [&'static str] {
        match self {
            Self::ConfigSet => &[],
            Self::InstallOwn => &["retail"],
            Self::InstallOther => &["era"],
        }
    }
}

/// A home with `_retail_` and `_classic_era_`, `foo` (`Foo`, `FooOld`) at v1 on retail and
/// SavedVariables of fake names, and F: the update of `foo` to v2, killed.
pub struct BarrierScene {
    pub home: Home,
    pub dl: PathBuf,
    pub crash: FolderCrash,
    /// F's transaction.
    pub f: String,
    /// The retail client dir, canonical.
    pub retail: PathBuf,
    /// The AddOns tree with `foo` at v1, before F.
    pub v1: String,
    /// `Foo` and `FooOld` at v1 (`addons.foo`).
    pub v1_foo: String,
    /// The set-aside's child outcome `lowered`, once it committed.
    lowered: RefCell<Option<Value>>,
    /// The set-aside's transaction, once its child ended or crashed.
    set_aside_tx: RefCell<Option<String>>,
    /// The doctor run each repair was given: the set-aside's, then the reactivation's.
    runs: RefCell<Vec<String>>,
}

impl BarrierScene {
    pub fn new(log: &TestLog, crash: FolderCrash) -> Self {
        log.step("build F");
        let home = Home::new(&["wow", "wow_classic_era"]);
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        for (name, entries) in [("v1", V1), ("v2", V2), ("bar", BAR), ("baz", BAZ)] {
            fs::write(dl.join(format!("{name}.zip")), zip_text(entries)).unwrap();
        }
        let retail = fs::canonicalize(home.client(RETAIL)).unwrap();
        for dir in [ACCOUNT, CHARACTER] {
            let dir = retail.join(dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("Foo.lua"), "FooDB = { seen = true }\n").unwrap();
        }
        let mut scene = Self {
            home,
            dl,
            crash,
            f: String::new(),
            retail,
            v1: String::new(),
            v1_foo: String::new(),
            lowered: RefCell::new(None),
            set_aside_tx: RefCell::new(None),
            runs: RefCell::new(Vec::new()),
        };
        log.run(
            &scene.home,
            &scene.json(scene.install_args("gh:o/foo", "v1", "retail")),
        )
        .ok();
        scene.v1 = tree_hash(&scene.home.addons(RETAIL));
        scene.v1_foo = scene.foo_now();
        let point = match crash {
            FolderCrash::MidSwap => "engine.swap.after_exchange=abort",
            FolderCrash::Swapped => "engine.swapped=abort",
        };
        log.cmd(
            &scene.home,
            &scene.json(scene.install_args("gh:o/foo", "v2", "retail")),
        )
        .failpoints(point)
        .run()
        .aborted();
        let folder: Vec<String> = repairs::journals(&scene.home)
            .iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?;
                (!name.ends_with(".repair.jsonl"))
                    .then(|| name.strip_suffix(".jsonl"))
                    .flatten()
                    .map(str::to_owned)
            })
            .collect();
        assert_eq!(
            folder.len(),
            1,
            "the killed update left one journal: {folder:?}"
        );
        scene.f = folder[0].clone();
        log.note(format!("F is {} ({crash:?})", scene.f));
        scene
    }

    /// `install <r> --file <zip> -t <target> --offline`.
    pub fn install_args(&self, r: &str, zip: &str, target: &str) -> Vec<String> {
        let zip = self.dl.join(format!("{zip}.zip")).display().to_string();
        ["install", r, "--file", &zip, "-t", target, "--offline"]
            .map(str::to_owned)
            .to_vec()
    }

    fn json(&self, mut argv: Vec<String>) -> Vec<String> {
        argv.push("--json".to_owned());
        argv
    }

    fn omaddons(&self) -> PathBuf {
        fs::canonicalize(self.home.env.state().join("omaddons")).unwrap()
    }

    fn meta(&self) -> PathBuf {
        self.retail.join("Interface/.omaddons")
    }

    /// F's journal under its own name.
    pub fn journal(&self) -> PathBuf {
        self.omaddons().join(format!("journal/{}.jsonl", self.f))
    }

    /// F's journal under its provisional name.
    pub fn provisional(&self) -> PathBuf {
        self.omaddons()
            .join(format!("journal/{}.reactivating.jsonl", self.f))
    }

    /// F's artifacts where the crash left them, each with its quarantine root, in `order`
    /// for the journal (the others: in-flight lock, state snapshot, client stage, client
    /// snapshot, those that exist).
    pub fn artifacts(&self, order: SuspendOrder) -> Vec<(PathBuf, Root)> {
        let (state, meta, f) = (self.omaddons(), self.meta(), &self.f);
        let client = || Root::Client(self.retail.clone());
        let mut found: Vec<(PathBuf, Root)> = [
            (state.join(format!("inflight/{f}.lock")), Root::State),
            (state.join(format!("snapshots/{f}")), Root::State),
            (meta.join(format!("stage/{f}")), client()),
            (meta.join(format!("snapshots/{f}")), client()),
        ]
        .into_iter()
        .filter(|(path, _)| fs::symlink_metadata(path).is_ok())
        .collect();
        let journal = (self.journal(), Root::State);
        match order {
            SuspendOrder::JournalFirst => found.insert(0, journal),
            SuspendOrder::JournalLast => found.push(journal),
        }
        found
    }

    /// The set-aside of F as a repair, committed by the child with `failpoints` armed.
    pub fn set_aside(&self, log: &TestLog, order: SuspendOrder, failpoints: &str) -> Child {
        log.step(&format!("set aside ({order:?}, {failpoints:?})"));
        let run = repairs::doctor_run(&self.home);
        self.runs.borrow_mut().push(run.clone());
        let target = omaddons::discover::registry::read_marker(&self.retail).map(|k| k.to_string());
        let fix = FixSpec::Suspend {
            tx: self.f.clone(),
            target,
            artifacts: self.artifacts(order),
        };
        let child = repairs::run_child(
            &self.home,
            log,
            &run,
            std::slice::from_ref(&self.retail),
            &[fix],
            failpoints,
        );
        *self.set_aside_tx.borrow_mut() = self.repair_tx();
        if let Child::Ended(outcome) = &child {
            assert_eq!(outcome["ok"], true, "the set-aside committed: {outcome}");
            *self.set_aside_tx.borrow_mut() = outcome["tx"].as_str().map(str::to_owned);
            *self.lowered.borrow_mut() = Some(outcome["lowered"].clone());
        }
        child
    }

    /// A set-aside that recorded its first step (F's journal, moved first) and was killed
    /// before running it, then F's journal replaced by a copy (another inode): its
    /// rollback finds that step in neither its before nor its after state, so recovery
    /// cannot settle it, and F, at its own name, waits behind it. The set-aside's tx.
    pub fn stuck_set_aside(&self, log: &TestLog) -> String {
        let child = self.set_aside(
            log,
            SuspendOrder::JournalFirst,
            "doctor.repair.step=abort@1",
        );
        assert!(matches!(child, Child::Crashed), "{child:?}");
        let journal = self.journal();
        let copy = journal.with_extension("copy");
        fs::copy(&journal, &copy).unwrap();
        fs::rename(&copy, &journal).unwrap();
        log.note("F's journal replaced by a copy: the set-aside's first step is in neither state");
        self.set_aside_tx()
    }

    /// The set-aside, committed with nothing armed, which must succeed.
    pub fn set_aside_whole(&self, log: &TestLog) {
        let child = self.set_aside(log, SuspendOrder::JournalLast, "");
        assert!(matches!(child, Child::Ended(_)), "{child:?}");
        assert!(!self.journal().exists(), "F's journal set aside");
    }

    /// How many primitives the reactivation has: each artifact back, the journal under its
    /// provisional name, then the activation.
    pub fn reactivation_len(&self) -> usize {
        let lowered = self.lowered.borrow();
        let steps = lowered.as_ref().expect("set aside first")["steps"]
            .as_array()
            .unwrap()
            .len();
        steps + 1
    }

    /// E7's reactivation of the set-aside, committed by the child with `failpoints` armed.
    pub fn reactivate(&self, log: &TestLog, failpoints: &str) -> Child {
        log.step(&format!("reactivate (crash at {failpoints:?})"));
        let run = repairs::doctor_run(&self.home);
        self.runs.borrow_mut().push(run.clone());
        let of = self.lowered.borrow().clone().expect("set aside first");
        repairs::reactivate_child(
            &self.home,
            log,
            &run,
            std::slice::from_ref(&self.retail),
            &of,
            &self.journal(),
            failpoints,
        )
    }

    /// E7's reactivation for real: `omaddons undo <the set-aside> --json --progress json`
    /// through the binary, `failpoints` armed (empty: none). The undo's own doctor run, once
    /// it is published, is recorded as the last repair's (for [`Self::actions_lines`]).
    pub fn undo_set_aside(&self, log: &TestLog, failpoints: &str) -> super::tlog::Ran {
        log.step(&format!("undo the set-aside (crash at {failpoints:?})"));
        let argv = ["undo", &self.set_aside_tx(), "--json", "--progress", "json"];
        let mut cmd = log.cmd(&self.home, &argv);
        if !failpoints.is_empty() {
            cmd = cmd.failpoints(failpoints);
        }
        let ran = cmd.run();
        let known = self.runs.borrow().clone();
        let newest = fs::read_dir(self.omaddons().join("doctor/runs"))
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.') && !known.contains(name))
            .max();
        if let Some(run) = newest {
            self.runs.borrow_mut().push(run);
        }
        ran
    }

    /// The set-aside's transaction.
    pub fn set_aside_tx(&self) -> String {
        self.set_aside_tx.borrow().clone().expect("set aside first")
    }

    /// The pending repair's transaction: its `.repair.jsonl`, else its in-flight lock (a
    /// crash before its `prepared` line), when one is left.
    pub fn repair_tx(&self) -> Option<String> {
        if let Some(tx) = repairs::repair_txs(&self.home).into_iter().next() {
            return Some(tx);
        }
        let inflight = self.omaddons().join("inflight");
        fs::read_dir(inflight).ok()?.find_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            let tx = name.strip_suffix(".lock")?;
            (tx != self.f).then(|| tx.to_owned())
        })
    }

    /// The pending repair's journal lines' types (`step 2` for a step line).
    pub fn repair_lines(&self) -> Vec<String> {
        let Some(tx) = repairs::repair_txs(&self.home).into_iter().next() else {
            return Vec::new();
        };
        let path = repairs::journal_dir(&self.home).join(format!("{tx}.repair.jsonl"));
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .map(|v| match v["seq"].as_u64() {
                Some(seq) => format!("step {seq}"),
                None => v["type"].as_str().unwrap_or("?").to_owned(),
            })
            .collect()
    }

    /// Whether the pending repair reached `done` (its journal holds `done` or
    /// `committed`), or, with no journal left, whether history records `tx`.
    pub fn done(&self, tx: &str) -> bool {
        let lines = self.repair_lines();
        if lines.is_empty() {
            return self.history().iter().any(|line| line == tx);
        }
        lines
            .iter()
            .any(|line| line == "done" || line == "committed")
    }

    /// The transactions of `history.jsonl`, in order.
    pub fn history(&self) -> Vec<String> {
        fs::read_to_string(self.omaddons().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|v| v["tx"].as_str().map(str::to_owned))
            .collect()
    }

    /// How many primitives the last repair's tracked session logged (`actions.jsonl` of its
    /// doctor run): a `checked` crash is primitive `k`'s when the journal holds step `k`'s
    /// line and this is `k - 1`.
    pub fn actions_lines(&self) -> usize {
        let run = self.runs.borrow().last().cloned().expect("a repair ran");
        let actions = self
            .omaddons()
            .join("doctor/runs")
            .join(run)
            .join("actions.jsonl");
        fs::read_to_string(actions).map_or(0, |text| text.lines().count())
    }

    /// The `checked` crash's point, attributed from the pending repair's journal: `None`
    /// for a hit on an untracked write (a root, a journal line, cleanup).
    pub fn checked_point(&self) -> Option<String> {
        let last = self.repair_lines().pop()?;
        let k: usize = last.strip_prefix("step ")?.parse().ok()?;
        (self.actions_lines() + 1 == k).then(|| format!("checked@{k}"))
    }

    /// `Foo` and `FooOld` of retail's AddOns, as [`Trees`] names them (`addons.foo`).
    pub fn foo_now(&self) -> String {
        let addons = self.home.addons(RETAIL);
        format!(
            "{} {}",
            hash(&addons.join("Foo")),
            hash(&addons.join("FooOld"))
        )
    }

    /// The `doctor` run directory the set-aside was given.
    fn set_aside_run(&self) -> PathBuf {
        let run = self
            .runs
            .borrow()
            .first()
            .cloned()
            .expect("set aside first");
        self.omaddons().join("doctor/runs").join(run)
    }

    /// Every tree a case compares.
    pub fn trees(&self) -> Trees {
        let addons = self.home.addons(RETAIL);
        let (state, meta) = (self.omaddons(), self.meta());
        let set_aside = self.set_aside_tx.borrow().clone().map_or_else(
            || "absent".to_owned(),
            |tx| hash(&meta.join(format!("snapshots/{tx}.repair"))),
        );
        let mut trees = Trees::new();
        trees.insert("addons", hash(&addons));
        trees.insert("addons.foo", self.foo_now());
        trees.insert("state.journal", hash(&state.join("journal")));
        trees.insert("state.inflight", hash(&state.join("inflight")));
        trees.insert("state.snapshots", hash(&state.join("snapshots")));
        trees.insert("state.targets", hash(&state.join("targets")));
        let quarantine = if self.runs.borrow().is_empty() {
            "absent".to_owned()
        } else {
            hash(&self.set_aside_run().join("quarantine"))
        };
        trees.insert("quarantine.state", quarantine);
        trees.insert("client.stage", hash(&meta.join("stage")));
        trees.insert("client.snapshots", hash(&meta.join("snapshots")));
        trees.insert("client.set_aside", set_aside);
        trees.insert("history", self.history().join(" "));
        trees
    }

    /// Whether a repair journal waits for recovery: one is left, and it ends neither
    /// `committed` nor `rolled_back`.
    pub fn repair_pending(&self) -> bool {
        self.repair_lines()
            .last()
            .is_some_and(|last| last != "committed" && last != "rolled_back")
    }

    /// Whether F is set aside: neither its own name nor its provisional one in the journal
    /// directory.
    pub fn f_absent(&self) -> bool {
        !self.journal().exists() && !self.provisional().exists()
    }
}

/// A tree's content hash, a file's size, or `absent`.
fn hash(path: &Path) -> String {
    if path.is_dir() {
        tree_hash(path)
    } else if path.exists() {
        format!("file {}", fs::read(path).map_or(0, |b| b.len()))
    } else {
        "absent".to_owned()
    }
}

/// The trees a recovering command may change when it commits something of its own: the
/// ones that hold only what the case is about.
pub const AFTER_A_COMMIT: &[&str] = &[
    "addons.foo",
    "state.journal",
    "state.inflight",
    "quarantine.state",
    "client.stage",
    "client.set_aside",
];

/// `trees` restricted to `keys`.
pub fn only(trees: &Trees, keys: &[&str]) -> Trees {
    trees
        .iter()
        .filter(|(key, _)| keys.contains(key))
        .map(|(key, value)| (*key, value.clone()))
        .collect()
}
