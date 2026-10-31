//! Crashed repairs (E5): a retail client registered by an install, one subject per fix
//! kind planted beside it where `config set`, `install gh:o/foo` and `undo` never reach,
//! SavedVariables under fake names, and a published doctor run. The kind's repair runs in
//! a child process of the test binary (`child_repair`, as `child_restore` runs a
//! restore), so a real `abort` stops it anywhere. Shared by the recovery smoke tests
//! (s5), the differential test over their crash states (s6) and the crash matrix (s7).
//!
//! The oracle is the subject: every path a kind's repair moves or changes, with its
//! quarantine destination, as `F:<sha256>:<mode>`, `D:<tree hash>:<mode>` or
//! `L:<target>` (absent: `None`). The before state is read once the scene is built; the
//! after state is predicted from it, never read from a run, so a reference run proves it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::engine::Scene;
use super::privacy::{ACCOUNT, CHARACTER, assert_private_all};
use super::tlog::TestLog;
use super::trees::tree_hash;

const SKILL: &str = "fm-plugins-skill-version-drift";
const FILE_MODE: &str = "fm-secrets-credential-file-mode";
const NESTED: &str = "fm-external_artifacts-nested-addon-folder";
const CORRUPT: &str = "fm-schemas-state-corrupt";
/// The skill a write replaces, and what it writes.
const GUIDE_V1: &[u8] = b"guide v1\n";
const GUIDE_V2: &[u8] = b"guide v2\n";
/// `<state>`'s folder for the planted state files.
const SCENE_DIR: &str = "scene";

/// One fix kind, as a synthetic repair exercises it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `<skills>/SKILL.md` "guide v1" (0640) written over with "guide v2" (0644), its
    /// backup at `<run>/backups/1`.
    WriteOver,
    /// `<skills>/SKILL.md` created.
    WriteCreate,
    /// `<config>/credentials/github` from 0644 to 0600.
    Chmod,
    /// `<state>/scene/.omaddons-stale1`, a file, into the run's state quarantine.
    QuarantineStateFile,
    /// `<client>/Interface/AddOns/Stray/` into the client's `.repair` quarantine.
    QuarantineClientDir,
    /// `<state>/scene/.omaddons-link1`, a temporary link, into the run's quarantine.
    QuarantineTempSymlink,
    /// `<A>/Wrap/Inner/` lifted to `<A>/Inner/`, `Wrap/` parked: two renames.
    FlattenTwo,
    /// `<A>/Same/Same/` lifted over its wrapper: three renames.
    FlattenThree,
}

impl Kind {
    pub const ALL: [Kind; 8] = [
        Kind::WriteOver,
        Kind::WriteCreate,
        Kind::Chmod,
        Kind::QuarantineStateFile,
        Kind::QuarantineClientDir,
        Kind::QuarantineTempSymlink,
        Kind::FlattenTwo,
        Kind::FlattenThree,
    ];

    /// How many primitives its repair lowers to.
    pub fn primitives(self) -> u32 {
        match self {
            Kind::FlattenTwo => 2,
            Kind::FlattenThree => 3,
            _ => 1,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::WriteOver => "write_over",
            Kind::WriteCreate => "write_create",
            Kind::Chmod => "chmod",
            Kind::QuarantineStateFile => "quarantine_state_file",
            Kind::QuarantineClientDir => "quarantine_client_dir",
            Kind::QuarantineTempSymlink => "quarantine_temp_symlink",
            Kind::FlattenTwo => "flatten_two",
            Kind::FlattenThree => "flatten_three",
        }
    }

    fn from_label(label: &str) -> Option<Kind> {
        // ubs:ignore — comparing fix-kind labels, not secrets
        Kind::ALL.into_iter().find(|kind| kind.label() == label)
    }
}

/// How far a crashed repair's journal got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPhase {
    /// No repair journal: the crash came before `prepared`, or cleanup pruned it.
    NoJournal,
    Unprepared,
    Prepared,
    Stepped(u32),
    Done,
    Committed,
    RolledBack,
}

