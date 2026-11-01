//! Undoing a repair (E7, M4-D46): what the undo of a repair-family line would do, and the
//! strict checks that refuse it before anything moves. Without the strict checks an undo
//! would be an arbitrary-write primitive, so this half reads and never writes: no lock, no
//! journal, no command. The commit (E7's second part) runs [`verify`] again under the lock
//! and commits exactly the plan it returns.
//!
//! **The inverses** come from the history line's `repairs[]` alone, never from a run
//! directory, each written in its own orientation (a rename moves from `path` to `to`) with
//! the original's `post` as its `pre`:
//!
//! - a write over a file restores its backup's bytes and the mode it replaced;
//! - a write that created a file is undone by quarantining that file, never by deleting it;
//! - a chmod restores the mode it replaced, on the same file;
//! - a rename is renamed back, so a flatten's two or three renames go back one by one;
//! - a suspended journal file (a set-aside, M4-D38) comes back through the activation
//!   barrier: under its provisional name first, renamed to its own name last;
//! - an activation (undoing a reactivation) hides the journal under its provisional name
//!   first.
//!
//! **The order** is activation-aware (E14's rules 1 and 3): deactivations, then the
//! provisional restores, then every other inverse, newest reverted primitive first, then
//! the activations, one per journal a set-aside suspended (omad-i2c5). A suspended
//! journal thus comes back before any of its artifacts, so the
//! orphan sweep never takes them for a dead transaction's, and recovery never sees the
//! journal under its own name before the undo that restores it has settled.
//!
//! **The checks** ([`verify`]), in order, the first failure returned: the line is in
//! effect; a set-aside's guards hold; no later transaction overlaps what the undo writes;
//! no newer line this build cannot read may have; every inverse's `pre` holds on what the
//! inverses before it leave, and each backup holds the bytes it should; every path is in
//! the doctor's scopes and every rename stays on one mount; no client whose files move is
//! running.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde_json::{Value, json};

use super::guard::{self, GuardValue};
use super::inverse::expect_of;
use super::{
    CommitCtx, JournalName, LowerCtx, LowerError, Lowered, RepairCleanup, RepairMeta,
    RepairOutcome, RepairRun, RepairTx, UndoOf, abandon, commit, journal_file,
    lowered_from_inverses, provisional_journal,
};
use crate::discover::target::DiscoveredTarget;
use crate::doctor::fix_runtime::ACTIONS_LOG;
use crate::doctor::report::{RunState, UndoReport};
use crate::doctor::runtime::Runtime;
use crate::doctor::undo_script::{UNDO_SCRIPT, undo_script};
use crate::engine::fence;
use crate::engine::journal::{RecoveryEnv, recover_all};
use crate::engine::repair_action::{
    Endpoint, Endpoints, FixEntry, FixKind, Guard, Identity, Mode, Primitive, Stamp, Step,
};
use crate::engine::resource::{PathKind, Resource, ResourceSet};
use crate::engine::tx::{EngineCtx, RunningNow, TxOutcome, running_now};
use crate::engine::undo::cancelled_after;
use crate::engine::undo::{SvCounts, UndoFacts, UndoOutcome, UndoRequest};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::error::{Warning, WarningCode};
use crate::fsx::{self, GlobalLock, RepairPaths, failpoint};
use crate::fsx::{
    Access, Described, Expect, FileId, Found, Mismatch, OwnedFolders, Refusal, ScopeTable,
    StampDigest, Writer, canonical_lenient,
};
use crate::limits::DOCTOR_FIX_MAX_ACTIONS;
use crate::model::Target;
use crate::model::{FolderName, TargetKey, TxId, TxKind};
use crate::output::{DoctorEnvelope, Meta, RunKind};
use crate::state::history::{self, HistoryFilter, HistoryRead};
use crate::state::history::{HistoryEntry, Reach, RepairEntry, UnreadableLine};
use crate::state::paths::Paths;
use crate::state::records::load_record;

// ---------------------------------------------------------------------------------------
// The repair family, and whether a line of it is in effect.

/// A line of the repair family: a repair, or the undo of one (which carries the inverse
/// primitives). It names no target and moves no folder through `ops`; E12's read rule
/// admits exactly these target-less lines.
pub fn is_repair_family(e: &HistoryEntry) -> bool {
    e.target.is_none()
        && e.well_formed()
        && e.ops.is_empty()
        && (e.kind == TxKind::Repair || (e.kind == TxKind::Undo && !e.repairs.is_empty()))
}

/// Whether a transaction's change stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoState {
    InEffect,
    /// This undo of it is in effect.
    UndoneBy(TxId),
}

/// Whether `entries[at]` is in effect: it is, unless a later undo of it is itself in
/// effect, that is, not reverted by a later undo in effect in turn. So a repair undone and
/// redone is in effect again, and the newest undo of it that stands is the one that undid
/// it. (The folder family's rule, which counts any undo of the transaction, is not
/// changed: a redone folder transaction stays "already undone" there.)
pub fn undo_state(entries: &[HistoryEntry], at: usize) -> UndoState {
    let Some(reverted) = entries.get(at) else {
        return UndoState::InEffect;
    };
    let later = entries.get(at + 1..).unwrap_or_default();
    let cancelled = cancelled_after(later, reverted.tx);
    later
        .iter()
        .rev()
        .find(|e| {
            e.kind == TxKind::Undo && e.reverts == Some(reverted.tx) && !cancelled.contains(&e.tx)
        })
        .map_or(UndoState::InEffect, |e| UndoState::UndoneBy(e.tx))
}

// ---------------------------------------------------------------------------------------
// The plan.

/// What one inverse does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InverseKind {
    /// Writes the reverted write's backup back over its file, with the mode it replaced.
    RestoreWrite,
    /// Moves the file the reverted write created into the undo's quarantine.
    QuarantineCreated,
    /// Sets back the mode the reverted chmod replaced.
    ChmodBack,
    /// Renames the reverted rename's destination back to its source.
    RenameBack,
    /// Moves a suspended journal file from quarantine to its provisional name.
    ToProvisional,
    /// Renames a journal from its provisional name to its own: the activation.
    Activate,
    /// Renames a journal a reactivation restored back to its provisional name.
    Deactivate,
}

impl InverseKind {
    /// The primitive it is made of.
    pub const fn op(self) -> &'static str {
        match self {
            Self::RestoreWrite => "write_file",
            Self::ChmodBack => "chmod",
            Self::QuarantineCreated
            | Self::RenameBack
            | Self::ToProvisional
            | Self::Activate
            | Self::Deactivate => "rename_noreplace",
        }
    }

    /// Whether it is a rename.
    pub const fn renames(self) -> bool {
        !matches!(self, Self::RestoreWrite | Self::ChmodBack)
    }
}

/// The side of the machine whose quarantine receives a file the undo quarantines (M4-D35:
/// a quarantine never crosses a filesystem).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QuarantineSide {
    State,
    Config,
    Cache,
    /// An agent skill directory, under the home directory.
    Skill,
    /// This client's files.
    Client(TargetKey),
}

impl QuarantineSide {
    /// Its name in a token, as `doctor fix`'s names it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Config => "config",
            Self::Cache => "cache",
            Self::Skill => "skill",
            Self::Client(_) => "client",
        }
    }
}

/// Where an inverse moves its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dest {
    /// It moves nothing: a write or a chmod.
    None,
    Path(PathBuf),
    /// Into the undo's own quarantine on `side`, at `rel`: the commit resolves it below
    /// the undo's run (or the client's `<tx>.repair` half), which this plan does not know.
    Quarantine {
        side: QuarantineSide,
        rel: PathBuf,
    },
}

impl Dest {
    /// A rename's destination, when this plan knows it.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Path(path) => Some(path),
            Self::None | Self::Quarantine { .. } => None,
        }
    }
}

/// One inverse primitive, in the order the undo runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InverseStep {
    /// 1-based and contiguous over the undo.
    pub seq: u32,
    /// The reverted primitive's `seq`.
    pub reverts: u32,
    pub fix: u32,
    pub fm: String,
    pub kind: InverseKind,
    /// What it acts on: a rename's source.
    pub path: PathBuf,
    pub to: Dest,
    /// The mode a write or a chmod sets.
    pub mode: Option<Mode>,
    /// What must hold before it: the reverted primitive's `post`, oriented as it runs.
    pub pre: Endpoints,
    /// What holds after it: the reverted primitive's `pre`, but a renamed directory's
    /// stamp (the inverse moves what has the recorded identity, and edits inside a moved
    /// directory travel with it).
    pub post: Endpoints,
    /// The activation (E14): an `Activate` only.
    pub activation: bool,
    pub target: Option<TargetKey>,
    pub folder: Option<FolderName>,
    /// The reverted write's backup a `RestoreWrite` writes back.
    pub backup: Option<PathBuf>,
}

impl InverseStep {
    /// Its canonical form for a token (M4-D44): `{fm, fix, op, path, to_rel, mode,
    /// activation}`, nothing run-specific. A destination in the undo's own quarantine is
    /// `<side>/<path below it>`, as `doctor fix` names one; any other is the absolute
    /// path. `to_rel` and `mode` are omitted when the inverse has none, `activation` but
    /// on an activation.
    pub fn canonical(&self) -> Value {
        let mut value = json!({
            "fm": self.fm,
            "fix": self.fix,
            "op": self.kind.op(),
            "path": self.path.to_string_lossy(),
        });
        let to_rel = match &self.to {
            Dest::None => None,
            Dest::Path(path) => Some(path.to_string_lossy().into_owned()),
            Dest::Quarantine { side, rel } => Some(format!("{}/{}", side.name(), rel.display())),
        };
        if let Some(object) = value.as_object_mut() {
            if let Some(to_rel) = to_rel {
                object.insert("to_rel".to_owned(), json!(to_rel));
            }
            if let Some(mode) = self.mode {
                object.insert("mode".to_owned(), json!(mode.to_string()));
            }
            if self.kind == InverseKind::Activate {
                object.insert("activation".to_owned(), json!(true));
            }
        }
        value
    }
}

/// A reverted repair's backup an inverse write reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRead {
    pub path: PathBuf,
    /// The bytes it must hold: the reverted write's `pre` content.
    pub sha256: [u8; 32],
    /// The reverted repair's doctor run directory.
    pub run: String,
    /// The reverted primitive's `seq`, which names the backup in that run.
    pub seq: u32,
}

/// The undo of one repair-family line, planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairUndoPlan {
    pub reverted: TxId,
    /// A repair, or an undo (the plan is then a redo).
    pub reverted_kind: TxKind,
    /// The reverted line's doctor run directory.
    pub reverted_run: Option<String>,
    /// The clients the reverted line touched, sorted.
    pub targets: Vec<TargetKey>,
    /// In execution order, `seq` 1, 2, 3 …
    pub inverses: Vec<InverseStep>,
    /// The reverted backups the inverse writes restore from.
    pub reads: Vec<BackupRead>,
    /// Every guard of the reverted primitives, in their order.
    pub guards: Vec<Guard>,
    /// One per reverted fix, for the undo's `prepared` line.
    pub fixes: Vec<FixEntry>,
    /// What the inverses write (both ends of a rename, a quarantine's destination aside,
    /// which the commit adds once it resolves it) and the backups they read.
    pub resources: ResourceSet,
}

impl RepairUndoPlan {
    /// The canonical form of its inverses, in order (M4-D44): equal for two undos of the
    /// same line, whatever their own transaction, run or sequence numbers.
    pub fn canonical(&self) -> Value {
        Value::Array(self.inverses.iter().map(InverseStep::canonical).collect())
    }
}

/// The inverse of `entries[at]`, a repair-family line, read from its `repairs[]` alone.
/// It reads the filesystem only to resolve omaddons' own directories, and writes nothing.
/// A line it cannot invert is `state_corrupt`: more operations than a repair may have, a
/// path that is not absolute, canonical and free of `..`, an operation whose endpoints do
/// not fit it, a path below a client's `WTF/`, or an overwritten file whose mode is not
/// recorded. A line with no operation is `not_found`: there is nothing to undo.
pub fn plan_undo(entries: &[HistoryEntry], at: usize, paths: &Paths) -> Result<RepairUndoPlan> {
    let reverted = entries
        .get(at)
        .ok_or_else(|| Error::internal(format!("history line {at} does not exist")))?;
    if !is_repair_family(reverted) {
        return Err(Error::internal(format!(
            "transaction {} is not of the repair family, so no repair undo plans it",
            reverted.tx
        )));
    }
    if reverted.repairs.is_empty() {
        return Err(nothing_to_undo(reverted));
    }
    if reverted.repairs.len() > DOCTOR_FIX_MAX_ACTIONS {
        return Err(corrupt(
            reverted,
            &format!(
                "it records {} operations, more than the {DOCTOR_FIX_MAX_ACTIONS} a repair may",
                reverted.repairs.len()
            ),
        ));
    }
    let bases = Bases::of(paths);
    let mut steps: Vec<(&RepairEntry, Step)> = Vec::with_capacity(reverted.repairs.len());
    for entry in &reverted.repairs {
        // Before anything else, so no message ever names a path below `WTF/`.
        if bases.below_wtf(entry) {
            return Err(corrupt(
                reverted,
                &format!(
                    "operation {} names a path below a client's WTF/, which no repair writes",
                    entry.seq
                ),
            ));
        }
        let step = step_of(entry).map_err(|why| corrupt(reverted, &why))?;
        steps.push((entry, step));
    }
    let mut inverted = Vec::new();
    for (entry, step) in &steps {
        inverted.extend(invert(reverted, entry, step, &bases, paths)?);
    }
    // A set-aside of several journals comes back with one activation each, all last
    // (E14's rule 1 as omad-i2c5 widened it): `ordered` puts every provisional restore
    // first and every activation last.
    let mut inverses = ordered(inverted);
    let fixes = renumbered(fixes_of(&steps, &inverses), &mut inverses);
    let mut reads = Vec::new();
    for inverse in inverses
        .iter()
        .filter(|inverse| inverse.kind == InverseKind::RestoreWrite)
    {
        let (
            Some(backup),
            Some(run),
            Endpoints::One(Endpoint::File {
                sha256: Some(sha256),
                ..
            }),
        ) = (&inverse.backup, &reverted.doctor_run_id, &inverse.post)
        else {
            return Err(corrupt(
                reverted,
                &format!(
                    "operation {} overwrote a file, but the line names no doctor run to hold its backup",
                    inverse.reverts
                ),
            ));
        };
        reads.push(BackupRead {
            path: backup.clone(),
            sha256: sha256.0,
            run: run.clone(),
            seq: inverse.reverts,
        });
    }
    let resources = resources_of(reverted, &inverses, &reads)?;
    let guards: Vec<Guard> = steps
        .iter()
        .flat_map(|(_, step)| step.guards.iter().cloned())
        .collect();
    let mut targets: BTreeSet<TargetKey> = reverted.targets.iter().copied().collect();
    targets.extend(reverted.repairs.iter().filter_map(|entry| entry.target));
    // A set-aside's guards watch a client whose files it may not have touched (a journal
    // with no client-side artifact left): the undo reads them on that client, so it is
    // among the undo's clients. A folder's epoch names its client by path alone; the
    // set-aside's marker guard names the same client by key.
    targets.extend(guards.iter().filter_map(|g| guard::target(g, &[])));
    Ok(RepairUndoPlan {
        reverted: reverted.tx,
        reverted_kind: reverted.kind,
        reverted_run: reverted.doctor_run_id.clone(),
        targets: targets.into_iter().collect(),
        fixes,
        guards,
        inverses,
        reads,
        resources,
    })
}

/// A history operation as the journal step it was (E1's [`Step`]): the two write the same
/// keys, `target` and `folder` aside, and it must validate as a step does.
fn step_of(entry: &RepairEntry) -> std::result::Result<Step, String> {
    let mut value = serde_json::to_value(entry).map_err(|err| err.to_string())?;
    if let Some(object) = value.as_object_mut() {
        object.remove("target");
        object.remove("folder");
    }
    let step: Step = serde_json::from_value(value)
        .map_err(|err| format!("operation {} does not read as a step: {err}", entry.seq))?;
    step.validate().map_err(|err| err.to_string())?;
    Ok(step)
}

/// The inverses of one reverted primitive: one, or two for a suspended journal file (its
/// provisional restore and its activation). Each is numbered later.
fn invert(
    reverted: &HistoryEntry,
    entry: &RepairEntry,
    step: &Step,
    bases: &Bases,
    paths: &Paths,
) -> Result<Vec<InverseStep>> {
    let make = |kind, path: &Path, to: Dest, mode, pre, post| InverseStep {
        seq: 0,
        reverts: step.seq,
        fix: step.fix,
        fm: step.fm.to_string(),
        kind,
        path: path.to_path_buf(),
        to,
        mode,
        pre,
        post,
        activation: kind == InverseKind::Activate,
        target: entry.target,
        folder: entry.folder.clone(),
        backup: None,
    };
    let refuse = |why: &str| corrupt(reverted, &format!("operation {}: {why}", step.seq));
    match (&step.op, &step.pre, &step.post) {
        (Primitive::WriteFile { path, backup, .. }, Endpoints::One(pre), Endpoints::One(post)) => {
            match (pre, backup) {
                (Endpoint::Absent, _) => {
                    let (side, rel) = bases.side_of(path, entry.target).ok_or_else(|| {
                        refuse("the file it created is on no side with a quarantine")
                    })?;
                    Ok(vec![make(
                        InverseKind::QuarantineCreated,
                        path,
                        Dest::Quarantine { side, rel },
                        None,
                        Endpoints::Pair {
                            path: post.clone(),
                            to: Endpoint::Absent,
                        },
                        Endpoints::Pair {
                            path: Endpoint::Absent,
                            to: Endpoint::File {
                                identity: None,
                                sha256: None,
                                mode: None,
                            },
                        },
                    )])
                }
                (
                    Endpoint::File {
                        sha256: Some(sha256),
                        mode: Some(mode),
                        ..
                    },
                    Some(backup),
                ) => {
                    let mut inverse = make(
                        InverseKind::RestoreWrite,
                        path,
                        Dest::None,
                        Some(*mode),
                        Endpoints::One(post.clone()),
                        Endpoints::One(Endpoint::File {
                            identity: None,
                            sha256: Some(*sha256),
                            mode: Some(*mode),
                        }),
                    );
                    inverse.backup = Some(backup.clone());
                    Ok(vec![inverse])
                }
                (Endpoint::File { mode: None, .. }, _) => Err(refuse(
                    "it records no mode of the file it overwrote, so that file cannot be put back as it was",
                )),
                _ => Err(refuse(
                    "it is neither a write that created its file nor one that kept a backup",
                )),
            }
        }
        (
            Primitive::Chmod { path, .. },
            Endpoints::One(Endpoint::File {
                identity,
                mode: Some(old),
                ..
            }),
            Endpoints::One(post),
        ) => Ok(vec![make(
            InverseKind::ChmodBack,
            path,
            Dest::None,
            Some(*old),
            Endpoints::One(post.clone()),
            Endpoints::One(Endpoint::File {
                identity: *identity,
                sha256: None,
                mode: Some(*old),
            }),
        )]),
        (
            Primitive::RenameNoReplace {
                from,
                to,
                activation,
            },
            Endpoints::Pair { path: source, .. },
            Endpoints::Pair { to: moved, .. },
        ) => {
            let pre = Endpoints::Pair {
                path: moved.clone(),
                to: Endpoint::Absent,
            };
            let back = Endpoints::Pair {
                path: Endpoint::Absent,
                to: without_stamp(source),
            };
            if *activation {
                return Ok(vec![make(
                    InverseKind::Deactivate,
                    to,
                    Dest::Path(from.clone()),
                    None,
                    pre,
                    back,
                )]);
            }
            if let Some((_, JournalName::Reactivating)) = journal_file(paths, from) {
                // A journal moved out of its provisional name: the undo of a reactivation
                // (a redo) setting it aside again. Its inverse is the provisional restore,
                // which runs first.
                return Ok(vec![make(
                    InverseKind::ToProvisional,
                    to,
                    Dest::Path(from.clone()),
                    None,
                    pre,
                    back,
                )]);
            }
            if let Some((tx, JournalName::Folder | JournalName::Wtf | JournalName::Repair)) =
                journal_file(paths, from)
            {
                let provisional = bases.provisional(paths, &tx);
                // ubs:ignore — comparing paths, not secrets
                if *to == provisional {
                    // A deactivation (an undo's, undoing an activation): the journal moved
                    // from its own name to its provisional one. Its inverse is the
                    // activation alone, back under its own name, last.
                    return Ok(vec![make(
                        InverseKind::Activate,
                        to,
                        Dest::Path(from.clone()),
                        None,
                        pre,
                        back,
                    )]);
                }
                let held = Endpoints::Pair {
                    path: Endpoint::Absent,
                    to: moved.clone(),
                };
                let activate = make(
                    InverseKind::Activate,
                    &provisional,
                    Dest::Path(from.clone()),
                    None,
                    Endpoints::Pair {
                        path: moved.clone(),
                        to: Endpoint::Absent,
                    },
                    back,
                );
                return Ok(vec![
                    make(
                        InverseKind::ToProvisional,
                        to,
                        Dest::Path(provisional),
                        None,
                        pre,
                        held,
                    ),
                    activate,
                ]);
            }
            Ok(vec![make(
                InverseKind::RenameBack,
                to,
                Dest::Path(from.clone()),
                None,
                pre,
                back,
            )])
        }
        _ => Err(refuse("its endpoints do not fit its operation")),
    }
}

