//! `<state>/omaddons/retention_sweep.stamp`: when snapshot retention last passed over
//! every registered client discovery found, not only the one a commit changed
//! ([`crate::engine::retention::sweep`]). The pass runs at most once per
//! [`crate::limits::RETENTION_SWEEP_INTERVAL`], and this stamp is how a later command
//! knows.
//!
//! One line, the instant as RFC 3339. It is no versioned document (no `KINDS` row, no
//! published schema): only this module reads it, and anything it cannot read (missing,
//! empty, longer than [`STAMP_MAX_BYTES`], not an instant, or an instant in the future)
//! makes the next pass due, which writes it again. Deleting it is safe.

use std::io::Read;
use std::time::Duration;

use jiff::Timestamp;

use super::paths::Paths;
use crate::error::Result;
use crate::fsx::{Content, Op, WriteSession, mutate};

/// The most of the stamp that is read: an RFC 3339 instant and its newline fit well
/// within it, so a larger file is not a stamp.
const STAMP_MAX_BYTES: u64 = 64;

/// Whether a pass is due at `now`: the last one `interval` or more ago, or no stamp this
/// module can read.
pub fn due(paths: &Paths, now: Timestamp, interval: Duration) -> bool {
    let interval = i64::try_from(interval.as_secs()).unwrap_or(i64::MAX);
    match read(paths) {
        Some(at) => !(0..interval).contains(&now.as_second().saturating_sub(at.as_second())),
        None => true,
    }
}

/// The stamped instant, if the file reads as one.
fn read(paths: &Paths) -> Option<Timestamp> {
    let file = std::fs::File::open(paths.retention_sweep_file()).ok()?;
    let mut text = String::new();
    file.take(STAMP_MAX_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > STAMP_MAX_BYTES {
        return None;
    }
    text.trim().parse().ok()
}

/// Stamps a pass that finished at `at`.
pub fn write(s: &mut WriteSession<'_>, paths: &Paths, at: Timestamp) -> Result<()> {
    let text = format!("{at}\n");
    let op = Op::WriteFile {
        content: Content::Bytes(text.as_bytes()),
        mode: Some(0o644),
    };
    mutate(s, &paths.retention_sweep_file(), op)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::{ScopeTable, Writer};

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    fn now() -> Timestamp {
        "2026-11-07T12:00:00Z".parse().unwrap()
    }

    fn ago(secs: i64) -> Timestamp {
        Timestamp::from_second(now().as_second() - secs).unwrap()
    }

    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        (dir, paths)
    }

    fn stamp(paths: &Paths, at: Timestamp) {
        let scopes = ScopeTable::new(paths, &[], None).unwrap();
        let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "test");
        write(&mut s, paths, at).unwrap();
    }

    #[test]
    fn a_pass_is_due_by_its_stamp() {
        let cases: [(&str, Option<Timestamp>, bool); 5] = [
            ("no stamp", None, true),
            ("a minute ago", Some(ago(60)), false),
            ("a second short of a day", Some(ago(86_399)), false),
            ("a day ago", Some(ago(86_400)), true),
            ("in the future", Some(ago(-60)), true),
        ];
        for (case, at, due_now) in cases {
            let (_dir, paths) = home();
            if let Some(at) = at {
                stamp(&paths, at);
            }
            assert_eq!(due(&paths, now(), DAY), due_now, "{case}: {at:?}");
        }
    }

    #[test]
    fn a_stamp_that_does_not_read_is_due() {
        let long = format!("{}\n{}", ago(60), " ".repeat(64));
        let cases: [(&str, &str); 5] = [
            ("empty", ""),
            ("not an instant", "yesterday\n"),
            ("JSON", r#"{"at":"2026-11-07T11:59:00Z"}"#),
            ("a date without a time", "2026-11-07\n"),
            ("past its bound", &long),
        ];
        for (case, body) in cases {
            let (_dir, paths) = home();
            std::fs::write(paths.retention_sweep_file(), body).unwrap();
            assert!(due(&paths, now(), DAY), "{case}: {body:?}");
        }
        // Whitespace around a recent instant still reads.
        let (_dir, paths) = home();
        std::fs::write(paths.retention_sweep_file(), format!("  {}  \n", ago(60))).unwrap();
        assert!(!due(&paths, now(), DAY), "padded");
    }

    #[test]
    fn the_stamp_is_one_line_holding_the_instant() {
        let (_dir, paths) = home();
        stamp(&paths, ago(5));
        let text = std::fs::read_to_string(paths.retention_sweep_file()).unwrap();
        assert_eq!(text, format!("{}\n", ago(5)));
        assert_eq!(read(&paths), Some(ago(5)));
    }
}
