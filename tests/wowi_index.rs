//! The WoWInterface project index end to end, over replayed fixtures: one gzipped
//! `filelist.json` request at most per `max(1 h, max-age)`, a slim stored index,
//! sanitised folders, the stale fallback, offline use, and an unreadable store.

mod common;

use std::fs;

use common::env::TestEnv;
use common::http::{fixture_bytes, paths, replay};
use common::netlog::read_net_log;
use common::wowi::{FILELIST, FileEntry, wowi_filelist};
use jiff::{SignedDuration, Timestamp};
use omaddons::error::WarningCode;
use omaddons::model::FolderName;
use omaddons::sources::wowi::index::{IndexPolicy, StoredIndex, WowiEntry, WowiIndex};
use omaddons::state::store;
use serde_json::Value;
use support::{Setup, entries, f};

#[cfg(test)]
mod support {
    use std::path::PathBuf;

    use omaddons::net::Http;
    use omaddons::state::paths::Paths;

    use super::*;

    /// A home, a fixture scenario serving `entries`, and the stack over it.
    pub struct Setup {
        pub env: TestEnv,
        pub scenario: PathBuf,
        pub paths: Paths,
    }

    impl Setup {
        pub fn new(served: &[FileEntry]) -> Self {
            let env = TestEnv::new();
            let scenario = env.root().join("scenario");
            fs::create_dir_all(&scenario).unwrap();
            if !served.is_empty() {
                wowi_filelist(&scenario, served);
            }
            let paths = paths(&env);
            Self {
                env,
                scenario,
                paths,
            }
        }

        pub fn http(&self) -> Box<dyn Http> {
            replay(&self.env, &self.scenario)
        }

        pub fn load(&self, policy: IndexPolicy) -> omaddons::sources::wowi::index::IndexLoad {
            WowiIndex::load_or_refresh(&self.paths, self.http().as_ref(), policy).unwrap()
        }

        /// `wowi-filelist` requests so far.
        pub fn requests(&self) -> usize {
            read_net_log(&self.paths)
                .iter()
                .filter(|e| e.purpose == "wowi-filelist")
                .count()
        }

        /// Stores an index of `entries` fetched `age` ago.
        pub fn store(&self, entries: Vec<WowiEntry>, age: SignedDuration) {
            let stored = StoredIndex {
                fetched_at: Timestamp::now() - age,
                max_age_s: 3600,
                entries,
            };
            let path = self.paths.wowi_index();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, store::encode(&stored).unwrap()).unwrap();
        }
    }

    pub fn f(name: &str) -> FolderName {
        FolderName::new(name).unwrap()
    }

    /// BugSack, BugGrabber, and a project sharing a folder with BugSack.
    pub fn entries() -> Vec<FileEntry> {
        let mut bugsack = FileEntry::new(
            5995,
            "BugSack",
            &[
                "BugSack",
                "",
                ".git",
                "__MACOSX",
                "interface",
                "WTF",
                "a/b",
                "bugsack",
            ],
        );
        bugsack.compat = vec![("12.1.0".into(), "Midnight".into())];
        vec![
            bugsack,
            FileEntry::new(5996, "!BugGrabber", &["!BugGrabber"]),
            FileEntry::new(7000, "Sack Lite", &["BugSack"]),
        ]
    }
}

fn warning_codes(load: &omaddons::sources::wowi::index::IndexLoad) -> Vec<WarningCode> {
    load.warnings.iter().map(|w| w.code).collect()
}

#[test]
fn a_first_load_fetches_once_and_stores_a_slim_sanitised_index() {
    let s = Setup::new(&entries());
    let load = s.load(IndexPolicy::Auto);
    assert!(load.warnings.is_empty(), "{:?}", load.warnings);
    let index = load.index.unwrap();
    assert_eq!(s.requests(), 1);
    // Junk, invalid names and the case duplicate are gone; the first spelling stays.
    let bugsack = index.by_uid(5995).unwrap();
    let dirs: Vec<&str> = bugsack.dirs.iter().map(FolderName::as_str).collect();
    assert_eq!(dirs, ["BugSack"]);
    assert_eq!(bugsack.compat[0].version, "12.1.0");
    assert_eq!(index.by_folder(&f("bugsack")), [5995, 7000]);
    assert_eq!(index.by_folder(&f("!BUGGRABBER")), [5996]);
    for e in index.entries() {
        assert_eq!(index.by_uid(e.uid), Some(e));
    }
    // The net.log line saw the gzipped wire.
    let line = &read_net_log(&s.paths)[0];
    assert!(
        line.wire_bytes.is_some_and(|w| Some(w) < line.bytes),
        "{line:?}"
    );

    let stored: Value = serde_json::from_slice(&fs::read(s.paths.wowi_index()).unwrap()).unwrap();
    assert_eq!(stored["schema_version"], "1.0");
    assert_eq!(stored["max_age_s"], 3600);
    assert!(stored["fetched_at"].is_string());
    let first = &stored["entries"][0];
    assert_eq!(first["uid"], 5995);
    assert!(first.get("UIFileInfoURL").is_none() && first.get("UIName").is_none());
    assert_eq!(first["dirs"], serde_json::json!(["BugSack"]));
}

