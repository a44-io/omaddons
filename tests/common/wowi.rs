//! WoWInterface fixtures: the file list, gzipped as the API serves it, `filedetails`
//! batches and entries, and the files their `UIDownload` links serve.

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
    let (name, url) = details_url(ids);
    let body = serde_json::to_vec(details).unwrap();
    fixture_seq(scenario, &name, &url, &[Resp::ok(body)]);
}

/// Serves the batch of `ids` as the API answers when it knows none of them: 404 with its
/// own error (verified 2026-10-16).
pub fn wowi_unknown(scenario: &Path, ids: &[u64]) {
    let (name, url) = details_url(ids);
    let mut resp = Resp::status(404).header("content-type", "application/json; charset=utf-8");
    resp.body = Some(br#"{"ERROR":"No AddOn found."}"#.to_vec());
    fixture_seq(scenario, &name, &url, &[resp]);
}

/// The fixture name and URL of the `filedetails` batch of `ids`, in that order.
fn details_url(ids: &[u64]) -> (String, String) {
    let joined: Vec<String> = ids.iter().map(u64::to_string).collect();
    let joined = joined.join(",");
    (
        format!("wowi-details-{joined}"),
        format!("https://api.mmoui.com/v3/game/WOW/filedetails/{joined}.json"),
    )
}

/// The CDN link `UIDownload` gives for `uid`'s file.
pub fn file_url(uid: u64) -> String {
    format!("https://cdn.wowinterface.com/downloads/getfile.php?id={uid}&d=1760300000&minion")
}

/// One `filedetails` entry as the API writes it: the id as a string, the date as a
/// number, the changelog as BBCode with CRLF, and `UIDownload` at [`file_url`]. `md5`
/// is the served file's ([`wowi_file`]).
pub fn details(uid: u64, name: &str, version: &str, md5: &str) -> Value {
    json!({
        "UID": uid.to_string(),
        "UICATID": "154",
        "UIVersion": version,
        "UIDate": 1_760_300_000_000_u64,
        "UIMD5": md5,
        "UIFileName": format!("{name}-{version}.zip"),
        "UIDownload": file_url(uid),
        "UIPending": "0",
        "UIName": name,
        "UIAuthorName": "someone",
        "UIDescription": format!("[b]{name}[/b] does things."),
        "UIChangeLog": format!("[SIZE=5]{name}[/SIZE]\r\n[list]\r\n[*]{version}: fixed [b]things[/b]\r\n[/list]"),
        "UIHitCount": "1000",
        "UIHitCountMonthly": "10",
        "UIFavoriteTotal": "5",
    })
}

/// Serves `zip` as `uid`'s file at [`file_url`]; returns its MD5, for `UIMD5`.
pub fn wowi_file(scenario: &Path, uid: u64, zip: &[u8]) -> String {
    use md5::{Digest, Md5};
    fixture_seq(
        scenario,
        &format!("wowi-file-{uid}"),
        &file_url(uid),
        &[Resp::ok(zip.to_vec())],
    );
    Md5::digest(zip)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
