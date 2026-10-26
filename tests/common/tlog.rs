//! A structured step log for integration and end-to-end tests (M4 T1).
//!
//! A scenario that fails at its fourteenth command needs the thirteen before it. A
//! [`TestLog`] records what a test did, as it does it, in two files under
//! [`log_root`] (`target/tmp/test-logs` by default):
//!
//! - `<suite>/<test>/steps.jsonl`: one compact JSON record per line, `kind` first and
//!   `seq` second, a `header` first and an `end` last. Each record is written with one
//!   `write_all` as soon as it is complete, so a test that is killed still leaves
//!   everything up to that point. The format is internal to the test suite, not part of
//!   the omaddons contract, and versioned by the header's `format: 1`.
//! - `<suite>/<test>/transcript.md`: the same records, readable.
//!
//! `let log = tlog!();` names the directory after the test crate and the test function
//! (`fn x` in `mod m` is `m.x`). Call it in the test function itself; a helper names
//! its log with [`TestLog::new`]. One log per test: a rerun replaces its directory, and
//! nothing else deletes logs (`cargo clean` does, and nothing is committed).
//!
//! **When a test fails.** A failing assertion made through the log ([`TestLog::assert_eq`])
//! writes a `failure` record and panics naming the step and the transcript. Whatever the
//! panic, the log's drop guard then prints `test log: <transcript path>` and the last
//! [`LAST_STEPS_SHOWN`] records, which libtest shows in the failing test's output.
//!
//! **Modes.** `OMADDONS_TEST_LOG` ([`LOG_MODE_VAR`]) is read once per test process.
//! Unset, the log writes its files only; `verbose` also echoes every record's one-line
//! summary straight to stderr as it happens, past libtest's capture, so it streams
//! without `--nocapture`. Any other value fails the first log created. `TestEnv` strips
//! every `OMADDONS_*` variable from the binary's environment, so the binary never sees it.
//!
//! **Privacy.** A transcript is what someone pastes into a bead comment, so:
//!
//! - every value given to [`TestLog::secret`] (and, with runs, every value of a variable
//!   whose name contains `TOKEN`, `SECRET` or `CREDENTIAL`) of at least
//!   [`SCRUB_MIN_LEN`] bytes is replaced by `<redacted>` in every later record;
//! - paths are rewritten: each `TestEnv` root to `<root>`, `<root2>`, …,
//!   `CARGO_TARGET_TMPDIR` to `<target-tmp>` and `CARGO_MANIFEST_DIR` to `<repo>`;
//! - the test process's own environment is never recorded, and the log never reads the
//!   developer's real home: it holds synthetic homes only.
//!
//! **Bounds.** A note's text or a JSON value's serialization keeps at most
//! [`STREAM_CAP_BYTES`], recorded as `truncated: {kept, total}`. One test writes at most
//! [`TEST_CAP_BYTES`] to both files together: the record that would pass it is replaced
//! by one `capped` record, nothing more is written but the `end` record, and `end.dropped`
//! counts the records left out. A write that fails (a full disk) prints one line and
//! turns that log's writes off; the log is a diagnostic and never fails a test itself.
//!
//! **Conventions.**
//!
//! 1. Every M4 integration or end-to-end test that runs more than one command logs
//!    through `TestLog`, and its crate carries a `//! e2e suite: <purpose>` line.
//! 2. Each phase of a scenario is a [`TestLog::step`].
//! 3. Every state assertion goes through the log's tree checks, or
//!    [`TestLog::assert_eq`], or logs the values it compares with [`TestLog::json`]
//!    before asserting.
//! 4. Unit tests are table-driven, with a name per case and assertion messages that name
//!    the case and show the input and the output; they do not use `TestLog`.
//! 5. An older suite adopts the log when a leaf rewrites it; nothing is converted for its
//!    own sake.
//! 6. Real-machine tests and live probes never use `TestLog`.
//! 7. Bead comments quote the log root as `target/tmp/test-logs`, never an absolute path.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

/// The most bytes a record keeps of one stream (stdout, stderr), one note's text or one
/// JSON value's serialization.
pub const STREAM_CAP_BYTES: usize = 1 << 20;
/// The most bytes one test writes to both files together, case sub-logs included.
pub const TEST_CAP_BYTES: u64 = 64 << 20;
/// The records the panic guard prints.
pub const LAST_STEPS_SHOWN: usize = 20;
/// The differing paths a tree check logs.
pub const DIFF_PATHS_SHOWN: usize = 20;
/// The longest one run may take: five times a failpoint pause's 60 s maximum, so a
/// paused child always resumes first.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(300);
/// The shortest secret value scrubbed from text: a shorter one (`1`) would destroy
/// unrelated output.
pub const SCRUB_MIN_LEN: usize = 8;
/// The variable that chooses the log mode.
pub const LOG_MODE_VAR: &str = "OMADDONS_TEST_LOG";

