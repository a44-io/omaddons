//! WoWInterface (wowinterface.com, through its keyless `api.mmoui.com` API): the `wowi:`
//! source, and the project index ([`index`]), by folder and by name, that dependency
//! resolution, adoption and search read.
//!
//! `filedetails` answers up to [`WOWI_DETAILS_BATCH_SIZE`] ids per request. A project
//! serves exactly one file, its current one, so a release set holds one release, and
//! its identity is its content (M2-D11): `wowi:<id>@<UIMD5>`, versioned by `UIVersion`.
//! Nothing the API says about compatibility excludes a build; TOC verification decides.

pub mod index;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use self::index::{number, source_text, text};
use super::{Source, SourceId, Wanted};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::limits::CHANGELOG_MAX_CHARS;
use crate::model::{Asset, AssetLocation, Channel, ExpectedHash, Ref, Release, ReleaseSet};
use crate::net::{Http, Request};
use crate::politeness::WOWI_DETAILS_BATCH_SIZE;
use crate::text::bbcode_to_text;

/// `filedetails/<id>,<id>,….json` under this.
pub const DETAILS_URL: &str = "https://api.mmoui.com/v3/game/WOW/filedetails";

/// The `wowi:` source.
#[derive(Debug, Default)]
pub struct WowiSource;

/// One `filedetails` entry: what a release is made of. Missing fields are empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WowiDetails {
    pub uid: u64,
    pub name: String,
    pub version: String,
    /// `UIDate`, in epoch milliseconds.
    pub date_ms: Option<u64>,
    pub md5: String,
    pub file_name: String,
    pub download: String,
    /// BBCode, CRLF line ends.
    pub changelog: String,
}

impl Source for WowiSource {
    fn id(&self) -> SourceId {
        SourceId::Wowi
    }

