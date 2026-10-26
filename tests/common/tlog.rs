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
//! **Runs.** [`TestLog::run`] runs the real binary on a [`Runner`] (a `TestEnv`, a
//! `Home`, a `Scene`) with exactly the argv given (nothing is appended, `--json`
//! included), and [`TestLog::cmd`] sets one up first: extra variables, a working
//! directory, stdin, failpoints, a `pause` directory, a timeout ([`RUN_TIMEOUT`] by
//! default, after which the child is killed and the test fails), or the background
//! ([`Cmd::spawn`], then [`Running::wait`]). Each run is one `run` record: its argv, the
//! variables it set beyond its environment's base (secret-named ones `<redacted>`), its
//! working directory, the failpoints armed, stdin's length (never its content), its
//! times, its exit or signal, stdout (the envelope parsed when it is one, its keys then
//! sorted), stderr (each NDJSON event parsed) and the `net.log` lines it appended. Two runs in parallel on one
//! home each see the other's `net.log` lines. The first run on a `TestEnv` writes an
//! `env` record with that environment's base variables. The returned [`Ran`] holds the
//! raw output; its assertions (`ok`, `exit`, `fails`, `aborted`, `envelope`, `data`, …)
//! write a `failure` record and fail the test naming the run, its step, the exit, the
//! envelope's error, the argv and the transcript.
//!
//! **Pauses, trees and cases.** [`TestLog::reached`] and [`TestLog::resume`] record a
//! `pause` failpoint's interleaving. [`TestLog::tree`] records a tree's `tree_hash` under
//! a label; [`TestLog::assert_unchanged`] fails naming the first [`DIFF_PATHS_SHOWN`]
//! paths that differ since, and [`TestLog::diff`] returns the difference for the test to
//! assert on. All three refuse a root outside the test's synthetic directories before
//! reading it. [`TestLog::case`] gives each case of a table-driven or parallel test a
//! sub-log of its own under `cases/<name>/`, within the parent's byte budget.
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

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsStr;
use std::fmt::Debug;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::engine::Scene;
use super::env::TestEnv;
use super::home::{Home, Run};
use super::pause::Pause;
use super::privacy::PRIVATE;
use super::trees::{Entry, TreeDiff, tree_diff, tree_entries, tree_hash};

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
/// The crate-doc line that marks a test crate as an end-to-end suite for `argc e2e`,
/// followed by the suite's purpose.
pub const E2E_MARKER: &str = "//! e2e suite:";

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
/// The longest argv a run's one-line summary shows.
const SUMMARY_ARGV_CHARS: usize = 60;
/// How often a waiting run checks whether its child exited.
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// The signal an `abort` failpoint dies by.
const SIGABRT: i32 = 6;

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
    /// A `TestEnv` the log met: its placeholder and base variables.
    Env {
        seq: u64,
        name: String,
        vars: BTreeMap<String, String>,
    },
    Run(Box<RunRecord>),
    Pause {
        seq: u64,
        step: Option<String>,
        action: &'static str,
        name: String,
        dir: String,
        waited_ms: Option<u64>,
    },
    Tree {
        seq: u64,
        step: Option<String>,
        label: String,
        root: String,
        exists: bool,
        hash: Option<String>,
        entries: usize,
    },
    TreeCheck {
        seq: u64,
        step: Option<String>,
        label: String,
        root: String,
        check: &'static str,
        expected: Option<String>,
        actual: Option<String>,
        equal: bool,
        diff: Option<DiffRecord>,
    },
    Case {
        seq: u64,
        step: Option<String>,
        name: String,
        dir: String,
    },
}

/// A `run` record (Technical Approach 4 of the bead).
#[derive(Serialize)]
struct RunRecord {
    seq: u64,
    step: Option<String>,
    env: String,
    argv: Vec<String>,
    cwd: String,
    overrides: BTreeMap<String, String>,
    failpoints: Option<FailpointsRecord>,
    stdin_bytes: Option<usize>,
    background: bool,
    started_at: String,
    elapsed_ms: u64,
    exit: i32,
    signal: Option<i32>,
    timed_out: bool,
    stdout: Stdout,
    stderr: Stderr,
    net_log: NetLogAdded,
    scrubbed: Vec<String>,
}

/// The failpoints a run armed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct FailpointsRecord {
    spec: Option<String>,
    pause_dir: Option<String>,
}

