//! Fixture replay for tests (DEC-7, debug builds only): `ReplayTransport` serves
//! recorded responses from `tests/fixtures/http/<scenario>/<host>/*.json` and never
//! opens a socket. It sits under `Guarded` like the real transport, so the allowlist,
//! offline mode and `net.log` behave exactly as in production.
//!
//! A fixture holds one normalized request and a list of responses, consumed in order
//! with the last repeating ("200 with an ETag, then 304"). Normalization, shared with
//! record mode, drops secret query parameters and sorts the rest.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{Http, Request, Response};
use crate::error::{Error, Result};

/// Query parameters that carry credentials or signatures; never stored or matched.
const SECRET_PARAMS: &[&str] = &[
    "token",
    "api-key",
    "api_key",
    "key",
    "access_token",
    "jwt",
    "sig",
    "signature",
    "se",
    "sp",
    "sv",
    "sr",
    "skoid",
    "sktid",
    "skt",
    "ske",
    "sks",
    "skv",
    "hmac",
];

/// A URL without secret query parameters, the rest sorted; no fragment.
pub fn normalize_url(url: &str) -> String {
    let url = url.split('#').next().unwrap_or(url);
    let Some((base, query)) = url.split_once('?') else {
        return url.to_owned();
    };
    let mut params: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let name = p.split('=').next().unwrap_or(p);
            let lower = name.to_ascii_lowercase();
            !SECRET_PARAMS.contains(&lower.as_str()) && !lower.starts_with("x-amz-")
        })
        .collect();
    params.sort_unstable();
    if params.is_empty() {
        base.to_owned()
    } else {
        format!("{base}?{}", params.join("&"))
    }
}

/// The key a request is stored and matched under: `METHOD normalized-url`.
pub fn request_key(method: &str, url: &str) -> String {
    format!("{method} {}", normalize_url(url))
}

/// One fixture file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fixture {
    pub schema_version: String,
    pub request: FixtureRequest,
    pub responses: Vec<FixtureResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureRequest {
    pub method: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureResponse {
    pub status: u16,
    /// Lowercase names.
    pub headers: Vec<(String, String)>,
    /// A sibling file holding the body; `None` for an empty body.
    pub body_file: Option<String>,
}

struct Entry {
    dir: PathBuf,
    responses: Vec<FixtureResponse>,
    next: Mutex<usize>,
}

/// Serves a scenario's fixtures.
pub struct ReplayTransport {
    entries: HashMap<String, Entry>,
    unmatched: Mutex<Vec<String>>,
}

impl ReplayTransport {
    /// Loads every `*.json` under the scenario directory; two fixtures with one key, or
    /// a file that does not parse, is an error.
    pub fn load(dir: &Path) -> Result<Self> {
        let mut entries = HashMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(at) = stack.pop() {
            let listing = std::fs::read_dir(&at).map_err(|e| load_error(&at, &e.to_string()))?;
            for item in listing.filter_map(std::result::Result::ok) {
                let path = item.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| load_error(&path, &e.to_string()))?;
                let fixture: Fixture =
                    serde_json::from_str(&text).map_err(|e| load_error(&path, &e.to_string()))?;
                if !fixture.schema_version.starts_with("1.") {
                    return Err(load_error(&path, "unsupported schema_version"));
                }
                if fixture.responses.is_empty() {
                    return Err(load_error(&path, "no responses"));
                }
                let key = request_key(&fixture.request.method, &fixture.request.url);
                let entry = Entry {
                    dir: path.parent().unwrap_or(dir).to_path_buf(),
                    responses: fixture.responses,
                    next: Mutex::new(0),
                };
                if entries.insert(key.clone(), entry).is_some() {
                    return Err(load_error(&path, &format!("a second fixture for {key}")));
                }
            }
        }
        Ok(Self {
            entries,
            unmatched: Mutex::new(Vec::new()),
        })
    }

    /// The keys requested that had no fixture.
    pub fn unmatched(&self) -> Vec<String> {
        self.unmatched.lock().map(|u| u.clone()).unwrap_or_default()
    }
}

fn load_error(path: &Path, why: &str) -> Error {
    Error::internal(format!("replay: cannot load {}: {why}", path.display()))
}

