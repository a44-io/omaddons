//! The two layers every request passes through, outermost first:
//!
//! - `Redirecting` follows 301/302/303/307/308 up to `MAX_REDIRECTS` hops, sending
//!   each hop back through the guard; credentials never follow a hop to another host.
//! - `Guarded` refuses offline runs, non-https URLs and hosts off the allowlist before
//!   anything is sent, caps body sizes, and writes one `net.log` line per request that
//!   reaches the transport (on error, or when the body is read to the end or dropped).

use std::io::{self, Read};
use std::time::Instant;

use super::allowlist::Allowlist;
use super::log::{NetLog, NetLogEntry, Via, now};
use super::{Http, Method, RedirectMode, Request, Response};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::politeness::MAX_REDIRECTS;

/// Headers that carry credentials; dropped when a redirect changes host.
const CREDENTIAL_HEADERS: &[&str] = &["authorization", "x-api-key"];

/// A URL the guard accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) host: String,
    pub(crate) path: String,
}

/// Parses `url` and accepts only `https` on the default port, without userinfo.
pub(crate) fn parse_target(url: &str, hop: usize) -> Result<Target> {
    let blocked = |why: &str| host_blocked(url, hop, why);
    let uri: ureq::http::Uri = url.parse().map_err(|_| blocked("it is not a valid URL"))?;
    if uri.scheme_str() != Some("https") {
        return Err(blocked("only https is allowed"));
    }
    let authority = uri.authority().ok_or_else(|| blocked("it has no host"))?;
    if authority.as_str().contains('@') {
        return Err(blocked("it carries credentials in the URL"));
    }
    if authority.port_u16().is_some_and(|port| port != 443) {
        return Err(blocked("only the default https port is allowed"));
    }
    Ok(Target {
        host: authority.host().to_ascii_lowercase(),
        path: uri.path().to_owned(),
    })
}

fn host_blocked(url: &str, hop: usize, why: &str) -> Error {
    let which = if hop == 0 {
        "request".to_owned()
    } else {
        format!("redirect hop {hop}")
    };
    Error::new(
        ErrorCode::HostBlocked,
        format!("{which} to {} refused: {why}", super::redact(url)),
        Suggestion::text("download the file yourself and install the local path"),
    )
    .details(serde_json::json!({ "url": super::redact(url), "hop": hop }))
}

/// Enforces the allowlist, offline mode and the size cap, and logs.
pub struct Guarded {
    inner: Box<dyn Http>,
    allowlist: Allowlist,
    log: NetLog,
    offline: bool,
    max_bytes: u64,
}

impl Guarded {
    pub fn new(
        inner: Box<dyn Http>,
        allowlist: Allowlist,
        log: NetLog,
        offline: bool,
        max_bytes: u64,
    ) -> Self {
        Self {
            inner,
            allowlist,
            log,
            offline,
            max_bytes,
        }
    }
}

impl Http for Guarded {
    fn send(&self, req: Request) -> Result<Response> {
        if self.offline {
            return Err(Error::new(
                ErrorCode::Offline,
                format!("offline: not requesting {}", super::redact(&req.url)),
                Suggestion::text("run without --offline and with OMADDONS_OFFLINE unset"),
            ));
        }
        let target = parse_target(&req.url, req.hop)?;
        if !self.allowlist.permits(&target.host) {
            return Err(host_blocked(
                &req.url,
                req.hop,
                &format!("{} is not on the allowlist", target.host),
            ));
        }
        let pending = PendingLine {
            log: self.log.clone(),
            entry: NetLogEntry {
                time: now(),
                method: req.method.as_str().to_owned(),
                host: target.host,
                path: target.path,
                status: None,
                bytes: None,
                duration_ms: 0,
                purpose: req.purpose.to_owned(),
                via: Via::Http,
                error: None,
            },
            started: Instant::now(),
        };
        let mut response = match self.inner.send(req) {
            Ok(response) => response,
            Err(err) => {
                pending.write(None, None, Some(err.code))?;
                return Err(err);
            }
        };
        let announced = response
            .header("content-length")
            .and_then(|len| len.trim().parse::<u64>().ok());
        if announced.is_some_and(|len| len > self.max_bytes) {
            let err = too_large(self.max_bytes);
            pending.write(Some(response.status), Some(0), Some(err.code))?;
            return Err(err);
        }
        let body = response.take_body();
        response.set_body(Box::new(LoggingReader {
            inner: body,
            bytes: 0,
            max_bytes: self.max_bytes,
            status: response.status,
            pending: Some(pending),
        }));
        Ok(response)
    }
}

/// A `net.log` line waiting for the body to finish.
struct PendingLine {
    log: NetLog,
    entry: NetLogEntry,
    started: Instant,
}

