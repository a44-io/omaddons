//! The `Reporter`: the only writer of stderr. In NDJSON mode every line is one object
//! with an `event` of `progress`, `log`, `warning` or `error`; in human mode lines read
//! `warning: …` and `error[<code>]: …` with their fixes. `--quiet` drops everything but
//! errors. Write failures on stderr are ignored: there is nowhere left to report them.

use std::io::Write;

use serde::Serialize;

use super::term::{OutputConfig, ProgressMode, StderrMode};
use crate::error::{Error, Warning};

/// A `log` event's level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    /// Shown only with `-v`.
    Debug,
}

/// Writes diagnostics to stderr (or any sink, for tests).
pub struct Reporter<W: Write> {
    sink: W,
    mode: StderrMode,
    progress: ProgressMode,
    quiet: bool,
    verbose: u8,
    color: bool,
    /// A human progress line is on screen and must be cleared before other output.
    progress_line: bool,
}

impl Reporter<std::io::Stderr> {
    pub fn stderr(cfg: &OutputConfig) -> Self {
        Self::new(std::io::stderr(), cfg)
    }
}

impl<W: Write> Reporter<W> {
    pub fn new(sink: W, cfg: &OutputConfig) -> Self {
        Self {
            sink,
            mode: cfg.stderr,
            progress: cfg.progress,
            quiet: cfg.style.quiet,
            verbose: cfg.style.verbose,
            color: cfg.stderr_color,
            progress_line: false,
        }
    }

    /// The sink, for tests.
    pub fn into_sink(self) -> W {
        self.sink
    }

