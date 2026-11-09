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
pub mod trace;

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::Instant;

use serde::Serialize;

pub use envelope::{DoctorEnvelope, DoctorFailure, Envelope, Meta, RunKind, SCHEMA_VERSION};
pub use human::{addons_in_folders, count, table, thousands};
pub use notes::RunNotes;
pub use stderr::{
    LogLevel, Phase, ProgressSink, Reporter, SharedReporter, Subject, lock, shared_stderr,
};
pub use term::{
    OutputConfig, OutputFlags, OutputMode, ProgressMode, StderrMode, Style, TermEnv,
    flags_from_argv, offline_from_argv, resolve, resolve_output_mode_from_argv,
};

use crate::doctor::exit::Exit as DoctorExit;
use crate::error::{CommandHint, Error, Exit, ProcessExit, Warning};

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

/// What a `doctor` command returns once it has a report (M4-D34): the report, whose keys
/// go at the top level of its envelope, and the doctor exit it ends with. A run that
/// failed before it had a report is a [`DoctorFailure`].
pub trait DoctorOutcome {
    type Report: Serialize + schemars::JsonSchema;

    /// The canonical subcommand path, e.g. `doctor diagnose`.
    fn command(&self) -> &'static str;
    fn report(&self) -> &Self::Report;
    fn exit(&self) -> DoctorExit;
    /// The failure of a run that still has its report (an I/O error while recording it).
    fn error(&self) -> Option<&Error> {
        None
    }
    fn warnings(&self) -> &[Warning] {
        &[]
    }
    fn commands(&self) -> &[CommandHint] {
        &[]
    }
    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()>;
}

static STDOUT_WRITTEN: AtomicBool = AtomicBool::new(false);
/// The exit of the document written, with its dictionary ([`encode_exit`]); `u16::MAX`
/// while there is none.
static EMITTED_EXIT: AtomicU16 = AtomicU16::new(NO_EXIT);

const NO_EXIT: u16 = u16::MAX;
/// The bit that marks a doctor exit, above every exit status.
const DOCTOR_BIT: u16 = 0x100;

/// Whether this process has written a document to stdout (once it serialized). A panic
/// render checks it so that it never emits a second envelope.
pub fn stdout_written() -> bool {
    STDOUT_WRITTEN.load(Ordering::SeqCst)
}

/// The exit of the document this process wrote, if any, in its own dictionary: a panic
/// after it keeps that exit, so stdout and the exit code agree.
pub fn emitted_exit() -> Option<ProcessExit> {
    decode_exit(EMITTED_EXIT.load(Ordering::SeqCst))
}

/// Remembers `exit` once a document is out.
fn record_emitted(exit: ProcessExit) {
    if stdout_written() {
        EMITTED_EXIT.store(encode_exit(exit), Ordering::SeqCst);
    }
}

/// An exit and its dictionary in one value: the status, with [`DOCTOR_BIT`] for a doctor
/// exit, so a doctor 4 is never read back as the general 4.
fn encode_exit(exit: ProcessExit) -> u16 {
    match exit {
        ProcessExit::General(exit) => u16::from(exit.code()),
        ProcessExit::Doctor(exit) => DOCTOR_BIT | u16::from(exit.code()),
    }
}

fn decode_exit(value: u16) -> Option<ProcessExit> {
    if value == NO_EXIT {
        return None;
    }
    let code = u8::try_from(value & !DOCTOR_BIT).ok()?;
    if value & DOCTOR_BIT == 0 {
        Exit::ALL
            .iter()
            .copied()
            .find(|e| e.code() == code)
            .map(ProcessExit::General)
    } else {
        DoctorExit::ALL
            .iter()
            .copied()
            .find(|e| e.code() == code)
            .map(ProcessExit::Doctor)
    }
}

/// The two dictionaries a document is written under, and what each writes on a failure.
trait ExitDictionary: Copy + Into<ProcessExit> {
    /// The exit of a run whose document could not be serialized.
    const INTERNAL: Self;

