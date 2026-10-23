//! Omarchy's own tools: the only processes the `omarchy` commands start.
//! `omarchy-plugin-validate` reads the rendered stage; `omarchy-shell` pings the shell
//! and asks it to rescan its plugins; `omarchy-bar put` places the widget and
//! `omarchy-plugin-disable` takes it off, which write Omarchy's `shell.json` (omaddons
//! never writes it). Each runs from `PATH` with argv only, the user's environment
//! inherited unchanged (the tools need `OMARCHY_PATH`, the Wayland and Hyprland
//! variables and the D-Bus session), in its own process group, which is stopped after
//! [`OMARCHY_COMMAND_TIMEOUT`] (the ping after [`OMARCHY_PING_TIMEOUT`]), its output read
//! up to [`API_RESPONSE_MAX_BYTES`].

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::limits::{
    API_RESPONSE_MAX_BYTES, OMARCHY_COMMAND_TIMEOUT, OMARCHY_KILL_GRACE, OMARCHY_PING_TIMEOUT,
};

/// The validator Omarchy ships.
pub const VALIDATOR: &str = "omarchy-plugin-validate";
/// The shell's IPC client.
pub const SHELL: &str = "omarchy-shell";
/// The bar's layout tool.
pub const BAR: &str = "omarchy-bar";
/// Omarchy's plugin disable.
pub const DISABLE: &str = "omarchy-plugin-disable";
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

/// Runs `program` with `args`, stdin closed, for [`OMARCHY_COMMAND_TIMEOUT`] at most.
pub fn run(program: &str, args: &[&str]) -> Result<Ran, NotRun> {
    run_for(program, args, OMARCHY_COMMAND_TIMEOUT)
}

/// [`run`], killed after `timeout`.
fn run_for(program: &str, args: &[&str], timeout: Duration) -> Result<Ran, NotRun> {
    // Omarchy's own tools, by name, from the user's PATH: no shell, argv only. Each in
    // its own process group, so a timeout stops the children the script started too.
    let mut command = Command::new(program); // ubs:ignore — fixed Omarchy tool names
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|err| match err.kind() {
            // ubs:ignore — an io::ErrorKind, not a secret
            std::io::ErrorKind::NotFound => NotRun::Missing,
            _ => NotRun::Failed(err.to_string()),
        })?;
    // Each stream read in chunks into a buffer the caller can take at any time: a
    // process the tool left running (a restarted shell) may hold the pipe open long after
    // the tool exits, and its output is what the tool wrote, not what that process might.
    let reader = |stream: Option<Box<dyn Read + Send>>| {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buffer);
        let handle = std::thread::spawn(move || {
            let Some(mut stream) = stream.map(|s| s.take(API_RESPONSE_MAX_BYTES)) else {
                return;
            };
            let mut chunk = [0u8; 8192];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => sink
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .extend_from_slice(chunk.get(..n).unwrap_or_default()),
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
        (handle, buffer)
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
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                stop(&mut child);
                return Err(NotRun::TimedOut);
            }
            Err(err) => {
                stop(&mut child);
                return Err(NotRun::Failed(err.to_string()));
            }
        }
    };
    // The rest of its output, until the pipes close or the deadline (the timeout, or a
    // grace after the exit when the tool took nearly all of it): never unbounded.
    let deadline = (started + timeout).max(Instant::now() + OMARCHY_KILL_GRACE);
    while !(stdout.0.is_finished() && stderr.0.is_finished()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let text = |buffer: &Arc<Mutex<Vec<u8>>>| {
        let bytes = buffer.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Ok(Ran {
        success: status.success(),
        stdout: text(&stdout.1),
        stderr: text(&stderr.1),
    })
}

/// Stops `child` and every process of the group it leads: SIGTERM, then SIGKILL after
/// [`OMARCHY_KILL_GRACE`], then reaps it. It is not reaped before the SIGKILL, so the
/// group's id cannot have passed to another process meanwhile.
fn stop(child: &mut Child) {
    use rustix::process::{Pid, Signal, kill_process_group};
    let group = i32::try_from(child.id()).ok().and_then(Pid::from_raw);
    if let Some(group) = group {
        let _ = kill_process_group(group, Signal::TERM);
        std::thread::sleep(OMARCHY_KILL_GRACE);
        let _ = kill_process_group(group, Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
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

/// Whether the shell is running: `omarchy-shell shell ping` answers `ok` within
/// [`OMARCHY_PING_TIMEOUT`]. Never with `-q`, which exits 0 whether or not anything
/// answered.
pub fn ping() -> bool {
    ping_with(SHELL)
}

fn ping_with(program: &str) -> bool {
    run_for(program, &["shell", "ping"], OMARCHY_PING_TIMEOUT)
        .is_ok_and(|ran| ran.success && ran.stdout.trim() == "ok")
}

/// Takes the plugin `id` off the bar: `omarchy-plugin-disable <id>`, which answers
/// `Disabled <id>`, or, without it, the shell's own `setPluginEnabled <id> false`,
/// which answers `ok`. Omarchy then splices the widget's entry, inline settings
/// included, out of `shell.json`. The error is why it did not happen, for the warning.
pub fn disable(id: &str) -> Result<(), String> {
    let said = |ran: &Ran| {
        let said = if ran.stderr.trim().is_empty() {
            ran.stdout.trim()
        } else {
            ran.stderr.trim()
        };
        said.to_owned()
    };
    match run(DISABLE, &[id]) {
        Ok(ran) if ran.success && ran.stdout.trim() == format!("Disabled {id}") => Ok(()),
        Ok(ran) => Err(format!("{DISABLE} said: {}", said(&ran))),
        Err(NotRun::Missing) => match run(SHELL, &["shell", "setPluginEnabled", id, "false"]) {
            Ok(ran) if ran.success && ran.stdout.trim() == "ok" => Ok(()),
            Ok(ran) => Err(format!("{SHELL} said: {}", said(&ran))),
            Err(not_run) => Err(not_run.describe(SHELL)),
        },
        Err(not_run) => Err(not_run.describe(DISABLE)),
    }
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

    /// Only `ok` on stdout and a clean exit is a running shell: `-q`'s silent exit 0,
    /// another answer or a failure is not.
    #[test]
    fn the_shell_runs_only_when_ping_says_ok() {
        let dir = tempfile::tempdir().unwrap();
        let shell = |body: &str| {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.path().join("omarchy-shell");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.to_string_lossy().into_owned()
        };
        let ok = shell(r#"[ "$1 $2" = "shell ping" ] && echo ok"#);
        assert!(ping_with(&ok));
        assert!(!ping_with(&shell("exit 0")));
        assert!(!ping_with(&shell(
            "echo 'omarchy-shell is not running' >&2; exit 1"
        )));
        assert!(!ping_with(&shell("echo pong")));
        assert!(!ping_with(&dir.path().join("missing").to_string_lossy()));
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
