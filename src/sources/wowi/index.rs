//! The WoWInterface project index (M2-D12): every project's id, name, author, version,
//! date, counts, category, folders and declared compatibility, from the one file list
//! (`filelist.json`, about 5.6 MB of JSON, 0.66 MB gzipped), kept slim in
//! `<cache>/omaddons/wowi/index.json`.
//!
//! WoWInterface sends no validators and ignores conditional requests, so the stored
//! index is refreshed only when it is older than `max(WOWI_FILELIST_MIN_AGE, max-age)`
//! (or on `--refresh`). A failed refresh falls back to the stored index with
//! `stale_check`; offline, the stored index serves at any age. The index is derived
//! cache data: unreadable means absent, never an error for the caller.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::model::FolderName;
use crate::net::{Http, Request};
use crate::politeness::WOWI_FILELIST_MIN_AGE;
use crate::resolve::cache::Freshness;
use crate::sources::{SourceId, SourceSkipReason, SourceSkipped};
use crate::state::paths::Paths;
use crate::state::store::{self, SchemaVersion, VersionStatus, Versioned};

/// The one file list.
pub const FILELIST_URL: &str = "https://api.mmoui.com/v3/game/WOW/filelist.json";

/// One project, slim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WowiEntry {
    pub uid: u64,
    pub name: String,
    pub author: String,
    /// Free-form; may be empty.
    pub version: String,
    /// Epoch milliseconds.
    pub date_ms: u64,
    pub downloads: u64,
    pub monthly: u64,
    pub favorites: u64,
    pub cat: u64,
    /// Its folders, sanitised (no junk, valid names, no case duplicates).
    pub dirs: Vec<FolderName>,
    /// What the author declares; lags behind, so only ever a search hint (M2-D11).
    pub compat: Vec<WowiCompat>,
}

/// A client version a project declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WowiCompat {
    pub version: String,
    pub name: String,
}

/// `<cache>/omaddons/wowi/index.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StoredIndex {
    #[schemars(with = "String")]
    pub fetched_at: Timestamp,
    /// The response's `Cache-Control: max-age`, 0 when it had none.
    pub max_age_s: u64,
    /// By uid.
    pub entries: Vec<WowiEntry>,
}

impl Versioned for StoredIndex {
    const KIND: &'static str = "wowi_index";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// How a command wants the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexPolicy {
    /// The stored index while it is fresh, else a refresh.
    Auto,
    /// A refresh whatever its age (`--refresh`).
    Refresh,
    /// Never a request; the stored index at any age (`--offline`, which wins).
    Offline,
}

impl From<Freshness> for IndexPolicy {
    fn from(freshness: Freshness) -> Self {
        match freshness {
            Freshness::Window(_) | Freshness::Revalidate => Self::Auto,
            Freshness::Refresh => Self::Refresh,
            Freshness::OfflineOnly => Self::Offline,
        }
    }
}

/// What loading gave: the index (or none to use), warnings for the outcome, and
/// diagnostics for `-v`.
#[derive(Debug, Default)]
pub struct IndexLoad {
    pub index: Option<WowiIndex>,
    pub warnings: Vec<Warning>,
    pub diagnostics: Vec<String>,
}

impl IndexLoad {
    /// `{source: wowi, reason: no_index}` when there is no index to use.
    pub fn skipped(&self) -> Option<SourceSkipped> {
        self.index.is_none().then_some(SourceSkipped {
            source: SourceId::Wowi,
            reason: SourceSkipReason::NoIndex,
            reset_at: None,
        })
    }
}

/// The index, with its lookups.
#[derive(Debug, Clone)]
pub struct WowiIndex {
    fetched_at: Timestamp,
    entries: Vec<WowiEntry>,
    /// Folder key → uids, ascending.
    by_folder: BTreeMap<String, Vec<u64>>,
    /// Uid → position in `entries`.
    by_uid: BTreeMap<u64, usize>,
}

impl WowiIndex {
    fn build(stored: StoredIndex) -> Self {
        let mut entries = stored.entries;
        entries.sort_by_key(|e| e.uid);
        entries.dedup_by_key(|e| e.uid);
        let mut by_folder: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        let mut by_uid = BTreeMap::new();
        for (i, e) in entries.iter().enumerate() {
            by_uid.insert(e.uid, i);
            for dir in &e.dirs {
                by_folder
                    .entry(dir.key().to_owned())
                    .or_default()
                    .push(e.uid);
            }
        }
        // Entries are in uid order, so each list already ascends.
        Self {
            fetched_at: stored.fetched_at,
            entries,
            by_folder,
            by_uid,
        }
    }