    fn code(self) -> u8;

    /// The failure document of `error`, exiting `self`.
    fn failure_document(
        self,
        command: &str,
        error: &Error,
        warnings: &[Warning],
        commands: &[CommandHint],
        meta: Meta,
    ) -> serde_json::Result<Vec<u8>>;
}

impl ExitDictionary for Exit {
    const INTERNAL: Self = Exit::Failed;

    fn code(self) -> u8 {
        Exit::code(self)
    }

    fn failure_document(
        self,
        command: &str,
        error: &Error,
        warnings: &[Warning],
        commands: &[CommandHint],
        meta: Meta,
    ) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(&Envelope {
            exit_code: self.code(),
            ..Envelope::failure(command, error, warnings, commands, meta)
        })
    }
}

impl ExitDictionary for DoctorExit {
    const INTERNAL: Self = DoctorExit::IoError;

    fn code(self) -> u8 {
        DoctorExit::code(self)
    }

    fn failure_document(
        self,
        command: &str,
        error: &Error,
        warnings: &[Warning],
        commands: &[CommandHint],
        meta: Meta,
    ) -> serde_json::Result<Vec<u8>> {
        let failure = DoctorFailure {
            error: error.clone(),
            exit: self,
        };
        serde_json::to_vec(&DoctorEnvelope::failure(
            command, &failure, warnings, commands, meta,
        ))
    }
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
            report_warnings(reporter, notes, &mut warnings, outcome.warnings());
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
                    write_json::<Exit>(
                        stdout,
                        serde_json::to_vec(&envelope),
                        outcome.command(),
                        meta,
                    )
                }
                OutputMode::Human => {
                    write_human(stdout, |w| outcome.render_human(w, &cfg.style));
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
        Err(error) => {
            let exit = error.exit();
            fail(
                cfg,
                stdout,
                reporter,
                command,
                meta,
                notes,
                warnings,
                &[],
                error,
                exit,
            )
        }
    };
    record_emitted(exit.into());
    exit
}

/// A `doctor` command's result, to the real stdout and the reporter, and its exit in the
/// doctor's dictionary (M4-D34, M4-D40).
pub fn emit_doctor<O: DoctorOutcome, W: Write>(
    cfg: &OutputConfig,
    reporter: &mut Reporter<W>,
    command: &'static str,
    started: Instant,
    offline: bool,
    notes: &RunNotes,
    result: Result<O, DoctorFailure>,
) -> DoctorExit {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    emit_doctor_to(
        cfg, &mut lock, reporter, command, started, offline, notes, result,
    )
}

/// [`emit_doctor`] with an explicit stdout, as [`emit_to`] is [`emit`]'s. A report is a
/// result, not a failure, whatever its exit: it writes no stderr `error` event unless it
/// carries an error.
#[expect(
    clippy::too_many_arguments,
    reason = "emit_doctor's arguments plus the stdout it writes to, as emit_to has them"
)]
pub fn emit_doctor_to<O: DoctorOutcome, W: Write>(
    cfg: &OutputConfig,
    stdout: &mut dyn Write,
    reporter: &mut Reporter<W>,
    command: &'static str,
    started: Instant,
    offline: bool,
    notes: &RunNotes,
    result: Result<O, DoctorFailure>,
) -> DoctorExit {
    let meta = meta(started, offline);
    reporter.clear_progress();
    let (mut warnings, _) = notes.snapshot();
    let exit = match result {
        Ok(outcome) => {
            report_warnings(reporter, notes, &mut warnings, outcome.warnings());
            let unserializable = match cfg.output {
                OutputMode::Json => {
                    let envelope = DoctorEnvelope::report(
                        outcome.command(),
                        outcome.report(),
                        outcome.exit(),
                        outcome.error(),
                        &warnings,
                        outcome.commands(),
                        meta,
                    );
                    write_json::<DoctorExit>(
                        stdout,
                        serde_json::to_vec(&envelope),
                        outcome.command(),
                        meta,
                    )
                }
                OutputMode::Human => {
                    write_human(stdout, |w| outcome.render_human(w, &cfg.style));
                    None
                }
            };
            match (unserializable, outcome.error()) {
                (Some(internal), _) => {
                    reporter.error_exiting(&internal, DoctorExit::INTERNAL.code());
                    DoctorExit::INTERNAL
                }
                (None, Some(error)) => {
                    reporter.error_exiting(error, outcome.exit().code());
                    outcome.exit()
                }
                (None, None) => outcome.exit(),
            }
        }
        Err(failure) => fail(
            cfg,
            stdout,
            reporter,
            command,
            meta,
            notes,
            warnings,
            &[],
            failure.error,
            failure.exit,
        ),
    };
    record_emitted(exit.into());
    exit
}