impl Http for ReplayTransport {
    fn send(&self, req: Request) -> Result<Response> {
        let key = request_key(req.method.as_str(), &req.url);
        let Some(entry) = self.entries.get(&key) else {
            if let Ok(mut unmatched) = self.unmatched.lock() {
                unmatched.push(key.clone());
            }
            // Through `Error::internal`, so the command boundary gives it its fix.
            return Err(Error::internal(format!("replay: no fixture for {key}")));
        };
        let index = {
            let mut next = entry.next.lock().unwrap_or_else(|p| p.into_inner());
            let index = (*next).min(entry.responses.len().saturating_sub(1));
            *next += 1;
            index
        };
        let Some(response) = entry.responses.get(index) else {
            return Err(Error::internal(format!("replay: {key} has no responses")));
        };
        let body: Box<dyn std::io::Read + Send> = match &response.body_file {
            Some(name) => {
                let path = entry.dir.join(name);
                let bytes = std::fs::read(&path).map_err(|e| load_error(&path, &e.to_string()))?;
                Box::new(std::io::Cursor::new(bytes))
            }
            None => Box::new(std::io::empty()),
        };
        Ok(Response::new(
            response.status,
            response.headers.clone(),
            req.url,
            body,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn secrets_are_stripped_and_the_rest_sorted() {
        let url =
            "https://x.org/a?z=1&token=t&a=2&X-Amz-Signature=s&X-Amz-Date=d&sig=q&keep=k#frag";
        assert_eq!(normalize_url(url), "https://x.org/a?a=2&keep=k&z=1");
        assert_eq!(
            normalize_url(&normalize_url(url)),
            normalize_url(url),
            "idempotent"
        );
        assert_eq!(normalize_url("https://x.org/a?token=t"), "https://x.org/a");
        assert_eq!(normalize_url("https://x.org/a"), "https://x.org/a");
        for secret in SECRET_PARAMS {
            assert_eq!(
                normalize_url(&format!("https://x.org/?{secret}=v&b=1")),
                "https://x.org/?b=1",
                "{secret}"
            );
        }
        assert_eq!(
            request_key("GET", "https://x.org/?b=1&a=2"),
            "GET https://x.org/?a=2&b=1"
        );
    }

    /// (status, headers, body)
    type Canned<'a> = (u16, &'a [(&'a str, &'a str)], Option<&'a str>);

    fn write_fixture(dir: &Path, name: &str, url: &str, responses: &[Canned<'_>]) {
        let fixture = Fixture {
            schema_version: "1.0".into(),
            request: FixtureRequest {
                method: "GET".into(),
                url: url.into(),
            },
            responses: responses
                .iter()
                .enumerate()
                .map(|(i, (status, headers, body))| {
                    let body_file = body.map(|b| {
                        let file = format!("{name}-{i}.body");
                        std::fs::write(dir.join(&file), b).unwrap();
                        file
                    });
                    FixtureResponse {
                        status: *status,
                        headers: headers
                            .iter()
                            .map(|(k, v)| ((*k).into(), (*v).into()))
                            .collect(),
                        body_file,
                    }
                })
                .collect(),
        };
        std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_string(&fixture).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn responses_play_in_order_and_the_last_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("github.com");
        std::fs::create_dir_all(&host).unwrap();
        write_fixture(
            &host,
            "feed",
            "https://github.com/a/b/releases.atom",
            &[
                (200, &[("etag", "\"v1\"")], Some("<feed/>")),
                (304, &[], None),
            ],
        );
        let replay = ReplayTransport::load(dir.path()).unwrap();
        let get = || {
            replay
                .send(Request::get(
                    "https://github.com/a/b/releases.atom?token=zzz",
                    "atom",
                ))
                .unwrap()
        };
        let first = get();
        assert_eq!((first.status, first.header("etag")), (200, Some("\"v1\"")));
        assert_eq!(first.bytes().unwrap(), b"<feed/>");
        assert_eq!(get().status, 304);
        assert_eq!(get().status, 304, "the last response repeats");

        let err = replay
            .send(Request::get("https://github.com/other", "x"))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(
            err.message
                .contains("no fixture for GET https://github.com/other")
        );
        assert_eq!(replay.unmatched(), ["GET https://github.com/other"]);
    }

    #[test]
    fn duplicate_keys_and_bad_files_fail_to_load() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture(
            dir.path(),
            "a",
            "https://x.org/f?b=1&a=2",
            &[(200, &[], None)],
        );
        write_fixture(
            dir.path(),
            "b",
            "https://x.org/f?a=2&b=1&token=q",
            &[(200, &[], None)],
        );
        assert!(
            ReplayTransport::load(dir.path()).is_err(),
            "same normalized key"
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.json"), "{ nope").unwrap();
        assert!(ReplayTransport::load(dir.path()).is_err());
    }
}
