//! `trace` log events (`-vv`, M4-D17): the release memo, discovery and the global lock
//! report each decision as they make it, through the run's one stderr reporter, without
//! a handle threaded through every caller. Off unless the run asked for `-vv`; then a
//! site's message is formatted only when it will be written.
//!
//! Tracing observes and never decides: it writes nothing to the filesystem, takes no
//! lock a command waits on, and changes nothing on stdout.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use super::stderr::{LogLevel, SharedReporter, lock};
use super::term::OutputConfig;

/// Whether a sink is installed and wants trace events.
static ON: AtomicBool = AtomicBool::new(false);
/// The run's reporter, while tracing is on.
static SINK: Mutex<Option<SharedReporter>> = Mutex::new(None);

/// Sends trace events to `reporter` for the rest of the process when `cfg` asks for them
/// (`-vv` and not `--quiet`); otherwise turns them off. Replaces any earlier sink.
pub fn install(reporter: &SharedReporter, cfg: &OutputConfig) {
    let on = cfg.style.trace() && !cfg.style.quiet;
    let mut sink = SINK.lock().unwrap_or_else(PoisonError::into_inner);
    *sink = on.then(|| reporter.clone());
    ON.store(on, Ordering::Release);
}

/// Whether a trace event would be written now (or, in tests, collected on this thread):
/// a site that must do extra work only to describe a decision checks it first.
pub fn enabled() -> bool {
    #[cfg(test)]
    if capture::active() {
        return true;
    }
    ON.load(Ordering::Acquire)
}

/// Writes `message()` as a `trace` log event when tracing is on. `message` is not called
/// otherwise, so a site pays one atomic load when tracing is off.
///
/// Call it on the thread that called the site's public function, never inside a
/// `run_bounded` worker: the order of lines then follows the decisions, and a test's
/// capture sees them.
pub fn emit(message: impl FnOnce() -> String) {
    #[cfg(test)]
    if capture::active() {
        capture::push(message());
        return;
    }
    write(&ON, &SINK, message);
}

/// [`emit`] through the state `on` and `sink` hold.
fn write(on: &AtomicBool, sink: &Mutex<Option<SharedReporter>>, message: impl FnOnce() -> String) {
    if !on.load(Ordering::Acquire) {
        return;
    }
    // The sink is released before the reporter is locked: no lock is held while
    // another is taken, so tracing cannot deadlock against progress.
    let reporter = sink.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(reporter) = reporter {
        lock(&reporter).log(LogLevel::Trace, &message());
    }
}

/// Test only: runs `f` with this thread's trace events collected instead of written, and
/// returns them in order. Collected whether or not a sink is installed.
#[cfg(test)]
pub(crate) fn capture<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    capture::run(f)
}

#[cfg(test)]
mod capture {
    use std::cell::RefCell;

    thread_local! {
        static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    /// Puts back what this thread collected before, also when `f` panics.
    struct Restore(Option<Vec<String>>);

    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            LINES.with(|lines| *lines.borrow_mut() = previous);
        }
    }

    pub(super) fn active() -> bool {
        LINES.with(|lines| lines.borrow().is_some())
    }

    /// Collects `line` while this thread captures.
    pub(super) fn push(line: String) {
        LINES.with(|lines| {
            if let Some(lines) = lines.borrow_mut().as_mut() {
                lines.push(line);
            }
        });
    }

    pub(super) fn run<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
        let restore = Restore(LINES.with(|lines| lines.borrow_mut().replace(Vec::new())));
        let out = f();
        let collected = LINES
            .with(|lines| lines.borrow_mut().take())
            .unwrap_or_default();
        drop(restore);
        (out, collected)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::output::Reporter;
    use crate::output::term::{StderrMode, Style};

    #[test]
    fn emit_formats_nothing_when_off() {
        let called = AtomicBool::new(false);
        let format = || {
            called.store(true, Ordering::SeqCst);
            String::new()
        };
        // Off, with no sink, and off with a sink: nothing is formatted either way.
        write(&AtomicBool::new(false), &Mutex::new(None), format);
        assert!(!called.load(Ordering::SeqCst), "formatted with tracing off");
        let reporter: SharedReporter = Arc::new(Mutex::new(Reporter::new(
            Box::new(Captured::default()),
            &OutputConfig::machine(),
        )));
        write(&AtomicBool::new(false), &Mutex::new(Some(reporter)), || {
            called.store(true, Ordering::SeqCst);
            String::new()
        });
        assert!(
            !called.load(Ordering::SeqCst),
            "formatted with a sink but off"
        );
        assert!(!capture::active(), "no capture outside capture()");
    }

    #[test]
    fn capture_collects_this_threads_events_in_order() {
        let ((), lines) = capture(|| {
            assert!(enabled(), "enabled while capturing");
            emit(|| "one".to_owned());
            emit(|| "two".to_owned());
            std::thread::scope(|s| {
                s.spawn(|| emit(|| "another thread".to_owned()));
            });
            emit(|| "three".to_owned());
        });
        assert_eq!(
            lines,
            ["one", "two", "three"],
            "in order, this thread's only"
        );
        assert!(!capture::active(), "the capture ends with its call");
        let (_, nested) = capture(|| {
            let (_, inner) = capture(|| emit(|| "inner".to_owned()));
            assert_eq!(inner, ["inner"]);
            emit(|| "outer".to_owned());
        });
        assert_eq!(nested, ["outer"], "a nested capture keeps its own lines");
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

    /// The only test that installs a sink, so no other test races it; it ends with
    /// tracing off. Lines without its marker are another test's site, and ignored.
    #[test]
    fn install_turns_tracing_on_only_at_level_two_without_quiet() {
        let config = |verbose, quiet| OutputConfig {
            stderr: StderrMode::Ndjson,
            style: Style {
                verbose,
                quiet,
                ..Style::default()
            },
            ..OutputConfig::machine()
        };
        let captured = Captured::default();
        let sink: Box<dyn Write + Send> = Box::new(captured.clone());
        let reporter: SharedReporter = Arc::new(Mutex::new(Reporter::new(sink, &config(2, false))));
        for (case, verbose, quiet, on) in [
            ("v0", 0, false, false),
            ("v1", 1, false, false),
            ("v2", 2, false, true),
            ("v3", 3, false, true),
            ("v2_quiet", 2, true, false),
        ] {
            install(&reporter, &config(verbose, quiet));
            assert_eq!(enabled(), on, "{case}: enabled after install");
        }
        install(&reporter, &config(2, false));
        let marker = "trace-install-marker-7f3a";
        emit(|| marker.to_owned());
        install(&reporter, &config(0, false));
        assert!(!enabled(), "tracing ends off");
        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| l.contains(marker)).collect();
        assert_eq!(
            lines,
            [format!(
                "{{\"event\":\"log\",\"level\":\"trace\",\"message\":\"{marker}\"}}"
            )],
            "one NDJSON trace line holding the marker"
        );
    }
}
