//! Omarchy's own tools: the only processes `omarchy install` (and E3's `remove`) start.
//! `omarchy-plugin-validate` reads the rendered stage; `omarchy-shell` asks the running
//! shell to rescan its plugins; `omarchy-bar put` places the widget, which writes
//! Omarchy's `shell.json` (omaddons never writes it). Each runs from `PATH` with argv
//! only, the user's environment inherited unchanged (the tools need `OMARCHY_PATH`, the
//! Wayland and Hyprland variables and the D-Bus session), killed after
//! [`OMARCHY_COMMAND_TIMEOUT`], its output read up to [`API_RESPONSE_MAX_BYTES`].

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::limits::{API_RESPONSE_MAX_BYTES, OMARCHY_COMMAND_TIMEOUT};

/// The validator Omarchy ships.
pub const VALIDATOR: &str = "omarchy-plugin-validate";
/// The shell's IPC client.
pub const SHELL: &str = "omarchy-shell";
/// The bar's layout tool.
pub const BAR: &str = "omarchy-bar";
/// How often `omarchy-bar put` is asked again while the shell has not seen the new
/// plugin yet, and how long apart (F0: the shell answers "unknown" for about 0.4 s
/// after the files land, before its debounced rescan starts).
const PUT_ATTEMPTS: usize = 5;
const PUT_RETRY: Duration = Duration::from_millis(200);

/// One finished run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Why a tool did not run to the end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotRun {
    /// Not on `PATH`.
    Missing,
    TimedOut,
    Failed(String),
}

impl NotRun {
    pub fn describe(&self, program: &str) -> String {
        match self {
            Self::Missing => format!("{program} is not on PATH"),
            Self::TimedOut => format!("{program} did not finish in time"),
            Self::Failed(why) => format!("{program} could not run: {why}"),
        }
    }
}

