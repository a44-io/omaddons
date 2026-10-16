//! Tukui (keyless `api.tukui.org/v1`): ElvUI and Tukui. One `GET /v1/addons` lists every
//! project and serves every `tukui:` ref of a command. Download URLs are tokens that die
//! within seconds (verified 2026-10-15), so assets are `Deferred` (M2-D13): `GET
//! /v1/addon/<slug>` resolves a fresh token right before each download, and the
//! response's `ETag`, the MD5 of the bytes, is checked. No token is ever stored: the
//! decoders drop every `url`, and `net::redact` cuts them from logs and messages.
//!
//! The list's `patch[]` only ranks candidates (it lags behind patches); the TOCs decide.

use std::sync::{Mutex, OnceLock, PoisonError};

use jiff::Timestamp;
use serde_json::Value;

use super::changelog::{Changelog, ChangelogEntry, notes_text};
use super::{ResolvedAsset, Source, SourceId, Wanted};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{
    Asset, AssetFlavor, AssetLocation, Channel, HashFromHeader, Ref, Release, ReleaseSet,
};
use crate::net::{Http, Request};
use crate::rules::{ClientVersion, InstallKind, RulesTable, interface_for};
use crate::state::memo::{TukuiEntry, TukuiList};

/// Every project.
pub const LIST_URL: &str = "https://api.tukui.org/v1/addons";
/// One project, `<ADDON_URL>/<slug>`, with a fresh download token.
pub const ADDON_URL: &str = "https://api.tukui.org/v1/addon";

/// One project as the list (or `/v1/addon/<slug>`) describes it, without its `url`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TukuiAddon {
    pub id: i64,
    /// Lowercased.
    pub slug: String,
    pub name: String,
    pub author: String,
    pub version: String,
    /// Client versions it supports (`12.1.0`), as the list says.
    pub patch: Vec<String>,
    pub directories: Vec<String>,
    /// `YYYY-MM-DD`.
    pub last_update: String,
    pub changelog_url: Option<String>,
}

impl TukuiAddon {
    /// The memo's slim entry (no URL).
    pub fn entry(&self) -> TukuiEntry {
        TukuiEntry {
            id: self.id,
            slug: self.slug.clone(),
            name: self.name.clone(),
            author: self.author.clone(),
            version: self.version.clone(),
            patch: self.patch.clone(),
            directories: self.directories.clone(),
            last_update: self.last_update.clone(),
        }
    }
}

/// The list as this command decoded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedList {
    pub fetched_at: Timestamp,
    pub addons: Vec<TukuiAddon>,
}

impl DecodedList {
    /// The memo's `tukui_list`.
    pub fn memo(&self) -> TukuiList {
        TukuiList {
            fetched_at: self.fetched_at,
            entries: self.addons.iter().map(TukuiAddon::entry).collect(),
        }
    }
}

/// The `tukui:` source. It keeps the first list it decodes for the registry's lifetime
/// (one command), so `fetch` and `ReleaseCache::tukui_list` share one request.
#[derive(Debug, Default)]
pub struct TukuiSource {
    list: Mutex<Option<DecodedList>>,
    /// The builtin rules, which map `patch[]` to flavors; built on first use.
    table: OnceLock<RulesTable>,
}

impl TukuiSource {
    /// The list: the one decoded earlier in this command, else one `GET /v1/addons`
    /// (`tukui-list`, never cached by validators: the API says `no-store`).
    pub fn list(&self, http: &dyn Http) -> Result<DecodedList> {
        let mut kept = self.list.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(list) = kept.as_ref() {
            return Ok(list.clone());
        }
        let body = http
            .send(Request::get(LIST_URL, "tukui-list"))?
            .error_for_status("tukui-list")?
            .bytes()?;
        let list = DecodedList {
            fetched_at: Timestamp::now(),
            addons: decode_list(&body)?,
        };
        *kept = Some(list.clone());
        Ok(list)
    }

