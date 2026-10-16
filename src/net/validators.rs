//! `Validating`: HTTP validators (ETag / Last-Modified) for requests that opt in
//! (`Request.validators`: feeds and file lists, never downloads). A stored response is
//! revalidated with `If-None-Match` / `If-Modified-Since`, and a 304 is answered from
//! `<cache>/omaddons/http/<sha256 of the URL>.{json,body}` with
//! [`CacheState::Revalidated`]. The cache is derived data written through fsx; a missing
//! or corrupt entry only means "no validators".

use std::io::Cursor;
use std::path::PathBuf;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{CacheState, Http, Method, Request, Response};
use crate::error::Result;
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::state::paths::Paths;
use crate::state::store::{self, SchemaVersion, Versioned};

/// What is stored beside a cached body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CachedMeta {
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    #[schemars(with = "String")]
    pub stored_at: Timestamp,
}

impl Versioned for CachedMeta {
    const KIND: &'static str = "http_validators";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// Revalidates opted-in GETs against the cache.
pub struct Validating {
    inner: Box<dyn Http>,
    paths: Paths,
    dir: PathBuf,
}

impl Validating {
    pub fn new(inner: Box<dyn Http>, paths: &Paths) -> Self {
        Self {
            inner,
            dir: paths.http_cache_dir(),
            paths: paths.clone(),
        }
    }

    fn entry(&self, url: &str) -> (PathBuf, PathBuf) {
        let key: String = Sha256::digest(url.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        (
            self.dir.join(format!("{key}.json")),
            self.dir.join(format!("{key}.body")),
        )
    }

    /// A cached entry, when its metadata parses and names this URL and its body exists.
    fn load(&self, url: &str) -> Option<(CachedMeta, Vec<u8>)> {
        let (meta_path, body_path) = self.entry(url);
        let text = std::fs::read_to_string(meta_path).ok()?;
        let meta: CachedMeta = serde_json::from_str(&text).ok()?;
        if meta.url != url || (meta.etag.is_none() && meta.last_modified.is_none()) {
            return None;
        }
        let body = std::fs::read(body_path).ok()?;
        Some((meta, body))
    }

    /// Stores an entry through fsx; a failure leaves the cache as it was.
    fn store(&self, meta: &CachedMeta, body: &[u8]) {
        let (meta_path, body_path) = self.entry(&meta.url);
        let Ok(json) = store::encode(meta) else {
            return;
        };
        let Ok(scopes) = ScopeTable::new(&self.paths, &[], None) else {
            return;
        };
        let mut s = WriteSession::untracked(Writer::Derived, &scopes, "http-cache");
        if ensure_dir_all(&mut s, &self.dir).is_err() {
            return;
        }
        let write = |s: &mut WriteSession<'_>, path: &PathBuf, bytes: &[u8]| {
            let op = Op::WriteFile {
                content: Content::Bytes(bytes),
                mode: Some(0o644),
            };
            mutate(s, path, op).is_ok()
        };
        // The body first: metadata without a body is never read.
        if write(&mut s, &body_path, body) {
            write(&mut s, &meta_path, &json);
        }
    }
}

impl Http for Validating {
    fn send(&self, req: Request) -> Result<Response> {
        if !req.validators || req.method != Method::Get {
            return self.inner.send(req);
        }
        let url = req.url.clone();
        let cached = self.load(&url);
        let mut conditional = req;
        if let Some((meta, _)) = &cached {
            if let Some(etag) = &meta.etag {
                conditional = conditional.header("If-None-Match", etag.clone());
            }
            if let Some(modified) = &meta.last_modified {
                conditional = conditional.header("If-Modified-Since", modified.clone());
            }
        }
        let resp = self.inner.send(conditional)?;
        match (resp.status, cached) {
            (304, Some((meta, body))) => {
                let final_url = resp.final_url.clone();
                // The stored headers, refreshed by the 304's own (as HTTP caches do), so
                // `X-RateLimit-*` are the server's current numbers, not the stored 200's.
                let headers = merge_headers(meta.headers, &resp.headers);
                drop(resp);
                let mut revalidated =
                    Response::new(meta.status, headers, final_url, Box::new(Cursor::new(body)));
                revalidated.cache = CacheState::Revalidated;
                Ok(revalidated)
            }
            (200, _) if resp.header("etag").is_some() || resp.header("last-modified").is_some() => {
                let meta = CachedMeta {
                    url,
                    etag: resp.header("etag").map(str::to_owned),
                    last_modified: resp.header("last-modified").map(str::to_owned),
                    status: resp.status,
                    headers: resp.headers.clone(),
                    stored_at: Timestamp::now(),
                };
                let final_url = resp.final_url.clone();
                let body = resp.bytes()?;
                self.store(&meta, &body);
                Ok(Response::new(
                    meta.status,
                    meta.headers,
                    final_url,
                    Box::new(Cursor::new(body)),
                ))
            }
            _ => Ok(resp),
        }
    }
}

/// `stored` with every field `fresh` carries replaced by `fresh`'s (names compared
/// ignoring ASCII case), and `fresh`'s other fields added.
fn merge_headers(
    stored: Vec<(String, String)>,
    fresh: &[(String, String)],
) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = stored
        .into_iter()
        .filter(|(name, _)| !fresh.iter().any(|(f, _)| f.eq_ignore_ascii_case(name)))
        .collect();
    merged.extend(fresh.iter().cloned());
    merged
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Answers each request from a closure and records the headers it saw.
    /// The headers of each request seen.
    type Seen = Arc<Mutex<Vec<Vec<(String, String)>>>>;
    /// (status, headers, body) for the nth request.
    type Answer = (u16, Vec<(&'static str, &'static str)>, &'static str);

    struct Fake {
        seen: Seen,
        answer: Box<dyn Fn(usize) -> Answer + Send + Sync>,
    }

    impl Http for Fake {
        fn send(&self, req: Request) -> Result<Response> {
            let mut seen = self.seen.lock().unwrap();
            seen.push(req.headers.clone());
            let (status, headers, body) = (self.answer)(seen.len());
            Ok(Response::new(
                status,
                headers
                    .into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
                req.url,
                Box::new(Cursor::new(body.as_bytes().to_vec())),
            ))
        }
    }

    fn setup(
        answer: impl Fn(usize) -> Answer + Send + Sync + 'static,
    ) -> (tempfile::TempDir, Validating, Seen) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        std::fs::create_dir_all(dir.path().join("k")).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let fake = Fake {
            seen: Arc::clone(&seen),
            answer: Box::new(answer),
        };
        let validating = Validating::new(Box::new(fake), &paths);
        (dir, validating, seen)
    }

