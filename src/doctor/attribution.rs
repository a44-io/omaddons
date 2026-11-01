//! Attribution without the payload, and the suspension manifest (M4-D38, F2 s2): which
//! files and directories belong to a stuck transaction, decided from its transaction id
//! and omaddons' own directories alone, and the guards a set-aside of them records so its
//! reactivation refuses once the world moved.
//!
//! A binary that cannot read a journal has no business deriving write authority from the
//! paths inside it. So the artifacts are found **by name only**, in their known locations,
//! on the clients whose markers check out; the journal's lines are read to judge whether
//! that set is `complete`, never followed: no path they name is stat'ed, moved or written.
//! The detectors of the stuck rows give the result as evidence, and the set-aside fixer
//! (s3) moves exactly its `artifacts`, in their order.
//!
//! Read-only: nothing here writes (the detectors' purity scan covers this file).

use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::budget::Budget;
use super::view::DoctorView;
use crate::discover::registry::read_marker;
use crate::engine::journal::{self, JournalDiagnosis, JournalVerdict};
use crate::engine::repair::{self, guard};
use crate::engine::repair_action::{Attribution, Guard, JournalFamily, Primitive};
use crate::error::Result;
use crate::model::{FolderName, TargetKey};
use crate::state::paths::{ClientPaths, Paths};

/// What a stuck transaction owns that can be attributed to it without its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalAttribution {
    /// Existing and attributed, in the order a set-aside moves them: client stages, client
    /// snapshots, state snapshots, the in-flight lock, the journal last.
    pub artifacts: Vec<Artifact>,
    /// The checked-out clients holding at least one artifact, by directory.
    pub clients: Vec<(PathBuf, TargetKey)>,
    pub attribution: Attribution,
    /// What may remain unaccounted for: directories, sorted and without duplicates, never
    /// a path below a client's `Interface/` or `WTF/`.
    pub unattributed: Vec<PathBuf>,
    /// The payload could not be read to its end (its budget ran out, or the file could
    /// not be read).
    pub incomplete: bool,
}

/// One attributed artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub path: PathBuf,
    pub kind: ArtifactKind,
    /// The client dir it lies in; `None` on the state side.
    pub client: Option<PathBuf>,
}

/// An artifact's kind, declared in the order a set-aside moves them: the journal last, so
/// it stays visible to every listing, recovery and sweep (this build's and older ones')
/// until the final primitive, and keeps protecting what it owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArtifactKind {
    ClientStage,
    ClientSnapshot,
    StateSnapshot,
    Inflight,
    Journal,
}

/// What a name must be to be an artifact: the journal and the lock are files, the rest
/// directories. A symlink or another kind at the name is none (fsx never renames through
/// a symlink).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    File,
    Dir,
}

/// The registered clients, by whether their identity checks out.
struct Clients {
    /// Marker names the entry's key, and `<meta>` can be stat'ed (or is not there).
    checked: Vec<(PathBuf, TargetKey)>,
    /// Every other registered client (or, with no Registry layer, every discovered one).
    unverified: Vec<PathBuf>,
    /// Every client dir omaddons knows, registered or discovered.
    known: Vec<PathBuf>,
}

fn clients(view: &DoctorView) -> Clients {
    let discovered: Vec<PathBuf> = view
        .world()
        .map(|world| {
            world
                .resolved
                .targets
                .iter()
                .map(|d| d.target.client_dir.clone())
                .collect()
        })
        .unwrap_or_default();
    let Some(registry) = view.registry() else {
        return Clients {
            checked: Vec::new(),
            unverified: discovered.clone(),
            known: discovered,
        };
    };
    let mut out = Clients {
        checked: Vec::new(),
        unverified: Vec::new(),
        known: discovered,
    };
    for entry in &registry.value.targets {
        let meta = ClientPaths::new(&entry.client_dir).meta_dir();
        let meta_ok = match std::fs::symlink_metadata(&meta) {
            Ok(_) => true,
            Err(e) => e.kind() == io::ErrorKind::NotFound,
        };
        // ubs:ignore — comparing target keys (public ULIDs), not secrets
        if meta_ok && read_marker(&entry.client_dir) == Some(entry.key) {
            out.checked.push((entry.client_dir.clone(), entry.key));
        } else {
            out.unverified.push(entry.client_dir.clone());
        }
        out.known.push(entry.client_dir.clone());
    }
    out
}

/// The verdict a stuck journal is reported under: its own, or, behind a barrier, the one
/// it holds (E14).
fn verdict_of(d: &JournalDiagnosis) -> JournalVerdict {
    match d.verdict {
        JournalVerdict::BehindBarrier { .. } => d.evidence.held.unwrap_or(d.verdict),
        verdict => verdict,
    }
}

/// A journal whose lines cannot be trusted to say what it owns: started by a newer build,
/// holding a line this build cannot read, or with no readable target (rows 3, 4, 8).
fn untrusted(d: &JournalDiagnosis) -> bool {
    matches!(
        verdict_of(d),
        JournalVerdict::TooNew | JournalVerdict::Unreadable | JournalVerdict::Unattributed { .. }
    )
}