/// A tree check's difference: how many paths, and the first ones.
#[derive(Serialize)]
struct DiffRecord {
    total: usize,
    first: Vec<String>,
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
            | Record::End { seq, .. }
            | Record::Env { seq, .. }
            | Record::Pause { seq, .. }
            | Record::Tree { seq, .. }
            | Record::TreeCheck { seq, .. }
            | Record::Case { seq, .. } => *seq,
            Record::Run(run) => run.seq,
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
            Record::Env { name, .. } => format!("env {name}"),
            Record::Run(run) => {
                let argv: String = run.argv.join(" ");
                let mut shown: String = argv.chars().take(SUMMARY_ARGV_CHARS).collect();
                if shown.len() < argv.len() {
                    shown.push('…');
                }
                format!("run {shown} → {}", run_ending(run))
            }
            Record::Pause {
                action,
                name,
                waited_ms,
                ..
            } => match waited_ms {
                Some(ms) => format!("pause {name}: {action} ({ms} ms)"),
                None => format!("pause {name}: {action}"),
            },
            Record::Tree {
                label,
                hash,
                entries,
                ..
            } => match hash {
                Some(hash) => format!("tree {label}: {} ({entries} entries)", short_hash(hash)),
                None => format!("tree {label}: absent"),
            },
            Record::TreeCheck {
                label,
                check,
                equal,
                diff,
                ..
            } => {
                let paths = diff.as_ref().map_or(0, |d| d.total);
                match (*check, *equal) {
                    ("unchanged", true) => format!("tree check {label}: unchanged"),
                    ("unchanged", false) => format!("tree check {label}: changed, {paths} paths"),
                    _ => format!("tree diff {label}: {paths} paths"),
                }
            }
            Record::Case { name, .. } => format!("case {name}"),
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
            Record::Env { name, vars, .. } => {
                let vars: Vec<String> = vars.iter().map(|(k, v)| format!("{k}={v}")).collect();
                format!("- env {}: {}\n", inline_code(name), vars.join(" "))
            }
            Record::Run(run) => render_run(run),
            Record::Pause {
                seq,
                action,
                name,
                waited_ms,
                ..
            } => {
                let after = waited_ms
                    .map(|ms| format!(" after {ms} ms"))
                    .unwrap_or_default();
                format!(
                    "\n### #{seq} pause {}: {action}{after}\n",
                    inline_code(name)
                )
            }
            Record::Tree {
                seq,
                label,
                root,
                hash,
                entries,
                ..
            } => {
                let label = inline_code(label);
                let heading = match hash {
                    Some(hash) => format!(
                        "tree {label} = {}… ({} entries)",
                        short_hash(hash),
                        thousands(*entries as u64)
                    ),
                    None => format!("tree {label}: absent"),
                };
                format!("\n### #{seq} {heading}\n\n- root {}\n", inline_code(root))
            }
            Record::TreeCheck {
                seq,
                label,
                check,
                equal,
                diff,
                ..
            } => {
                let label = inline_code(label);
                let total = diff.as_ref().map_or(0, |d| d.total);
                let heading = match (*check, *equal) {
                    ("unchanged", true) => format!("tree check {label}: unchanged"),
                    ("unchanged", false) => format!("tree check {label}: changed ({total} paths)"),
                    _ => format!("tree diff {label}: {total} paths"),
                };
                let mut out = format!("\n### #{seq} {heading}\n");
                if let Some(diff) = diff.as_ref().filter(|d| !d.first.is_empty()) {
                    out.push('\n');
                    for line in &diff.first {
                        out.push_str(&format!("- {}\n", inline_code(line)));
                    }
                    if diff.total > diff.first.len() {
                        out.push_str(&format!("- … {} more\n", diff.total - diff.first.len()));
                    }
                }
                out
            }
            Record::Case { seq, name, dir, .. } => {
                format!(
                    "\n### #{seq} case {} → {dir}/transcript.md\n",
                    inline_code(name)
                )
            }
        }
    }
}

/// How a run ended, for its heading and summary.
fn run_ending(run: &RunRecord) -> String {
    let ms = thousands(run.elapsed_ms);
    if run.timed_out {
        format!("timed out after {ms} ms, killed")
    } else if let Some(signal) = run.signal {
        format!("killed by signal {signal} ({ms} ms)")
    } else {
        format!("exit {} ({ms} ms)", run.exit)
    }
}

/// A run record's part of the transcript.
fn render_run(run: &RunRecord) -> String {
    let overrides = if run.overrides.is_empty() {
        "none".to_owned()
    } else {
        let vars: Vec<String> = run
            .overrides
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        vars.join(" ")
    };
    let failpoints = match &run.failpoints {
        None => "none".to_owned(),
        Some(armed) => {
            let mut text = armed
                .spec
                .clone()
                .unwrap_or_else(|| "none armed".to_owned());
            if let Some(dir) = &armed.pause_dir {
                text.push_str(&format!(" (pause dir {dir})"));
            }
            text
        }
    };
    let stdin = run.stdin_bytes.map_or_else(
        || "none".to_owned(),
        |n| format!("{} bytes", thousands(n as u64)),
    );
    let rotated = if run.net_log.rotated {
        " (rotated)"
    } else {
        ""
    };
    let background = if run.background { " · background" } else { "" };
    let mut out = format!(
        "\n### #{} run {} → {}\n\n- cwd `{}` · overrides {overrides} · failpoints {failpoints} · stdin {stdin} · net.log +{}{rotated}{background}\n",
        run.seq,
        inline_code(&run.argv.join(" ")),
        run_ending(run),
        run.cwd,
        run.net_log.added.len()
    );
    match &run.stdout {
        Stdout::Empty { .. } => out.push_str("\nstdout: empty\n"),
        Stdout::Envelope { value, .. } => {
            out.push_str(&format!(
                "\nstdout (envelope):\n\n{}",
                fenced(&pretty(value), "json")
            ));
        }
        Stdout::Text {
            text, truncated, ..
        } => out.push_str(&format!(
            "\nstdout (text):\n\n{}{}",
            fenced(text, "text"),
            truncation_line(*truncated)
        )),
    }
    if run.stderr.lines.is_empty() {
        out.push_str("\nstderr: empty\n");
    } else {
        let lines: Vec<String> = run.stderr.lines.iter().map(line_text).collect();
        let plural = if lines.len() == 1 { "" } else { "s" };
        out.push_str(&format!(
            "\nstderr ({} line{plural}):\n\n{}{}",
            lines.len(),
            fenced(&lines.join("\n"), "json"),
            truncation_line(run.stderr.truncated)
        ));
    }
    if !run.net_log.added.is_empty() {
        let lines: Vec<String> = run.net_log.added.iter().map(line_text).collect();
        out.push_str(&format!(
            "\nnet.log (+{}{rotated}):\n\n{}",
            lines.len(),
            fenced(&lines.join("\n"), "json")
        ));
    }
    out
}

