//! Undoing and finishing a repair's steps (E5): the one routine that rolls a repair back,
//! in process when a step fails and in every lock-taking command that recovers one, and
//! the one that rolls it forward once every step ran. So the crash matrix exercises the
//! same code a failed commit runs.
//!
//! **Classifying a step.** Each step whose `step` line was written, newest first, is
//! compared with the live paths: each endpoint's kind and identity, plus the one part
//! its primitive changes (a write's content digest, a chmod's mode). A rename's
//! directory stamp is left out: it says nothing about whether the rename ran. `post`
//! holds: it ran, and is inverted. `pre` holds: it never ran. Neither: the walk stops
//! there, nothing more is inverted, and the journal is left for manual repair
//! (`step_unrecognized`).
//!
//! **Inverting a step**, through fsx with the step's `post` as the expectation:
//!
//! - a write over a file: the reserved backup, checked against `pre`'s digest, written
//!   back with `pre`'s mode;
//! - a write that created its file: renamed to its rollback destination in its side's
//!   quarantine, never deleted;
//! - a chmod: `pre`'s mode back;
//! - a rename: back, never replacing anything.

use std::io;
use std::path::Path;

use serde_json::json;

use super::journal::{RepairLine, RepairPrepared, history_line};
use crate::engine::journal::StopReason;
use crate::engine::repair_action::{Endpoint, Endpoints, Identity, Primitive, Step};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{
    self, Content, Expect, FileId, FileKind, Mismatch, MutateOpts, Op, Predicate, RenameMode,
    StampDigest, WriteSession, failpoint, mutate_with,
};
use crate::model::TxId;
use crate::state::history::{self, HistoryFilter};
use crate::state::paths::Paths;

/// What classifying a repair's steps reads: the disk ([`OnDisk`]), or a simulation of it
/// (`journal::diagnose`).
pub(crate) trait RepairView {
    /// The first part of `expect` that does not hold at `path`, if any.
    fn check(&mut self, path: &Path, expect: &Expect) -> io::Result<Option<Mismatch>>;
}

/// What a step's live paths say about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Classified {
    /// Its `post` holds.
    Ran,
    /// Its `pre` holds.
    NotRun,
    /// Neither: something else changed its paths.
    Neither,
}

/// The paths a step's endpoints describe, each with its endpoint.
fn sides<'s>(step: &'s Step, endpoints: &'s Endpoints) -> Vec<(&'s Path, &'s Endpoint)> {
    match (&step.op, endpoints) {
        (Primitive::RenameNoReplace { from, to, .. }, Endpoints::Pair { path, to: dest }) => {
            vec![(from.as_path(), path), (to.as_path(), dest)]
        }
        (op, Endpoints::One(endpoint)) => vec![(op.path(), endpoint)],
        (op, Endpoints::Pair { path, .. }) => vec![(op.path(), path)],
    }
}

/// Whether `step` ran, never ran, or neither, as `view` reads its paths.
pub(crate) fn classify(view: &mut dyn RepairView, step: &Step) -> io::Result<Classified> {
    let mut holds = |endpoints: &Endpoints| -> io::Result<bool> {
        for (path, endpoint) in sides(step, endpoints) {
            if view.check(path, &decisive(step, endpoint))?.is_some() {
                return Ok(false);
            }
        }
        Ok(true)
    };
    Ok(if holds(&step.post)? {
        Classified::Ran
    } else if holds(&step.pre)? {
        Classified::NotRun
    } else {
        Classified::Neither
    })
}

/// The parts of `endpoint` that tell whether `step` ran: its kind, its identity, and the
/// one part the primitive changes.
fn decisive(step: &Step, endpoint: &Endpoint) -> Expect {
    let identity = endpoint.identity().map(file_id);
    let (file_type, sha256, mode) = match endpoint {
        Endpoint::Absent => return Expect::Absent,
        Endpoint::File { sha256, mode, .. } => (
            FileKind::Regular,
            matches!(step.op, Primitive::WriteFile { .. })
                .then_some(*sha256)
                .flatten()
                .map(|digest| digest.0),
            matches!(step.op, Primitive::Chmod { .. })
                .then_some(*mode)
                .flatten()
                .map(|mode| mode.bits()),
        ),
        Endpoint::Dir { .. } => (FileKind::Directory, None, None),
        Endpoint::Symlink { .. } => (FileKind::Symlink, None, None),
    };
    Expect::Present(Predicate {
        file_type: Some(file_type),
        identity,
        sha256,
        mode,
        ..Predicate::default()
    })
}

