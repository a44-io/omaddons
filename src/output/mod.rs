//! Rendering: the JSON envelope, human output, NDJSON stderr events, and argv display
//! strings.
//!
//! stdout is data and stderr is diagnostics (vision §5.9, D17). Under JSON output,
//! stdout carries exactly one envelope, on success and on failure; in human output it
//! carries rendered data and stays empty on error. Every error is mirrored to stderr
//! once. Commands never print: they return an [`Outcome`] or an [`Error`], and
//! [`emit`] renders it.

mod envelope;
mod human;
mod notes;
mod stderr;
mod term;

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Instant;

use serde::Serialize;

pub use envelope::{Envelope, Meta, SCHEMA_VERSION};
pub use human::{count, table};
pub use notes::RunNotes;
pub use stderr::{LogLevel, Phase, ProgressSink, Reporter, SharedReporter, lock, shared_stderr};
pub use term::{
    OutputConfig, OutputFlags, OutputMode, ProgressMode, StderrMode, Style, TermEnv,
    flags_from_argv, offline_from_argv, resolve, resolve_output_mode_from_argv,
};

use crate::error::{CommandHint, Error, Exit, Warning};

/// What a command returns on success.
pub trait Outcome {
    type Data: Serialize + schemars::JsonSchema;

    /// The canonical subcommand path, e.g. `install` or `targets add`.
    fn command(&self) -> &'static str;
    fn data(&self) -> &Self::Data;
    fn warnings(&self) -> &[Warning] {
        &[]
    }
    fn commands(&self) -> &[CommandHint] {
        &[]
    }
    /// Some items failed; they are listed in the data and mirrored in warnings.
    fn partial(&self) -> bool {
        false
    }
    /// `--check` / `--strict`: a `check_failed` error turns the result into
    /// `ok: false`, exit 6, with the data kept.
    fn assertion_failure(&self) -> Option<Error> {
        None
    }
    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()>;
}

static STDOUT_WRITTEN: AtomicBool = AtomicBool::new(false);
/// The exit status of the document written; `u8::MAX` while there is none.
static EMITTED_EXIT: AtomicU8 = AtomicU8::new(u8::MAX);

/// Whether this process has written a document to stdout (once it serialized). A panic
/// render checks it so that it never emits a second envelope.
pub fn stdout_written() -> bool {
    STDOUT_WRITTEN.load(Ordering::SeqCst)
}

/// The exit status of the document this process wrote, if any: a panic after it keeps
/// that status, so stdout and the exit code agree.
pub fn emitted_exit() -> Option<Exit> {
    let code = EMITTED_EXIT.load(Ordering::SeqCst);
    Exit::ALL.iter().copied().find(|e| e.code() == code)
}

/// Renders a command's result to the real stdout and the reporter, and returns the
/// exit status. `notes` are the warnings the run found on its way, reported on failure
/// too.
pub fn emit<O: Outcome, W: Write>(
    cfg: &OutputConfig,
    reporter: &mut Reporter<W>,
    command: &'static str,
    started: Instant,
    offline: bool,
    notes: &RunNotes,
    result: Result<O, Error>,
) -> Exit {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    emit_to(
        cfg, &mut lock, reporter, command, started, offline, notes, result,
    )
}

/// [`emit`] with an explicit stdout. `command` names the command for failures, which
/// have no outcome to ask. The run's warnings come first, on success followed by the
/// outcome's that are not among them, so each is reported once; its journal warnings
/// describe the end of the run ([`RunNotes::settle_journal`]).
#[expect(
    clippy::too_many_arguments,
    reason = "emit's arguments plus the stdout it writes to; a struct would only rename them"
)]
pub fn emit_to<O: Outcome, W: Write>(
    cfg: &OutputConfig,
    stdout: &mut dyn Write,
    reporter: &mut Reporter<W>,
    command: &'static str,
    started: Instant,
    offline: bool,
    notes: &RunNotes,
    result: Result<O, Error>,
) -> Exit {
    let meta = meta(started, offline);
    // A progress line ends with no newline: the result must not start on it.
    reporter.clear_progress();
    let (mut warnings, _) = notes.snapshot();
    let exit = match result {
        Ok(outcome) => {
            let found = warnings.len();
            for warning in outcome.warnings() {
                if !warnings[..found].contains(warning) {
                    warnings.push(warning.clone());
                }
            }
            notes.settle_journal(&mut warnings);
            notes.rerun_stale(&mut warnings);
            for warning in &warnings {
                reporter.warning(warning);
            }
            let failure = outcome.assertion_failure();
            let unserializable = match cfg.output {
                OutputMode::Json => {
                    let envelope = match &failure {
                        None => Envelope::success(
                            outcome.command(),
                            outcome.data(),
                            outcome.partial(),
                            &warnings,
                            outcome.commands(),
                            meta,
                        ),
                        Some(error) => Envelope::check_failed(
                            outcome.command(),
                            outcome.data(),
                            outcome.partial(),
                            error,
                            &warnings,
                            outcome.commands(),
                            meta,
                        ),
                    };
                    write_json(stdout, &envelope, outcome.command(), meta)
                }
                OutputMode::Human => {
                    // Written once rendered: a panic inside the rendering leaves stdout
                    // to the error render.
                    let rendered = outcome
                        .render_human(stdout, &cfg.style)
                        .and_then(|()| stdout.flush());
                    STDOUT_WRITTEN.store(true, Ordering::SeqCst);
                    ignore_write_error(rendered);
                    None
                }
            };
            match (unserializable, failure) {
                (Some(internal), _) => {
                    reporter.error(&internal);
                    Exit::Failed
                }
                (None, Some(error)) => {
                    reporter.error(&error);
                    error.exit()
                }
                (None, None) => Exit::Ok,
            }
        }
        Err(error) => fail(
            cfg,
            stdout,
            reporter,
            command,
            meta,
            notes,
            warnings,
            &[],
            error,
        ),
    };
    if stdout_written() {
        EMITTED_EXIT.store(exit.code(), Ordering::SeqCst);
    }
    exit
}