/// A parsed line as it was written: a string as is, JSON compact.
fn line_text(line: &Value) -> String {
    match line {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The first 8 hex digits of a hash.
fn short_hash(hash: &str) -> &str {
    hash.get(..8).unwrap_or(hash)
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
    /// Each tree label's last hash and entries.
    trees: BTreeMap<String, (Option<String>, BTreeMap<String, Entry>)>,
}

/// What a new log starts from: its place and names, and what a case inherits from its
/// parent.
struct Setup {
    dir: PathBuf,
    suite: String,
    test: String,
    echo_name: String,
    mode: Mode,
    caps: Caps,
    budget: Arc<Mutex<Budget>>,
    parent: Option<String>,
    scrubber: Scrubber,
    paths: Relativizer,
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

    /// The record's summary on stderr when verbose (`[tlog <suite>::<test> #<seq>]
    /// <summary>`), and in the ring (`#<seq> <summary>`).
    fn show(&mut self, record: &Record) {
        let (seq, summary) = (record.seq(), record.summary());
        if self.mode == Mode::Verbose {
            let name = &self.echo_name;
            let _ = writeln!(std::io::stderr().lock(), "[tlog {name} #{seq}] {summary}");
        }
        if !matches!(record, Record::End { .. }) {
            if self.ring.len() == LAST_STEPS_SHOWN {
                self.ring.pop_front();
            }
            self.ring.push_back(format!("#{seq} {summary}"));
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
        Self::open(Setup {
            dir,
            echo_name: format!("{suite}::{test}"),
            suite,
            test,
            mode,
            caps,
            budget: Arc::new(Mutex::new(budget)),
            parent: None,
            scrubber: Scrubber::new(),
            paths: Relativizer::new(),
        })
    }

    /// The log [`tlog!`] makes, named from where it was called.
    #[doc(hidden)]
    #[track_caller]
    pub fn from_location(module_path: &str, fn_type_name: &str) -> Self {
        let (suite, test) = split_location(module_path, fn_type_name);
        Self::new(&suite, &test)
    }

    fn open(setup: Setup) -> Self {
        let Setup {
            dir,
            suite,
            test,
            echo_name,
            mode,
            caps,
            budget,
            parent,
            scrubber,
            paths,
        } = setup;
        let files = match prepare(&dir) {
            Ok(files) => Some(files),
            Err((path, error)) => {
                report_write_failure(&path, &error);
                None
            }
        };
        let mut inner = Inner {
            echo_name,
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
            scrubber,
            paths,
            ring: VecDeque::with_capacity(LAST_STEPS_SHOWN),
            ended: false,
            trees: BTreeMap::new(),
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

// ---------------------------------------------------------------------------------------
// Runs of the binary.

/// Where [`TestLog::run`] runs the binary: a synthetic environment, and the command for
/// an argv.
pub trait Runner {
    fn test_env(&self) -> &TestEnv;
    /// The binary with this home's environment, and `argv` (without the binary's name).
    fn command(&self, argv: &[String]) -> Command;
}

impl Runner for TestEnv {
    fn test_env(&self) -> &TestEnv {
        self
    }

    fn command(&self, argv: &[String]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_omaddons"));
        self.apply_to(&mut command);
        command.args(argv);
        command
    }
}

impl Runner for Home {
    fn test_env(&self) -> &TestEnv {
        &self.env
    }

    fn command(&self, argv: &[String]) -> Command {
        self.std_command(argv, &[])
    }
}

impl Runner for Scene {
    fn test_env(&self) -> &TestEnv {
        &self.home.env
    }

    fn command(&self, argv: &[String]) -> Command {
        self.home.std_command(argv, &[])
    }
}

/// The variables a `TestEnv` sets on every child, as `TestEnv::apply_to` sets them, read
/// back from a command it was applied to.
pub fn base_vars(env: &TestEnv) -> BTreeMap<String, String> {
    let mut probe = Command::new("omaddons");
    env.apply_to(&mut probe);
    probe
        .get_envs()
        .filter_map(|(key, value)| {
            let value = value?;
            let key = key.to_string_lossy().into_owned();
            Some((key, value.to_string_lossy().into_owned()))
        })
        .collect()
}

/// The variables a command sets beyond its environment's base, and the secrets among
/// them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    /// Every variable set whose value differs from the base or that the base lacks; a
    /// secret-named one is `<redacted>`. Removals are not listed.
    pub vars: BTreeMap<String, String>,
    /// The secret-named ones, by name and value.
    pub secrets: Vec<(String, String)>,
}

pub fn overrides(base: &BTreeMap<String, String>, command: &Command) -> Overrides {
    let mut out = Overrides::default();
    for (key, value) in command.get_envs() {
        let Some(value) = value else { continue };
        let key = key.to_string_lossy().into_owned();
        let value = value.to_string_lossy().into_owned();
        if base.get(&key) == Some(&value) {
            continue;
        }
        if is_secret_name(&key) {
            out.vars.insert(key.clone(), REDACTED.to_owned());
            out.secrets.push((key, value));
        } else {
            out.vars.insert(key, value);
        }
    }
    out
}

/// A run's stdout as its record holds it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "form", rename_all = "snake_case")]
pub enum Stdout {
    /// Exactly one line (a trailing newline allowed) that parses as a JSON object.
    Envelope {
        value: Value,
        bytes: usize,
        truncated: Option<Truncated>,
    },
    Text {
        text: String,
        bytes: usize,
        truncated: Option<Truncated>,
    },
    Empty {
        bytes: usize,
        truncated: Option<Truncated>,
    },
}

/// `text` (a run's stdout) classified, cut at `cap`: a cut envelope is text, its prefix
/// kept.
pub fn classify_stdout(text: &str, cap: usize) -> Stdout {
    let bytes = text.len();
    if text.is_empty() {
        return Stdout::Empty {
            bytes,
            truncated: None,
        };
    }
    let (kept, truncated) = cap_text(text, cap);
    let line = text.strip_suffix('\n').unwrap_or(text);
    if truncated.is_none()
        && !line.contains('\n')
        && let Ok(value @ Value::Object(_)) = serde_json::from_str::<Value>(line)
    {
        return Stdout::Envelope {
            value,
            bytes,
            truncated,
        };
    }
    Stdout::Text {
        text: kept.to_owned(),
        bytes,
        truncated,
    }
}

/// A run's stderr as its record holds it: each line parsed as JSON when it parses (an
/// NDJSON event), else the raw string, in order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stderr {
    pub lines: Vec<Value>,
    pub bytes: usize,
    pub truncated: Option<Truncated>,
}

pub fn parse_stderr(text: &str, cap: usize) -> Stderr {
    let (kept, truncated) = cap_text(text, cap);
    Stderr {
        lines: kept.lines().map(parse_line).collect(),
        bytes: text.len(),
        truncated,
    }
}

/// `line` as JSON, or as the string it is.
fn parse_line(line: &str) -> Value {
    serde_json::from_str(line).unwrap_or_else(|_| Value::String(line.to_owned()))
}

/// The `net.log` lines a run appended.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NetLogAdded {
    pub added: Vec<Value>,
    /// The log rotated meanwhile, so `added` is the whole current file.
    pub rotated: bool,
}

/// What a run appended to `net.log`. `before` is `net.log.1` then `net.log` as the run
/// started; `after_old` and `after_current` are the two files once it exited. When
/// `before` is a prefix of the two after files, the rest is what the run added;
/// otherwise the log rotated, and every line of the current file counts. Lines are read
/// raw, so a malformed one never breaks the log: it is kept as a string.
pub fn net_log_added(
    before: &[String],
    after_old: &[String],
    after_current: &[String],
) -> NetLogAdded {
    let after: Vec<&String> = after_old.iter().chain(after_current).collect();
    let is_prefix =
        before.len() <= after.len() && before.iter().zip(&after).all(|(was, now)| was == *now);
    let (lines, rotated) = if is_prefix {
        (after[before.len()..].to_vec(), false)
    } else {
        (after_current.iter().collect(), true)
    };
    NetLogAdded {
        added: lines.into_iter().map(|line| parse_line(line)).collect(),
        rotated,
    }
}

/// The lines of `net.log.1` and `net.log` under a state directory; a missing file has
/// none.
fn net_log_lines(state: &Path) -> (Vec<String>, Vec<String>) {
    let read = |name: &str| {
        let bytes = fs::read(state.join("omaddons").join(name)).unwrap_or_default();
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::to_owned)
            .collect()
    };
    (read("net.log.1"), read("net.log"))
}

/// The failpoints a command arms, from its variables.
fn failpoints_of(command: &Command) -> Option<FailpointsRecord> {
    let var = |name: &str| {
        command
            .get_envs()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned())
    };
    let armed = FailpointsRecord {
        spec: var("OMADDONS_FAILPOINTS"),
        pause_dir: var("OMADDONS_FAILPOINT_DIR"),
    };
    (armed.spec.is_some() || armed.pause_dir.is_some()).then_some(armed)
}

/// A run being set up: [`TestLog::cmd`], options, then [`Cmd::run`] or [`Cmd::spawn`].
pub struct Cmd<'a> {
    log: &'a TestLog,
    root: PathBuf,
    state: PathBuf,
    base: BTreeMap<String, String>,
    argv: Vec<String>,
    command: Command,
    stdin: Option<Vec<u8>>,
    cwd: Option<PathBuf>,
    timeout: Duration,
}

