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
    /// `<config>/credentials/github` into the run's config quarantine: only the mount
    /// scene uses it (it is not in [`Kind::ALL`]).
    QuarantineConfigFile,
    /// A stuck folder journal on retail (a `prepared` line that names its target but does
    /// not decode), its in-flight lock and its client stage set aside, guarded by the
    /// client's generation and marker: three renames. E7's undo scenes use it (it is not
    /// in [`Kind::ALL`]).
    Suspend,
}

/// The stuck journal [`Kind::Suspend`] sets aside.
pub const STUCK: &str = "01JB8Q2VA00000000000000009";

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
            Kind::FlattenThree | Kind::Suspend => 3,
            _ => 1,
        }
    }

    /// How many inverses its undo runs: one per primitive, and for a set-aside one more,
    /// the journal file being restored under its provisional name first and activated last.
    pub fn undo_inverses(self) -> u32 {
        match self {
            Kind::Suspend => self.primitives() + 1,
            _ => self.primitives(),
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
            Kind::QuarantineConfigFile => "quarantine_config_file",
            Kind::Suspend => "suspend",
        }
    }

    fn from_label(label: &str) -> Option<Kind> {
        Kind::ALL
            .into_iter()
            .chain([Kind::QuarantineConfigFile, Kind::Suspend])
            // ubs:ignore — comparing fix-kind labels, not secrets
            .find(|kind| kind.label() == label)
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

    /// [`Kind::Suspend`]'s stuck journal, its in-flight lock and its client stage.
    fn stuck(&self) -> [PathBuf; 3] {
        [
            self.state().join(format!("journal/{STUCK}.jsonl")),
            self.state().join(format!("inflight/{STUCK}.lock")),
            self.client()
                .join(format!("Interface/.omaddons/stage/{STUCK}")),
        ]
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
            Kind::Chmod | Kind::QuarantineConfigFile => {
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
            Kind::Suspend => {
                // A `prepared` line that names its target (so recovery leaves it on
                // retail alone) but does not decode: `journal_unreadable`.
                let key = omaddons::discover::registry::read_marker(&self.client()).unwrap();
                let [journal, lock, stage] = self.stuck();
                let line = json!({
                    "type": "prepared",
                    "schema_version": "1.1",
                    "target_key": key,
                    "written_by": "a build this one does not read",
                });
                write(&journal, format!("{line}\n").as_bytes(), 0o644);
                write(&lock, b"", 0o644);
                write(&stage.join("Foo/Foo.toc"), toc("Foo").as_bytes(), 0o644);
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
            Kind::QuarantineConfigFile => {
                add("<config>/credentials/github".to_owned(), self.credential());
                add(
                    "<run>/quarantine/config/credentials/github".to_owned(),
                    self.run_dir().join("quarantine/config/credentials/github"),
                );
            }
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
            Kind::Suspend => {
                let [journal, lock, stage] = self.stuck();
                add(format!("<state>/journal/{STUCK}.jsonl"), journal);
                add(
                    format!("<state>/journal/{STUCK}.reactivating.jsonl"),
                    self.state()
                        .join(format!("journal/{STUCK}.reactivating.jsonl")),
                );
                add(format!("<state>/inflight/{STUCK}.lock"), lock);
                add(format!("<meta>/stage/{STUCK}"), stage);
                let quarantine = self.run_dir().join("quarantine/state");
                add(
                    format!("<run>/quarantine/state/journal/{STUCK}.jsonl"),
                    quarantine.join(format!("journal/{STUCK}.jsonl")),
                );
                add(
                    format!("<run>/quarantine/state/inflight/{STUCK}.lock"),
                    quarantine.join(format!("inflight/{STUCK}.lock")),
                );
                for half in self.client_halves() {
                    paths.push((
                        format!("<client>/Interface/.omaddons/snapshots/<tx>.repair/quarantine/Interface/.omaddons/stage/{STUCK}"),
                        half.join(format!("quarantine/Interface/.omaddons/stage/{STUCK}")),
                    ));
                }
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
            Kind::QuarantineConfigFile => vec![
                "<config>/credentials/github".to_owned(),
                "<run>/quarantine/config/credentials/github".to_owned(),
            ],
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
            Kind::Suspend => vec![
                format!("<state>/journal/{STUCK}.jsonl"),
                format!("<state>/inflight/{STUCK}.lock"),
                format!("<meta>/stage/{STUCK}"),
                format!("<run>/quarantine/state/journal/{STUCK}.jsonl"),
                format!("<run>/quarantine/state/inflight/{STUCK}.lock"),
                format!(
                    "<client>/Interface/.omaddons/snapshots/<tx>.repair/quarantine/Interface/.omaddons/stage/{STUCK}"
                ),
                format!("<state>/journal/{STUCK}.reactivating.jsonl"),
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
            Kind::QuarantineStateFile
            | Kind::QuarantineTempSymlink
            | Kind::QuarantineClientDir
            | Kind::QuarantineConfigFile => moved(&labels[0], &labels[1]),
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
            Kind::Suspend => {
                for (from, to) in [(0, 3), (1, 4), (2, 5)] {
                    moved(&labels[from], &labels[to]);
                }
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
        self.child_in(log, spec, None)
    }

    /// [`Self::child`] with `dir` bind-mounted over itself first, in an unprivileged
    /// user and mount namespace, so it is a mount of its own; `None` where this machine
    /// refuses such namespaces.
    pub fn child_bound(&self, log: &TestLog, spec: &str, dir: &Path) -> Option<Child> {
        super::mounts::can_bind_mount(dir).then(|| self.child_in(log, spec, Some(dir)))
    }

    fn child_in(&self, log: &TestLog, spec: &str, bind: Option<&Path>) -> Child {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let out = self.env().root().join(format!(
            "repair-{}.json",
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let exe = std::env::current_exe().unwrap();
        let mut cmd = match bind {
            None => Command::new(exe), // ubs:ignore — the test binary running itself
            Some(dir) => super::mounts::under_bind(dir, exe),
        };
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
            Child::Crashed => self
                .journal()
                .and_then(|path| {
                    let name = path.file_name()?.to_str()?;
                    name.strip_suffix(".repair.jsonl").map(str::to_owned)
                })
                .or_else(|| self.inflight_tx()),
        };
        *self.tx.borrow_mut() = tx;
        ended
    }

    /// The transaction of the one in-flight lock in `<state>/inflight/`: a crash before
    /// the journal leaves only that of a repair.
    fn inflight_tx(&self) -> Option<String> {
        let locks: Vec<String> = listed(&self.state().join("inflight"))
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?;
                name.strip_suffix(".lock").map(str::to_owned)
            })
            .collect();
        match locks.as_slice() {
            [tx] => Some(tx.clone()),
            _ => None,
        }
    }

    /// The last child's transaction, once known (from its outcome, or its journal's or
    /// in-flight lock's name after a crash).
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
            // ubs:ignore — comparing subject descriptions, not secrets
            .filter(|(label, wanted)| now.get(*label) != Some(*wanted))
            .map(|(label, wanted)| format!("{label}: want {wanted:?}, got {:?}", now.get(label)))
            .collect();
        // ubs:ignore — comparing a fix kind and an end state, not secrets
        if self.kind == Kind::WriteOver && end == End::After {
            let backup = self.run_dir().join("backups/1");
            // ubs:ignore — comparing a test file's bytes, not secrets
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
                    // ubs:ignore — walking the test's own scene, never following a link
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
    specs_over(kind.primitives())
}

/// [`crash_specs`] of a repair transaction of `n` primitives.
fn specs_over(n: u32) -> Vec<String> {
    let steps = 1..=n;
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
/// not addressed), each a `log.step` named `<kind> at <spec>`. Returns each crashed scene
/// with its N.
pub fn checked_sweep(scene: impl Fn() -> RepairScene, log: &TestLog) -> Vec<(u64, RepairScene)> {
    let mut crashed = Vec::new();
    for n in 1..=CHECKED_SWEEP_MAX {
        let fresh = scene();
        let spec = format!("fsx.mutate.checked=abort@{n}");
        log.step(&format!("{} at {spec}", fresh.kind.label()));
        match fresh.child(log, &spec) {
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
    // A repair of the fixes the parent names (`common::repairs`, E14's fence scenarios).
    if super::repairs::child_main() {
        return;
    }
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
        CommitCtx, LowerCtx, LowerError, PlannedFix, RepairMeta, RepairOutcome, RepairRun,
        RepairTx, commit, lower,
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
        Kind::QuarantineConfigFile => (
            CORRUPT,
            Fix::Quarantine {
                path: paths.credential_file("github"),
                root: QuarantineRoot::Config,
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
        Kind::Suspend => {
            use omaddons::engine::repair::guard;
            use omaddons::engine::repair_action::{
                Attribution, JournalFamily, JournalSet, SuspendedArtifact,
            };
            let tx: omaddons::model::TxId = STUCK.parse().unwrap();
            let state = || QuarantineRoot::State;
            let artifact = |path: PathBuf, root: QuarantineRoot| SuspendedArtifact { path, root };
            (
                "fm-concurrency_primitives-journal-unreadable",
                Fix::Suspend {
                    journal: JournalSet {
                        tx,
                        family: JournalFamily::Folder,
                        target: Some(key),
                        artifacts: vec![
                            artifact(paths.journal(&tx), state()),
                            artifact(paths.inflight_lock(&tx), state()),
                            artifact(
                                client_dir.join(format!("Interface/.omaddons/stage/{STUCK}")),
                                QuarantineRoot::Client {
                                    client_dir: client_dir.to_path_buf(),
                                },
                            ),
                        ],
                        guards: vec![
                            guard::observe_generation(&paths, key).unwrap(),
                            guard::observe_marker(client_dir, key).unwrap(),
                        ],
                        attribution: Attribution::Complete,
                        unattributed: Vec::new(),
                    },
                },
            )
        }
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
    let lowered = match lower(
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
    ) {
        Ok(lowered) => lowered,
        Err(LowerError::Refused {
            reason, message, ..
        }) => {
            return json!({
                "ok": false,
                "tx": tx,
                "stage": "lower",
                "refused": reason.wire(),
                "message": message,
            });
        }
        Err(LowerError::Error(err)) => {
            return json!({
                "ok": false,
                "tx": tx,
                "stage": "lower",
                "code": err.code.wire(),
                "message": err.message,
            });
        }
    };
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
        undo: None,
    };
    let committed = commit(&ctx, rtx, &lowered, &meta);
    drop(lock);
    match committed {
        Ok((outcome, cleanup)) => {
            let warnings: Vec<&str> = cleanup.finish().iter().map(|w| w.code.wire()).collect();
            match outcome {
                RepairOutcome::Committed { primitives, .. } => json!({
                    "ok": true,
                    "tx": tx,
                    "outcome": "committed",
                    "primitives": primitives,
                    "warnings": warnings,
                }),
                RepairOutcome::RolledBack {
                    failed_seq,
                    cause,
                    backups_restored,
                    ..
                } => json!({
                    "ok": true,
                    "tx": tx,
                    "outcome": "rolled_back",
                    "failed_seq": failed_seq,
                    "cause": cause.code.wire(),
                    "backups_restored": backups_restored,
                    "warnings": warnings,
                }),
            }
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

// ---------------------------------------------------------------------------------------
// The oracle (s7): what every crash or failure of a repair must settle to.

impl RepairScene {
    /// Where a repair stopped at `phase` must end: before its durable `done` the whole
    /// before state, from it the whole after state. With no journal, history says which
    /// (a crash after its cleanup pruned the journal leaves a history line).
    pub fn end_at(&self, phase: CrashPhase) -> End {
        match phase {
            CrashPhase::Done | CrashPhase::Committed => End::After,
            // ubs:ignore — counting history lines, not comparing secrets
            CrashPhase::NoJournal if self.history_names_tx() == 1 => End::After,
            _ => End::Before,
        }
    }

    /// The trees a repair scene's commands may write, by label.
    pub fn trees(&self) -> [(&'static str, PathBuf); 4] {
        [
            ("state", self.env().state()),
            ("config", self.env().config()),
            ("cache", self.env().cache()),
            ("client", self.client()),
        ]
    }

    /// The scene's `install gh:o/foo v1` transaction, as history names it.
    pub fn install_tx(&self) -> String {
        self.history_lines()
            .into_iter()
            // ubs:ignore — comparing history kinds, not secrets
            .find(|line| line["kind"] == "install")
            .and_then(|line| line["tx"].as_str().map(str::to_owned))
            .unwrap()
    }

    /// Every history line that reads.
    fn history_lines(&self) -> Vec<Value> {
        fs::read_to_string(self.state().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect()
    }

    /// The repair lines in history naming the last child's transaction.
    pub fn repair_history(&self) -> Vec<Value> {
        let tx = self.tx();
        self.history_lines()
            .into_iter()
            // ubs:ignore — comparing history kinds and transaction ids, not secrets
            .filter(|line| line["kind"] == "repair")
            .filter(|line| tx.as_deref().is_none_or(|tx| line["tx"] == tx))
            .collect()
    }

    /// Every history line and quarantine entry of a repair that names a path below a
    /// `WTF/` (in any case): what a repair must never write.
    pub fn wtf_named(&self) -> Vec<String> {
        let mut named: Vec<String> = self
            .repair_history()
            .iter()
            .map(Value::to_string)
            .filter(|line| names_wtf(line))
            .collect();
        let mut dirs = vec![
            self.run_dir().join("quarantine"),
            self.env().cache().join("omaddons/quarantine"),
        ];
        dirs.extend(self.client_halves());
        while let Some(dir) = dirs.pop() {
            for path in listed(&dir) {
                let text = path.display().to_string();
                if names_wtf(&text) {
                    named.push(text);
                }
                if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                    // ubs:ignore — walking the test's own quarantines, never following a link
                    dirs.push(path);
                }
            }
        }
        named
    }

    /// Whether some live process holds `tx`'s in-flight lock.
    fn inflight_held(&self, tx: &str) -> bool {
        fs::File::open(self.state().join(format!("inflight/{tx}.lock")))
            .is_ok_and(|file| file.try_lock().is_err())
    }
}

/// Whether `text` names a path below a `WTF/` folder.
fn names_wtf(text: &str) -> bool {
    text.to_ascii_lowercase().contains("/wtf/")
}

/// `omaddons <args> --json --progress json`.
pub fn json_args(args: &[&str]) -> Vec<String> {
    args.iter()
        .copied()
        .chain(["--json", "--progress", "json"])
        .map(str::to_owned)
        .collect()
}

/// `config set check_interval_hours 6`: recovery under the lock, committing nothing.
pub fn config_set() -> Vec<String> {
    json_args(&["config", "set", "check_interval_hours", "6"])
}

/// What [`assert_settled`] saw of one stopped repair.
#[derive(Debug, Clone, PartialEq)]
pub struct Settled {
    pub phase: CrashPhase,
    pub end: End,
    /// The recovering command's `journal_recovered` messages about the repair, its
    /// transaction written `<tx>`.
    pub recovered: Vec<String>,
    /// The subject once recovered.
    pub subject: BTreeMap<String, Option<String>>,
}

/// The oracle every crash and failure of a repair is held to (s7), right after it:
///
/// 1. the phase it stopped at says the end: before `done` the before state, from it the
///    after state;
/// 2. `status` reports `journal_pending` for a journal recovery will settle, and writes
///    nothing (`<state>`, `<config>`, `<cache>`, the client dir);
/// 3. `recover` (a lock-taking command) succeeds, with `journal_recovered` exactly when
///    there was something to roll back or forward;
/// 4. the whole subject is that end's, never a mixture, and 6. every preserved original
///    is accounted for (`accounted`);
/// 5. no journal is left, nor a live in-flight lock (a crash before the journal leaves
///    a dead one, which the orphan sweep takes once it is an hour old);
/// 7. no `.omaddons-*` entry is left outside the repair's own roots;
/// 8. history holds the repair's line exactly in the after state, well formed, one
///    `repairs[]` entry per primitive;
/// 9. nothing names a path below `WTF/`, and every run named no planted account.
#[track_caller]
pub fn assert_settled(log: &TestLog, r: &RepairScene, recover: &[String]) -> Settled {
    let phase = r.phase();
    let journal = r.journal_lines();
    log.json("phase", &format!("{phase:?}"));
    let label = format!("{} stopped at {phase:?}", r.kind.label());
    let transcript = log.transcript_path().display().to_string();
    let tx = r.tx();
    let naming = |envelope: &Value, code: &str| -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            // ubs:ignore — comparing warning codes, not secrets
            .filter(|warning| warning["code"] == code)
            .filter_map(|warning| warning["message"].as_str())
            .filter(|message| message.contains("repair transaction"))
            .map(|message| match &tx {
                Some(tx) => message.replace(tx.as_str(), "<tx>"),
                None => message.to_owned(),
            })
            .collect()
    };
    let end = r.end_at(phase);
    // 2. The reader.
    for (tree, root) in r.trees() {
        log.tree(tree, &root);
    }
    let read = log.run(&r.s, &json_args(&["status"]));
    read.ok().assert_private();
    let waits =
        journal.is_some() && !matches!(phase, CrashPhase::Committed | CrashPhase::RolledBack);
    assert_eq!(
        naming(read.envelope(), "journal_pending").len(),
        usize::from(waits),
        "{label}: what the reader reports\ntranscript: {transcript}"
    );
    for (tree, root) in r.trees() {
        log.assert_unchanged(tree, &root);
    }
    // 3. The recovery.
    let ran = log.run(&r.s, recover);
    ran.ok().assert_private();
    let recovered = naming(ran.envelope(), "journal_recovered");
    let acts = matches!(
        phase,
        CrashPhase::Prepared | CrashPhase::Stepped(_) | CrashPhase::Done
    );
    assert_eq!(
        recovered.len(),
        usize::from(acts),
        "{label}: what recovery reports: {recovered:?}\ntranscript: {transcript}"
    );
    // 1, 4, 6. The whole end state, every original accounted for.
    let subject = r.subject();
    log.json("subject after recovery", &subject);
    let problems = r.accounted(end);
    assert!(
        problems.is_empty(),
        "{label}: not the whole {end:?} state: {problems:#?}\ntranscript: {transcript}"
    );
    // 5. Nothing left to recover.
    assert_eq!(
        r.journal(),
        None,
        "{label}: a journal is left\ntranscript: {transcript}"
    );
    if let Some(tx) = &tx {
        assert!(!r.inflight_held(tx), "{label}: a live in-flight lock");
        // ubs:ignore — comparing crash phases, not secrets
        if phase != CrashPhase::NoJournal {
            assert!(
                !r.state().join(format!("inflight/{tx}.lock")).exists(),
                "{label}: the in-flight lock is left\ntranscript: {transcript}"
            );
        }
    }
    // 7. No temporary entry stranded.
    assert_eq!(
        r.leftovers(),
        Vec::<PathBuf>::new(),
        "{label}: leftovers\ntranscript: {transcript}"
    );
    // 8. History.
    let lines = r.repair_history();
    assert_eq!(
        lines.len(),
        usize::from(end == End::After), // ubs:ignore — comparing end states, not secrets
        "{label}: repair history lines\ntranscript: {transcript}"
    );
    for line in &lines {
        let entry: omaddons::state::history::HistoryEntry =
            serde_json::from_value(line.clone()).unwrap();
        assert!(entry.well_formed(), "{label}: {line}");
        assert_eq!(
            entry.repairs.len(),
            r.kind.primitives() as usize,
            "{label}: one repairs[] entry per primitive: {line}"
        );
    }
    // 9. Nothing below `WTF/`.
    let mut wtf = r.wtf_named();
    wtf.extend(
        journal
            .iter()
            .flatten()
            .filter(|line| names_wtf(line))
            .cloned(),
    );
    assert_eq!(wtf, Vec::<String>::new(), "{label}: a repair named WTF/");
    Settled {
        phase,
        end,
        recovered,
        subject,
    }
}

/// The crash point a spec addresses (`prepared`, `step@2`, `executed@1`, …), when it
/// names one; the `checked` sweep's are attributed from the journal ([`attributed`]).
pub fn point_of(spec: &str) -> Option<String> {
    let (name, rest) = spec.split_once('=')?;
    let at = rest.split_once('@').map(|(_, n)| n);
    let point = name
        .strip_prefix("doctor.repair.")
        .or_else(|| name.strip_prefix("fsx.mutate."))?;
    Some(match at {
        Some(n) => format!("{point}@{n}"),
        None => point.to_owned(),
    })
}

/// The `checked` crash's primitive: the journal holds step `k`'s line and `actions.jsonl`
/// `k - 1` lines, so primitive `k` was about to run. Any other `checked` hit is an
/// untracked write (a root, a journal or history line, cleanup), no primitive's.
pub fn attributed(r: &RepairScene) -> Option<String> {
    match r.phase() {
        CrashPhase::Stepped(k) if r.actions_lines().len() + 1 == k as usize => {
            Some(format!("checked@{k}"))
        }
        _ => None,
    }
}

/// Every crash point a kind's repair has: each primitive's five boundaries, and the
/// commit's four phase points.
pub fn crash_points(kind: Kind) -> std::collections::BTreeSet<String> {
    points_over(kind.primitives())
}

/// [`crash_points`] of a repair transaction of `n` primitives.
fn points_over(n: u32) -> std::collections::BTreeSet<String> {
    let mut points: std::collections::BTreeSet<String> =
        ["prepared", "done", "history", "committed"]
            .map(str::to_owned)
            .into();
    for k in 1..=n {
        for boundary in ["step", "checked", "backed_up", "executed", "logged"] {
            points.insert(format!("{boundary}@{k}"));
        }
    }
    points
}

// ---------------------------------------------------------------------------------------
// The undo of a repair (E7 s5): the kind's repair committed unarmed (the subject is `R`,
// E5's after state), then `omaddons undo <T>` through the binary, crashed by a real
// `abort` or failed by an injected error. The repair's own journal is pruned once it
// commits, so the one repair journal left is the undo's. The oracle swaps E5's: before
// the undo's durable `done` the whole `R`, from it the whole `O` (E5's before state).

impl RepairScene {
    /// The kind's scene with its repair committed unarmed: the subject is `R`.
    pub fn repaired(log: &TestLog, kind: Kind) -> Self {
        let r = Self::new(kind);
        let Child::Ended(outcome) = r.child(log, "") else {
            panic!("{}: the unarmed repair crashed", kind.label()); // ubs:ignore — fails the test
        };
        assert_eq!(outcome["outcome"], "committed", "{}", kind.label());
        assert_eq!(
            r.accounted(End::After),
            Vec::<String>::new(),
            "{}",
            kind.label()
        );
        r
    }

    /// `omaddons undo <the repair> --json --progress json`, `spec` armed (empty: none).
    pub fn undo(&self, log: &TestLog, spec: &str) -> super::tlog::Ran {
        let tx = self.tx().unwrap_or_default();
        let mut cmd = log.cmd(&self.s, &json_args(&["undo", &tx]));
        if !spec.is_empty() {
            cmd = cmd.failpoints(spec);
        }
        cmd.run()
    }

    /// The undo's transaction, once its journal or in-flight lock is on disk.
    pub fn undo_tx(&self) -> Option<String> {
        self.journal()
            .and_then(|path| {
                let name = path.file_name()?.to_str()?;
                name.strip_suffix(".repair.jsonl").map(str::to_owned)
            })
            .or_else(|| self.inflight_tx())
    }

    /// The undo's own run directory: the one besides the repair's (and no stage).
    pub fn undo_run_dir(&self) -> Option<PathBuf> {
        // Run names begin with their UTC start time: the newest is the greatest, so after
        // an undo of the undo, the redo's.
        listed(&self.state().join("doctor/runs"))
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name != self.run_id && !name.starts_with('.') // ubs:ignore — run names
                    })
            })
            .max()
    }

    /// The undo's `actions.jsonl` lines.
    pub fn undo_actions_lines(&self) -> Vec<String> {
        self.undo_run_dir()
            .and_then(|dir| fs::read_to_string(dir.join("actions.jsonl")).ok())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// History's `undo` lines that revert the repair.
    pub fn undo_history(&self) -> Vec<Value> {
        self.tx()
            .map(|tx| self.undo_lines_of(&tx))
            .unwrap_or_default()
    }

    /// History's `undo` lines that revert `tx`.
    fn undo_lines_of(&self, tx: &str) -> Vec<Value> {
        self.history_lines()
            .into_iter()
            // ubs:ignore — comparing history kinds and transaction ids, not secrets
            .filter(|line| line["kind"] == "undo" && line["reverts"] == tx)
            .collect()
    }

    /// The undo `U` of the repair, once it committed.
    pub fn undo_line_tx(&self) -> Option<String> {
        match self.undo_history().as_slice() {
            [one] => one["tx"].as_str().map(str::to_owned),
            _ => None,
        }
    }

    /// History's `undo` lines that revert the undo `U` (redos of the repair).
    pub fn redo_history(&self) -> Vec<Value> {
        self.undo_line_tx()
            .map(|undo| self.undo_lines_of(&undo))
            .unwrap_or_default()
    }

    /// Where an undo stopped at `phase` must end: before its durable `done` the whole
    /// `R` ([`End::After`]), from it the whole `O` ([`End::Before`]). With no journal,
    /// history says which.
    pub fn undo_end_at(&self, phase: CrashPhase) -> End {
        match phase {
            CrashPhase::Done | CrashPhase::Committed => End::Before,
            CrashPhase::NoJournal if self.undo_history().len() == 1 => End::Before,
            _ => End::After,
        }
    }

    /// The scene with its repair committed and then undone unarmed: the subject is `O`.
    pub fn undone(log: &TestLog, kind: Kind) -> Self {
        let r = Self::repaired(log, kind);
        r.undo(log, "").ok();
        assert_eq!(
            r.accounted(End::Before),
            Vec::<String>::new(),
            "{}",
            kind.label()
        );
        assert!(
            r.undo_line_tx().is_some(),
            "{}: the undo's line",
            kind.label()
        );
        r
    }

    /// `omaddons undo <U> --json --progress json`, the undo of the repair's undo, `spec`
    /// armed (empty: none).
    pub fn redo(&self, log: &TestLog, spec: &str) -> super::tlog::Ran {
        let undo = self.undo_line_tx().unwrap_or_default();
        let mut cmd = log.cmd(&self.s, &json_args(&["undo", &undo]));
        if !spec.is_empty() {
            cmd = cmd.failpoints(spec);
        }
        cmd.run()
    }

    /// The problems with the originals the undo itself preserves, in `O`: the bytes a
    /// write over `R` replaced kept as the undo's backup, and a file the repair created
    /// set aside in the undo's quarantine. Empty when both are there.
    pub fn undo_accounted(&self) -> Vec<String> {
        let Some(dir) = self.undo_run_dir() else {
            return vec!["the undo has no run directory".to_owned()];
        };
        let mut problems = Vec::new();
        match self.kind {
            Kind::WriteOver => {
                let backup = dir.join("backups/1");
                // ubs:ignore — comparing a test file's bytes, not secrets
                if fs::read(&backup).ok().as_deref() != Some(GUIDE_V2) {
                    problems.push(format!("{} does not hold guide v2", backup.display()));
                }
            }
            Kind::WriteCreate => {
                let rel = self
                    .skill_file()
                    .strip_prefix(self.env().home())
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                let parked = dir.join("quarantine/skill").join(rel);
                // ubs:ignore — comparing a test file's bytes, not secrets
                if fs::read(&parked).ok().as_deref() != Some(GUIDE_V2) {
                    problems.push(format!("{} does not hold guide v2", parked.display()));
                }
            }
            _ => {}
        }
        problems
    }
}

/// What [`assert_undo_settled`] saw of one stopped undo.
#[derive(Debug, Clone, PartialEq)]
pub struct UndoSettled {
    pub phase: CrashPhase,
    pub end: End,
    pub recovered: usize,
}

/// The oracle every crash and failure of a repair's undo is held to, right after it:
///
/// 1. the phase the undo's journal stopped at says the end: before its `done` the whole
///    `R`, from it the whole `O`;
/// 2. `status` reports `journal_pending` for a journal recovery will settle, and writes
///    nothing;
/// 3. `recover` succeeds, with `journal_recovered` exactly when there was something to
///    roll back or forward;
/// 4. the whole subject is that end's, never a mixture, and every preserved original is
///    accounted for: in `R` the repair's backups and quarantine, in `O` the undo's own;
/// 5. no journal is left, nor the undo's in-flight lock;
/// 6. no `.omaddons-*` entry is left outside the repairs' own roots;
/// 7. history holds the repair's line once, and the undo's line once in `O` (one
///    `repairs[]` entry per inverse) and never in `R`;
/// 8. nothing names a path below `WTF/`, and no run named a planted account.
#[track_caller]
pub fn assert_undo_settled(log: &TestLog, r: &RepairScene, recover: &[String]) -> UndoSettled {
    settle(log, r, recover, Depth::Undo)
}

/// [`assert_undo_settled`] for the undo of the undo `U` (a redo of the repair), its ends
/// swapped: before the redo's durable `done` the whole `O`, from it the whole `R`; the
/// undo's line stays once, and the redo's appears once from `done` on.
#[track_caller]
pub fn assert_redo_settled(log: &TestLog, r: &RepairScene, recover: &[String]) -> UndoSettled {
    settle(log, r, recover, Depth::Redo)
}

/// Which line a stopped undo reverts: the repair's, or the repair's undo's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    Undo,
    Redo,
}

#[track_caller]
fn settle(log: &TestLog, r: &RepairScene, recover: &[String], depth: Depth) -> UndoSettled {
    let phase = r.phase();
    let undo_tx = r.undo_tx();
    let from_done = match phase {
        CrashPhase::Done | CrashPhase::Committed => true,
        CrashPhase::NoJournal => match depth {
            Depth::Undo => r.undo_history().len() == 1,
            Depth::Redo => r.redo_history().len() == 1,
        },
        _ => false,
    };
    // An undo leaves `O` from its `done`, a redo `R`; each leaves the other before it.
    let end = match (depth, from_done) {
        (Depth::Undo, true) | (Depth::Redo, false) => End::Before,
        (Depth::Undo, false) | (Depth::Redo, true) => End::After,
    };
    log.json("undo phase", &format!("{phase:?}"));
    let what = match depth {
        Depth::Undo => "the undo",
        Depth::Redo => "the undo of the undo",
    };
    let label = format!("{what} of {} stopped at {phase:?}", r.kind.label());
    let transcript = log.transcript_path().display().to_string();
    let naming = |envelope: &Value, code: &str| -> usize {
        envelope["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            // ubs:ignore — comparing warning codes, not secrets
            .filter(|warning| warning["code"] == code)
            .filter_map(|warning| warning["message"].as_str())
            .filter(|message| undo_tx.as_deref().is_some_and(|tx| message.contains(tx)))
            .count()
    };
    // 2. The reader.
    for (tree, root) in r.trees() {
        log.tree(tree, &root);
    }
    let read = log.run(&r.s, &json_args(&["status"]));
    read.ok().assert_private();
    let waits =
        r.journal().is_some() && !matches!(phase, CrashPhase::Committed | CrashPhase::RolledBack);
    assert_eq!(
        naming(read.envelope(), "journal_pending"),
        usize::from(waits),
        "{label}: what the reader reports\ntranscript: {transcript}"
    );
    for (tree, root) in r.trees() {
        log.assert_unchanged(tree, &root);
    }
    // 3. The recovery.
    let ran = log.run(&r.s, recover);
    ran.ok().assert_private();
    let recovered = naming(ran.envelope(), "journal_recovered");
    let acts = matches!(
        phase,
        CrashPhase::Prepared | CrashPhase::Stepped(_) | CrashPhase::Done
    );
    assert_eq!(
        recovered,
        usize::from(acts),
        "{label}: what recovery reports\ntranscript: {transcript}"
    );
    // 4. The whole end state, every original accounted for.
    let subject = r.subject();
    log.json("subject after recovery", &subject);
    let mut problems = r.accounted(end);
    // `R` is the repair's after state, `O` its before state.
    let wanted = match end {
        End::After => "R",
        End::Before => {
            // The undo's own originals; a redo rolled back leaves the undo's, which its
            // own reference run checked.
            if matches!(depth, Depth::Undo) {
                problems.extend(r.undo_accounted());
            }
            "O"
        }
    };
    log.json(
        "crash",
        &json!({
            "kind": r.kind.label(),
            "phase": format!("{phase:?}"),
            "expected": wanted,
            "got": if problems.is_empty() { wanted } else { "mixture" },
        }),
    );
    assert!(
        problems.is_empty(),
        "{label}: not the whole {wanted} state: {problems:#?}\ntranscript: {transcript}"
    );
    // 5. Nothing left to recover.
    assert_eq!(
        r.journal(),
        None,
        "{label}: a journal is left\ntranscript: {transcript}"
    );
    if let Some(tx) = &undo_tx {
        assert!(
            !r.inflight_held(tx),
            "{label}: a live in-flight lock\ntranscript: {transcript}"
        );
        if !matches!(phase, CrashPhase::NoJournal) {
            assert!(
                !r.state().join(format!("inflight/{tx}.lock")).exists(),
                "{label}: the in-flight lock is left\ntranscript: {transcript}"
            );
        }
    }
    // 6. No temporary entry stranded.
    assert_eq!(
        r.leftovers(),
        Vec::<PathBuf>::new(),
        "{label}: leftovers\ntranscript: {transcript}"
    );
    // 7. History.
    assert_eq!(
        r.repair_history().len(),
        1,
        "{label}: the repair's line\ntranscript: {transcript}"
    );
    let undos = r.undo_history();
    let (undo_lines, redo_lines) = match depth {
        Depth::Undo => (usize::from(from_done), 0),
        Depth::Redo => (1, usize::from(from_done)),
    };
    assert_eq!(
        undos.len(),
        undo_lines,
        "{label}: the undo's lines\ntranscript: {transcript}"
    );
    let redos = r.redo_history();
    assert_eq!(
        redos.len(),
        redo_lines,
        "{label}: the redo's lines\ntranscript: {transcript}"
    );
    for line in undos.iter().chain(&redos) {
        let entry: omaddons::state::history::HistoryEntry =
            serde_json::from_value(line.clone()).unwrap();
        assert!(entry.well_formed(), "{label}: {line}");
        assert_eq!(
            entry.repairs.len(),
            r.kind.undo_inverses() as usize,
            "{label}: one repairs[] entry per inverse: {line}"
        );
    }
    // 8. Nothing below `WTF/`.
    assert_eq!(
        r.wtf_named(),
        Vec::<String>::new(),
        "{label}: a repair named WTF/"
    );
    UndoSettled {
        phase,
        end,
        recovered,
    }
}

/// Logs the crash point of one case (`crash point`: the kind, the failpoint and `k`), so
/// the `crash` record [`assert_undo_settled`] logs next reads with what fired.
pub fn log_point(log: &TestLog, kind: Kind, spec: &str) {
    let (failpoint, rest) = spec.split_once('=').unwrap_or((spec, ""));
    let k = rest
        .split_once('@')
        .and_then(|(_, n)| n.parse::<u64>().ok());
    log.json(
        "crash point",
        &json!({ "kind": kind.label(), "failpoint": failpoint, "k": k }),
    );
}

/// The crash specs on the path of a repair's undo: the run directory's and the engine's
/// before its journal, then every repair-journal point and tracked fsx boundary of its
/// inverses (one per primitive of the repair).
pub fn undo_crash_specs(kind: Kind) -> Vec<String> {
    let mut specs: Vec<String> = [
        "doctor.run.staged=abort",
        "doctor.run.published=abort",
        "engine.snapshot.prestaged=abort",
        "engine.lock.acquired=abort",
        "engine.revalidated=abort",
    ]
    .map(str::to_owned)
    .into();
    specs.extend(specs_over(kind.undo_inverses()));
    specs
}

/// Every crash point a repair's undo has: [`crash_points`], and the five before its
/// journal.
pub fn undo_crash_points(kind: Kind) -> std::collections::BTreeSet<String> {
    let mut points = points_over(kind.undo_inverses());
    points.extend(
        [
            "run.staged",
            "run.published",
            "snapshot.prestaged",
            "lock.acquired",
            "revalidated",
        ]
        .map(str::to_owned),
    );
    points
}

/// The crash point an undo spec addresses: [`point_of`], and the run directory's and the
/// engine's points before the journal.
pub fn undo_point_of(spec: &str) -> Option<String> {
    let (name, _) = spec.split_once('=')?;
    name.strip_prefix("doctor.run.")
        .map(|point| format!("run.{point}"))
        .or_else(|| name.strip_prefix("engine.").map(str::to_owned))
        .or_else(|| point_of(spec))
}

/// The `checked` crash's inverse, as [`attributed`] does for the repair: the undo's journal
/// holds step `k`'s line and its `actions.jsonl` `k - 1` lines.
pub fn undo_attributed(r: &RepairScene) -> Option<String> {
    match r.phase() {
        CrashPhase::Stepped(k) if r.undo_actions_lines().len() + 1 == k as usize => {
            Some(format!("checked@{k}"))
        }
        _ => None,
    }
}

/// `fsx.mutate.checked=abort@N` for N = 1, 2, … on the undo of a fresh repaired scene
/// each, until an undo completes without crashing. Returns each crashed scene with its N.
pub fn undo_checked_sweep(kind: Kind, log: &TestLog) -> Vec<(u64, RepairScene)> {
    let mut crashed = Vec::new();
    for n in 1..=CHECKED_SWEEP_MAX {
        let r = RepairScene::repaired(log, kind);
        let spec = format!("fsx.mutate.checked=abort@{n}");
        log.step(&format!("the undo of {} at {spec}", kind.label()));
        let ran = r.undo(log, &spec);
        if ran.run.code >= 0 {
            return crashed;
        }
        crashed.push((n, r));
    }
    panic!("an undo still crashed at fsx.mutate.checked@{CHECKED_SWEEP_MAX}"); // ubs:ignore — fails the test
}

/// [`undo_checked_sweep`] for the undo of the undo: each on a fresh scene whose repair was
/// committed and undone.
pub fn redo_checked_sweep(kind: Kind, log: &TestLog) -> Vec<(u64, RepairScene)> {
    let mut crashed = Vec::new();
    for n in 1..=CHECKED_SWEEP_MAX {
        let r = RepairScene::undone(log, kind);
        let spec = format!("fsx.mutate.checked=abort@{n}");
        log.step(&format!(
            "the undo of the undo of {} at {spec}",
            kind.label()
        ));
        let ran = r.redo(log, &spec);
        if ran.run.code >= 0 {
            return crashed;
        }
        crashed.push((n, r));
    }
    panic!("a redo still crashed at fsx.mutate.checked@{CHECKED_SWEEP_MAX}"); // ubs:ignore — fails the test
}
