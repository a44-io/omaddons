//! Human output on a real terminal: the binary with stderr (and, for help, stdout) on a
//! pseudo-terminal, where progress is one line that each update overwrites. What only a
//! terminal shows is checked here: progress cut to the terminal's width, a panic's
//! message starting on a cleared line, never after leftover progress text, and help
//! styled where the output policy allows color.

use crate::common;

use support::{CLEAR, all_on_terminal, home_with_a_zip, on_terminal};

#[cfg(test)]
mod support {
    use std::fs::{self, File};
    use std::io::Read;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;
    use std::process::Stdio;

    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use rustix::termios::{Winsize, tcsetwinsize};

    use super::common::addons::toc;
    use super::common::archives::zip_text;
    use super::common::home::Home;

    /// What a human progress line starts with, and what erases it: a carriage return and
    /// "erase the whole line".
    pub const CLEAR: &str = "\r\u{1b}[2K";

    /// The two ends of a pseudo-terminal `columns` wide: the side the test reads, and
    /// the terminal a child's stderr is opened on. `None` where `/dev/ptmx` cannot be
    /// opened.
    fn pty(columns: u16) -> Option<(File, File)> {
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).ok()?;
        grantpt(&master).ok()?;
        unlockpt(&master).ok()?;
        let name = PathBuf::from(std::ffi::OsString::from_vec(
            ptsname(&master, Vec::new()).ok()?.into_bytes(),
        ));
        let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
        let terminal = rustix::fs::open(name.as_path(), flags, Mode::empty()).ok()?;
        let size = Winsize {
            ws_row: 24,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        tcsetwinsize(&terminal, size).ok()?;
        Some((File::from(master), File::from(terminal)))
    }

    /// `omaddons <args>` with this home's environment and `extra`, its stderr a terminal
    /// `columns` wide (`TERM=xterm`, no `CI`), its stdout a pipe; returns its exit code
    /// and what reached the terminal. `None` where no pseudo-terminal can be opened.
    pub fn on_terminal(
        home: &Home,
        args: &[&str],
        extra: &[(&str, &str)],
        columns: u16,
    ) -> Option<(i32, String)> {
        run_on_terminal(home, args, extra, columns, false)
    }

    /// [`on_terminal`] with stdout on the terminal too, as an interactive shell runs a
    /// command; returns its exit code and everything it printed.
    pub fn all_on_terminal(
        home: &Home,
        args: &[&str],
        extra: &[(&str, &str)],
        columns: u16,
    ) -> Option<(i32, String)> {
        run_on_terminal(home, args, extra, columns, true)
    }

    fn run_on_terminal(
        home: &Home,
        args: &[&str],
        extra: &[(&str, &str)],
        columns: u16,
        stdout_too: bool,
    ) -> Option<(i32, String)> {
        let (mut master, terminal) = pty(columns)?;
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let mut env = vec![("TERM", "xterm")];
        env.extend_from_slice(extra);
        let mut cmd = home.std_command(&argv, &env);
        let stdout = if stdout_too {
            Stdio::from(terminal.try_clone().ok()?)
        } else {
            Stdio::piped()
        };
        cmd.stdin(Stdio::null())
            .stdout(stdout)
            .stderr(Stdio::from(terminal));
        let child = cmd.spawn().unwrap();
        // The command holds the terminal's descriptor: once it is gone, the reader sees
        // the end when the child exits.
        drop(cmd);
        let reader = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut buf = [0u8; 4096];
            // A read past the last writer's close fails (EIO): the end.
            while let Ok(n @ 1..) = master.read(&mut buf) {
                seen.extend_from_slice(&buf[..n]);
            }
            seen
        });
        let output = child.wait_with_output().unwrap();
        let seen = reader.join().unwrap();
        Some((
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&seen).into_owned(),
        ))
    }

    /// A retail home and a local addon zip deep enough in folders that its progress
    /// lines are wider than a narrow terminal; returns the home and the zip's path.
    pub fn home_with_a_zip() -> (Home, String) {
        let home = Home::new(&["wow"]);
        let dir = home
            .env
            .root()
            .join("downloads/a-folder-whose-name-is-wider-than-a-narrow-terminal");
        fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("Foo-1.0.0.zip");
        let text = toc("120105", &["## Version: 1.0.0"]);
        fs::write(&zip, zip_text(&[("Foo/Foo.toc", &text)])).unwrap();
        (home, zip.display().to_string())
    }
}

