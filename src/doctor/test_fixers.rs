//! The doctor's test-only failure modes (E6 s2): three states only a test plants, so the
//! fix runtime can be driven to every terminal state before any real fixer exists. They
//! are compiled into debug builds only, and registered only when `OMADDONS_TEST_FIXERS=1`
//! (`registry::failure_modes`), as failpoints are read only by debug builds.
//!
//! - `fm-test-write`, the ordinary auto-fixer: a `*.write` file below
//!   `<state>/doctor-test/` that does not hold `repaired\n` (a `*.residual.write` is
//!   rewritten `still broken\n`, so verification finds it again), or a
//!   `<name>.write.missing` marker with no `<name>.write` beside it.
//! - `fm-test-quarantine`, gated as F2's set-aside (only with `--force --yes`): a
//!   `*.quarantine` file or folder below a `doctor-test/` of the state, config or cache
//!   directory or of a client's `<meta>`.
//! - `fm-test-flatten`, disabled as F5's flatten, refused while the client runs: an
//!   unmanaged `AddOns/TestWrap*` folder whose only entry is one folder.
//! - `fm-test-chmod` (E7), an ordinary auto-fixer: a `*.chmod` file below
//!   `<config>/doctor-test/` whose mode is not 0600, which it chmods to 0600.
//! - `fm-test-suspend` (E7), gated as F2's set-aside: a request `<tx>.suspend` below
//!   `<state>/doctor-test/` naming `<target key> [<folder> … | WTF]`, for a journal `<tx>`
//!   still in `<state>/journal/`. It sets the journal's artifacts aside, guarded by the
//!   target's generation and marker and the epochs of the folders named (or of `WTF/`), so
//!   it works on a readable, an unreadable and a newer journal alike.
//!
//! Detectors only read, through E3's bounded walker, at most three levels deep, and never
//! record content (M4-D42); fixers return `Fix` values.

#![cfg(debug_assertions)]

use std::path::{Path, PathBuf};

use super::budget::{Budget, EntryKind, WalkStep, list_dir, walk};
use super::finding::{Confidence, Detected, Evidence, Finding, Observation, Subject, Subsystem};
use super::registry::{
    Cardinality, FailureMode, FixerPrecondition, FixerSpec, Refusal, ScopeName, Tier,
};
use super::selection::Selection;
use super::view::DoctorView;
use crate::engine::repair::guard;
use crate::engine::repair_action::{
    Attribution, Fix, FixKind, JournalFamily, JournalSet, Mode, QuarantineRoot, SuspendedArtifact,
};
use crate::model::{FolderName, TargetKey, TxId};
use crate::state::paths::{ClientPaths, Paths};

pub use super::registry::ids::{
    TEST_CHMOD as CHMOD, TEST_FLATTEN as FLATTEN, TEST_QUARANTINE as QUARANTINE,
    TEST_SUSPEND as SUSPEND, TEST_WRITE as WRITE,
};

/// The mode `fm-test-chmod` sets.
pub const CHMOD_MODE: u32 = 0o600;
/// The most a `*.suspend` request may hold.
const REQUEST_MAX_BYTES: u64 = 4096;

/// The folder below each root a test plants its subjects in.
pub const TEST_DIR: &str = "doctor-test";
/// What `fm-test-write` repairs a file to.
pub const REPAIRED: &[u8] = b"repaired\n";
/// What it writes into a `*.residual.write`, which therefore stays broken.
pub const STILL_BROKEN: &[u8] = b"still broken\n";
/// The prefix of a folder `fm-test-flatten` lifts out of.
pub const WRAPPER_PREFIX: &str = "TestWrap";

/// How deep below a `doctor-test/` a detector looks.
const MAX_DEPTH: usize = 3;

const WHY: &str = "It exists only to exercise doctor fix.";