    /// The list this command decoded, if any.
    pub fn decoded(&self) -> Option<DecodedList> {
        self.list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The one release of `addon`.
    pub fn release(&self, addon: &TukuiAddon) -> Release {
        let slug = &addon.slug;
        Release {
            release_id: format!("tukui:{slug}@{}", addon.version),
            version: addon.version.clone(),
            channel: Channel::Stable,
            published_at: midnight(&addon.last_update),
            assets: vec![Asset {
                filename: format!("{slug}-{}.zip", addon.version),
                location: AssetLocation::Deferred {
                    source: SourceId::Tukui,
                    key: slug.clone(),
                },
                size: None,
                expected_hash: None,
                flavors: self.flavors(&addon.patch),
                name_flavors: Vec::new(),
                nolib: false,
            }],
            changelog_url: addon.changelog_url.clone(),
            notes: None,
            name_hint: Some(slug.clone()),
        }
    }

    /// `patch[]` as release metadata: each parsable client version's family flavor and
    /// interface, by the builtin rules, once each. Families without a packager flavor
    /// (legacy clients) are left out.
    pub fn flavors(&self, patch: &[String]) -> Vec<AssetFlavor> {
        let table = self.table.get_or_init(RulesTable::builtin);
        let mut flavors: Vec<AssetFlavor> = Vec::new();
        for version in patch
            .iter()
            .filter_map(|p| p.trim().parse::<ClientVersion>().ok())
        {
            let family = table.family_for(&version, InstallKind::Battlenet);
            let Some(flavor) = family.release_flavor() else {
                continue;
            };
            let interface =
                interface_for(&version, table.rules_for(&version, InstallKind::Battlenet));
            let flavor = AssetFlavor {
                flavor: flavor.to_owned(),
                interface: Some(interface),
            };
            if !flavors.contains(&flavor) {
                flavors.push(flavor);
            }
        }
        flavors
    }
}

impl Source for TukuiSource {
    fn id(&self) -> SourceId {
        SourceId::Tukui
    }

    /// One entry, the latest version's, from its `changelog_url` (one request; HTML
    /// stripped, text kept). [`super::changelog::changelog_for`] asks nothing when the
    /// installed version is the latest.
    fn changelog(
        &self,
        r: &Ref,
        latest: &Release,
        _since: Option<&str>,
        _limit: usize,
        http: &dyn Http,
    ) -> Option<Result<Changelog>> {
        let Ref::Tukui { slug } = r else {
            return None;
        };
        let url = latest.changelog_url.clone().unwrap_or_else(|| {
            format!(
                "https://api.tukui.org/v1/changelog/{}",
                slug.to_ascii_lowercase()
            )
        });
        let fetched = http
            .send(Request::get(url, "tukui-changelog"))
            .and_then(|resp| resp.error_for_status("Tukui's changelog"))
            .and_then(|resp| resp.bytes());
        Some(fetched.map(|body| Changelog {
            source: SourceId::Tukui,
            entries: vec![ChangelogEntry {
                version: latest.version.clone(),
                published_at: latest.published_at,
                text: notes_text(&String::from_utf8_lossy(&body)),
            }],
            truncated: false,
            more_url: Some(canonical_url(slug)),
        }))
    }

