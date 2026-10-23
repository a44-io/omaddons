//! The `Reporter`: the only writer of stderr. In NDJSON mode every line is one object
//! with an `event` of `progress`, `log`, `warning` or `error`; in human mode lines read
//! `warning: …` and `error[<code>]: …` with their fixes. `--quiet` drops everything but
//! errors. Write failures on stderr are ignored: there is nowhere left to report them.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Serialize;

use super::term::{OutputConfig, ProgressMode, StderrMode};
use crate::error::{Error, Warning};

/// The one stderr reporter of a run, shared by the command's progress and `dispatch`'s
/// output, so a human progress line is always cleared before other output. Lock it
/// only for one write.
pub type SharedReporter = Arc<Mutex<Reporter<Box<dyn Write + Send>>>>;

/// The shared reporter for `cfg`, writing to stderr: the one progress draws through, so
/// it measures the terminal, as [`Reporter::stderr`] does.
pub fn shared_stderr(cfg: &OutputConfig) -> SharedReporter {
    let sink: Box<dyn Write + Send> = Box::new(std::io::stderr());
    Arc::new(Mutex::new(Reporter {
        fit_width: true,
        ..Reporter::new(sink, cfg)
    }))
}

/// The reporter, locked for one write (a poisoned lock is still usable: a reporter has
/// no invariant a panic could break).
pub fn lock(reporter: &SharedReporter) -> MutexGuard<'_, Reporter<Box<dyn Write + Send>>> {
    reporter.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A phase of a long command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Release metadata fetched from a source.
    Fetch,
    /// An archive downloaded.
    Download,
    /// An archive verified against a client's load rules, or a backup's files hashed
    /// (under a fixed label, as for `Copy`).
    Verify,
    /// A target's folders staged.
    Stage,
    /// A target's transaction committed.
    Commit,
    /// Files of a `WTF/` tree copied (backups and restores); its message is a fixed
    /// label, never a path, because paths below `WTF/Account/` name the player.
    Copy,
}

impl Phase {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Download => "download",
            Self::Verify => "verify",
            Self::Stage => "stage",
            Self::Commit => "commit",
            Self::Copy => "copy",
        }
    }
}

/// A phase's count so far.
#[derive(Debug, Default)]
struct PhaseCount {
    total: Option<u64>,
    current: u64,
    seen: BTreeSet<String>,
}

#[derive(Debug)]
struct SinkInner {
    reporter: SharedReporter,
    phases: Mutex<BTreeMap<Phase, PhaseCount>>,
}

/// Where a command and the engine report progress: a cheap handle on the run's shared
/// reporter, or nothing when progress is off. Each phase is counted in one place, so
/// `current` only grows, never passes `total`, `total` is the first one given, and an
/// item is reported at most once per phase (a second engine run over the same items,
/// such as `install`'s real run after its probe, adds nothing).
#[derive(Debug, Clone, Default)]
pub struct ProgressSink {
    inner: Option<Arc<SinkInner>>,
    /// This handle's share of a run-wide count ([`ProgressSink::offset`]).
    shift: Option<Shift>,
}

/// Where one of several trees' counts start in a run-wide count, and that count's total.
#[derive(Debug, Clone, Copy)]
struct Shift {
    base: u64,
    total: u64,
}

impl std::fmt::Debug for Reporter<Box<dyn Write + Send>> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reporter")
            .field("mode", &self.mode)
            .field("progress", &self.progress)
            .finish_non_exhaustive()
    }
}

impl ProgressSink {
    /// No progress: every event is dropped without taking a lock.
    pub fn none() -> Self {
        Self::default()
    }

    /// Progress through `reporter`, when `cfg` asks for it (`--quiet` never shows it).
    pub fn new(reporter: &SharedReporter, cfg: &OutputConfig) -> Self {
        if cfg.progress == ProgressMode::None || cfg.style.quiet {
            return Self::none();
        }
        Self {
            inner: Some(Arc::new(SinkInner {
                reporter: reporter.clone(),
                phases: Mutex::new(BTreeMap::new()),
            })),
            shift: None,
        }
    }