/// Whether `program` is an executable file in a `PATH` directory.
pub fn on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|dirs| {
        std::env::split_paths(&dirs).any(|dir| {
            std::fs::metadata(dir.join(program))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

/// Runs `program` with `args`, stdin closed.
pub fn run(program: &str, args: &[&str]) -> Result<Ran, NotRun> {
    // Omarchy's own tools, by name, from the user's PATH: no shell, argv only.
    let mut command = Command::new(program); // ubs:ignore — fixed Omarchy tool names
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| match err.kind() {
            // ubs:ignore — an io::ErrorKind, not a secret
            std::io::ErrorKind::NotFound => NotRun::Missing,
            _ => NotRun::Failed(err.to_string()),
        })?;
    let reader = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(stream) = stream {
                let _ = stream.take(API_RESPONSE_MAX_BYTES).read_to_end(&mut out);
            }
            String::from_utf8_lossy(&out).into_owned()
        })
    };
    let stdout = reader(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let stderr = reader(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    // ubs:ignore — a timeout clock, not randomness
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < OMARCHY_COMMAND_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(NotRun::TimedOut);
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(NotRun::Failed(err.to_string()));
            }
        }
    };
    Ok(Ran {
        success: status.success(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// What Omarchy's validator says about the plugin at `dir`: `None` when it is not on
/// `PATH` (omaddons' own port has already run), else the problems it reports (empty
/// when it accepts the plugin; one line when it could not finish).
pub fn validate(dir: &Path) -> Option<Vec<String>> {
    let dir = dir.to_string_lossy();
    match run(VALIDATOR, &[dir.as_ref()]) {
        Err(NotRun::Missing) => None,
        Err(other) => Some(vec![other.describe(VALIDATOR)]),
        Ok(ran) if ran.success => Some(Vec::new()),
        Ok(ran) => {
            let lines: Vec<String> = ran
                .stderr
                .lines()
                .chain(ran.stdout.lines())
                .filter(|l| !l.trim().is_empty())
                .map(str::to_owned)
                .collect();
            Some(if lines.is_empty() {
                vec![format!("{VALIDATOR} refused the plugin")]
            } else {
                lines
            })
        }
    }
}

/// Asks the running shell to rescan its plugins. Best effort: the shell's own hot
/// reload catches the swap anyway.
pub fn rescan() {
    let _ = run(SHELL, &["-q", "shell", "rescanPlugins"]);
}

/// How placing the widget went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placed {
    /// `omarchy-bar put` answered `<id> is on the bar`.
    Yes,
    /// Anything else: the tool missing, a shell that is not running (it then exits 0),
    /// or an answer this build does not know; the reason, for the warning.
    Unreachable(String),
}

/// `omarchy-bar put <id> --section <section>`. Only the exact answer `<id> is on the
/// bar` is success: the tool exits 0 when the shell is not running. While the shell
/// has not seen the new plugin yet it says `is not a known widget`, which is asked
/// again a few times.
pub fn put(id: &str, section: &str) -> Placed {
    put_with(BAR, id, section, PUT_RETRY)
}

fn put_with(program: &str, id: &str, section: &str, pause: Duration) -> Placed {
    let expected = format!("{id} is on the bar");
    let mut last = String::new();
    for attempt in 0..PUT_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(pause);
        }
        match run(program, &["put", id, "--section", section]) {
            Err(not_run) => return Placed::Unreachable(not_run.describe(BAR)),
            Ok(ran) if ran.stdout.trim() == expected => return Placed::Yes,
            Ok(ran) => {
                let said = if ran.stderr.trim().is_empty() {
                    ran.stdout.trim()
                } else {
                    ran.stderr.trim()
                };
                last = format!("{BAR} said: {said}");
                if !ran.stderr.contains("is not a known widget") {
                    break;
                }
            }
        }
    }
    Placed::Unreachable(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_tool_is_missing() {
        assert_eq!(
            run("omaddons-no-such-tool-anywhere", &[]),
            Err(NotRun::Missing)
        );
        assert!(!on_path("omaddons-no-such-tool-anywhere"));
        assert!(on_path("sh"));
    }

    /// A stub `omarchy-bar` at an absolute path: `body` is its shell script, `$2` the id.
    fn stub(dir: &Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("omarchy-bar");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// Only the exact answer is success: the not-running case exits 0 with nothing on
    /// stdout, a changed tool says something else, and "not a known widget" is asked
    /// again, five times at most.
    #[test]
    fn only_the_exact_answer_places_the_widget() {
        let dir = tempfile::tempdir().unwrap();
        let fast = Duration::from_millis(1);
        let ok = stub(dir.path(), r#"echo "$2 is on the bar""#);
        assert_eq!(put_with(&ok, "a44.omaddons", "right", fast), Placed::Yes);

        let not_running = stub(
            dir.path(),
            "echo 'omarchy-shell is not running; start it first' >&2; exit 0",
        );
        let placed = put_with(&not_running, "a44.omaddons", "right", fast);
        assert!(
            matches!(&placed, Placed::Unreachable(why) if why.contains("is not running")),
            "{placed:?}"
        );

        let other = stub(dir.path(), r#"echo "$2 is somewhere else""#);
        assert!(matches!(
            put_with(&other, "a44.omaddons", "right", fast),
            Placed::Unreachable(_)
        ));

        // Unknown twice, then placed; and unknown every time, given up after five.
        let count = dir.path().join("count");
        let script = format!(
            r#"n=0; [ -f {c} ] && read n < {c}; n=$((n+1)); echo $n > {c}
if [ $n -le $LIMIT ]; then echo "$2 is not a known widget" >&2; exit 1; fi
echo "$2 is on the bar""#,
            c = count.display()
        );
        let flaky = stub(dir.path(), &script.replace("$LIMIT", "2"));
        assert_eq!(put_with(&flaky, "a44.omaddons", "left", fast), Placed::Yes);
        assert_eq!(std::fs::read_to_string(&count).unwrap().trim(), "3");
        std::fs::remove_file(&count).unwrap();
        let never = stub(dir.path(), &script.replace("$LIMIT", "99"));
        assert!(matches!(
            put_with(&never, "a44.omaddons", "left", fast),
            Placed::Unreachable(_)
        ));
        assert_eq!(std::fs::read_to_string(&count).unwrap().trim(), "5");

        let missing = dir
            .path()
            .join("nothing-here")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            put_with(&missing, "a44.omaddons", "right", fast),
            Placed::Unreachable("omarchy-bar is not on PATH".into())
        );
    }

    #[test]
    fn output_is_captured() {
        let ran = run("sh", &["-c", "echo out; echo err >&2; exit 3"]).unwrap();
        assert_eq!(
            ran,
            Ran {
                success: false,
                stdout: "out\n".into(),
                stderr: "err\n".into()
            }
        );
    }
}
