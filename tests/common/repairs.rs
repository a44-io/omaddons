//! Stuck doctor repairs for the fence scenarios (E14): a repair of any fixes, committed
//! by E5's real `lower` and `commit` in a child of the test binary (`child_repair`, as
//! `common::repair` runs its kinds) and aborted after its first primitive, then its live
//! path changed so that it matches neither the step's `pre` nor its `post`; and the
//! repair journal lines no code writes (one that does not decode, one of a newer major,
//! a torn first line), planted.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::home::Home;
use super::tlog::TestLog;
use super::trees::tree_hash;

const SKILL: &str = "fm-plugins-skill-version-drift";
const FILE_MODE: &str = "fm-secrets-credential-file-mode";
const CORRUPT: &str = "fm-schemas-state-corrupt";
const SET_ASIDE: &str = "fm-concurrency_primitives-journal-step-unrecognized";

/// Where a quarantined path goes.
#[derive(Debug, Clone)]
pub enum Root {
    /// The client's own quarantine (its `.repair` snapshot half).
    Client(PathBuf),
    State,
    Config,
    Cache,
}

/// One fix, as the parent describes it to the child (E1's `Fix` is not serialized).
#[derive(Debug, Clone)]
pub enum FixSpec {
    Chmod {
        path: PathBuf,
        mode: u32,
    },
    Write {
        path: PathBuf,
        content: String,
        mode: u32,
    },
    Quarantine {
        path: PathBuf,
        root: Root,
    },
    /// F2's set-aside of the folder journal `tx` on `target`: each artifact into its
    /// quarantine root, in this order, with no guards (E14's barrier scene).
    Suspend {
        tx: String,
        target: Option<String>,
        artifacts: Vec<(PathBuf, Root)>,
    },
}

impl Root {
    fn json(&self) -> Value {
        match self {
            Self::Client(dir) => json!({ "client": dir }),
            Self::State => json!("state"),
            Self::Config => json!("config"),
            Self::Cache => json!("cache"),
        }
    }
}

impl FixSpec {
    fn json(&self) -> Value {
        match self {
            Self::Chmod { path, mode } => json!({"op": "chmod", "path": path, "mode": mode}),
            Self::Write {
                path,
                content,
                mode,
            } => json!({"op": "write", "path": path, "content": content, "mode": mode}),
            Self::Quarantine { path, root } => {
                json!({"op": "quarantine", "path": path, "root": root.json()})
            }
            Self::Suspend {
                tx,
                target,
                artifacts,
            } => {
                let artifacts: Vec<Value> = artifacts
                    .iter()
                    .map(|(path, root)| json!({"path": path, "root": root.json()}))
                    .collect();
                json!({"op": "suspend", "tx": tx, "target": target, "artifacts": artifacts})
            }
        }
    }
}

/// The child's quarantine root from its JSON form.
fn root_of(value: &Value) -> omaddons::engine::repair_action::QuarantineRoot {
    use omaddons::engine::repair_action::QuarantineRoot;
    match value {
        Value::String(root) if root == "state" => QuarantineRoot::State,
        Value::String(root) if root == "config" => QuarantineRoot::Config,
        Value::String(root) if root == "cache" => QuarantineRoot::Cache,
        other => QuarantineRoot::Client {
            client_dir: PathBuf::from(other["client"].as_str().unwrap()),
        },
    }
}

/// What a committed repair lowered to, as the child reports it so a later child can
/// revert it: its fixes, its steps, each step's resources (their text form), its
/// clients and its targets.
fn lowered_json(lowered: &omaddons::engine::repair::Lowered) -> Value {
    use omaddons::engine::repair::ResourcesWire;
    let resources: Vec<ResourcesWire> = lowered
        .steps
        .iter()
        .map(|s| ResourcesWire::of(&s.resources).unwrap())
        .collect();
    json!({
        "fixes": lowered.fixes,
        "steps": lowered.steps.iter().map(|s| &s.step).collect::<Vec<_>>(),
        "resources": resources,
        "client_dirs": lowered.client_dirs,
        "targets": lowered.targets,
    })
}