/// The artifacts of `d`'s transaction, found by name in their known locations on the
/// checked-out clients, and what may remain unaccounted for; the journal's lines read
/// (each absolute path string charged one entry to `budget`), never followed.
pub fn attribute(
    view: &DoctorView,
    d: &JournalDiagnosis,
    budget: &mut Budget,
) -> JournalAttribution {
    let paths = view.paths();
    let tx = d.tx;
    let clients = clients(view);
    let mut unattributed: BTreeSet<PathBuf> = clients.unverified.iter().cloned().collect();
    let mut artifacts: Vec<Artifact> = Vec::new();

    // By name only, state side, then each checked-out client.
    let state = [
        (paths.journal(&tx), ArtifactKind::Journal, Want::File),
        (paths.journal_wtf(&tx), ArtifactKind::Journal, Want::File),
        (paths.journal_repair(&tx), ArtifactKind::Journal, Want::File),
        (
            repair::provisional_journal(paths, &tx),
            ArtifactKind::Journal,
            Want::File,
        ),
        (paths.inflight_lock(&tx), ArtifactKind::Inflight, Want::File),
        (
            paths.state_snapshot(&tx),
            ArtifactKind::StateSnapshot,
            Want::Dir,
        ),
        (
            paths.state_snapshot_wtf(&tx),
            ArtifactKind::StateSnapshot,
            Want::Dir,
        ),
    ];
    for (path, kind, want) in state {
        match probe(&path, want) {
            Probe::Artifact => artifacts.push(Artifact {
                path,
                kind,
                client: None,
            }),
            Probe::Absent => {}
            Probe::Unexpected => {
                unattributed.extend(path.parent().map(Path::to_path_buf));
            }
        }
    }
    for (dir, _) in &clients.checked {
        let client = ClientPaths::new(dir);
        let named = [
            (client.stage(&tx), ArtifactKind::ClientStage),
            (client.stage_wtf(&tx), ArtifactKind::ClientStage),
            (client.snapshots(&tx), ArtifactKind::ClientSnapshot),
            (client.snapshots_wtf(&tx), ArtifactKind::ClientSnapshot),
            (client.snapshots_repair(&tx), ArtifactKind::ClientSnapshot),
        ];
        for (path, kind) in named {
            match probe(&path, Want::Dir) {
                Probe::Artifact => artifacts.push(Artifact {
                    path,
                    kind,
                    client: Some(dir.clone()),
                }),
                Probe::Absent => {}
                // Never a path below `Interface/`: the client is what remains unaccounted.
                Probe::Unexpected => {
                    unattributed.insert(dir.clone());
                }
            }
        }
    }
    artifacts.sort_by(|a, b| (a.kind, &a.path).cmp(&(b.kind, &b.path)));
    let holding: Vec<(PathBuf, TargetKey)> = clients
        .checked
        .iter()
        .filter(|(dir, _)| artifacts.iter().any(|a| a.client.as_ref() == Some(dir)))
        .cloned()
        .collect();

    // The clients the payload may name without leaving anything unaccounted for: its own
    // target's checked-out client; a trusted repair's own clients; with no target, the
    // clients its artifacts are on.
    let mut own: Vec<PathBuf> = match d.target {
        Some(key) => clients
            .checked
            .iter()
            .filter(|(_, k)| *k == key)
            .map(|(dir, _)| dir.clone())
            .collect(),
        None => holding.iter().map(|(dir, _)| dir.clone()).collect(),
    };
    if d.family == JournalFamily::Repair
        && !untrusted(d)
        && let Ok(parsed) = repair::journal::read(paths, &d.path)
        && let Some(prepared) = parsed.prepared
    {
        own.extend(
            prepared
                .client_dirs
                .iter()
                .filter(|c| clients.checked.iter().any(|(dir, _)| *dir == c.dir))
                .map(|c| c.dir.clone()),
        );
    }

    // The payload, read never followed.
    let (named, incomplete) = payload_paths(&d.path, budget);
    for path in named {
        if let Some(dir) = foreign(paths, &path, &own, &clients.known) {
            unattributed.insert(dir);
        }
    }
    // A repair's own run directory (the backups its rollback reads) cannot be told
    // without its payload, and stays where it is.
    if d.family == JournalFamily::Repair && untrusted(d) {
        unattributed.insert(paths.doctor_runs_dir());
    }

    let attribution = if unattributed.is_empty() && !incomplete {
        Attribution::Complete
    } else {
        Attribution::Partial
    };
    JournalAttribution {
        artifacts,
        clients: holding,
        attribution,
        unattributed: unattributed.into_iter().collect(),
        incomplete,
    }
}

enum Probe {
    Artifact,
    Absent,
    /// Something else at the name: a symlink, another kind, or an entry that cannot be
    /// stat'ed.
    Unexpected,
}

fn probe(path: &Path, want: Want) -> Probe {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            let kind = meta.file_type();
            let fits = match want {
                Want::File => kind.is_file(),
                Want::Dir => kind.is_dir(),
            };
            if fits {
                Probe::Artifact
            } else {
                Probe::Unexpected
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Probe::Absent,
        Err(_) => Probe::Unexpected,
    }
}

/// Every absolute path string in the complete lines of the journal at `path` that parse
/// as JSON, each charged one entry to `budget`; and whether the read stopped short (its
/// budget ran out, or the file could not be read).
fn payload_paths(path: &Path, budget: &mut Budget) -> (Vec<PathBuf>, bool) {
    let Ok(bytes) = std::fs::read(path) else {
        return (Vec::new(), true);
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Vec::new();
    for line in text.split_inclusive('\n').filter(|l| l.ends_with('\n')) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if !strings(&value, &mut |s| {
            if !s.starts_with('/') {
                return true;
            }
            if budget.charge(1).is_err() {
                return false;
            }
            out.push(PathBuf::from(s));
            true
        }) {
            return (out, true);
        }
    }
    (out, false)
}

/// Calls `f` on every string in `value`, depth first, until it returns `false`; whether
/// it went through all of them.
fn strings(value: &Value, f: &mut dyn FnMut(&str) -> bool) -> bool {
    match value {
        Value::String(s) => f(s),
        Value::Array(items) => items.iter().all(|v| strings(v, f)),
        Value::Object(map) => map.values().all(|v| strings(v, f)),
        _ => true,
    }
}