/// The record format, in the header.
const FORMAT: u32 = 1;
/// What a secret becomes.
const REDACTED: &str = "<redacted>";
/// Room the budget keeps for the one `capped` record, so a capped log stays within its
/// cap; the `end` record alone may pass it.
const CAPPED_RESERVE_BYTES: u64 = 256;
/// The longest one-line summary of a note or a failure.
const SUMMARY_CHARS: usize = 120;
/// The longest value an `assert` heading shows inline.
const INLINE_VALUE_CHARS: usize = 80;

/// The directory every test log is under: `<CARGO_TARGET_TMPDIR>/test-logs`.
pub fn log_root() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("test-logs")
}

/// What a log writes besides its files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The two files only.
    Files,
    /// The files, and each record's summary echoed to stderr as it is made.
    Verbose,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Files => "files",
            Mode::Verbose => "verbose",
        }
    }
}

/// The mode an `OMADDONS_TEST_LOG` value asks for: unset or empty is [`Mode::Files`],
/// `verbose` is [`Mode::Verbose`], and anything else is an error naming the values.
pub fn parse_mode(value: Option<&str>) -> Result<Mode, String> {
    match value {
        None | Some("") => Ok(Mode::Files),
        Some("verbose") => Ok(Mode::Verbose),
        Some(other) => Err(format!(
            "{LOG_MODE_VAR}={other} is not a log mode: use `verbose`, or leave it unset"
        )),
    }
}

/// This process's mode, read once.
fn process_mode() -> Result<Mode, String> {
    static MODE: OnceLock<Result<Mode, String>> = OnceLock::new();
    MODE.get_or_init(|| {
        let value = std::env::var_os(LOG_MODE_VAR);
        let value = value.as_ref().map(|v| v.to_string_lossy());
        parse_mode(value.as_deref())
    })
    .clone()
}

/// A log's bounds; the self-test uses small ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Caps {
    pub stream_bytes: usize,
    pub test_bytes: u64,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            stream_bytes: STREAM_CAP_BYTES,
            test_bytes: TEST_CAP_BYTES,
        }
    }
}

/// What a cap kept of a text: `kept` of its `total` bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Truncated {
    pub kept: usize,
    pub total: usize,
}

/// `text` cut to at most `cap` bytes on a character boundary, with what was kept when it
/// was cut.
pub fn cap_text(text: &str, cap: usize) -> (&str, Option<Truncated>) {
    if text.len() <= cap {
        return (text, None);
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = Truncated {
        kept: end,
        total: text.len(),
    };
    (&text[..end], Some(truncated))
}

/// Whether a variable of this name holds a secret: it contains `TOKEN`, `SECRET` or
/// `CREDENTIAL`, ignoring case. Over-redaction is harmless.
pub fn is_secret_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    ["TOKEN", "SECRET", "CREDENTIAL"]
        .iter()
        .any(|fragment| name.contains(fragment))
}

/// The secret values a log has learned, replaced by `<redacted>` in every text it
/// records after learning them, longest first (so a secret holding another is replaced
/// whole).
#[derive(Clone, Debug, Default)]
pub struct Scrubber {
    values: Vec<String>,
}

impl Scrubber {
    pub fn new() -> Self {
        Self::default()
    }

    /// Learns `value`. Returns whether it will be scrubbed from text: a value shorter
    /// than [`SCRUB_MIN_LEN`] is not.
    pub fn add(&mut self, value: &str) -> bool {
        if value.len() < SCRUB_MIN_LEN {
            return false;
        }
        if !self.values.iter().any(|known| known == value) {
            self.values.push(value.to_owned());
            self.values
                .sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        }
        true
    }

    pub fn scrub(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for value in &self.values {
            if text.contains(value.as_str()) {
                text = text.replace(value.as_str(), REDACTED);
            }
        }
        text
    }
}