/// A panic after progress drew its line (here, once staging reported, when the commit
/// takes the lock) is reported by a reporter made afresh: it erases the line first, so
/// the message starts on a cleared line, never after the progress text.
#[cfg(debug_assertions)]
#[test]
fn a_panic_after_progress_starts_on_a_cleared_line() {
    let (home, zip) = home_with_a_zip();
    let args = ["install", &zip, "-t", "retail", "--no-color"];
    let panic = [("OMADDONS_FAILPOINTS", "engine.lock.acquired=panic")];
    let Some((code, seen)) = on_terminal(&home, &args, &panic, 200) else {
        common::checkpoint::skip(
            "a_panic_after_progress_starts_on_a_cleared_line",
            "no pseudo-terminal here (/dev/ptmx)",
        );
        return;
    };
    assert_eq!(code, 1, "{seen:?}");
    let at = seen.find("error[internal]");
    assert!(at.is_some(), "the panic is reported: {seen:?}");
    let before = &seen[..at.unwrap()];
    assert!(
        before.contains(&format!("{CLEAR}stage: ")),
        "progress drew its line first: {seen:?}"
    );
    assert!(
        before.ends_with(CLEAR),
        "the panic's message starts on a cleared line: {seen:?}"
    );
}

/// Each progress line fits one row of the terminal (`columns - 1` characters): a message
/// that would wrap is cut, with `…`, since erasing a line clears only the row the cursor
/// is on.
#[test]
fn progress_lines_fit_the_terminal() {
    let (home, zip) = home_with_a_zip();
    let args = ["install", &zip, "-t", "retail", "--no-color"];
    let Some((code, seen)) = on_terminal(&home, &args, &[], 40) else {
        common::checkpoint::skip(
            "progress_lines_fit_the_terminal",
            "no pseudo-terminal here (/dev/ptmx)",
        );
        return;
    };
    assert_eq!(code, 0, "{seen:?}");
    // Every progress line: what follows a clear, up to the next clear or line break,
    // when it starts with a phase (a warning also follows a clear).
    let phases = ["fetch", "download", "verify", "stage", "commit", "copy"];
    let lines: Vec<&str> = seen
        .split(CLEAR)
        .filter_map(|after| after.split(['\r', '\n']).next())
        .filter(|line| phases.iter().any(|p| line.starts_with(&format!("{p}: "))))
        .collect();
    assert!(!lines.is_empty(), "progress drew: {seen:?}");
    for line in &lines {
        assert!(
            line.chars().count() <= 39,
            "{line:?} fits 40 columns: {seen:?}"
        );
    }
    assert!(
        lines.iter().any(|line| line.contains('…')),
        "a line too wide was cut: {seen:?}"
    );
}

/// Help on a terminal is styled where the output policy allows color (`Usage:` bold,
/// `Options:` bold and underlined, flags bold), and plain under every condition that
/// turns color off: `NO_COLOR`, `--no-color`, `TERM=dumb`, CI and `--json`.
#[test]
fn help_on_a_terminal_is_styled_unless_color_is_off() {
    let home = common::home::Home::new(&["wow"]);
    let Some((code, seen)) = all_on_terminal(&home, &["status", "--help"], &[], 100) else {
        common::checkpoint::skip(
            "help_on_a_terminal_is_styled_unless_color_is_off",
            "no pseudo-terminal here (/dev/ptmx)",
        );
        return;
    };
    assert_eq!(code, 0, "{seen:?}");
    for styled in [
        "\u{1b}[1mUsage:\u{1b}[0m",
        "\u{1b}[1m\u{1b}[4mOptions:\u{1b}[0m",
        "\u{1b}[1m--cached\u{1b}[0m",
        "\u{1b}[1m\u{1b}[4mGlobal options:\u{1b}[0m",
    ] {
        assert!(seen.contains(styled), "{styled:?} in {seen:?}");
    }
    let bare = all_on_terminal(&home, &[], &[], 100).map(|(_, seen)| seen);
    assert!(
        bare.as_deref()
            .is_some_and(|seen| seen.contains("\u{1b}[1mstatus\u{1b}[0m")),
        "bare help names its commands in bold: {bare:?}"
    );
    for (case, args, extra) in [
        (
            "NO_COLOR",
            &["status", "--help"][..],
            &[("NO_COLOR", "1")][..],
        ),
        ("--no-color", &["status", "--help", "--no-color"], &[]),
        ("TERM=dumb", &["status", "--help"], &[("TERM", "dumb")]),
        ("CI", &["status", "-h"], &[("CI", "1")]),
        ("--json", &["help", "status", "--json"], &[]),
        ("bare under NO_COLOR", &[], &[("NO_COLOR", "1")]),
    ] {
        let seen = all_on_terminal(&home, args, extra, 100).map(|(_, seen)| seen);
        assert!(
            seen.as_deref()
                .is_some_and(|seen| !seen.is_empty() && !seen.contains('\u{1b}')),
            "{case}: no escape: {seen:?}"
        );
    }
}