/// E7's reactivation of the set-aside `of` reported (its [`lowered_json`]), built from
/// E5's primitives as M4-D38 orders it (E14): the journal at `journal` back from its
/// quarantine under its provisional name first, then every other artifact back where it
/// was, in the set-aside's order, then the activation, the provisional name renamed to the
/// journal's own. Each step is the inverse of the set-aside's: what the set-aside left at
/// the destination is its `pre`, what it took from the source its `post`. No fix kind
/// lowers this before E7 lands, so the scene writes the primitives itself.
fn reactivation(
    paths: &omaddons::state::paths::Paths,
    of: &Value,
    journal: &Path,
) -> omaddons::engine::repair::Lowered {
    use omaddons::engine::repair::{Lowered, LoweredStep, provisional_journal};
    use omaddons::engine::repair_action::{Endpoint, Endpoints, FixEntry, Primitive, Step};
    use omaddons::engine::resource::{PathKind, Resource, ResourceSet};

    let fixes: Vec<FixEntry> = serde_json::from_value(of["fixes"].clone()).unwrap();
    let steps: Vec<Step> = serde_json::from_value(of["steps"].clone()).unwrap();
    let tx = journal
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".jsonl"))
        .unwrap()
        .parse()
        .unwrap();
    let provisional = provisional_journal(paths, &tx);
    // (from, to, what is at `from` before, what is at `to` after, activation)
    let mut moves: Vec<(PathBuf, PathBuf, Endpoint, Endpoint, bool)> = Vec::new();
    let mut journal_back = None;
    for step in &steps {
        let (Primitive::RenameNoReplace { from, to, .. }, Endpoints::Pair { path: was, .. }) =
            (&step.op, &step.pre)
        else {
            panic!("a set-aside renames only: {step:?}"); // ubs:ignore — fails the test
        };
        let Endpoints::Pair { to: left, .. } = &step.post else {
            panic!("{step:?}"); // ubs:ignore — fails the test
        };
        // A rename keeps the entry and what it holds: the inverse's source is what the
        // set-aside found (its stamp too, which a renamed directory must carry), and its
        // destination afterwards what the set-aside left (its identity).
        if from == journal {
            journal_back = Some((to.clone(), was.clone(), left.clone()));
        } else {
            moves.push((to.clone(), from.clone(), was.clone(), left.clone(), false));
        }
    }
    let (quarantined, was, left) = journal_back.expect("the set-aside moves the journal");
    moves.insert(
        0,
        (
            quarantined,
            provisional.clone(),
            was.clone(),
            left.clone(),
            false,
        ),
    );
    moves.push((provisional, journal.to_path_buf(), was, left, true));
    let template = &steps[0];
    let mut all = ResourceSet::new();
    let lowered_steps: Vec<LoweredStep> = moves
        .into_iter()
        .zip(1..)
        .map(|((from, to, before, after, activation), seq)| {
            let mut resources = ResourceSet::new();
            for end in [&from, &to] {
                resources.write(Resource::from_canonical(end, PathKind::File).unwrap());
            }
            all.extend(&resources);
            LoweredStep {
                step: Step {
                    seq,
                    fix: template.fix,
                    fm: template.fm.clone(),
                    op: Primitive::RenameNoReplace {
                        from,
                        to,
                        activation,
                    },
                    pre: Endpoints::Pair {
                        path: before,
                        to: Endpoint::Absent,
                    },
                    post: Endpoints::Pair {
                        path: Endpoint::Absent,
                        to: after,
                    },
                    guards: Vec::new(),
                },
                resources,
                backup_bytes: 0,
                quarantine: None,
                rollback_to: None,
                content: None,
            }
        })
        .collect();
    Lowered {
        fixes,
        steps: lowered_steps,
        resources: all,
        targets: serde_json::from_value(of["targets"].clone()).unwrap(),
        client_dirs: serde_json::from_value(of["client_dirs"].clone()).unwrap(),
        roots: Vec::new(),
    }
}

/// How the child ended.
#[derive(Debug)]
pub enum Child {
    /// An `abort` killed it.
    Crashed,
    /// Its outcome: `{ok, tx, …}`.
    Ended(Value),
}

