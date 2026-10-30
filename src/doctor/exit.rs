//! The doctor's exit dictionary (vision D18, M4-D40): every `doctor` command exits
//! through it, never through the general [`crate::error::Exit`], because findings are a
//! normal outcome there and not a failure.
//!
//! [`exit_for`] maps every error code into it. The one exit no code gives is 73: the
//! runtime's failure to create its run directory names it explicitly
//! (`DoctorFailure::cannot_create`), since an `io` error elsewhere is 74.

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::{self, ErrorCode};

/// How a `doctor` command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Exit {
    Healthy,
    FindingsPresent,
    FixPartial,
    FixFailedRolledBack,
    RefusedUnsafe,
    ConcurrencyLost,
    OnlineRequired,
    UsageError,
    NoInput,
    CannotCreate,
    IoError,
}

impl Exit {
    /// In code order.
    pub const ALL: &[Exit] = &[
        Self::Healthy,
        Self::FindingsPresent,
        Self::FixPartial,
        Self::FixFailedRolledBack,
        Self::RefusedUnsafe,
        Self::ConcurrencyLost,
        Self::OnlineRequired,
        Self::UsageError,
        Self::NoInput,
        Self::CannotCreate,
        Self::IoError,
    ];

    /// The process exit status.
    pub const fn code(self) -> u8 {
        match self {
            Self::Healthy => 0,
            Self::FindingsPresent => 1,
            Self::FixPartial => 2,
            Self::FixFailedRolledBack => 3,
            Self::RefusedUnsafe => 4,
            Self::ConcurrencyLost => 5,
            Self::OnlineRequired => 6,
            Self::UsageError => 64,
            Self::NoInput => 66,
            Self::CannotCreate => 73,
            Self::IoError => 74,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::FindingsPresent => "findings_present",
            Self::FixPartial => "fix_partial",
            Self::FixFailedRolledBack => "fix_failed_rolled_back",
            Self::RefusedUnsafe => "refused_unsafe",
            Self::ConcurrencyLost => "concurrency_lost",
            Self::OnlineRequired => "online_required",
            Self::UsageError => "usage_error",
            Self::NoInput => "no_input",
            Self::CannotCreate => "cannot_create",
            Self::IoError => "io_error",
        }
    }

    pub const fn meaning(self) -> &'static str {
        match self {
            Self::Healthy => "every selected check ran to completion and found nothing",
            Self::FindingsPresent => {
                "the run completed and reports findings, a check that could not finish among them"
            }
            Self::FixPartial => "a fix ran and some findings remain",
            Self::FixFailedRolledBack => "a fix failed and was rolled back; nothing changed",
            Self::RefusedUnsafe => {
                "refused before changing anything: a safety precondition, a missing --force, a \
                 conflict, a stale preview or an over-budget selection"
            }
            Self::ConcurrencyLost => {
                "another omaddons process held the lock past the acquire timeout"
            }
            Self::OnlineRequired => "the selection needs the network: add --online",
            Self::UsageError => "unknown command or flag, or a bad argument",
            Self::NoInput => {
                "a target, run, failure mode or other input was not found, or is ambiguous"
            }
            Self::CannotCreate => "the run directory could not be created",
            Self::IoError => "a runtime failure: I/O, state, network or internal",
        }
    }
}

