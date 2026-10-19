//! Checkpoint-only tests (vision §5.12, D16): ignored by default, run on purpose at a
//! milestone checkpoint whose close reason quotes their result. libtest has no
//! "skipped" outcome, so a test that cannot run must not pass quietly: [`skip`] prints
//! one `SKIP <test>: <reason>` line and fails, unless `OMADDONS_ALLOW_SKIP` names the
//! test. The binary never reads that variable.

use std::io::Write;

/// The test-only variable naming the skips a run accepts: test names, comma-separated.
pub const ALLOW_SKIP: &str = "OMADDONS_ALLOW_SKIP";

/// Whether `allow` (the variable's value) names `test` exactly.
pub fn skip_allowed(allow: Option<&str>, test: &str) -> bool {
    allow.is_some_and(|list| list.split(',').any(|name| name.trim() == test))
}

/// `test` did not run, because of `reason`: one `SKIP` line on stderr (written directly,
/// so libtest does not capture it), then a failure unless the run accepts this skip.
pub fn skip(test: &str, reason: &str) {
    let _ = writeln!(std::io::stderr(), "SKIP {test}: {reason}");
    assert!(
        skip_allowed(std::env::var(ALLOW_SKIP).ok().as_deref(), test),
        "{test} did not run: {reason}. A checkpoint run must not skip it; to accept this skip, run with {ALLOW_SKIP}={test}"
    );
}

/// Whether `test` may run: each of `vars` is `1`. Otherwise it skips, naming the first
/// that is not, and returns false when that skip is accepted. Call it first, before
/// anything reads or writes the real home.
pub fn require(test: &str, vars: &[&str]) -> bool {
    match vars
        .iter()
        .find(|var| std::env::var(var).as_deref() != Ok("1"))
    {
        None => true,
        Some(var) => {
            skip(test, &format!("{var} is not 1"));
            false
        }
    }
}