    /// The stored index, or a fresh one, as `policy` allows.
    pub fn load_or_refresh(
        paths: &Paths,
        http: &dyn Http,
        policy: IndexPolicy,
    ) -> Result<IndexLoad> {
        Self::load_or_refresh_at(paths, http, policy, Timestamp::now())
    }

    /// [`WowiIndex::load_or_refresh`] at `now`.
    pub fn load_or_refresh_at(
        paths: &Paths,
        http: &dyn Http,
        policy: IndexPolicy,
        now: Timestamp,
    ) -> Result<IndexLoad> {
        let mut load = IndexLoad::default();
        let stored = load_stored(paths, &mut load.diagnostics);
        let fresh = stored
            .as_ref()
            .is_some_and(|s| is_fresh(s.fetched_at, s.max_age_s, now));
        if policy == IndexPolicy::Offline || (policy == IndexPolicy::Auto && fresh) {
            if let Some(s) = stored {
                if !fresh {
                    load.warnings.push(stale(s.fetched_at));
                }
                load.index = Some(Self::build(s));
            }
            return Ok(load);
        }
        match fetch(http, now, &mut load.diagnostics) {
            Ok(fetched) => {
                if let Err(why) = write_stored(paths, &fetched) {
                    load.diagnostics
                        .push(format!("WoWInterface index not written: {why}"));
                }
                load.index = Some(Self::build(fetched));
            }
            Err(error) => {
                load.diagnostics
                    .push(format!("WoWInterface file list: {}", error.message));
                if let Some(s) = stored {
                    load.warnings.push(stale(s.fetched_at));
                    load.index = Some(Self::build(s));
                }
            }
        }
        Ok(load)
    }

    /// The uids of the projects shipping `folder` (ignoring case), ascending.
    pub fn by_folder(&self, folder: &FolderName) -> &[u64] {
        self.by_folder.get(folder.key()).map_or(&[], Vec::as_slice)
    }

    pub fn by_uid(&self, uid: u64) -> Option<&WowiEntry> {
        self.by_uid.get(&uid).and_then(|i| self.entries.get(*i))
    }

    /// Every project, by uid.
    pub fn entries(&self) -> &[WowiEntry] {
        &self.entries
    }

    pub fn fetched_at(&self) -> Timestamp {
        self.fetched_at
    }
}

/// Whether an index fetched at `fetched_at` with `max_age_s` is still fresh at `now`:
/// younger than `max(WOWI_FILELIST_MIN_AGE, max-age)`, and not in the future.
fn is_fresh(fetched_at: Timestamp, max_age_s: u64, now: Timestamp) -> bool {
    let window = WOWI_FILELIST_MIN_AGE.max(Duration::from_secs(max_age_s));
    fetched_at <= now && now.duration_since(fetched_at).unsigned_abs() < window
}

fn stale(fetched_at: Timestamp) -> Warning {
    Warning::new(
        WarningCode::StaleCheck,
        format!("the WoWInterface index is from {fetched_at}"),
    )
    .with_fix(Suggestion::text(
        "run the command again online to refresh it",
    ))
}

/// The stored index, when it is readable and not newer than this binary.
fn load_stored(paths: &Paths, diagnostics: &mut Vec<String>) -> Option<StoredIndex> {
    match store::load::<StoredIndex>(&paths.wowi_index()) {
        Ok(Some(loaded)) if loaded.status != VersionStatus::Newer => Some(loaded.value),
        Ok(Some(_)) => {
            diagnostics.push("WoWInterface index written by a newer omaddons; not used".into());
            None
        }
        Ok(None) => None,
        Err(error) => {
            diagnostics.push(format!("WoWInterface index unreadable: {}", error.message));
            None
        }
    }
}

/// Writes the index through an untracked, derived-data session.
fn write_stored(paths: &Paths, index: &StoredIndex) -> std::result::Result<(), String> {
    let bytes = store::encode(index).map_err(|e| e.message.clone())?;
    let scopes = ScopeTable::new(paths, &[], None).map_err(|e| e.message.clone())?;
    let mut s = WriteSession::untracked(Writer::Derived, &scopes, "wowi-index");
    ensure_dir_all(&mut s, &paths.wowi_cache_dir()).map_err(|e| e.to_string())?;
    let op = Op::WriteFile {
        content: Content::Bytes(&bytes),
        mode: Some(0o644),
    };
    mutate(&mut s, &paths.wowi_index(), op).map_err(|e| e.to_string())?;
    Ok(())
}