/// `endpoint` without a directory's stamp.
fn without_stamp(endpoint: &Endpoint) -> Endpoint {
    match endpoint {
        Endpoint::Dir { identity, .. } => Endpoint::Dir {
            identity: *identity,
            stamp: None,
        },
        other => other.clone(),
    }
}

/// `inverted` in the order the undo runs it, numbered 1, 2, 3 …: deactivations, then the
/// provisional restores, then every other inverse, then the activation; within each, the
/// newest reverted primitive first.
fn ordered(mut inverted: Vec<InverseStep>) -> Vec<InverseStep> {
    // Stable sorts: a suspended journal's provisional restore stays ahead of its
    // activation, which share their reverted primitive.
    inverted.sort_by_key(|inverse| std::cmp::Reverse(inverse.reverts));
    inverted.sort_by_key(|inverse| match inverse.kind {
        InverseKind::Deactivate => 0,
        InverseKind::ToProvisional => 1,
        InverseKind::Activate => 3,
        _ => 2,
    });
    for (seq, inverse) in (1..).zip(&mut inverted) {
        inverse.seq = seq;
    }
    inverted
}

/// One fix entry per reverted fix, its kind from its primitives (history records no kind,
/// and the entry only describes): a guard or a journal it suspends or restores, a
/// suspension; then a write, a chmod; two or three renames of one client's AddOns
/// entries, a flatten; otherwise a quarantine.
fn fixes_of(steps: &[(&RepairEntry, Step)], inverses: &[InverseStep]) -> Vec<FixEntry> {
    let mut by_fix: BTreeMap<u32, (Cow<'static, str>, Vec<&Step>)> = BTreeMap::new();
    for (_, step) in steps {
        by_fix
            .entry(step.fix)
            .or_insert_with(|| (step.fm.clone(), Vec::new()))
            .1
            .push(step);
    }
    by_fix
        .into_iter()
        .map(|(fix, (fm, steps))| {
            let journals = inverses.iter().any(|inverse| {
                inverse.fix == fix
                    && matches!(
                        inverse.kind,
                        InverseKind::ToProvisional
                            | InverseKind::Activate
                            | InverseKind::Deactivate
                    )
            });
            let any = |op: fn(&Primitive) -> bool| steps.iter().any(|step| op(&step.op));
            let kind = if journals || steps.iter().any(|step| !step.guards.is_empty()) {
                FixKind::Suspend
            } else if any(|op| matches!(op, Primitive::WriteFile { .. })) {
                FixKind::Write
            } else if any(|op| matches!(op, Primitive::Chmod { .. })) {
                FixKind::Chmod
            } else if flattens(&steps) {
                FixKind::Flatten
            } else {
                FixKind::Quarantine
            };
            FixEntry { fix, fm, kind }
        })
        .collect()
}

/// Numbers the undo's fixes 1, 2, 3 … in the reverted fixes' order, as a repair journal
/// requires (`validate_steps`), and each inverse by its fix's new number: a reverted
/// repair's delegations lowered to no primitive, so its line may name fixes 1 and 3 alone.
fn renumbered(mut fixes: Vec<FixEntry>, inverses: &mut [InverseStep]) -> Vec<FixEntry> {
    let numbers: BTreeMap<u32, u32> = fixes.iter().map(|entry| entry.fix).zip(1..).collect();
    for entry in &mut fixes {
        entry.fix = numbers.get(&entry.fix).copied().unwrap_or(entry.fix);
    }
    for inverse in inverses {
        inverse.fix = numbers.get(&inverse.fix).copied().unwrap_or(inverse.fix);
    }
    fixes
}

/// Whether `steps` are two or three renames whose sources are entries of one client's
/// `Interface/AddOns`.
fn flattens(steps: &[&Step]) -> bool {
    let clients: Option<BTreeSet<PathBuf>> = steps
        .iter()
        .map(|step| match &step.op {
            Primitive::RenameNoReplace { from, .. } => addons_client(from),
            _ => None,
        })
        .collect();
    (2..=3).contains(&steps.len()) && clients.is_some_and(|clients| clients.len() == 1)
}

/// What the inverses write and read, as resources: a write's or a chmod's path, both ends
/// of a rename known here, and each backup read.
fn resources_of(
    reverted: &HistoryEntry,
    inverses: &[InverseStep],
    reads: &[BackupRead],
) -> Result<ResourceSet> {
    let resource = |path: &Path, kind| {
        Resource::from_canonical(path, kind).map_err(|err| corrupt(reverted, &err.to_string()))
    };
    let mut set = ResourceSet::new();
    for inverse in inverses {
        let kind = match &inverse.pre {
            Endpoints::Pair {
                path: Endpoint::Dir { .. },
                ..
            } => PathKind::Subtree,
            _ => PathKind::File,
        };
        set.write(resource(&inverse.path, kind)?);
        if let Some(to) = inverse.to.path() {
            set.write(resource(to, kind)?);
        }
    }
    for read in reads {
        set.read(
            Resource::backup(&read.run, read.seq)
                .map_err(|err| corrupt(reverted, &err.to_string()))?,
        );
    }
    Ok(set)
}

/// omaddons' own directories, canonical, to place a path by.
struct Bases {
    state: PathBuf,
    config: PathBuf,
    cache: PathBuf,
    home: PathBuf,
    journal: PathBuf,
}

impl Bases {
    fn of(paths: &Paths) -> Self {
        let whole = |dir: &Path| canonical_lenient(dir).unwrap_or_else(|| dir.to_path_buf());
        Self {
            state: whole(paths.state_dir()),
            config: whole(paths.config_dir()),
            cache: whole(paths.cache_dir()),
            home: whole(paths.home()),
            journal: whole(&paths.journal_dir()),
        }
    }

    fn app_owned(&self, path: &Path) -> bool {
        [&self.state, &self.config, &self.cache]
            .into_iter()
            .any(|root| path.starts_with(root))
    }

    /// Whether a path of `entry` outside omaddons' own directories has a `WTF` component,
    /// in any case: a client's settings, which no repair writes.
    fn below_wtf(&self, entry: &RepairEntry) -> bool {
        [Some(&entry.path), entry.to.as_ref(), entry.backup.as_ref()]
            .into_iter()
            .flatten()
            .filter(|path| !self.app_owned(path))
            .any(|path| {
                path.components().any(
                    |c| matches!(c, Component::Normal(name) if name.eq_ignore_ascii_case("WTF")),
                )
            })
    }

    /// The quarantine side of `path`, and its path below that side's base: omaddons'
    /// state, config and cache by their directories, a client's files (`target`'s, which
    /// must be known) from its `Interface/`, else a skill directory's below the home.
    fn side_of(&self, path: &Path, target: Option<TargetKey>) -> Option<(QuarantineSide, PathBuf)> {
        let below = |root: &Path| {
            path.strip_prefix(root)
                .ok()
                .filter(|rel| !rel.as_os_str().is_empty())
                .map(Path::to_path_buf)
        };
        for (side, root) in [
            (QuarantineSide::State, &self.state),
            (QuarantineSide::Config, &self.config),
            (QuarantineSide::Cache, &self.cache),
        ] {
            if let Some(rel) = below(root) {
                return Some((side, rel));
            }
        }
        if let Some((_, rel)) = client_parts(path) {
            return Some((QuarantineSide::Client(target?), rel));
        }
        below(&self.home).map(|rel| (QuarantineSide::Skill, rel))
    }

    /// `tx`'s provisional journal, canonical.
    fn provisional(&self, paths: &Paths, tx: &TxId) -> PathBuf {
        let provisional = provisional_journal(paths, tx);
        match provisional.strip_prefix(paths.journal_dir()) {
            Ok(name) => self.journal.join(name),
            Err(_) => provisional,
        }
    }
}

/// Where `path` is a client's file: its client directory and its path from `Interface/`,
/// where `Interface` is followed by `AddOns` or `.omaddons` (in any case), the last such.
fn client_parts(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let parts: Vec<Component<'_>> = path.components().collect();
    let named = |c: &Component<'_>, want: &str| matches!(c, Component::Normal(name) if name.eq_ignore_ascii_case(want));
    let at = (0..parts.len().saturating_sub(1)).rev().find(|&i| {
        named(&parts[i], "Interface")
            && (named(&parts[i + 1], "AddOns") || named(&parts[i + 1], ".omaddons"))
    })?;
    let client = parts[..at].iter().collect();
    let rel = parts[at..].iter().collect();
    Some((client, rel))
}

/// The client directory whose `Interface/AddOns` holds `path` as an entry or below one.
fn addons_client(path: &Path) -> Option<PathBuf> {
    let (client, rel) = client_parts(path)?;
    let mut rel = rel.components();
    rel.next();
    let addons = rel.next()?;
    (addons.as_os_str().eq_ignore_ascii_case("AddOns") && rel.next().is_some()).then_some(client)
}

/// The key of the AddOns folder `path` is or is in.
fn addons_folder_key(path: &Path) -> Option<String> {
    let (_, rel) = client_parts(path)?;
    let mut rel = rel.components();
    rel.next();
    let addons = rel.next()?;
    if !addons.as_os_str().eq_ignore_ascii_case("AddOns") {
        return None;
    }
    let name = rel.next()?.as_os_str().to_str()?;
    FolderName::new(name).ok().map(|f| f.key().to_owned())
}

// ---------------------------------------------------------------------------------------
// The strict checks.

/// A client the undo may find, as discovery found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoClient {
    pub key: TargetKey,
    /// Its id, for messages.
    pub id: String,
    pub dir: PathBuf,
}

/// What [`verify`] reads besides the plan.
pub struct VerifyCtx<'a> {
    pub paths: &'a Paths,
    /// The whole history, oldest first, as `history::read` returns it.
    pub entries: &'a [HistoryEntry],
    /// The lines it could not read.
    pub unreadable: &'a [UnreadableLine],
    /// The reverted line.
    pub at: usize,
    /// The discovered clients of the line's targets.
    pub clients: &'a [UndoClient],
    /// The table the commit will write through (skill directories applied).
    pub scopes: &'a ScopeTable,
    /// Whether a client runs now; the caller passes `running_now`, which fails closed.
    pub running: &'a dyn Fn(TargetKey) -> bool,
    /// `fsx::mount_id` in production; a seam in unit tests.
    pub mount_id: &'a dyn Fn(&Path) -> io::Result<u64>,
}

/// What the checks read, for the undo's token (E7's third part): each inverse's `pre` as
/// it was checked (a moved directory's stamp and a created file's identity filled in),
/// each backup's digest, each guard's value now and each target's record generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedFacts {
    pub predicates: Vec<Endpoints>,
    pub backups: Vec<(PathBuf, [u8; 32])>,
    pub guards: Vec<GuardValue>,
    pub generations: Vec<(TargetKey, u64)>,
}

/// A plan that passed every check, with what the checks read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPlan {
    /// The plan, each inverse's `pre` and `post` completed from what is there now: a
    /// moved directory's stamp and a symlink's target in `pre`, a created file's identity
    /// in `pre` and `post`.
    pub plan: RepairUndoPlan,
    pub observed: ObservedFacts,
}

/// Checks `plan` strictly against the world now, reading only, and returns the first
/// failure in this order:
///
/// 1. the line is in effect, else `undo_conflict` (already undone);
/// 2. every guard of a set-aside holds, else `undo_conflict`
///    (`changed_since_suspension`, `details.changed_guards`, `details.target`);
/// 3. no later transaction that stands overlaps what the undo writes, else `undo_conflict`
///    (`changed_since_repair`, `details.paths` and `conflicting`);
/// 4. no newer line this build cannot read may have touched it, else `undo_conflict`
///    (`history_unreadable`);
/// 5. every inverse's `pre` holds on what the inverses before it leave, and each backup is
///    a regular file with the bytes it kept, else `undo_conflict` (`changed_since_repair`,
///    `details.paths`);
/// 6. every path is in the doctor's write scopes (`scope_denied`, `protected_folder`), and
///    every rename stays on one mount (`undo_conflict`, `changed_since_repair`);
/// 7. no client whose files the undo moves is running (`game_running`, `repair`).
pub fn verify(plan: RepairUndoPlan, ctx: &VerifyCtx<'_>) -> Result<VerifiedPlan> {
    let reverted = ctx
        .entries
        .get(ctx.at)
        .filter(|entry| entry.tx == plan.reverted)
        .ok_or_else(|| {
            Error::internal(format!(
                "the undo of {} was checked against another history line",
                plan.reverted
            ))
        })?;
    let clients: Vec<(TargetKey, PathBuf)> = ctx
        .clients
        .iter()
        .map(|client| (client.key, client.dir.clone()))
        .collect();
    if let UndoState::UndoneBy(by) = undo_state(ctx.entries, ctx.at) {
        return Err(already_undone(reverted, by));
    }
    let guards = check_guards(reverted, &plan, ctx, &clients)?;
    check_later(reverted, &plan, ctx)?;
    check_unreadable(reverted, ctx)?;
    let simulated = simulate(reverted, &plan, ctx)?;
    if !simulated.changed.is_empty() {
        return Err(changed_since_repair(reverted, &simulated.changed));
    }
    let mut plan = plan;
    plan.inverses = simulated.inverses;
    check_scopes(reverted, &plan, ctx)?;
    if let Some(mounts) = simulated.mounts {
        return Err(mounts);
    }
    check_running(reverted, ctx)?;
    let mut generations = Vec::with_capacity(plan.targets.len());
    for key in &plan.targets {
        generations.push((*key, load_record(ctx.paths, key)?.value.generation));
    }
    let predicates = plan
        .inverses
        .iter()
        .map(|inverse| inverse.pre.clone())
        .collect();
    Ok(VerifiedPlan {
        observed: ObservedFacts {
            predicates,
            backups: simulated.backups,
            guards,
            generations,
        },
        plan,
    })
}

/// Check 2: every guard's value now, or the refusal naming the kinds that changed.
fn check_guards(
    reverted: &HistoryEntry,
    plan: &RepairUndoPlan,
    ctx: &VerifyCtx<'_>,
    clients: &[(TargetKey, PathBuf)],
) -> Result<Vec<GuardValue>> {
    let mut values = Vec::with_capacity(plan.guards.len());
    for guard in &plan.guards {
        values.push(guard::current(guard, clients, ctx.paths)?);
    }
    let changed = guard::changed_kinds(&plan.guards, &values);
    if changed.is_empty() {
        return Ok(values);
    }
    // The first changed guard names the fix, and the client it is about.
    let first = plan
        .guards
        .iter()
        .zip(&values)
        .position(|(guard, value)| guard::recorded(guard) != *value);
    let owner = first.and_then(|index| {
        let mut seen = 0;
        reverted.repairs.iter().find(|entry| {
            let range = seen..seen + entry.guards.len();
            seen = range.end;
            range.contains(&index)
        })
    });
    let target = first
        .and_then(|index| guard::target(&plan.guards[index], clients))
        .or_else(|| owner.and_then(|entry| entry.target));
    let fm = owner.map_or_else(
        || plan.fixes.first().map(|f| f.fm.to_string()),
        |entry| Some(entry.fm.clone()),
    );
    Err(changed_since_suspension(
        reverted,
        &changed,
        target,
        fm.as_deref(),
        ctx.clients,
    ))
}

/// Check 3, the repair direction (M4-D62): each later line that stands, and is not an undo
/// of this one, conflicts when it wrote what the undo writes. A repair-family line by its
/// operations' paths; a folder transaction on a client the reverted line reached, by the
/// AddOns folders its operations name and, where the repair wrote them, by the client's
/// record and files index.
fn check_later(reverted: &HistoryEntry, plan: &RepairUndoPlan, ctx: &VerifyCtx<'_>) -> Result<()> {
    let later = ctx.entries.get(ctx.at + 1..).unwrap_or_default();
    let cancelled = cancelled_after(later, reverted.tx);
    let writes: Vec<&Resource> = plan
        .resources
        .writes()
        .iter()
        .filter(|resource| resource.path().is_some())
        .collect();
    let reach = reverted.reach();
    let mut conflicting: Vec<(&HistoryEntry, BTreeSet<FolderName>)> = Vec::new();
    let mut overlapped: BTreeSet<PathBuf> = BTreeSet::new();
    for line in later {
        if cancelled.contains(&line.tx) || line.reverts == Some(reverted.tx) {
            continue;
        }
        let hit = if is_repair_family(line) {
            repair_overlap(line, &writes)
        } else {
            folder_overlap(line, reverted, &reach, ctx.paths)
        };
        if let Some((folders, paths)) = hit {
            overlapped.extend(paths);
            conflicting.push((line, folders));
        }
    }
    let Some((newest, _)) = conflicting.last() else {
        return Ok(());
    };
    let listed: Vec<Value> = conflicting
        .iter()
        .map(|(line, folders)| {
            let folders: Vec<&str> = folders.iter().map(FolderName::as_str).collect();
            json!({ "tx": line.tx, "kind": line.kind, "folders": folders })
        })
        .collect();
    Err(Error::new(
        ErrorCode::UndoConflict,
        format!(
            "{} later transaction(s) changed what {} repaired; the newest is {}",
            conflicting.len(),
            reverted.tx,
            newest.tx
        ),
        Suggestion::command([
            "omaddons".to_owned(),
            "undo".to_owned(),
            newest.tx.to_string(),
        ]),
    )
    .details(json!({
        "tx": reverted.tx,
        "reason": "changed_since_repair",
        "paths": path_texts(&overlapped),
        "conflicting": listed,
    })))
}

/// The AddOns folders a later repair-family `line` touched and the undo's paths it
/// overlapped, when it wrote what the undo writes. A path of it that is no resource counts
/// as overlapping: a conflict the history hides is never assumed away.
fn repair_overlap(
    line: &HistoryEntry,
    writes: &[&Resource],
) -> Option<(BTreeSet<FolderName>, BTreeSet<PathBuf>)> {
    let mut folders = BTreeSet::new();
    let mut overlapped = BTreeSet::new();
    for entry in &line.repairs {
        let theirs: Vec<Option<Resource>> =
            [Some(&entry.path), entry.to.as_ref(), entry.backup.as_ref()]
                .into_iter()
                .flatten()
                .map(|path| {
                    Resource::from_canonical(path, PathKind::File)
                        .or_else(|_| Resource::canonical(path, PathKind::File))
                        .ok()
                })
                .collect();
        for ours in writes {
            if theirs
                .iter()
                .any(|theirs| theirs.as_ref().is_none_or(|theirs| ours.overlaps(theirs)))
            {
                overlapped.extend(ours.path().map(Path::to_path_buf));
                folders.extend(entry.addon_folders());
            }
        }
    }
    (!overlapped.is_empty()).then_some((folders, overlapped))
}

/// The AddOns folders a later folder transaction `line` changed that the reverted repair
/// touched on a client it reached, and the repair's paths among them; or the client's
/// record or files index, whenever the repair wrote them (every committed change rewrites
/// both, and a repair writes them from the home side, reaching no client).
fn folder_overlap(
    line: &HistoryEntry,
    reverted: &HistoryEntry,
    reach: &Reach,
    paths: &Paths,
) -> Option<(BTreeSet<FolderName>, BTreeSet<PathBuf>)> {
    let target = line.target?;
    if !line.kind.is_undoable() {
        return None;
    }
    let reached = match reach {
        Reach::Targets(keys) => keys.contains(&target),
        Reach::Every => true,
    };
    let keys: BTreeSet<&str> = line
        .ops
        .iter()
        .filter(|op| op.edit.is_none() && op.op.is_folder())
        .map(|op| op.folder.key())
        .collect();
    let whole = |path: PathBuf| canonical_lenient(&path).unwrap_or(path);
    let records = [
        whole(paths.target_record(&target)),
        whole(paths.files_index(&target)),
    ];
    let mut folders = BTreeSet::new();
    let mut overlapped = BTreeSet::new();
    for entry in &reverted.repairs {
        let named: Vec<&PathBuf> = [Some(&entry.path), entry.to.as_ref()]
            .into_iter()
            .flatten()
            .collect();
        if reached && entry.target.is_none_or(|key| key == target) {
            let hit: BTreeSet<FolderName> = entry
                .addon_folders()
                .into_iter()
                .filter(|folder| keys.contains(folder.key()))
                .collect();
            if !hit.is_empty() {
                let mut in_folders = named.iter().filter(|path| {
                    addons_folder_key(path).is_some_and(|key| keys.contains(key.as_str()))
                });
                match in_folders.next() {
                    Some(first) => {
                        overlapped.insert((*first).clone());
                        overlapped.extend(in_folders.map(|path| (*path).clone()));
                    }
                    None => {
                        overlapped.insert(entry.path.clone());
                    }
                }
                folders.extend(hit);
            }
        }
        for path in &named {
            if records
                .iter()
                .any(|record| path.starts_with(record) || record.starts_with(path))
            {
                overlapped.insert((*path).clone());
            }
        }
    }
    (!overlapped.is_empty()).then_some((folders, overlapped))
}