/// The directory a payload path leaves unaccounted for, if any. A path under omaddons'
/// own state, config, cache or skill folders, or under one of `own`, names nothing
/// foreign; nor does one that is not shaped like a client's file (no `Interface`, `WTF` or
/// `AddOns` component, under no known client: an archive given with `--file`). Otherwise:
/// the known client it lies under; else the path cut before its first `Interface` or
/// `WTF` component; else its parent directory. Never a path below `Interface/` or `WTF/`.
fn foreign(paths: &Paths, path: &Path, own: &[PathBuf], known: &[PathBuf]) -> Option<PathBuf> {
    let home_side = [
        paths.state_dir().to_path_buf(),
        paths.config_dir().to_path_buf(),
        paths.cache_dir().to_path_buf(),
        paths.skill_dir(),
    ];
    if home_side.iter().chain(own).any(|dir| path.starts_with(dir)) {
        return None;
    }
    if let Some(client) = known.iter().find(|dir| path.starts_with(dir)) {
        return Some(client.clone());
    }
    let mut prefix = PathBuf::new();
    let mut addons = false;
    for component in path.components() {
        if let Component::Normal(name) = component {
            if name == "Interface" || name == "WTF" {
                return Some(prefix);
            }
            addons |= name == "AddOns";
        }
        prefix.push(component);
    }
    if addons {
        return path.parent().map(Path::to_path_buf);
    }
    None
}

// ---------------------------------------------------------------------------------------
// The suspension manifest.

/// The guards a set-aside of `a` records (M4-D38), built with E7's constructors so the
/// recorded value and the one reactivation reads under the lock are computed one way:
///
/// - `generation` and `marker` of every client the set touches and of the journal's own
///   target when it is registered; for a journal with no readable target, every
///   registered target's (what it would recover could be on any client);
/// - for a journal whose lines can be trusted, the `epoch` of each distinct
///   `Interface/AddOns/<folder>` its steps name on a registered client, absent ones
///   included;
/// - for a journal that puts settings back, one `epoch` of that client's whole `WTF/`.
///
/// No guard names a path below `WTF/`: a step path through it contributes the client's
/// `WTF` epoch. Sorted by kind, target and folder.
pub fn manifest(
    view: &DoctorView,
    d: &JournalDiagnosis,
    a: &JournalAttribution,
) -> Result<Vec<Guard>> {
    let paths = view.paths();
    let registered: Vec<(PathBuf, TargetKey)> = view
        .registry()
        .map(|r| {
            r.value
                .targets
                .iter()
                .map(|e| (e.client_dir.clone(), e.key))
                .collect()
        })
        .unwrap_or_default();
    let dir_of = |key: TargetKey| {
        a.clients
            .iter()
            .chain(&registered)
            .find(|(_, k)| *k == key)
            .map(|(dir, _)| dir.clone())
    };
    let mut targets: BTreeSet<TargetKey> = a.clients.iter().map(|(_, k)| *k).collect();
    match d.target {
        Some(key) if registered.iter().any(|(_, k)| *k == key) => {
            targets.insert(key);
        }
        Some(_) => {}
        None => targets.extend(registered.iter().map(|(_, k)| *k)),
    }
    let mut guards = Vec::new();
    for key in &targets {
        guards.push(guard::observe_generation(paths, *key)?);
        if let Some(dir) = dir_of(*key) {
            guards.push(guard::observe_marker(&dir, *key)?);
        }
    }

    // The resources recovery would touch: what a trusted journal's steps name.
    let text = std::fs::read(&d.path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    let mut folders: BTreeSet<(PathBuf, String)> = BTreeSet::new();
    let mut wtf: BTreeSet<(PathBuf, TargetKey)> = BTreeSet::new();
    let mut writes_wtf = d
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".wtf.jsonl"));
    if !untrusted(d) {
        let mut named: Vec<PathBuf> = Vec::new();
        match d.family {
            JournalFamily::Folder => {
                let parsed = journal::parse(&text);
                writes_wtf |= parsed.prepared.as_ref().is_some_and(|p| p.writes_wtf);
                for step in &parsed.steps {
                    named.extend([step.from.clone(), step.to.clone()]);
                }
            }
            JournalFamily::Repair => {
                if let Ok(parsed) = repair::journal::read(paths, &d.path)
                    && let Some(prepared) = parsed.prepared
                {
                    for step in &prepared.primitives {
                        match &step.op {
                            Primitive::WriteFile { path, .. } | Primitive::Chmod { path, .. } => {
                                named.push(path.clone());
                            }
                            Primitive::RenameNoReplace { from, to, .. } => {
                                named.extend([from.clone(), to.clone()]);
                            }
                        }
                    }
                }
            }
        }
        for path in named {
            let Some((dir, key)) = registered.iter().find(|(dir, _)| path.starts_with(dir)) else {
                continue;
            };
            let rel = path.strip_prefix(dir).unwrap_or(&path);
            let mut parts = rel.components().filter_map(|c| match c {
                Component::Normal(name) => name.to_str(),
                _ => None,
            });
            match (parts.next(), parts.next(), parts.next()) {
                (Some("WTF"), _, _) => {
                    wtf.insert((dir.clone(), *key));
                }
                (Some("Interface"), Some("AddOns"), Some(folder)) => {
                    folders.insert((dir.clone(), folder.to_owned()));
                }
                _ => {}
            }
        }
    }
    for (dir, folder) in &folders {
        if let Ok(name) = FolderName::new(folder) {
            guards.push(guard::observe_folder_epoch(dir, &name)?);
        }
    }
    if writes_wtf {
        let own: Vec<(PathBuf, TargetKey)> =
            match d.target.and_then(|key| dir_of(key).map(|dir| (dir, key))) {
                Some(client) => vec![client],
                None => a.clients.clone(),
            };
        wtf.extend(own);
    }
    for (dir, key) in &wtf {
        guards.push(guard::observe_wtf_epoch(dir, *key)?);
    }

    let by_key: Vec<(TargetKey, PathBuf)> = registered
        .iter()
        .map(|(dir, key)| (*key, dir.clone()))
        .collect();
    guards.sort_by_cached_key(|g| sort_key(g, &by_key));
    guards.dedup();
    Ok(guards)
}