    /// One request per batch of distinct ids, in first-seen order; every wish for an id
    /// (pinned or not) shares its answer, and a batch that fails fails each of its ids.
    fn fetch(&self, wanted: &[Wanted], http: &dyn Http) -> Vec<Result<ReleaseSet>> {
        let mut seen = BTreeSet::new();
        let ids: Vec<u64> = wanted
            .iter()
            .filter_map(|w| match w.r#ref {
                Ref::Wowi { id } => Some(id),
                _ => None,
            })
            .filter(|id| seen.insert(*id))
            .collect();
        let mut answers: BTreeMap<u64, Result<Option<WowiDetails>>> = BTreeMap::new();
        for batch in ids.chunks(WOWI_DETAILS_BATCH_SIZE) {
            match fetch_batch(batch, http) {
                Ok(mut found) => {
                    for id in batch {
                        answers.insert(*id, Ok(found.remove(id)));
                    }
                }
                Err(error) => {
                    for id in batch {
                        answers.insert(*id, Err(error.clone()));
                    }
                }
            }
        }
        wanted
            .iter()
            .map(|w| {
                let Ref::Wowi { id } = w.r#ref else {
                    return Err(Error::internal(format!("the wowi source got {}", w.r#ref)));
                };
                match answers.get(&id) {
                    Some(Ok(Some(details))) => release_set(w, details),
                    Some(Ok(None)) => Err(missing(&w.r#ref)),
                    Some(Err(error)) => Err(error.clone()),
                    None => Err(Error::internal(format!("{} was not fetched", w.r#ref))),
                }
            })
            .collect()
    }
}

/// One `filedetails` request: the entries by `UID`. A 404 carrying the API's own
/// `{"ERROR": …}` means none of the ids exists (verified 2026-10-16: `{"ERROR":"No AddOn
/// found."}`); any other failure is the batch's error.
fn fetch_batch(ids: &[u64], http: &dyn Http) -> Result<BTreeMap<u64, WowiDetails>> {
    let joined: Vec<String> = ids.iter().map(u64::to_string).collect();
    let url = format!("{DETAILS_URL}/{}.json", joined.join(","));
    let resp = http.send(Request::get(url, "wowi-details"))?;
    if resp.status == 404 {
        let status = resp.status;
        let body = resp.bytes()?;
        let none_found = serde_json::from_slice::<Value>(&body)
            .is_ok_and(|v| v.get("ERROR").is_some_and(Value::is_string));
        if none_found {
            return Ok(BTreeMap::new());
        }
        return Err(Error::http_status(
            status,
            "wowi-details: HTTP 404 from api.mmoui.com",
            Suggestion::text("retry later; if it persists, the source changed its format"),
        ));
    }
    let body = resp.error_for_status("wowi-details")?.bytes()?;
    Ok(decode_details(&body)?
        .into_iter()
        .map(|d| (d.uid, d))
        .collect())
}

/// A `filedetails` body's entries. `UID` and `UIDate` may be numbers or strings, other
/// fields missing or null are empty, entries without a usable `UID` are dropped, and
/// anything but a JSON array is an error.
pub fn decode_details(body: &[u8]) -> Result<Vec<WowiDetails>> {
    let not_a_list = |why: String| {
        Error::new(
            ErrorCode::Network,
            format!("WoWInterface file details are not a JSON array ({why})"),
            Suggestion::text("retry later; if it persists, the source changed its format"),
        )
    };
    let value: Value = serde_json::from_slice(body).map_err(|e| not_a_list(e.to_string()))?;
    let Value::Array(items) = value else {
        return Err(not_a_list("another JSON value".into()));
    };
    Ok(items
        .iter()
        .filter(|item| item.is_object())
        .filter_map(|item| {
            Some(WowiDetails {
                uid: number(&item["UID"])?,
                // Shown and matched: decoded and made safe at once.
                name: source_text(&item["UIName"]),
                version: source_text(&item["UIVersion"]),
                date_ms: number(&item["UIDate"]),
                md5: text(&item["UIMD5"]),
                file_name: text(&item["UIFileName"]),
                download: text(&item["UIDownload"]),
                changelog: text(&item["UIChangeLog"]),
            })
        })
        .collect())
}

/// `not_found` for an id the API does not know. The fix names `help install`: the id
/// carries no name a `search` could use.
fn missing(r: &Ref) -> Error {
    Error::new(
        ErrorCode::NotFound,
        format!("{r} does not exist or was removed from WoWInterface"),
        Suggestion::command(["omaddons", "help", "install"]),
    )
}

/// Whether two versions are equal, ignoring a leading `v` on either.
fn same_version(a: &str, b: &str) -> bool {
    let bare = |s: &str| s.strip_prefix(['v', 'V']).unwrap_or(s).to_owned();
    bare(a.trim()) == bare(b.trim())
}

/// The one release `details` describes, or `not_found` when `w` pins another version:
/// WoWInterface serves no old files.
fn release_set(w: &Wanted, details: &WowiDetails) -> Result<ReleaseSet> {
    if let Some(pin) = &w.version
        && !same_version(pin, &details.version)
    {
        return Err(Error::new(
            ErrorCode::NotFound,
            format!(
                "{} has no release {pin}: WoWInterface serves only its current file ({})",
                w.r#ref,
                if details.version.is_empty() {
                    "unversioned"
                } else {
                    details.version.as_str()
                }
            ),
            Suggestion::command([
                "omaddons".to_owned(),
                "install".to_owned(),
                w.r#ref.to_string(),
            ]),
        ));
    }
    Ok(ReleaseSet {
        r#ref: w.r#ref.clone(),
        releases: vec![release(details)],
    })
}

/// `wowi:<UID>@<md5>` (without a usable MD5: `@d<UIDate>`, else `@v<UIVersion>`), one
/// asset at `UIDownload` checked against `UIMD5`, the changelog as plain text.
pub fn release(details: &WowiDetails) -> Release {
    let uid = details.uid;
    let expected_hash = ExpectedHash::md5(details.md5.trim());
    let release_id = match (&expected_hash, details.date_ms) {
        (Some(md5), _) => format!("wowi:{uid}@{}", md5.hex()),
        (None, Some(date)) => format!("wowi:{uid}@d{date}"),
        (None, None) => format!("wowi:{uid}@v{}", details.version.trim()),
    };
    let published_at = details
        .date_ms
        .and_then(|ms| i64::try_from(ms).ok())
        .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok());
    let assets = (!details.download.is_empty())
        .then(|| Asset {
            filename: if details.file_name.is_empty() {
                format!("wowi-{uid}.zip")
            } else {
                details.file_name.clone()
            },
            location: AssetLocation::Url(details.download.clone()),
            size: None,
            expected_hash,
            flavors: Vec::new(),
            name_flavors: Vec::new(),
            nolib: false,
        })
        .into_iter()
        .collect();
    let notes = bbcode_to_text(&details.changelog, CHANGELOG_MAX_CHARS);
    Release {
        release_id,
        version: if details.version.trim().is_empty() {
            "unknown".to_owned()
        } else {
            details.version.trim().to_owned()
        },
        channel: Channel::Stable,
        published_at,
        assets,
        changelog_url: Some(format!("https://www.wowinterface.com/downloads/info{uid}")),
        notes: (!notes.is_empty()).then_some(notes),
        name_hint: Some(details.name.trim().to_owned()).filter(|n| !n.is_empty()),
        revision: None,
    }
}

/// Why a download of `release_id` goes unchecked: a WoWInterface file whose details
/// published no usable MD5 (`expected` is then `None`). Other sources' assets without a
/// hash (GitHub's) never had one to check, and say nothing.
pub fn unpublished_md5(release_id: &str, expected: Option<&ExpectedHash>) -> Option<String> {
    (release_id.starts_with("wowi:") && expected.is_none())
        .then(|| "WoWInterface published no usable MD5 for it".to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::net::Response;

    /// Answers `filedetails` from `entries` by the ids in the URL (404 with the API's
    /// error when none is known); fails batches containing an id in `failing`; records
    /// every URL.
    #[derive(Default)]
    struct Api {
        entries: HashMap<u64, Value>,
        failing: Vec<u64>,
        seen: Mutex<Vec<String>>,
    }

    impl Api {
        fn with(ids: &[u64]) -> Self {
            Self {
                entries: ids.iter().map(|id| (*id, entry(*id))).collect(),
                ..Self::default()
            }
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Http for Api {
        fn send(&self, req: Request) -> Result<Response> {
            assert_eq!(req.purpose, "wowi-details");
            self.seen.lock().unwrap().push(req.url.clone());
            let ids: Vec<u64> = req
                .url
                .strip_prefix(&format!("{DETAILS_URL}/"))
                .and_then(|rest| rest.strip_suffix(".json"))
                .unwrap()
                .split(',')
                .map(|id| id.parse().unwrap())
                .collect();
            let reply = |status: u16, body: String| {
                Ok(Response::new(
                    status,
                    Vec::new(),
                    req.url.clone(),
                    Box::new(std::io::Cursor::new(body.into_bytes())),
                ))
            };
            if ids.iter().any(|id| self.failing.contains(id)) {
                return reply(503, String::new());
            }
            let found: Vec<&Value> = ids.iter().filter_map(|id| self.entries.get(id)).collect();
            if found.is_empty() {
                return reply(404, r#"{"ERROR":"No AddOn found."}"#.to_owned());
            }
            reply(200, serde_json::to_string(&found).unwrap())
        }
    }

    const MD5: &str = "683278e9d6e1cbc1514c3066d3d347b2";

    /// A `filedetails` entry as the API writes it: the id as a string, the date as a
    /// number, BBCode with CRLF.
    fn entry(uid: u64) -> Value {
        json!({
            "UID": uid.to_string(),
            "UICATID": "154",
            "UIVersion": "v12.1.7",
            "UIDate": 1_791_824_100_000_u64,
            "UIMD5": MD5,
            "UIFileName": "BugSack-v12.1.7.zip",
            "UIDownload": format!("https://cdn.wowinterface.com/downloads/getfile.php?id={uid}&d=1791824100&minion"),
            "UIPending": "0",
            "UIName": "BugSack",
            "UIAuthorName": "someone",
            "UIDescription": "",
            "UIChangeLog": "[SIZE=5]BugSack[/SIZE]\r\n[list]\r\n[*]Fixed [b]errors[/b]\r\n[/list]",
            "UIHitCount": "1",
            "UIHitCountMonthly": "1",
            "UIFavoriteTotal": "1",
        })
    }

    fn w(s: &str) -> Wanted {
        Wanted::stable(s.parse().unwrap())
    }

    #[test]
    fn details_decode_with_strings_numbers_and_gaps() {
        let body = json!([
            entry(5995),
            {"UID": 7032, "UIDate": "1700000000000", "UIVersion": null},
            {"UID": "x"},
            "not an object",
            {"UIName": "no id"},
        ]);
        let details = decode_details(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert_eq!(details.len(), 2, "entries without a usable UID are dropped");
        assert_eq!(details[0].uid, 5995);
        assert_eq!(details[0].md5, MD5);
        assert_eq!(details[0].date_ms, Some(1_791_824_100_000));
        assert_eq!(
            details[1],
            WowiDetails {
                uid: 7032,
                date_ms: Some(1_700_000_000_000),
                ..WowiDetails::default()
            }
        );
        for bad in [&b"{}"[..], b"null", b"<html>", b""] {
            assert_eq!(decode_details(bad).unwrap_err().code, ErrorCode::Network);
        }
    }

    #[test]
    fn an_entry_is_one_release_identified_by_its_content() {
        let details = decode_details(&serde_json::to_vec(&json!([entry(5995)])).unwrap())
            .unwrap()
            .remove(0);
        let release = release(&details);
        assert_eq!(release.release_id, format!("wowi:5995@{MD5}"));
        assert_eq!(release.version, "v12.1.7");
        assert_eq!(release.channel, Channel::Stable);
        assert_eq!(
            release.published_at.map(|t| t.to_string()),
            Some("2026-10-12T16:55:00Z".to_owned())
        );
        assert_eq!(
            release.changelog_url.as_deref(),
            Some("https://www.wowinterface.com/downloads/info5995")
        );
        // `[list]` on its own line leaves one blank line under the heading.
        assert_eq!(release.notes.as_deref(), Some("BugSack\n\n- Fixed errors"));
        assert_eq!(release.name_hint.as_deref(), Some("BugSack"));
        let [asset] = release.assets.as_slice() else {
            unreachable!("one asset: {:?}", release.assets)
        };
        assert_eq!(asset.filename, "BugSack-v12.1.7.zip");
        assert_eq!(asset.expected_hash, ExpectedHash::md5(MD5));
        assert!(asset.flavors.is_empty() && asset.name_flavors.is_empty());
        assert!(
            matches!(&asset.location, AssetLocation::Url(u) if u.starts_with("https://cdn.wowinterface.com/downloads/getfile.php?id=5995&"))
        );
    }

    #[test]
    fn missing_fields_degrade_without_failing() {
        let bare = WowiDetails {
            uid: 1,
            date_ms: Some(1_700_000_000_000),
            md5: "not-an-md5".into(),
            ..WowiDetails::default()
        };
        let release = release(&bare);
        assert_eq!(release.release_id, "wowi:1@d1700000000000", "no usable MD5");
        assert_eq!(release.version, "unknown");
        assert!(release.assets.is_empty(), "no UIDownload, no asset");
        assert_eq!((release.notes, release.name_hint), (None, None));
        // Without an MD5 the file installs unchecked, and says so.
        let reason = unpublished_md5(&release.release_id, None).unwrap();
        assert_eq!(reason, "WoWInterface published no usable MD5 for it");
        assert_eq!(
            unpublished_md5("gh:o/r@v1", None),
            None,
            "GitHub never publishes one"
        );
        // Neither an MD5 nor a date: the version names it, never `@d0`.
        let undated = WowiDetails {
            uid: 3,
            version: " 1.2 ".into(),
            download: "https://cdn.wowinterface.com/downloads/getfile.php?id=3".into(),
            ..WowiDetails::default()
        };
        let release = super::release(&undated);
        assert_eq!(release.release_id, "wowi:3@v1.2");
        assert_eq!(release.assets[0].expected_hash, None);
        let unnamed = WowiDetails {
            uid: 2,
            download: "https://cdn.wowinterface.com/downloads/getfile.php?id=2".into(),
            md5: MD5.to_uppercase(),
            ..WowiDetails::default()
        };
        let release = super::release(&unnamed);
        assert_eq!(release.release_id, format!("wowi:2@{MD5}"), "lowercased");
        assert_eq!(release.assets[0].filename, "wowi-2.zip");
        assert_eq!(release.assets[0].expected_hash, ExpectedHash::md5(MD5));
        let checked = release.assets[0].expected_hash.as_ref();
        assert_eq!(unpublished_md5(&release.release_id, checked), None);
    }

    #[test]
    fn ids_are_batched_once_each_in_first_seen_order() {
        let ids: Vec<u64> = (1..=120).collect();
        let api = Api::with(&ids);
        let mut wanted: Vec<Wanted> = ids
            .iter()
            .rev()
            .map(|id| w(&format!("wowi:{id}")))
            .collect();
        wanted.push(Wanted {
            version: Some("12.1.7".into()),
            ..w("wowi:120")
        });
        let results = WowiSource.fetch(&wanted, &api);
        assert_eq!(results.len(), 121);
        assert!(results.iter().all(Result::is_ok));
        let seen = api.seen();
        assert_eq!(seen.len(), 3, "120 ids, 50 per request");
        assert!(
            seen[0].starts_with(&format!("{DETAILS_URL}/120,119,")),
            "{}",
            seen[0]
        );
        assert!(seen[2].ends_with("/20,19,18,17,16,15,14,13,12,11,10,9,8,7,6,5,4,3,2,1.json"));
    }

    #[test]
    fn unknown_ids_are_not_found_and_failed_batches_fail_their_ids() {
        let mut api = Api::with(&[5995]);
        let results = WowiSource.fetch(&[w("wowi:5995"), w("wowi:404")], &api);
        let missing = results[1].as_ref().unwrap_err();
        assert_eq!(
            (missing.code, missing.exit().code()),
            (ErrorCode::NotFound, 3)
        );
        assert_eq!(
            missing.message,
            "wowi:404 does not exist or was removed from WoWInterface"
        );
        assert_eq!(
            missing.fix.argv(),
            Some(
                ["omaddons", "help", "install"]
                    .map(str::to_owned)
                    .as_slice()
            )
        );
        // Every id unknown: the API answers 404 with its own error.
        let none = WowiSource.fetch(&[w("wowi:404")], &api);
        assert_eq!(none[0].as_ref().unwrap_err().code, ErrorCode::NotFound);

        api.failing = vec![5995];
        let failed = WowiSource.fetch(&[w("wowi:5995"), w("wowi:404")], &api);
        for result in &failed {
            let error = result.as_ref().unwrap_err();
            assert_eq!(error.code, ErrorCode::HttpStatus, "{error:?}");
            assert!(error.retryable);
        }
    }

    #[test]
    fn a_404_without_the_api_error_is_not_a_missing_id() {
        struct Moved;
        impl Http for Moved {
            fn send(&self, req: Request) -> Result<Response> {
                Ok(Response::new(
                    404,
                    Vec::new(),
                    req.url,
                    Box::new(std::io::Cursor::new(b"<html>gone</html>".to_vec())),
                ))
            }
        }
        let results = WowiSource.fetch(&[w("wowi:5995")], &Moved);
        let error = results[0].as_ref().unwrap_err();
        assert_eq!(error.code, ErrorCode::HttpStatus, "{error:?}");
    }

    #[test]
    fn a_pin_matches_the_current_version_only() {
        let api = Api::with(&[5995]);
        let pinned = |v: &str| Wanted {
            version: Some(v.into()),
            ..w("wowi:5995")
        };
        let results = WowiSource.fetch(
            &[
                pinned("12.1.7"),
                pinned("v12.1.7"),
                pinned("12.1.6"),
                pinned(MD5),
            ],
            &api,
        );
        assert_eq!(api.seen().len(), 1, "one request for every wish");
        assert!(results[0].is_ok() && results[1].is_ok());
        for result in &results[2..] {
            let error = result.as_ref().unwrap_err();
            assert_eq!(error.code, ErrorCode::NotFound);
            assert!(
                error
                    .message
                    .contains("WoWInterface serves only its current file (v12.1.7)"),
                "{}",
                error.message
            );
            assert_eq!(
                error.fix.argv(),
                Some(
                    ["omaddons", "install", "wowi:5995"]
                        .map(str::to_owned)
                        .as_slice()
                )
            );
        }
    }
}