/// Check 4 (M3-D28 rule 5): a newer line this build cannot read refuses, unless it names
/// a client none of the operations touch and every operation is a named client's.
fn check_unreadable(reverted: &HistoryEntry, ctx: &VerifyCtx<'_>) -> Result<()> {
    let touched: BTreeSet<TargetKey> = reverted
        .repairs
        .iter()
        .filter_map(|entry| entry.target)
        .collect();
    // A home-side operation, or a client's that names no client, is never ruled out.
    let clients_only = reverted
        .repairs
        .iter()
        .all(|entry| entry.client_side() && entry.target.is_some());
    let rules_out = |line: &UnreadableLine| {
        clients_only
            && serde_json::from_str::<Value>(&line.raw)
                .ok()
                .and_then(|value| value.get("target")?.as_str()?.parse::<TargetKey>().ok())
                .is_some_and(|key| !touched.contains(&key))
    };
    let newer = ctx
        .unreadable
        .iter()
        .filter(|line| line.after > ctx.at && !rules_out(line))
        .count();
    if newer == 0 {
        return Ok(());
    }
    let plural = if newer == 1 { "" } else { "s" };
    Err(Error::new(
        ErrorCode::UndoConflict,
        format!(
            "history.jsonl has {newer} newer line{plural} this omaddons cannot read; they may have changed what {} repaired since, so undoing it could overwrite that",
            reverted.tx
        ),
        Suggestion::text("a newer omaddons wrote that history; undo it with that version"),
    )
    .details(json!({
        "tx": reverted.tx,
        "reason": "history_unreadable",
        "lines": newer,
    })))
}

/// What check 5 found, and the mount check it made on the way.
struct Simulated {
    /// The inverses, completed from the live paths.
    inverses: Vec<InverseStep>,
    /// The paths whose `pre` did not hold, and the backups that do not hold their bytes.
    changed: BTreeSet<PathBuf>,
    backups: Vec<(PathBuf, [u8; 32])>,
    /// Check 6's mount refusal, reported after the scopes.
    mounts: Option<Error>,
}

/// Check 5: each inverse's `pre` against what the inverses before it leave (a virtual
/// map of the paths they name: a flatten's third rename back finds the folder its second
/// put in place), then what it moves completed from there, then its effect. A moved
/// directory travels with what is below it.
fn simulate(
    reverted: &HistoryEntry,
    plan: &RepairUndoPlan,
    ctx: &VerifyCtx<'_>,
) -> Result<Simulated> {
    let mut view = Virtual::default();
    let mut changed = BTreeSet::new();
    let mut mounts = None;
    let mut inverses = plan.inverses.clone();
    for inverse in &mut inverses {
        for (path, endpoint) in sides(inverse) {
            let found = view
                .check(&path, &expect_of(&endpoint))
                .map_err(|err| unreadable(&path, &err))?;
            if found.is_some() {
                changed.insert(path);
            }
        }
        complete(&view, inverse)?;
        if mounts.is_none()
            && inverse.kind.renames()
            && let Some(to) = inverse.to.path()
            && let Err(refusal) = same_mount(&view, reverted, &inverse.path, to, ctx)
        {
            mounts = Some(refusal);
        }
        view.apply(inverse);
    }
    let mut backups = Vec::with_capacity(plan.reads.len());
    for read in &plan.reads {
        let found = Expect::hash(read.sha256)
            .check(&read.path)
            .map_err(|err| unreadable(&read.path, &err))?;
        if found.is_some() {
            changed.insert(read.path.clone());
        }
        backups.push((read.path.clone(), read.sha256));
    }
    Ok(Simulated {
        inverses,
        changed,
        backups,
        mounts,
    })
}

/// Each path an inverse's `pre` describes, with its endpoint: its path, and a rename's
/// destination when the plan knows it (the undo's own quarantine is new, and its commit
/// checks it).
fn sides(inverse: &InverseStep) -> Vec<(PathBuf, Endpoint)> {
    match &inverse.pre {
        Endpoints::One(endpoint) => vec![(inverse.path.clone(), endpoint.clone())],
        Endpoints::Pair { path, to } => {
            let mut sides = vec![(inverse.path.clone(), path.clone())];
            if let Some(dest) = inverse.to.path() {
                sides.push((dest.to_path_buf(), to.clone()));
            }
            sides
        }
    }
}

/// Fills in what an inverse moves as it is now, which its commit's step must record: a
/// created file's identity (in `pre` and `post`), a directory's stamp and a symlink's
/// target (in `pre`).
fn complete(view: &Virtual, inverse: &mut InverseStep) -> Result<()> {
    let Endpoints::Pair { path: source, .. } = &mut inverse.pre else {
        return Ok(());
    };
    match source {
        Endpoint::File { identity, .. } if inverse.kind == InverseKind::QuarantineCreated => {
            let live = view.identity(&inverse.path);
            *identity = live;
            if let Endpoints::Pair {
                to: Endpoint::File { identity, .. },
                ..
            } = &mut inverse.post
            {
                *identity = live;
            }
        }
        Endpoint::Dir {
            stamp: stamp @ None,
            ..
        } => *stamp = view.stamp(&inverse.path)?,
        Endpoint::Symlink {
            link_target: link_target @ None,
            ..
        } => *link_target = view.link_target(&inverse.path),
        _ => {}
    }
    Ok(())
}

/// Check 6's mounts: `from`, where it is now, and `to`'s folder are on one mount. A kernel
/// that gives no mount id fails closed, as a mismatch.
fn same_mount(
    view: &Virtual,
    reverted: &HistoryEntry,
    from: &Path,
    to: &Path,
    ctx: &VerifyCtx<'_>,
) -> Result<()> {
    let mount = |path: &Path| -> Result<Option<u64>> {
        let real = match view.origin(path) {
            Origin::Disk(real) => real,
            Origin::Gone | Origin::Set(_) => path.to_path_buf(),
        };
        let Some(existing) = real
            .ancestors()
            .find(|ancestor| fs::symlink_metadata(ancestor).is_ok())
        else {
            return Ok(None);
        };
        match (ctx.mount_id)(existing) {
            Ok(id) => Ok(Some(id)),
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) if err.kind() == io::ErrorKind::Unsupported => Ok(None),
            Err(err) => Err(unreadable(existing, &err)),
        }
    };
    let source = mount(from)?;
    let destination = mount(to.parent().unwrap_or(to))?;
    if source.is_some() && source == destination {
        return Ok(());
    }
    let ends: BTreeSet<PathBuf> = [from.to_path_buf(), to.to_path_buf()].into();
    Err(Error::new(
        ErrorCode::UndoConflict,
        format!(
            "{} and {} are not on one mount now, so the undo of {} cannot move between them; nothing was moved",
            from.display(),
            to.display(),
            reverted.tx
        ),
        Suggestion::command(["omaddons", "history"]),
    )
    .details(json!({
        "tx": reverted.tx,
        "reason": "changed_since_repair",
        "paths": path_texts(&ends),
    })))
}

/// Check 6's scopes: every path an inverse writes is one the doctor may, as the commit's
/// table says. A path below none of omaddons' own folders and no discovered client is a
/// skill directory the table does not admit (`details.reason: "skill_dir"`).
fn check_scopes(reverted: &HistoryEntry, plan: &RepairUndoPlan, ctx: &VerifyCtx<'_>) -> Result<()> {
    for inverse in &plan.inverses {
        let access = if inverse.kind.renames() {
            Access::Rename
        } else {
            Access::Write
        };
        let ends = std::iter::once(inverse.path.as_path()).chain(inverse.to.path());
        for path in ends {
            if let Err(refusal) =
                ctx.scopes
                    .check(Writer::Doctor, &OwnedFolders::default(), path, access)
            {
                return Err(scope_refused(reverted, path, refusal, ctx));
            }
        }
    }
    Ok(())
}

fn scope_refused(
    reverted: &HistoryEntry,
    path: &Path,
    refusal: Refusal,
    ctx: &VerifyCtx<'_>,
) -> Error {
    let fix = Suggestion::text(
        "nothing was moved; the repair wrote where this omaddons may not write, so put back what you need by hand",
    );
    match refusal {
        Refusal::Protected(message) => Error::new(
            ErrorCode::ProtectedFolder,
            format!("{message}; the undo of {} never writes it", reverted.tx),
            fix,
        ),
        Refusal::ScopeDenied(message) => {
            let whole = |dir: &Path| canonical_lenient(dir).unwrap_or_else(|| dir.to_path_buf());
            let mut fixed = vec![
                whole(ctx.paths.state_dir()),
                whole(ctx.paths.config_dir()),
                whole(ctx.paths.cache_dir()),
                whole(&ctx.paths.omarchy_plugins_dir()),
            ];
            fixed.extend(ctx.clients.iter().map(|client| whole(&client.dir)));
            let mut details = json!({ "tx": reverted.tx });
            if !fixed.iter().any(|root| path.starts_with(root))
                && let Some(object) = details.as_object_mut()
            {
                object.insert("reason".to_owned(), json!("skill_dir"));
            }
            Error::new(
                ErrorCode::ScopeDenied,
                format!("{message}; the undo of {} cannot write it", reverted.tx),
                fix,
            )
            .details(details)
        }
    }
}

/// Check 7: no client whose AddOns or `.omaddons` folder an operation touches is running
/// (M4-D39's rule for a flatten, M4-D38's for a set-aside). An operation on a client's
/// files that names no discovered client counts for every client.
fn check_running(reverted: &HistoryEntry, ctx: &VerifyCtx<'_>) -> Result<()> {
    let clients: Vec<(TargetKey, PathBuf)> = ctx
        .clients
        .iter()
        .map(|client| {
            let dir = canonical_lenient(&client.dir).unwrap_or_else(|| client.dir.clone());
            (client.key, dir)
        })
        .collect();
    let mut keys: BTreeSet<TargetKey> = BTreeSet::new();
    for entry in reverted.repairs.iter().filter(|entry| entry.client_side()) {
        let mut its: BTreeSet<TargetKey> = entry.target.into_iter().collect();
        for (key, dir) in &clients {
            let below = |path: &PathBuf| path.starts_with(dir);
            if below(&entry.path) || entry.to.as_ref().is_some_and(below) {
                its.insert(*key);
            }
        }
        if its.is_empty() {
            its.extend(clients.iter().map(|(key, _)| *key));
        }
        keys.extend(its);
    }
    let Some(key) = keys.into_iter().find(|key| (ctx.running)(*key)) else {
        return Ok(());
    };
    let id = ctx
        .clients
        .iter()
        .find(|client| client.key == key)
        .map_or_else(|| key.to_string(), |client| client.id.clone());
    Err(Error::new(
        ErrorCode::GameRunning,
        format!("{id} is running, and the undo of a repair never moves its files while it runs"),
        Suggestion::text("quit the game client, then run the undo again"),
    )
    .details(json!({ "target": id, "reason": "repair" })))
}

// ---------------------------------------------------------------------------------------
// The virtual map: what the inverses before one leave.

#[derive(Debug, Default)]
struct Virtual {
    events: Vec<Event>,
}

#[derive(Debug)]
enum Event {
    /// What was at or below `from` is at or below `to`.
    Move { from: PathBuf, to: PathBuf },
    /// A write or a chmod left `post` at `path`.
    Set { path: PathBuf, post: Endpoint },
    /// The path went to the undo's quarantine.
    Gone { path: PathBuf },
}

/// Where what the map has at a path comes from.
#[derive(Debug)]
enum Origin {
    /// What is at this path on disk now.
    Disk(PathBuf),
    /// Nothing: an earlier inverse moved it away.
    Gone,
    /// What an earlier write or chmod left.
    Set(Endpoint),
}

impl Virtual {
    fn origin(&self, path: &Path) -> Origin {
        let mut path = path.to_path_buf();
        for event in self.events.iter().rev() {
            match event {
                Event::Move { from, to } => {
                    if let Ok(rest) = path.strip_prefix(to) {
                        path = if rest.as_os_str().is_empty() {
                            from.clone()
                        } else {
                            from.join(rest)
                        };
                    } else if path.starts_with(from) {
                        return Origin::Gone;
                    }
                }
                Event::Set { path: set, post } => {
                    // ubs:ignore — comparing paths, not secrets
                    if path == *set {
                        return Origin::Set(post.clone());
                    }
                    if path.starts_with(set) {
                        return Origin::Gone;
                    }
                }
                Event::Gone { path: gone } => {
                    if path.starts_with(gone) {
                        return Origin::Gone;
                    }
                }
            }
        }
        Origin::Disk(path)
    }

    fn check(&self, path: &Path, expect: &Expect) -> io::Result<Option<Mismatch>> {
        match self.origin(path) {
            Origin::Disk(real) => expect.check(&real),
            Origin::Gone => Ok(expect.check_described(None)),
            Origin::Set(post) => Ok(expect.check_described(Some(&described(&post)))),
        }
    }

    fn identity(&self, path: &Path) -> Option<Identity> {
        match self.origin(path) {
            Origin::Disk(real) => FileId::of(&real).map(|id| Identity {
                ino: id.ino,
                born_ns: id.born_ns,
            }),
            Origin::Set(post) => post.identity(),
            Origin::Gone => None,
        }
    }

    fn stamp(&self, path: &Path) -> Result<Option<Stamp>> {
        let Origin::Disk(real) = self.origin(path) else {
            return Ok(None);
        };
        if !fs::symlink_metadata(&real).is_ok_and(|meta| meta.is_dir()) {
            return Ok(None);
        }
        let digest = StampDigest::compute(&real).map_err(|err| unreadable(&real, &err))?;
        Stamp::from_str(&digest.to_string())
            .map(Some)
            .map_err(|err| Error::internal(format!("a stamp digest that does not read: {err}")))
    }

    fn link_target(&self, path: &Path) -> Option<PathBuf> {
        match self.origin(path) {
            Origin::Disk(real) => fs::read_link(real).ok(),
            Origin::Gone | Origin::Set(_) => None,
        }
    }

    fn apply(&mut self, inverse: &InverseStep) {
        let event = match (inverse.kind, &inverse.post, &inverse.to) {
            (InverseKind::RestoreWrite | InverseKind::ChmodBack, Endpoints::One(post), _) => {
                Event::Set {
                    path: inverse.path.clone(),
                    post: post.clone(),
                }
            }
            (InverseKind::QuarantineCreated, _, _) | (_, _, Dest::Quarantine { .. }) => {
                Event::Gone {
                    path: inverse.path.clone(),
                }
            }
            (_, _, Dest::Path(to)) => Event::Move {
                from: inverse.path.clone(),
                to: to.clone(),
            },
            (_, _, Dest::None) => return,
        };
        self.events.push(event);
    }
}

/// A file an inverse wrote, as a predicate is checked against it: the parts it records.
fn described(post: &Endpoint) -> Described {
    match post {
        Endpoint::File {
            identity,
            sha256,
            mode,
        } => Described {
            identity: identity.map(|id| FileId {
                dev: 0,
                ino: id.ino,
                born_ns: id.born_ns,
            }),
            sha256: sha256.map(|digest| digest.0),
            mode: mode.map(Mode::bits),
            ..Described::kind(Found::Regular)
        },
        Endpoint::Dir { .. } => Described::kind(Found::Directory),
        Endpoint::Symlink { .. } => Described::kind(Found::Symlink),
        Endpoint::Absent => Described::kind(Found::Other),
    }
}

// ---------------------------------------------------------------------------------------
// Refusals.

/// The line cannot be inverted (persisted input, fail closed).
fn corrupt(reverted: &HistoryEntry, why: &str) -> Error {
    Error::new(
        ErrorCode::StateCorrupt,
        format!(
            "history line for {} is not a repair this omaddons can invert: {why}",
            reverted.tx
        ),
        Suggestion::command(["omaddons", "history"]),
    )
    .details(json!({ "tx": reverted.tx }))
}

fn nothing_to_undo(reverted: &HistoryEntry) -> Error {
    Error::new(
        ErrorCode::NotFound,
        format!(
            "transaction {} changed no file; there is nothing to undo",
            reverted.tx
        ),
        Suggestion::command(["omaddons", "history"]),
    )
    .details(json!({ "tx": reverted.tx }))
}

fn already_undone(reverted: &HistoryEntry, by: TxId) -> Error {
    Error::new(
        ErrorCode::UndoConflict,
        format!("transaction {} was already undone by {by}", reverted.tx),
        Suggestion::command(["omaddons".to_owned(), "undo".to_owned(), by.to_string()]),
    )
    .details(json!({
        "tx": reverted.tx,
        "conflicting": [{ "tx": by, "kind": TxKind::Undo, "folders": [] }],
    }))
}

fn changed_since_suspension(
    reverted: &HistoryEntry,
    changed: &BTreeSet<&'static str>,
    target: Option<TargetKey>,
    fm: Option<&str>,
    clients: &[UndoClient],
) -> Error {
    let who = match target {
        Some(key) => clients
            .iter()
            .find(|client| client.key == key)
            .map_or_else(|| format!("target {key}"), |client| client.id.clone()),
        None => "a client".to_owned(),
    };
    let what: Vec<&str> = changed
        .iter()
        .map(|kind| match *kind {
            "generation" => "record",
            "marker" => "marker",
            _ => "folders",
        })
        .collect();
    let mut argv = vec![
        "omaddons".to_owned(),
        "doctor".to_owned(),
        "explain".to_owned(),
    ];
    let fix = match fm {
        Some(fm) => {
            argv.push(fm.to_owned());
            if let Some(key) = target {
                argv.extend(["-t".to_owned(), key.to_string()]);
            }
            Suggestion::command(argv)
        }
        None => Suggestion::command(["omaddons", "history"]),
    };
    let mut details = json!({
        "tx": reverted.tx,
        "reason": "changed_since_suspension",
        "changed_guards": changed,
    });
    if let (Some(key), Some(object)) = (target, details.as_object_mut()) {
        object.insert("target".to_owned(), json!(key));
    }
    Error::new(
        ErrorCode::UndoConflict,
        format!(
            "transaction {} set aside a journal of {who}, and its {} changed since; putting the journal back would replay it over newer work",
            reverted.tx,
            what.join(" and ")
        ),
        fix,
    )
    .details(details)
}

fn changed_since_repair(reverted: &HistoryEntry, paths: &BTreeSet<PathBuf>) -> Error {
    Error::new(
        ErrorCode::UndoConflict,
        format!(
            "{} path(s) changed since the repair {}; undoing it now could overwrite that",
            paths.len(),
            reverted.tx
        ),
        Suggestion::command(["omaddons", "history"]),
    )
    .details(json!({
        "tx": reverted.tx,
        "reason": "changed_since_repair",
        "paths": path_texts(paths),
    }))
}

fn unreadable(path: &Path, err: &io::Error) -> Error {
    Error::io(
        path,
        err,
        Suggestion::text(format!(
            "check {} and its permissions; nothing was moved",
            path.display()
        )),
    )
}

/// Sorted absolute paths, as `details.paths` lists them.
fn path_texts(paths: &BTreeSet<PathBuf>) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

// ---------------------------------------------------------------------------------------
// The undo transaction (E7's second part).

/// What undoing a repair-family line did, beside the folder undo's facts (`UndoOutcome`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairUndone {
    /// The plan as verified, under the lock for a real run.
    pub plan: VerifiedPlan,
    /// The undo's own doctor run; `None` for a dry run, which makes none.
    pub undo_run: Option<String>,
    /// The inverses as the undo's history line records them; empty for a dry run.
    pub committed: Vec<RepairEntry>,
    /// A dry run's: each inverse primitive lowered as the run would lower it, its own run
    /// and transaction written `<run>` and `<tx>` (`doctor undo --dry-run`'s
    /// `actions_planned`); empty for a real run.
    pub planned: Vec<crate::doctor::report::ActionPlanned>,
}

/// Undoes the repair-family line `read.entries[at]` (M4-D46): planned and verified without
/// the lock (a dry run stops there, having written nothing), then its own run directory
/// of kind `undo`, the lock, recovery, the plan and its checks again on the history as it
/// is now, and the inverses committed as one repair-family transaction of kind `undo`
/// through E5's machinery, as `Writer::Doctor` restricted to the paths the reverted line
/// names (§6.14.4). The run's `report.json` names the run reverted.
pub fn undo_repair(
    req: &UndoRequest,
    ctx: &EngineCtx<'_>,
    read: &HistoryRead,
    at: usize,
) -> Result<UndoOutcome> {
    undo_repair_reported(req, ctx, read, at).0
}

/// What the undo of a repair recorded, for `doctor undo`'s report (§6.11).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Recorded {
    /// It ended before its run directory was made: a dry run, or a refusal.
    #[default]
    Nothing,
    /// Its run directory could not be made (the doctor's exit 73).
    CannotCreate,
    /// The report its run recorded, whatever the outcome.
    Report(Box<UndoReport>),
}

/// [`undo_repair`], and what its run recorded: `doctor undo` prints that report.
pub fn undo_repair_reported(
    req: &UndoRequest,
    ctx: &EngineCtx<'_>,
    read: &HistoryRead,
    at: usize,
) -> (Result<UndoOutcome>, Recorded) {
    let mut recorded = Recorded::Nothing;
    let result = undo_repair_recording(req, ctx, read, at, &mut recorded);
    (result, recorded)
}

