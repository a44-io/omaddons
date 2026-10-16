//! Record mode for fixtures (DEC-7, debug builds only, `OMADDONS_RECORD=1`):
//! `RecordTransport` forwards each request to the real transport and writes the
//! response as a fixture under the scenario directory, through fsx under the debug-only
//! record-fixtures scope. Request headers are never stored (so `Authorization` cannot
//! leak), `set-cookie` is dropped, and secret query parameters are stripped from the
//! stored URL and from `location` headers.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::replay::{Fixture, FixtureRequest, FixtureResponse, normalize_url, request_key};
use super::{Http, Request, Response};
use crate::error::Result;
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::state::paths::Paths;

/// Records a scenario.
pub struct RecordTransport {
    inner: Box<dyn Http>,
    dir: PathBuf,
    scopes: ScopeTable,
}

impl RecordTransport {
    /// Fails in release builds (the record scope does not exist there).
    pub fn new(dir: &Path, inner: Box<dyn Http>, paths: &Paths) -> Result<Self> {
        let mut scopes = ScopeTable::new(paths, &[], None)?;
        std::fs::metadata(dir).map_err(|e| {
            crate::error::Error::internal(format!("record: {}: {e}", dir.display()))
        })?;
        scopes.add_record_fixtures(dir)?;
        Ok(Self {
            inner,
            dir: dir.to_path_buf(),
            scopes,
        })
    }

    fn write(
        &self,
        req: &Request,
        status: u16,
        headers: Vec<(String, String)>,
        body: &[u8],
    ) -> Result<()> {
        let url = normalize_url(&req.url);
        let key = request_key(req.method.as_str(), &req.url);
        let (host, path) = url
            .split_once("://")
            .map(|(_, rest)| rest.split_once('/').unwrap_or((rest, "")))
            .unwrap_or(("unknown", ""));
        let hash: String = Sha256::digest(key.as_bytes())
            .iter()
            .take(4)
            .map(|b| format!("{b:02x}"))
            .collect();
        let slug = slug(path.split('?').next().unwrap_or(""));
        let stem = format!("{}-{slug}-{hash}", req.method.as_str().to_ascii_lowercase());
        let host_dir = self.dir.join(host.to_ascii_lowercase());
        let body_file = (!body.is_empty()).then(|| format!("{stem}.body"));
        let fixture = Fixture {
            schema_version: "1.0".into(),
            request: FixtureRequest {
                method: req.method.as_str().into(),
                url,
            },
            responses: vec![FixtureResponse {
                status,
                headers,
                body_file: body_file.clone(),
            }],
        };
        let json = serde_json::to_vec_pretty(&fixture)
            .map_err(|e| crate::error::Error::internal(format!("record: {e}")))?;
        let mut s = WriteSession::untracked(Writer::Derived, &self.scopes, "http-fixtures");
        ensure_dir_all(&mut s, &host_dir)?;
        let write = |s: &mut WriteSession<'_>, name: &str, bytes: &[u8]| -> Result<()> {
            let op = Op::WriteFile {
                content: Content::Bytes(bytes),
                mode: Some(0o644),
            };
            mutate(s, &host_dir.join(name), op)?;
            Ok(())
        };
        if let Some(file) = &body_file {
            write(&mut s, file, body)?;
        }
        write(&mut s, &format!("{stem}.json"), &json)
    }
}

/// A file-name-safe slug of a URL path; `root` for `/`.
fn slug(path: &str) -> String {
    let slug: String = path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(80)
        .collect();
    if slug.is_empty() {
        "root".to_owned()
    } else {
        slug
    }
}

impl Http for RecordTransport {
    fn send(&self, req: Request) -> Result<Response> {
        let resp = self.inner.send(req.clone())?;
        let status = resp.status;
        let final_url = resp.final_url.clone();
        // The fixture gets stripped headers; the live run keeps the originals, because a
        // signed redirect without its signature fails upstream.
        let live_headers = resp.headers.clone();
        let stored: Vec<(String, String)> = resp
            .headers
            .iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("set-cookie"))
            .map(|(k, v)| {
                let k = k.to_ascii_lowercase();
                let v = if k == "location" {
                    normalize_url(v)
                } else {
                    v.clone()
                };
                (k, v)
            })
            .collect();
        let body = resp.bytes()?;
        self.write(&req, status, stored, &body)?;
        Ok(Response::new(
            status,
            live_headers,
            final_url,
            Box::new(Cursor::new(body)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Upstream;

    impl Http for Upstream {
        fn send(&self, req: Request) -> Result<Response> {
            Ok(Response::new(
                302,
                vec![
                    (
                        "Location".into(),
                        "https://release-assets.githubusercontent.com/x?X-Amz-Signature=s&id=1"
                            .into(),
                    ),
                    ("Set-Cookie".into(), "session=secret".into()),
                    ("Content-Type".into(), "text/html".into()),
                ],
                req.url,
                Box::new(Cursor::new(b"moved".to_vec())),
            ))
        }
    }

    // Record mode exists only in debug builds (DEC-7).
    #[cfg(debug_assertions)]
    #[test]
    fn record_writes_stripped_fixtures_that_replay() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        let scenario = dir.path().join("scenario");
        std::fs::create_dir_all(&scenario).unwrap();
        let record = RecordTransport::new(&scenario, Box::new(Upstream), &paths).unwrap();
        let req = Request::get(
            "https://github.com/a/b/releases/latest?token=t&x=1",
            "latest",
        )
        .header("Authorization", "Bearer s3cret");
        let resp = record.send(req).unwrap();
        assert_eq!(resp.status, 302);
        assert_eq!(
            resp.location(),
            Some("https://release-assets.githubusercontent.com/x?X-Amz-Signature=s&id=1"),
            "the live redirect keeps its signature; only the fixture is stripped"
        );
        assert_eq!(resp.bytes().unwrap(), b"moved");

        let host = scenario.join("github.com");
        let files: Vec<String> = std::fs::read_dir(&host)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 2, "{files:?}");
        let json = files.iter().find(|f| f.ends_with(".json")).unwrap();
        assert!(json.starts_with("get-a-b-releases-latest-"), "{json}");
        let text = std::fs::read_to_string(host.join(json)).unwrap();
        assert!(
            !text.contains("token") && !text.contains("s3cret") && !text.contains("Bearer"),
            "{text}"
        );
        assert!(
            !text.contains("X-Amz-Signature") && !text.to_ascii_lowercase().contains("set-cookie"),
            "{text}"
        );
        assert!(text.contains("\"location\""), "{text}");

        let replay = super::super::replay::ReplayTransport::load(&scenario).unwrap();
        let again = replay
            .send(Request::get(
                "https://github.com/a/b/releases/latest?x=1&token=other",
                "latest",
            ))
            .unwrap();
        assert_eq!(
            (again.status, again.location()),
            (
                302,
                Some("https://release-assets.githubusercontent.com/x?id=1")
            )
        );
        assert_eq!(again.bytes().unwrap(), b"moved");
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("a/b/releases/latest"), "a-b-releases-latest");
        assert_eq!(slug(""), "root");
        assert_eq!(slug("x".repeat(200).as_str()).len(), 80);
    }
}