    pub fn warning(&mut self, warning: &Warning) {
        if self.quiet {
            return;
        }
        match self.mode {
            StderrMode::Ndjson => {
                #[derive(Serialize)]
                struct Event<'a> {
                    event: &'static str,
                    code: &'static str,
                    message: &'a str,
                    fix: Option<&'a str>,
                    fix_argv: Option<&'a [String]>,
                }
                self.json(&Event {
                    event: "warning",
                    code: warning.code.wire(),
                    message: &warning.message,
                    fix: warning.fix.as_ref().map(|f| f.display()),
                    fix_argv: warning.fix.as_ref().and_then(|f| f.argv()),
                });
            }
            StderrMode::Human => {
                let mut text = format!("{} {}\n", self.paint("33", "warning:"), warning.message);
                if let Some(fix) = &warning.fix {
                    text.push_str(&format!("  fix: {}\n", fix.display()));
                }
                self.human(&text);
            }
        }
    }

    /// Mirrors an error. Never suppressed.
    pub fn error(&mut self, error: &Error) {
        match self.mode {
            StderrMode::Ndjson => {
                #[derive(Serialize)]
                struct Event<'a> {
                    event: &'static str,
                    code: &'static str,
                    message: &'a str,
                    fix: &'a str,
                    fix_argv: Option<&'a [String]>,
                    retryable: bool,
                    exit_code: u8,
                }
                self.json(&Event {
                    event: "error",
                    code: error.code.wire(),
                    message: &error.message,
                    fix: error.fix.display(),
                    fix_argv: error.fix.argv(),
                    retryable: error.retryable,
                    exit_code: error.exit().code(),
                });
            }
            StderrMode::Human => {
                let head = self.paint("31", &format!("error[{}]:", error.code.wire()));
                let mut text = format!("{head} {}\nfix: {}\n", error.message, error.fix.display());
                for alternative in &error.alternatives {
                    text.push_str(&format!("  or: {}\n", alternative.display()));
                }
                self.human(&text);
            }
        }
    }

    pub fn log(&mut self, level: LogLevel, message: &str) {
        if self.quiet || (level == LogLevel::Debug && self.verbose == 0) {
            return;
        }
        match self.mode {
            StderrMode::Ndjson => {
                #[derive(Serialize)]
                struct Event<'a> {
                    event: &'static str,
                    level: LogLevel,
                    message: &'a str,
                }
                self.json(&Event {
                    event: "log",
                    level,
                    message,
                });
            }
            StderrMode::Human => {
                let label = match level {
                    LogLevel::Info => "info:",
                    LogLevel::Debug => "debug:",
                };
                self.human(&format!("{} {message}\n", self.paint("2", label)));
            }
        }
    }

    /// Reports progress of a long operation, as the progress mode allows.
    pub fn progress(
        &mut self,
        phase: &str,
        message: &str,
        current: Option<u64>,
        total: Option<u64>,
    ) {
        if self.quiet {
            return;
        }
        match self.progress {
            ProgressMode::None => {}
            ProgressMode::Json => {
                #[derive(Serialize)]
                struct Event<'a> {
                    event: &'static str,
                    phase: &'a str,
                    message: &'a str,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    current: Option<u64>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    total: Option<u64>,
                }
                self.json(&Event {
                    event: "progress",
                    phase,
                    message,
                    current,
                    total,
                });
            }
            ProgressMode::Human => {
                let count = match (current, total) {
                    (Some(c), Some(t)) => format!(" {c}/{t}"),
                    (Some(c), None) => format!(" {c}"),
                    _ => String::new(),
                };
                drop(write!(self.sink, "\r\u{1b}[2K{phase}: {message}{count}"));
                drop(self.sink.flush());
                self.progress_line = true;
            }
        }
    }

    fn json<T: Serialize>(&mut self, event: &T) {
        self.clear_progress();
        if let Ok(mut line) = serde_json::to_vec(event) {
            line.push(b'\n');
            drop(self.sink.write_all(&line));
            drop(self.sink.flush());
        }
    }

    fn human(&mut self, text: &str) {
        self.clear_progress();
        drop(self.sink.write_all(text.as_bytes()));
        drop(self.sink.flush());
    }

    fn clear_progress(&mut self) {
        if std::mem::take(&mut self.progress_line) {
            drop(self.sink.write_all(b"\r\x1b[2K"));
        }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\u{1b}[{code}m{text}\u{1b}[0m")
        } else {
            text.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ErrorCode, Suggestion, WarningCode};
    use crate::output::term::Style;

    fn cfg(mode: StderrMode, progress: ProgressMode, quiet: bool) -> OutputConfig {
        OutputConfig {
            stderr: mode,
            progress,
            style: Style {
                quiet,
                ..Style::default()
            },
            ..OutputConfig::machine()
        }
    }

    fn error() -> Error {
        Error::new(
            ErrorCode::TargetAmbiguous,
            "weakauras is compatible with 2 targets",
            Suggestion::command(["omaddons", "install", "weakauras", "-t", "retail"]),
        )
        .alternative(Suggestion::command([
            "omaddons",
            "install",
            "weakauras",
            "-t",
            "forever",
        ]))
    }

    fn warning() -> Warning {
        Warning::new(WarningCode::ReloadRequired, "retail needs a /reload")
            .with_fix(Suggestion::text("/reload in game"))
    }

    fn run(cfg: &OutputConfig) -> String {
        let mut r = Reporter::new(Vec::new(), cfg);
        r.warning(&warning());
        r.log(LogLevel::Info, "checking 3 addons");
        r.log(LogLevel::Debug, "hidden without -v");
        r.progress("download", "WeakAuras", Some(1), Some(3));
        r.error(&error());
        String::from_utf8(r.into_sink()).unwrap()
    }

    #[test]
    fn ndjson_lines_are_events() {
        let text = run(&cfg(StderrMode::Ndjson, ProgressMode::Json, false));
        let events: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| e["event"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["warning", "log", "progress", "error"]);
        assert_eq!(
            text.lines().next().unwrap(),
            r#"{"event":"warning","code":"reload_required","message":"retail needs a /reload","fix":"/reload in game","fix_argv":null}"#
        );
        assert_eq!(
            text.lines().nth(2).unwrap(),
            r#"{"event":"progress","phase":"download","message":"WeakAuras","current":1,"total":3}"#
        );
        assert_eq!(events[3]["exit_code"], 3);
        assert_eq!(events[3]["fix_argv"][4], "retail");
        assert!(!text.contains('\u{1b}'), "no ANSI in NDJSON");
    }

    #[test]
    fn human_lines_read_naturally() {
        let text = run(&cfg(StderrMode::Human, ProgressMode::None, false));
        assert_eq!(
            text,
            "warning: retail needs a /reload\n  fix: /reload in game\n\
             info: checking 3 addons\n\
             error[target_ambiguous]: weakauras is compatible with 2 targets\n\
             fix: omaddons install weakauras -t retail\n  or: omaddons install weakauras -t forever\n"
        );
    }

    #[test]
    fn quiet_drops_everything_but_errors() {
        let text = run(&cfg(StderrMode::Ndjson, ProgressMode::Json, true));
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains(r#""event":"error""#));
    }
}