/// Every part `endpoint` records, as fsx checks it immediately before an operation.
pub(crate) fn expect_of(endpoint: &Endpoint) -> Expect {
    let identity = endpoint.identity().map(file_id);
    Expect::Present(match endpoint {
        Endpoint::Absent => return Expect::Absent,
        Endpoint::File { sha256, mode, .. } => Predicate {
            file_type: Some(FileKind::Regular),
            identity,
            sha256: sha256.map(|digest| digest.0),
            mode: mode.map(|mode| mode.bits()),
            ..Predicate::default()
        },
        Endpoint::Dir { stamp, .. } => Predicate {
            file_type: Some(FileKind::Directory),
            identity,
            stamp: stamp.map(|stamp| StampDigest::from_bytes(stamp.0)),
            ..Predicate::default()
        },
        Endpoint::Symlink { link_target, .. } => Predicate {
            file_type: Some(FileKind::Symlink),
            identity,
            link_target: link_target.clone(),
            ..Predicate::default()
        },
    })
}

/// An identity as fsx compares it: the device is never compared ([`FileId::same`]).
fn file_id(identity: Identity) -> FileId {
    FileId {
        dev: 0,
        ino: identity.ino,
        born_ns: identity.born_ns,
    }
}

/// Why a rollback walk stopped, with the journal kept for the next command that
/// recovers it.
#[derive(Debug)]
pub(crate) enum Stopped {
    /// Journal step `seq` is one recovery cannot place: in neither its before nor its
    /// after state, or not invertible as recorded (`why`).
    Unrecognized { seq: u32, why: String },
    /// Reading or writing failed (an I/O error, or fsx's refusal of an inverse).
    Failed(Error),
}

impl Stopped {
    /// The stop as the error recovery and the commit report: `internal` with
    /// `step_unrecognized`, or the failure itself.
    pub(crate) fn into_error(self, prepared: &RepairPrepared) -> Error {
        match self {
            Self::Unrecognized { seq, why } => unrecognized(prepared, seq, &why),
            Self::Failed(error) => error,
        }
    }
}