impl Cmd<'_> {
    pub fn env(mut self, key: &str, value: impl AsRef<OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    pub fn cwd(mut self, dir: &Path) -> Self {
        self.cwd = Some(dir.to_path_buf());
        self
    }

    /// Feeds `bytes` on standard input; the record holds only their length.
    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    /// Arms failpoints: `OMADDONS_FAILPOINTS=<spec>`.
    pub fn failpoints(self, spec: &str) -> Self {
        self.env("OMADDONS_FAILPOINTS", spec)
    }

    /// Points the child's `pause` failpoints at `pause`'s markers.
    pub fn pause(self, pause: &Pause) -> Self {
        let (key, value) = pause.env();
        self.env(key, value)
    }

    /// How long the run may take before it is killed ([`RUN_TIMEOUT`] by default).
    pub fn timeout(mut self, limit: Duration) -> Self {
        self.timeout = limit;
        self
    }

    /// Runs to the end, records the run and returns it.
    #[track_caller]
    pub fn run(self) -> Ran {
        self.start(false).wait()
    }

    /// Starts the run in the background; [`Running::wait`] records it.
    #[track_caller]
    pub fn spawn(self) -> Running {
        self.start(true)
    }

    #[track_caller]
    fn start(self, background: bool) -> Running {
        let Cmd {
            log,
            root,
            state,
            base,
            argv,
            mut command,
            stdin,
            cwd,
            timeout,
        } = self;
        if let Some(dir) = &cwd {
            command.current_dir(dir);
        }
        let overrides = overrides(&base, &command);
        let failpoints = failpoints_of(&command);
        let before = net_log_lines(&state);
        let (seq, step, env) = log.begin_run(&root, &base, &overrides.secrets);
        let stdin_pipe = if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        };
        command
            .stdin(stdin_pipe)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started_at = now_rfc3339();
        let started = Instant::now();
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => log.fail(&format!(
                "run #{seq} {}: the binary did not start: {error}",
                in_step(step.as_deref())
            )),
        };
        let stdin_bytes = stdin.as_ref().map(Vec::len);
        let writer = stdin.and_then(|bytes| {
            let mut pipe = child.stdin.take()?;
            // Written, then closed by the drop, so the child sees the end of its input.
            Some(std::thread::spawn(move || {
                let _ = pipe.write_all(&bytes);
            }))
        });
        let out = read_all(child.stdout.take());
        let err = read_all(child.stderr.take());
        Running {
            child: Some(child),
            threads: Some((writer, out, err)),
            started,
            deadline: started + timeout,
            started_run: StartedRun {
                seq,
                step,
                env,
                argv,
                cwd,
                overrides,
                failpoints,
                stdin_bytes,
                background,
                started_at,
                state,
                before,
            },
            log: Arc::downgrade(&log.inner),
            transcript: log.transcript_path(),
        }
    }
}