/// Rewrites the absolute paths a log may meet to placeholders, longest prefix first:
/// each `TestEnv` root to `<root>`, `<root2>`, … in the order they were added,
/// `CARGO_TARGET_TMPDIR` to `<target-tmp>` and `CARGO_MANIFEST_DIR` to `<repo>`. So a
/// home made with `tempdir_in(CARGO_TARGET_TMPDIR)` is `<root>`, and two runs' logs diff
/// cleanly. A prefix is replaced only where it ends a path component: `<root>-other`
/// stays as it is.
#[derive(Clone, Debug)]
pub struct Relativizer {
    /// `(prefix, placeholder)`, longest prefix first.
    prefixes: Vec<(String, String)>,
    roots: Vec<PathBuf>,
}

impl Default for Relativizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Relativizer {
    pub fn new() -> Self {
        let mut paths = Self {
            prefixes: Vec::new(),
            roots: Vec::new(),
        };
        paths.insert(Path::new(env!("CARGO_TARGET_TMPDIR")), "<target-tmp>");
        paths.insert(Path::new(env!("CARGO_MANIFEST_DIR")), "<repo>");
        paths
    }

    /// Registers a `TestEnv` root and returns its placeholder, the same one when the
    /// root is known already.
    pub fn add_root(&mut self, root: &Path) -> String {
        if let Some(index) = self.roots.iter().position(|known| known == root) {
            return root_placeholder(index + 1);
        }
        self.roots.push(root.to_path_buf());
        let placeholder = root_placeholder(self.roots.len());
        self.insert(root, &placeholder);
        placeholder
    }

    /// The roots added, in order.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn rewrite(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (prefix, placeholder) in &self.prefixes {
            if text.contains(prefix.as_str()) {
                text = replace_at_boundary(&text, prefix, placeholder);
            }
        }
        text
    }

    /// Maps `path`, and its canonical form when that differs, to `placeholder`. A
    /// prefix already mapped keeps its placeholder.
    fn insert(&mut self, path: &Path, placeholder: &str) {
        let mut forms = vec![path.to_path_buf()];
        if let Ok(canonical) = path.canonicalize()
            && canonical != path
        {
            forms.push(canonical);
        }
        for form in forms {
            let prefix = form.to_string_lossy();
            let prefix = prefix.trim_end_matches('/');
            if prefix.len() < 2 || self.prefixes.iter().any(|(known, _)| known == prefix) {
                continue;
            }
            self.prefixes
                .push((prefix.to_owned(), placeholder.to_owned()));
        }
        self.prefixes
            .sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    }
}

fn root_placeholder(index: usize) -> String {
    if index == 1 {
        "<root>".to_owned()
    } else {
        format!("<root{index}>")
    }
}

