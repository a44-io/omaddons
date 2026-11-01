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
                let root = match root {
                    Root::Client(dir) => json!({ "client": dir }),
                    Root::State => json!("state"),
                    Root::Config => json!("config"),
                    Root::Cache => json!("cache"),
                };
                json!({"op": "quarantine", "path": path, "root": root})
            }
        }
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
    use omaddons::engine::repair_action::{Fix, Mode, QuarantineRoot};
    use omaddons::fsx::{GlobalLock, ScopeTable};
    use omaddons::model::{Actor, RunId};
    use omaddons::state::paths::Paths;

    let var = |key: &str| std::env::var_os(key);
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
            let path = PathBuf::from(spec["path"].as_str().unwrap());
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
                _ => {
                    let root = match &spec["root"] {
                        Value::String(root) if root == "state" => QuarantineRoot::State,
                        Value::String(root) if root == "config" => QuarantineRoot::Config,
                        Value::String(root) if root == "cache" => QuarantineRoot::Cache,
                        other => QuarantineRoot::Client {
                            client_dir: PathBuf::from(other["client"].as_str().unwrap()),
                        },
                    };
                    (CORRUPT, Fix::Quarantine { path, root })
                }
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
        &planned,
    );
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
                Ok(_) => json!({"ok": true, "tx": tx}),
                Err(err) => json!({"ok": false, "tx": tx, "code": err.code.wire()}),
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
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if !spec.is_empty() {
        cmd.env("OMADDONS_FAILPOINTS", spec);
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