/// The child's side: when `OMADDONS_REPAIR_FIXES` names fixes, lowers and commits them as
/// one repair (no recovery first, so it can build states a `doctor fix` never would) and
/// writes its outcome to `OMADDONS_REPAIR_OUT`. Whether it ran.
pub fn child_main() -> bool {
    use omaddons::engine::repair::{
        CommitCtx, LowerCtx, PlannedFix, RepairMeta, RepairRun, RepairTx, commit, lower,
    };
    use omaddons::engine::repair_action::{
        Attribution, Fix, JournalFamily, JournalSet, Mode, SuspendedArtifact,
    };
    use omaddons::fsx::{GlobalLock, ScopeTable};
    use omaddons::model::{Actor, RunId};
    use omaddons::state::paths::Paths;

    let var = |key: &str| std::env::var_os(key);
    // A reactivation names no fixes: its input file stands in for them.
    let (Some(fixes), Some(run), Some(clients), Some(out)) = (
        var("OMADDONS_REPAIR_FIXES"),
        var("OMADDONS_REPAIR_RUN"),
        var("OMADDONS_REPAIR_CLIENTS"),
        var("OMADDONS_REPAIR_OUT"),
    ) else {
        return false;
    };
    let path = |key: &str| PathBuf::from(std::env::var_os(key).unwrap());
    let paths = Paths::with_roots(
        path("HOME"),
        path("XDG_CONFIG_HOME"),
        path("XDG_STATE_HOME"),
        path("XDG_CACHE_HOME"),
    );
    let specs: Vec<Value> = serde_json::from_str(&fixes.to_string_lossy()).unwrap();
    let client_dirs: Vec<PathBuf> = serde_json::from_str(&clients.to_string_lossy()).unwrap();
    let mode = |v: &Value| Mode::new(u32::try_from(v.as_u64().unwrap()).unwrap()).unwrap();
    let planned: Vec<PlannedFix> = specs
        .iter()
        .map(|spec| {
            // A set-aside names its artifacts, not one path.
            let path = PathBuf::from(spec["path"].as_str().unwrap_or_default());
            let (fm, fix) = match spec["op"].as_str().unwrap() {
                "chmod" => (
                    FILE_MODE,
                    Fix::Chmod {
                        path,
                        mode: mode(&spec["mode"]),
                    },
                ),
                "write" => (
                    SKILL,
                    Fix::Write {
                        path,
                        content: spec["content"].as_str().unwrap().as_bytes().to_vec(),
                        mode: mode(&spec["mode"]),
                    },
                ),
                "suspend" => (
                    SET_ASIDE,
                    Fix::Suspend {
                        journal: JournalSet {
                            tx: spec["tx"].as_str().unwrap().parse().unwrap(),
                            family: JournalFamily::Folder,
                            target: spec["target"].as_str().map(|key| key.parse().unwrap()),
                            artifacts: spec["artifacts"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|artifact| SuspendedArtifact {
                                    path: PathBuf::from(artifact["path"].as_str().unwrap()),
                                    root: root_of(&artifact["root"]),
                                })
                                .collect(),
                            guards: Vec::new(),
                            attribution: Attribution::Complete,
                            unattributed: Vec::new(),
                        },
                    },
                ),
                _ => (
                    CORRUPT,
                    Fix::Quarantine {
                        path,
                        root: root_of(&spec["root"]),
                    },
                ),
            };
            PlannedFix { fm: fm.into(), fix }
        })
        .collect();
    let clients: Vec<_> = client_dirs
        .iter()
        .map(|dir| {
            let key = omaddons::discover::registry::read_marker(dir).unwrap();
            (dir.clone(), key)
        })
        .collect();
    let run_id = run.to_string_lossy().into_owned();
    let run = RepairRun {
        dir: paths.doctor_runs_dir().join(&run_id),
        id: run_id,
    };
    let skill_dirs = [paths.skill_dir()];
    let scopes = ScopeTable::new(&paths, &client_dirs, None).unwrap();
    let rtx = RepairTx::begin(&paths).unwrap();
    let tx = rtx.tx();
    // A reactivation is E7's to lower; the scene builds it from the set-aside it reverts.
    let lowered = match var("OMADDONS_REPAIR_REACTIVATE") {
        Some(input) => {
            let input: Value = serde_json::from_slice(&fs::read(input).unwrap()).unwrap();
            let journal = PathBuf::from(input["journal"].as_str().unwrap());
            Ok(reactivation(&paths, &input["of"], &journal))
        }
        None => lower(
            &LowerCtx {
                paths: &paths,
                tx,
                run: &run,
                clients: &clients,
                skill_dirs: &skill_dirs,
                scopes: &scopes,
                mount_id: &omaddons::fsx::mount_id,
            },
            &planned,
        ),
    };
    let outcome = match lowered {
        Err(err) => json!({"ok": false, "tx": tx, "stage": "lower", "error": format!("{err:?}")}),
        Ok(lowered) => {
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
            match commit(&ctx, rtx, &lowered, &meta) {
                Ok((outcome, cleanup)) => {
                    drop(lock);
                    cleanup.finish();
                    let committed = matches!(
                        outcome,
                        omaddons::engine::repair::RepairOutcome::Committed { .. }
                    );
                    json!({"ok": committed, "tx": tx, "lowered": lowered_json(&lowered)})
                }
                Err(err) => {
                    json!({"ok": false, "tx": tx, "code": err.code.wire(), "message": err.message})
                }
            }
        }
    };
    fs::write(out, outcome.to_string()).unwrap();
    true
}