/// `text` with each occurrence of `prefix` that is not followed by a character that
/// continues a file name replaced by `with`.
fn replace_at_boundary(text: &str, prefix: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(prefix) {
        let after = &rest[at + prefix.len()..];
        let continues = after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-');
        out.push_str(&rest[..at]);
        out.push_str(if continues { prefix } else { with });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The bytes one test may still write, shared by a log and its cases. Once a record is
/// refused, every later one is too, and each refusal counts as dropped.
#[derive(Debug)]
pub struct Budget {
    cap: u64,
    reserve: u64,
    used: u64,
    closed: bool,
    dropped: u64,
}

impl Budget {
    pub fn new(cap: u64) -> Self {
        Self::with_reserve(cap, 0)
    }

    /// A budget that admits records only while `reserve` bytes stay free for the record
    /// that closes it.
    pub fn with_reserve(cap: u64, reserve: u64) -> Self {
        Self {
            cap,
            reserve,
            used: 0,
            closed: false,
            dropped: 0,
        }
    }

    /// Whether a record of `bytes` may be written; admitting it counts its bytes.
    pub fn admit(&mut self, bytes: u64) -> bool {
        let total = self.used.saturating_add(bytes).saturating_add(self.reserve);
        if !self.closed && total <= self.cap {
            self.used += bytes;
            return true;
        }
        self.closed = true;
        self.dropped += 1;
        false
    }

    /// Counts bytes written without asking: the `capped` and `end` records.
    pub fn charge(&mut self, bytes: u64) {
        self.used = self.used.saturating_add(bytes);
    }

    pub fn used(&self) -> u64 {
        self.used
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// `name` made safe as one path component: every character outside `[A-Za-z0-9_.-]`
/// becomes `_`, and an empty name or one made only of dots is `_`.
pub fn sanitize_component(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.chars().all(|c| c == '.') {
        "_".to_owned()
    } else {
        name
    }
}

/// The suite and test names [`tlog!`] derives: the suite is `module_path`'s first
/// segment (the test crate), and the test is the function's path without that segment,
/// the macro's own `__tlog_here` and any `{{closure}}`, joined with `.` so one test's
/// directory never nests inside another's.
pub fn split_location(module_path: &str, fn_type_name: &str) -> (String, String) {
    let suite = module_path.split("::").next().unwrap_or_default();
    let mut segments: Vec<&str> = fn_type_name.split("::").skip(1).collect();
    if segments.last() == Some(&"__tlog_here") {
        segments.pop();
    }
    let test = segments
        .into_iter()
        .filter(|segment| *segment != "{{closure}}")
        .map(sanitize_component)
        .collect::<Vec<_>>()
        .join(".");
    (sanitize_component(suite), sanitize_component(&test))
}

/// One record of `steps.jsonl`.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record {
    Header {
        seq: u64,
        format: u32,
        suite: String,
        test: String,
        started_at: String,
        mode: &'static str,
        caps: Caps,
        parent: Option<String>,
    },
    Step {
        seq: u64,
        index: u64,
        label: String,
        at_ms: u64,
    },
    Note {
        seq: u64,
        step: Option<String>,
        text: String,
        truncated: Option<Truncated>,
    },
    Json {
        seq: u64,
        step: Option<String>,
        label: String,
        value: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        truncated: Option<Truncated>,
    },
    Assert {
        seq: u64,
        step: Option<String>,
        label: String,
        actual: Value,
        expected: Value,
        equal: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        actual_truncated: Option<Truncated>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expected_truncated: Option<Truncated>,
    },
    Failure {
        seq: u64,
        step: Option<String>,
        message: String,
    },
    Capped {
        seq: u64,
        test_bytes: u64,
    },
    End {
        seq: u64,
        outcome: &'static str,
        records: u64,
        runs: u64,
        bytes: u64,
        dropped: u64,
        elapsed_ms: u64,
    },
}

impl Record {
    fn seq(&self) -> u64 {
        match self {
            Record::Header { seq, .. }
            | Record::Step { seq, .. }
            | Record::Note { seq, .. }
            | Record::Json { seq, .. }
            | Record::Assert { seq, .. }
            | Record::Failure { seq, .. }
            | Record::Capped { seq, .. }
            | Record::End { seq, .. } => *seq,
        }
    }

    /// The one line the ring, the live echo and the guard show.
    fn summary(&self) -> String {
        match self {
            Record::Header {
                suite, test, mode, ..
            } => format!("header {suite}::{test} (mode {mode})"),
            Record::Step { index, label, .. } => format!("step {index}: {label}"),
            Record::Note { text, .. } => format!("note: {}", one_line(text)),
            Record::Json { label, .. } => format!("json {label}"),
            Record::Assert { label, equal, .. } => {
                format!(
                    "assert {label}: {}",
                    if *equal { "equal" } else { "differs" }
                )
            }
            Record::Failure { message, .. } => format!("failure: {}", one_line(message)),
            Record::Capped { test_bytes, .. } => format!(
                "capped: the log reached {} bytes; later records are not written",
                thousands(*test_bytes)
            ),
            Record::End { outcome, .. } => format!("end: {outcome}"),
        }
    }

    /// The record's part of `transcript.md`.
    fn render(&self) -> String {
        match self {
            Record::Header {
                suite,
                test,
                started_at,
                mode,
                parent,
                ..
            } => {
                let case = parent
                    .as_ref()
                    .map(|parent| format!(" · case of {parent}"))
                    .unwrap_or_default();
                format!(
                    "# {suite} :: {test}\n\n- started {started_at} · mode {mode} · steps.jsonl beside this file{case}\n"
                )
            }
            Record::Step { index, label, .. } => format!("\n## {index}. {label}\n"),
            Record::Note {
                text, truncated, ..
            } => {
                let body = if text.contains('\n') {
                    format!("note:\n\n{}", fenced(text, "text"))
                } else {
                    format!("note: {text}\n")
                };
                format!("\n{body}{}", truncation_line(*truncated))
            }
            Record::Json {
                label,
                value,
                text,
                truncated,
                ..
            } => {
                let body = match text {
                    Some(text) => fenced(text, "text"),
                    None => fenced(&pretty(value), "json"),
                };
                format!(
                    "\n### json {}\n\n{body}{}",
                    inline_code(label),
                    truncation_line(*truncated)
                )
            }
            Record::Assert {
                label,
                actual,
                expected,
                equal,
                actual_truncated,
                expected_truncated,
                ..
            } => {
                let label = inline_code(label);
                let compact = actual.to_string();
                if *equal && compact.chars().count() <= INLINE_VALUE_CHARS {
                    format!("\n### assert {label}: equal ({compact})\n")
                } else if *equal {
                    format!(
                        "\n### assert {label}: equal\n\n{}{}",
                        fenced(&pretty(actual), "json"),
                        truncation_line(*actual_truncated)
                    )
                } else {
                    format!(
                        "\n### assert {label}: differs\n\nexpected:\n\n{}{}\nactual:\n\n{}{}",
                        fenced(&pretty(expected), "json"),
                        truncation_line(*expected_truncated),
                        fenced(&pretty(actual), "json"),
                        truncation_line(*actual_truncated)
                    )
                }
            }
            Record::Failure { step, message, .. } => {
                format!(
                    "\n**failure** {}:\n\n{}",
                    in_step(step.as_deref()),
                    fenced(message, "text")
                )
            }
            Record::Capped { test_bytes, .. } => format!(
                "\nlog capped at {} bytes: the records after this one are not written\n",
                thousands(*test_bytes)
            ),
            Record::End {
                outcome,
                records,
                runs,
                bytes,
                dropped,
                elapsed_ms,
                ..
            } => {
                let dropped = if *dropped > 0 {
                    format!(" · {} records dropped by the cap", thousands(*dropped))
                } else {
                    String::new()
                };
                let seconds = *elapsed_ms as f64 / 1000.0;
                format!(
                    "\n---\n\nend: {outcome} · {} records · {} runs · {} bytes · {seconds:.1} s{dropped}\n",
                    thousands(*records),
                    thousands(*runs),
                    thousands(*bytes)
                )
            }
        }
    }
}

/// Where a record was made, for messages: `in step "<label>"`, or `before any step`.
fn in_step(step: Option<&str>) -> String {
    match step {
        Some(step) => format!("in step \"{step}\""),
        None => "before any step".to_owned(),
    }
}

/// The first line of `text`, at most [`SUMMARY_CHARS`] characters, with `…` when more
/// was cut.
fn one_line(text: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let mut line: String = first.chars().take(SUMMARY_CHARS).collect();
    if line.len() < first.len() || lines.next().is_some() {
        line.push('…');
    }
    line
}

/// `n` with a comma between each group of three digits.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn truncation_line(truncated: Option<Truncated>) -> String {
    match truncated {
        Some(Truncated { kept, total }) => format!(
            "\ntruncated: kept {} of {} bytes\n",
            thousands(kept as u64),
            thousands(total as u64)
        ),
        None => String::new(),
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn longest_backtick_run(text: &str) -> usize {
    let (mut longest, mut run) = (0, 0);
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// `body` in a fenced block whose fence no backtick run inside it can close.
fn fenced(body: &str, lang: &str) -> String {
    let fence = "`".repeat(longest_backtick_run(body).max(2) + 1);
    let newline = if body.ends_with('\n') { "" } else { "\n" };
    format!("{fence}{lang}\n{body}{newline}{fence}\n")
}

/// `text` as inline code, with a longer delimiter when it holds backticks.
fn inline_code(text: &str) -> String {
    match longest_backtick_run(text) {
        0 => format!("`{text}`"),
        longest => {
            let ticks = "`".repeat(longest + 1);
            format!("{ticks} {text} {ticks}")
        }
    }
}

/// `value` as JSON, or a string saying why it is not.
fn to_value(value: &(impl Serialize + ?Sized)) -> Value {
    serde_json::to_value(value)
        .unwrap_or_else(|error| Value::String(format!("<not serializable: {error}>")))
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One line on the terminal for a log file that cannot be written; that log then writes
/// nothing more.
#[allow(
    clippy::print_stderr,
    reason = "the line belongs in libtest's captured output of the test whose log failed"
)]
fn report_write_failure(path: &Path, error: &io::Error) {
    eprintln!("test log: cannot write {}: {error}", path.display());
}

/// The guard's lines, printed while the test panics.
#[allow(
    clippy::print_stderr,
    reason = "the guard's lines belong in libtest's captured output of the failing test, beside its panic message"
)]
fn print_guard(transcript: &Path, records: u64, last: &[String]) {
    // Captured by libtest, so this cannot fail and panic while unwinding; only a
    // `--nocapture` run with a closed stderr could.
    eprintln!("test log: {}", transcript.display());
    eprintln!("last {} of {records} records:", last.len());
    for line in last {
        eprintln!("  {line}");
    }
}

/// A log directory made afresh: an earlier run's removed, both files created empty.
fn prepare(dir: &Path) -> Result<(File, File), (PathBuf, io::Error)> {
    match fs::remove_dir_all(dir) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err((dir.to_path_buf(), error));
        }
        _ => {}
    }
    fs::create_dir_all(dir).map_err(|error| (dir.to_path_buf(), error))?;
    let open = |name: &str| {
        let path = dir.join(name);
        OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .map_err(|error| (path, error))
    };
    Ok((open("steps.jsonl")?, open("transcript.md")?))
}