    fn feed() -> Request {
        let mut req = Request::get("https://github.com/x/y/releases.atom", "atom");
        req.validators = true;
        req
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn etags_turn_304s_into_cached_bodies() {
        let (dir, validating, seen) = setup(|n| match n {
            1 => (200, vec![("ETag", "\"v1\"")], "<feed/>"),
            _ => (304, vec![], ""),
        });
        let first = validating.send(feed()).unwrap();
        assert_eq!(first.cache, CacheState::Network);
        assert_eq!(first.bytes().unwrap(), b"<feed/>");
        let second = validating.send(feed()).unwrap();
        assert_eq!(
            (second.status, second.cache),
            (200, CacheState::Revalidated)
        );
        assert_eq!(second.bytes().unwrap(), b"<feed/>");
        let seen = seen.lock().unwrap();
        assert_eq!(header(&seen[0], "if-none-match"), None);
        assert_eq!(header(&seen[1], "if-none-match"), Some("\"v1\""));
        let stored: Vec<_> = std::fs::read_dir(dir.path().join("k/omaddons/http"))
            .unwrap()
            .collect();
        assert_eq!(stored.len(), 2, "metadata and body");
    }

    #[test]
    fn a_304_refreshes_the_stored_headers() {
        let (_dir, validating, _) = setup(|n| match n {
            1 => (
                200,
                vec![
                    ("ETag", "\"v1\""),
                    ("X-RateLimit-Remaining", "50"),
                    ("Content-Type", "application/json"),
                ],
                "[]",
            ),
            _ => (304, vec![("x-ratelimit-remaining", "3")], ""),
        });
        validating.send(feed()).unwrap().bytes().unwrap();
        let second = validating.send(feed()).unwrap();
        assert_eq!(second.cache, CacheState::Revalidated);
        assert_eq!(header(&second.headers, "x-ratelimit-remaining"), Some("3"));
        assert_eq!(
            header(&second.headers, "content-type"),
            Some("application/json"),
            "fields the 304 lacks are kept"
        );
        assert_eq!(
            second
                .headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("x-ratelimit-remaining"))
                .count(),
            1
        );
    }

    #[test]
    fn last_modified_alone_works() {
        let (_dir, validating, seen) = setup(|n| match n {
            1 => (
                200,
                vec![("Last-Modified", "Wed, 14 Oct 2026 12:00:00 GMT")],
                "a",
            ),
            _ => (304, vec![], ""),
        });
        validating.send(feed()).unwrap().bytes().unwrap();
        assert_eq!(
            validating.send(feed()).unwrap().cache,
            CacheState::Revalidated
        );
        assert_eq!(
            header(&seen.lock().unwrap()[1], "if-modified-since"),
            Some("Wed, 14 Oct 2026 12:00:00 GMT")
        );
    }

    #[test]
    fn corrupt_entries_and_downloads_skip_validators() {
        let (dir, validating, seen) = setup(|_| (200, vec![("ETag", "\"v1\"")], "x"));
        validating.send(feed()).unwrap().bytes().unwrap();
        for entry in std::fs::read_dir(dir.path().join("k/omaddons/http")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "json") {
                std::fs::write(&path, "{ not json").unwrap();
            }
        }
        validating.send(feed()).unwrap().bytes().unwrap();
        let mut download = Request::get("https://github.com/x/y/archive.zip", "asset").download();
        download.validators = false;
        validating.send(download).unwrap().bytes().unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            header(&seen[1], "if-none-match"),
            None,
            "a corrupt entry is no entry"
        );
        assert_eq!(
            header(&seen[2], "if-none-match"),
            None,
            "downloads never revalidate"
        );
    }
}