/// Fetches and decodes the file list.
fn fetch(http: &dyn Http, now: Timestamp, diagnostics: &mut Vec<String>) -> Result<StoredIndex> {
    let resp = http
        .send(Request::get(FILELIST_URL, "wowi-filelist"))?
        .error_for_status("wowi-filelist")?;
    let max_age_s = max_age(resp.header("cache-control"));
    let body = resp.bytes()?;
    let (entries, skipped) = decode_filelist(&body)?;
    if skipped > 0 {
        diagnostics.push(format!(
            "WoWInterface file list: {skipped} entries without a usable UID skipped"
        ));
    }
    Ok(StoredIndex {
        fetched_at: now,
        max_age_s,
        entries,
    })
}

/// The `max-age` of a `Cache-Control` header, 0 when absent or unparsable.
fn max_age(cache_control: Option<&str>) -> u64 {
    cache_control
        .into_iter()
        .flat_map(|v| v.split(','))
        .find_map(|d| {
            let (key, value) = d.trim().split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("max-age")
                .then(|| value.trim().trim_matches('"').parse().ok())
                .flatten()
        })
        .unwrap_or(0)
}

/// The file list's entries, slim, and how many were skipped for an unusable `UID`.
/// Numbers may come as strings (unparsable is 0), nulls are empty, and anything but a
/// JSON array is an error.
pub fn decode_filelist(body: &[u8]) -> Result<(Vec<WowiEntry>, usize)> {
    let not_a_list = |why: String| {
        Error::new(
            ErrorCode::Network,
            format!("the WoWInterface file list is not a JSON array ({why})"),
            Suggestion::text("retry later; if it persists, the source changed its format"),
        )
    };
    let value: Value = serde_json::from_slice(body).map_err(|e| not_a_list(e.to_string()))?;
    let Value::Array(items) = value else {
        return Err(not_a_list("another JSON value".into()));
    };
    let mut entries = Vec::with_capacity(items.len());
    let mut skipped = 0;
    for item in &items {
        let Some(uid) = number(&item["UID"]).filter(|_| item.is_object()) else {
            skipped += 1;
            continue;
        };
        let dirs: Vec<String> = item["UIDir"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| d.as_str().map(str::to_owned))
            .collect();
        let compat = item["UICompatibility"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| WowiCompat {
                version: text(&c["version"]),
                name: text(&c["name"]),
            })
            .collect();
        entries.push(WowiEntry {
            uid,
            name: text(&item["UIName"]),
            author: text(&item["UIAuthorName"]),
            version: text(&item["UIVersion"]),
            date_ms: number(&item["UIDate"]).unwrap_or(0),
            downloads: number(&item["UIDownloadTotal"]).unwrap_or(0),
            monthly: number(&item["UIDownloadMonthly"]).unwrap_or(0),
            favorites: number(&item["UIFavoriteTotal"]).unwrap_or(0),
            cat: number(&item["UICATID"]).unwrap_or(0),
            dirs: sanitize_dirs(&dirs),
            compat,
        });
    }
    Ok((entries, skipped))
}