/// The state behind a log's one lock.
struct Inner {
    suite: String,
    test: String,
    /// `<suite>::<test>`, the live echo's prefix.
    echo_name: String,
    mode: Mode,
    caps: Caps,
    started: Instant,
    steps_path: PathBuf,
    transcript_path: PathBuf,
    /// `None` once a write failed.
    files: Option<(File, File)>,
    /// Shared with the log's cases.
    budget: Arc<Mutex<Budget>>,
    seq: u64,
    /// Records in `steps.jsonl`.
    written: u64,
    /// Bytes this log wrote to both files.
    bytes: u64,
    /// Records the cap kept out.
    dropped: u64,
    capped: bool,
    runs: u64,
    step_index: u64,
    step: Option<String>,
    scrubber: Scrubber,
    paths: Relativizer,
    /// The last [`LAST_STEPS_SHOWN`] summaries, `#<seq> <summary>`.
    ring: VecDeque<String>,
    ended: bool,
}

impl Inner {
    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// `text` as a log may hold it: secrets scrubbed, paths rewritten.
    fn clean(&self, text: &str) -> String {
        self.paths.rewrite(&self.scrubber.scrub(text))
    }

    fn clean_capped(&self, text: &str) -> (String, Option<Truncated>) {
        let text = self.clean(text);
        let (kept, truncated) = cap_text(&text, self.caps.stream_bytes);
        (kept.to_owned(), truncated)
    }

