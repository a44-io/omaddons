//! Tukui fixtures: the project list, `/v1/addon/<slug>` handing out one download token
//! per call, token downloads (served with an `ETag`, or refused with 403 as a dead token
//! is), and changelogs.

use std::path::Path;

use serde_json::{Value, json};

use super::http::{Resp, fixture_seq};

/// The list's URL.
pub const LIST: &str = "https://api.tukui.org/v1/addons";

/// A project as the list writes it: `patch[]` the client versions it supports.
pub fn entry(
    id: i64,
    slug: &str,
    name: &str,
    version: &str,
    patch: &[&str],
    dirs: &[&str],
) -> Value {
    json!({
        "id": id,
        "slug": slug,
        "author": "someone",
        "name": name,
        "url": format!("https://api.tukui.org/v1/download/{slug}/LIST-TOKEN"),
        "version": version,
        "changelog_url": format!("https://api.tukui.org/v1/changelog/{slug}"),
        "ticket_url": format!("https://github.com/someone/{slug}/issues"),
        "git_url": format!("https://github.com/someone/{slug}"),
        "patch": patch,
        "last_update": "2026-10-13",
        "web_url": format!("https://tukui.org/{slug}"),
        "donate_url": "https://tukui.org/donate",
        "small_desc": format!("{name} does things."),
        "screenshot_url": null,
        "gallery_url": null,
        "logo_url": null,
        "logo_square_url": null,
        "directories": dirs,
        "desc": format!("{name} does many things."),
    })
}

/// ElvUI 16.00 as the list wrote it on 2026-10-15.
pub fn elvui() -> Value {
    entry(
        -2,
        "elvui",
        "ElvUI",
        "16.00",
        &["12.1.0", "5.5.4", "1.15.9", "2.5.6"],
        &["ElvUI", "ElvUI_Libraries", "ElvUI_Options"],
    )
}

/// Serves `entries` as the list (`cache-control: private, no-store`, as the API does).
pub fn tukui_list(scenario: &Path, entries: &[Value]) {
    let body = serde_json::to_vec(entries).unwrap();
    let resp = Resp::ok(body).header("cache-control", "private, no-store");
    fixture_seq(scenario, "tukui-list", LIST, &[resp]);
}

/// The download URL of `slug`'s `token`.
pub fn download_url(slug: &str, token: &str) -> String {
    format!("https://api.tukui.org/v1/download/{slug}/{token}")
}

/// Serves `/v1/addon/<slug>` once per token, in order (the last repeats): each call
/// hands out the next download URL, as the API does.
pub fn tukui_addon(scenario: &Path, entry: &Value, tokens: &[&str]) {
    let slug = entry["slug"].as_str().unwrap();
    let responses: Vec<Resp> = tokens
        .iter()
        .map(|token| {
            let mut one = entry.clone();
            one["url"] = Value::from(download_url(slug, token));
            Resp::ok(serde_json::to_vec(&one).unwrap()).header("cache-control", "private, no-store")
        })
        .collect();
    fixture_seq(
        scenario,
        &format!("tukui-addon-{slug}"),
        &format!("https://api.tukui.org/v1/addon/{slug}"),
        &responses,
    );
}

/// Serves `zip` at `slug`'s `token` with `etag` (quoted, as the API sends the MD5).
pub fn tukui_download(scenario: &Path, slug: &str, token: &str, zip: &[u8], etag: Option<&str>) {
    let mut resp = Resp::ok(zip.to_vec()).header(
        "content-disposition",
        &format!("attachment; filename={slug}.zip"),
    );
    if let Some(etag) = etag {
        resp = resp.header("etag", etag);
    }
    fixture_seq(
        scenario,
        &format!("tukui-download-{slug}-{token}"),
        &download_url(slug, token),
        &[resp],
    );
}

/// `slug`'s `token` refused with 403, as a token is a few seconds after its issue.
pub fn tukui_dead_token(scenario: &Path, slug: &str, token: &str) {
    fixture_seq(
        scenario,
        &format!("tukui-download-{slug}-{token}"),
        &download_url(slug, token),
        &[Resp::status(403)],
    );
}

/// Serves `slug`'s changelog page.
pub fn tukui_changelog(scenario: &Path, slug: &str, body: &str) {
    fixture_seq(
        scenario,
        &format!("tukui-changelog-{slug}"),
        &format!("https://api.tukui.org/v1/changelog/{slug}"),
        &[Resp::ok(body.as_bytes().to_vec())],
    );
}

/// The `"<md5>"` `ETag` of `bytes`.
pub fn etag(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    let hex: String = Md5::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("\"{hex}\"")
}
