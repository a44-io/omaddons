//! The stdout envelope (vision §5.9, D17): one shape for success and failure, with
//! keys in declaration order. Data is serialized generically, never through
//! `serde_json::Value`, which would sort its keys. The doctor tree has its own
//! ([`DoctorEnvelope`], M4-D34).

use schemars::JsonSchema;
use serde::Serialize;

use crate::doctor::exit::{Exit as DoctorExit, exit_for};
use crate::error::{CommandHint, Error, ErrorBody, Warning};

/// The envelope's own version.
pub const SCHEMA_VERSION: &str = "1.0";

/// Volatile per-run facts; golden diffs strip this object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Meta {
    pub elapsed_ms: u64,
    pub offline: bool,
}

/// One stdout document.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(rename = "Envelope")]
pub struct Envelope<'a, D: Serialize> {
    pub ok: bool,
    pub schema_version: &'static str,
    pub tool_version: &'static str,
    pub command: &'a str,
    pub exit_code: u8,
    /// Present whenever the command produced data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<bool>,
    /// Omitted on a failure, except an exit-6 assertion failure (`--check`, `--strict`),
    /// which keeps it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<&'a D>,
    /// Present iff `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    pub warnings: &'a [Warning],
    pub commands: &'a [CommandHint],
    pub meta: Meta,
}

impl<'a, D: Serialize> Envelope<'a, D> {
    /// A successful (possibly partial) run: exit 0.
    pub fn success(
        command: &'a str,
        data: &'a D,
        partial: bool,
        warnings: &'a [Warning],
        commands: &'a [CommandHint],
        meta: Meta,
    ) -> Self {
        Self {
            ok: true,
            schema_version: SCHEMA_VERSION,
            tool_version: env!("CARGO_PKG_VERSION"),
            command,
            exit_code: 0,
            partial: Some(partial),
            data: Some(data),
            error: None,
            warnings,
            commands,
            meta,
        }
    }

    /// An opt-in assertion failed (`--check`, `--strict`): `ok: false`, exit 6, and the
    /// data stays populated.
    pub fn check_failed(
        command: &'a str,
        data: &'a D,
        partial: bool,
        error: &Error,
        warnings: &'a [Warning],
        commands: &'a [CommandHint],
        meta: Meta,
    ) -> Self {
        Self {
            ok: false,
            exit_code: error.exit().code(),
            error: Some(ErrorBody::from(error.clone())),
            ..Self::success(command, data, partial, warnings, commands, meta)
        }
    }
}

impl<'a> Envelope<'a, ()> {
    /// A failed run: no data, the error's exit code.
    pub fn failure(
        command: &'a str,
        error: &Error,
        warnings: &'a [Warning],
        commands: &'a [CommandHint],
        meta: Meta,
    ) -> Self {
        Self {
            ok: false,
            schema_version: SCHEMA_VERSION,
            tool_version: env!("CARGO_PKG_VERSION"),
            command,
            exit_code: error.exit().code(),
            partial: None,
            data: None,
            error: Some(ErrorBody::from(error.clone())),
            warnings,
            commands,
            meta,
        }
    }
}

/// Which doctor run a recorded report is (its `report.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Diagnose,
    Fix,
}

/// One stdout document of a `doctor` command (M4-D34): the general envelope's keys with
/// the report's own at the top level, after `exit_code`, and no `data` or `partial`.
/// Findings, a partial fix and a rolled-back fix are reports, so `error` is there only
/// for a run that failed (exits 4, 5, 6, 64, 66, 73 and 74), beside its report when it
/// had one. A run's record carries `kind` instead of `meta`.
#[derive(Debug, Serialize)]
pub struct DoctorEnvelope<'a, R: Serialize> {
    pub ok: bool,
    pub schema_version: &'static str,
    pub tool_version: &'static str,
    pub command: &'a str,
    /// The record form only ([`DoctorEnvelope::record`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<RunKind>,
    pub exit_code: u8,
    /// Absent when the run failed before it had one.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub report: Option<&'a R>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
    pub warnings: &'a [Warning],
    pub commands: &'a [CommandHint],
    /// On stdout only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

impl<'a, R: Serialize> DoctorEnvelope<'a, R> {
    /// A run that produced its report, exiting `exit`; `error` when it also failed.
    pub fn report(
        command: &'a str,
        report: &'a R,
        exit: DoctorExit,
        error: Option<&Error>,
        warnings: &'a [Warning],
        commands: &'a [CommandHint],
        meta: Meta,
    ) -> Self {
        Self {
            ok: exit == DoctorExit::Healthy,
            schema_version: SCHEMA_VERSION,
            tool_version: env!("CARGO_PKG_VERSION"),
            command,
            kind: None,
            exit_code: exit.code(),
            report: Some(report),
            error: error.map(|e| ErrorBody::from(e.clone())),
            warnings,
            commands,
            meta: Some(meta),
        }
    }

    /// The form a run directory records: its `kind`, and no `meta`.
    pub fn record(self, kind: RunKind) -> Self {
        Self {
            kind: Some(kind),
            meta: None,
            ..self
        }
    }
}

impl<'a> DoctorEnvelope<'a, ()> {
    /// A run that failed before it had a report: the envelope's keys and its error.
    pub fn failure(
        command: &'a str,
        failure: &DoctorFailure,
        warnings: &'a [Warning],
        commands: &'a [CommandHint],
        meta: Meta,
    ) -> Self {
        Self {
            ok: false,
            schema_version: SCHEMA_VERSION,
            tool_version: env!("CARGO_PKG_VERSION"),
            command,
            kind: None,
            exit_code: failure.exit.code(),
            report: None,
            error: Some(ErrorBody::from(failure.error.clone())),
            warnings,
            commands,
            meta: Some(meta),
        }
    }
}

/// A doctor command's failure, with its exit: the error's own ([`exit_for`]), except where
/// the runtime names one (73, the run directory).
#[derive(Debug, Clone)]
pub struct DoctorFailure {
    pub error: Error,
    pub exit: DoctorExit,
}

impl DoctorFailure {
    /// The run directory could not be created: exit 73, whatever the error's code.
    pub fn cannot_create(error: Error) -> Self {
        Self {
            error,
            exit: DoctorExit::CannotCreate,
        }
    }
}

impl From<Error> for DoctorFailure {
    fn from(error: Error) -> Self {
        let exit = exit_for(error.code);
        Self { error, exit }
    }
}