    /// `value` with every string and key cleaned.
    fn clean_value(&self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.clean(&text)),
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|item| self.clean_value(item))
                    .collect(),
            ),
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, item)| (self.clean(&key), self.clean_value(item)))
                    .collect(),
            ),
            other => other,
        }
    }

    /// `value` cleaned, or, when its serialization passes the stream cap, `null` with
    /// the kept prefix of that serialization.
    fn capped_value(&self, value: Value) -> (Value, Option<String>, Option<Truncated>) {
        let value = self.clean_value(value);
        let text = value.to_string();
        match cap_text(&text, self.caps.stream_bytes) {
            (_, None) => (value, None, None),
            (kept, truncated) => (Value::Null, Some(kept.to_owned()), truncated),
        }
    }

    /// Records `record`: its summary in the ring (and on stderr when verbose), and both
    /// files while the budget admits it.
    fn emit(&mut self, record: Record) {
        self.show(&record);
        if self.files.is_none() {
            return;
        }
        if self.capped {
            self.dropped += 1;
            return;
        }
        let (line, text) = encode(&record);
        let bytes = (line.len() + text.len()) as u64;
        if lock(&self.budget).admit(bytes) {
            self.write(&line, &text);
            return;
        }
        self.dropped += 1;
        self.capped = true;
        let capped = Record::Capped {
            seq: record.seq(),
            test_bytes: self.caps.test_bytes,
        };
        self.show(&capped);
        let (line, text) = encode(&capped);
        lock(&self.budget).charge((line.len() + text.len()) as u64);
        self.write(&line, &text);
    }

    fn show(&mut self, record: &Record) {
        let summary = format!("#{} {}", record.seq(), record.summary());
        if self.mode == Mode::Verbose {
            let _ = writeln!(
                std::io::stderr().lock(),
                "[tlog {}] {summary}",
                self.echo_name
            );
        }
        if !matches!(record, Record::End { .. }) {
            if self.ring.len() == LAST_STEPS_SHOWN {
                self.ring.pop_front();
            }
            self.ring.push_back(summary);
        }
    }

    fn write(&mut self, line: &str, text: &str) {
        let Some((steps, transcript)) = self.files.as_mut() else {
            return;
        };
        let failed = match steps.write_all(line.as_bytes()) {
            Err(error) => Some((self.steps_path.clone(), error)),
            Ok(()) => transcript
                .write_all(text.as_bytes())
                .err()
                .map(|error| (self.transcript_path.clone(), error)),
        };
        match failed {
            Some((path, error)) => {
                report_write_failure(&path, &error);
                self.files = None;
            }
            None => {
                self.written += 1;
                self.bytes += (line.len() + text.len()) as u64;
            }
        }
    }

    /// The `end` record, once.
    fn finish(&mut self, panicking: bool) {
        if self.ended {
            return;
        }
        self.ended = true;
        let end = Record::End {
            seq: self.next_seq(),
            outcome: if panicking { "panicked" } else { "passed" },
            records: self.written + 1,
            runs: self.runs,
            bytes: self.bytes,
            dropped: self.dropped,
            elapsed_ms: millis(self.started.elapsed()),
        };
        self.show(&end);
        if self.files.is_some() {
            let (line, text) = encode(&end);
            lock(&self.budget).charge((line.len() + text.len()) as u64);
            self.write(&line, &text);
        }
    }
}