/// The internal error of a panic that reached the top, to the real stdout: its envelope
/// carries the run's warnings and hints so far (the `undo` of a transaction that
/// committed before it).
pub fn emit_panic<W: Write>(
    cfg: &OutputConfig,
    reporter: &mut Reporter<W>,
    command: &'static str,
    started: Instant,
    offline: bool,
    notes: &RunNotes,
    error: Error,
) -> Exit {
    let (warnings, commands) = notes.snapshot();
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let meta = meta(started, offline);
    let exit = fail(
        cfg, &mut lock, reporter, command, meta, notes, warnings, &commands, error,
    );
    if stdout_written() {
        EMITTED_EXIT.store(exit.code(), Ordering::SeqCst);
    }
    exit
}

fn meta(started: Instant, offline: bool) -> Meta {
    Meta {
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        offline,
    }
}

/// A failure: the run's `warnings` (journal ones settled) on stderr, its envelope under
/// JSON, then the error on stderr.
#[expect(
    clippy::too_many_arguments,
    reason = "a failure's pieces as emit_to has them; a struct would only rename them"
)]
fn fail<W: Write>(
    cfg: &OutputConfig,
    stdout: &mut dyn Write,
    reporter: &mut Reporter<W>,
    command: &str,
    meta: Meta,
    notes: &RunNotes,
    mut warnings: Vec<Warning>,
    commands: &[CommandHint],
    error: Error,
) -> Exit {
    notes.settle_journal(&mut warnings);
    notes.rerun_stale(&mut warnings);
    for warning in &warnings {
        reporter.warning(warning);
    }
    if cfg.output == OutputMode::Json {
        let envelope = Envelope::failure(command, &error, &warnings, commands, meta);
        if let Some(internal) = write_json(stdout, &envelope, command, meta) {
            reporter.error(&internal);
            return Exit::Failed;
        }
    }
    reporter.error(&error);
    error.exit()
}

/// One compact JSON document and a newline, in one write; the document counts as
/// written only once it serialized. A value that does not serialize (a bug) is replaced
/// by an internal-error envelope, whose error is returned.
fn write_json<T: Serialize>(
    stdout: &mut dyn Write,
    value: &T,
    command: &str,
    meta: Meta,
) -> Option<Error> {
    let (mut line, failed) = match serde_json::to_vec(value) {
        Ok(line) => (line, None),
        Err(e) => {
            let error = Error::internal(format!("the result could not be serialized: {e}"));
            let envelope = Envelope::failure(command, &error, &[], &[], meta);
            let line = serde_json::to_vec(&envelope)
                .unwrap_or_else(|_| unserializable_literal(command).into_bytes());
            (line, Some(error))
        }
    };
    STDOUT_WRITTEN.store(true, Ordering::SeqCst);
    line.push(b'\n');
    ignore_write_error(stdout.write_all(&line).and_then(|()| stdout.flush()));
    failed
}

/// The internal-error envelope as a literal, every key in place, for when even that
/// does not serialize.
fn unserializable_literal(command: &str) -> String {
    let command = serde_json::to_string(command).unwrap_or_else(|_| "\"omaddons\"".to_owned());
    format!(
        "{{\"ok\":false,\"schema_version\":\"{SCHEMA_VERSION}\",\"tool_version\":\"{}\",\"command\":{command},\"exit_code\":1,\"error\":{{\"code\":\"internal\",\"message\":\"the result could not be serialized\",\"fix\":\"omaddons status\",\"fix_argv\":[\"omaddons\",\"status\"],\"alternatives\":[],\"retryable\":false,\"details\":{{}}}},\"warnings\":[],\"commands\":[],\"meta\":{{\"elapsed_ms\":0,\"offline\":false}}}}",
        env!("CARGO_PKG_VERSION")
    )
}

/// A reader that went away (`| head`, a broken pipe) is not an error, and no other
/// stdout failure can be reported anywhere useful, so the exit status stands.
fn ignore_write_error(result: std::io::Result<()>) {
    drop(result);
}

#[cfg(test)]
mod tests;
