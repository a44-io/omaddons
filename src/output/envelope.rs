//! The stdout envelope (vision §5.9, D17): one shape for success and failure, with
//! keys in declaration order. Data is serialized generically, never through
//! `serde_json::Value`, which would sort its keys.

use schemars::JsonSchema;
use serde::Serialize;

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