/// Which state the subject must end in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Before,
    After,
}

/// How a child repair ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Child {
    /// An `abort` killed it.
    Crashed,
    /// Its outcome: `{ok, tx, outcome}` (`committed` or `rolled_back`) with the cleanup's
    /// warnings, or `{ok: false, tx, code, message, details}`.
    Ended(Value),
}

pub struct RepairScene {
    pub s: Scene,
    pub kind: Kind,
    /// The published doctor run the repair writes its backups and quarantine into.
    pub run_id: String,
    /// The subject as built.
    before: BTreeMap<String, Option<String>>,
    /// The subject a committed repair leaves, predicted from the planted trees.
    after: BTreeMap<String, Option<String>>,
    /// The last child's transaction, once known.
    tx: RefCell<Option<String>>,
}

impl RepairScene {
    /// A retail client registered by `install gh:o/foo v1`, the kind's subject planted,
    /// FAKEACCT/FakeRealm/FakeChar SavedVariables under `WTF/`, and a published `<run>`.
    pub fn new(kind: Kind) -> Self {
        let s = Scene::new();
        s.install("gh:o/foo", "v1");
        let client = s.home.client("_retail_");
        for dir in [ACCOUNT, CHARACTER] {
            let dir = client.join(dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("Foo.lua"), "FooDB = { seen = true }\n").unwrap();
        }
        let run = s.home.run(&["doctor", "--progress", "json"]);
        assert_private_all(&run.stdout, &run.stderr);
        let run_id = run.envelope()["run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("doctor published no run: {}", run.context())) // ubs:ignore — fails the test
            .to_owned();
        let mut scene = Self {
            s,
            kind,
            run_id,
            before: BTreeMap::new(),
            after: BTreeMap::new(),
            tx: RefCell::new(None),
        };
        let parked = scene.plant();
        scene.before = scene.subject();
        scene.after = scene.predicted(parked);
        scene
    }

    fn env(&self) -> &super::env::TestEnv {
        &self.s.home.env
    }

    fn state(&self) -> PathBuf {
        self.env().state().join("omaddons")
    }

    fn client(&self) -> PathBuf {
        self.s.home.client("_retail_")
    }

    /// `<client>/Interface/AddOns/<rel>`.
    fn addons(&self, rel: &str) -> PathBuf {
        self.s.addons().join(rel)
    }

    fn skill_file(&self) -> PathBuf {
        self.env().home().join(".claude/skills/omaddons/SKILL.md")
    }

    fn credential(&self) -> PathBuf {
        self.env().config().join("omaddons/credentials/github")
    }

    fn run_dir(&self) -> PathBuf {
        self.state().join("doctor/runs").join(&self.run_id)
    }

    /// The planted state file or link of the quarantine kinds.
    fn state_victim(&self) -> PathBuf {
        let name = match self.kind {
            Kind::QuarantineTempSymlink => ".omaddons-link1",
            _ => ".omaddons-stale1",
        };
        self.state().join(SCENE_DIR).join(name)
    }

    /// Plants the kind's subject, and returns what a committed repair parks: the tree a
    /// flatten leaves of its wrapper, as `D:<hash>:<mode>`.
    fn plant(&self) -> Option<String> {
        let write = |path: &Path, bytes: &[u8], mode: u32| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        };
        let toc = |name: &str| format!("## Interface: 120105\n## Title: {name}\n");
        match self.kind {
            Kind::WriteOver => write(&self.skill_file(), GUIDE_V1, 0o640),
            Kind::WriteCreate => fs::create_dir_all(self.skill_file().parent().unwrap()).unwrap(),
            Kind::Chmod => {
                fs::create_dir_all(self.credential().parent().unwrap()).unwrap();
                fs::set_permissions(
                    self.credential().parent().unwrap(),
                    fs::Permissions::from_mode(0o700),
                )
                .unwrap();
                write(&self.credential(), b"ghp_scenefixture\n", 0o644);
            }
            Kind::QuarantineStateFile => write(&self.state_victim(), b"stale\n", 0o644),
            Kind::QuarantineTempSymlink => {
                fs::create_dir_all(self.state_victim().parent().unwrap()).unwrap();
                std::os::unix::fs::symlink("../doctor/runs/old", self.state_victim()).unwrap();
            }
            Kind::QuarantineClientDir => {
                write(
                    &self.addons("Stray/Stray.toc"),
                    toc("Stray").as_bytes(),
                    0o644,
                );
                write(&self.addons("Stray/stray.lua"), b"-- stray\n", 0o644);
                write(&self.addons("Stray/sub/data.lua"), b"-- data\n", 0o644);
            }
            Kind::FlattenTwo => {
                write(
                    &self.addons("Wrap/Inner/Inner.toc"),
                    toc("Inner").as_bytes(),
                    0o644,
                );
                write(
                    &self.addons("Wrap/README.txt"),
                    b"wrapped by a zip\n",
                    0o644,
                );
                return Some(self.parked_tree(&self.addons("Wrap"), "Inner"));
            }
            Kind::FlattenThree => {
                write(
                    &self.addons("Same/Same/Same.toc"),
                    toc("Same").as_bytes(),
                    0o644,
                );
                write(
                    &self.addons("Same/notes.txt"),
                    b"wrapped in its own name\n",
                    0o644,
                );
                return Some(self.parked_tree(&self.addons("Same"), "Same"));
            }
        }
        None
    }