/// The id of the undo that already undid the line an undo was refused for (s1's "already
/// undone" refusal), which `doctor undo` turns into a no-op; `None` for any other error.
pub fn already_undone_by(error: &Error) -> Option<TxId> {
    if error.code != ErrorCode::UndoConflict || error.details.get("reason").is_some() {
        return None;
    }
    match error
        .details
        .get("conflicting")
        .and_then(Value::as_array)?
        .as_slice()
    {
        [one] if one.get("kind") == Some(&json!(TxKind::Undo)) => {
            one.get("tx")?.as_str()?.parse().ok()
        }
        _ => None,
    }
}

fn undo_repair_recording(
    req: &UndoRequest,
    ctx: &EngineCtx<'_>,
    read: &HistoryRead,
    at: usize,
    recorded: &mut Recorded,
) -> Result<UndoOutcome> {
    let paths = ctx.paths;
    let reverted = read
        .entries
        .get(at)
        .ok_or_else(|| Error::internal(format!("history line {at} does not exist")))?;
    let reverts = reverted.tx;
    let plan = plan_undo(&read.entries, at, paths)?;
    let discovered = discovered_targets(req, reverted, &plan)?;
    let clients: Vec<UndoClient> = discovered
        .iter()
        .filter_map(|d| {
            Some(UndoClient {
                key: d.target.key?,
                id: d.target.id.clone(),
                dir: d.target.client_dir.clone(),
            })
        })
        .collect();
    let client_dirs: Vec<PathBuf> = clients.iter().map(|c| c.dir.clone()).collect();
    let targets: Vec<Target> = discovered.iter().map(|d| d.target.clone()).collect();
    let scopes = fence::table(paths, &client_dirs, None)?;
    let ids = clients
        .iter()
        .map(|c| c.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    // 1. Without the lock.
    let running = running_now(ctx, &targets);
    let verified = verify_now(plan, read, at, &clients, &scopes, &running, paths)?;
    let mut warnings: Vec<Warning> = running.warning().into_iter().collect();
    let preview = outcome_of(
        reverts,
        &ids,
        verified.clone(),
        None,
        Vec::new(),
        true,
        &warnings,
    );
    if req.dry_run {
        // Lowered as the run would lower it, against the names a dry run never makes
        // (`doctor diff`'s): what `doctor undo --dry-run` plans. It creates nothing.
        let mut preview = preview;
        let run = RepairRun {
            dir: paths.doctor_runs_dir().join("<run>"),
            id: "<run>".to_owned(),
        };
        let lower_clients: Vec<(PathBuf, TargetKey)> =
            clients.iter().map(|c| (c.dir.clone(), c.key)).collect();
        let skill_dirs = [paths.skill_dir()];
        let roots = LowerCtx {
            paths,
            tx: TxId::new(),
            run: &run,
            clients: &lower_clients,
            skill_dirs: &skill_dirs,
            scopes: &scopes,
            mount_id: &fsx::mount_id,
        };
        let lowered =
            lowered_from_inverses(&roots, &verified).map_err(|err| lowering_error(reverts, err))?;
        if let Some(repair) = preview.repair.as_mut() {
            repair.planned = crate::doctor::fix::planned_actions(&roots, &lowered);
        }
        return Ok(preview);
    }
    // 2. A bound run plans what its preview showed, or nothing.
    if let Some(binding) = ctx.binding {
        binding.undo(&preview)?;
    }
    // 3. Before anything is written.
    for target in &targets {
        super::super::tx::refuse_newer_state(paths, target)?;
    }
    let command = command_name(ctx.command);
    let rtx = RepairTx::begin(paths)?;
    let run_scopes = ScopeTable::new(paths, &client_dirs, None)?;
    let mut runtime = match Runtime::start(paths, &run_scopes, crate::clock::now()) {
        Ok(runtime) => runtime,
        Err(failure) => {
            warnings.extend(abandon(rtx, &scopes));
            if failure.exit == crate::doctor::exit::Exit::CannotCreate {
                *recorded = Recorded::CannotCreate;
            }
            return Err(failure.error);
        }
    };
    let started_at = crate::clock::now();
    let run = RepairRun {
        dir: runtime.dir(),
        id: runtime.id().as_str().to_owned(),
    };
    let ended = committed(
        ctx,
        req,
        &Committing {
            reverts,
            clients: &clients,
            targets: &targets,
            scopes: &scopes,
            run: &run,
            command,
        },
        rtx,
        &mut warnings,
    );
    // 6. After the lock: the run's record, then cleanup.
    let (state, error, tx, done) = match ended {
        Ok(done) => (done.state, done.error.clone(), Some(done.tx), Some(done)),
        Err(stop) => (stop.state, Some(stop.error), stop.tx, None),
    };
    let actions_taken = done.as_ref().map_or(0, |done| done.primitives);
    // A committed undo's `undo.sh`; one that cannot be written fails the run after its
    // record, as `doctor fix`'s does: the undo stands, and history records it.
    let mut error = error;
    if let Some(done) = &done
        && state == RunState::DoneOk
        && let Err(err) = write_undo_script(&mut runtime, paths, done.tx)
    {
        error = Some(err);
    }
    let report = UndoReport {
        doctor_contract_version: crate::doctor::report::DOCTOR_CONTRACT_VERSION,
        run_id: Some(runtime.id().clone()),
        run_dir: Some(run.dir.display().to_string()),
        started_at,
        finished_at: crate::clock::now(),
        reverted_run: reverted.doctor_run_id.clone(),
        tx: Some(reverts),
        undo_tx: tx,
        state,
        actions_taken,
        already_undone: false,
        actions_planned: None,
    };
    *recorded = Recorded::Report(Box::new(report.clone()));
    let record = record_of(command, &report, error.as_ref(), &warnings)?;
    let mut harden = Vec::new();
    if run.dir.join(ACTIONS_LOG).is_file() {
        harden.push((ACTIONS_LOG, 0o400));
    }
    if run.dir.join(UNDO_SCRIPT).is_file() {
        harden.push((UNDO_SCRIPT, 0o500));
    }
    let finished = runtime.finish_with(&record, state.moves_latest(), &harden);
    let Some(done) = done else {
        // The record of a refusal or a rollback stands beside its error.
        return Err(error.unwrap_or_else(|| Error::internal("an undo ended without its error")));
    };
    warnings.extend(done.cleanup.finish());
    if let Err(failure) = finished {
        return Err(failure.error);
    }
    if let Some(error) = error {
        return Err(error);
    }
    let mut outcome = outcome_of(
        reverts,
        &ids,
        done.verified,
        Some(done.tx),
        done.entries,
        false,
        &warnings,
    );
    if let Some(repair) = &mut outcome.repair {
        repair.undo_run = Some(run.id);
    }
    Ok(outcome)
}

/// What the commit window needs beside the request.
struct Committing<'c> {
    reverts: TxId,
    clients: &'c [UndoClient],
    targets: &'c [Target],
    scopes: &'c ScopeTable,
    run: &'c RepairRun,
    command: &'static str,
}

/// An undo that reached its commit: how it ended there.
struct Done {
    state: RunState,
    /// A rollback's cause, returned after the record.
    error: Option<Error>,
    tx: TxId,
    primitives: u32,
    verified: VerifiedPlan,
    entries: Vec<RepairEntry>,
    cleanup: RepairCleanup,
}

/// An undo that stopped before its commit, with its run directory made.
struct Stopped {
    state: RunState,
    error: Error,
    /// The undo's transaction, when its journal was started and kept for recovery.
    tx: Option<TxId>,
}

/// Steps 4 and 5 under the lock: recover, plan and check again on the history as it is
/// now, lower the inverses and commit them.
fn committed(
    ctx: &EngineCtx<'_>,
    req: &UndoRequest,
    c: &Committing<'_>,
    rtx: RepairTx,
    warnings: &mut Vec<Warning>,
) -> std::result::Result<Done, Stopped> {
    let paths = ctx.paths;
    let refuse = |error: Error| Stopped {
        state: RunState::Refusing,
        error,
        tx: None,
    };
    let stop = |rtx: RepairTx, warnings: &mut Vec<Warning>, stopped: Stopped| {
        warnings.extend(abandon(rtx, c.scopes));
        stopped
    };
    if let Err(err) = failpoint::hit("engine.snapshot.prestaged") {
        return Err(stop(rtx, warnings, refuse(failpoint_error(paths, &err))));
    }
    let lock = match GlobalLock::acquire(paths, ctx.command) {
        Ok(lock) => lock,
        Err(error) => {
            let state = if error.code == ErrorCode::Locked {
                RunState::LockLost
            } else {
                RunState::Refusing
            };
            return Err(stop(
                rtx,
                warnings,
                Stopped {
                    state,
                    error,
                    tx: None,
                },
            ));
        }
    };
    let checked = (|| -> Result<(VerifiedPlan, Lowered, Vec<Warning>)> {
        failpoint::hit("engine.lock.acquired").map_err(|err| failpoint_error(paths, &err))?;
        let sv = |dir: &Path| super::super::tx::sv_writable(ctx, dir);
        let own: Vec<TargetKey> = c.clients.iter().map(|client| client.key).collect();
        let recovery = RecoveryEnv {
            lock: &lock,
            paths,
            scopes: c.scopes,
            own: &own,
            sv_writable: &sv,
        };
        // A plan made before recovery may name the journal the undo is about: the one an
        // undo of a reactivation sets aside again, which recovery cannot settle.
        let early = plan_undo_of(paths, c.reverts)?;
        let mut found = match recover_all(&recovery) {
            Ok(found) => found,
            Err(error) if subject_of(&error, &early, paths) => {
                // Recovery stops at the first journal it cannot settle, so any journal
                // after the subject was never tried: one still there refuses the undo.
                if let Some(other) = unsettled_besides(paths, &error)? {
                    return Err(other);
                }
                vec![
                    Warning::new(WarningCode::JournalPending, error.message.clone())
                        .with_fix(error.fix.clone()),
                ]
            }
            Err(error) => return Err(error),
        };
        failpoint::hit("engine.recovered").map_err(|err| failpoint_error(paths, &err))?;
        for target in c.targets {
            super::super::tx::refuse_newer_state(paths, target)?;
        }
        // Again on the history as it is now: recovery may have appended lines.
        let read = history::read(paths, &HistoryFilter::default())?;
        let at = read
            .entries
            .iter()
            .position(|entry| entry.tx == c.reverts)
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::NotFound,
                    format!("there is no transaction {} in the history", c.reverts),
                    Suggestion::command(["omaddons", "history"]),
                )
            })?;
        let plan = plan_undo(&read.entries, at, paths)?;
        let writes = undo_writes(&plan);
        super::super::fence::under_lock(c.scopes, paths, None, &writes)?;
        let running = running_now(ctx, c.targets);
        let verified = verify_now(plan, &read, at, c.clients, c.scopes, &running, paths)?;
        if let Some(binding) = ctx.binding {
            let ids = c
                .clients
                .iter()
                .map(|client| client.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            binding.undo(&outcome_of(
                c.reverts,
                &ids,
                verified.clone(),
                None,
                Vec::new(),
                true,
                &[],
            ))?;
        }
        failpoint::hit("engine.revalidated").map_err(|err| failpoint_error(paths, &err))?;
        let clients: Vec<(PathBuf, TargetKey)> = c
            .clients
            .iter()
            .map(|client| (client.dir.clone(), client.key))
            .collect();
        let skill_dirs = [paths.skill_dir()];
        let lowered = lowered_from_inverses(
            &LowerCtx {
                paths,
                tx: rtx.tx(),
                run: c.run,
                clients: &clients,
                skill_dirs: &skill_dirs,
                scopes: c.scopes,
                mount_id: &fsx::mount_id,
            },
            &verified,
        )
        .map_err(|err| lowering_error(c.reverts, err))?;
        found.extend(running.warning());
        Ok((verified, lowered, found))
    })();
    let (verified, lowered, found) = match checked {
        Ok(checked) => checked,
        Err(error) => {
            drop(lock);
            return Err(stop(rtx, warnings, refuse(error)));
        }
    };
    warnings.extend(found);
    let tx = rtx.tx();
    let undo = UndoOf {
        reverts: c.reverts,
        paths: repair_paths(paths, &verified.plan, c, &tx),
        plan: ctx.applying,
    };
    let meta = RepairMeta {
        run: req.run,
        actor: ctx.actor,
        reason: req.reason.clone(),
        argv: ctx.argv.to_vec(),
        command: c.command,
        undo: Some(undo),
    };
    let commit_ctx = CommitCtx {
        lock: &lock,
        scopes: c.scopes,
        run: c.run,
        mount_id: &fsx::mount_id,
    };
    let result = commit(&commit_ctx, rtx, &lowered, &meta);
    drop(lock);
    match result {
        Ok((RepairOutcome::Committed { primitives, .. }, cleanup)) => {
            let entries = history::read(paths, &HistoryFilter::default())
                .ok()
                .and_then(|read| read.entries.into_iter().find(|entry| entry.tx == tx))
                .map(|entry| entry.repairs)
                .unwrap_or_default();
            Ok(Done {
                state: RunState::DoneOk,
                error: None,
                tx,
                primitives,
                verified,
                entries,
                cleanup,
            })
        }
        Ok((RepairOutcome::RolledBack { cause, .. }, cleanup)) => Ok(Done {
            state: RunState::DoneFailed,
            error: Some(cause),
            tx,
            primitives: 0,
            verified,
            entries: Vec::new(),
            cleanup,
        }),
        // Nothing was written (a revalidation), or a rollback or roll forward could not
        // finish: the journal is kept for the next lock-taking command.
        Err(error) => {
            let kept = error.details.get("journal_pending").is_some();
            Err(Stopped {
                state: if kept {
                    RunState::DoneFailed
                } else {
                    RunState::Refusing
                },
                error,
                tx: kept.then_some(tx),
            })
        }
    }
}

/// [`plan_undo`] of `reverts` on the history as it is: the subject a recovery stop may be
/// about.
fn plan_undo_of(paths: &Paths, reverts: TxId) -> Result<RepairUndoPlan> {
    let read = history::read(paths, &HistoryFilter::default())?;
    let at = read
        .entries
        .iter()
        .position(|entry| entry.tx == reverts)
        .ok_or_else(|| Error::internal(format!("transaction {reverts} left the history")))?;
    plan_undo(&read.entries, at, paths)
}

/// Whether a recovery stop is about a journal the undo's own inverses move (the stuck
/// journal an undo of a reactivation sets aside again): that one is its subject, and E14
/// lets a set-aside run while it blocks every other change.
fn subject_of(error: &Error, plan: &RepairUndoPlan, paths: &Paths) -> bool {
    let Some(pending) = error
        .details
        .get("journal_pending")
        .and_then(Value::as_str)
        .and_then(|tx| tx.parse::<TxId>().ok())
    else {
        return false;
    };
    plan.inverses
        .iter()
        .flat_map(|inverse| std::iter::once(inverse.path.as_path()).chain(inverse.to.path()))
        // ubs:ignore — comparing transaction ids, not secrets
        .any(|path| journal_file(paths, path).is_some_and(|(tx, _)| tx == pending))
}

/// A journal besides the subject of the recovery stop `stop` that is still in
/// `<state>/journal/`, as the refusal it is: recovery stopped at the subject without
/// settling it, so its state is unknown, and the undo writes nothing (fail closed). Every
/// later change stops at the subject too; `doctor`, which never recovers first, is the
/// way out.
fn unsettled_besides(paths: &Paths, stop: &Error) -> Result<Option<Error>> {
    let subject = stop
        .details
        .get("journal_pending")
        .and_then(Value::as_str)
        .and_then(|tx| tx.parse::<TxId>().ok());
    let dir = paths.journal_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(unreadable(&dir, &err)),
    };
    let mut others: BTreeSet<TxId> = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|err| unreadable(&dir, &err))?;
        // ubs:ignore — comparing transaction ids, not secrets
        if let Some((tx, _)) = journal_file(paths, &entry.path())
            && Some(tx) != subject
        {
            others.insert(tx);
        }
    }
    Ok(others.first().map(|other| {
        Error::new(
            ErrorCode::StateCorrupt,
            format!(
                "transaction {other} did not finish, and recovery stopped at {}, which this undo would set aside, without settling it; nothing was moved",
                subject.map_or_else(|| "a journal".to_owned(), |tx| tx.to_string())
            ),
            Suggestion::command(["omaddons", "doctor"]),
        )
        .details(json!({ "journal_pending": other }))
    }))
}

/// Every path the plan's inverses write, for the fences' gate under the lock.
fn undo_writes(plan: &RepairUndoPlan) -> Vec<PathBuf> {
    plan.resources
        .writes()
        .iter()
        .filter_map(|resource| resource.path().map(Path::to_path_buf))
        .collect()
}

/// [`verify`] with the running state `running` read for these clients.
fn verify_now(
    plan: RepairUndoPlan,
    read: &HistoryRead,
    at: usize,
    clients: &[UndoClient],
    scopes: &ScopeTable,
    running: &RunningNow,
    paths: &Paths,
) -> Result<VerifiedPlan> {
    let running = |key: TargetKey| {
        clients
            .iter()
            .find(|client| client.key == key)
            .is_none_or(|client| running.contains(&client.dir))
    };
    verify(
        plan,
        &VerifyCtx {
            paths,
            entries: &read.entries,
            unreadable: &read.unreadable,
            at,
            clients,
            scopes,
            running: &running,
            mount_id: &fsx::mount_id,
        },
    )
}

/// The discovered targets the reverted line touched: every key of its `targets` and of
/// its operations, each on this machine now (`target_not_found` otherwise, before
/// anything is written).
fn discovered_targets(
    req: &UndoRequest,
    reverted: &HistoryEntry,
    plan: &RepairUndoPlan,
) -> Result<Vec<DiscoveredTarget>> {
    plan.targets
        .iter()
        .map(|key| {
            req.targets
                .iter()
                .find(|d| d.target.key == Some(*key))
                .cloned()
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::TargetNotFound,
                        format!(
                            "transaction {} changed target {key}, which is not on this machine now",
                            reverted.tx
                        ),
                        Suggestion::command(["omaddons", "targets"]),
                    )
                })
        })
        .collect()
}

/// What the undo's primitive session may write (§6.14.4): every path and destination the
/// reverted line names, the provisional names its activations use, and, as destinations,
/// below this undo's own quarantine roots. All canonical.
fn repair_paths(
    paths: &Paths,
    plan: &RepairUndoPlan,
    c: &Committing<'_>,
    tx: &TxId,
) -> RepairPaths {
    let whole = |path: &Path| canonical_lenient(path).unwrap_or_else(|| path.to_path_buf());
    let mut named: BTreeSet<PathBuf> = BTreeSet::new();
    for inverse in &plan.inverses {
        named.insert(whole(&inverse.path));
        if let Some(to) = inverse.to.path() {
            named.insert(whole(to));
        }
    }
    let mut roots = vec![
        whole(&c.run.dir.join("quarantine")),
        whole(&paths.cache_quarantine(&c.run.id)),
    ];
    roots.extend(c.clients.iter().map(|client| {
        whole(
            &crate::state::paths::ClientPaths::new(&client.dir)
                .snapshots_repair(tx)
                .join("quarantine"),
        )
    }));
    RepairPaths {
        paths: named,
        destination_roots: roots,
    }
}

/// The doorbell's and the record's command: the caller's, among the three that undo a
/// repair.
fn command_name(command: &str) -> &'static str {
    match command {
        "doctor undo" => "doctor undo",
        "apply" => "apply",
        _ => "undo",
    }
}

fn outcome_of(
    reverts: TxId,
    ids: &str,
    verified: VerifiedPlan,
    tx: Option<TxId>,
    committed: Vec<RepairEntry>,
    dry_run: bool,
    warnings: &[Warning],
) -> UndoOutcome {
    UndoOutcome {
        undoes: reverts,
        outcome: TxOutcome {
            target: ids.to_owned(),
            key: None,
            tx,
            kind: tx.map(|_| TxKind::Undo),
            dry_run,
            changes: Vec::new(),
            atomic_swap: true,
            reload_required: crate::model::ReloadRequired::None,
            warnings: warnings.to_vec(),
            creates_addons_dir: false,
            lock_held_ms: None,
            notes: Vec::new(),
        },
        sv: SvCounts::default(),
        plan: UndoFacts::default(),
        wtf: None,
        repair: Some(RepairUndone {
            plan: verified,
            undo_run: None,
            committed,
            planned: Vec::new(),
        }),
    }
}

/// Why lowering the undo of `reverts` stopped: its own error, or a refusal, which is a
/// path that changed since the repair.
fn lowering_error(reverts: TxId, err: LowerError) -> Error {
    match err {
        LowerError::Error(error) => error,
        LowerError::Refused { message, .. } => Error::new(
            ErrorCode::UndoConflict,
            message,
            Suggestion::command(["omaddons", "history"]),
        )
        .details(json!({ "tx": reverts, "reason": "changed_since_repair" })),
    }
}