/// An error no outcome came with (a usage error, a context that could not be built), to
/// the real stdout, exiting `exit` in its command's dictionary. As [`emit`]'s failures,
/// it carries the run's warnings and none of its hints.
#[expect(
    clippy::too_many_arguments,
    reason = "a failure's pieces as emit has them, and the exit its dictionary gives"
)]
pub fn emit_failure<W: Write>(
    cfg: &OutputConfig,
    reporter: &mut Reporter<W>,
    command: &'static str,
    started: Instant,
    offline: bool,
    notes: &RunNotes,
    error: Error,
    exit: ProcessExit,
) -> ProcessExit {
    reporter.clear_progress();
    let (warnings, _) = notes.snapshot();
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let meta = meta(started, offline);
    let exit = fail_as(
        cfg,
        &mut lock,
        reporter,
        command,
        meta,
        notes,
        warnings,
        &[],
        error,
        exit,
    );
    record_emitted(exit);
    exit
}

/// The internal error of a panic that reached the top, to the real stdout, exiting
/// `exit` in its command's dictionary: its envelope carries the run's warnings and hints
/// so far (the `undo` of a transaction that committed before it).
#[expect(
    clippy::too_many_arguments,
    reason = "a failure's pieces as emit has them, and the exit its dictionary gives"
)]
pub fn emit_panic<W: Write>(
    cfg: &OutputConfig,
    reporter: &mut Reporter<W>,
    command: &'static str,
    started: Instant,
    offline: bool,
    notes: &RunNotes,
    error: Error,
    exit: ProcessExit,
) -> ProcessExit {
    let (warnings, commands) = notes.snapshot();
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let meta = meta(started, offline);
    let exit = fail_as(
        cfg, &mut lock, reporter, command, meta, notes, warnings, &commands, error, exit,
    );
    record_emitted(exit);
    exit
}

fn meta(started: Instant, offline: bool) -> Meta {
    Meta {
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        offline,
    }
}

/// The run's `warnings`, then those of its outcome not among them, so each is reported
/// once; its journal warnings describe the end of the run
/// ([`RunNotes::settle_journal`]). Each goes to stderr.
fn report_warnings<W: Write>(
    reporter: &mut Reporter<W>,
    notes: &RunNotes,
    warnings: &mut Vec<Warning>,
    outcome: &[Warning],
) {
    let found = warnings.len();
    for warning in outcome {
        if !warnings[..found].contains(warning) {
            warnings.push(warning.clone());
        }
    }
    notes.settle_journal(warnings);
    notes.rerun_stale(warnings);
    for warning in warnings.iter() {
        reporter.warning(warning);
    }
}

/// Human output, counted as written once rendered: a panic inside the rendering leaves
/// stdout to the error render.
fn write_human(stdout: &mut dyn Write, render: impl FnOnce(&mut dyn Write) -> std::io::Result<()>) {
    let rendered = render(&mut *stdout).and_then(|()| stdout.flush());
    STDOUT_WRITTEN.store(true, Ordering::SeqCst);
    ignore_write_error(rendered);
}