/// The doctor exit of a failure with `code`: its own row first, then its general class.
/// Total: the inner match names every general exit, so a new class does not compile until
/// it is mapped here.
pub const fn exit_for(code: ErrorCode) -> Exit {
    match code {
        // The run did nothing because the world moved since its preview.
        ErrorCode::PlanStale => Exit::RefusedUnsafe,
        ErrorCode::CheckFailed => Exit::FindingsPresent,
        _ => match code.exit() {
            error::Exit::Usage => Exit::UsageError,
            error::Exit::NotFound => Exit::NoInput,
            error::Exit::Refused => Exit::RefusedUnsafe,
            error::Exit::Locked => Exit::ConcurrencyLost,
            error::Exit::Failed => Exit::IoError,
            error::Exit::CheckFailed => Exit::FindingsPresent,
            // No error code exits 0 (a registry test holds it); were one to, the
            // conservative failure is the answer, never `healthy`.
            error::Exit::Ok => Exit::IoError,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn codes_and_names_are_the_visions() {
        let table = [
            (Exit::Healthy, 0, "healthy"),
            (Exit::FindingsPresent, 1, "findings_present"),
            (Exit::FixPartial, 2, "fix_partial"),
            (Exit::FixFailedRolledBack, 3, "fix_failed_rolled_back"),
            (Exit::RefusedUnsafe, 4, "refused_unsafe"),
            (Exit::ConcurrencyLost, 5, "concurrency_lost"),
            (Exit::OnlineRequired, 6, "online_required"),
            (Exit::UsageError, 64, "usage_error"),
            (Exit::NoInput, 66, "no_input"),
            (Exit::CannotCreate, 73, "cannot_create"),
            (Exit::IoError, 74, "io_error"),
        ];
        let order: Vec<Exit> = table.iter().map(|(exit, _, _)| *exit).collect();
        assert_eq!(Exit::ALL, order.as_slice(), "ALL is in code order");
        let mut names = BTreeSet::new();
        for (exit, code, name) in table {
            assert_eq!(
                (exit.code(), exit.name()),
                (code, name),
                "{exit:?}: code and name"
            );
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_') && !name.starts_with('_'),
                "{name} is snake_case"
            );
            assert!(names.insert(name), "{name} is listed twice");
            assert!(!exit.meaning().is_empty(), "{name} has a meaning");
            assert_eq!(
                serde_json::to_value(exit).unwrap(),
                serde_json::Value::from(name),
                "{name}: wire name"
            );
        }
    }

    #[test]
    fn exit_for_is_total_over_every_error_code() {
        // Restated from the rule, independently of `exit_for`'s match.
        let expected = |code: ErrorCode| match (code, code.exit().code()) {
            (ErrorCode::PlanStale, _) => Exit::RefusedUnsafe,
            (ErrorCode::CheckFailed, _) | (_, 6) => Exit::FindingsPresent,
            (_, 2) => Exit::UsageError,
            (_, 3) => Exit::NoInput,
            (_, 4) => Exit::RefusedUnsafe,
            (_, 5) => Exit::ConcurrencyLost,
            _ => Exit::IoError,
        };
        for &code in ErrorCode::ALL {
            assert_eq!(
                exit_for(code),
                expected(code),
                "{}: general exit {} ({}), doctor exit {:?}, expected {:?}",
                code.wire(),
                code.exit().code(),
                code.exit().name(),
                exit_for(code),
                expected(code)
            );
        }
    }

    #[test]
    fn exit_for_classes() {
        let cases = [
            ("usage", ErrorCode::Usage, Exit::UsageError),
            ("ref_invalid", ErrorCode::RefInvalid, Exit::UsageError),
            (
                "desired_state_invalid",
                ErrorCode::DesiredStateInvalid,
                Exit::UsageError,
            ),
            ("target_not_found", ErrorCode::TargetNotFound, Exit::NoInput),
            ("plan_not_found", ErrorCode::PlanNotFound, Exit::NoInput),
            ("game_running", ErrorCode::GameRunning, Exit::RefusedUnsafe),
            (
                "undo_conflict",
                ErrorCode::UndoConflict,
                Exit::RefusedUnsafe,
            ),
            ("plan_stale", ErrorCode::PlanStale, Exit::RefusedUnsafe),
            ("locked", ErrorCode::Locked, Exit::ConcurrencyLost),
            (
                "check_failed",
                ErrorCode::CheckFailed,
                Exit::FindingsPresent,
            ),
            ("io", ErrorCode::Io, Exit::IoError),
            ("internal", ErrorCode::Internal, Exit::IoError),
            ("network", ErrorCode::Network, Exit::IoError),
            ("hash_mismatch", ErrorCode::HashMismatch, Exit::IoError),
            ("state_corrupt", ErrorCode::StateCorrupt, Exit::IoError),
        ];
        for (name, code, exit) in cases {
            assert_eq!(code.wire(), name, "the case names its code");
            assert_eq!(
                exit_for(code),
                exit,
                "{name} (general exit {}) → {exit:?}",
                code.exit().code()
            );
        }
    }

    #[test]
    fn no_code_maps_to_healthy() {
        let healthy: Vec<&str> = ErrorCode::ALL
            .iter()
            .filter(|code| exit_for(**code) == Exit::Healthy)
            .map(|code| code.wire())
            .collect();
        assert!(healthy.is_empty(), "errors that exit healthy: {healthy:?}");
    }
}