/// Runs `fixes` as one repair in a child of the test binary on `home`, with `spec` armed
/// (`OMADDONS_FAILPOINTS`), the published doctor run `run_id` and the registered
/// `clients` a repair may touch.
pub fn run_child(
    home: &Home,
    log: &TestLog,
    run_id: &str,
    clients: &[PathBuf],
    fixes: &[FixSpec],
    spec: &str,
) -> Child {
    run_child_with(home, log, run_id, clients, fixes, spec, None)
}

/// [`run_child`] for the reactivation (E7's undo of a set-aside, built by the scene) of
/// the set-aside whose child outcome's `lowered` is `of`, for the journal at `journal`.
pub fn reactivate_child(
    home: &Home,
    log: &TestLog,
    run_id: &str,
    clients: &[PathBuf],
    of: &Value,
    journal: &Path,
    spec: &str,
) -> Child {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let input = home.env.root().join(format!(
        "reactivation-{}.json",
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::write(&input, json!({"of": of, "journal": journal}).to_string()).unwrap();
    run_child_with(home, log, run_id, clients, &[], spec, Some(&input))
}

fn run_child_with(
    home: &Home,
    log: &TestLog,
    run_id: &str,
    clients: &[PathBuf],
    fixes: &[FixSpec],
    spec: &str,
    reactivate: Option<&Path>,
) -> Child {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let out = home.env.root().join(format!(
        "fence-repair-{}.json",
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let specs: Vec<Value> = fixes.iter().map(FixSpec::json).collect();
    log.json("repair fixes", &specs);
    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe); // ubs:ignore — the test binary running itself
    cmd.args([
        "child_repair",
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    home.env.apply_to(&mut cmd);
    cmd.env("OMADDONS_REPAIR_FIXES", Value::from(specs).to_string())
        .env("OMADDONS_REPAIR_RUN", run_id)
        .env("OMADDONS_REPAIR_CLIENTS", json!(clients).to_string())
        .env("OMADDONS_REPAIR_OUT", &out)
        .env_remove("OMADDONS_FAILPOINTS")
        .env_remove("OMADDONS_REPAIR_REACTIVATE")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if !spec.is_empty() {
        cmd.env("OMADDONS_FAILPOINTS", spec);
    }
    if let Some(input) = reactivate {
        cmd.env("OMADDONS_REPAIR_REACTIVATE", input);
    }
    let output = cmd.output().unwrap();
    log.note(format!(
        "child_repair with OMADDONS_FAILPOINTS={spec:?}: {}",
        output.status
    ));
    let stderr = String::from_utf8_lossy(&output.stderr);
    match output.status.code() {
        None => Child::Crashed,
        Some(0) => {
            let outcome: Value = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
            log.json("child outcome", &outcome);
            Child::Ended(outcome)
        }
        Some(code) => panic!("child_repair exited {code}: {stderr}"), // ubs:ignore — fails the test
    }
}

/// A doctor run published on `home` (`omaddons doctor`), for a repair's backups and
/// quarantine: its id.
pub fn doctor_run(home: &Home) -> String {
    let run = home.run(&["doctor", "--json"]);
    run.envelope()["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("doctor published no run: {}", run.context())) // ubs:ignore — fails the test
        .to_owned()
}

/// `<state>/omaddons/journal`.
pub fn journal_dir(home: &Home) -> PathBuf {
    home.env.state().join("omaddons/journal")
}

/// Every file of the journal directory, sorted.
pub fn journals(home: &Home) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(journal_dir(home))
        .map(|listing| listing.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default();
    found.sort();
    found
}

/// The transactions of the repair journals on `home`.
pub fn repair_txs(home: &Home) -> Vec<String> {
    journals(home)
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            name.strip_suffix(".repair.jsonl").map(str::to_owned)
        })
        .collect()
}

/// A stuck repair: its transaction and its journal.
#[derive(Debug, Clone)]
pub struct Stuck {
    pub tx: String,
    pub journal: PathBuf,
}

/// The repair of `fix`, aborted after its first primitive ran (its `step` line durable,
/// no `done`), then `disturb` changes the live path so it matches neither the step's
/// `pre` nor its `post`: the next recovery stops at it (`step_unrecognized`).
pub fn stuck(
    home: &Home,
    log: &TestLog,
    run_id: &str,
    clients: &[PathBuf],
    fix: FixSpec,
    disturb: impl FnOnce(),
) -> Stuck {
    let before = repair_txs(home);
    let ended = run_child(
        home,
        log,
        run_id,
        clients,
        &[fix],
        "fsx.mutate.executed=abort@1",
    );
    assert!(
        matches!(ended, Child::Crashed),
        "the repair was killed after its first primitive: {ended:?}"
    );
    let tx = repair_txs(home)
        .into_iter()
        .find(|tx| !before.contains(tx))
        .expect("the crashed repair left its journal");
    disturb();
    let journal = journal_dir(home).join(format!("{tx}.repair.jsonl"));
    let lines: Vec<String> = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    log.json("stuck repair journal", &lines);
    Stuck { tx, journal }
}

/// Writes the repair journal `<tx>.repair.jsonl` holding `text`, and an in-flight lock
/// nobody holds: its writer is gone.
fn plant(home: &Home, text: &str) -> String {
    let tx = omaddons::model::TxId::new().to_string();
    let dir = journal_dir(home);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{tx}.repair.jsonl")),
        text.replace("<tx>", &tx),
    )
    .unwrap();
    let inflight = home.env.state().join("omaddons/inflight");
    fs::create_dir_all(&inflight).unwrap();
    fs::write(inflight.join(format!("{tx}.lock")), "").unwrap();
    tx
}

/// A complete `prepared` line that does not decode (its primitives are not a list).
pub fn plant_undecodable_prepared(home: &Home) -> String {
    plant(
        home,
        "{\"type\":\"prepared\",\"format\":\"1.0\",\"tx\":\"<tx>\",\"primitives\":\"not a list\"}\n",
    )
}

/// A complete `prepared` line of a newer major format: whatever else it holds, this
/// build never decodes it (its format is read on its own).
pub fn plant_newer_major(home: &Home) -> String {
    plant(
        home,
        "{\"type\":\"prepared\",\"format\":\"9.0\",\"tx\":\"<tx>\",\"primitives\":[]}\n",
    )
}

/// A first line a crash cut short: no complete line, nothing ran.
pub fn plant_torn_first_line(home: &Home) -> String {
    plant(home, "{\"type\":\"prep")
}

/// What a refused command must leave as it was, by label: the config directory, each
/// client, the records, snapshots, in-flight locks, history, registry, blobs and journals.
/// Derived files a read command or a dry run writes (the request log, the discovery cache,
/// the history projection, the doorbell, the plan store) are left out.
pub fn subject_hashes(home: &Home, clients: &[&str]) -> BTreeMap<String, String> {
    let state = home.env.state().join("omaddons");
    let mut subjects: Vec<(String, PathBuf)> = vec![
        ("config".into(), home.env.config().join("omaddons")),
        ("targets".into(), state.join("targets")),
        ("snapshots".into(), state.join("snapshots")),
        ("inflight".into(), state.join("inflight")),
        ("history".into(), state.join("history.jsonl")),
        ("registry".into(), state.join("registry.json")),
        ("blobs".into(), home.env.cache().join("omaddons/blobs")),
        ("journal".into(), state.join("journal")),
    ];
    for client in clients {
        subjects.push(((*client).to_owned(), home.client(client)));
    }
    subjects
        .into_iter()
        .map(|(label, path)| (label, hash_of(&path)))
        .collect()
}

/// A directory's tree hash, a file's bytes and mode, or `absent`.
pub fn hash_of(path: &Path) -> String {
    match fs::symlink_metadata(path) {
        Err(_) => "absent".to_owned(),
        Ok(meta) if meta.is_dir() => format!("D:{}", tree_hash(path)),
        Ok(meta) => {
            let digest: String = Sha256::digest(fs::read(path).unwrap_or_default())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("F:{digest}:{:o}", meta.permissions().mode() & 0o7777)
        }
    }
}