/// A guard's place: its kind (`generation`, `marker`, `epoch`), its target, its folder
/// (`WTF` for the settings epoch).
fn sort_key(guard: &Guard, clients: &[(TargetKey, PathBuf)]) -> (u8, Option<TargetKey>, String) {
    let target = guard::target(guard, clients);
    match guard {
        Guard::Generation { .. } => (0, target, String::new()),
        Guard::Marker { .. } => (1, target, String::new()),
        Guard::Epoch { path, .. } => (
            2,
            target,
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
        Guard::WtfEpoch { .. } => (2, target, "WTF".to_owned()),
    }
}

/// The attribution tests, whose temporary homes the set-aside's tests share.
#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use super::*;
    use crate::doctor::view::{LayerState, Preloaded};
    use crate::engine::journal::Phase;
    use crate::engine::simulate::{JournalEvidence, ProtectedPaths, UnattributedCause};
    use crate::model::TxId;
    use crate::state::records::TargetRecord;
    use crate::state::registry::{Origin, Registry, RegistryEntry};
    use crate::state::store::{self, Loaded, VersionStatus, Versioned};

    const TX: &str = "01JB8Q2VA00000000000000009";

    /// A home with a retail and a Classic Era client, each marked with its key, and the
    /// journal directory.
    pub(crate) struct Home {
        _dir: tempfile::TempDir,
        pub(crate) paths: Paths,
        pub(crate) retail: PathBuf,
        pub(crate) era: PathBuf,
        pub(crate) rk: TargetKey,
        pub(crate) ek: TargetKey,
    }

    pub(crate) fn home() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let base = root.join("home");
        let paths = Paths::with_roots(
            &base,
            base.join(".config"),
            base.join(".local/state"),
            base.join(".cache"),
        );
        let (retail, era) = (
            root.join("games/_retail_"),
            root.join("games/_classic_era_"),
        );
        let (rk, ek) = (TargetKey::new(), TargetKey::new());
        for (dir, key) in [(&retail, rk), (&era, ek)] {
            std::fs::create_dir_all(ClientPaths::new(dir).addons()).unwrap();
            crate::engine::testing::mark(dir, key);
        }
        std::fs::create_dir_all(paths.journal_dir()).unwrap();
        Home {
            _dir: dir,
            paths,
            retail,
            era,
            rk,
            ek,
        }
    }

    pub(crate) fn tx() -> TxId {
        TX.parse().unwrap()
    }

    pub(crate) fn entry(dir: &Path, key: TargetKey, id: &str) -> RegistryEntry {
        RegistryEntry {
            key,
            id: id.to_owned(),
            install_root: dir.parent().unwrap().to_path_buf(),
            client_dir: dir.to_path_buf(),
            flavor_dir: None,
            product: None,
            qualifier: String::new(),
            origin: Origin::Discovered,
            registered_at: jiff::Timestamp::UNIX_EPOCH,
        }
    }

    pub(crate) fn both(h: &Home) -> Vec<RegistryEntry> {
        vec![entry(&h.retail, h.rk, "retail"), entry(&h.era, h.ek, "era")]
    }

    /// A view whose Registry layer holds `entries` (`None`: no Registry layer).
    pub(crate) fn view(h: &Home, entries: Option<Vec<RegistryEntry>>) -> DoctorView {
        view_with(h, entries, None)
    }

    /// [`view`], with a Journals layer holding exactly `diagnoses` when given.
    pub(crate) fn view_with(
        h: &Home,
        entries: Option<Vec<RegistryEntry>>,
        diagnoses: Option<Vec<JournalDiagnosis>>,
    ) -> DoctorView {
        use crate::doctor::registry::Layer;
        use crate::doctor::view::Unavailable;
        let registry = match entries {
            Some(targets) => LayerState::Available(Loaded {
                value: Registry {
                    targets,
                    default: None,
                },
                version: Registry::CURRENT,
                status: VersionStatus::Current,
            }),
            None => LayerState::Unavailable(Unavailable {
                layer: Layer::Registry,
                cause: Layer::Registry,
            }),
        };
        let journals = diagnoses.map(|journals| {
            LayerState::Available(crate::doctor::view::Journals {
                pending: Vec::new(),
                diagnoses: crate::engine::simulate::Diagnoses {
                    journals,
                    unlisted: None,
                },
                counts: std::collections::BTreeMap::new(),
            })
        });
        DoctorView::preloaded(
            h.paths.clone(),
            Preloaded {
                registry: Some(registry),
                journals,
                ..Preloaded::default()
            },
        )
    }

    pub(crate) fn diagnosis(
        path: PathBuf,
        family: JournalFamily,
        target: Option<TargetKey>,
        verdict: JournalVerdict,
    ) -> JournalDiagnosis {
        JournalDiagnosis {
            tx: tx(),
            family,
            path,
            target,
            client_dir: None,
            phase: Phase::Prepared,
            repair_phase: None,
            live: false,
            verdict,
            evidence: JournalEvidence::default(),
            protected: ProtectedPaths::default(),
        }
    }

    pub(crate) fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    pub(crate) fn mkdir(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
    }

    /// A line naming each of `paths`, as JSON (one object of strings).
    fn line_naming(paths: &[&Path]) -> String {
        let values: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        format!(
            "{}\n",
            serde_json::json!({ "type": "prepared", "paths": values })
        )
    }

    /// A step line renaming `from` to `to`, as `journal::parse` reads it.
    pub(crate) fn step_line(n: u32, from: &Path, to: &Path) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type": "step", "n": n, "op": "noreplace", "from": from,
                "to": to, "purpose": "old_to_snapshot", "from_id": null, "to_id": null})
        )
    }

    /// The journal, its lock and state snapshot, and the retail client's stage and
    /// snapshot of the transaction, planted.
    pub(crate) fn plant_retail_set(h: &Home, journal_text: &str) {
        let client = ClientPaths::new(&h.retail);
        write(&h.paths.journal(&tx()), journal_text);
        write(&h.paths.inflight_lock(&tx()), "");
        mkdir(&h.paths.state_snapshot(&tx()));
        mkdir(&client.stage(&tx()));
        mkdir(&client.snapshots(&tx()));
    }

    fn attribute_all(view: &DoctorView, d: &JournalDiagnosis) -> JournalAttribution {
        attribute(view, d, &mut Budget::unlimited())
    }

    fn kinds(a: &JournalAttribution) -> Vec<(ArtifactKind, PathBuf)> {
        a.artifacts
            .iter()
            .map(|x| (x.kind, x.path.clone()))
            .collect()
    }

    /// The retail set's artifacts, in move order.
    pub(crate) fn retail_set(h: &Home) -> Vec<(ArtifactKind, PathBuf)> {
        let client = ClientPaths::new(&h.retail);
        vec![
            (ArtifactKind::ClientStage, client.stage(&tx())),
            (ArtifactKind::ClientSnapshot, client.snapshots(&tx())),
            (ArtifactKind::StateSnapshot, h.paths.state_snapshot(&tx())),
            (ArtifactKind::Inflight, h.paths.inflight_lock(&tx())),
            (ArtifactKind::Journal, h.paths.journal(&tx())),
        ]
    }

    #[test]
    fn trusted_folder_journal_on_its_own_client() {
        let h = home();
        let addons = ClientPaths::new(&h.retail).addons();
        let text = step_line(
            1,
            &addons.join("Foo"),
            &ClientPaths::new(&h.retail).stage(&tx()).join("Foo"),
        );
        plant_retail_set(&h, &format!("{}{text}", line_naming(&[&h.retail])));
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::StepUnrecognized { step: 1 },
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(kinds(&a), retail_set(&h), "{a:#?}");
        assert_eq!(a.clients, [(h.retail.clone(), h.rk)], "{a:#?}");
        assert_eq!(
            (a.attribution, &a.unattributed[..], a.incomplete),
            (Attribution::Complete, &[][..], false),
            "{a:#?}"
        );
    }

    #[test]
    fn newer_journal_naming_only_its_own_client() {
        let h = home();
        let addons = ClientPaths::new(&h.retail).addons();
        let text = format!(
            "{}\n{}",
            serde_json::json!({"type": "prepared", "schema_version": "9.0", "client_dir": h.retail}),
            step_line(1, &addons.join("Foo"), &addons.join("Bar"))
        );
        plant_retail_set(&h, &text);
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::TooNew,
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(kinds(&a), retail_set(&h), "{a:#?}");
        assert_eq!(
            (a.attribution, a.unattributed.len()),
            (Attribution::Complete, 0),
            "{a:#?}"
        );
    }

    #[test]
    fn unreadable_payload_naming_another_client() {
        let h = home();
        let era_folder = ClientPaths::new(&h.era).addons().join("X");
        let text = format!(
            "{}not json at all\n{}",
            line_naming(&[&h.retail]),
            line_naming(&[&era_folder])
        );
        plant_retail_set(&h, &text);
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::Unreadable,
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(a.attribution, Attribution::Partial, "{a:#?}");
        assert_eq!(a.unattributed, std::slice::from_ref(&h.era), "{a:#?}");
        assert!(
            a.artifacts.iter().all(|x| !x.path.starts_with(&h.era)),
            "{a:#?}"
        );
        assert_eq!(kinds(&a), retail_set(&h), "{a:#?}");
    }

    #[test]
    fn marker_mismatch() {
        let h = home();
        // Retail's dir now carries another client's marker.
        crate::engine::testing::mark(&h.retail, TargetKey::new());
        plant_retail_set(&h, &line_naming(&[&h.retail.join("Interface/AddOns/Foo")]));
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::MarkerMismatch,
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(a.attribution, Attribution::Partial, "{a:#?}");
        assert_eq!(a.unattributed, std::slice::from_ref(&h.retail), "{a:#?}");
        assert!(
            a.artifacts.iter().all(|x| x.client.is_none()),
            "its stage is no artifact: {a:#?}"
        );
        assert!(a.clients.is_empty(), "{a:#?}");
    }

    #[test]
    fn unverified_registered_client() {
        let h = home();
        std::fs::remove_file(ClientPaths::new(&h.era).marker()).unwrap();
        mkdir(&ClientPaths::new(&h.era).stage(&tx()));
        plant_retail_set(&h, &line_naming(&[&h.retail]));
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::StepUnrecognized { step: 1 },
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(
            (a.attribution, &a.unattributed[..]),
            (Attribution::Partial, std::slice::from_ref(&h.era)),
            "{a:#?}"
        );
        assert_eq!(
            kinds(&a),
            retail_set(&h),
            "era's stage is not moved: {a:#?}"
        );
    }

    #[test]
    fn unattributed_journal() {
        let h = home();
        plant_retail_set(&h, &line_naming(&[&h.retail.join("Interface/AddOns/Foo")]));
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            None,
            JournalVerdict::Unattributed {
                cause: UnattributedCause::Unreadable,
            },
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(kinds(&a), retail_set(&h), "{a:#?}");
        assert_eq!(a.clients, [(h.retail.clone(), h.rk)], "{a:#?}");
        assert_eq!(
            a.attribution,
            Attribution::Complete,
            "it names nothing outside them: {a:#?}"
        );
    }

    /// A repair's journal with its lock and its retail quarantine half.
    fn plant_repair(h: &Home, text: &str) -> JournalAttribution {
        write(&h.paths.journal_repair(&tx()), text);
        write(&h.paths.inflight_lock(&tx()), "");
        mkdir(&ClientPaths::new(&h.retail).snapshots_repair(&tx()));
        let d = diagnosis(
            h.paths.journal_repair(&tx()),
            JournalFamily::Repair,
            None,
            JournalVerdict::StepUnrecognized { step: 1 },
        );
        attribute_all(&view(h, Some(both(h))), &d)
    }

    #[test]
    fn repair_family_readable() {
        let h = home();
        let run = h.paths.doctor_runs_dir().join("R/quarantine/state/x");
        let text = line_naming(&[&h.retail.join("Interface/AddOns/Wrap"), &run]);
        let a = plant_repair(&h, &text);
        assert_eq!(
            kinds(&a),
            [
                (
                    ArtifactKind::ClientSnapshot,
                    ClientPaths::new(&h.retail).snapshots_repair(&tx())
                ),
                (ArtifactKind::Inflight, h.paths.inflight_lock(&tx())),
                (ArtifactKind::Journal, h.paths.journal_repair(&tx())),
            ],
            "{a:#?}"
        );
        assert_eq!(a.attribution, Attribution::Complete, "{a:#?}");
    }

    #[test]
    fn repair_family_untrusted() {
        let h = home();
        write(
            &h.paths.journal_repair(&tx()),
            "{\"type\":\"prepared\",\"format\":\"9.0\"}\n",
        );
        let d = diagnosis(
            h.paths.journal_repair(&tx()),
            JournalFamily::Repair,
            None,
            JournalVerdict::TooNew,
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(a.attribution, Attribution::Partial, "{a:#?}");
        assert_eq!(
            a.unattributed,
            [h.paths.doctor_runs_dir()],
            "its run directory: {a:#?}"
        );
    }

    #[test]
    fn payload_path_below_wtf() {
        let h = home();
        let private = h.era.join("WTF/Account/FAKEACCT/SavedVariables/X.lua");
        plant_retail_set(&h, &line_naming(&[&private]));
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::Unreadable,
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(a.unattributed, std::slice::from_ref(&h.era), "{a:#?}");
        assert!(!format!("{a:?}").contains("FAKEACCT"), "{a:#?}");
        // Nor when the client is not one omaddons knows: cut before `WTF`.
        let h = home();
        let elsewhere = h.retail.parent().unwrap().join("_other_");
        plant_retail_set(
            &h,
            &line_naming(&[&elsewhere.join("WTF/Account/FAKEACCT/SavedVariables/X.lua")]),
        );
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::Unreadable,
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(a.unattributed, [elsewhere], "{a:#?}");
        assert!(!format!("{a:?}").contains("FAKEACCT"), "{a:#?}");
    }

    #[test]
    fn payload_path_outside_every_client() {
        let h = home();
        let archive = h.paths.home().join("dl/foo.zip");
        plant_retail_set(
            &h,
            &line_naming(&[Path::new("/srv/elsewhere/AddOns/X"), &archive]),
        );
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::Unreadable,
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(
            a.unattributed,
            [PathBuf::from("/srv/elsewhere/AddOns")],
            "an archive given with --file names no client: {a:#?}"
        );
    }

    #[test]
    fn scan_cut_short() {
        let h = home();
        let named: Vec<PathBuf> = (0..9)
            .map(|n| h.retail.join(format!("Interface/AddOns/F{n}")))
            .collect();
        let named: Vec<&Path> = named.iter().map(PathBuf::as_path).collect();
        plant_retail_set(&h, &line_naming(&named));
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::Unreadable,
        );
        let mut budget = Budget::new(Duration::from_secs(60), 8);
        let a = attribute(&view(&h, Some(both(&h))), &d, &mut budget);
        assert!(a.incomplete, "{a:#?}");
        assert_eq!(a.attribution, Attribution::Partial, "{a:#?}");
        assert!(budget.incomplete().is_some());
    }

    #[test]
    fn symlink_at_an_artifact_name() {
        let h = home();
        plant_retail_set(&h, &line_naming(&[&h.retail]));
        let stage = ClientPaths::new(&h.retail).stage(&tx());
        std::fs::remove_dir(&stage).unwrap();
        std::os::unix::fs::symlink(&h.era, &stage).unwrap();
        let snapshot = h.paths.state_snapshot(&tx());
        std::fs::remove_dir(&snapshot).unwrap();
        std::os::unix::fs::symlink(&h.era, &snapshot).unwrap();
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::StepUnrecognized { step: 1 },
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert!(
            a.artifacts
                .iter()
                .all(|x| x.path != stage && x.path != snapshot),
            "{a:#?}"
        );
        assert_eq!(
            a.unattributed,
            [snapshot.parent().unwrap().to_path_buf(), h.retail.clone()]
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>(),
            "the state side's parent, the client's dir (never below Interface/): {a:#?}"
        );
        assert_eq!(a.attribution, Attribution::Partial);
    }

    #[test]
    fn meta_cannot_be_stated() {
        let h = home();
        std::fs::remove_dir_all(h.era.join("Interface")).unwrap();
        write(
            &h.era.join("Interface"),
            "a file, so a stat below it fails with ENOTDIR\n",
        );
        plant_retail_set(&h, &line_naming(&[&h.retail]));
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::StepUnrecognized { step: 1 },
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(
            (a.attribution, &a.unattributed[..]),
            (Attribution::Partial, std::slice::from_ref(&h.era)),
            "{a:#?}"
        );
    }

    #[test]
    fn no_registry_layer() {
        let h = home();
        plant_retail_set(&h, &line_naming(&[&h.retail.join("Interface/AddOns/Foo")]));
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::Unreadable,
        );
        let a = attribute_all(&view(&h, None), &d);
        assert!(
            a.artifacts.iter().all(|x| x.client.is_none()),
            "no client checked out: {a:#?}"
        );
        assert_eq!(
            (a.attribution, &a.unattributed[..]),
            (Attribution::Partial, std::slice::from_ref(&h.retail)),
            "{a:#?}"
        );
    }

    #[test]
    fn artifacts_are_ordered_journal_last() {
        let h = home();
        let (r, e) = (ClientPaths::new(&h.retail), ClientPaths::new(&h.era));
        write(&h.paths.journal(&tx()), "");
        write(&h.paths.journal_wtf(&tx()), "");
        write(&h.paths.inflight_lock(&tx()), "");
        mkdir(&h.paths.state_snapshot_wtf(&tx()));
        for client in [&r, &e] {
            mkdir(&client.stage(&tx()));
            mkdir(&client.snapshots_wtf(&tx()));
        }
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            None,
            JournalVerdict::Unattributed {
                cause: UnattributedCause::Unreadable,
            },
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        let mut stages = vec![r.stage(&tx()), e.stage(&tx())];
        stages.sort();
        let mut snapshots = vec![r.snapshots_wtf(&tx()), e.snapshots_wtf(&tx())];
        snapshots.sort();
        let mut journals = vec![h.paths.journal(&tx()), h.paths.journal_wtf(&tx())];
        journals.sort();
        let mut want: Vec<(ArtifactKind, PathBuf)> = Vec::new();
        want.extend(stages.into_iter().map(|p| (ArtifactKind::ClientStage, p)));
        want.extend(
            snapshots
                .into_iter()
                .map(|p| (ArtifactKind::ClientSnapshot, p)),
        );
        want.push((
            ArtifactKind::StateSnapshot,
            h.paths.state_snapshot_wtf(&tx()),
        ));
        want.push((ArtifactKind::Inflight, h.paths.inflight_lock(&tx())));
        want.extend(journals.into_iter().map(|p| (ArtifactKind::Journal, p)));
        assert_eq!(kinds(&a), want, "{a:#?}");
        assert_eq!(a.clients.len(), 2);
    }

    #[test]
    fn nothing_under_doctor_or_cache_is_an_artifact() {
        let h = home();
        plant_retail_set(&h, "");
        mkdir(&h.paths.doctor_runs_dir().join(TX));
        mkdir(&h.paths.cache_dir().join(format!("tmp/{TX}")));
        write(&h.paths.cache_dir().join(format!("inflight/{TX}.lock")), "");
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::StepUnrecognized { step: 1 },
        );
        let a = attribute_all(&view(&h, Some(both(&h))), &d);
        assert_eq!(kinds(&a), retail_set(&h), "{a:#?}");
    }

    // -----------------------------------------------------------------------------------
    // The manifest.

    fn clients_of(h: &Home) -> Vec<(TargetKey, PathBuf)> {
        vec![(h.rk, h.retail.clone()), (h.ek, h.era.clone())]
    }

    /// Every guard reads its recorded value on the unchanged home (E7's `current`).
    fn assert_current(h: &Home, guards: &[Guard]) {
        for g in guards {
            assert_eq!(
                guard::current(g, &clients_of(h), &h.paths).unwrap(),
                guard::recorded(g),
                "{g:?}"
            );
        }
    }

    fn record(h: &Home, key: TargetKey, generation: u64) {
        let mut rec = TargetRecord::empty(key);
        rec.generation = generation;
        let bytes = store::encode(&rec).unwrap();
        std::fs::create_dir_all(h.paths.target_record(&key).parent().unwrap()).unwrap();
        std::fs::write(h.paths.target_record(&key), bytes).unwrap();
    }

    fn manifest_of(
        h: &Home,
        d: &JournalDiagnosis,
        entries: Option<Vec<RegistryEntry>>,
    ) -> Vec<Guard> {
        let v = view(h, entries);
        let a = attribute_all(&v, d);
        let guards = manifest(&v, d, &a).unwrap();
        assert_current(h, &guards);
        guards
    }

    fn generations(guards: &[Guard]) -> Vec<(TargetKey, u64)> {
        guards
            .iter()
            .filter_map(|g| match g {
                Guard::Generation { target, generation } => Some((*target, *generation)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn generation_of_every_touched_target() {
        let h = home();
        record(&h, h.rk, 7);
        plant_retail_set(&h, "");
        mkdir(&ClientPaths::new(&h.era).stage(&tx()));
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::StepUnrecognized { step: 1 },
        );
        let guards = manifest_of(&h, &d, Some(both(&h)));
        let mut want = vec![(h.rk, 7), (h.ek, 0)];
        want.sort();
        let mut got = generations(&guards);
        got.sort();
        assert_eq!(got, want, "{guards:#?}");
    }

    #[test]
    fn row8_guards_every_registered_target() {
        let h = home();
        write(&h.paths.journal(&tx()), "");
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            None,
            JournalVerdict::Unattributed {
                cause: UnattributedCause::Unreadable,
            },
        );
        let guards = manifest_of(&h, &d, Some(both(&h)));
        let mut got: Vec<TargetKey> = generations(&guards).into_iter().map(|(k, _)| k).collect();
        got.sort();
        let mut want = vec![h.rk, h.ek];
        want.sort();
        assert_eq!(got, want, "{guards:#?}");
    }

    #[test]
    fn marker_digest_is_of_the_file_bytes() {
        use sha2::{Digest, Sha256};
        let h = home();
        plant_retail_set(&h, "");
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::StepUnrecognized { step: 1 },
        );
        let guards = manifest_of(&h, &d, Some(both(&h)));
        let marker = guards
            .iter()
            .find_map(|g| match g {
                Guard::Marker { target, sha256 } if *target == h.rk => Some(*sha256),
                _ => None,
            })
            .unwrap();
        let bytes = std::fs::read(ClientPaths::new(&h.retail).marker()).unwrap();
        let want: [u8; 32] = Sha256::digest(&bytes).into();
        assert_eq!(marker.map(|digest| digest.0), Some(want));
    }

    #[test]
    fn row7_guards_the_mismatched_marker() {
        let h = home();
        let foreign = TargetKey::new();
        crate::engine::testing::mark(&h.retail, foreign);
        plant_retail_set(&h, "");
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::MarkerMismatch,
        );
        let guards = manifest_of(&h, &d, Some(both(&h)));
        let marker = guards
            .iter()
            .find(|g| matches!(g, Guard::Marker { target, .. } if *target == h.rk));
        assert!(
            matches!(
                marker,
                Some(Guard::Marker {
                    sha256: Some(_),
                    ..
                })
            ),
            "the foreign marker is recorded, so the next re-mark refuses the reactivation: {guards:#?}"
        );
        // Re-marked with its own key: the guard now differs.
        crate::engine::testing::mark(&h.retail, h.rk);
        let g = marker.unwrap();
        assert_ne!(
            guard::current(g, &clients_of(&h), &h.paths).unwrap(),
            guard::recorded(g)
        );
    }

    fn epochs(guards: &[Guard]) -> Vec<String> {
        guards
            .iter()
            .filter_map(|g| match g {
                Guard::Epoch { path, .. } => {
                    path.file_name().map(|n| n.to_string_lossy().into_owned())
                }
                Guard::WtfEpoch { .. } => Some("WTF".to_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn folder_epochs_for_a_decodable_journal() {
        let h = home();
        let addons = ClientPaths::new(&h.retail).addons();
        mkdir(&addons.join("Present"));
        write(&addons.join("Present/Present.toc"), "## Interface: 1\n");
        let stage = ClientPaths::new(&h.retail).stage(&tx());
        let text = format!(
            "{}{}",
            step_line(1, &addons.join("Present"), &stage.join("Present")),
            step_line(2, &stage.join("Gone"), &addons.join("Gone"))
        );
        plant_retail_set(&h, &text);
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::StepUnrecognized { step: 1 },
        );
        let guards = manifest_of(&h, &d, Some(both(&h)));
        assert_eq!(epochs(&guards), ["Gone", "Present"], "{guards:#?}");
        let gone = guards
            .iter()
            .find(|g| matches!(g, Guard::Epoch { path, .. } if path.ends_with("Gone")))
            .unwrap();
        assert!(
            matches!(gone, Guard::Epoch { stamp: None, .. }),
            "absent: {gone:?}"
        );
        // A file edited in a named folder moves its epoch.
        let present = guards
            .iter()
            .find(|g| matches!(g, Guard::Epoch { path, .. } if path.ends_with("Present")))
            .unwrap();
        write(&addons.join("Present/new.lua"), "-- new\n");
        assert_ne!(
            guard::current(present, &clients_of(&h), &h.paths).unwrap(),
            guard::recorded(present)
        );
    }

    #[test]
    fn no_folder_epochs_for_an_untrusted_journal() {
        let h = home();
        let addons = ClientPaths::new(&h.retail).addons();
        plant_retail_set(&h, &step_line(1, &addons.join("Foo"), &addons.join("Bar")));
        for verdict in [
            JournalVerdict::TooNew,
            JournalVerdict::Unreadable,
            JournalVerdict::Unattributed {
                cause: UnattributedCause::Unreadable,
            },
        ] {
            let d = diagnosis(
                h.paths.journal(&tx()),
                JournalFamily::Folder,
                Some(h.rk),
                verdict,
            );
            let guards = manifest_of(&h, &d, Some(both(&h)));
            assert!(epochs(&guards).is_empty(), "{verdict:?}: {guards:#?}");
        }
    }

    #[test]
    fn one_wtf_epoch_for_a_settings_journal() {
        let h = home();
        let wtf = ClientPaths::new(&h.retail).wtf();
        let sv = wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua");
        write(&sv, "FooDB = {}\n");
        let stage = ClientPaths::new(&h.retail).stage_wtf(&tx());
        mkdir(&stage);
        write(
            &h.paths.journal_wtf(&tx()),
            &step_line(1, &wtf, &stage.join("WTF")),
        );
        let d = diagnosis(
            h.paths.journal_wtf(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::UnexpectedState { step: 1 },
        );
        let guards = manifest_of(&h, &d, Some(both(&h)));
        assert_eq!(
            epochs(&guards),
            ["WTF"],
            "one WTF epoch and no folder's: {guards:#?}"
        );
        let text = serde_json::to_string(&guards).unwrap();
        assert!(
            !text.contains("FAKEACCT") && !text.contains("SavedVariables"),
            "{text}"
        );
        let wtf_guard = guards
            .iter()
            .find(|g| matches!(g, Guard::WtfEpoch { .. }))
            .unwrap();
        write(&sv, "FooDB = { played = true }\n");
        assert_ne!(
            guard::current(wtf_guard, &clients_of(&h), &h.paths).unwrap(),
            guard::recorded(wtf_guard)
        );
    }

    #[test]
    fn guards_are_sorted() {
        let h = home();
        record(&h, h.rk, 2);
        let addons = ClientPaths::new(&h.retail).addons();
        let stage = ClientPaths::new(&h.retail).stage(&tx());
        let text = format!(
            "{}{}",
            step_line(1, &addons.join("Zed"), &stage.join("Zed")),
            step_line(2, &addons.join("Alpha"), &stage.join("Alpha"))
        );
        plant_retail_set(&h, &text);
        mkdir(&ClientPaths::new(&h.era).stage(&tx()));
        let d = diagnosis(
            h.paths.journal(&tx()),
            JournalFamily::Folder,
            Some(h.rk),
            JournalVerdict::StepUnrecognized { step: 1 },
        );
        let guards = manifest_of(&h, &d, Some(both(&h)));
        let by_key = clients_of(&h);
        let keys: Vec<_> = guards.iter().map(|g| sort_key(g, &by_key)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "{guards:#?}");
        assert_eq!(epochs(&guards), ["Alpha", "Zed"]);
        // A record generation bumped moves its guard.
        let generation = guards
            .iter()
            .find(|g| matches!(g, Guard::Generation { target, .. } if *target == h.rk))
            .unwrap();
        record(&h, h.rk, 3);
        assert_ne!(
            guard::current(generation, &by_key, &h.paths).unwrap(),
            guard::recorded(generation)
        );
    }

    #[test]
    fn evidence_serializes() {
        let complete = crate::doctor::finding::Evidence {
            attribution: Some(Attribution::Complete),
            unattributed_dirs: Some(Vec::new()),
            ..Default::default()
        };
        let partial = crate::doctor::finding::Evidence {
            attribution: Some(Attribution::Partial),
            unattributed_dirs: Some(vec![PathBuf::from("/games/wow/_classic_era_")]),
            ..Default::default()
        };
        insta::assert_json_snapshot!([complete, partial]);
    }
}