/// What a run's record needs that is known when it starts.
struct StartedRun {
    seq: u64,
    step: Option<String>,
    env: String,
    argv: Vec<String>,
    cwd: Option<PathBuf>,
    overrides: Overrides,
    failpoints: Option<FailpointsRecord>,
    stdin_bytes: Option<usize>,
    background: bool,
    started_at: String,
    state: PathBuf,
    /// `net.log.1` and `net.log` as the run started.
    before: (Vec<String>, Vec<String>),
}

/// What a run's end adds to its record.
struct Ended<'a> {
    run: &'a Run,
    signal: Option<i32>,
    timed_out: bool,
    elapsed_ms: u64,
    net_log: NetLogAdded,
}

/// A run's stdin writer and its two output readers.
type Threads = (
    Option<JoinHandle<()>>,
    JoinHandle<Vec<u8>>,
    JoinHandle<Vec<u8>>,
);

/// Everything `pipe` yields, read on a thread of its own.
fn read_all<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

fn now_rfc3339() -> String {
    jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// How a run ended.
struct Finished {
    status: Option<ExitStatus>,
    timed_out: bool,
    elapsed: Duration,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// A run started in the background ([`Cmd::spawn`]). Dropped without [`Running::wait`],
/// it kills its child and records the run with a `failure` after it, so no child
/// outlives its test.
pub struct Running {
    child: Option<Child>,
    threads: Option<Threads>,
    started: Instant,
    deadline: Instant,
    started_run: StartedRun,
    log: Weak<Mutex<Inner>>,
    transcript: PathBuf,
}

impl Running {
    pub fn pid(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    /// Waits for the run to end, records it and returns it. A run still going at its
    /// deadline is killed, recorded with `timed_out: true`, and fails the test.
    #[track_caller]
    pub fn wait(mut self) -> Ran {
        let finished = self.finish(false);
        let ran = self.record(finished);
        if ran.timed_out {
            ran.fail(&format!(
                "timed out after {} ms and was killed",
                thousands(ran.elapsed_ms)
            ));
        }
        ran
    }

    /// Waits for the child (killing it at its deadline, or at once with `kill_now`) and
    /// collects its output.
    fn finish(&mut self, kill_now: bool) -> Finished {
        let mut timed_out = false;
        let status = match self.child.take() {
            None => None,
            Some(mut child) => loop {
                if !kill_now {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) => {}
                        Err(_) => break child.wait().ok(),
                    }
                }
                if kill_now || Instant::now() >= self.deadline {
                    timed_out = !kill_now;
                    let _ = child.kill();
                    break child.wait().ok();
                }
                std::thread::sleep(POLL_INTERVAL);
            },
        };
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        if let Some((writer, out, err)) = self.threads.take() {
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            stdout = out.join().unwrap_or_default();
            stderr = err.join().unwrap_or_default();
        }
        Finished {
            status,
            timed_out,
            elapsed: self.started.elapsed(),
            stdout,
            stderr,
        }
    }

    /// Writes the run's record (while its log exists) and returns the run.
    fn record(&self, finished: Finished) -> Ran {
        let started = &self.started_run;
        let run = Run {
            argv: started.argv.clone(),
            code: finished.status.and_then(|s| s.code()).unwrap_or(-1),
            stdout: String::from_utf8_lossy(&finished.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&finished.stderr).into_owned(),
        };
        let signal = finished.status.and_then(|s| s.signal());
        let elapsed_ms = millis(finished.elapsed);
        let before: Vec<String> = started
            .before
            .0
            .iter()
            .chain(&started.before.1)
            .cloned()
            .collect();
        let (old, current) = net_log_lines(&started.state);
        if let Some(inner) = self.log.upgrade() {
            let ended = Ended {
                run: &run,
                signal,
                timed_out: finished.timed_out,
                elapsed_ms,
                net_log: net_log_added(&before, &old, &current),
            };
            lock(&inner).record_run(started, ended);
        }
        let envelope = match classify_stdout(&run.stdout, usize::MAX) {
            Stdout::Envelope { value, .. } => Some(value),
            _ => None,
        };
        Ran {
            seq: started.seq,
            step: started.step.clone().unwrap_or_default(),
            elapsed_ms,
            signal,
            timed_out: finished.timed_out,
            envelope,
            transcript: self.transcript.clone(),
            log: self.log.clone(),
            run,
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.child.is_none() {
            return;
        }
        let finished = self.finish(true);
        let ran = self.record(finished);
        if let Some(inner) = self.log.upgrade() {
            let message = format!(
                "run #{} was dropped without wait(); its child was killed",
                ran.seq
            );
            lock(&inner).failure(&message);
        }
    }
}

/// A suggested next step from an envelope's `commands`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Suggested {
    pub action: String,
    pub command: String,
    pub argv: Vec<String>,
    pub destructive: bool,
}

/// One finished, recorded run. Its assertions return `&Self` for chaining in one
/// statement; a failing one writes a `failure` record and fails the test naming the run,
/// its step, the exit, the envelope's error, the argv and the transcript.
pub struct Ran {
    /// The raw run (absolute paths, nothing scrubbed): assertions check what the binary
    /// printed, and only the log is rewritten.
    pub run: Run,
    pub seq: u64,
    /// The phase it ran in; empty before any step.
    pub step: String,
    pub elapsed_ms: u64,
    /// The signal that killed it: `Some(6)` for an `abort` failpoint (`run.code` is then
    /// -1).
    pub signal: Option<i32>,
    pub timed_out: bool,
    envelope: Option<Value>,
    transcript: PathBuf,
    log: Weak<Mutex<Inner>>,
}

impl Ran {
    /// Exit 0, and `ok: true` when stdout is an envelope.
    #[track_caller]
    pub fn ok(&self) -> &Self {
        if !self.exited(0) {
            self.fail(&format!("expected exit 0, {}", self.got()));
        }
        if self.envelope.as_ref().is_some_and(|e| e["ok"] != true) {
            self.fail("exit 0, but the envelope is not ok: true");
        }
        self
    }

    #[track_caller]
    pub fn exit(&self, code: i32) -> &Self {
        if !self.exited(code) {
            self.fail(&format!("expected exit {code}, {}", self.got()));
        }
        self
    }

    /// Exit `exit`, with `error.code` `code`.
    #[track_caller]
    pub fn fails(&self, exit: i32, code: &str) -> &Self {
        self.exit(exit);
        if self.error_code() != Some(code) {
            let got = self.error_code().unwrap_or("no error code");
            self.fail(&format!("expected error {code}, got {got}"));
        }
        self
    }

    /// Killed by `SIGABRT` (an `abort` failpoint).
    #[track_caller]
    pub fn aborted(&self) -> &Self {
        if self.signal != Some(SIGABRT) {
            self.fail(&format!("expected to be killed by SIGABRT, {}", self.got()));
        }
        self
    }

    /// The one JSON envelope stdout is.
    #[track_caller]
    pub fn envelope(&self) -> &Value {
        match &self.envelope {
            Some(envelope) => envelope,
            None => self.fail("expected stdout to be exactly one JSON envelope line"),
        }
    }

    /// The envelope's `data`, which must be there.
    #[track_caller]
    pub fn data(&self) -> &Value {
        match self.envelope().get("data") {
            Some(data) => data,
            None => self.fail("the envelope has no data"),
        }
    }

    pub fn error_code(&self) -> Option<&str> {
        self.envelope.as_ref()?.get("error")?.get("code")?.as_str()
    }

    /// The codes of the envelope's `warnings`, in order.
    #[track_caller]
    pub fn warnings(&self) -> Vec<String> {
        let warnings = self.envelope()["warnings"].as_array();
        warnings
            .into_iter()
            .flatten()
            .map(|warning| warning["code"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// The envelope's `commands`.
    #[track_caller]
    pub fn commands(&self) -> Vec<Suggested> {
        let commands = self.envelope()["commands"].clone();
        match serde_json::from_value(commands) {
            Ok(commands) => commands,
            Err(error) => self.fail(&format!("the envelope's commands do not read: {error}")),
        }
    }

    /// Every stderr line, each of which must be JSON (an NDJSON event).
    #[track_caller]
    pub fn events(&self) -> Vec<Value> {
        let mut events = Vec::new();
        for (index, line) in self.run.stderr.lines().enumerate() {
            match serde_json::from_str(line) {
                Ok(event) => events.push(event),
                Err(_) => self.fail(&format!("stderr line {} is not JSON: {line}", index + 1)),
            }
        }
        events
    }

    /// Neither stream names an account, realm or character of
    /// `common::privacy::PRIVATE`, in any case.
    #[track_caller]
    pub fn assert_private(&self) -> &Self {
        for (stream, text) in [("stdout", &self.run.stdout), ("stderr", &self.run.stderr)] {
            let folded = text.to_lowercase();
            if let Some(name) = PRIVATE
                .iter()
                .find(|name| folded.contains(&name.to_lowercase()))
            {
                self.fail(&format!("{stream} names {name}"));
            }
        }
        self
    }

    pub fn stdout(&self) -> &str {
        &self.run.stdout
    }

    pub fn stderr(&self) -> &str {
        &self.run.stderr
    }

    fn exited(&self, code: i32) -> bool {
        self.signal.is_none() && !self.timed_out && self.run.code == code
    }

    /// How the run ended, for messages.
    fn got(&self) -> String {
        match self.signal {
            _ if self.timed_out => "timed out".to_owned(),
            Some(signal) => format!("killed by signal {signal}"),
            None => format!("got {}", self.run.code),
        }
    }

    /// Writes a `failure` record (while the log exists) and fails the test.
    #[track_caller]
    fn fail(&self, what: &str) -> ! {
        let step = (!self.step.is_empty()).then_some(self.step.as_str());
        let mut message = format!("run #{} {}: {what}", self.seq, in_step(step));
        let error = self.envelope.as_ref().and_then(|e| e.get("error"));
        if let Some(error) = error.filter(|error| error.is_object()) {
            let code = error["code"].as_str().unwrap_or_default();
            let text = error["message"].as_str().unwrap_or_default();
            message.push_str(&format!("\n  error: {code}: {text}"));
        }
        let argv = std::iter::once("omaddons").chain(self.run.argv.iter().map(String::as_str));
        message.push_str(&format!("\n  argv: {}", shell_words::join(argv)));
        if let Some(inner) = self.log.upgrade() {
            let mut inner = lock(&inner);
            inner.failure(&message);
            message = inner.scrubber.scrub(&message);
        }
        let transcript = self.transcript.display().to_string();
        panic!("tlog: {message}\n  transcript: {transcript}"); // ubs:ignore — the assertion
    }
}

impl TestLog {
    /// Runs `omaddons <argv>` on `on` (the argv used verbatim: nothing is appended),
    /// records the run, and returns it.
    #[track_caller]
    pub fn run<A: AsRef<str>>(&self, on: &impl Runner, argv: &[A]) -> Ran {
        self.cmd(on, argv).run()
    }

    /// A run of `omaddons <argv>` on `on` to set up: variables, stdin, failpoints, a
    /// timeout, the background.
    pub fn cmd<A: AsRef<str>>(&self, on: &impl Runner, argv: &[A]) -> Cmd<'_> {
        let argv: Vec<String> = argv.iter().map(|arg| arg.as_ref().to_owned()).collect();
        let env = on.test_env();
        Cmd {
            log: self,
            root: env.root().to_path_buf(),
            state: env.state(),
            base: base_vars(env),
            command: on.command(&argv),
            argv,
            stdin: None,
            cwd: None,
            timeout: RUN_TIMEOUT,
        }
    }

    /// Registers `on`'s environment (its `env` record and its root's placeholder)
    /// without running anything, so a tree under it can be recorded before the first
    /// run.
    pub fn env(&self, on: &impl Runner) {
        let env = on.test_env();
        self.lock().register_env(env.root(), &base_vars(env));
    }

    /// A run's number, step and environment placeholder; its environment registered
    /// and its secrets learned first.
    fn begin_run(
        &self,
        root: &Path,
        base: &BTreeMap<String, String>,
        secrets: &[(String, String)],
    ) -> (u64, Option<String>, String) {
        let mut inner = self.lock();
        for (_, value) in secrets {
            inner.scrubber.add(value);
        }
        let env = inner.register_env(root, base);
        (inner.next_seq(), inner.step.clone(), env)
    }

    /// Writes a `failure` record and fails the test with `message` and the transcript.
    #[track_caller]
    fn fail(&self, message: &str) -> ! {
        let message = {
            let mut inner = self.lock();
            inner.failure(message);
            inner.scrubber.scrub(message)
        };
        let transcript = self.transcript_path().display().to_string();
        panic!("tlog: {message}\n  transcript: {transcript}"); // ubs:ignore — the assertion
    }

    /// Waits until the child stops at its `pause` failpoint `name`, recording the wait.
    #[track_caller]
    pub fn reached(&self, pause: &Pause, name: &str) {
        let dir = pause.env().1.to_owned();
        self.lock().pause("waiting", name, &dir, None);
        let start = Instant::now();
        pause.reached(name);
        let waited = millis(start.elapsed());
        self.lock().pause("reached", name, &dir, Some(waited));
    }

    /// Lets the child go on from `name`.
    pub fn resume(&self, pause: &Pause, name: &str) {
        let dir = pause.env().1.to_owned();
        pause.resume(name);
        self.lock().pause("resumed", name, &dir, None);
    }

    /// Records the hash and entries of the tree at `root` under `label`, and returns the
    /// hash (`None` when `root` is absent).
    #[track_caller]
    pub fn tree(&self, label: &str, root: &Path) -> Option<String> {
        self.confine(root);
        let (hash, entries) = read_tree(root);
        let mut inner = self.lock();
        let record = Record::Tree {
            seq: inner.next_seq(),
            step: inner.step.clone(),
            label: inner.clean_capped(label).0,
            root: inner.clean(&root.display().to_string()),
            exists: hash.is_some(),
            hash: hash.clone(),
            entries: entries.len(),
        };
        inner.emit(record);
        inner
            .trees
            .insert(label.to_owned(), (hash.clone(), entries));
        hash
    }

    /// The tree at `root` is as `label`'s last [`TestLog::tree`] recorded it; otherwise
    /// the `tree_check` record names the first differing paths, and the test fails.
    #[track_caller]
    pub fn assert_unchanged(&self, label: &str, root: &Path) {
        let (equal, diff) = self.check_tree("unchanged", label, root);
        if !equal {
            let mut paths = diff.first(DIFF_PATHS_SHOWN).join("\n    ");
            if diff.total > DIFF_PATHS_SHOWN {
                let more = diff.total - DIFF_PATHS_SHOWN;
                paths.push_str(&format!("\n    … {more} more"));
            }
            let step = self.lock().step.clone();
            self.fail(&format!(
                "tree `{label}` changed {} ({} paths):\n    {paths}",
                in_step(step.as_deref()),
                diff.total
            ));
        }
    }

    /// How the tree at `root` differs from `label`'s last [`TestLog::tree`], recorded;
    /// never fails, so the test can assert exactly what changed.
    #[track_caller]
    pub fn diff(&self, label: &str, root: &Path) -> TreeDiff {
        self.check_tree("diff", label, root).1
    }

    #[track_caller]
    fn check_tree(&self, check: &'static str, label: &str, root: &Path) -> (bool, TreeDiff) {
        self.confine(root);
        let recorded = self.lock().trees.get(label).cloned();
        let Some((expected, before)) = recorded else {
            self.fail(&format!("no tree `{label}` was recorded before this check"));
        };
        let (actual, after) = read_tree(root);
        let diff = tree_diff(&before, &after);
        let equal = expected == actual;
        let mut inner = self.lock();
        let shown = (!equal || !diff.is_empty()).then(|| DiffRecord {
            total: diff.total,
            first: diff
                .first(DIFF_PATHS_SHOWN)
                .iter()
                .map(|line| inner.clean(line))
                .collect(),
        });
        let record = Record::TreeCheck {
            seq: inner.next_seq(),
            step: inner.step.clone(),
            label: inner.clean_capped(label).0,
            root: inner.clean(&root.display().to_string()),
            check,
            expected,
            actual,
            equal,
            diff: shown,
        };
        inner.emit(record);
        (equal, diff)
    }

    /// Fails the test, before anything is read, when `root` is outside its synthetic
    /// directories.
    #[track_caller]
    fn confine(&self, root: &Path) {
        let roots = self.lock().paths.roots().to_vec();
        if !root_allowed(root, &roots) {
            self.fail(&format!(
                "{} is outside this test's synthetic directories; the test log never reads the developer's files",
                root.display()
            ));
        }
    }

    /// A sub-log for one case of a table-driven or parallel test, under
    /// `cases/<name>/`: its own files and guard, the parent's secrets and placeholders,
    /// and the parent's byte budget.
    #[track_caller]
    pub fn case(&self, name: &str) -> TestLog {
        let name = sanitize_component(name);
        let mut inner = self.lock();
        let setup = Setup {
            dir: self.dir.join("cases").join(&name),
            suite: inner.suite.clone(),
            test: name.clone(),
            echo_name: format!("{}::{}::{name}", inner.suite, inner.test),
            mode: inner.mode,
            caps: inner.caps,
            budget: Arc::clone(&inner.budget),
            parent: Some(format!("{}/{}", inner.suite, inner.test)),
            scrubber: inner.scrubber.clone(),
            paths: inner.paths.clone(),
        };
        let record = Record::Case {
            seq: inner.next_seq(),
            step: inner.step.clone(),
            dir: format!("cases/{name}"),
            name,
        };
        inner.emit(record);
        drop(inner);
        TestLog::open(setup)
    }
}

impl Inner {
    /// `root`'s placeholder; on first sight, its `env` record with the base variables.
    fn register_env(&mut self, root: &Path, base: &BTreeMap<String, String>) -> String {
        let known = self.paths.roots().iter().any(|seen| seen == root);
        let name = self.paths.add_root(root);
        if !known {
            let vars = base
                .iter()
                .map(|(key, value)| (key.clone(), self.clean(value)))
                .collect();
            let record = Record::Env {
                seq: self.next_seq(),
                name: name.clone(),
                vars,
            };
            self.emit(record);
        }
        name
    }

    /// A `failure` record in the current step.
    fn failure(&mut self, message: &str) {
        let (message, _) = self.clean_capped(message);
        let record = Record::Failure {
            seq: self.next_seq(),
            step: self.step.clone(),
            message,
        };
        self.emit(record);
    }

    fn pause(&mut self, action: &'static str, name: &str, dir: &str, waited_ms: Option<u64>) {
        let record = Record::Pause {
            seq: self.next_seq(),
            step: self.step.clone(),
            action,
            name: self.clean(name),
            dir: self.clean(dir),
            waited_ms,
        };
        self.emit(record);
    }

    /// The `run` record of a run that ended: every string cleaned, each stream capped.
    fn record_run(&mut self, started: &StartedRun, ended: Ended<'_>) {
        let cap = self.caps.stream_bytes;
        let cwd = started
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .map(|dir| self.clean(&dir.display().to_string()))
            .unwrap_or_default();
        let argv = std::iter::once("omaddons")
            .chain(started.argv.iter().map(String::as_str))
            .map(|arg| self.clean(arg))
            .collect();
        let overrides = started
            .overrides
            .vars
            .iter()
            .map(|(key, value)| (key.clone(), self.clean(value)))
            .collect();
        let failpoints = started.failpoints.as_ref().map(|armed| FailpointsRecord {
            spec: armed.spec.as_deref().map(|spec| self.clean(spec)),
            pause_dir: armed.pause_dir.as_deref().map(|dir| self.clean(dir)),
        });
        let added = ended.net_log.added.into_iter();
        let net_log = NetLogAdded {
            added: added.map(|line| self.clean_value(line)).collect(),
            rotated: ended.net_log.rotated,
        };
        let record = RunRecord {
            seq: started.seq,
            step: started.step.clone(),
            env: started.env.clone(),
            argv,
            cwd,
            overrides,
            failpoints,
            stdin_bytes: started.stdin_bytes,
            background: started.background,
            started_at: started.started_at.clone(),
            elapsed_ms: ended.elapsed_ms,
            exit: ended.run.code,
            signal: ended.signal,
            timed_out: ended.timed_out,
            stdout: classify_stdout(&self.clean(&ended.run.stdout), cap),
            stderr: parse_stderr(&self.clean(&ended.run.stderr), cap),
            net_log,
            scrubbed: started
                .overrides
                .secrets
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
        };
        self.runs += 1;
        self.emit(Record::Run(Box::new(record)));
    }
}

/// A tree's hash and entries, or nothing when it is absent.
fn read_tree(root: &Path) -> (Option<String>, BTreeMap<String, Entry>) {
    if fs::symlink_metadata(root).is_err() {
        return (None, BTreeMap::new());
    }
    (Some(tree_hash(root)), tree_entries(root))
}

/// Whether a log may read the tree at `root`: under one of `env_roots` (the `TestEnv`
/// roots it has seen), `CARGO_TARGET_TMPDIR` or the system's temporary directory, with
/// symlinks resolved. A missing root with a `..` in it is refused.
pub fn root_allowed(root: &Path, env_roots: &[PathBuf]) -> bool {
    let resolved = match root.canonicalize() {
        Ok(resolved) => resolved,
        Err(_) if root.components().any(|c| c == Component::ParentDir) => return false,
        Err(_) => root.to_path_buf(),
    };
    let fixed = [
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")),
        std::env::temp_dir(),
    ];
    env_roots.iter().chain(&fixed).any(|allowed| {
        let allowed = allowed.canonicalize().unwrap_or_else(|_| allowed.clone());
        resolved.starts_with(allowed)
    })
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
