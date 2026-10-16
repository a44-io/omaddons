//! WoWInterface fixtures: the file list, gzipped as the API serves it, and
//! `filedetails` batches.

use std::path::Path;

use serde_json::{Value, json};

use super::http::{Resp, fixture_seq, gzip_bytes};

/// The file list's URL.
pub const FILELIST: &str = "https://api.mmoui.com/v3/game/WOW/filelist.json";

/// One file-list entry; what a test leaves alone gets a default.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub uid: u64,
    pub name: String,
    pub author: String,
    pub version: String,
    pub date_ms: u64,
    pub downloads: u64,
    pub dirs: Vec<String>,
    /// `(version, name)` pairs; empty serves `null`.
    pub compat: Vec<(String, String)>,
}

impl FileEntry {
    pub fn new(uid: u64, name: &str, dirs: &[&str]) -> Self {
        Self {
            uid,
            name: name.to_owned(),
            author: "someone".to_owned(),
            version: "1.0".to_owned(),
            date_ms: 1_760_300_000_000,
            downloads: 1_000,
            dirs: dirs.iter().map(|d| (*d).to_owned()).collect(),
            compat: Vec::new(),
        }
    }

    /// As the API writes it: ids and counts as strings, the date as a number.
    pub fn json(&self) -> Value {
        let compat: Value = if self.compat.is_empty() {
            Value::Null
        } else {
            self.compat
                .iter()
                .map(|(version, name)| json!({"version": version, "name": name}))
                .collect()
        };
        json!({
            "UID": self.uid.to_string(),
            "UICATID": "19",
            "UIVersion": self.version,
            "UIDate": self.date_ms,
            "UIName": self.name,
            "UIAuthorName": self.author,
            "UIFileInfoURL": format!("https://www.wowinterface.com/downloads/info{}", self.uid),
            "UIDownloadTotal": self.downloads.to_string(),
            "UIDownloadMonthly": "10",
            "UIFavoriteTotal": "5",
            "UICompatibility": compat,
            "UIDir": self.dirs,
            "UIIMG_Thumbs": null,
            "UIIMGs": null,
            "UISiblings": null,
        })
    }
}

/// Serves `entries` as the file list: gzipped, `cache-control: max-age=3600`.
pub fn wowi_filelist(scenario: &Path, entries: &[FileEntry]) {
    let list: Vec<Value> = entries.iter().map(FileEntry::json).collect();
    let body = serde_json::to_vec(&list).unwrap();
    let resp = Resp::ok(gzip_bytes(&body))
        .header("content-encoding", "gzip")
        .header("cache-control", "public, max-age=3600");
    fixture_seq(scenario, "wowi-filelist", FILELIST, &[resp]);
}

/// Serves one `filedetails/<ids>.json` batch of `details` (the WoWInterface source).
pub fn wowi_details(scenario: &Path, ids: &[u64], details: &[Value]) {
    let joined: Vec<String> = ids.iter().map(u64::to_string).collect();
    let joined = joined.join(",");
    let url = format!("https://api.mmoui.com/v3/game/WOW/filedetails/{joined}.json");
    let body = serde_json::to_vec(details).unwrap();
    fixture_seq(
        scenario,
        &format!("wowi-details-{joined}"),
        &url,
        &[Resp::ok(body)],
    );
}