/// What a repair's rollback walks: the disk ([`OnDisk`]: recovery and the in-process
/// commit), or a simulation of it (`journal::diagnose`). Classifying reads it and
/// [`inverse`] decides from it; only applying the decided inverse differs.
pub(crate) trait RepairReplay: RepairView {
    /// Applies `inverse`, decided for `step`, which ran.
    fn apply(&mut self, step: &Step, inverse: &Inverse<'_>) -> Result<()>;
}

/// Inverts every step `1..=recorded` that ran, newest first, and returns how many it
/// inverted. A step in neither state, or one its journal records too little to invert,
/// stops the walk (`Stopped::Unrecognized`); a read or an inverse that fails stops it
/// with its error. Either way nothing more is inverted.
pub(crate) fn roll_back(
    replay: &mut dyn RepairReplay,
    prepared: &RepairPrepared,
    recorded: u32,
) -> std::result::Result<u32, Stopped> {
    let mut inverted = 0;
    for step in prepared
        .primitives
        .iter()
        .filter(|step| step.seq <= recorded)
        .rev()
    {
        let classified =
            classify(replay, step).map_err(|err| Stopped::Failed(step_io(step, &err)))?;
        match classified {
            Classified::NotRun => continue,
            Classified::Neither if undone_before(replay, prepared, step)? => continue,
            Classified::Neither => {
                return Err(Stopped::Unrecognized {
                    seq: step.seq,
                    why: "is in neither its before nor its after state".to_owned(),
                });
            }
            Classified::Ran => {}
        }
        let inverse = inverse(replay, prepared, step)?;
        replay.apply(step, &inverse).map_err(Stopped::Failed)?;
        inverted += 1;
    }
    Ok(inverted)
}

/// Whether a step before `step` is in its before state: steps run first to last and
/// roll back last first, so `step` then never ran, or was undone already, and its paths
/// may hold neither of its states (a three-rename flatten's last step names the
/// provisional folder its first step makes, so once a recovery a crash interrupted has
/// undone all three, it is in neither). The folder family's rule too
/// (`journal::step_state_after`).
fn undone_before(
    view: &mut dyn RepairView,
    prepared: &RepairPrepared,
    step: &Step,
) -> std::result::Result<bool, Stopped> {
    for earlier in prepared.primitives.iter().filter(|e| e.seq < step.seq) {
        let classified =
            classify(view, earlier).map_err(|err| Stopped::Failed(step_io(earlier, &err)))?;
        if classified == Classified::NotRun {
            return Ok(true);
        }
    }
    Ok(false)
}

/// How a step that ran is undone, decided from the journal and the view alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inverse<'p> {
    /// The reserved `backup`, which holds the bytes (`sha256`) the write replaced, written
    /// back at `path` with `mode`.
    Restore {
        path: &'p Path,
        backup: &'p Path,
        sha256: [u8; 32],
        mode: Option<u32>,
    },
    /// The file the write created, renamed to its rollback destination `to`.
    Unmake { path: &'p Path, to: &'p Path },
    /// The mode the chmod replaced, back on `path`.
    Chmod { path: &'p Path, mode: u32 },
    /// The rename, back from its destination `from` to its source `to`.
    RenameBack {
        from: &'p Path,
        to: &'p Path,
        symlink: bool,
    },
}

/// The inverse of `step`, which ran. A write over a file is undone only from a backup
/// that holds the bytes it replaced (checked through `view`); a step whose journal does
/// not record what its inverse needs is `Stopped::Unrecognized`.
pub(crate) fn inverse<'p>(
    view: &mut dyn RepairView,
    prepared: &'p RepairPrepared,
    step: &'p Step,
) -> std::result::Result<Inverse<'p>, Stopped> {
    let unrecognized = |why: &str| Stopped::Unrecognized {
        seq: step.seq,
        why: why.to_owned(),
    };
    match (&step.op, &step.pre, &step.post) {
        (
            Primitive::WriteFile {
                path,
                backup: Some(backup),
                ..
            },
            Endpoints::One(pre),
            Endpoints::One(_),
        ) => {
            let Endpoint::File {
                sha256: Some(old),
                mode,
                ..
            } = pre
            else {
                return Err(unrecognized("overwrote no file it recorded"));
            };
            let held = view
                .check(backup, &Expect::hash(old.0))
                .map_err(|err| Stopped::Failed(step_io(step, &err)))?;
            if held.is_some() {
                return Err(unrecognized(&format!(
                    "has a backup, {}, that does not hold the bytes it replaced",
                    backup.display()
                )));
            }
            Ok(Inverse::Restore {
                path,
                backup,
                sha256: old.0,
                mode: mode.map(|mode| mode.bits()),
            })
        }
        (Primitive::WriteFile { path, .. }, Endpoints::One(_), Endpoints::One(_)) => {
            match prepared.rollbacks.iter().find(|r| r.seq == step.seq) {
                Some(target) => Ok(Inverse::Unmake {
                    path,
                    to: &target.to,
                }),
                None => Err(unrecognized(
                    "created a file that has no rollback destination",
                )),
            }
        }
        (Primitive::Chmod { path, .. }, Endpoints::One(pre), Endpoints::One(_)) => match pre {
            Endpoint::File {
                mode: Some(old), ..
            } => Ok(Inverse::Chmod {
                path,
                mode: old.bits(),
            }),
            _ => Err(unrecognized("recorded no mode to put back")),
        },
        (
            Primitive::RenameNoReplace { from, to, .. },
            Endpoints::Pair { .. },
            Endpoints::Pair { to: arrived, .. },
        ) => Ok(Inverse::RenameBack {
            from: to,
            to: from,
            symlink: matches!(arrived, Endpoint::Symlink { .. }),
        }),
        _ => Err(unrecognized("has endpoints that do not fit its operation")),
    }
}

/// The disk: classifying reads it through fsx's own predicate check (E13), and an
/// inverse is written through `s` (an untracked `Writer::Doctor` session) with the
/// step's `post` as its expectation, never replacing anything a rename lands on.
pub(crate) struct OnDisk<'s, 'a> {
    pub s: &'s mut WriteSession<'a>,
}

impl RepairView for OnDisk<'_, '_> {
    fn check(&mut self, path: &Path, expect: &Expect) -> io::Result<Option<Mismatch>> {
        expect.check(path)
    }
}

