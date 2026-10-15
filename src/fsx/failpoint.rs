//! Debug-only failure injection (DEC-7).
//!
//! `OMADDONS_FAILPOINTS=<name>[=abort|=error|=sleep:<ms>|=panic][,…]` arms failpoints by
//! name; an entry without an action aborts. Names are dotted and lowercase: `fsx.*` in
//! fsx, `engine.*` in the engine, `cli.*` in the CLI. Each primitive hits
//! `fsx.<primitive>.before` and, where it has a second step, `fsx.<primitive>.after`.
//! Unknown names and malformed entries are ignored. Release builds never read the
//! variable: [`hit`] is a no-op there.

use std::collections::BTreeMap;
use std::io;

/// What an armed failpoint does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// `std::process::abort()`: a simulated crash with no unwinding or destructors.
    Abort,
    /// Return an I/O error from [`hit`].
    Error,
    /// Sleep this many milliseconds, then continue.
    Sleep(u64),
    /// `panic!`, for testing the panic hook.
    Panic,
}

pub(crate) type Spec = BTreeMap<String, Action>;

/// Parses a spec string. Malformed entries are dropped, never fatal.
pub(crate) fn parse(spec: &str) -> Spec {
    spec.split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            let (name, action) = match entry.split_once('=') {
                Some((name, action)) => (name.trim(), parse_action(action.trim())?),
                None => (entry, Action::Abort),
            };
            (!name.is_empty()).then(|| (name.to_owned(), action))
        })
        .collect()
}

fn parse_action(action: &str) -> Option<Action> {
    match action {
        "abort" => Some(Action::Abort),
        "error" => Some(Action::Error),
        "panic" => Some(Action::Panic),
        _ => action
            .strip_prefix("sleep:")
            .and_then(|ms| ms.parse().ok())
            .map(Action::Sleep),
    }
}

/// Runs the failpoint `name` if it is armed.
pub(crate) fn hit(name: &str) -> io::Result<()> {
    match configured(name) {
        None => Ok(()),
        Some(Action::Abort) => std::process::abort(),
        Some(Action::Error) => Err(io::Error::other(format!("failpoint {name}"))),
        Some(Action::Sleep(ms)) => {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Ok(())
        }
        // The `panic` action exists to test the panic hook; `configured` never arms a
        // failpoint in release builds.
        Some(Action::Panic) => panic!("failpoint {name}"), // ubs:ignore — deliberate (DEC-7)
    }
}

/// Whether `name` is armed with any action. For switch-like failpoints such as
/// `fsx.renameat2.einval`, whose effect is fixed by the code that checks them.
pub(crate) fn enabled(name: &str) -> bool {
    configured(name).is_some()
}

#[cfg(not(debug_assertions))]
fn configured(name: &str) -> Option<Action> {
    #[cfg(test)]
    if let Some(action) = overridden(name) {
        return action;
    }
    let _ = name;
    None
}

#[cfg(debug_assertions)]
fn configured(name: &str) -> Option<Action> {
    #[cfg(test)]
    if let Some(action) = overridden(name) {
        return action;
    }
    static SPEC: std::sync::OnceLock<Spec> = std::sync::OnceLock::new();
    SPEC.get_or_init(|| {
        std::env::var("OMADDONS_FAILPOINTS")
            .map(|spec| parse(&spec))
            .unwrap_or_default()
    })
    .get(name)
    .copied()
}

#[cfg(test)]
thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<Spec>> = const { std::cell::RefCell::new(None) };
}

/// `Some(lookup)` while a test override is installed on this thread.
#[cfg(test)]
fn overridden(name: &str) -> Option<Option<Action>> {
    OVERRIDE.with(|o| o.borrow().as_ref().map(|spec| spec.get(name).copied()))
}

/// Runs `f` with `spec` replacing the environment on this thread only. Tests use this
/// because changing the process environment is unsafe in Rust 2024.
#[cfg(test)]
pub(crate) fn with_spec<T>(spec: &str, f: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            OVERRIDE.with(|o| *o.borrow_mut() = None);
        }
    }
    OVERRIDE.with(|o| *o.borrow_mut() = Some(parse(spec)));
    let _reset = Reset;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_and_actions() {
        let spec = parse("a, b=error ,c=sleep:5,d=panic,e=abort");
        assert_eq!(
            spec,
            Spec::from([
                ("a".to_owned(), Action::Abort),
                ("b".to_owned(), Action::Error),
                ("c".to_owned(), Action::Sleep(5)),
                ("d".to_owned(), Action::Panic),
                ("e".to_owned(), Action::Abort),
            ])
        );
    }

    #[test]
    fn malformed_entries_are_ignored() {
        for spec in ["=x", "a=explode", "a=sleep:", "a=sleep:x", ",,", "", " = "] {
            assert!(parse(spec).is_empty(), "{spec:?}");
        }
        assert_eq!(
            parse("a=explode,b"),
            Spec::from([("b".to_owned(), Action::Abort)])
        );
    }

    #[test]
    fn hit_and_enabled_follow_the_spec() {
        with_spec("t.err=error,t.sleep=sleep:1", || {
            let err = hit("t.err").unwrap_err();
            assert_eq!(err.to_string(), "failpoint t.err");
            assert!(hit("t.sleep").is_ok());
            assert!(hit("t.unknown").is_ok());
            assert!(enabled("t.err"));
            assert!(!enabled("t.unknown"));
        });
        assert!(hit("t.err").is_ok(), "the override ends with the closure");
    }

    #[test]
    fn panic_action_panics() {
        let result = std::panic::catch_unwind(|| with_spec("t.panic=panic", || hit("t.panic")));
        assert!(result.is_err());
        assert!(
            hit("t.panic").is_ok(),
            "the override is reset after a panic"
        );
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn release_builds_ignore_the_environment() {
        assert!(configured("anything").is_none());
    }
}