    /// This sink, with each [`count`](Self::count) reported as `base` plus it, of the
    /// run-wide `total`: one of several trees a run copies, starting where the trees
    /// before it end, so the phase keeps growing across them. A count that restarts (a
    /// retried copy) stays dropped until it passes what was reported.
    pub fn offset(&self, base: u64, total: u64) -> Self {
        Self {
            inner: self.inner.clone(),
            shift: Some(Shift { base, total }),
        }
    }

    /// Reports that `item` (a ref, an archive or a target id) is through `phase`, one of
    /// `total` items expected in it.
    pub fn event(&self, phase: Phase, total: u64, item: &str) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut phases = inner.phases.lock().unwrap_or_else(PoisonError::into_inner);
        let count = phases.entry(phase).or_default();
        let total = *count.total.get_or_insert(total);
        if count.current >= total || !count.seen.insert(item.to_owned()) {
            return;
        }
        count.current += 1;
        let current = count.current;
        lock(&inner.reporter).progress(phase.wire(), item, Some(current), Some(total));
    }

    /// Reports that `current` of `total` units of `phase` are done, under `message`, a
    /// fixed label (never a name or a path): for a phase that counts files rather than
    /// items (`copy`). Counts only move forward; a repeat or a step back is dropped.
    pub fn count(&self, phase: Phase, current: u64, total: u64, message: &str) {
        let Some(inner) = &self.inner else {
            return;
        };
        let (current, total) = match self.shift {
            Some(shift) => (shift.base.saturating_add(current), shift.total),
            None => (current, total),
        };
        let mut phases = inner.phases.lock().unwrap_or_else(PoisonError::into_inner);
        let count = phases.entry(phase).or_default();
        let total = *count.total.get_or_insert(total);
        let current = current.min(total);
        if current <= count.current {
            return;
        }
        count.current = current;
        lock(&inner.reporter).progress(phase.wire(), message, Some(current), Some(total));
    }

    /// Announces `phase` before any of its `total` items: one event at `current: 0`
    /// saying `message` (what is about to happen, e.g. how much will download).
    pub fn announce(&self, phase: Phase, total: u64, message: &str) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut phases = inner.phases.lock().unwrap_or_else(PoisonError::into_inner);
        let count = phases.entry(phase).or_default();
        if count.current > 0 {
            return;
        }
        let total = *count.total.get_or_insert(total);
        lock(&inner.reporter).progress(phase.wire(), message, Some(0), Some(total));
    }

    /// `more` items join `phase` after its total was given (a dependency round): the
    /// total grows by them, or starts at them when no event came yet.
    pub fn grow(&self, phase: Phase, more: u64) {
        let Some(inner) = &self.inner else {
            return;
        };
        if more == 0 {
            return;
        }
        let mut phases = inner.phases.lock().unwrap_or_else(PoisonError::into_inner);
        let count = phases.entry(phase).or_default();
        count.total = Some(count.total.map_or(more, |t| t.saturating_add(more)));
    }
}

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
    /// Measures the terminal so a progress line fits one row (clearing erases only the
    /// row the cursor is on): the real stderr only.
    fit_width: bool,
}

/// The width in columns of the terminal stderr is, when it says.
fn terminal_width() -> Option<usize> {
    rustix::termios::tcgetwinsize(std::io::stderr())
        .ok()
        .map(|size| usize::from(size.ws_col))
        .filter(|columns| *columns > 0)
}

/// `message` cut so `{phase}: {message}{count}` fits in `columns - 1` characters, with
/// `…` where it was cut.
fn fit(phase: &str, message: &str, count: &str, columns: usize) -> String {
    let room = columns.saturating_sub(1 + phase.chars().count() + 2 + count.chars().count());
    if message.chars().count() <= room {
        return message.to_owned();
    }
    let kept: String = message.chars().take(room.saturating_sub(1)).collect();
    if room == 0 {
        String::new()
    } else {
        format!("{kept}…")
    }
}