impl RepairReplay for OnDisk<'_, '_> {
    fn apply(&mut self, step: &Step, inverse: &Inverse<'_>) -> Result<()> {
        let post = match &step.post {
            Endpoints::One(post) => post,
            Endpoints::Pair { to, .. } => to,
        };
        let expect = MutateOpts {
            expect: Some(expect_of(post)),
            ..MutateOpts::default()
        };
        let s = &mut *self.s;
        let done = match *inverse {
            Inverse::Restore {
                path, backup, mode, ..
            } => {
                let bytes = std::fs::read(backup).map_err(|err| step_io(step, &err))?;
                mutate_with(
                    s,
                    path,
                    Op::WriteFile {
                        content: Content::Bytes(&bytes),
                        mode,
                    },
                    expect,
                )
            }
            Inverse::Unmake { path, to } => mutate_with(
                s,
                path,
                Op::Rename {
                    to,
                    mode: RenameMode::NoReplace,
                },
                MutateOpts {
                    expect_to: Some(Expect::Absent),
                    ..expect
                },
            ),
            Inverse::Chmod { path, mode } => mutate_with(s, path, Op::Chmod { mode }, expect),
            Inverse::RenameBack { from, to, symlink } => mutate_with(
                s,
                from,
                Op::Rename {
                    to,
                    mode: RenameMode::NoReplace,
                },
                MutateOpts {
                    expect_to: Some(Expect::Absent),
                    temp_symlink: symlink,
                    ..expect
                },
            ),
        };
        done.map_err(Error::from)?;
        failpoint::hit("doctor.recover.step").map_err(|err| step_io(step, &err))
    }
}

/// Finishes a repair whose every step ran: its history line, appended only when the
/// history holds no line of its transaction yet (a commit or a recovery may have written
/// it before stopping), then `committed`, unless the journal ends in a torn line that an
/// append would run into.
pub(crate) fn roll_forward(
    s: &mut WriteSession<'_>,
    paths: &Paths,
    journal: &Path,
    prepared: &RepairPrepared,
    torn_tail: bool,
) -> Result<()> {
    let text = history::text(paths)?;
    let written = history::parse(&text, &HistoryFilter::default())
        .entries
        .iter()
        .any(|entry| entry.tx == prepared.tx);
    if !written {
        history::append(s, paths, &history_line(prepared))?;
    }
    if !torn_tail {
        append(s, journal, &RepairLine::Committed)?;
    }
    Ok(())
}

/// Appends one journal line, fsync'd before it returns.
pub(crate) fn append(s: &mut WriteSession<'_>, journal: &Path, line: &RepairLine) -> Result<()> {
    let mut bytes = serde_json::to_vec(line).map_err(|err| {
        Error::internal(format!("a repair journal line does not serialize: {err}"))
    })?;
    bytes.push(b'\n');
    fsx::mutate(s, journal, Op::AppendFile { content: &bytes })
        .map(drop)
        .map_err(Error::from)
}

/// The stop for a step recovery cannot place: `internal`, with the journal kept for
/// manual repair. E11's convention names `journal step <k>`, and the reason is in the
/// text too, since `recover_under` turns the stop into a warning, which has no details.
/// A repair names no single target (`target: null`).
fn unrecognized(prepared: &RepairPrepared, seq: u32, why: &str) -> Error {
    Error::new(
        ErrorCode::Internal,
        format!(
            "repair transaction {} could not be recovered ({}): journal step {seq} {why}; recovery stopped and keeps everything",
            prepared.tx,
            StopReason::StepUnrecognized.as_str(),
        ),
        manual_repair(),
    )
    .details(json!({
        "journal_pending": prepared.tx,
        "reason": StopReason::StepUnrecognized.as_str(),
        "target": null,
    }))
}

/// The fix of a repair journal recovery stopped for: advice, since no command of this
/// build settles it.
pub(crate) fn manual_repair() -> Suggestion {
    Suggestion::text(
        "see \"journal_pending that does not clear\" under Troubleshooting in the README",
    )
}

/// `err` with `details.journal_pending` naming `tx`, whose journal is kept.
pub(crate) fn pending(mut err: Error, tx: TxId) -> Error {
    if !err.details.is_object() {
        err.details = json!({});
    }
    if let Some(details) = err.details.as_object_mut() {
        details.insert("journal_pending".into(), tx.to_string().into());
    }
    err
}

/// An I/O failure while classifying or inverting a step: `io`, the journal kept (its
/// caller adds `details.journal_pending`).
fn step_io(step: &Step, err: &io::Error) -> Error {
    Error::io(
        step.op.path(),
        err,
        Suggestion::command(["omaddons", "status"]),
    )
}