/// The test-only rows, in this order: write, quarantine, flatten, chmod, suspend.
pub static TEST_FAILURE_MODES: &[FailureMode] = &[
    FailureMode {
        id: WRITE,
        subsystem: Subsystem::StateFiles,
        severity: super::finding::Severity::P3,
        title: "a test-only state for exercising doctor fix (debug builds with OMADDONS_TEST_FIXERS=1)",
        description: "A file below the state directory's doctor-test folder that does not hold what the test fixer writes.",
        why: WHY,
        tier: Tier::Default,
        online: false,
        cost_ms: 1,
        requires: &[],
        detect: Some(detect_write),
        fix: Some(FixerSpec {
            plan: plan_write,
            writes_to: &[ScopeName::State],
            ops: &[FixKind::Write],
            enabled: true,
            needs_force: false,
            cardinality: Cardinality::Low,
            preconditions: &[FixerPrecondition::LockHeld],
        }),
        manual: None,
    },
    FailureMode {
        id: QUARANTINE,
        subsystem: Subsystem::StateFiles,
        severity: super::finding::Severity::P3,
        title: "a test-only state for exercising doctor fix (debug builds with OMADDONS_TEST_FIXERS=1)",
        description: "A file or folder named *.quarantine below a doctor-test folder, which the test fixer sets aside.",
        why: WHY,
        tier: Tier::Default,
        online: false,
        cost_ms: 1,
        requires: &[],
        detect: Some(detect_quarantine),
        fix: Some(FixerSpec {
            plan: plan_quarantine,
            writes_to: &[
                ScopeName::State,
                ScopeName::Config,
                ScopeName::Cache,
                ScopeName::ClientMeta,
            ],
            ops: &[FixKind::Quarantine],
            enabled: false,
            needs_force: true,
            cardinality: Cardinality::Low,
            preconditions: &[FixerPrecondition::LockHeld, FixerPrecondition::SameFs],
        }),
        manual: None,
    },
    FailureMode {
        id: FLATTEN,
        subsystem: Subsystem::ExternalArtifacts,
        severity: super::finding::Severity::P3,
        title: "a test-only state for exercising doctor fix (debug builds with OMADDONS_TEST_FIXERS=1)",
        description: "An unmanaged TestWrap folder in a client's AddOns whose only entry is one folder, which the test fixer lifts out.",
        why: WHY,
        tier: Tier::Default,
        online: false,
        cost_ms: 1,
        requires: &[],
        detect: Some(detect_flatten),
        fix: Some(FixerSpec {
            plan: plan_flatten,
            writes_to: &[ScopeName::AddOns, ScopeName::ClientMeta],
            ops: &[FixKind::Flatten],
            enabled: false,
            needs_force: false,
            cardinality: Cardinality::Low,
            preconditions: &[
                FixerPrecondition::LockHeld,
                FixerPrecondition::ClientNotRunning,
            ],
        }),
        manual: None,
    },
    FailureMode {
        id: CHMOD,
        subsystem: Subsystem::Permissions,
        severity: super::finding::Severity::P3,
        title: "a test-only state for exercising doctor fix (debug builds with OMADDONS_TEST_FIXERS=1)",
        description: "A *.chmod file below the config directory's doctor-test folder whose mode is not 0600, which the test fixer chmods.",
        why: WHY,
        tier: Tier::Default,
        online: false,
        cost_ms: 1,
        requires: &[],
        detect: Some(detect_chmod),
        fix: Some(FixerSpec {
            plan: plan_chmod,
            writes_to: &[ScopeName::Config],
            ops: &[FixKind::Chmod],
            enabled: true,
            needs_force: false,
            cardinality: Cardinality::Low,
            preconditions: &[FixerPrecondition::LockHeld],
        }),
        manual: None,
    },
    FailureMode {
        id: SUSPEND,
        subsystem: Subsystem::ConcurrencyPrimitives,
        severity: super::finding::Severity::P3,
        title: "a test-only state for exercising doctor fix (debug builds with OMADDONS_TEST_FIXERS=1)",
        description: "A journal a request below the state directory's doctor-test folder names, which the test fixer sets aside, guarded.",
        why: WHY,
        tier: Tier::Default,
        online: false,
        cost_ms: 1,
        requires: &[],
        detect: Some(detect_suspend),
        fix: Some(FixerSpec {
            plan: plan_suspend,
            writes_to: &[ScopeName::State, ScopeName::ClientMeta],
            ops: &[FixKind::Suspend],
            enabled: false,
            needs_force: true,
            cardinality: Cardinality::Low,
            preconditions: &[
                FixerPrecondition::LockHeld,
                FixerPrecondition::InflightFree,
                FixerPrecondition::ClientNotRunning,
            ],
        }),
        manual: None,
    },
];

// ---------------------------------------------------------------------------------------
// The subjects.

/// One subject found: where, about which scope, and what it is (`file`, `dir` or
/// `missing`), never its content.
fn observation(
    subject: Subject,
    path: &Path,
    kind: &str,
    others: Option<Vec<PathBuf>>,
) -> Observation {
    Observation {
        subject: subject.path(path),
        title: "a test-only state".to_owned(),
        confidence: Confidence::High,
        addon: None,
        folder: None,
        evidence: Evidence {
            path: Some(path.to_path_buf()),
            paths: others,
            observed: Some(kind.to_owned()),
            ..Evidence::default()
        },
    }
}

/// `found`, cut short when `budget` ran out.
fn detected(found: Vec<Observation>, complete: bool, budget: &Budget) -> Detected {
    if complete {
        Detected::complete(found)
    } else {
        Detected::cut_short(found, budget)
    }
}