/// [`fail`] in `exit`'s own dictionary.
#[expect(
    clippy::too_many_arguments,
    reason = "a failure's pieces as fail takes them"
)]
fn fail_as<W: Write>(
    cfg: &OutputConfig,
    stdout: &mut dyn Write,
    reporter: &mut Reporter<W>,
    command: &str,
    meta: Meta,
    notes: &RunNotes,
    warnings: Vec<Warning>,
    commands: &[CommandHint],
    error: Error,
    exit: ProcessExit,
) -> ProcessExit {
    match exit {
        ProcessExit::General(exit) => fail(
            cfg, stdout, reporter, command, meta, notes, warnings, commands, error, exit,
        )
        .into(),
        ProcessExit::Doctor(exit) => fail(
            cfg, stdout, reporter, command, meta, notes, warnings, commands, error, exit,
        )
        .into(),
    }
}

/// A failure exiting `exit`: the run's `warnings` (journal ones settled) on stderr, its
/// envelope under JSON, then the error on stderr. A document that does not serialize
/// exits as its dictionary's internal error.
#[expect(
    clippy::too_many_arguments,
    reason = "a failure's pieces as emit_to has them; a struct would only rename them"
)]
fn fail<X: ExitDictionary, W: Write>(
    cfg: &OutputConfig,
    stdout: &mut dyn Write,
    reporter: &mut Reporter<W>,
    command: &str,
    meta: Meta,
    notes: &RunNotes,
    mut warnings: Vec<Warning>,
    commands: &[CommandHint],
    error: Error,
    exit: X,
) -> X {
    report_warnings(reporter, notes, &mut warnings, &[]);
    if cfg.output == OutputMode::Json {
        let document = exit.failure_document(command, &error, &warnings, commands, meta);
        if let Some(internal) = write_json::<X>(stdout, document, command, meta) {
            reporter.error_exiting(&internal, X::INTERNAL.code());
            return X::INTERNAL;
        }
    }
    reporter.error_exiting(&error, exit.code());
    exit
}

/// One compact JSON document and a newline, in one write; the document counts as
/// written only once it serialized. A document that did not serialize (a bug) is
/// replaced by an internal-error envelope of `X`'s dictionary, whose error is returned.
fn write_json<X: ExitDictionary>(
    stdout: &mut dyn Write,
    document: serde_json::Result<Vec<u8>>,
    command: &str,
    meta: Meta,
) -> Option<Error> {
    let (mut line, failed) = match document {
        Ok(line) => (line, None),
        Err(e) => {
            let error = Error::internal(format!("the result could not be serialized: {e}"));
            let line = X::INTERNAL
                .failure_document(command, &error, &[], &[], meta)
                .unwrap_or_else(|_| {
                    unserializable_literal(command, X::INTERNAL.code()).into_bytes()
                });
            (line, Some(error))
        }
    };
    STDOUT_WRITTEN.store(true, Ordering::SeqCst);
    line.push(b'\n');
    ignore_write_error(stdout.write_all(&line).and_then(|()| stdout.flush()));
    failed
}

/// The internal-error envelope as a literal, every key in place, for when even that
/// does not serialize; `exit_code` is the internal exit of its command's dictionary.
fn unserializable_literal(command: &str, exit_code: u8) -> String {
    let command = serde_json::to_string(command).unwrap_or_else(|_| "\"omaddons\"".to_owned());
    format!(
        "{{\"ok\":false,\"schema_version\":\"{SCHEMA_VERSION}\",\"tool_version\":\"{}\",\"command\":{command},\"exit_code\":{exit_code},\"error\":{{\"code\":\"internal\",\"message\":\"the result could not be serialized\",\"fix\":\"omaddons status\",\"fix_argv\":[\"omaddons\",\"status\"],\"alternatives\":[],\"retryable\":false,\"details\":{{}}}},\"warnings\":[],\"commands\":[],\"meta\":{{\"elapsed_ms\":0,\"offline\":false}}}}",
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