/// A human progress line is on the terminal, whichever reporter wrote it: a reporter made
/// afresh (the panic path, after the run's own was dropped while unwinding) clears it.
static PROGRESS_ON_SCREEN: AtomicBool = AtomicBool::new(false);

impl Reporter<std::io::Stderr> {
    pub fn stderr(cfg: &OutputConfig) -> Self {
        Self {
            progress_line: PROGRESS_ON_SCREEN.load(Ordering::Relaxed),
            fit_width: true,
            ..Self::new(std::io::stderr(), cfg)
        }
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
            fit_width: false,
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
                // A line wider than the terminal wraps, and clearing erases only its
                // last row: cut to fit.
                let message = match self.fit_width.then(terminal_width).flatten() {
                    Some(columns) => fit(phase, message, &count, columns),
                    None => message.to_owned(),
                };
                drop(write!(self.sink, "\r\u{1b}[2K{phase}: {message}{count}"));
                drop(self.sink.flush());
                self.progress_line = true;
                PROGRESS_ON_SCREEN.store(true, Ordering::Relaxed);
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

    /// Erases a human progress line still on the terminal: before any other stderr line,
    /// and before the result goes to stdout, which shares the terminal.
    pub(super) fn clear_progress(&mut self) {
        if std::mem::take(&mut self.progress_line) {
            drop(self.sink.write_all(b"\r\x1b[2K"));
            PROGRESS_ON_SCREEN.store(false, Ordering::Relaxed);
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

    /// A progress line fits one row: its message is cut, never its phase or count.
    #[test]
    fn a_progress_message_is_cut_to_the_terminal() {
        let long = "file:/tmp/a/very/long/path/to/an/archive.zip on retail";
        let line = |columns| format!("fetch: {} 1/2", fit("fetch", long, " 1/2", columns));
        assert_eq!(line(200), format!("fetch: {long} 1/2"));
        let cut = line(40);
        assert_eq!(cut.chars().count(), 39, "{cut}");
        assert!(cut.ends_with("… 1/2"), "{cut}");
        assert_eq!(fit("fetch", long, " 1/2", 5), "");
    }

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

    /// A sink whose bytes a test can read while a shared reporter still owns it.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn shared(cfg: &OutputConfig) -> (SharedReporter, Captured) {
        let captured = Captured::default();
        let sink: Box<dyn Write + Send> = Box::new(captured.clone());
        (Arc::new(Mutex::new(Reporter::new(sink, cfg))), captured)
    }

    #[test]
    fn file_counts_only_move_forward_under_one_label() {
        let config = cfg(StderrMode::Ndjson, ProgressMode::Json, false);
        let (reporter, captured) = shared(&config);
        let sink = ProgressSink::new(&reporter, &config);
        for current in [256, 256, 100, 512, 900, 1_000] {
            sink.count(Phase::Copy, current, 600, "WTF/ files");
        }
        let counts: Vec<(String, u64, u64)> = captured
            .text()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .map(|v| {
                (
                    v["message"].as_str().unwrap().to_owned(),
                    v["current"].as_u64().unwrap(),
                    v["total"].as_u64().unwrap(),
                )
            })
            .collect();
        let label = || "WTF/ files".to_owned();
        assert_eq!(
            counts,
            [
                (label(), 256, 600),
                (label(), 512, 600),
                (label(), 600, 600)
            ],
            "no repeat, no step back, never past the total"
        );
        ProgressSink::none().count(Phase::Copy, 1, 1, "WTF/ files");
    }

    /// Three trees of 10, 20 and 30 files in one run: one count of 60, each tree from
    /// where the ones before it ended, a retried tree never stepping back.
    #[test]
    fn offset_trees_count_as_one_run() {
        let config = cfg(StderrMode::Ndjson, ProgressMode::Json, false);
        let (reporter, captured) = shared(&config);
        let sink = ProgressSink::new(&reporter, &config);
        sink.announce(Phase::Copy, 60, "WTF/ files");
        for current in [5, 10] {
            sink.offset(0, 60)
                .count(Phase::Copy, current, 10, "WTF/ files");
        }
        let second = sink.offset(10, 60);
        // A retry from the start: dropped until it passes 15.
        for current in [5, 2, 4, 5, 6, 20] {
            second.count(Phase::Copy, current, 20, "WTF/ files");
        }
        sink.offset(30, 60).count(Phase::Copy, 30, 30, "WTF/ files");
        let counts: Vec<(u64, u64)> = captured
            .text()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .map(|v| (v["current"].as_u64().unwrap(), v["total"].as_u64().unwrap()))
            .collect();
        assert_eq!(
            counts,
            [
                (0, 60),
                (5, 60),
                (10, 60),
                (15, 60),
                (16, 60),
                (30, 60),
                (60, 60)
            ]
        );
    }

    #[test]
    fn progress_is_counted_once_per_item_up_to_a_fixed_total() {
        let config = cfg(StderrMode::Ndjson, ProgressMode::Json, false);
        let (reporter, captured) = shared(&config);
        let sink = ProgressSink::new(&reporter, &config);
        sink.event(Phase::Fetch, 2, "gh:o/foo");
        sink.event(Phase::Fetch, 2, "gh:o/foo");
        sink.event(Phase::Download, 1, "gh:o/foo@1 on retail");
        sink.event(Phase::Fetch, 9, "gh:o/bar");
        sink.event(Phase::Fetch, 9, "gh:o/baz");
        let events: Vec<serde_json::Value> = captured
            .text()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            events,
            [
                serde_json::json!({"event": "progress", "phase": "fetch", "message": "gh:o/foo", "current": 1, "total": 2}),
                serde_json::json!({"event": "progress", "phase": "download", "message": "gh:o/foo@1 on retail", "current": 1, "total": 1}),
                serde_json::json!({"event": "progress", "phase": "fetch", "message": "gh:o/bar", "current": 2, "total": 2}),
            ],
            "an item once, the first total kept, never past it"
        );
        let wires: Vec<&str> = [
            Phase::Fetch,
            Phase::Download,
            Phase::Verify,
            Phase::Stage,
            Phase::Commit,
            Phase::Copy,
        ]
        .iter()
        .map(|p| p.wire())
        .collect();
        assert_eq!(
            wires,
            ["fetch", "download", "verify", "stage", "commit", "copy"]
        );
        for phase in [Phase::Commit, Phase::Copy] {
            assert_eq!(
                serde_json::to_value(phase).unwrap(),
                serde_json::Value::from(phase.wire()),
                "serde agrees with wire()"
            );
        }
    }

    #[test]
    fn no_progress_sink_when_progress_is_off_or_quiet() {
        for config in [
            cfg(StderrMode::Ndjson, ProgressMode::None, false),
            cfg(StderrMode::Ndjson, ProgressMode::Json, true),
        ] {
            let (reporter, captured) = shared(&config);
            let sink = ProgressSink::new(&reporter, &config);
            sink.event(Phase::Fetch, 1, "gh:o/foo");
            assert!(sink.inner.is_none() && captured.text().is_empty());
        }
        ProgressSink::none().event(Phase::Commit, 1, "retail");
    }

    #[test]
    fn a_shared_human_progress_line_is_cleared_before_a_warning() {
        let config = cfg(StderrMode::Human, ProgressMode::Human, false);
        let (reporter, captured) = shared(&config);
        let sink = ProgressSink::new(&reporter, &config);
        sink.event(Phase::Download, 3, "WeakAuras");
        lock(&reporter).warning(&warning());
        let text = captured.text();
        assert!(
            text.starts_with("\r\u{1b}[2Kdownload: WeakAuras 1/3\r\u{1b}[2Kwarning: "),
            "{text:?}"
        );
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