/// A non-negative integer given as a number or a string.
pub(super) fn number(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A string, or a number written as one; anything else is empty.
pub(super) fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Folder names a project lists, without junk (empty, dot-names, `__MACOSX`,
/// `Interface`, `WTF`), invalid names, or case duplicates (the first spelling stays).
pub fn sanitize_dirs(dirs: &[String]) -> Vec<FolderName> {
    const JUNK: [&str; 3] = ["__MACOSX", "Interface", "WTF"];
    let mut out: Vec<FolderName> = Vec::new();
    for dir in dirs {
        if dir.is_empty()
            || dir.starts_with('.')
            || JUNK.iter().any(|j| j.eq_ignore_ascii_case(dir))
        {
            continue;
        }
        let Ok(name) = FolderName::new(dir) else {
            continue;
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;

    use super::*;

    fn f(name: &str) -> FolderName {
        FolderName::new(name).unwrap()
    }

    #[test]
    fn junk_invalid_and_case_duplicate_folders_are_dropped() {
        let raw: Vec<String> = [
            "BugSack",
            "",
            ".git",
            "__MACOSX",
            "interface",
            "WTF",
            "a/b",
            "bugsack",
            "!BugGrabber",
            "Interface_Extras",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let kept: Vec<String> = sanitize_dirs(&raw)
            .iter()
            .map(|d| d.as_str().to_owned())
            .collect();
        assert_eq!(kept, ["BugSack", "!BugGrabber", "Interface_Extras"]);
    }

    #[test]
    fn the_file_list_decodes_its_variants() {
        let body = br#"[
            {"UID":"5995","UIName":"BugSack","UIAuthorName":"funkydude","UIVersion":"v12.1.7",
             "UIDate":1760300000000,"UIDownloadTotal":"1234567","UIDownloadMonthly":"2345",
             "UIFavoriteTotal":"678","UICATID":"19","UIDir":["BugSack"],
             "UICompatibility":[{"version":"12.1.0","name":"Midnight"}],
             "UIFileInfoURL":"https://www.wowinterface.com/downloads/info5995","UIIMGs":["x"]},
            {"UID":7,"UIName":"Bare","UIVersion":3,"UIDate":"not a date","UIDownloadTotal":"lots",
             "UIDir":["Bare"],"UICompatibility":null},
            {"UID":"x","UIName":"No uid"},
            "not an object",
            {"UID":"8","UIName":"Junk only","UIDir":["__MACOSX"]}
        ]"#;
        let (entries, skipped) = decode_filelist(body).unwrap();
        assert_eq!(skipped, 2);
        assert_eq!(entries.len(), 3);
        let bugsack = &entries[0];
        assert_eq!(
            (
                bugsack.uid,
                bugsack.downloads,
                bugsack.monthly,
                bugsack.favorites,
                bugsack.cat
            ),
            (5995, 1_234_567, 2345, 678, 19)
        );
        assert_eq!(bugsack.date_ms, 1_760_300_000_000);
        assert_eq!(bugsack.compat[0].version, "12.1.0");
        let bare = &entries[1];
        assert_eq!(
            (
                bare.uid,
                bare.version.as_str(),
                bare.date_ms,
                bare.downloads,
                bare.compat.len()
            ),
            (7, "3", 0, 0, 0)
        );
        assert!(entries[2].dirs.is_empty(), "kept for search by name");
        assert!(decode_filelist(b"{}").is_err());
        assert!(decode_filelist(b"not json").is_err());
    }

    #[test]
    fn freshness_is_the_larger_of_the_floor_and_max_age() {
        let now = Timestamp::now();
        let ago = |mins: i64| now - SignedDuration::from_mins(mins);
        // No max-age: the 1 h floor.
        assert!(is_fresh(ago(30), 0, now));
        assert!(!is_fresh(ago(61), 0, now));
        // A larger max-age wins.
        assert!(is_fresh(ago(90), 2 * 3600, now));
        assert!(!is_fresh(ago(121), 2 * 3600, now));
        // A smaller one does not shorten the floor.
        assert!(is_fresh(ago(30), 60, now));
        // From the future: stale.
        assert!(!is_fresh(now + SignedDuration::from_mins(5), 3600, now));
        assert_eq!(max_age(Some("public, max-age=3600")), 3600);
        assert_eq!(max_age(Some("no-cache")), 0);
        assert_eq!(max_age(Some("max-age=abc")), 0);
        assert_eq!(max_age(None), 0);
    }

    #[test]
    fn lookups_by_folder_ignore_case_and_ascend() {
        let entry = |uid, dirs: &[&str]| WowiEntry {
            uid,
            name: format!("P{uid}"),
            author: String::new(),
            version: String::new(),
            date_ms: 0,
            downloads: 0,
            monthly: 0,
            favorites: 0,
            cat: 0,
            dirs: dirs.iter().map(|d| f(d)).collect(),
            compat: Vec::new(),
        };
        let index = WowiIndex::build(StoredIndex {
            fetched_at: Timestamp::now(),
            max_age_s: 0,
            entries: vec![
                entry(30, &["Shared", "Thirty"]),
                entry(10, &["shared"]),
                entry(20, &["Twenty"]),
            ],
        });
        assert_eq!(index.by_folder(&f("SHARED")), [10, 30]);
        assert_eq!(index.by_folder(&f("Twenty")), [20]);
        assert!(index.by_folder(&f("Nothing")).is_empty());
        assert_eq!(index.by_uid(30).map(|e| e.name.as_str()), Some("P30"));
        assert!(index.by_uid(99).is_none());
        let uids: Vec<u64> = index.entries().iter().map(|e| e.uid).collect();
        assert_eq!(uids, [10, 20, 30]);
    }

    #[test]
    fn freshness_maps_to_policies() {
        assert_eq!(
            IndexPolicy::from(Freshness::Window(Duration::from_secs(900))),
            IndexPolicy::Auto
        );
        assert_eq!(IndexPolicy::from(Freshness::Revalidate), IndexPolicy::Auto);
        assert_eq!(IndexPolicy::from(Freshness::Refresh), IndexPolicy::Refresh);
        assert_eq!(
            IndexPolicy::from(Freshness::OfflineOnly),
            IndexPolicy::Offline
        );
    }
}
