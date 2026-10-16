//! Debug-only failure injection (DEC-7).
//!
//! `OMADDONS_FAILPOINTS=<name>[=abort|=error|=sleep:<ms>|=panic][@N][,…]` arms failpoints
//! by name; an entry without an action aborts. `@N` (N ≥ 1) fires on the Nth hit of that
//! name only, which is how a test fails exactly one target of several. Names are dotted
//! and lowercase: `fsx.*` in fsx, `engine.*` in the engine, `cli.*` in the CLI. Each
//! primitive hits `fsx.<primitive>.before` and, where it has a second step,
//! `fsx.<primitive>.after`. Unknown names and malformed entries are ignored. Release
//! builds never read the variable or an override: [`hit`] is a no-op there.

// The spec parser and the actions exist only for debug builds.
#![cfg_attr(not(debug_assertions), allow(dead_code))]

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

/// One armed failpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Armed {
    pub action: Action,
    /// `@N`: fires on this hit only; on every hit when `None`.
    pub nth: Option<u64>,
}

impl Armed {
    const fn every(action: Action) -> Self {
        Self { action, nth: None }
    }

    /// The action, if this is a hit it fires on; `count` gives this hit's number.
    fn fires(self, count: impl FnOnce() -> u64) -> Option<Action> {
        match self.nth {
            None => Some(self.action),
            Some(n) => (count() == n).then_some(self.action),
        }
    }
}

pub(crate) type Spec = BTreeMap<String, Armed>;

/// Parses a spec string. Malformed entries are dropped, never fatal.
pub(crate) fn parse(spec: &str) -> Spec {
    spec.split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            let (name, armed) = match entry.split_once('=') {
                Some((name, rest)) => (name.trim(), parse_armed(rest.trim())?),
                None => (entry, Armed::every(Action::Abort)),
            };
            (!name.is_empty()).then(|| (name.to_owned(), armed))
        })
        .collect()
}

/// `<action>` or `<action>@N`; `N` must be a number of at least 1.
fn parse_armed(text: &str) -> Option<Armed> {
    let (action, nth) = match text.split_once('@') {
        Some((action, n)) => {
            let n: u64 = n.trim().parse().ok().filter(|n| *n >= 1)?;
            (action.trim(), Some(n))
        }
        None => (text, None),
    };
    Some(Armed {
        action: parse_action(action)?,
        nth,
    })
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
    let _ = name;
    None
}

#[cfg(debug_assertions)]
fn configured(name: &str) -> Option<Action> {
    use std::sync::atomic::{AtomicU64, Ordering};

    if let Some(action) = overridden(name) {
        return action;
    }
    /// The environment's spec and each entry's hit count.
    static SPEC: std::sync::OnceLock<(Spec, BTreeMap<String, AtomicU64>)> =
        std::sync::OnceLock::new();
    let (spec, hits) = SPEC.get_or_init(|| {
        let spec = std::env::var("OMADDONS_FAILPOINTS")
            .map(|spec| parse(&spec))
            .unwrap_or_default();
        let hits = spec
            .keys()
            .map(|k| (k.clone(), AtomicU64::new(0)))
            .collect();
        (spec, hits)
    });
    spec.get(name)?.fires(|| {
        hits.get(name)
            .map_or(0, |h| h.fetch_add(1, Ordering::SeqCst) + 1)
    })
}

/// An override's spec and each entry's hit count.
#[cfg(debug_assertions)]
type Override = (Spec, BTreeMap<String, u64>);

#[cfg(debug_assertions)]
thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<Override>> = const { std::cell::RefCell::new(None) };
}

/// `Some(lookup)` while an override is installed on this thread.
#[cfg(debug_assertions)]
fn overridden(name: &str) -> Option<Option<Action>> {
    OVERRIDE.with(|o| {
        let mut o = o.borrow_mut();
        let (spec, hits) = o.as_mut()?;
        Some(spec.get(name).and_then(|armed| {
            armed.fires(|| {
                let count = hits.entry(name.to_owned()).or_insert(0);
                *count += 1;
                *count
            })
        }))
    })
}

/// Runs `f` with `spec` replacing the environment on this thread only (debug builds).
/// Tests use this because changing the process environment is unsafe in Rust 2024;
/// integration tests reach it as `omaddons::fsx::with_spec`.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn with_spec<T>(spec: &str, f: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            OVERRIDE.with(|o| *o.borrow_mut() = None);
        }
    }
    OVERRIDE.with(|o| *o.borrow_mut() = Some((parse(spec), BTreeMap::new())));
    let _reset = Reset;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every(action: Action) -> Armed {
        Armed::every(action)
    }

    #[test]
    fn parses_names_and_actions() {
        let spec = parse("a, b=error ,c=sleep:5,d=panic,e=abort,f=error@2,g=abort@3,h=sleep:5@1");
        assert_eq!(
            spec,
            Spec::from([
                ("a".to_owned(), every(Action::Abort)),
                ("b".to_owned(), every(Action::Error)),
                ("c".to_owned(), every(Action::Sleep(5))),
                ("d".to_owned(), every(Action::Panic)),
                ("e".to_owned(), every(Action::Abort)),
                (
                    "f".to_owned(),
                    Armed {
                        action: Action::Error,
                        nth: Some(2)
                    }
                ),
                (
                    "g".to_owned(),
                    Armed {
                        action: Action::Abort,
                        nth: Some(3)
                    }
                ),
                (
                    "h".to_owned(),
                    Armed {
                        action: Action::Sleep(5),
                        nth: Some(1)
                    }
                ),
            ])
        );
    }

    #[test]
    fn malformed_entries_are_ignored() {
        for spec in [
            "=x",
            "a=explode",
            "a=sleep:",
            "a=sleep:x",
            ",,",
            "",
            " = ",
            "a=error@0",
            "a=error@x",
            "a=error@",
            "a=@2",
        ] {
            assert!(parse(spec).is_empty(), "{spec:?}");
        }
        assert_eq!(
            parse("a=explode,b"),
            Spec::from([("b".to_owned(), every(Action::Abort))])
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn an_nth_failpoint_fires_on_that_hit_only() {
        with_spec("t.nth=error@2,t.every=error", || {
            let fired: Vec<bool> = (0..4).map(|_| hit("t.nth").is_err()).collect();
            assert_eq!(fired, [false, true, false, false]);
            assert!(hit("t.every").is_err() && hit("t.every").is_err());
        });
        with_spec("t.nth=error@2", || {
            assert!(hit("t.nth").is_ok(), "a new override counts from 1 again");
            assert!(hit("t.nth").is_err());
        });
    }

    #[cfg(debug_assertions)]
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

    #[cfg(debug_assertions)]
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