    /// `wrapper` without its `inner` child, as `D:<hash>:<mode>`: built in a scratch
    /// directory, since the scene's own tree must not change.
    fn parked_tree(&self, wrapper: &Path, inner: &str) -> String {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let scratch = self
            .env()
            .root()
            .join(format!("parked-{}", NEXT.fetch_add(1, Ordering::SeqCst)));
        fs::create_dir_all(&scratch).unwrap();
        for entry in fs::read_dir(wrapper).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == inner {
                continue;
            }
            let to = scratch.join(entry.file_name());
            fs::copy(entry.path(), &to).unwrap();
        }
        fs::set_permissions(&scratch, fs::metadata(wrapper).unwrap().permissions()).unwrap();
        let described = describe(&scratch).unwrap();
        fs::remove_dir_all(&scratch).unwrap();
        described
    }

    /// The subject paths, each by its label: the kind's victim and where its repair
    /// moves it, any `<tx>` read from what is on disk now.
    fn subject_paths(&self) -> Vec<(String, PathBuf)> {
        let mut paths = Vec::new();
        let mut add = |label: String, path: PathBuf| paths.push((label, path));
        let client_parked = |name: &str| -> Vec<(String, PathBuf)> {
            self.client_halves()
                .into_iter()
                .map(|half| {
                    (
                        format!("<client>/Interface/.omaddons/snapshots/<tx>.repair/quarantine/Interface/AddOns/{name}"),
                        half.join("quarantine/Interface/AddOns").join(name),
                    )
                })
                .collect()
        };
        match self.kind {
            Kind::WriteOver | Kind::WriteCreate => {
                add("<skills>/SKILL.md".to_owned(), self.skill_file());
            }
            Kind::Chmod => add("<config>/credentials/github".to_owned(), self.credential()),
            Kind::QuarantineStateFile | Kind::QuarantineTempSymlink => {
                let name = self.state_victim();
                let name = name.file_name().unwrap().to_string_lossy().into_owned();
                add(format!("<state>/{SCENE_DIR}/{name}"), self.state_victim());
                add(
                    format!("<run>/quarantine/state/{SCENE_DIR}/{name}"),
                    self.run_dir()
                        .join("quarantine/state")
                        .join(SCENE_DIR)
                        .join(&name),
                );
            }
            Kind::QuarantineClientDir => {
                add("<A>/Stray".to_owned(), self.addons("Stray"));
                paths.extend(client_parked("Stray"));
            }
            Kind::FlattenTwo => {
                add("<A>/Wrap".to_owned(), self.addons("Wrap"));
                add("<A>/Inner".to_owned(), self.addons("Inner"));
                paths.extend(client_parked("Wrap"));
            }
            Kind::FlattenThree => {
                add("<A>/Same".to_owned(), self.addons("Same"));
                for provisional in self.provisionals() {
                    paths.push(("<A>/.omaddons-flatten-<tx>".to_owned(), provisional));
                }
                paths.extend(client_parked("Same"));
            }
        }
        paths
    }

    /// The client's `<tx>.repair` snapshot halves.
    fn client_halves(&self) -> Vec<PathBuf> {
        listed(&self.client().join("Interface/.omaddons/snapshots"))
            .into_iter()
            .filter(|path| path.to_string_lossy().ends_with(".repair"))
            .collect()
    }

    /// A three-rename flatten's provisional folders in AddOns.
    fn provisionals(&self) -> Vec<PathBuf> {
        listed(&self.s.addons())
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".omaddons-flatten-"))
            })
            .collect()
    }

    /// Every subject path, by its label, → `None` (absent) or `F:<sha256>:<mode>`,
    /// `D:<tree hash>:<mode>` or `L:<target>`. A destination named for a transaction
    /// that is not on disk is absent.
    pub fn subject(&self) -> BTreeMap<String, Option<String>> {
        let mut subject: BTreeMap<String, Option<String>> = self
            .predicted_labels()
            .into_iter()
            .map(|label| (label, None))
            .collect();
        for (label, path) in self.subject_paths() {
            let described = describe(&path);
            let slot = subject.entry(label.clone()).or_insert(None);
            assert!(
                slot.is_none() || described.is_none(),
                "{label}: two transactions' halves hold it"
            );
            if described.is_some() {
                *slot = described;
            }
        }
        subject
    }

    /// Every label the subject has, present or not.
    fn predicted_labels(&self) -> Vec<String> {
        let client = |name: &str| {
            format!(
                "<client>/Interface/.omaddons/snapshots/<tx>.repair/quarantine/Interface/AddOns/{name}"
            )
        };
        match self.kind {
            Kind::WriteOver | Kind::WriteCreate => vec!["<skills>/SKILL.md".to_owned()],
            Kind::Chmod => vec!["<config>/credentials/github".to_owned()],
            Kind::QuarantineStateFile | Kind::QuarantineTempSymlink => {
                let name = self.state_victim();
                let name = name.file_name().unwrap().to_string_lossy().into_owned();
                vec![
                    format!("<state>/{SCENE_DIR}/{name}"),
                    format!("<run>/quarantine/state/{SCENE_DIR}/{name}"),
                ]
            }
            Kind::QuarantineClientDir => vec!["<A>/Stray".to_owned(), client("Stray")],
            Kind::FlattenTwo => vec![
                "<A>/Wrap".to_owned(),
                "<A>/Inner".to_owned(),
                client("Wrap"),
            ],
            Kind::FlattenThree => vec![
                "<A>/Same".to_owned(),
                "<A>/.omaddons-flatten-<tx>".to_owned(),
                client("Same"),
            ],
        }
    }

    /// The subject a committed repair leaves, from the subject as built.
    fn predicted(&self, parked: Option<String>) -> BTreeMap<String, Option<String>> {
        let mut after = self.before.clone();
        let mut moved = |from: &str, to: &str| {
            let it = after.insert(from.to_owned(), None).flatten();
            assert!(it.is_some(), "{from} is planted");
            after.insert(to.to_owned(), it);
        };
        let labels = self.predicted_labels();
        match self.kind {
            Kind::WriteOver | Kind::WriteCreate => {
                after.insert(labels[0].clone(), Some(file(GUIDE_V2, 0o644)));
            }
            Kind::Chmod => {
                let before = after[&labels[0]].clone().unwrap();
                let (prefix, _) = before.rsplit_once(':').unwrap();
                after.insert(labels[0].clone(), Some(format!("{prefix}:600")));
            }
            Kind::QuarantineStateFile | Kind::QuarantineTempSymlink | Kind::QuarantineClientDir => {
                moved(&labels[0], &labels[1])
            }
            Kind::FlattenTwo => {
                let inner = describe(&self.addons("Wrap/Inner"));
                after.insert("<A>/Wrap".to_owned(), None);
                after.insert("<A>/Inner".to_owned(), inner);
                after.insert(labels[2].clone(), parked);
            }
            Kind::FlattenThree => {
                let inner = describe(&self.addons("Same/Same"));
                after.insert("<A>/Same".to_owned(), inner);
                after.insert(labels[2].clone(), parked);
            }
        }
        after
    }

    /// The subject `end` must leave.
    pub fn expected(&self, end: End) -> &BTreeMap<String, Option<String>> {
        match end {
            End::Before => &self.before,
            End::After => &self.after,
        }
    }

    /// Runs the kind's repair in `child_repair`, `OMADDONS_FAILPOINTS=spec` (empty:
    /// none): `Crashed`, or the child's outcome JSON. The spec, the exit status and the
    /// outcome are logged.
    pub fn child(&self, log: &TestLog, spec: &str) -> Child {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let out = self.env().root().join(format!(
            "repair-{}.json",
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "child_repair",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        self.env().apply_to(&mut cmd);
        cmd.env("OMADDONS_REPAIR_KIND", self.kind.label())
            .env("OMADDONS_REPAIR_RUN", &self.run_id)
            .env("OMADDONS_REPAIR_CLIENT", self.client())
            .env("OMADDONS_REPAIR_OUT", &out)
            .env_remove("OMADDONS_FAILPOINTS")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if !spec.is_empty() {
            cmd.env("OMADDONS_FAILPOINTS", spec);
        }
        let output = cmd.output().unwrap();
        log.note(format!(
            "child_repair {} with OMADDONS_FAILPOINTS={spec:?}: {}",
            self.kind.label(),
            output.status
        ));
        let stderr = String::from_utf8_lossy(&output.stderr);
        let ended = match output.status.code() {
            None => Child::Crashed,
            Some(0) => {
                let outcome: Value = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
                log.json("child outcome", &outcome);
                Child::Ended(outcome)
            }
            Some(code) => panic!("child_repair exited {code}: {stderr}"), // ubs:ignore — fails the test
        };
        let tx = match &ended {
            Child::Ended(outcome) => outcome["tx"].as_str().map(str::to_owned),
            Child::Crashed => self.journal().and_then(|path| {
                let name = path.file_name()?.to_str()?;
                name.strip_suffix(".repair.jsonl").map(str::to_owned)
            }),
        };
        *self.tx.borrow_mut() = tx;
        ended
    }

    /// The last child's transaction, once known (from its outcome, or its journal's
    /// name after a crash).
    pub fn tx(&self) -> Option<String> {
        self.tx.borrow().clone()
    }

    /// The repair journal, if any.
    fn journal(&self) -> Option<PathBuf> {
        listed(&self.state().join("journal"))
            .into_iter()
            .find(|path| path.to_string_lossy().ends_with(".repair.jsonl"))
    }

    /// The repair journal's lines, if there is one.
    pub fn journal_lines(&self) -> Option<Vec<String>> {
        let text = fs::read_to_string(self.journal()?).ok()?;
        Some(text.lines().map(str::to_owned).collect())
    }

    /// `<run>/actions.jsonl`'s lines.
    pub fn actions_lines(&self) -> Vec<String> {
        fs::read_to_string(self.run_dir().join("actions.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// How far the repair journal got.
    pub fn phase(&self) -> CrashPhase {
        use omaddons::engine::repair::RepairPhase;
        let Some(journal) = self.journal() else {
            return CrashPhase::NoJournal;
        };
        let text = String::from_utf8_lossy(&fs::read(journal).unwrap()).into_owned();
        match omaddons::engine::repair::journal::parse(&text).phase {
            RepairPhase::Unprepared => CrashPhase::Unprepared,
            RepairPhase::Prepared => CrashPhase::Prepared,
            RepairPhase::Stepped(k) => CrashPhase::Stepped(k),
            RepairPhase::Done => CrashPhase::Done,
            RepairPhase::Committed => CrashPhase::Committed,
            RepairPhase::RolledBack => CrashPhase::RolledBack,
        }
    }

    /// The repair lines in history naming the last child's transaction (every repair
    /// line when it is not known).
    pub fn history_names_tx(&self) -> usize {
        let tx = self.tx();
        fs::read_to_string(self.state().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            // ubs:ignore — comparing history kinds and transaction ids, not secrets
            .filter(|line| line["kind"] == "repair")
            .filter(|line| tx.as_deref().is_none_or(|tx| line["tx"] == tx))
            .count()
    }

    /// The problems with where `end` puts each preserved original: a subject path that
    /// is not as `end` says, and, after a write over a file, a backup that does not hold
    /// the bytes it replaced. Empty when everything is accounted for.
    pub fn accounted(&self, end: End) -> Vec<String> {
        let now = self.subject();
        let want = self.expected(end);
        let mut problems: Vec<String> = want
            .iter()
            .filter(|(label, wanted)| now.get(*label) != Some(*wanted))
            .map(|(label, wanted)| format!("{label}: want {wanted:?}, got {:?}", now.get(label)))
            .collect();
        // ubs:ignore — comparing a fix kind and an end state, not secrets
        if self.kind == Kind::WriteOver && end == End::After {
            let backup = self.run_dir().join("backups/1");
            if fs::read(&backup).ok().as_deref() != Some(GUIDE_V1) {
                problems.push(format!("{} does not hold guide v1", backup.display()));
            }
        }
        problems
    }

    /// `.omaddons-*` entries anywhere in the scene but the repair's own roots (the run
    /// directory, the client's `.repair` halves, the cache quarantine) and the planted
    /// victim: what a finished or recovered repair must not leave.
    pub fn leftovers(&self) -> Vec<PathBuf> {
        let roots = [
            self.state().join("doctor/runs"),
            self.client().join("Interface/.omaddons/snapshots"),
            self.env().cache().join("omaddons/quarantine"),
        ];
        let mut found = Vec::new();
        let mut dirs = vec![self.env().root().to_path_buf()];
        while let Some(dir) = dirs.pop() {
            for path in listed(&dir) {
                if roots.contains(&path) {
                    continue;
                }
                let meta = fs::symlink_metadata(&path).unwrap();
                let named = path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".omaddons-"));
                if named && path != self.state_victim() {
                    found.push(path.clone());
                }
                if meta.is_dir() {
                    dirs.push(path);
                }
            }
        }
        found.sort();
        found
    }
}

/// The crash specs on a kind's path, shared by s6's differential test and s7's matrix:
/// the doctor abort points (`doctor.repair.prepared`, `doctor.repair.step=abort@k` for
/// each k, `doctor.repair.done`, `doctor.repair.history`, `doctor.repair.committed`) and
/// the tracked-only fsx boundaries (`fsx.mutate.{backed_up,executed,logged}=abort@k` for
/// each k).
pub fn crash_specs(kind: Kind) -> Vec<String> {
    let steps = 1..=kind.primitives();
    let mut specs = vec!["doctor.repair.prepared=abort".to_owned()];
    specs.extend(
        steps
            .clone()
            .map(|k| format!("doctor.repair.step=abort@{k}")),
    );
    specs.extend(
        [
            "doctor.repair.done=abort",
            "doctor.repair.history=abort",
            "doctor.repair.committed=abort",
        ]
        .map(str::to_owned),
    );
    for boundary in ["backed_up", "executed", "logged"] {
        specs.extend(
            steps
                .clone()
                .map(|k| format!("fsx.mutate.{boundary}=abort@{k}")),
        );
    }
    specs
}

/// The most hits of `fsx.mutate.checked` a sweep tries before it calls the repair
/// unbounded.
const CHECKED_SWEEP_MAX: u64 = 64;

/// `fsx.mutate.checked=abort@N` for N = 1, 2, … on a fresh scene each, until a child
/// completes without crashing (`checked` is hit by untracked writes too, so it is swept,
/// not addressed). Returns each crashed scene with its N.
pub fn checked_sweep(scene: impl Fn() -> RepairScene, log: &TestLog) -> Vec<(u64, RepairScene)> {
    let mut crashed = Vec::new();
    for n in 1..=CHECKED_SWEEP_MAX {
        let fresh = scene();
        match fresh.child(log, &format!("fsx.mutate.checked=abort@{n}")) {
            Child::Crashed => crashed.push((n, fresh)),
            Child::Ended(_) => return crashed,
        }
    }
    panic!("a repair still crashed at fsx.mutate.checked@{CHECKED_SWEEP_MAX}"); // ubs:ignore — fails the test
}

/// The body of each test binary's `child_repair` (an ignored test, as `child_restore`):
/// reads `HOME`/`XDG_*`, `OMADDONS_REPAIR_KIND`, `OMADDONS_REPAIR_RUN`,
/// `OMADDONS_REPAIR_CLIENT` and `OMADDONS_REPAIR_OUT`, lowers and commits the kind's
/// repair in process (the failpoints come from `OMADDONS_FAILPOINTS`), finishes its
/// cleanup, and writes the outcome JSON. Run on its own it does nothing.
pub fn child_main() {
    let var = |key: &str| std::env::var_os(key);
    let (Some(kind), Some(run), Some(client), Some(out)) = (
        var("OMADDONS_REPAIR_KIND"),
        var("OMADDONS_REPAIR_RUN"),
        var("OMADDONS_REPAIR_CLIENT"),
        var("OMADDONS_REPAIR_OUT"),
    ) else {
        return;
    };
    let kind = Kind::from_label(&kind.to_string_lossy()).unwrap();
    let outcome = repair(kind, &run.to_string_lossy(), Path::new(&client));
    fs::write(out, outcome.to_string()).unwrap();
}

/// The kind's repair, lowered and committed as `doctor fix` will: the transaction begun
/// at lowering, the commit under the global lock, the cleanup after it.
fn repair(kind: Kind, run_id: &str, client_dir: &Path) -> Value {
    use omaddons::engine::repair::{
        CommitCtx, LowerCtx, PlannedFix, RepairMeta, RepairOutcome, RepairRun, RepairTx, commit,
        lower,
    };
    use omaddons::engine::repair_action::{Fix, Mode, QuarantineRoot};
    use omaddons::fsx::{GlobalLock, ScopeTable};
    use omaddons::model::{Actor, RunId};
    use omaddons::state::paths::Paths;

    let var = |key: &str| PathBuf::from(std::env::var_os(key).unwrap());
    let paths = Paths::with_roots(
        var("HOME"),
        var("XDG_CONFIG_HOME"),
        var("XDG_STATE_HOME"),
        var("XDG_CACHE_HOME"),
    );
    let key = omaddons::discover::registry::read_marker(client_dir).unwrap();
    let addons = client_dir.join("Interface/AddOns");
    let mode = |bits| Mode::new(bits).unwrap();
    let state_victim = |name: &str| paths.state_dir().join(SCENE_DIR).join(name);
    let (fm, fix) = match kind {
        Kind::WriteOver | Kind::WriteCreate => (
            SKILL,
            Fix::Write {
                path: paths.skill_dir().join("SKILL.md"),
                content: GUIDE_V2.to_vec(),
                mode: mode(0o644),
            },
        ),
        Kind::Chmod => (
            FILE_MODE,
            Fix::Chmod {
                path: paths.credential_file("github"),
                mode: mode(0o600),
            },
        ),
        Kind::QuarantineStateFile => (
            CORRUPT,
            Fix::Quarantine {
                path: state_victim(".omaddons-stale1"),
                root: QuarantineRoot::State,
            },
        ),
        Kind::QuarantineTempSymlink => (
            CORRUPT,
            Fix::Quarantine {
                path: state_victim(".omaddons-link1"),
                root: QuarantineRoot::State,
            },
        ),
        Kind::QuarantineClientDir => (
            CORRUPT,
            Fix::Quarantine {
                path: addons.join("Stray"),
                root: QuarantineRoot::Client {
                    client_dir: client_dir.to_path_buf(),
                },
            },
        ),
        Kind::FlattenTwo => (
            NESTED,
            Fix::Flatten {
                inner: addons.join("Wrap/Inner"),
                outer: addons.join("Wrap"),
            },
        ),
        Kind::FlattenThree => (
            NESTED,
            Fix::Flatten {
                inner: addons.join("Same/Same"),
                outer: addons.join("Same"),
            },
        ),
    };
    let fixes = [PlannedFix { fm: fm.into(), fix }];
    let clients = [(client_dir.to_path_buf(), key)];
    let skill_dirs = [paths.skill_dir()];
    let scopes = ScopeTable::new(&paths, &[client_dir.to_path_buf()], None).unwrap();
    let run = RepairRun {
        dir: paths.doctor_runs_dir().join(run_id),
        id: run_id.to_owned(),
    };
    let rtx = RepairTx::begin(&paths).unwrap();
    let tx = rtx.tx();
    let lowered = lower(
        &LowerCtx {
            paths: &paths,
            tx,
            run: &run,
            clients: &clients,
            skill_dirs: &skill_dirs,
            scopes: &scopes,
            mount_id: &omaddons::fsx::mount_id,
        },
        &fixes,
    )
    .unwrap_or_else(|err| panic!("{} does not lower: {err:?}", kind.label())); // ubs:ignore — fails the test
    let lock = GlobalLock::acquire(&paths, "doctor fix").unwrap();
    let ctx = CommitCtx {
        lock: &lock,
        scopes: &scopes,
        run: &run,
        mount_id: &omaddons::fsx::mount_id,
    };
    let meta = RepairMeta {
        run: RunId::new(),
        actor: Actor::Agent,
        reason: None,
        argv: ["omaddons", "doctor", "fix"].map(str::to_owned).to_vec(),
        command: "doctor fix",
    };
    let committed = commit(&ctx, rtx, &lowered, &meta);
    drop(lock);
    match committed {
        Ok((outcome, cleanup)) => {
            let warnings: Vec<&str> = cleanup.finish().iter().map(|w| w.code.wire()).collect();
            let outcome = match outcome {
                RepairOutcome::Committed { .. } => "committed",
                RepairOutcome::RolledBack { .. } => "rolled_back",
            };
            json!({"ok": true, "tx": tx, "outcome": outcome, "warnings": warnings})
        }
        Err(err) => json!({
            "ok": false,
            "tx": tx,
            "code": err.code.wire(),
            "message": err.message,
            "details": err.details,
        }),
    }
}

/// A path as the subject has it: `None` when absent.
fn describe(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    let mode = meta.permissions().mode() & 0o7777;
    Some(if meta.file_type().is_symlink() {
        format!("L:{}", fs::read_link(path).unwrap().display())
    } else if meta.is_dir() {
        format!("D:{}:{mode:o}", tree_hash(path))
    } else {
        file(&fs::read(path).unwrap(), mode)
    })
}

fn file(bytes: &[u8], mode: u32) -> String {
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("F:{digest}:{mode:o}")
}

/// The entries of `dir`, none when it is absent.
fn listed(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|listing| listing.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default()
}