impl PendingLine {
    fn write(
        mut self,
        status: Option<u16>,
        bytes: Option<u64>,
        error: Option<ErrorCode>,
    ) -> Result<()> {
        self.entry.status = status;
        self.entry.bytes = bytes;
        self.entry.error = error.map(|code| code.wire().to_owned());
        self.entry.duration_ms =
            u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.log.append(&self.entry)
    }
}

/// Counts body bytes, enforces the size cap, and writes the request's `net.log` line
/// at the end of the body, on a read error, or when dropped unread.
struct LoggingReader {
    inner: Box<dyn Read + Send>,
    bytes: u64,
    max_bytes: u64,
    status: u16,
    pending: Option<PendingLine>,
}

impl LoggingReader {
    fn finish(&mut self, error: Option<ErrorCode>) -> io::Result<()> {
        match self.pending.take() {
            // The crate error rides inside the io::Error; `body_error` unwraps it.
            Some(line) => line
                .write(Some(self.status), Some(self.bytes), error)
                .map_err(io::Error::other),
            None => Ok(()),
        }
    }
}

impl Read for LoggingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = match self.inner.read(buf) {
            Ok(n) => n,
            Err(err) => {
                self.finish(Some(ErrorCode::Network))?;
                return Err(err);
            }
        };
        self.bytes = self.bytes.saturating_add(n as u64);
        if self.bytes > self.max_bytes {
            self.finish(Some(ErrorCode::ArchiveTooLarge))?;
            return Err(io::Error::other(SizeCapExceeded {
                max_bytes: self.max_bytes,
            }));
        }
        if n == 0 {
            self.finish(None)?;
        }
        Ok(n)
    }
}

impl Drop for LoggingReader {
    fn drop(&mut self) {
        // Best effort: a body dropped unread still gets its line.
        drop(self.finish(None));
    }
}

/// The streaming size cap tripped.
#[derive(Debug, thiserror::Error)]
#[error("the response is larger than the {max_bytes}-byte download limit")]
pub(crate) struct SizeCapExceeded {
    pub(crate) max_bytes: u64,
}

pub(crate) fn too_large(max_bytes: u64) -> Error {
    Error::new(
        ErrorCode::ArchiveTooLarge,
        format!("the response is larger than the {max_bytes}-byte download limit"),
        Suggestion::text("raise download_max_bytes under [limits] in config.toml"),
    )
    .details(serde_json::json!({ "max_bytes": max_bytes }))
}

/// Follows redirects hop by hop through the guard.
pub struct Redirecting {
    inner: Guarded,
}

impl Redirecting {
    pub fn new(inner: Guarded) -> Self {
        Self { inner }
    }
}

impl Http for Redirecting {
    fn send(&self, req: Request) -> Result<Response> {
        if req.redirect == RedirectMode::Manual {
            return self.inner.send(req);
        }
        let origin_host = parse_target(&req.url, 0)?.host;
        let mut req = req;
        loop {
            let response = self.inner.send(req.clone())?;
            if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                return Ok(response);
            }
            let Some(location) = response.location().map(str::to_owned) else {
                return Ok(response);
            };
            if req.hop >= MAX_REDIRECTS {
                return Err(Error::new(
                    ErrorCode::Network,
                    format!(
                        "more than {MAX_REDIRECTS} redirects from {}",
                        super::redact(&req.url)
                    ),
                    Suggestion::text("retry later; if it persists, the source moved"),
                )
                .retryable(false));
            }
            let next = resolve(&req.url, &location);
            if response.status == 303 {
                req.method = Method::Get;
            }
            // Dropping the body writes this hop's net.log line.
            drop(response);
            let next_host = parse_target(&next, req.hop + 1)
                .map(|t| t.host)
                .unwrap_or_default();
            if next_host != origin_host {
                req.headers.retain(|(name, _)| {
                    !CREDENTIAL_HEADERS
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(name))
                });
            }
            req.url = next;
            req.hop += 1;
        }
    }
}

/// Resolves a `Location` header against the URL that returned it.
pub(crate) fn resolve(base: &str, location: &str) -> String {
    if location.contains("://") {
        return location.to_owned();
    }
    let Some((scheme, rest)) = base.split_once("://") else {
        return location.to_owned();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    if let Some(network_path) = location.strip_prefix("//") {
        return format!("{scheme}://{network_path}");
    }
    if location.starts_with('/') {
        return format!("{scheme}://{authority}{location}");
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let dir = path
        .rfind('/')
        .map_or("/", |i| path.get(..=i).unwrap_or("/"));
    format!("{scheme}://{authority}{dir}{location}")
}