/// The run's `report.json`: the doctor envelope's record form with the undo's report.
fn record_of(
    command: &'static str,
    report: &UndoReport,
    error: Option<&Error>,
    warnings: &[Warning],
) -> Result<Vec<u8>> {
    let meta = Meta {
        elapsed_ms: 0,
        offline: false,
    };
    let envelope = DoctorEnvelope::report(
        command,
        report,
        report.state.exit(),
        error,
        warnings,
        &[],
        meta,
    )
    .record(RunKind::Undo);
    serde_json::to_vec(&envelope)
        .map_err(|e| Error::internal(format!("the undo's record could not be serialized: {e}")))
}

/// `undo.sh`: it undoes this undo, by `doctor undo <this run>`.
fn write_undo_script(runtime: &mut Runtime<'_>, paths: &Paths, tx: TxId) -> Result<()> {
    let binary = crate::omarchy::install::current_binary()?;
    let context = crate::omarchy::render::Context::of(paths);
    let script = undo_script(&binary, &context, runtime.id(), tx);
    runtime
        .write_file(UNDO_SCRIPT, &script, 0o600)
        .map(|_| ())
        .map_err(|failure| failure.error)
}

/// A failpoint's error, before anything of the undo was written.
fn failpoint_error(paths: &Paths, err: &io::Error) -> Error {
    Error::io(
        paths.state_dir(),
        err,
        Suggestion::command(["omaddons", "status"]),
    )
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;
    use crate::engine::repair::{
        CommitCtx, LowerCtx, PlannedFix, RepairMeta, RepairOutcome, RepairRun, RepairTx, commit,
        lower,
    };
    use crate::engine::repair_action::{
        Attribution, Fix, JournalFamily, JournalSet, QuarantineRoot, SuspendedArtifact,
    };
    use crate::engine::testing::{RepairHome, repair_home, repair_prepared, tree_hash};
    use crate::fsx::{self, GlobalLock};
    use crate::model::{Actor, RunId};
    use crate::state::history::{self, FolderOp, HistoryFilter, HistoryOp, HistoryRead, TxResult};
    use crate::state::records::TargetRecord;
    use crate::state::store;

    const SKILL: &str = "fm-plugins-skill-version-drift";
    const FILE_MODE: &str = "fm-secrets-credential-file-mode";
    const NESTED: &str = "fm-external_artifacts-nested-addon-folder";
    const CORRUPT: &str = "fm-schemas-state-corrupt";
    const STUCK: &str = "fm-concurrency_primitives-journal-step-unrecognized";
    const NEW_GUIDE: &[u8] = b"---\nname: omaddons\n---\nnew guide\n";
    /// A second doctor run, for a later repair on the same home.
    const LATER_RUN: &str = "2026-10-31T08-00-00Z__01M678YN3A22K2ASPW2VZH88B0";

    // -----------------------------------------------------------------------------------
    // A home, and real repairs committed on it.

    /// [`repair_home`] with a target record that reads, at generation 1.
    fn home() -> RepairHome {
        let home = repair_home();
        record_at(&home, 1);
        home
    }

    fn record_at(home: &RepairHome, generation: u64) {
        let mut record = TargetRecord::empty(home.key);
        record.generation = generation;
        fs::write(home.record(), store::encode(&record).unwrap()).unwrap();
    }

    fn mode(bits: u32) -> Mode {
        Mode::new(bits).unwrap()
    }

    fn planned(fm: &'static str, fix: Fix) -> PlannedFix {
        PlannedFix {
            fm: Cow::Borrowed(fm),
            fix,
        }
    }

    fn write_over(home: &RepairHome) -> PlannedFix {
        planned(
            SKILL,
            Fix::Write {
                path: home.skill_file(),
                content: NEW_GUIDE.to_vec(),
                mode: mode(0o644),
            },
        )
    }

    fn notes(home: &RepairHome) -> PathBuf {
        home.paths.skill_dir().join("NOTES.md")
    }

    fn write_create(home: &RepairHome) -> PlannedFix {
        planned(
            SKILL,
            Fix::Write {
                path: notes(home),
                content: b"notes\n".to_vec(),
                mode: mode(0o644),
            },
        )
    }

    fn chmod(home: &RepairHome) -> PlannedFix {
        planned(
            FILE_MODE,
            Fix::Chmod {
                path: home.credential(),
                mode: mode(0o600),
            },
        )
    }

    fn quarantine(path: PathBuf, root: QuarantineRoot) -> PlannedFix {
        planned(CORRUPT, Fix::Quarantine { path, root })
    }

    fn client_root(home: &RepairHome) -> QuarantineRoot {
        QuarantineRoot::Client {
            client_dir: home.client.client_dir().to_path_buf(),
        }
    }

    fn flatten(home: &RepairHome, inner: &str, outer: &str) -> PlannedFix {
        planned(
            NESTED,
            Fix::Flatten {
                inner: home.addons(inner),
                outer: home.addons(outer),
            },
        )
    }

    /// The set-aside of journal `tx`'s `artifacts`, each into its own side's quarantine,
    /// with `guards` as its manifest.
    fn set_aside(
        home: &RepairHome,
        tx: TxId,
        artifacts: &[PathBuf],
        guards: Vec<Guard>,
    ) -> PlannedFix {
        let root = |path: &PathBuf| {
            if path.starts_with(home.client.client_dir()) {
                client_root(home)
            } else {
                QuarantineRoot::State
            }
        };
        planned(
            STUCK,
            Fix::Suspend {
                journal: JournalSet {
                    tx,
                    family: JournalFamily::Folder,
                    target: Some(home.key),
                    artifacts: artifacts
                        .iter()
                        .map(|path| SuspendedArtifact {
                            path: path.clone(),
                            root: root(path),
                        })
                        .collect(),
                    guards,
                    attribution: Attribution::Complete,
                    unattributed: Vec::new(),
                },
            },
        )
    }

    /// A set-aside of a journal with no client-side artifact names no client in its line,
    /// but its guards watch one: the undo counts that client among its own, so the guards
    /// are read there (before, its marker read as absent and refused the undo).
    #[test]
    fn a_set_aside_without_a_client_artifact_reads_its_guards_on_their_client() {
        let home = home();
        let stuck = home.stuck();
        let dir = home.client.client_dir();
        let guards = vec![
            guard::observe_generation(&home.paths, home.key).unwrap(),
            guard::observe_marker(dir, home.key).unwrap(),
        ];
        let tx = repaired(
            &home,
            &[set_aside(
                &home,
                stuck,
                &[home.paths.journal(&stuck)],
                guards,
            )],
        );
        assert_eq!(line_of(&home, tx).targets, [], "the line names no client");
        assert_eq!(plan_of(&home, tx).targets, [home.key], "the guards' client");
        let outcome = engine_undo(&home, tx, true, vec![discovered(&home)]);
        assert!(outcome.is_ok(), "the guards hold: {outcome:?}");
    }

    /// The set-aside of `home`'s stuck journal and its client stage.
    fn stuck_set_aside(home: &RepairHome, guards: Vec<Guard>) -> PlannedFix {
        let stuck = home.stuck();
        set_aside(
            home,
            stuck,
            &[home.paths.journal(&stuck), home.client.stage(&stuck)],
            guards,
        )
    }

    /// Every guard kind of `home`'s planned client, its `Stray` folder's epoch included.
    fn guards(home: &RepairHome) -> Vec<Guard> {
        let dir = home.client.client_dir();
        vec![
            guard::observe_generation(&home.paths, home.key).unwrap(),
            guard::observe_marker(dir, home.key).unwrap(),
            guard::observe_folder_epoch(dir, &FolderName::new("Stray").unwrap()).unwrap(),
            guard::observe_wtf_epoch(dir, home.key).unwrap(),
        ]
    }

    fn meta() -> RepairMeta {
        RepairMeta {
            run: RunId::new(),
            actor: Actor::Human,
            reason: Some("a test repair".to_owned()),
            argv: ["omaddons", "doctor", "fix"].map(str::to_owned).to_vec(),
            command: "doctor fix",
            undo: None,
        }
    }

    /// A second published doctor run on `home`.
    fn later_run(home: &RepairHome) -> RepairRun {
        let dir = home.paths.doctor_runs_dir().join(LATER_RUN);
        fs::create_dir_all(&dir).unwrap();
        RepairRun {
            dir,
            id: LATER_RUN.to_owned(),
        }
    }

    /// `fixes` lowered and committed on `home` in `run`, as one repair: its transaction.
    fn repaired_in(home: &RepairHome, run: &RepairRun, fixes: &[PlannedFix]) -> TxId {
        let rtx = RepairTx::begin(&home.paths).unwrap();
        let tx = rtx.tx();
        let lowered = lower(
            &LowerCtx {
                paths: &home.paths,
                tx,
                run,
                clients: &home.planned,
                skill_dirs: &home.skill_dirs,
                scopes: &home.scopes,
                mount_id: &fsx::mount_id,
            },
            fixes,
        )
        .unwrap_or_else(|err| panic!("lowering {fixes:?}: {err}")); // ubs:ignore — fails the test
        let lock = GlobalLock::acquire(&home.paths, "test").unwrap();
        let ctx = CommitCtx {
            lock: &lock,
            scopes: &home.scopes,
            run,
            mount_id: &fsx::mount_id,
        };
        let (outcome, cleanup) = commit(&ctx, rtx, &lowered, &meta()).unwrap();
        assert!(
            matches!(outcome, RepairOutcome::Committed { .. }),
            "{outcome:?}"
        );
        drop(lock);
        cleanup.finish();
        tx
    }

    fn repaired(home: &RepairHome, fixes: &[PlannedFix]) -> TxId {
        repaired_in(home, &home.run, fixes)
    }

    fn history_of(home: &RepairHome) -> HistoryRead {
        history::read(&home.paths, &HistoryFilter::default()).unwrap()
    }

    fn at_of(read: &HistoryRead, tx: TxId) -> usize {
        read.entries.iter().position(|e| e.tx == tx).unwrap()
    }

    fn line_of(home: &RepairHome, tx: TxId) -> HistoryEntry {
        let read = history_of(home);
        read.entries[at_of(&read, tx)].clone()
    }

    fn plan_of(home: &RepairHome, tx: TxId) -> RepairUndoPlan {
        let read = history_of(home);
        plan_undo(&read.entries, at_of(&read, tx), &home.paths)
            .unwrap_or_else(|err| panic!("planning the undo of {tx}: {err:?}")) // ubs:ignore — fails the test
    }

    /// Appends `line` to `home`'s history as a later transaction.
    fn plant(home: &RepairHome, line: &HistoryEntry) {
        let mut bytes = serde_json::to_vec(line).unwrap();
        bytes.push(b'\n');
        crate::fsx::sys::append_durable(&home.paths.history_file(), &bytes).unwrap();
    }

    /// Appends a line no build reads.
    fn plant_raw(home: &RepairHome, raw: &str) {
        crate::fsx::sys::append_durable(&home.paths.history_file(), format!("{raw}\n").as_bytes())
            .unwrap();
    }

    fn clients(home: &RepairHome) -> Vec<UndoClient> {
        vec![UndoClient {
            key: home.key,
            id: "retail".to_owned(),
            dir: home.client.client_dir().to_path_buf(),
        }]
    }

    /// What [`verify`] runs with besides the home.
    struct Seams<'a> {
        running: bool,
        mount_id: &'a dyn Fn(&Path) -> io::Result<u64>,
        scopes: Option<ScopeTable>,
    }

    impl Default for Seams<'_> {
        fn default() -> Self {
            Self {
                running: false,
                mount_id: &fsx::mount_id,
                scopes: None,
            }
        }
    }

    /// The undo of `tx` planned and verified on `home` as it is now: nothing below the
    /// home may change, whatever the outcome.
    fn check_with(home: &RepairHome, tx: TxId, seams: &Seams<'_>) -> Result<VerifiedPlan> {
        let read = history_of(home);
        let at = at_of(&read, tx);
        let clients = clients(home);
        let running = |_: TargetKey| seams.running;
        let ctx = VerifyCtx {
            paths: &home.paths,
            entries: &read.entries,
            unreadable: &read.unreadable,
            at,
            clients: &clients,
            scopes: seams.scopes.as_ref().unwrap_or(&home.scopes),
            running: &running,
            mount_id: seams.mount_id,
        };
        let before = tree_hash(&home.base);
        let result = plan_undo(&read.entries, at, &home.paths).and_then(|plan| verify(plan, &ctx));
        assert_eq!(tree_hash(&home.base), before, "verification wrote nothing");
        if let Err(err) = &result {
            assert_eq!(
                err.code.check_details(&err.details),
                Ok(()),
                "{}: {}",
                err.message,
                err.details
            );
        }
        result
    }

    fn check(home: &RepairHome, tx: TxId) -> Result<VerifiedPlan> {
        check_with(home, tx, &Seams::default())
    }

    fn refused(result: Result<VerifiedPlan>, case: &str) -> Error {
        match result {
            Ok(verified) => panic!("{case}: the undo passed: {:?}", verified.plan), // ubs:ignore — fails the test
            Err(err) => err,
        }
    }

    /// `<client>/Interface/.omaddons/snapshots/<tx>.repair/quarantine/<rel>`.
    fn parked(home: &RepairHome, tx: TxId, rel: &str) -> PathBuf {
        home.client
            .snapshots_repair(&tx)
            .join("quarantine")
            .join(rel)
    }

    /// `<run>/quarantine/state/<rel>` of `home`'s doctor run.
    fn state_quarantine(home: &RepairHome, rel: &str) -> PathBuf {
        home.run.dir.join("quarantine/state").join(rel)
    }

    /// A folder transaction on `home`'s client with one op per `(op, folder)`.
    fn folder_line(home: &RepairHome, kind: TxKind, ops: &[(FolderOp, &str)]) -> HistoryEntry {
        HistoryEntry {
            schema_version: history::CURRENT,
            tx: TxId::new(),
            kind,
            at: jiff::Timestamp::now(),
            target: Some(home.key),
            targets: Vec::new(),
            actor: Actor::Human,
            reason: None,
            argv: vec!["omaddons".into(), "install".into()],
            ops: ops
                .iter()
                .map(|(op, folder)| HistoryOp {
                    op: *op,
                    folder: FolderName::new(folder).unwrap(),
                    addon: (*folder).to_owned(),
                    edit: None,
                    backup: None,
                    from: None,
                    files: None,
                })
                .collect(),
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: None,
            backup: None,
            plan: None,
            doctor_run_id: None,
            repairs: Vec::new(),
        }
    }

    /// A synthetic repair-family line like `base`, of `kind`, reverting `reverts`, made of
    /// `steps` (each recorded for `target`).
    fn family_line(
        base: &HistoryEntry,
        kind: TxKind,
        reverts: Option<TxId>,
        steps: &[Step],
        target: Option<TargetKey>,
    ) -> HistoryEntry {
        HistoryEntry {
            tx: TxId::new(),
            kind,
            reverts,
            repairs: steps
                .iter()
                .map(|step| RepairEntry::from_step(step, |_| target))
                .collect(),
            ..base.clone()
        }
    }

    /// A rename of `kind` from `from` to `to`, recorded as one step.
    fn rename_step(seq: u32, from: &Path, to: &Path, source: Endpoint, activation: bool) -> Step {
        Step {
            seq,
            fix: 1,
            fm: Cow::Borrowed(STUCK),
            op: Primitive::RenameNoReplace {
                from: from.to_path_buf(),
                to: to.to_path_buf(),
                activation,
            },
            post: Endpoints::Pair {
                path: Endpoint::Absent,
                to: without_stamp(&source),
            },
            pre: Endpoints::Pair {
                path: source,
                to: Endpoint::Absent,
            },
            guards: Vec::new(),
        }
    }

    /// A verified plan's inverses as the steps a repair-family line records (all known
    /// destinations), for synthetic undo lines.
    fn steps_of(inverses: &[InverseStep]) -> Vec<Step> {
        inverses
            .iter()
            .map(|inverse| Step {
                seq: inverse.seq,
                fix: inverse.fix,
                fm: Cow::Owned(inverse.fm.clone()),
                op: Primitive::RenameNoReplace {
                    from: inverse.path.clone(),
                    to: inverse.to.path().unwrap().to_path_buf(),
                    activation: inverse.activation,
                },
                pre: inverse.pre.clone(),
                post: inverse.post.clone(),
                guards: Vec::new(),
            })
            .collect()
    }

    /// An inverse's kind, path and destination, for comparing.
    fn shape(inverse: &InverseStep) -> (InverseKind, PathBuf, Dest) {
        (inverse.kind, inverse.path.clone(), inverse.to.clone())
    }

    // -----------------------------------------------------------------------------------
    // The family and its state.

    #[test]
    fn is_repair_family_cases() {
        let home = home();
        let tx = repaired(&home, &[chmod(&home)]);
        let repair = line_of(&home, tx);
        let undo_of_repair = HistoryEntry {
            kind: TxKind::Undo,
            reverts: Some(repair.tx),
            ..repair.clone()
        };
        let folder = folder_line(&home, TxKind::Install, &[(FolderOp::Add, "Foo")]);
        let undo_of_folder = HistoryEntry {
            kind: TxKind::Undo,
            reverts: Some(folder.tx),
            ..folder.clone()
        };
        let target_less_other = HistoryEntry {
            kind: TxKind::Install,
            ..repair.clone()
        };
        let undo_without_repairs = HistoryEntry {
            repairs: Vec::new(),
            ..undo_of_repair.clone()
        };
        for (case, line, want) in [
            ("repair", &repair, true),
            ("undo_of_a_repair", &undo_of_repair, true),
            ("undo_of_a_folder_transaction", &undo_of_folder, false),
            ("folder_line", &folder, false),
            (
                "target_less_line_of_another_kind",
                &target_less_other,
                false,
            ),
            ("undo_without_operations", &undo_without_repairs, false),
        ] {
            assert_eq!(
                is_repair_family(line),
                want,
                "case {case}: {}",
                serde_json::to_string(line).unwrap()
            );
        }
    }

    #[test]
    fn undo_state_follows_undo_chains() {
        let home = home();
        let tx = repaired(&home, &[chmod(&home)]);
        let r = line_of(&home, tx);
        let undo = |of: &HistoryEntry| HistoryEntry {
            tx: TxId::new(),
            kind: TxKind::Undo,
            reverts: Some(of.tx),
            ..r.clone()
        };
        let u = undo(&r);
        let u2 = undo(&u);
        let u3 = undo(&r);
        let other = undo(&folder_line(&home, TxKind::Install, &[]));
        let cases: Vec<(&str, Vec<HistoryEntry>, UndoState)> = vec![
            ("alone", vec![r.clone()], UndoState::InEffect),
            (
                "undone",
                vec![r.clone(), u.clone()],
                UndoState::UndoneBy(u.tx),
            ),
            (
                "undone_then_redone",
                vec![r.clone(), u.clone(), u2.clone()],
                UndoState::InEffect,
            ),
            (
                "redone_then_undone_again",
                vec![r.clone(), u.clone(), u2.clone(), u3.clone()],
                UndoState::UndoneBy(u3.tx),
            ),
            (
                "an_unrelated_undo",
                vec![r.clone(), other],
                UndoState::InEffect,
            ),
        ];
        for (case, entries, want) in cases {
            assert_eq!(undo_state(&entries, 0), want, "case {case}");
        }
    }

    // -----------------------------------------------------------------------------------
    // The inverse table.

    /// A case: its name, the fixes committed, and the inverses expected (kind, path,
    /// destination, mode), given the home and the repair.
    type Expected = Vec<(InverseKind, PathBuf, Dest, Option<u32>)>;
    type TableCase = (
        &'static str,
        fn(&RepairHome) -> Vec<PlannedFix>,
        fn(&RepairHome, TxId) -> Expected,
    );

    fn table() -> Vec<TableCase> {
        vec![
            (
                "write_over",
                |h| vec![write_over(h)],
                |h, _| {
                    vec![(
                        InverseKind::RestoreWrite,
                        h.skill_file(),
                        Dest::None,
                        Some(0o644),
                    )]
                },
            ),
            (
                "write_create",
                |h| vec![write_create(h)],
                |h, _| {
                    let rel = notes(h).strip_prefix(h.paths.home()).unwrap().to_path_buf();
                    vec![(
                        InverseKind::QuarantineCreated,
                        notes(h),
                        Dest::Quarantine {
                            side: QuarantineSide::Skill,
                            rel,
                        },
                        None,
                    )]
                },
            ),
            (
                "chmod",
                |h| vec![chmod(h)],
                |h, _| {
                    vec![(
                        InverseKind::ChmodBack,
                        h.credential(),
                        Dest::None,
                        Some(0o644),
                    )]
                },
            ),
            (
                "rename_file",
                |h| vec![quarantine(h.record(), QuarantineRoot::State)],
                |h, _| {
                    let rel = format!("targets/{}.json", h.key);
                    vec![(
                        InverseKind::RenameBack,
                        state_quarantine(h, &rel),
                        Dest::Path(h.record()),
                        None,
                    )]
                },
            ),
            (
                "rename_dir",
                |h| vec![quarantine(h.addons("Stray"), client_root(h))],
                |h, tx| {
                    vec![(
                        InverseKind::RenameBack,
                        parked(h, tx, "Interface/AddOns/Stray"),
                        Dest::Path(h.addons("Stray")),
                        None,
                    )]
                },
            ),
            (
                "rename_temp_symlink",
                |h| vec![quarantine(h.temp_link(), QuarantineRoot::Cache)],
                |h, _| {
                    let to = h
                        .paths
                        .cache_quarantine(&h.run.id)
                        .join("tmp/.omaddons-link");
                    vec![(InverseKind::RenameBack, to, Dest::Path(h.temp_link()), None)]
                },
            ),
            (
                "flatten_two",
                |h| vec![flatten(h, "Wrap/Inner", "Wrap")],
                |h, tx| {
                    vec![
                        (
                            InverseKind::RenameBack,
                            parked(h, tx, "Interface/AddOns/Wrap"),
                            Dest::Path(h.addons("Wrap")),
                            None,
                        ),
                        (
                            InverseKind::RenameBack,
                            h.addons("Inner"),
                            Dest::Path(h.addons("Wrap/Inner")),
                            None,
                        ),
                    ]
                },
            ),
            (
                "flatten_three",
                |h| vec![flatten(h, "Same/Same", "Same")],
                // Lowered as: the wrapper to the provisional name, the inner folder out of
                // it into its place, the wrapper into quarantine. The second inverse's
                // destination exists only once the first has put the wrapper back.
                |h, tx| {
                    let provisional = h.client.flatten_provisional(&tx);
                    vec![
                        (
                            InverseKind::RenameBack,
                            parked(h, tx, "Interface/AddOns/Same"),
                            Dest::Path(provisional.clone()),
                            None,
                        ),
                        (
                            InverseKind::RenameBack,
                            h.addons("Same"),
                            Dest::Path(provisional.join("Same")),
                            None,
                        ),
                        (
                            InverseKind::RenameBack,
                            provisional,
                            Dest::Path(h.addons("Same")),
                            None,
                        ),
                    ]
                },
            ),
            (
                "suspended_journal",
                |h| vec![stuck_set_aside(h, guards(h))],
                |h, tx| {
                    let stuck = h.stuck();
                    let journal = h.paths.journal(&stuck);
                    let provisional = super::super::provisional_journal(&h.paths, &stuck);
                    vec![
                        (
                            InverseKind::ToProvisional,
                            state_quarantine(h, &format!("journal/{stuck}.jsonl")),
                            Dest::Path(provisional.clone()),
                            None,
                        ),
                        (
                            InverseKind::RenameBack,
                            parked(h, tx, &format!("Interface/.omaddons/stage/{stuck}")),
                            Dest::Path(h.client.stage(&stuck)),
                            None,
                        ),
                        (
                            InverseKind::Activate,
                            provisional,
                            Dest::Path(journal),
                            None,
                        ),
                    ]
                },
            ),
        ]
    }

    #[test]
    fn inverse_table_per_primitive_kind() {
        for (case, fixes, want) in table() {
            let home = home();
            let tx = repaired(&home, &fixes(&home));
            let plan = plan_of(&home, tx);
            let got: Vec<(InverseKind, PathBuf, Dest, Option<u32>)> = plan
                .inverses
                .iter()
                .map(|inverse| {
                    let (kind, path, to) = shape(inverse);
                    (kind, path, to, inverse.mode.map(Mode::bits))
                })
                .collect();
            assert_eq!(
                got,
                want(&home, tx),
                "case {case}: line {} gave {:#?}",
                serde_json::to_string(&line_of(&home, tx)).unwrap(),
                plan.inverses
            );
            let seqs: Vec<u32> = plan.inverses.iter().map(|inverse| inverse.seq).collect();
            assert_eq!(
                seqs,
                (1..=u32::try_from(seqs.len()).unwrap()).collect::<Vec<_>>(),
                "case {case}"
            );
            assert_eq!(
                plan.inverses
                    .iter()
                    .filter(|inverse| inverse.activation)
                    .map(|inverse| inverse.kind)
                    .collect::<Vec<_>>(),
                plan.inverses
                    .iter()
                    .filter(|inverse| inverse.kind == InverseKind::Activate)
                    .map(|inverse| inverse.kind)
                    .collect::<Vec<_>>(),
                "case {case}: only an activation is flagged"
            );
        }
    }

    /// Each inverse's `pre` is the reverted primitive's `post` (oriented as it runs), and
    /// its `post` the reverted `pre`'s parts, a directory's stamp aside.
    #[test]
    fn predicates_swap_pre_and_post() {
        for (case, fixes, _) in table() {
            let home = home();
            let tx = repaired(&home, &fixes(&home));
            let line = line_of(&home, tx);
            let plan = plan_of(&home, tx);
            for inverse in &plan.inverses {
                let reverted = line
                    .repairs
                    .iter()
                    .find(|entry| entry.seq == inverse.reverts)
                    .map(|entry| step_of(entry).unwrap())
                    .unwrap();
                let (pre, post) = (&reverted.pre, &reverted.post);
                let context = format!("case {case}, inverse {}: {inverse:#?}", inverse.seq);
                match (inverse.kind, pre, post) {
                    (InverseKind::RestoreWrite, Endpoints::One(was), Endpoints::One(now)) => {
                        assert_eq!(inverse.pre, Endpoints::One(now.clone()), "{context}");
                        let Endpoint::File { sha256, mode, .. } = was else {
                            panic!("{context}"); // ubs:ignore — fails the test
                        };
                        assert_eq!(
                            inverse.post,
                            Endpoints::One(Endpoint::File {
                                identity: None,
                                sha256: *sha256,
                                mode: *mode,
                            }),
                            "{context}"
                        );
                    }
                    (InverseKind::ChmodBack, Endpoints::One(was), Endpoints::One(now)) => {
                        assert_eq!(inverse.pre, Endpoints::One(now.clone()), "{context}");
                        let Endpoint::File { identity, mode, .. } = was else {
                            panic!("{context}"); // ubs:ignore — fails the test
                        };
                        assert_eq!(
                            inverse.post,
                            Endpoints::One(Endpoint::File {
                                identity: *identity,
                                sha256: None,
                                mode: *mode,
                            }),
                            "{context}"
                        );
                    }
                    (InverseKind::QuarantineCreated, Endpoints::One(_), Endpoints::One(now)) => {
                        assert_eq!(
                            inverse.pre,
                            Endpoints::Pair {
                                path: now.clone(),
                                to: Endpoint::Absent,
                            },
                            "{context}"
                        );
                    }
                    (
                        InverseKind::RenameBack | InverseKind::Activate,
                        Endpoints::Pair { path: was, .. },
                        Endpoints::Pair { to: now, .. },
                    ) => {
                        assert_eq!(
                            inverse.pre,
                            Endpoints::Pair {
                                path: now.clone(),
                                to: Endpoint::Absent,
                            },
                            "{context}"
                        );
                        assert_eq!(
                            inverse.post,
                            Endpoints::Pair {
                                path: Endpoint::Absent,
                                to: without_stamp(was),
                            },
                            "{context}"
                        );
                        assert_eq!(was.identity(), now.identity(), "{context}");
                    }
                    (
                        InverseKind::ToProvisional,
                        Endpoints::Pair { .. },
                        Endpoints::Pair { to: now, .. },
                    ) => {
                        assert_eq!(
                            inverse.post,
                            Endpoints::Pair {
                                path: Endpoint::Absent,
                                to: now.clone(),
                            },
                            "{context}"
                        );
                    }
                    other => panic!("{context}: unexpected {other:?}"), // ubs:ignore — fails the test
                }
            }
        }
    }

    #[test]
    fn an_unchanged_world_passes_and_says_what_it_read() {
        for (case, fixes, _) in table() {
            let home = home();
            let tx = repaired(&home, &fixes(&home));
            let verified = check(&home, tx).unwrap_or_else(|err| panic!("case {case}: {err:?}")); // ubs:ignore — fails the test
            let plan = &verified.plan;
            let observed = &verified.observed;
            assert_eq!(
                observed.predicates.len(),
                plan.inverses.len(),
                "case {case}"
            );
            assert_eq!(observed.backups.len(), plan.reads.len(), "case {case}");
            assert_eq!(observed.guards.len(), plan.guards.len(), "case {case}");
            assert_eq!(
                observed.generations,
                plan.targets.iter().map(|key| (*key, 1)).collect::<Vec<_>>(),
                "case {case}"
            );
            for inverse in &plan.inverses {
                // What the commit's step records is complete: it validates as a step.
                if let Some(to) = inverse.to.path() {
                    let step = Step {
                        seq: inverse.seq,
                        fix: inverse.fix,
                        fm: Cow::Owned(inverse.fm.clone()),
                        op: Primitive::RenameNoReplace {
                            from: inverse.path.clone(),
                            to: to.to_path_buf(),
                            activation: inverse.activation,
                        },
                        pre: inverse.pre.clone(),
                        post: inverse.post.clone(),
                        guards: Vec::new(),
                    };
                    assert_eq!(step.validate(), Ok(()), "case {case}: {inverse:#?}");
                }
                if inverse.kind == InverseKind::QuarantineCreated {
                    let Endpoints::Pair { path, .. } = &inverse.pre else {
                        panic!("case {case}"); // ubs:ignore — fails the test
                    };
                    assert!(path.identity().is_some(), "case {case}: {inverse:#?}");
                }
            }
        }
    }

    #[test]
    fn a_suspension_restores_the_provisional_name_first_then_everything_else_then_the_activation() {
        for journal_at in [0, 2, 4] {
            let home = home();
            let stuck = home.stuck();
            fs::create_dir_all(home.paths.state_snapshot(&stuck)).unwrap();
            fs::write(home.paths.state_snapshot(&stuck).join("x"), "x").unwrap();
            fs::create_dir_all(home.client.snapshots(&stuck)).unwrap();
            let mut artifacts = vec![
                home.paths.inflight_lock(&stuck),
                home.paths.state_snapshot(&stuck),
                home.client.stage(&stuck),
                home.client.snapshots(&stuck),
            ];
            artifacts.insert(journal_at, home.paths.journal(&stuck));
            let tx = repaired(&home, &[set_aside(&home, stuck, &artifacts, Vec::new())]);
            let plan = plan_of(&home, tx);
            let case = format!("journal at {}: {:#?}", journal_at + 1, plan.inverses);
            let kinds: Vec<InverseKind> = plan.inverses.iter().map(|i| i.kind).collect();
            assert_eq!(
                kinds,
                [
                    InverseKind::ToProvisional,
                    InverseKind::RenameBack,
                    InverseKind::RenameBack,
                    InverseKind::RenameBack,
                    InverseKind::RenameBack,
                    InverseKind::Activate,
                ],
                "{case}"
            );
            let journal_seq = u32::try_from(journal_at + 1).unwrap();
            assert_eq!(plan.inverses[0].reverts, journal_seq, "{case}");
            assert_eq!(plan.inverses[5].reverts, journal_seq, "{case}");
            let others: Vec<u32> = plan.inverses[1..5].iter().map(|i| i.reverts).collect();
            let mut newest_first = others.clone();
            newest_first.sort_by_key(|seq| std::cmp::Reverse(*seq));
            assert_eq!(others, newest_first, "{case}");
            assert_eq!(
                plan.inverses.iter().map(|i| i.seq).collect::<Vec<_>>(),
                (1..=6).collect::<Vec<_>>(),
                "{case}"
            );
            // E14's activation rules accept the undo it plans.
            let verified = check(&home, tx).unwrap();
            let mut prepared = repair_prepared(&home, &[], &[chmod(&home)]);
            prepared.primitives = steps_of(&verified.plan.inverses);
            assert_eq!(
                crate::engine::barrier::validate(&prepared, &home.paths),
                Ok(()),
                "{case}"
            );
        }
    }

    /// A repair that set aside two journals (a forced fix selects every stuck journal of
    /// its failure mode, and one run is one repair) comes back with both provisional
    /// restores first and both activations last, which E14's rules accept, and the undo
    /// runs: both journals back under their own names, neither under its provisional one
    /// (omad-i2c5; before, `plan_undo` refused it with `state_corrupt`).
    #[test]
    fn a_set_aside_of_two_journals_reactivates_both_last() {
        let home = home();
        let stuck = home.stuck();
        let second = TxId::new();
        let journal = "{\"line\":\"prepared\"}\n";
        fs::write(home.paths.journal(&second), journal).unwrap();
        let tx = repaired(
            &home,
            &[
                stuck_set_aside(&home, Vec::new()),
                set_aside(&home, second, &[home.paths.journal(&second)], Vec::new()),
            ],
        );
        let plan = plan_of(&home, tx);
        let kinds: Vec<InverseKind> = plan.inverses.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            [
                InverseKind::ToProvisional,
                InverseKind::ToProvisional,
                InverseKind::RenameBack,
                InverseKind::Activate,
                InverseKind::Activate,
            ],
            "{:#?}",
            plan.inverses
        );
        let activated: BTreeSet<PathBuf> = plan.inverses[3..]
            .iter()
            .map(|i| match &i.to {
                Dest::Path(to) => to.clone(),
                other => panic!("{other:?}"), // ubs:ignore — fails the test
            })
            .collect();
        assert_eq!(
            activated,
            [home.paths.journal(&stuck), home.paths.journal(&second)].into(),
            "one activation per journal"
        );
        assert_eq!(
            plan.fixes.iter().map(|f| f.kind).collect::<Vec<_>>(),
            [FixKind::Suspend, FixKind::Suspend]
        );
        let verified = check(&home, tx).unwrap();
        let mut prepared = repair_prepared(&home, &[], &[chmod(&home)]);
        prepared.primitives = steps_of(&verified.plan.inverses);
        assert_eq!(
            crate::engine::barrier::validate(&prepared, &home.paths),
            Ok(()),
            "E14's rules accept two activations, both last"
        );
        let outcome = engine_undo(&home, tx, false, vec![discovered(&home)]);
        assert!(outcome.is_ok(), "{outcome:?}");
        for (tx, bytes) in [(stuck, None), (second, Some(journal))] {
            let own = home.paths.journal(&tx);
            assert!(own.exists(), "{tx} is back under its own name");
            if let Some(bytes) = bytes {
                assert_eq!(fs::read_to_string(&own).unwrap(), bytes, "{tx}");
            }
            let provisional = super::super::provisional_journal(&home.paths, &tx);
            assert!(!provisional.exists(), "{tx} is not left provisional");
        }
        assert!(home.client.stage(&stuck).exists(), "the stage is back");
    }

    #[test]
    fn the_inverse_of_an_activation_hides_the_journal_first() {
        let home = home();
        let tx = repaired(&home, &[stuck_set_aside(&home, Vec::new())]);
        let set_aside = line_of(&home, tx);
        // The reactivation an undo of the set-aside commits, as its history line records it.
        let verified = check(&home, tx).unwrap();
        let reactivation = family_line(
            &set_aside,
            TxKind::Undo,
            Some(tx),
            &steps_of(&verified.plan.inverses),
            Some(home.key),
        );
        let entries = vec![set_aside.clone(), reactivation.clone()];
        let plan = plan_undo(&entries, 1, &home.paths).unwrap();
        let stuck = home.stuck();
        let provisional = super::super::provisional_journal(&home.paths, &stuck);
        let quarantined = state_quarantine(&home, &format!("journal/{stuck}.jsonl"));
        assert_eq!(
            plan.inverses.iter().map(shape).collect::<Vec<_>>(),
            vec![
                (
                    InverseKind::Deactivate,
                    home.paths.journal(&stuck),
                    Dest::Path(provisional.clone()),
                ),
                (
                    InverseKind::RenameBack,
                    home.client.stage(&stuck),
                    Dest::Path(parked(
                        &home,
                        tx,
                        &format!("Interface/.omaddons/stage/{stuck}")
                    )),
                ),
                (
                    InverseKind::RenameBack,
                    provisional,
                    Dest::Path(quarantined)
                ),
            ],
            "{:#?}",
            plan.inverses
        );
        assert!(plan.inverses.iter().all(|i| !i.activation), "{plan:#?}");
        assert_eq!(plan.reverted_kind, TxKind::Undo);
        assert_eq!(
            plan.fixes.iter().map(|f| f.kind).collect::<Vec<_>>(),
            [FixKind::Suspend]
        );
    }

    #[test]
    fn the_provisional_names_are_invisible_to_folder_listing() {
        for wtf in [false, true] {
            let home = home();
            let stuck = home.stuck();
            let journal = if wtf {
                let named = home.paths.journal_wtf(&stuck);
                fs::rename(home.paths.journal(&stuck), &named).unwrap();
                named
            } else {
                home.paths.journal(&stuck)
            };
            let tx = repaired(
                &home,
                &[set_aside(
                    &home,
                    stuck,
                    std::slice::from_ref(&journal),
                    Vec::new(),
                )],
            );
            let plan = plan_of(&home, tx);
            let provisional = super::super::provisional_journal(&home.paths, &stuck);
            assert_eq!(
                plan.inverses.iter().map(shape).collect::<Vec<_>>(),
                vec![
                    (
                        InverseKind::ToProvisional,
                        state_quarantine(
                            &home,
                            &format!("journal/{}", journal.file_name().unwrap().to_str().unwrap()),
                        ),
                        Dest::Path(provisional.clone()),
                    ),
                    (
                        InverseKind::Activate,
                        provisional.clone(),
                        Dest::Path(journal)
                    ),
                ],
                "wtf {wtf}"
            );
            // A journal under its provisional name is listed by nothing that recovers.
            fs::write(&provisional, "{\"line\":\"prepared\"}\n").unwrap();
            let name = provisional.file_name().unwrap().to_str().unwrap();
            assert_eq!(
                crate::state::paths::tx_named(name.strip_suffix(".jsonl").unwrap()),
                None,
                "{name}"
            );
            let pending = crate::engine::journal::pending(&home.paths).unwrap();
            assert!(
                pending.iter().all(|p| p.tx != stuck),
                "wtf {wtf}: {pending:?}"
            );
        }
    }

    #[test]
    fn the_canonical_form_holds_nothing_run_specific() {
        let home = home();
        let tx = repaired(&home, &[write_create(&home), chmod(&home)]);
        let first = plan_of(&home, tx).canonical();
        let second = plan_of(&home, tx).canonical();
        assert_eq!(first, second, "two undos of one line plan alike");
        let text = first.to_string();
        for key in [
            "\"seq\"",
            "\"backup\"",
            "\"reverts\"",
            "\"pre\"",
            "\"post\"",
        ] {
            assert!(!text.contains(key), "{key} in {text}");
        }
        let rel = notes(&home)
            .strip_prefix(home.paths.home())
            .unwrap()
            .display()
            .to_string();
        let notes = json!(notes(&home));
        let created = first
            .as_array()
            .unwrap()
            .iter()
            // ubs:ignore — comparing paths, not secrets
            .find(|inverse| inverse["path"] == notes)
            .unwrap();
        assert_eq!(
            created["to_rel"],
            json!(format!("skill/{rel}")),
            "{created}"
        );
        assert_eq!(created["op"], json!("rename_noreplace"));
        let chmod_back = first
            .as_array()
            .unwrap()
            .iter()
            .find(|inverse| inverse["op"] == json!("chmod"))
            .unwrap();
        assert_eq!(chmod_back["mode"], json!("0644"), "{chmod_back}");
        assert!(chmod_back.get("to_rel").is_none(), "{chmod_back}");
    }

    // -----------------------------------------------------------------------------------
    // Lines that cannot be inverted.

    #[test]
    fn a_line_that_cannot_be_inverted_is_state_corrupt() {
        let home = home();
        let chmod_tx = repaired(&home, &[chmod(&home)]);
        let base = line_of(&home, chmod_tx);
        let write_tx = repaired_in(&home, &later_run(&home), &[write_over(&home)]);
        let write = line_of(&home, write_tx);
        let edit = |line: &HistoryEntry, f: &dyn Fn(&mut RepairEntry)| {
            let mut line = line.clone();
            for entry in &mut line.repairs {
                f(entry);
            }
            line
        };
        let wtf_path = home
            .client
            .wtf()
            .join("Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/X.lua");
        let mut too_many = base.clone();
        too_many.repairs = (1..=u32::try_from(DOCTOR_FIX_MAX_ACTIONS).unwrap() + 1)
            .map(|seq| RepairEntry {
                seq,
                ..base.repairs[0].clone()
            })
            .collect();
        let cases: Vec<(&str, HistoryEntry, &str)> = vec![
            ("too_many_primitives", too_many, "more than"),
            (
                "relative_path",
                edit(&base, &|e| e.path = "relative/credential".into()),
                "not absolute",
            ),
            (
                "dotdot_path",
                edit(&base, &|e| e.path = home.credential().join("../credential")),
                "`..`",
            ),
            (
                "not_canonical",
                edit(&base, &|e| {
                    e.path = home.credential().parent().unwrap().join("./github");
                }),
                "canonical",
            ),
            (
                "rename_without_to",
                edit(&base, &|e| e.op = history::RepairOp::RenameNoReplace),
                "does not read as a step",
            ),
            (
                "write_without_post_sha256",
                edit(&write, &|e| {
                    if let history::RepairEndpoints::Path(post) = &mut e.post {
                        post.sha256 = None;
                    }
                }),
                "sha256",
            ),
            (
                "overwrite_without_backup",
                edit(&write, &|e| e.backup = None),
                "backup",
            ),
            (
                "overwrite_without_its_mode",
                edit(&write, &|e| {
                    if let history::RepairEndpoints::Path(pre) = &mut e.pre {
                        pre.mode = None;
                    }
                }),
                "no mode",
            ),
            (
                "chmod_without_identity",
                edit(&base, &|e| {
                    for side in [&mut e.pre, &mut e.post] {
                        if let history::RepairEndpoints::Path(predicate) = side {
                            predicate.identity = None;
                        }
                    }
                }),
                "identity",
            ),
            (
                "path_under_wtf",
                edit(&base, &|e| e.path = wtf_path.clone()),
                "below a client's WTF/",
            ),
        ];
        for (case, line, phrase) in cases {
            let entries = [line.clone()];
            let err = plan_undo(&entries, 0, &home.paths).unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::StateCorrupt,
                "case {case}: line {} gave {err:?}",
                serde_json::to_string(&line).unwrap()
            );
            assert!(
                err.message
                    .contains("is not a repair this omaddons can invert")
                    && err.message.contains(phrase),
                "case {case}: {}",
                err.message
            );
            assert_eq!(err.details["tx"], json!(line.tx), "case {case}");
            assert_eq!(err.code.check_details(&err.details), Ok(()), "case {case}");
            assert_eq!(
                err.fix.argv(),
                Some(&["omaddons".to_owned(), "history".to_owned()][..]),
                "case {case}"
            );
            for name in ["FAKEACCT", "FakeRealm", "FakeChar"] {
                assert!(!err.message.contains(name), "case {case}: {}", err.message);
            }
        }
    }

    #[test]
    fn a_line_without_primitives_is_not_found() {
        let home = home();
        let tx = repaired(&home, &[chmod(&home)]);
        let line = HistoryEntry {
            repairs: Vec::new(),
            ..line_of(&home, tx)
        };
        let err = plan_undo(std::slice::from_ref(&line), 0, &home.paths).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound, "{err:?}");
        assert!(err.message.contains("changed no file"), "{}", err.message);
        assert_eq!(err.code.check_details(&err.details), Ok(()));
    }

    // -----------------------------------------------------------------------------------
    // The checks.

    #[test]
    fn an_undone_repair_is_refused_as_already_undone() {
        let home = home();
        let tx = repaired(&home, &[chmod(&home)]);
        let r = line_of(&home, tx);
        let undo = HistoryEntry {
            tx: TxId::new(),
            kind: TxKind::Undo,
            reverts: Some(tx),
            ..r
        };
        plant(&home, &undo);
        let err = refused(check(&home, tx), "undone");
        assert_eq!(err.code, ErrorCode::UndoConflict);
        assert_eq!(
            err.message,
            format!("transaction {tx} was already undone by {}", undo.tx)
        );
        assert_eq!(
            err.details["conflicting"],
            json!([{"tx": undo.tx, "kind": "undo", "folders": []}])
        );
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "undo", undo.tx.to_string().as_str()]
        );
    }

    #[test]
    fn a_changed_guard_refuses_with_its_kinds_and_target() {
        type Change = fn(&RepairHome);
        let cases: Vec<(&str, Change, &[&str])> = vec![
            ("generation", |h| record_at(h, 7), &["generation"]),
            (
                "marker",
                |h| fs::write(h.client.marker(), "{\"key\":\"another\"}\n").unwrap(),
                &["marker"],
            ),
            (
                "folder",
                |h| fs::write(h.addons("Stray/stray.lua"), "-- changed, and longer\n").unwrap(),
                &["epochs"],
            ),
            (
                "wtf",
                |h| fs::write(h.client.wtf().join("Config.wtf"), "SET x \"y\"\n").unwrap(),
                &["epochs"],
            ),
            (
                "generation_and_wtf",
                |h| {
                    record_at(h, 7);
                    fs::write(h.client.wtf().join("Config.wtf"), "SET x \"y\"\n").unwrap();
                },
                &["epochs", "generation"],
            ),
        ];
        for (case, change, want) in cases {
            let home = home();
            let tx = repaired(&home, &[stuck_set_aside(&home, guards(&home))]);
            change(&home);
            let err = refused(check(&home, tx), case);
            assert_eq!(err.code, ErrorCode::UndoConflict, "case {case}: {err:?}");
            assert_eq!(
                err.details,
                json!({
                    "tx": tx,
                    "reason": "changed_since_suspension",
                    "changed_guards": want,
                    "target": home.key,
                }),
                "case {case}"
            );
            assert!(
                err.message.contains("set aside a journal of retail"),
                "case {case}: {}",
                err.message
            );
            assert_eq!(
                err.fix.argv().unwrap(),
                [
                    "omaddons",
                    "doctor",
                    "explain",
                    STUCK,
                    "-t",
                    home.key.to_string().as_str()
                ],
                "case {case}"
            );
        }
    }

    #[test]
    fn a_changed_path_refuses_with_its_paths() {
        type Setup = fn(&RepairHome) -> Vec<PlannedFix>;
        type Change = fn(&RepairHome, TxId) -> Vec<PathBuf>;
        let cases: Vec<(&str, Setup, Change)> = vec![
            (
                "identity_replaced",
                |h| vec![chmod(h)],
                |h, _| {
                    let path = h.credential();
                    fs::remove_file(&path).unwrap();
                    fs::write(&path, "ghp_fixture\n").unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                    vec![path]
                },
            ),
            (
                "same_size_content_edit",
                |h| vec![write_over(h)],
                |h, _| {
                    let path = h.skill_file();
                    fs::write(&path, b"---\nname: omaddons\n---\nNEW guide\n").unwrap();
                    vec![path]
                },
            ),
            (
                "mode_changed",
                |h| vec![chmod(h)],
                |h, _| {
                    let path = h.credential();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
                    vec![path]
                },
            ),
            (
                "destination_appeared",
                |h| vec![quarantine(h.addons("Stray"), client_root(h))],
                |h, _| {
                    fs::create_dir(h.addons("Stray")).unwrap();
                    vec![h.addons("Stray")]
                },
            ),
            (
                "source_vanished",
                |h| vec![quarantine(h.record(), QuarantineRoot::State)],
                |h, _| {
                    let path = state_quarantine(h, &format!("targets/{}.json", h.key));
                    fs::remove_file(&path).unwrap();
                    vec![path]
                },
            ),
            (
                "symlink_retargeted",
                |h| vec![quarantine(h.temp_link(), QuarantineRoot::Cache)],
                |h, _| {
                    let path = h
                        .paths
                        .cache_quarantine(&h.run.id)
                        .join("tmp/.omaddons-link");
                    fs::remove_file(&path).unwrap();
                    symlink("elsewhere", &path).unwrap();
                    vec![path]
                },
            ),
            (
                "created_file_edited",
                |h| vec![write_create(h)],
                |h, _| {
                    fs::write(notes(h), "notes, edited\n").unwrap();
                    vec![notes(h)]
                },
            ),
            (
                "flatten_wrapper_put_back_by_hand",
                |h| vec![flatten(h, "Wrap/Inner", "Wrap")],
                |h, _| {
                    // A folder where the undo would put the wrapper back.
                    fs::create_dir(h.addons("Wrap")).unwrap();
                    vec![h.addons("Wrap")]
                },
            ),
        ];
        for (case, setup, change) in cases {
            let home = home();
            let tx = repaired(&home, &setup(&home));
            let mut want: Vec<String> = change(&home, tx)
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            want.sort();
            let err = refused(check(&home, tx), case);
            assert_eq!(err.code, ErrorCode::UndoConflict, "case {case}: {err:?}");
            assert_eq!(
                err.details,
                json!({"tx": tx, "reason": "changed_since_repair", "paths": want}),
                "case {case}"
            );
            assert!(
                err.message.contains("changed since the repair"),
                "case {case}: {}",
                err.message
            );
            assert_eq!(
                err.fix.argv().unwrap(),
                ["omaddons", "history"],
                "case {case}"
            );
        }
    }

    #[test]
    fn a_missing_or_edited_backup_refuses() {
        type Change = fn(&Path);
        let cases: Vec<(&str, Change)> = vec![
            ("deleted", |backup| fs::remove_file(backup).unwrap()),
            ("one_byte_flipped", |backup| {
                let mut bytes = fs::read(backup).unwrap();
                bytes[0] ^= 1;
                fs::set_permissions(backup, fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(backup, bytes).unwrap();
            }),
            ("replaced_by_a_directory", |backup| {
                fs::remove_file(backup).unwrap();
                fs::create_dir(backup).unwrap();
            }),
        ];
        for (case, change) in cases {
            let home = home();
            let tx = repaired(&home, &[write_over(&home)]);
            let backup = plan_of(&home, tx).reads[0].path.clone();
            assert!(backup.starts_with(&home.run.dir), "case {case}: {backup:?}");
            // The run keeps its backups read-only; the user changing one is the case.
            let parent = backup.parent().unwrap();
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
            change(&backup);
            let err = refused(check(&home, tx), case);
            assert_eq!(
                err.details,
                json!({
                    "tx": tx,
                    "reason": "changed_since_repair",
                    "paths": [backup.display().to_string()],
                }),
                "case {case}"
            );
        }
    }

    #[test]
    fn a_later_overlapping_transaction_refuses() {
        type Setup = fn(&RepairHome) -> Vec<PlannedFix>;
        type Later = fn(&RepairHome, &HistoryEntry) -> HistoryEntry;
        type Named = fn(&RepairHome) -> Vec<PathBuf>;
        // A case: the repair reverted, the later line, and the folders and paths named.
        let cases: Vec<(&str, Setup, Later, &[&str], Named)> = vec![
            (
                "later_install_replacing_a_flattened_folder",
                |h| vec![flatten(h, "Wrap/Inner", "Wrap")],
                |h, _| folder_line(h, TxKind::Install, &[(FolderOp::Replace, "Inner")]),
                &["Inner"],
                |h| vec![h.addons("Inner")],
            ),
            (
                "later_adopt_recording_it",
                |h| vec![flatten(h, "Wrap/Inner", "Wrap")],
                |h, _| folder_line(h, TxKind::Adopt, &[(FolderOp::Record, "inner")]),
                &["Inner"],
                |h| vec![h.addons("Inner")],
            ),
            (
                "later_repair_writing_the_same_skill_file",
                |h| vec![write_over(h)],
                |h, r| {
                    let step = Step {
                        op: Primitive::WriteFile {
                            path: h.skill_file(),
                            mode: mode(0o644),
                            backup: Some(later_run(h).dir.join("backups/1")),
                        },
                        ..step_of(&r.repairs[0]).unwrap()
                    };
                    family_line(r, TxKind::Repair, None, &[step], None)
                },
                &[],
                |h| vec![h.skill_file()],
            ),
            (
                "later_repair_quarantining_a_parent_directory",
                |h| vec![write_create(h)],
                |h, r| {
                    let skill = h.paths.skill_dir();
                    let step = rename_step(
                        1,
                        &skill,
                        &later_run(h).dir.join("quarantine/skill/omaddons"),
                        Endpoint::Dir {
                            identity: Some(Identity {
                                ino: 9,
                                born_ns: None,
                            }),
                            stamp: Some(Stamp([1; 32])),
                        },
                        false,
                    );
                    family_line(r, TxKind::Repair, None, &[step], None)
                },
                &[],
                |h| vec![notes(h)],
            ),
            (
                "later_repair_writing_a_child_of_a_quarantined_directory",
                |h| vec![quarantine(h.addons("Stray"), client_root(h))],
                |h, r| {
                    let step = Step {
                        seq: 1,
                        fix: 1,
                        fm: Cow::Borrowed(SKILL),
                        op: Primitive::WriteFile {
                            path: h.addons("Stray/new.lua"),
                            mode: mode(0o644),
                            backup: None,
                        },
                        pre: Endpoints::One(Endpoint::Absent),
                        post: Endpoints::One(Endpoint::File {
                            identity: None,
                            sha256: Some(crate::engine::repair_action::Sha256([2; 32])),
                            mode: Some(mode(0o644)),
                        }),
                        guards: Vec::new(),
                    };
                    family_line(r, TxKind::Repair, None, &[step], Some(h.key))
                },
                &["Stray"],
                |h| vec![h.addons("Stray")],
            ),
            (
                "later_install_over_a_quarantined_record",
                |h| vec![quarantine(h.record(), QuarantineRoot::State)],
                |h, _| folder_line(h, TxKind::Install, &[(FolderOp::Add, "Foo")]),
                &[],
                |h| vec![h.record()],
            ),
        ];
        for (case, setup, later, folders, paths) in cases {
            let home = home();
            let tx = repaired(&home, &setup(&home));
            let line = later(&home, &line_of(&home, tx));
            plant(&home, &line);
            let err = refused(check(&home, tx), case);
            let mut want: Vec<String> = paths(&home)
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            want.sort();
            assert_eq!(err.code, ErrorCode::UndoConflict, "case {case}: {err:?}");
            assert_eq!(
                err.details,
                json!({
                    "tx": tx,
                    "reason": "changed_since_repair",
                    "paths": want,
                    "conflicting": [{"tx": line.tx, "kind": line.kind, "folders": folders}],
                }),
                "case {case}"
            );
            assert_eq!(
                err.message,
                format!(
                    "1 later transaction(s) changed what {tx} repaired; the newest is {}",
                    line.tx
                ),
                "case {case}"
            );
            assert_eq!(
                err.fix.argv().unwrap(),
                ["omaddons", "undo", line.tx.to_string().as_str()],
                "case {case}"
            );
        }
    }

    #[test]
    fn unrelated_or_cancelled_later_transactions_pass() {
        let home = home();
        let tx = repaired(&home, &[write_over(&home)]);
        let r = line_of(&home, tx);
        // Another client's install, an install of another folder, and a later repair of
        // the same file that its own undo cancelled.
        plant(
            &home,
            &HistoryEntry {
                target: Some(TargetKey::new()),
                ..folder_line(&home, TxKind::Install, &[(FolderOp::Add, "Foo")])
            },
        );
        plant(
            &home,
            &folder_line(&home, TxKind::Install, &[(FolderOp::Add, "Bar")]),
        );
        let later = family_line(
            &r,
            TxKind::Repair,
            None,
            &[step_of(&r.repairs[0]).unwrap()],
            None,
        );
        let undo_of_later = HistoryEntry {
            reverts: Some(later.tx),
            ..family_line(
                &r,
                TxKind::Undo,
                None,
                &[step_of(&r.repairs[0]).unwrap()],
                None,
            )
        };
        plant(&home, &later);
        plant(&home, &undo_of_later);
        check(&home, tx).unwrap();
    }

    #[test]
    fn a_newer_unreadable_line_refuses_unless_it_rules_itself_out() {
        let other = TargetKey::new();
        let named = format!("{{\"schema_version\":\"9.0\",\"target\":\"{other}\"}}");
        type Setup = fn(&RepairHome) -> Vec<PlannedFix>;
        let cases: Vec<(&str, Setup, String, bool)> = vec![
            (
                "another_client_and_a_client_only_repair",
                |h| vec![quarantine(h.addons("Stray"), client_root(h))],
                named.clone(),
                true,
            ),
            (
                "another_client_and_a_home_side_repair",
                |h| vec![chmod(h)],
                named.clone(),
                false,
            ),
            (
                "not_json",
                |h| vec![quarantine(h.addons("Stray"), client_root(h))],
                "not json at all".to_owned(),
                false,
            ),
        ];
        for (case, setup, raw, passes) in cases {
            let home = home();
            let tx = repaired(&home, &setup(&home));
            plant_raw(&home, &raw);
            let result = check(&home, tx);
            if passes {
                assert!(result.is_ok(), "case {case}: {result:?}");
                continue;
            }
            let err = refused(result, case);
            assert_eq!(
                err.details,
                json!({"tx": tx, "reason": "history_unreadable", "lines": 1}),
                "case {case}"
            );
            assert!(err.fix.argv().is_none(), "case {case}: prose");
        }
    }

    #[test]
    fn a_mount_change_refuses_before_anything_moves() {
        let home = home();
        let tx = repaired(
            &home,
            &[quarantine(home.addons("Stray"), client_root(&home))],
        );
        let quarantine = home.client.snapshots_repair(&tx);
        let mounts = |path: &Path| -> io::Result<u64> {
            Ok(if path.starts_with(&quarantine) { 2 } else { 1 })
        };
        let err = refused(
            check_with(
                &home,
                tx,
                &Seams {
                    mount_id: &mounts,
                    ..Seams::default()
                },
            ),
            "two mounts",
        );
        let mut ends = vec![
            parked(&home, tx, "Interface/AddOns/Stray")
                .display()
                .to_string(),
            home.addons("Stray").display().to_string(),
        ];
        ends.sort();
        assert_eq!(
            err.details,
            json!({"tx": tx, "reason": "changed_since_repair", "paths": ends})
        );
        // A kernel that gives no mount id fails closed the same way.
        let unsupported = |_: &Path| -> io::Result<u64> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "no mount id"))
        };
        let err = refused(
            check_with(
                &home,
                tx,
                &Seams {
                    mount_id: &unsupported,
                    ..Seams::default()
                },
            ),
            "no mount id",
        );
        assert_eq!(err.details["reason"], json!("changed_since_repair"));
    }

    #[test]
    fn a_running_client_refuses_a_client_side_undo() {
        let home = home();
        let client_side = repaired(
            &home,
            &[quarantine(home.addons("Stray"), client_root(&home))],
        );
        let running = Seams {
            running: true,
            ..Seams::default()
        };
        let err = refused(check_with(&home, client_side, &running), "running");
        assert_eq!(err.code, ErrorCode::GameRunning);
        assert_eq!(err.details, json!({"target": "retail", "reason": "repair"}));
        assert!(
            err.message.starts_with("retail is running"),
            "{}",
            err.message
        );
        assert!(err.fix.argv().is_none(), "prose");
        // A home-side undo moves no client's files.
        let home_side = repaired_in(&home, &later_run(&home), &[chmod(&home)]);
        check_with(&home, home_side, &running).unwrap();
    }

    #[test]
    fn a_path_outside_the_scopes_refuses() {
        let home = home();
        let tx = repaired(
            &home,
            &[quarantine(home.addons("Stray"), client_root(&home))],
        );
        let without_the_client = ScopeTable::new(&home.paths, &[], None).unwrap();
        let err = refused(
            check_with(
                &home,
                tx,
                &Seams {
                    scopes: Some(without_the_client),
                    ..Seams::default()
                },
            ),
            "scopes",
        );
        assert_eq!(err.code, ErrorCode::ScopeDenied, "{err:?}");
        assert_eq!(err.details, json!({"tx": tx}), "a discovered client's path");
    }

    /// Two checks broken at once: the earlier one in the order is the one reported.
    #[test]
    fn the_first_failing_check_names_its_reason() {
        type Case = (
            &'static str,
            fn(&RepairHome) -> Vec<PlannedFix>,
            fn(&RepairHome, TxId),
            bool,
            fn(&Error) -> bool,
        );
        let cases: Vec<Case> = vec![
            (
                "in_effect_over_guards",
                |h| vec![stuck_set_aside(h, guards(h))],
                |h, tx| {
                    record_at(h, 7);
                    let r = line_of(h, tx);
                    plant(
                        h,
                        &HistoryEntry {
                            tx: TxId::new(),
                            kind: TxKind::Undo,
                            reverts: Some(tx),
                            ..r
                        },
                    );
                },
                false,
                |e| e.message.contains("already undone"),
            ),
            (
                "guards_over_overlaps",
                |h| vec![stuck_set_aside(h, guards(h))],
                |h, tx| {
                    record_at(h, 7);
                    let r = line_of(h, tx);
                    let stage = h.client.stage(&h.stuck());
                    let step = rename_step(
                        1,
                        &stage,
                        &later_run(h).dir.join("x"),
                        Endpoint::Dir {
                            identity: Some(Identity {
                                ino: 9,
                                born_ns: None,
                            }),
                            stamp: Some(Stamp([1; 32])),
                        },
                        false,
                    );
                    plant(
                        h,
                        &family_line(&r, TxKind::Repair, None, &[step], Some(h.key)),
                    );
                },
                false,
                |e| e.details["reason"] == json!("changed_since_suspension"),
            ),
            (
                "overlaps_over_unreadable",
                |h| vec![write_over(h)],
                |h, tx| {
                    let r = line_of(h, tx);
                    let step = step_of(&r.repairs[0]).unwrap();
                    plant(h, &family_line(&r, TxKind::Repair, None, &[step], None));
                    plant_raw(h, "not json");
                },
                false,
                |e| e.details["conflicting"].is_array(),
            ),
            (
                "unreadable_over_predicates",
                |h| vec![chmod(h)],
                |h, _| {
                    fs::set_permissions(h.credential(), fs::Permissions::from_mode(0o640)).unwrap();
                    plant_raw(h, "not json");
                },
                false,
                |e| e.details["reason"] == json!("history_unreadable"),
            ),
            (
                "predicates_over_scopes",
                |h| vec![quarantine(h.addons("Stray"), client_root(h))],
                |h, tx| fs::remove_dir_all(parked(h, tx, "Interface/AddOns/Stray")).unwrap(),
                true,
                |e| {
                    e.details["reason"] == json!("changed_since_repair")
                        && e.details["paths"].is_array()
                },
            ),
            (
                "scopes_over_running",
                |h| vec![quarantine(h.addons("Stray"), client_root(h))],
                |_, _| {},
                true,
                |e| e.code == ErrorCode::ScopeDenied,
            ),
        ];
        for (case, setup, change, narrow_scopes, holds) in cases {
            let home = home();
            let tx = repaired(&home, &setup(&home));
            change(&home, tx);
            let seams = Seams {
                running: true,
                scopes: narrow_scopes.then(|| ScopeTable::new(&home.paths, &[], None).unwrap()),
                ..Seams::default()
            };
            let err = refused(check_with(&home, tx, &seams), case);
            assert!(holds(&err), "case {case}: {err:?}");
        }
    }

    // -----------------------------------------------------------------------------------
    // The undo transaction.

    /// [`home`] without its stuck journal, which no build reads and which, naming no
    /// target, refuses every change until it is set aside.
    fn plain_home() -> RepairHome {
        let home = home();
        let stuck = home.stuck();
        fs::remove_file(home.paths.journal(&stuck)).unwrap();
        fs::remove_file(home.paths.inflight_lock(&stuck)).unwrap();
        home
    }

    /// `home`'s planned client as discovery finds it.
    fn discovered(home: &RepairHome) -> DiscoveredTarget {
        let mut target = crate::engine::testing::target(home.client.client_dir());
        target.id = "retail".into();
        target.key = Some(home.key);
        DiscoveredTarget {
            target,
            default_id: "retail".into(),
            qualifier: "battlenet".into(),
            source: crate::discover::DiscoverySource::Scan,
        }
    }

    /// `omaddons undo <tx>` on `home` through the engine (`engine::undo::undo`, which
    /// routes a repair-family line here), with `targets` discovered.
    fn engine_undo(
        home: &RepairHome,
        tx: TxId,
        dry_run: bool,
        targets: Vec<DiscoveredTarget>,
    ) -> Result<UndoOutcome> {
        let registry = crate::sources::SourceRegistry::new();
        let table = crate::rules::RulesTable::builtin();
        let argv: Vec<String> = ["omaddons", "undo", tx.to_string().as_str()]
            .map(str::to_owned)
            .to_vec();
        let running = BTreeSet::new();
        let known: Vec<Target> = targets.iter().map(|d| d.target.clone()).collect();
        let ctx = EngineCtx {
            paths: &home.paths,
            http: &crate::engine::testing::NoNetwork,
            registry: &registry,
            table: &table,
            policy: crate::state::config::RunningPolicy::Safe,
            limits: crate::engine::inspect::ArchiveLimits::default(),
            retention: crate::engine::retention::RetentionLimits::default(),
            actor: Actor::Agent,
            argv: &argv,
            command: "undo",
            running: Some(&running),
            known_targets: &known,
            cache: None,
            progress: &crate::output::ProgressSink::none(),
            catalog: None,
            default_channel: crate::model::Channel::Stable,
            downloads: None,
            indexes: None,
            notes: None,
            binding: None,
            auto_backup: None,
            applying: None,
        };
        let req = UndoRequest {
            selector: crate::engine::undo::UndoSelector::Tx(tx),
            sv_policy: crate::engine::undo::SvPolicy::Auto,
            dry_run,
            while_running: false,
            reason: Some("undo it".to_owned()),
            targets,
            run: RunId::new(),
        };
        crate::engine::undo::undo(&req, &ctx)
    }

    fn undone(home: &RepairHome, tx: TxId) -> UndoOutcome {
        engine_undo(home, tx, false, vec![discovered(home)])
            .unwrap_or_else(|err| panic!("undoing {tx}: {err:?}")) // ubs:ignore — fails the test
    }

    /// The run directories under `<state>/doctor/runs/`, by name.
    fn runs(home: &RepairHome) -> BTreeSet<String> {
        fs::read_dir(home.paths.doctor_runs_dir())
            .map(|dir| {
                dir.filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn undo_run_dir(home: &RepairHome, outcome: &UndoOutcome) -> PathBuf {
        let id = outcome
            .repair
            .as_ref()
            .and_then(|repair| repair.undo_run.clone())
            .unwrap();
        home.paths.doctor_runs_dir().join(id)
    }

    fn mode_of(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn journals(home: &RepairHome) -> Vec<String> {
        fs::read_dir(home.paths.journal_dir())
            .map(|dir| {
                dir.filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn an_undo_commits_through_the_repair_journal() {
        let home = plain_home();
        let credential = home.credential();
        let identity = crate::fsx::FileId::of(&credential).unwrap();
        let tx = repaired(&home, &[chmod(&home)]);
        assert_eq!(mode_of(&credential), 0o600);
        let journals_before = journals(&home);
        let outcome = undone(&home, tx);
        assert_eq!(mode_of(&credential), 0o644, "the mode is back");
        assert!(
            crate::fsx::FileId::same(crate::fsx::FileId::of(&credential), Some(identity)),
            "the same file"
        );
        let undo_tx = outcome.outcome.tx.unwrap();
        assert_eq!(outcome.undoes, tx);
        assert_eq!(outcome.outcome.kind, Some(TxKind::Undo));
        let line = line_of(&home, undo_tx);
        assert_eq!(line.kind, TxKind::Undo);
        assert_eq!(line.reverts, Some(tx));
        assert_eq!(line.target, None);
        assert!(line.well_formed() && is_repair_family(&line), "{line:?}");
        assert_eq!(line.repairs.len(), 1);
        assert_eq!(line.repairs[0].path, credential);
        let dir = undo_run_dir(&home, &outcome);
        assert_eq!(
            line.doctor_run_id.as_deref(),
            dir.file_name().unwrap().to_str(),
            "the undo's own run"
        );
        assert_eq!(journals(&home), journals_before, "the journal is gone");
        assert!(
            !home.paths.inflight_lock(&undo_tx).exists(),
            "and the in-flight lock"
        );
        let actions = fs::read_to_string(dir.join(ACTIONS_LOG)).unwrap();
        let actions: Vec<Value> = actions
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(actions.len(), 1, "exactly the inverse: {actions:?}");
        assert_eq!(actions[0]["op"], json!("chmod"), "{actions:?}");
        assert_eq!(actions[0]["path"], json!(credential), "{actions:?}");
        assert_eq!(actions[0]["before_hash"], json!("mode:600"), "{actions:?}");
        assert_eq!(actions[0]["after_hash"], json!("mode:644"), "{actions:?}");
        assert_eq!(
            actions[0]["tx"],
            json!(undo_tx),
            "the undo's own: {actions:?}"
        );
        let report: Value =
            serde_json::from_slice(&fs::read(dir.join("report.json")).unwrap()).unwrap();
        assert_eq!(report["kind"], json!("undo"), "{report}");
        assert_eq!(report["command"], json!("undo"), "{report}");
        assert_eq!(report["state"], json!("DONE_OK"), "{report}");
        assert_eq!(report["tx"], json!(tx), "{report}");
        assert_eq!(report["reverted_run"], json!(home.run.id), "{report}");
        assert_eq!(report["undo_tx"], json!(undo_tx), "{report}");
        assert_eq!(report["actions_taken"], json!(1), "{report}");
        assert_eq!(report["already_undone"], json!(false), "{report}");
        assert_eq!(
            report["run_id"],
            json!(dir.file_name().unwrap().to_str()),
            "{report}"
        );
        assert_eq!(mode_of(&dir.join("report.json")), 0o400);
        assert_eq!(mode_of(&dir.join(ACTIONS_LOG)), 0o400);
        assert_eq!(mode_of(&dir.join(UNDO_SCRIPT)), 0o500);
        let script = fs::read_to_string(dir.join(UNDO_SCRIPT)).unwrap();
        assert!(
            script.contains("doctor undo")
                && script.contains(dir.file_name().unwrap().to_str().unwrap()),
            "{script}"
        );
        // And the undo is undone in turn: a redo.
        let redo = undone(&home, undo_tx);
        assert_eq!(mode_of(&credential), 0o600, "redone");
        assert_eq!(
            line_of(&home, redo.outcome.tx.unwrap()).reverts,
            Some(undo_tx)
        );
    }

    /// Lowering an undo resolves each quarantine below the undo's own run and reserves
    /// each write's backup there, reads the reverted backup as `backup:<run>/<seq>`, and
    /// creates nothing: the commit makes the roots.
    #[test]
    fn lowered_from_inverses_resolves_destinations_and_reservations() {
        let home = plain_home();
        let tx = repaired(&home, &[write_over(&home), write_create(&home)]);
        let verified = check(&home, tx).unwrap();
        let undo_run = later_run(&home);
        let undo_tx = TxId::new();
        let before = tree_hash(&home.base);
        let lowered = lowered_from_inverses(
            &LowerCtx {
                paths: &home.paths,
                tx: undo_tx,
                run: &undo_run,
                clients: &home.planned,
                skill_dirs: &home.skill_dirs,
                scopes: &home.scopes,
                mount_id: &fsx::mount_id,
            },
            &verified,
        )
        .unwrap();
        assert_eq!(tree_hash(&home.base), before, "nothing created");
        let mut backups = Vec::new();
        let mut destinations = Vec::new();
        for step in &lowered.steps {
            match &step.step.op {
                Primitive::WriteFile { backup, .. } => backups.push(backup.clone()),
                Primitive::RenameNoReplace { to, .. } => destinations.push(to.clone()),
                Primitive::Chmod { .. } => {}
            }
        }
        let seq_of_write = lowered
            .steps
            .iter()
            .find(|s| matches!(s.step.op, Primitive::WriteFile { .. }))
            .map(|s| s.step.seq)
            .unwrap();
        assert_eq!(
            backups,
            [Some(
                undo_run.dir.join("backups").join(seq_of_write.to_string())
            )],
            "the undo's own backup"
        );
        let rel = notes(&home)
            .strip_prefix(home.paths.home())
            .unwrap()
            .to_path_buf();
        let parked = undo_run.dir.join("quarantine/skill").join(&rel);
        assert_eq!(destinations, std::slice::from_ref(&parked));
        assert!(
            lowered
                .roots
                .contains(&parked.parent().unwrap().to_path_buf()),
            "{:?}",
            lowered.roots
        );
        assert!(
            lowered
                .resources
                .reads()
                .contains(&Resource::backup(&home.run.id, 1).unwrap()),
            "{:?}",
            lowered.resources
        );
        assert!(
            lowered.steps.iter().all(|s| s.step.validate().is_ok()),
            "every step validates"
        );
    }

    #[test]
    fn an_overwriting_undo_backs_up_what_it_overwrites() {
        let home = plain_home();
        let original = fs::read(home.skill_file()).unwrap();
        let tx = repaired(&home, &[write_over(&home)]);
        let outcome = undone(&home, tx);
        assert_eq!(fs::read(home.skill_file()).unwrap(), original);
        assert_eq!(mode_of(&home.skill_file()), 0o644);
        let dir = undo_run_dir(&home, &outcome);
        assert_eq!(
            fs::read(dir.join("backups/1")).unwrap(),
            NEW_GUIDE,
            "the undo kept the repaired bytes it overwrote"
        );
    }

    #[test]
    fn a_created_file_goes_to_the_undos_quarantine() {
        let home = plain_home();
        let tx = repaired(&home, &[write_create(&home)]);
        let outcome = undone(&home, tx);
        assert!(!notes(&home).exists(), "the created file left its place");
        let rel = notes(&home)
            .strip_prefix(home.paths.home())
            .unwrap()
            .to_path_buf();
        let parked = undo_run_dir(&home, &outcome)
            .join("quarantine/skill")
            .join(rel);
        assert_eq!(fs::read(&parked).unwrap(), b"notes\n", "never deleted");
    }

    #[test]
    fn a_flatten_is_undone_one_rename_at_a_time() {
        // A case: the flatten, and what must hold once it is undone.
        type Back = fn(&RepairHome, TxId) -> Vec<(&'static str, bool)>;
        let cases: Vec<(&str, &str, Back)> = vec![
            ("Wrap/Inner", "Wrap", |h, _| {
                vec![
                    (
                        "inner back in its wrapper",
                        h.addons("Wrap/Inner/Inner.toc").is_file(),
                    ),
                    (
                        "wrapper's own file back",
                        h.addons("Wrap/README.txt").is_file(),
                    ),
                    ("no lifted folder", !h.addons("Inner").exists()),
                ]
            }),
            ("Same/Same", "Same", |h, tx| {
                vec![
                    (
                        "inner back in its wrapper",
                        h.addons("Same/Same/Same.toc").is_file(),
                    ),
                    (
                        "wrapper's own file back",
                        h.addons("Same/notes.txt").is_file(),
                    ),
                    (
                        "no provisional folder",
                        !h.client.flatten_provisional(&tx).exists(),
                    ),
                ]
            }),
        ];
        for (inner, outer, back) in cases {
            let home = plain_home();
            let tx = repaired(&home, &[flatten(&home, inner, outer)]);
            undone(&home, tx);
            for (what, holds) in back(&home, tx) {
                assert!(holds, "{inner}: {what}");
            }
        }
    }

    #[test]
    fn a_failed_inverse_rolls_the_undo_back() {
        let home = plain_home();
        let tx = repaired(&home, &[flatten(&home, "Wrap/Inner", "Wrap")]);
        let addons = crate::engine::testing::tree_hash(&home.addons(""));
        let runs_before = runs(&home);
        let result = crate::fsx::with_spec("fsx.mutate.executed=error@2", || {
            engine_undo(&home, tx, false, vec![discovered(&home)])
        });
        assert!(result.is_err(), "{result:?}");
        assert_eq!(
            crate::engine::testing::tree_hash(&home.addons("")),
            addons,
            "the subject is as the repair left it"
        );
        let read = history_of(&home);
        assert!(
            !read.entries.iter().any(|e| e.reverts == Some(tx)),
            "no undo line"
        );
        let new: Vec<String> = runs(&home).difference(&runs_before).cloned().collect();
        assert_eq!(new.len(), 1, "{new:?}");
        let report: Value = serde_json::from_slice(
            &fs::read(
                home.paths
                    .doctor_runs_dir()
                    .join(&new[0])
                    .join("report.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(report["state"], json!("DONE_FAILED"), "{report}");
    }

    #[test]
    fn the_reverted_run_directory_is_never_written() {
        let home = plain_home();
        let tx = repaired(&home, &[write_over(&home)]);
        let before = crate::engine::testing::tree_hash(&home.run.dir);
        undone(&home, tx);
        assert_eq!(
            crate::engine::testing::tree_hash(&home.run.dir),
            before,
            "its backups read, never written"
        );
        // A quarantine is the one thing an undo moves out of it.
        let home = home_with(|h| vec![quarantine(h.record(), QuarantineRoot::State)]);
        let (home, tx) = home;
        let backups = home.run.dir.join("backups");
        let had_backups = backups.exists();
        undone(&home, tx);
        assert!(home.record().is_file(), "the record is back");
        assert_eq!(backups.exists(), had_backups, "nothing else changed there");
    }

    /// A fresh home and the repair of `fixes` committed on it.
    fn home_with(fixes: fn(&RepairHome) -> Vec<PlannedFix>) -> (RepairHome, TxId) {
        let home = plain_home();
        let tx = repaired(&home, &fixes(&home));
        (home, tx)
    }

    #[test]
    fn a_dry_run_creates_nothing() {
        let (home, tx) = home_with(|h| vec![chmod(h), write_create(h)]);
        let before = tree_hash(&home.base);
        let outcome = engine_undo(&home, tx, true, vec![discovered(&home)]).unwrap();
        assert_eq!(tree_hash(&home.base), before, "nothing written");
        assert_eq!(outcome.outcome.tx, None);
        assert!(outcome.outcome.dry_run);
        let repair = outcome.repair.unwrap();
        assert_eq!(repair.undo_run, None);
        assert_eq!(repair.plan.plan.inverses.len(), 2);
    }

    #[test]
    fn a_refusal_before_the_lock_creates_no_run() {
        let (home, tx) = home_with(|h| vec![chmod(h)]);
        fs::set_permissions(home.credential(), fs::Permissions::from_mode(0o640)).unwrap();
        let runs_before = runs(&home);
        let journals_before = journals(&home);
        let err = engine_undo(&home, tx, false, vec![discovered(&home)]).unwrap_err();
        assert_eq!(err.code, ErrorCode::UndoConflict, "{err:?}");
        assert_eq!(runs(&home), runs_before, "no run directory");
        assert_eq!(journals(&home), journals_before, "no journal");
        let inflight: Vec<_> = fs::read_dir(home.paths.inflight_dir())
            .map(|d| {
                d.filter_map(std::result::Result::ok)
                    .map(|e| e.file_name())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            inflight.iter().all(|name| name
                .to_str()
                .is_some_and(|n| n.starts_with(&home.stuck().to_string()))),
            "only the stuck journal's in-flight lock: {inflight:?}"
        );
    }

    #[test]
    fn a_target_not_on_this_machine_is_refused() {
        let (home, tx) = home_with(|h| vec![quarantine(h.addons("Stray"), client_root(h))]);
        let before = tree_hash(&home.base);
        let err = engine_undo(&home, tx, false, Vec::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::TargetNotFound, "{err:?}");
        assert_eq!(tree_hash(&home.base), before, "nothing written");
    }

    /// A set-aside's undo reactivates its journal, and the undo of that undo sets it
    /// aside again, though recovery cannot settle the journal it suspends: that journal
    /// is the undo's subject. Any other stuck journal still refuses the undo.
    #[test]
    fn an_undo_of_a_reactivation_is_not_blocked_by_the_journal_it_suspends() {
        let home = home();
        let stuck = home.stuck();
        let tx = repaired(&home, &[stuck_set_aside(&home, Vec::new())]);
        assert!(!home.paths.journal(&stuck).exists(), "set aside");
        let reactivation = undone(&home, tx);
        assert!(home.paths.journal(&stuck).is_file(), "reactivated");
        let redo = engine_undo(
            &home,
            reactivation.outcome.tx.unwrap(),
            false,
            vec![discovered(&home)],
        )
        .unwrap_or_else(|err| panic!("{err:?}")); // ubs:ignore — fails the test
        assert!(!home.paths.journal(&stuck).exists(), "set aside again");
        let pending: Vec<&Warning> = redo
            .outcome
            .warnings
            .iter()
            .filter(|w| w.code == WarningCode::JournalPending)
            .collect();
        assert_eq!(pending.len(), 1, "{:?}", redo.outcome.warnings);
    }

    /// A set-aside's round trip three deep: set aside, reactivated (its undo), set aside
    /// again (the undo of that), and reactivated again (the undo of the redo), the journal
    /// back under its own name each time it should be, no provisional name left.
    #[test]
    fn a_set_aside_round_trips_three_deep() {
        let home = home();
        let stuck = home.stuck();
        let provisional = home
            .paths
            .journal_dir()
            .join(format!("{stuck}.reactivating.jsonl"));
        let tx = repaired(&home, &[stuck_set_aside(&home, Vec::new())]);
        let reactivation = undone(&home, tx).outcome.tx.unwrap();
        assert!(home.paths.journal(&stuck).is_file(), "reactivated");
        let redo = undone(&home, reactivation).outcome.tx.unwrap();
        assert!(!home.paths.journal(&stuck).exists(), "set aside again");
        let plan = plan_of(&home, redo);
        let shown: Vec<(InverseKind, PathBuf, Option<PathBuf>)> = plan
            .inverses
            .iter()
            .map(|i| (i.kind, i.path.clone(), i.to.path().map(Path::to_path_buf)))
            .collect();
        let again = engine_undo(&home, redo, false, vec![discovered(&home)]);
        assert!(
            again.is_ok(),
            "the undo of the redo: {again:?}\nits inverses: {shown:#?}"
        );
        assert!(home.paths.journal(&stuck).is_file(), "reactivated again");
        assert!(!provisional.exists(), "no provisional name left");
    }

    #[test]
    fn another_stuck_journal_still_blocks_an_undo() {
        let home = home();
        let stuck = home.stuck();
        let tx = repaired(&home, &[stuck_set_aside(&home, Vec::new())]);
        let reactivation = undone(&home, tx);
        assert!(home.paths.journal(&stuck).is_file());
        // Another journal no build reads, which the undo does not move.
        let other = TxId::new();
        fs::write(home.paths.journal(&other), "{\"line\":\"prepared\"}\n").unwrap();
        let runs_before = runs(&home);
        let err = engine_undo(
            &home,
            reactivation.outcome.tx.unwrap(),
            false,
            vec![discovered(&home)],
        )
        .unwrap_err();
        assert!(
            home.paths.journal(&stuck).is_file(),
            "nothing moved: {err:?}"
        );
        assert_eq!(
            err.details["journal_pending"],
            json!(other),
            "refused for the other journal: {err:?}"
        );
        assert_eq!(err.code.check_details(&err.details), Ok(()), "{err:?}");
        // Refused under the lock, after its run was made: that run says so.
        let new: Vec<String> = runs(&home).difference(&runs_before).cloned().collect();
        assert_eq!(new.len(), 1, "{new:?}");
        let report: Value = serde_json::from_slice(
            &fs::read(
                home.paths
                    .doctor_runs_dir()
                    .join(&new[0])
                    .join("report.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(report["state"], json!("REFUSING"), "{report}");
    }
}