/// A record's `steps.jsonl` line and its transcript part.
fn encode(record: &Record) -> (String, String) {
    let mut line = serde_json::to_string(record).unwrap_or_else(|error| {
        format!(
            "{{\"kind\":\"unencodable\",\"seq\":{},\"error\":{}}}",
            record.seq(),
            Value::String(error.to_string())
        )
    });
    line.push('\n');
    (line, record.render())
}

/// One test's structured step log (see the module doc). `Send + Sync`: scoped threads
/// may share it.
pub struct TestLog {
    inner: Arc<Mutex<Inner>>,
    dir: PathBuf,
}

impl TestLog {
    /// The log of test `test` in suite `suite`, under `<log root>/<suite>/<test>/`,
    /// replacing an earlier run's. Panics when `OMADDONS_TEST_LOG` holds no log mode.
    #[track_caller]
    pub fn new(suite: &str, test: &str) -> Self {
        Self::with_caps(suite, test, Caps::default())
    }

    /// [`TestLog::new`] with other bounds.
    #[track_caller]
    pub fn with_caps(suite: &str, test: &str, caps: Caps) -> Self {
        let mode = match process_mode() {
            Ok(mode) => mode,
            // A test harness fails its test by panicking: an unknown mode fails loudly.
            Err(message) => panic!("{message}"), // ubs:ignore — fails the test
        };
        let (suite, test) = (sanitize_component(suite), sanitize_component(test));
        let dir = log_root().join(&suite).join(&test);
        let budget = Budget::with_reserve(caps.test_bytes, CAPPED_RESERVE_BYTES);
        Self::open(
            dir,
            suite,
            test,
            mode,
            caps,
            Arc::new(Mutex::new(budget)),
            None,
        )
    }

    /// The log [`tlog!`] makes, named from where it was called.
    #[doc(hidden)]
    #[track_caller]
    pub fn from_location(module_path: &str, fn_type_name: &str) -> Self {
        let (suite, test) = split_location(module_path, fn_type_name);
        Self::new(&suite, &test)
    }

