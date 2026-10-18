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
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Whether this process has written a document to stdout. The panic hook checks it so
/// that it never emits a second envelope.
pub fn stdout_written() -> bool {
    STDOUT_WRITTEN.load(Ordering::SeqCst)
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
/// outcome's that are not among them, so each is reported once.
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
    let meta = Meta {
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        offline,
    };
    let (mut warnings, _) = notes.snapshot();
    match result {
        Ok(outcome) => {
            let found = warnings.len();
            for warning in outcome.warnings() {
                if !warnings[..found].contains(warning) {
                    warnings.push(warning.clone());
                }
            }
            for warning in &warnings {
                reporter.warning(warning);
            }
            let failure = outcome.assertion_failure();
            match cfg.output {
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
                    write_json(stdout, &envelope);
                }
                OutputMode::Human => {
                    STDOUT_WRITTEN.store(true, Ordering::SeqCst);
                    let rendered = outcome
                        .render_human(stdout, &cfg.style)
                        .and_then(|()| stdout.flush());
                    ignore_write_error(rendered);
                }
            }
            match failure {
                Some(error) => {
                    reporter.error(&error);
                    error.exit()
                }
                None => Exit::Ok,
            }
        }
        Err(error) => {
            for warning in &warnings {
                reporter.warning(warning);
            }
            if cfg.output == OutputMode::Json {
                write_json(
                    stdout,
                    &Envelope::failure(command, &error, &warnings, &[], meta),
                );
            }
            reporter.error(&error);
            error.exit()
        }
    }
}

/// One compact JSON document and a newline, in one write.
fn write_json<T: Serialize>(stdout: &mut dyn Write, value: &T) {
    STDOUT_WRITTEN.store(true, Ordering::SeqCst);
    let mut line = match serde_json::to_vec(value) {
        Ok(line) => line,
        // Every envelope is plain data; a failure here is a bug with nowhere to go.
        Err(_) => b"{\"ok\":false}".to_vec(),
    };
    line.push(b'\n');
    ignore_write_error(stdout.write_all(&line).and_then(|()| stdout.flush()));
}

/// A reader that went away (`| head`, a broken pipe) is not an error, and no other
/// stdout failure can be reported anywhere useful, so the exit status stands.
fn ignore_write_error(result: std::io::Result<()>) {
    drop(result);
}

#[cfg(test)]
mod tests;