    fn fetch(&self, wanted: &[Wanted], http: &dyn Http) -> Vec<Result<ReleaseSet>> {
        let list = match self.list(http) {
            Ok(list) => list,
            Err(error) => return wanted.iter().map(|_| Err(error.clone())).collect(),
        };
        wanted
            .iter()
            .map(|w| {
                let Ref::Tukui { slug } = &w.r#ref else {
                    return Err(Error::internal(format!("the tukui source got {}", w.r#ref)));
                };
                let addon = list
                    .addons
                    .iter()
                    .find(|a| a.slug.eq_ignore_ascii_case(slug))
                    .ok_or_else(|| missing(slug))?;
                Ok(ReleaseSet {
                    r#ref: w.r#ref.clone(),
                    releases: vec![self.release(addon)],
                })
            })
            .collect()
    }

    /// A fresh token URL for `slug`'s file, checked by the download's `ETag` MD5. A 404
    /// is `not_found`; a project without a URL is a network error.
    fn resolve_asset(&self, slug: &str, http: &dyn Http) -> Result<ResolvedAsset> {
        if !is_slug(slug) {
            return Err(Error::internal(format!("{slug:?} is not a Tukui slug")));
        }
        let canonical = canonical_url(slug);
        let resp = http.send(Request::get(canonical.clone(), "tukui-addon"))?;
        if resp.status == 404 {
            return Err(missing(slug));
        }
        let body = resp.error_for_status("tukui-addon")?.bytes()?;
        let (_, url) = decode_addon(&body)?;
        let url = url.ok_or_else(|| {
            Error::new(
                ErrorCode::Network,
                format!("Tukui gave no download for tukui:{slug}"),
                Suggestion::text("retry later; if it persists, the source changed its format"),
            )
        })?;
        Ok(ResolvedAsset {
            url,
            expected_hash: None,
            hash_from_header: Some(HashFromHeader::EtagMd5),
            canonical_url: Some(canonical),
        })
    }
}

/// The stable metadata URL plans and `info -v` show for `slug`'s download.
pub fn canonical_url(slug: &str) -> String {
    format!("{ADDON_URL}/{slug}")
}

/// `not_found` for a slug Tukui does not list: the fix searches for it by that name.
fn missing(slug: &str) -> Error {
    let slug = slug.to_ascii_lowercase();
    Error::new(
        ErrorCode::NotFound,
        format!("tukui:{slug} is not on Tukui"),
        Suggestion::command(["omaddons", "search", slug.as_str()]),
    )
}

/// Letters, digits, `-` and `_`: safe in a URL path.
fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `YYYY-MM-DD` at midnight UTC.
fn midnight(date: &str) -> Option<Timestamp> {
    let date: jiff::civil::Date = date.trim().parse().ok()?;
    date.to_zoned(jiff::tz::TimeZone::UTC)
        .ok()
        .map(|z| z.timestamp())
}

fn not_json(what: &str, why: String) -> Error {
    Error::new(
        ErrorCode::Network,
        format!("the Tukui {what} is not what the API sends ({why})"),
        Suggestion::text("retry later; if it persists, the source changed its format"),
    )
}

/// The list's projects (`GET /v1/addons`): a JSON array of objects. Projects without a
/// usable slug are dropped; every `url` is dropped (it is a token).
pub fn decode_list(body: &[u8]) -> Result<Vec<TukuiAddon>> {
    let value: Value =
        serde_json::from_slice(body).map_err(|e| not_json("addon list", e.to_string()))?;
    let Value::Array(items) = value else {
        return Err(not_json("addon list", "not a JSON array".into()));
    };
    Ok(items.iter().filter_map(addon).collect())
}

/// One project (`GET /v1/addon/<slug>`) and its download URL.
pub fn decode_addon(body: &[u8]) -> Result<(TukuiAddon, Option<String>)> {
    let value: Value =
        serde_json::from_slice(body).map_err(|e| not_json("addon", e.to_string()))?;
    let found = addon(&value).ok_or_else(|| not_json("addon", "no project".into()))?;
    let url = value["url"]
        .as_str()
        .filter(|u| u.starts_with("https://"))
        .map(str::to_owned);
    Ok((found, url))
}

/// A project object, or `None` without a usable slug.
fn addon(item: &Value) -> Option<TukuiAddon> {
    let slug = item["slug"].as_str()?.trim().to_ascii_lowercase();
    if !is_slug(&slug) {
        return None;
    }
    let text = |key: &str| match &item[key] {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    let list = |key: &str| -> Vec<String> {
        item[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_owned()))
            .filter(|s| !s.is_empty())
            .collect()
    };
    Some(TukuiAddon {
        id: match &item["id"] {
            Value::Number(n) => n.as_i64().unwrap_or(0),
            Value::String(s) => s.trim().parse().unwrap_or(0),
            _ => 0,
        },
        name: text("name"),
        author: text("author"),
        version: Some(text("version"))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        patch: list("patch"),
        directories: list("directories"),
        last_update: text("last_update"),
        changelog_url: item["changelog_url"]
            .as_str()
            .filter(|u| u.starts_with("https://"))
            .map(str::to_owned),
        slug,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;
    use crate::net::Response;

    /// ElvUI as the list writes it (trimmed), with a token `url`.
    fn elvui() -> Value {
        json!({
            "id": -2,
            "slug": "elvui",
            "author": "Elv",
            "name": "ElvUI",
            "url": "https://api.tukui.org/v1/download/elvui/SECRET-TOKEN",
            "version": "16.00",
            "changelog_url": "https://api.tukui.org/v1/changelog/elvui",
            "patch": ["12.1.0", "5.5.4", "1.15.9", "2.5.6"],
            "directories": ["ElvUI", "ElvUI_Libraries", "ElvUI_Options"],
            "last_update": "2026-10-13",
            "web_url": "https://tukui.org/elvui",
        })
    }

    /// Answers the list and `/v1/addon/<slug>` from canned bodies, counting requests by
    /// purpose; each addon call hands out the next token.
    #[derive(Default)]
    struct Api {
        list: Option<(u16, String)>,
        addons: HashMap<String, Value>,
        sent: Mutex<Vec<&'static str>>,
        tokens: AtomicUsize,
    }

    impl Api {
        fn sent(&self, purpose: &str) -> usize {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter(|p| **p == purpose)
                .count()
        }
    }

    impl Http for Api {
        fn send(&self, req: Request) -> Result<Response> {
            self.sent.lock().unwrap().push(req.purpose);
            assert!(!req.validators, "no-store: never cached");
            let (status, body) = if req.url == LIST_URL {
                self.list.clone().unwrap_or((404, String::new()))
            } else {
                let slug = req.url.strip_prefix(&format!("{ADDON_URL}/")).unwrap();
                match self.addons.get(slug) {
                    Some(addon) => {
                        let n = self.tokens.fetch_add(1, Ordering::SeqCst) + 1;
                        let mut addon = addon.clone();
                        addon["url"] =
                            json!(format!("https://api.tukui.org/v1/download/{slug}/TOKEN{n}"));
                        (200, addon.to_string())
                    }
                    None => (404, r#"{"detail":"Not Found"}"#.to_owned()),
                }
            };
            Ok(Response::new(
                status,
                Vec::new(),
                req.url,
                Box::new(std::io::Cursor::new(body.into_bytes())),
            ))
        }
    }

    fn w(s: &str) -> Wanted {
        Wanted::stable(s.parse().unwrap())
    }

    #[test]
    fn the_list_decodes_without_urls() {
        let body = json!([
            elvui(),
            {"id": "-1", "slug": "Tukui", "version": 20.463, "patch": null},
            {"slug": "no/slash"},
            {"name": "no slug"},
            "not an object",
        ]);
        let list = decode_list(body.to_string().as_bytes()).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, -2);
        assert_eq!(
            list[0].directories,
            ["ElvUI", "ElvUI_Libraries", "ElvUI_Options"]
        );
        assert_eq!(
            (list[1].id, list[1].slug.as_str(), list[1].version.as_str()),
            (-1, "tukui", "20.463"),
            "ids and versions may be strings or numbers; slugs are lowercased"
        );
        assert!(list[1].patch.is_empty());
        assert!(
            !format!("{list:?}").contains("SECRET"),
            "the url is dropped"
        );
        let entry = serde_json::to_string(&list[0].entry()).unwrap();
        assert!(!entry.contains("https"), "{entry}");
        for bad in [&b"{}"[..], b"null", b"<html>", b""] {
            assert_eq!(decode_list(bad).unwrap_err().code, ErrorCode::Network);
        }
        let (one, url) = decode_addon(elvui().to_string().as_bytes()).unwrap();
        assert_eq!(one.slug, "elvui");
        assert_eq!(
            url.as_deref(),
            Some("https://api.tukui.org/v1/download/elvui/SECRET-TOKEN")
        );
        assert!(decode_addon(b"[]").is_err());
    }

    #[test]
    fn patches_rank_as_flavors_by_the_rules() {
        let source = TukuiSource::default();
        let patch: Vec<String> = ["12.1.0", "5.5.4", "1.15.9", "2.5.6", "x", "12.1.0"]
            .map(str::to_owned)
            .to_vec();
        let flavors: Vec<(String, Option<u32>)> = source
            .flavors(&patch)
            .into_iter()
            .map(|f| (f.flavor, f.interface))
            .collect();
        assert_eq!(
            flavors,
            [
                ("mainline".to_owned(), Some(120_100)),
                ("mists".to_owned(), Some(50_504)),
                ("classic".to_owned(), Some(11_509)),
                ("bcc".to_owned(), Some(20_506)),
            ],
            "a bad version and a repeat are left out"
        );
    }

    #[test]
    fn a_project_is_one_deferred_release() {
        let source = TukuiSource::default();
        let addon = decode_list(json!([elvui()]).to_string().as_bytes())
            .unwrap()
            .remove(0);
        let release = source.release(&addon);
        assert_eq!(release.release_id, "tukui:elvui@16.00");
        assert_eq!(
            release.published_at.map(|t| t.to_string()).as_deref(),
            Some("2026-10-13T00:00:00Z")
        );
        assert_eq!(
            release.changelog_url.as_deref(),
            Some("https://api.tukui.org/v1/changelog/elvui")
        );
        assert_eq!(release.name_hint.as_deref(), Some("elvui"));
        let asset = &release.assets[0];
        assert_eq!(asset.filename, "elvui-16.00.zip");
        assert_eq!(
            asset.location,
            AssetLocation::Deferred {
                source: SourceId::Tukui,
                key: "elvui".into()
            }
        );
        assert_eq!(
            (asset.expected_hash.as_ref(), asset.flavors.len()),
            (None, 4)
        );
    }

    #[test]
    fn patches_rank_listed_families_declared_and_others_mismatched() {
        use crate::model::RankReason;
        use crate::resolve::select::select;

        let client = |version: &str| {
            let table = RulesTable::builtin();
            let version: ClientVersion = version.parse().unwrap();
            let mut target = crate::engine::testing::target(std::path::Path::new("/g/x"));
            target.family = table.family_for(&version, InstallKind::Battlenet);
            target.interface =
                interface_for(&version, table.rules_for(&version, InstallKind::Battlenet));
            target.version = version;
            target.refresh_rules(&table);
            target
        };
        let source = TukuiSource::default();
        let addon = decode_list(json!([elvui()]).to_string().as_bytes())
            .unwrap()
            .remove(0);
        let set = ReleaseSet {
            r#ref: "tukui:elvui".parse().unwrap(),
            releases: vec![source.release(&addon)],
        };
        for (version, rank) in [
            ("12.1.5.70391", RankReason::MetadataFlavor),
            ("5.5.4.65000", RankReason::MetadataInterface),
            ("1.15.9.64520", RankReason::MetadataInterface),
            ("1.60.1.70245", RankReason::MetadataMismatch),
        ] {
            let selection = select(&set, &client(version), Channel::Stable, None);
            let ranks: Vec<RankReason> = selection.candidates.iter().map(|c| c.rank).collect();
            assert_eq!(ranks, [rank], "{version}: still tried, the TOCs decide");
        }
    }

    #[test]
    fn one_list_serves_every_wish_and_unknown_slugs_are_not_found() {
        let api = Api {
            list: Some((200, json!([elvui()]).to_string())),
            ..Api::default()
        };
        let source = TukuiSource::default();
        let results = source.fetch(&[w("tukui:elvui"), w("tukui:nosuch")], &api);
        assert!(results[0].is_ok());
        let missing = results[1].as_ref().unwrap_err();
        assert_eq!(
            (missing.code, missing.exit().code()),
            (ErrorCode::NotFound, 3)
        );
        assert_eq!(missing.message, "tukui:nosuch is not on Tukui");
        assert_eq!(
            missing.fix.argv(),
            Some(
                ["omaddons", "search", "nosuch"]
                    .map(str::to_owned)
                    .as_slice()
            )
        );
        // Again in the same command: no second request.
        assert!(source.fetch(&[w("tukui:elvui")], &api)[0].is_ok());
        assert_eq!(source.list(&api).unwrap().addons.len(), 1);
        assert_eq!(api.sent("tukui-list"), 1);
        assert_eq!(source.decoded().unwrap().memo().entries.len(), 1);

        // A failed list fails every wish, and is not kept.
        let down = Api {
            list: Some((503, String::new())),
            ..Api::default()
        };
        let fresh = TukuiSource::default();
        for result in fresh.fetch(&[w("tukui:elvui"), w("tukui:tukui")], &down) {
            assert_eq!(result.unwrap_err().code, ErrorCode::HttpStatus);
        }
        assert!(fresh.decoded().is_none());
    }

    #[test]
    fn each_resolve_gets_a_fresh_token_checked_by_its_etag() {
        let api = Api {
            addons: [("elvui".to_owned(), elvui())].into(),
            ..Api::default()
        };
        let source = TukuiSource::default();
        let first = source.resolve_asset("elvui", &api).unwrap();
        let second = source.resolve_asset("elvui", &api).unwrap();
        assert_eq!(first.url, "https://api.tukui.org/v1/download/elvui/TOKEN1");
        assert_eq!(second.url, "https://api.tukui.org/v1/download/elvui/TOKEN2");
        assert_eq!(first.hash_from_header, Some(HashFromHeader::EtagMd5));
        assert_eq!(
            first.canonical_url.as_deref(),
            Some("https://api.tukui.org/v1/addon/elvui")
        );
        assert_eq!(api.sent("tukui-addon"), 2);
        let gone = source.resolve_asset("nosuch", &api).unwrap_err();
        assert_eq!(gone.code, ErrorCode::NotFound);
        assert_eq!(
            source.resolve_asset("../x", &api).unwrap_err().code,
            ErrorCode::Internal
        );
    }
}