    fn open(
        dir: PathBuf,
        suite: String,
        test: String,
        mode: Mode,
        caps: Caps,
        budget: Arc<Mutex<Budget>>,
        parent: Option<String>,
    ) -> Self {
        let files = match prepare(&dir) {
            Ok(files) => Some(files),
            Err((path, error)) => {
                report_write_failure(&path, &error);
                None
            }
        };
        let mut inner = Inner {
            echo_name: format!("{suite}::{test}"),
            suite,
            test,
            mode,
            caps,
            started: Instant::now(),
            steps_path: dir.join("steps.jsonl"),
            transcript_path: dir.join("transcript.md"),
            files,
            budget,
            seq: 0,
            written: 0,
            bytes: 0,
            dropped: 0,
            capped: false,
            runs: 0,
            step_index: 0,
            step: None,
            scrubber: Scrubber::new(),
            paths: Relativizer::new(),
            ring: VecDeque::with_capacity(LAST_STEPS_SHOWN),
            ended: false,
        };
        let header = Record::Header {
            seq: inner.next_seq(),
            format: FORMAT,
            suite: inner.suite.clone(),
            test: inner.test.clone(),
            started_at: jiff::Timestamp::now()
                .strftime("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string(),
            mode: mode.as_str(),
            caps,
            parent,
        };
        inner.emit(header);
        Self {
            inner: Arc::new(Mutex::new(inner)),
            dir,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    /// `<log root>/<suite>/<test>`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn steps_path(&self) -> PathBuf {
        self.dir.join("steps.jsonl")
    }

    pub fn transcript_path(&self) -> PathBuf {
        self.dir.join("transcript.md")
    }

    /// Opens a phase of the scenario: later records name it.
    pub fn step(&self, label: &str) {
        let mut inner = self.lock();
        let (label, _) = inner.clean_capped(label);
        inner.step_index += 1;
        inner.step = Some(label.clone());
        let record = Record::Step {
            seq: inner.next_seq(),
            index: inner.step_index,
            label,
            at_ms: millis(inner.started.elapsed()),
        };
        inner.emit(record);
    }

    pub fn note(&self, text: impl AsRef<str>) {
        let mut inner = self.lock();
        let (text, truncated) = inner.clean_capped(text.as_ref());
        let record = Record::Note {
            seq: inner.next_seq(),
            step: inner.step.clone(),
            text,
            truncated,
        };
        inner.emit(record);
    }

    /// Records `value` as JSON under `label`.
    pub fn json(&self, label: &str, value: &(impl Serialize + ?Sized)) {
        let value = to_value(value);
        let mut inner = self.lock();
        let (label, _) = inner.clean_capped(label);
        let (value, text, truncated) = inner.capped_value(value);
        let record = Record::Json {
            seq: inner.next_seq(),
            step: inner.step.clone(),
            label,
            value,
            text,
            truncated,
        };
        inner.emit(record);
    }

    /// Scrubs `value` from every record made after this one, when it is at least
    /// [`SCRUB_MIN_LEN`] bytes: call it before planting a credential in a file or feeding
    /// one on stdin.
    pub fn secret(&self, value: &str) {
        self.lock().scrubber.add(value);
    }

    /// Records both values and whether they are equal; when they are not, also a
    /// `failure` record, then panics naming the label, the step and the transcript.
    #[track_caller]
    pub fn assert_eq<T: Serialize + PartialEq + Debug>(
        &self,
        label: &str,
        actual: &T,
        expected: &T,
    ) {
        let equal = actual == expected;
        let (actual_value, expected_value) = (to_value(actual), to_value(expected));
        let failure = {
            let mut inner = self.lock();
            let (label, _) = inner.clean_capped(label);
            let (actual_value, actual_truncated) = as_recorded(inner.capped_value(actual_value));
            let (expected_value, expected_truncated) =
                as_recorded(inner.capped_value(expected_value));
            let step = inner.step.clone();
            let record = Record::Assert {
                seq: inner.next_seq(),
                step: step.clone(),
                label: label.clone(),
                actual: actual_value,
                expected: expected_value,
                equal,
                actual_truncated,
                expected_truncated,
            };
            inner.emit(record);
            if equal {
                None
            } else {
                let message = format!(
                    "assert \"{label}\" {}: expected {expected:?}, got {actual:?}",
                    in_step(step.as_deref())
                );
                let (logged, _) = inner.clean_capped(&message);
                let record = Record::Failure {
                    seq: inner.next_seq(),
                    step,
                    message: logged,
                };
                inner.emit(record);
                Some(inner.scrubber.scrub(&message))
            }
        };
        if let Some(message) = failure {
            // The test's assertion: a mismatch fails the test that made it.
            let transcript = self.transcript_path().display().to_string();
            panic!("tlog: {message}\n  transcript: {transcript}"); // ubs:ignore — the assertion
        }
    }
}

/// A capped value as an `assert` record holds it: a cut serialization becomes the string
/// of its kept prefix.
fn as_recorded(
    (value, text, truncated): (Value, Option<String>, Option<Truncated>),
) -> (Value, Option<Truncated>) {
    (text.map_or(value, Value::String), truncated)
}

impl Drop for TestLog {
    /// Writes the `end` record and, while the test panics, prints where the log is and
    /// its last records. Never panics: a panic while unwinding would abort the binary.
    fn drop(&mut self) {
        let panicking = std::thread::panicking();
        let (records, last) = {
            let mut inner = self.lock();
            let records = inner.seq;
            inner.finish(panicking);
            (records, inner.ring.iter().cloned().collect::<Vec<_>>())
        };
        if panicking {
            print_guard(&self.transcript_path(), records, &last);
        }
    }
}

/// A [`TestLog`] named after the test crate and the function it is called in:
/// `<log root>/<crate>/<module path>.<fn>/`. Call it in the test function itself.
#[allow(unused_macros)]
macro_rules! tlog {
    () => {{
        fn __tlog_here() {}
        $crate::common::tlog::TestLog::from_location(
            module_path!(),
            ::std::any::type_name_of_val(&__tlog_here),
        )
    }};
}
#[allow(unused_imports)]
pub(crate) use tlog;
