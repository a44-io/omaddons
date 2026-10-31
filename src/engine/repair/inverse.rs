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
use crate::state::history::{self, HistoryFilter};
use crate::state::paths::Paths;

/// What classifying a repair's steps reads: the disk ([`OnDisk`]), or a simulation of it.
pub(crate) trait RepairView {
    /// The first part of `expect` that does not hold at `path`, if any.
    fn check(&mut self, path: &Path, expect: &Expect) -> io::Result<Option<Mismatch>>;
}

/// The disk, read through fsx's own predicate check (E13).
pub(crate) struct OnDisk;

impl RepairView for OnDisk {
    fn check(&mut self, path: &Path, expect: &Expect) -> io::Result<Option<Mismatch>> {
        expect.check(path)
    }
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

/// Inverts every step `1..=recorded` that ran, newest first, through `s` (an untracked
/// `Writer::Doctor` session), and returns how many it inverted. A step in neither state
/// stops the walk with `internal` (`step_unrecognized`); an inverse that fails stops it
/// with its error. Either way the journal stays for the next command that recovers it.
pub(crate) fn roll_back(
    s: &mut WriteSession<'_>,
    view: &mut dyn RepairView,
    prepared: &RepairPrepared,
    recorded: u32,
) -> Result<u32> {
    let mut inverted = 0;
    for step in prepared
        .primitives
        .iter()
        .filter(|step| step.seq <= recorded)
        .rev()
    {
        let classified = classify(view, step).map_err(|err| step_io(prepared, step, &err))?;
        match classified {
            Classified::NotRun => continue,
            Classified::Neither => {
                return Err(unrecognized(
                    prepared,
                    step,
                    "neither its before nor its after state holds",
                ));
            }
            Classified::Ran => {}
        }
        invert(s, prepared, step)?;
        inverted += 1;
        failpoint::hit("doctor.recover.step").map_err(|err| step_io(prepared, step, &err))?;
    }
    Ok(inverted)
}

fn invert(s: &mut WriteSession<'_>, prepared: &RepairPrepared, step: &Step) -> Result<()> {
    let (Endpoints::One(pre), Endpoints::One(post)) = (&step.pre, &step.post) else {
        return invert_rename(s, prepared, step);
    };
    let expect = MutateOpts {
        expect: Some(expect_of(post)),
        ..MutateOpts::default()
    };
    let done = match &step.op {
        Primitive::WriteFile {
            path,
            backup: Some(backup),
            ..
        } => {
            let Endpoint::File {
                sha256: Some(old),
                mode: old_mode,
                ..
            } = pre
            else {
                return Err(unrecognized(
                    prepared,
                    step,
                    "it overwrote no file it recorded",
                ));
            };
            if Expect::hash(old.0)
                .check(backup)
                .map_err(|err| step_io(prepared, step, &err))?
                .is_some()
            {
                return Err(unrecognized(
                    prepared,
                    step,
                    &format!(
                        "its backup {} does not hold the bytes it replaced",
                        backup.display()
                    ),
                ));
            }
            let bytes = std::fs::read(backup).map_err(|err| step_io(prepared, step, &err))?;
            mutate_with(
                s,
                path,
                Op::WriteFile {
                    content: Content::Bytes(&bytes),
                    mode: old_mode.map(|mode| mode.bits()),
                },
                expect,
            )
        }
        Primitive::WriteFile { path, .. } => {
            let Some(target) = prepared.rollbacks.iter().find(|r| r.seq == step.seq) else {
                return Err(unrecognized(
                    prepared,
                    step,
                    "the file it created has no rollback destination",
                ));
            };
            mutate_with(
                s,
                path,
                Op::Rename {
                    to: &target.to,
                    mode: RenameMode::NoReplace,
                },
                MutateOpts {
                    expect_to: Some(Expect::Absent),
                    ..expect
                },
            )
        }
        Primitive::Chmod { path, .. } => {
            let Endpoint::File {
                mode: Some(old), ..
            } = pre
            else {
                return Err(unrecognized(
                    prepared,
                    step,
                    "it recorded no mode to put back",
                ));
            };
            mutate_with(s, path, Op::Chmod { mode: old.bits() }, expect)
        }
        Primitive::RenameNoReplace { .. } => return invert_rename(s, prepared, step),
    };
    done.map(drop).map_err(Error::from)
}

/// A rename back from its destination to its source, never replacing anything there.
fn invert_rename(s: &mut WriteSession<'_>, prepared: &RepairPrepared, step: &Step) -> Result<()> {
    let (Primitive::RenameNoReplace { from, to, .. }, Endpoints::Pair { to: arrived, .. }) =
        (&step.op, &step.post)
    else {
        return Err(unrecognized(
            prepared,
            step,
            "its endpoints do not fit its operation",
        ));
    };
    mutate_with(
        s,
        to,
        Op::Rename {
            to: from,
            mode: RenameMode::NoReplace,
        },
        MutateOpts {
            expect: Some(expect_of(arrived)),
            expect_to: Some(Expect::Absent),
            temp_symlink: matches!(arrived, Endpoint::Symlink { .. }),
            ..MutateOpts::default()
        },
    )
    .map(drop)
    .map_err(Error::from)
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
/// manual repair (E11's convention, naming `journal step <k>`).
fn unrecognized(prepared: &RepairPrepared, step: &Step, why: &str) -> Error {
    Error::new(
        ErrorCode::Internal,
        format!(
            "repair {}: journal step {} cannot be rolled back: {why}; the journal is kept for manual repair",
            prepared.tx, step.seq
        ),
        Suggestion::command(["omaddons", "status"]),
    )
    .details(json!({
        "journal_pending": prepared.tx,
        "reason": StopReason::StepUnrecognized.as_str(),
    }))
}

/// An I/O failure while classifying or inverting a step: `io`, the journal kept.
fn step_io(prepared: &RepairPrepared, step: &Step, err: &io::Error) -> Error {
    let mut error = Error::io(
        step.op.path(),
        err,
        Suggestion::command(["omaddons", "status"]),
    );
    if let Some(details) = error.details.as_object_mut() {
        details.insert("journal_pending".into(), prepared.tx.to_string().into());
    }
    error
}