fn name_of(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

/// Whether the file at `path` (of `len` bytes) holds exactly [`REPAIRED`]: compared
/// without keeping a byte.
fn repaired(path: &Path, len: u64) -> bool {
    len == REPAIRED.len() as u64 && std::fs::read(path).is_ok_and(|bytes| bytes == REPAIRED)
}

fn detect_write(view: &DoctorView, _: &Selection, budget: &mut Budget) -> Detected {
    let root = view.paths().state_dir().join(TEST_DIR);
    let mut found = Vec::new();
    let result = walk(&root, MAX_DEPTH, budget, |entry| {
        if entry.kind != EntryKind::File {
            return WalkStep::Continue;
        }
        let name = name_of(entry.path);
        if name.ends_with(".write") && !repaired(entry.path, entry.meta.len()) {
            found.push(observation(Subject::machine(), entry.path, "file", None));
        } else if let Some(file) = name
            .strip_suffix(".missing")
            .filter(|f| f.ends_with(".write"))
        {
            let target = entry.path.with_file_name(file);
            if std::fs::symlink_metadata(&target).is_err() {
                found.push(observation(Subject::machine(), &target, "missing", None));
            }
        }
        WalkStep::Continue
    });
    detected(found, result.complete, budget)
}

fn plan_write(_: &DoctorView, f: &Finding) -> Result<Vec<Fix>, Refusal> {
    Ok(f.evidence
        .path
        .iter()
        .map(|path| {
            let content = if name_of(path).ends_with(".residual.write") {
                STILL_BROKEN
            } else {
                REPAIRED
            };
            Fix::Write {
                path: path.clone(),
                content: content.to_vec(),
                mode: Mode::new(0o644).unwrap_or_else(|| unreachable!("0o644 is a mode")),
            }
        })
        .collect())
}

/// The registered clients the view discovered: their id, key and directory.
fn clients(view: &DoctorView) -> Vec<(String, TargetKey, PathBuf)> {
    view.world()
        .map(|world| {
            world
                .resolved
                .targets
                .iter()
                .filter_map(|d| {
                    let key = d.target.key?;
                    Some((d.target.id.clone(), key, d.target.client_dir.clone()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn detect_quarantine(view: &DoctorView, _: &Selection, budget: &mut Budget) -> Detected {
    let paths = view.paths();
    // (the root, the subject's scope, the client dir a client's subject names).
    let mut roots: Vec<(PathBuf, Subject, Option<PathBuf>)> =
        [paths.state_dir(), paths.config_dir(), paths.cache_dir()]
            .into_iter()
            .map(|dir| (dir.join(TEST_DIR), Subject::machine(), None))
            .collect();
    for (id, key, dir) in clients(view) {
        let root = ClientPaths::new(&dir).meta_dir().join(TEST_DIR);
        roots.push((root, Subject::target(&id, key), Some(dir)));
    }
    let mut found = Vec::new();
    let mut complete = true;
    for (root, subject, client) in roots {
        let result = walk(&root, MAX_DEPTH, budget, |entry| {
            if !name_of(entry.path).ends_with(".quarantine") {
                return WalkStep::Continue;
            }
            let kind = match entry.kind {
                EntryKind::File => "file",
                EntryKind::Dir => "dir",
                // A final-component symlink is never moved (fsx refuses it).
                EntryKind::Symlink | EntryKind::Other => return WalkStep::Continue,
            };
            let others = client.clone().map(|dir| vec![dir]);
            found.push(observation(subject.clone(), entry.path, kind, others));
            WalkStep::SkipChildren
        });
        if !result.complete {
            complete = false;
            break;
        }
    }
    detected(found, complete, budget)
}

fn plan_quarantine(view: &DoctorView, f: &Finding) -> Result<Vec<Fix>, Refusal> {
    let paths = view.paths();
    Ok(f.evidence
        .path
        .iter()
        .map(|path| {
            let client = f.evidence.paths.as_ref().and_then(|p| p.first()).cloned();
            let root = match client {
                Some(client_dir) => QuarantineRoot::Client { client_dir },
                None if path.starts_with(paths.config_dir()) => QuarantineRoot::Config,
                None if path.starts_with(paths.cache_dir()) => QuarantineRoot::Cache,
                None => QuarantineRoot::State,
            };
            Fix::Quarantine {
                path: path.clone(),
                root,
            }
        })
        .collect())
}

fn detect_flatten(view: &DoctorView, _: &Selection, budget: &mut Budget) -> Detected {
    let mut found = Vec::new();
    for (id, key, dir) in clients(view) {
        let addons = ClientPaths::new(&dir).addons();
        let Ok(listing) = list_dir(&addons, budget) else {
            continue;
        };
        let record = view
            .records()
            .and_then(|records| records.get(&key))
            .map(|loaded| &loaded.value);
        for (name, kind) in &listing.entries {
            let Some(name) = name.to_str().filter(|n| n.starts_with(WRAPPER_PREFIX)) else {
                continue;
            };
            let managed = FolderName::new(name)
                .ok()
                .is_some_and(|folder| record.is_some_and(|r| r.owner_of(folder.key()).is_some()));
            if *kind != EntryKind::Dir || managed {
                continue;
            }
            let wrapper = addons.join(name);
            let Ok(inside) = list_dir(&wrapper, budget) else {
                continue;
            };
            if let [(child, EntryKind::Dir)] = inside.entries.as_slice() {
                let inner = wrapper.join(child);
                found.push(observation(
                    Subject::target(&id, key),
                    &wrapper,
                    "dir",
                    Some(vec![inner]),
                ));
            }
        }
        if !listing.complete {
            return Detected::cut_short(found, budget);
        }
    }
    Detected::complete(found)
}

fn detect_chmod(view: &DoctorView, _: &Selection, budget: &mut Budget) -> Detected {
    use std::os::unix::fs::PermissionsExt;
    let root = view.paths().config_dir().join(TEST_DIR);
    let mut found = Vec::new();
    let result = walk(&root, MAX_DEPTH, budget, |entry| {
        if entry.kind == EntryKind::File
            && name_of(entry.path).ends_with(".chmod")
            && entry.meta.permissions().mode() & 0o7777 != CHMOD_MODE
        {
            found.push(observation(Subject::machine(), entry.path, "file", None));
        }
        WalkStep::Continue
    });
    detected(found, result.complete, budget)
}

fn plan_chmod(_: &DoctorView, f: &Finding) -> Result<Vec<Fix>, Refusal> {
    Ok(f.evidence
        .path
        .iter()
        .map(|path| Fix::Chmod {
            path: path.clone(),
            mode: Mode::new(CHMOD_MODE).unwrap_or_else(|| unreachable!("0o600 is a mode")),
        })
        .collect())
}

/// A `<tx>.suspend` request: the target and what to guard of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    tx: TxId,
    target: TargetKey,
    /// Folder names whose epochs to guard; `WTF` guards the client's `WTF/` instead.
    guarded: Vec<String>,
}

/// The request at `path` (`<tx>.suspend`), when it reads: bounded, its first line.
fn request(path: &Path) -> Option<Request> {
    let tx = name_of(path).strip_suffix(".suspend")?.parse().ok()?;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > REQUEST_MAX_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let mut words = text.lines().next()?.split_whitespace();
    let target = words.next()?.parse().ok()?;
    Some(Request {
        tx,
        target,
        guarded: words.map(str::to_owned).collect(),
    })
}

/// The journal file of `tx` still in `<state>/journal/`: a folder's or a settings one's.
fn journal_of(paths: &Paths, tx: &TxId) -> Option<PathBuf> {
    [paths.journal(tx), paths.journal_wtf(tx)]
        .into_iter()
        .find(|path| std::fs::symlink_metadata(path).is_ok())
}

fn detect_suspend(view: &DoctorView, _: &Selection, budget: &mut Budget) -> Detected {
    let paths = view.paths();
    let root = paths.state_dir().join(TEST_DIR);
    let mut found = Vec::new();
    let Ok(listing) = list_dir(&root, budget) else {
        return Detected::complete(found);
    };
    let clients = clients(view);
    for (name, kind) in &listing.entries {
        let path = root.join(name);
        let Some(req) = (*kind == EntryKind::File)
            .then(|| request(&path))
            .flatten()
            .filter(|req| journal_of(paths, &req.tx).is_some())
        else {
            continue;
        };
        let subject = clients
            .iter()
            .find(|(_, key, _)| *key == req.target)
            .map_or_else(Subject::machine, |(id, key, _)| Subject::target(id, *key));
        found.push(observation(subject, &path, "file", None));
    }
    if listing.complete {
        Detected::complete(found)
    } else {
        Detected::cut_short(found, budget)
    }
}

/// One guarded set-aside of the journal the request names, every artifact of it present
/// in its known places; nothing when the request, its client or a guard does not read.
fn plan_suspend(view: &DoctorView, f: &Finding) -> Result<Vec<Fix>, Refusal> {
    let paths = view.paths();
    let Some(req) = f.evidence.path.as_deref().and_then(request) else {
        return Ok(Vec::new());
    };
    let Some((_, _, client_dir)) = clients(view)
        .into_iter()
        .find(|(_, key, _)| *key == req.target)
    else {
        return Ok(Vec::new());
    };
    Ok(suspension(paths, &req, &client_dir).into_iter().collect())
}

/// The set-aside of `req`'s journal on the client at `client_dir`.
fn suspension(paths: &Paths, req: &Request, client_dir: &Path) -> Option<Fix> {
    let client = ClientPaths::new(client_dir);
    let state = || QuarantineRoot::State;
    let on_client = || QuarantineRoot::Client {
        client_dir: client_dir.to_path_buf(),
    };
    let candidates = [
        (paths.journal(&req.tx), state()),
        (paths.journal_wtf(&req.tx), state()),
        (paths.inflight_lock(&req.tx), state()),
        (paths.state_snapshot(&req.tx), state()),
        (paths.state_snapshot_wtf(&req.tx), state()),
        (client.stage(&req.tx), on_client()),
        (client.stage_wtf(&req.tx), on_client()),
        (client.snapshots(&req.tx), on_client()),
        (client.snapshots_wtf(&req.tx), on_client()),
    ];
    let artifacts: Vec<SuspendedArtifact> = candidates
        .into_iter()
        .filter(|(path, _)| std::fs::symlink_metadata(path).is_ok())
        .map(|(path, root)| SuspendedArtifact { path, root })
        .collect();
    if artifacts.is_empty() {
        return None;
    }
    let mut guards = vec![
        guard::observe_generation(paths, req.target).ok()?,
        guard::observe_marker(client_dir, req.target).ok()?,
    ];
    for name in &req.guarded {
        guards.push(if name.eq_ignore_ascii_case("WTF") {
            guard::observe_wtf_epoch(client_dir, req.target).ok()?
        } else {
            guard::observe_folder_epoch(client_dir, &FolderName::new(name).ok()?).ok()?
        });
    }
    Some(Fix::Suspend {
        journal: JournalSet {
            tx: req.tx,
            family: JournalFamily::Folder,
            target: Some(req.target),
            artifacts,
            guards,
            attribution: Attribution::Complete,
            unattributed: Vec::new(),
        },
    })
}

fn plan_flatten(_: &DoctorView, f: &Finding) -> Result<Vec<Fix>, Refusal> {
    let inner = f.evidence.paths.as_ref().and_then(|p| p.first());
    Ok(match (&f.evidence.path, inner) {
        (Some(outer), Some(inner)) => vec![Fix::Flatten {
            inner: inner.clone(),
            outer: outer.clone(),
        }],
        _ => Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::doctor::view::tests::{client, world_of};
    use crate::doctor::view::{LayerState, Preloaded};
    use crate::engine::testing::{RepairHome, repair_home, tree_hash};
    use crate::model::{AddonReason, AddonRecord, AddonState, Channel, TxId, VersionConfidence};
    use crate::state::records::TargetRecord;

    /// The view of `home`, whose planned client discovery finds registered under its key,
    /// its record managing `managed` (folder names).
    fn view_with(home: &RepairHome, managed: &[&str]) -> DoctorView {
        let mut record = TargetRecord::empty(home.key);
        if !managed.is_empty() {
            record.addons.insert(
                "managed".to_owned(),
                AddonRecord {
                    r#ref: "gh:o/managed".parse().unwrap(),
                    name: "Managed".to_owned(),
                    version: "v1".to_owned(),
                    version_confidence: VersionConfidence::Release,
                    release_id: None,
                    channel: Channel::Stable,
                    pinned: false,
                    reason: AddonReason::Explicit,
                    required_by: Vec::new(),
                    folders: managed
                        .iter()
                        .map(|f| FolderName::new(f).unwrap())
                        .collect(),
                    state: AddonState::Ok,
                    installed_at: jiff::Timestamp::now(),
                    tx_id: TxId::new(),
                    blob_sha256: None,
                    revision: None,
                    left_out: Vec::new(),
                    any_flavor: false,
                    skipped_releases: Vec::new(),
                },
            );
        }
        let bytes = crate::state::store::encode(&record).unwrap();
        std::fs::write(home.paths.target_record(&home.key), bytes).unwrap();
        let key = home.key.to_string();
        let retail = client("retail", home.client.client_dir(), Some(&key));
        DoctorView::preloaded(
            home.paths.clone(),
            Preloaded {
                world: Some(LayerState::Available(world_of(&home.paths, vec![retail]))),
                ..Preloaded::default()
            },
        )
    }

    fn view(home: &RepairHome) -> DoctorView {
        view_with(home, &[])
    }

    fn row(id: &str) -> &'static FailureMode {
        TEST_FAILURE_MODES.iter().find(|r| r.id == id).unwrap()
    }

    /// The findings of `id`'s detector on `home`, unbounded.
    fn findings(home: &RepairHome, view: &DoctorView, id: &str) -> Vec<Finding> {
        let row = row(id);
        let detect = row.detect.unwrap();
        detect(view, &Selection::all(), &mut Budget::unlimited())
            .found
            .into_iter()
            .map(|obs| row.finding(obs))
            .filter(|f| {
                f.evidence
                    .path
                    .as_ref()
                    .is_some_and(|p| p.starts_with(&home.base))
            })
            .collect()
    }

    /// `(path below base, observed kind)` of each finding.
    fn subjects(home: &RepairHome, found: &[Finding]) -> Vec<(String, String)> {
        found
            .iter()
            .map(|f| {
                let path = f.evidence.path.as_ref().unwrap();
                let below = path.strip_prefix(&home.base).unwrap().display().to_string();
                (below, f.evidence.observed.clone().unwrap_or_default())
            })
            .collect()
    }

    fn put(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn state_test(home: &RepairHome) -> PathBuf {
        home.paths.state_dir().join(TEST_DIR)
    }

    fn below(home: &RepairHome, path: &Path) -> String {
        path.strip_prefix(&home.base).unwrap().display().to_string()
    }

    type Plant = fn(&RepairHome);

    #[test]
    fn write_detector() {
        // (case, what it plants, the subjects found below the home's base).
        type Case = (&'static str, Plant, &'static [(&'static str, &'static str)]);
        let cases: [Case; 6] = [
            (
                "broken_file_is_found",
                |h| put(&state_test(h).join("a.write"), b"broken\n"),
                &[("home/.local/state/omaddons/doctor-test/a.write", "file")],
            ),
            (
                "repaired_file_is_clean",
                |h| put(&state_test(h).join("a.write"), REPAIRED),
                &[],
            ),
            (
                "missing_marker_without_file_is_found",
                |h| put(&state_test(h).join("b.write.missing"), b""),
                &[("home/.local/state/omaddons/doctor-test/b.write", "missing")],
            ),
            (
                "missing_marker_with_repaired_file_is_clean",
                |h| {
                    put(&state_test(h).join("b.write.missing"), b"");
                    put(&state_test(h).join("b.write"), REPAIRED);
                },
                &[],
            ),
            (
                "depth_four_is_ignored",
                |h| put(&state_test(h).join("1/2/3/deep.write"), b"broken\n"),
                &[],
            ),
            (
                "outside_doctor_test_is_ignored",
                |h| put(&h.paths.state_dir().join("other/a.write"), b"broken\n"),
                &[],
            ),
        ];
        for (case, plant, want) in cases {
            let home = repair_home();
            plant(&home);
            let view = view(&home);
            let found = findings(&home, &view, WRITE);
            let got = subjects(&home, &found);
            let want: Vec<(String, String)> = want
                .iter()
                .map(|(p, k)| ((*p).to_owned(), (*k).to_owned()))
                .collect();
            assert_eq!(
                got,
                want,
                "{case}: found {:?}",
                found
                    .iter()
                    .map(|f| f.instance.as_str())
                    .collect::<Vec<_>>()
            );
            for f in &found {
                let path = f.evidence.path.as_ref().unwrap().display().to_string();
                assert_eq!(
                    f.instance.as_str(),
                    format!("{WRITE}@machine:path={path}"),
                    "{case}"
                );
            }
        }
    }

    #[test]
    fn write_fixer() {
        let home = repair_home();
        let view = view(&home);
        let cases = [
            ("ordinary_file_gets_repaired_content", "a.write", REPAIRED),
            (
                "residual_file_gets_still_broken",
                "a.residual.write",
                STILL_BROKEN,
            ),
            ("missing_marker_creates_the_file", "b.write", REPAIRED),
        ];
        for (case, name, bytes) in cases {
            let path = state_test(&home).join(name);
            let f = row(WRITE).finding(observation(Subject::machine(), &path, "file", None));
            let fixes = (row(WRITE).fix.unwrap().plan)(&view, &f).unwrap();
            assert_eq!(
                fixes,
                [Fix::Write {
                    path,
                    content: bytes.to_vec(),
                    mode: Mode::new(0o644).unwrap(),
                }],
                "{case}"
            );
        }
    }

    /// The planned client's `<meta>/doctor-test/`.
    fn client_test(home: &RepairHome) -> PathBuf {
        home.client.meta_dir().join(TEST_DIR)
    }

    #[test]
    fn quarantine_detector() {
        let home = repair_home();
        let sides = [
            home.paths.state_dir().join(TEST_DIR).join("s.quarantine"),
            home.paths.config_dir().join(TEST_DIR).join("c.quarantine"),
            home.paths.cache_dir().join(TEST_DIR).join("k.quarantine"),
        ];
        for path in &sides {
            put(path, b"x");
        }
        let folder = home.paths.state_dir().join(TEST_DIR).join("d.quarantine");
        put(&folder.join("inner.quarantine"), b"x");
        symlink(
            "s.quarantine",
            home.paths.state_dir().join(TEST_DIR).join("l.quarantine"),
        )
        .unwrap();
        let client = client_test(&home).join("m.quarantine");
        put(&client, b"x");
        let view = view(&home);
        let found = findings(&home, &view, QUARANTINE);
        let mut got = subjects(&home, &found);
        got.sort();
        let mut want = vec![
            (below(&home, &sides[0]), "file".to_owned()),
            (below(&home, &sides[1]), "file".to_owned()),
            (below(&home, &sides[2]), "file".to_owned()),
            (below(&home, &folder), "dir".to_owned()),
            (below(&home, &client), "file".to_owned()),
        ];
        want.sort();
        assert_eq!(
            got, want,
            "file_on_each_side (a client's <meta> included), directory_is_reported_and_not_descended, symlink_is_skipped"
        );

        // The fixer sends each to its own side's quarantine.
        for f in &found {
            let fix = (row(QUARANTINE).fix.unwrap().plan)(&view, f).unwrap();
            let path = f.evidence.path.clone().unwrap();
            let root = if path.starts_with(home.client.client_dir()) {
                QuarantineRoot::Client {
                    client_dir: home.client.client_dir().to_path_buf(),
                }
            } else if path.starts_with(home.paths.config_dir()) {
                QuarantineRoot::Config
            } else if path.starts_with(home.paths.cache_dir()) {
                QuarantineRoot::Cache
            } else {
                QuarantineRoot::State
            };
            assert_eq!(fix, [Fix::Quarantine { path, root }]);
        }
    }

    #[test]
    fn flatten_detector() {
        // (case, the folders planted below AddOns, the wrappers found).
        let cases: [(&str, &[&str], &[&str]); 6] = [
            ("two_rename_shape", &["TestWrapA/Inner"], &["TestWrapA"]),
            (
                "three_rename_shape",
                &["TestWrapB/TestWrapB"],
                &["TestWrapB"],
            ),
            (
                "managed_wrapper_is_ignored",
                &["TestWrapManaged/Inner"],
                &[],
            ),
            (
                "wrapper_with_two_entries_is_ignored",
                &["TestWrapC/One", "TestWrapC/Two"],
                &[],
            ),
            (
                "wrapper_with_a_toc_is_ignored",
                &["TestWrapD/TestWrapD.toc"],
                &[],
            ),
            ("name_without_prefix_is_ignored", &["Wrapped/Inner"], &[]),
        ];
        for (case, planted, want) in cases {
            let home = repair_home();
            for rel in planted {
                let path = home.addons(rel);
                if rel.ends_with(".toc") {
                    put(&path, b"## Interface: 120105\n");
                } else {
                    std::fs::create_dir_all(&path).unwrap();
                }
            }
            let managed: &[&str] = if case == "managed_wrapper_is_ignored" {
                &["TestWrapManaged"]
            } else {
                &[]
            };
            let view = view_with(&home, managed);
            let found = findings(&home, &view, FLATTEN);
            let got: Vec<&str> = found
                .iter()
                .map(|f| name_of(f.evidence.path.as_ref().unwrap()))
                .collect();
            assert_eq!(got, *want, "{case}: planted {planted:?}");
            for f in &found {
                let fix = (row(FLATTEN).fix.unwrap().plan)(&view, f).unwrap();
                let outer = f.evidence.path.clone().unwrap();
                let inner = f.evidence.paths.as_ref().unwrap()[0].clone();
                assert_eq!(inner.parent(), Some(outer.as_path()), "{case}");
                assert_eq!(fix, [Fix::Flatten { inner, outer }], "{case}");
                assert_eq!(f.target.as_ref().map(|t| t.key), Some(home.key), "{case}");
            }
        }
    }

    #[test]
    fn detectors_write_nothing() {
        let home = repair_home();
        put(&state_test(&home).join("a.write"), b"broken\n");
        put(&state_test(&home).join("q.quarantine"), b"x");
        std::fs::create_dir_all(home.addons("TestWrapA/Inner")).unwrap();
        plant_chmod(&home, "a.chmod", 0o644);
        plant_request(&home, &home.stuck(), "");
        // The view's fixture record is written first: only the detectors are measured.
        let view = view(&home);
        let before = tree_hash(&home.base);
        for row in TEST_FAILURE_MODES {
            let found = findings(&home, &view, row.id);
            assert!(!found.is_empty(), "{}", row.id);
        }
        assert_eq!(tree_hash(&home.base), before, "a detector wrote");
    }

    /// `<config>/doctor-test/<name>` with mode `mode`: `fm-test-chmod`'s subject unless 0600.
    fn plant_chmod(home: &RepairHome, name: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = home.paths.config_dir().join(TEST_DIR).join(name);
        put(&path, b"secret\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    /// The request to set `tx` aside, its target `home`'s client, guarding `rest`.
    fn plant_request(home: &RepairHome, tx: &TxId, rest: &str) {
        put(
            &state_test(home).join(format!("{tx}.suspend")),
            format!("{} {rest}\n", home.key).as_bytes(),
        );
    }

    #[test]
    fn fm_test_chmod_plans_one_chmod() {
        let home = repair_home();
        let open = plant_chmod(&home, "open.chmod", 0o644);
        plant_chmod(&home, "closed.chmod", 0o600);
        let view = view(&home);
        let found = findings(&home, &view, CHMOD);
        assert_eq!(found.len(), 1, "only the open file: {found:?}");
        let plan = (row(CHMOD).fix.unwrap().plan)(&view, &found[0]).unwrap();
        assert_eq!(
            plan,
            [Fix::Chmod {
                path: open,
                mode: Mode::new(0o600).unwrap()
            }]
        );
    }

    #[test]
    fn fm_test_suspend_plans_one_rename_per_present_artifact_with_its_guards() {
        // (case, what it plants for a transaction, the request's guarded names, the
        // artifacts below the base, the guard kinds)
        type Prepare = fn(&RepairHome) -> TxId;
        type Case = (
            &'static str,
            Prepare,
            &'static str,
            Vec<&'static str>,
            Vec<&'static str>,
        );
        let cases: [Case; 4] = [
            (
                "all five artifacts",
                |h| {
                    let tx = h.stuck();
                    std::fs::create_dir_all(h.paths.state_snapshot(&tx)).unwrap();
                    std::fs::create_dir_all(h.client.snapshots(&tx)).unwrap();
                    tx
                },
                "",
                vec![
                    "journal",
                    "inflight",
                    "state snapshot",
                    "stage",
                    "client snapshots",
                ],
                vec!["generation", "marker"],
            ),
            (
                "journal and in-flight lock only",
                |h| {
                    let tx = h.stuck();
                    std::fs::remove_dir_all(h.client.stage(&tx)).unwrap();
                    tx
                },
                "",
                vec!["journal", "inflight"],
                vec!["generation", "marker"],
            ),
            (
                "a WTF journal with a WTF epoch guard",
                |h| {
                    let tx = TxId::new();
                    put(&h.paths.journal_wtf(&tx), b"{\"line\":\"prepared\"}\n");
                    tx
                },
                "WTF",
                vec!["wtf journal"],
                vec!["generation", "marker", "epochs"],
            ),
            (
                "one epoch per named folder",
                |h| {
                    std::fs::create_dir_all(h.addons("Foo")).unwrap();
                    h.stuck()
                },
                "Foo Bar",
                vec!["journal", "inflight", "stage"],
                vec!["generation", "marker", "epochs", "epochs"],
            ),
        ];
        for (case, prepare, guarded, artifacts, guards) in cases {
            let home = repair_home();
            let tx = prepare(&home);
            plant_request(&home, &tx, guarded);
            let view = view(&home);
            let found = findings(&home, &view, SUSPEND);
            assert_eq!(found.len(), 1, "{case}: {found:?}");
            let plan = (row(SUSPEND).fix.unwrap().plan)(&view, &found[0]).unwrap();
            let [Fix::Suspend { journal }] = plan.as_slice() else {
                panic!("{case}: {plan:?}"); // ubs:ignore — fails the test
            };
            assert_eq!(journal.tx, tx, "{case}");
            assert_eq!(journal.target, Some(home.key), "{case}");
            let name = |path: &Path| {
                let p = &home.paths;
                let c = &home.client;
                [
                    (p.journal(&tx), "journal"),
                    (p.journal_wtf(&tx), "wtf journal"),
                    (p.inflight_lock(&tx), "inflight"),
                    (p.state_snapshot(&tx), "state snapshot"),
                    (c.stage(&tx), "stage"),
                    (c.snapshots(&tx), "client snapshots"),
                ]
                .into_iter()
                // ubs:ignore — comparing paths, not secrets
                .find(|(known, _)| known == path)
                .map_or("?", |(_, name)| name)
            };
            let got: Vec<&str> = journal.artifacts.iter().map(|a| name(&a.path)).collect();
            assert_eq!(got, artifacts, "{case}");
            let kinds: Vec<&str> = journal.guards.iter().map(guard::kind).collect();
            assert_eq!(kinds, guards, "{case}");
        }
    }

    #[test]
    fn detectors_respect_their_budget() {
        let home = repair_home();
        for name in ["a.write", "b.write", "c.write"] {
            put(&state_test(&home).join(name), b"broken\n");
        }
        let view = view(&home);
        let mut budget = Budget::new(std::time::Duration::from_secs(60), 1);
        let detect = row(WRITE).detect.unwrap();
        let got = detect(&view, &Selection::all(), &mut budget);
        assert!(got.incomplete.is_some(), "{got:?}");
    }
}
