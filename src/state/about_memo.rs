//! The about memo, `<cache>/omaddons/about.json`: what each source last said of a
//! project (`sources::about`), so `info`'s `about` costs no request inside
//! `ABOUT_RECHECK_INTERVAL` and an offline `info` still answers
//! (omad-widget-user-first-1n2p.3).
//!
//! Derived data, like the release memo: written through fsx with no lock and merged on
//! write (two writers at once can lose one answer, which costs one request later), and
//! it can never fail a command. A missing, corrupt or newer file reads as empty, a
//! corrupt one is replaced by the next write, and a newer one is never overwritten. It
//! changes nothing `status` reports, so its writes ring no doorbell.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::paths::Paths;
use super::store::{self, SchemaVersion, VersionStatus, Versioned};
use crate::error::Result;
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::sources::about::AboutFacts;

/// The memo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AboutMemo {
    /// By the project's key (`sources::about::Project::key`).
    #[serde(default)]
    pub entries: BTreeMap<String, AboutEntry>,
}

/// One project's last answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AboutEntry {
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub fetched_at: Timestamp,
    /// What the source said; `null` when it knows no such project.
    pub found: Option<AboutFacts>,
}

impl AboutEntry {
    /// Fetched less than `window` before `now`. A `fetched_at` in the future (a clock
    /// that moved back) is not fresh.
    pub fn fresh(&self, now: Timestamp, window: Duration) -> bool {
        let Ok(window) = jiff::SignedDuration::try_from(window) else {
            return false;
        };
        self.fetched_at <= now && now.duration_since(self.fetched_at) < window
    }
}

impl Versioned for AboutMemo {
    const KIND: &'static str = "about_memo";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

impl AboutMemo {
    /// Keeps the `max` most recent answers.
    fn cap(&mut self, max: usize) {
        if self.entries.len() <= max {
            return;
        }
        let mut by_age: Vec<(Timestamp, String)> = self
            .entries
            .iter()
            .map(|(k, e)| (e.fetched_at, k.clone()))
            .collect();
        by_age.sort();
        let excess = self.entries.len() - max;
        for (_, key) in by_age.into_iter().take(excess) {
            self.entries.remove(&key);
        }
    }
}

/// The memo on disk; empty when it is missing, corrupt or written by a newer omaddons,
/// the last two with a `-v` note.
pub fn load(paths: &Paths) -> (AboutMemo, Option<String>) {
    let path = paths.about_memo();
    match store::load::<AboutMemo>(&path) {
        Ok(None) => (AboutMemo::default(), None),
        Ok(Some(loaded)) if loaded.status == VersionStatus::Newer => (
            AboutMemo::default(),
            Some(format!(
                "about memo: {} was written by a newer omaddons; not used",
                path.display()
            )),
        ),
        Ok(Some(loaded)) => (loaded.value, None),
        Err(error) => (
            AboutMemo::default(),
            Some(format!("about memo ignored: {}", error.message)),
        ),
    }
}

/// Records `entry` under `key` in the memo on disk, keeping at most `max` answers.
/// Best effort: `Some(note)` for `-v` when the write was skipped (a newer file) or
/// failed. A corrupt file is replaced; an unchanged one is not written.
pub fn record(paths: &Paths, key: &str, entry: AboutEntry, max: usize) -> Option<String> {
    let path = paths.about_memo();
    let on_disk = std::fs::read(&path).ok();
    let loaded = on_disk
        .as_deref()
        .map(|bytes| store::load_bytes::<AboutMemo>(&path, bytes));
    let mut memo = match loaded {
        // Newer, decoded or not: read, never overwritten.
        Some(loaded) if store::written_by_newer(&loaded) => {
            return Some(format!(
                "about memo: {} was written by a newer omaddons; not updated",
                path.display()
            ));
        }
        Some(Ok(loaded)) => loaded.value,
        Some(Err(_)) | None => AboutMemo::default(),
    };
    memo.entries.insert(key.to_owned(), entry);
    memo.cap(max);
    let written = (|| -> Result<()> {
        let json = store::encode(&memo)?;
        if on_disk.as_deref() == Some(json.as_slice()) {
            return Ok(());
        }
        let scopes = ScopeTable::new(paths, &[], None)?;
        let mut s = WriteSession::untracked(Writer::Derived, &scopes, "about-memo");
        ensure_dir_all(&mut s, paths.cache_dir())?;
        let op = Op::WriteFile {
            content: Content::Bytes(&json),
            mode: Some(0o644),
        };
        mutate(&mut s, &path, op)?;
        Ok(())
    })();
    written
        .err()
        .map(|error| format!("about memo not written: {}", error.message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at: &str) -> AboutEntry {
        AboutEntry {
            fetched_at: at.parse().unwrap(),
            found: None,
        }
    }

    #[test]
    fn the_oldest_answers_go_first() {
        let mut memo = AboutMemo::default();
        for (key, at) in [
            ("a", "2026-11-03T00:00:00Z"),
            ("b", "2026-11-01T00:00:00Z"),
            ("c", "2026-11-02T00:00:00Z"),
        ] {
            memo.entries.insert(key.into(), entry(at));
        }
        memo.cap(2);
        let kept: Vec<&str> = memo.entries.keys().map(String::as_str).collect();
        assert_eq!(kept, ["a", "c"]);
        memo.cap(5);
        assert_eq!(memo.entries.len(), 2, "under the cap nothing goes");
    }

    #[test]
    fn freshness_is_an_age_inside_the_window() {
        let e = entry("2026-11-08T00:00:00Z");
        let day = Duration::from_secs(24 * 3600);
        let at = |s: &str| s.parse::<Timestamp>().unwrap();
        assert!(e.fresh(at("2026-11-08T23:59:59Z"), day));
        assert!(!e.fresh(at("2026-11-09T00:00:00Z"), day), "a day old");
        assert!(
            !e.fresh(at("2026-11-07T00:00:00Z"), day),
            "fetched in the future"
        );
    }

    #[test]
    fn a_record_round_trips_and_a_newer_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::with_roots(root, root.join("c"), root.join("s"), root.join("k"));
        let found = AboutEntry {
            fetched_at: "2026-11-08T00:00:00Z".parse().unwrap(),
            found: Some(AboutFacts {
                summary: Some("Raid alerts".into()),
                author: Some("o".into()),
                updated_at: None,
                popularity: Some(crate::resolve::search::Popularity::Stars(3)),
            }),
        };
        assert_eq!(record(&paths, "github:o/r", found.clone(), 8), None);
        let (memo, note) = load(&paths);
        assert_eq!((memo.entries.get("github:o/r"), note), (Some(&found), None));

        let newer = br#"{"schema_version":"9.0","entries":{}}"#;
        std::fs::write(paths.about_memo(), newer).unwrap();
        assert!(record(&paths, "wowi:1", entry("2026-11-08T00:00:00Z"), 8).is_some());
        assert_eq!(std::fs::read(paths.about_memo()).unwrap(), newer);
        assert!(load(&paths).1.is_some(), "a newer file is not used");

        // A newer major's file in a shape this build cannot decode is newer all the same.
        let reshaped = br#"{"schema_version":"9.0","entries":"reshaped"}"#;
        std::fs::write(paths.about_memo(), reshaped).unwrap();
        let note = record(&paths, "wowi:1", entry("2026-11-08T00:00:00Z"), 8);
        assert!(note.is_some_and(|n| n.contains("newer")));
        assert_eq!(std::fs::read(paths.about_memo()).unwrap(), reshaped);
    }
}