#[test]
fn the_index_is_refreshed_only_past_its_age_or_when_asked() {
    let s = Setup::new(&entries());
    s.load(IndexPolicy::Auto);
    s.load(IndexPolicy::Auto);
    assert_eq!(s.requests(), 1, "fresh: no request");
    s.load(IndexPolicy::Refresh);
    assert_eq!(s.requests(), 2, "--refresh always asks");

    let aged = Setup::new(&entries());
    aged.store(Vec::new(), SignedDuration::from_mins(61));
    let load = aged.load(IndexPolicy::Auto);
    assert_eq!(aged.requests(), 1, "past max(1 h, max-age): one request");
    assert_eq!(load.index.unwrap().entries().len(), 3);
}

#[test]
fn a_failed_refresh_falls_back_to_the_stored_index_or_to_none() {
    // The list answers 404: no retry, the refresh fails.
    let s = Setup::new(&[]);
    fixture_bytes(&s.scenario, "wowi-filelist", FILELIST, 404, &[], None);
    let load = s.load(IndexPolicy::Auto);
    assert!(load.index.is_none());
    assert!(load.warnings.is_empty());
    assert_eq!(
        serde_json::to_value(load.skipped()).unwrap(),
        serde_json::json!({"source": "wowi", "reason": "no_index"})
    );
    assert!(!load.diagnostics.is_empty());

    let kept = vec![WowiEntry {
        uid: 1,
        name: "Old".into(),
        author: String::new(),
        version: String::new(),
        date_ms: 0,
        downloads: 0,
        monthly: 0,
        favorites: 0,
        cat: 0,
        dirs: vec![f("Old")],
        compat: Vec::new(),
    }];
    s.store(kept, SignedDuration::from_hours(5));
    let load = s.load(IndexPolicy::Auto);
    assert_eq!(warning_codes(&load), [WarningCode::StaleCheck]);
    assert!(
        load.warnings[0]
            .message
            .contains("WoWInterface index is from")
    );
    assert!(load.skipped().is_none(), "the stale index is used");
    assert_eq!(load.index.unwrap().by_folder(&f("old")), [1]);
}

#[test]
fn offline_never_asks() {
    let s = Setup::new(&entries());
    let load = s.load(IndexPolicy::Offline);
    assert!(load.index.is_none() && load.warnings.is_empty());

    s.store(Vec::new(), SignedDuration::from_hours(5));
    let load = s.load(IndexPolicy::Offline);
    assert_eq!(warning_codes(&load), [WarningCode::StaleCheck]);
    assert!(load.index.is_some());

    s.store(Vec::new(), SignedDuration::from_mins(5));
    let load = s.load(IndexPolicy::Offline);
    assert!(load.warnings.is_empty() && load.index.is_some());
    assert_eq!(s.requests(), 0);
}

#[test]
fn an_unreadable_or_newer_store_is_absent() {
    let s = Setup::new(&entries());
    let path = s.paths.wowi_index();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{ not json").unwrap();
    let load = s.load(IndexPolicy::Offline);
    assert!(load.index.is_none() && load.warnings.is_empty());
    assert!(!load.diagnostics.is_empty());
    // Online, it is simply refetched (and rewritten).
    assert!(s.load(IndexPolicy::Auto).index.is_some());
    assert_eq!(s.requests(), 1);

    fs::write(
        &path,
        r#"{"schema_version":"9.0","fetched_at":"2026-10-15T20:01:02Z","max_age_s":3600,"entries":[]}"#,
    )
    .unwrap();
    let load = s.load(IndexPolicy::Offline);
    assert!(load.index.is_none() && load.warnings.is_empty());
}
