//! The network layer: the `Http` trait with real, replay and record implementations,
//! the per-hop host allowlist, `net.log`, validators, retry and backoff.
//!
//! Every request goes through one stack (D9, D13, G5), built by [`stack`], outermost
//! first: `Limited<Retrying<Validating<Redirecting<Guarded<Decoding<transport>>>>>>`.
//! The guard checks the allowlist on every hop, honors `--offline`, caps body sizes
//! (decoded bytes, per request kind) and logs every request that reaches the transport;
//! `Decoding` asks API requests for gzip and decodes it (M2-D14).

pub mod allowlist;
mod client;
pub mod decode;
mod guard;
pub mod limit;
pub mod log;
pub mod pool;
pub mod record;
pub mod replay;
pub mod retry;
#[cfg(test)]
mod tests;
pub mod validators;

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

pub use allowlist::{Allowlist, GitHosts};
pub use client::UreqTransport;
pub use guard::{Guarded, Redirecting, download_too_large, offline_refusal};
pub use limit::Limited;
pub use log::{GitLogEntry, NetLog, NetLogEntry, Via};
pub use pool::run_bounded;
pub use retry::Retrying;
pub use validators::Validating;

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::state::paths::Paths;

/// Sends one request. Implemented by the transports and by every layer around them.
pub trait Http: Send + Sync {
    fn send(&self, req: Request) -> Result<Response>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    /// WebDAV's `PROPFIND` with `Depth: 1`, asking only for each child's `version-name`
    /// (its last-changed revision): the WowAce source's tag list. Its body is always
    /// [`PROPFIND_VERSION_NAMES`], so it is as idempotent as a GET.
    Propfind,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Propfind => "PROPFIND",
        }
    }
}

/// The one body a `PROPFIND` is sent with.
pub const PROPFIND_VERSION_NAMES: &str = r#"<?xml version="1.0" encoding="utf-8"?><D:propfind xmlns:D="DAV:"><D:prop><D:version-name/></D:prop></D:propfind>"#;

/// Which whole-call timeout applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutKind {
    /// `API_READ_TIMEOUT`, for JSON and feeds.
    Api,
    /// `DOWNLOAD_TIMEOUT`, for archives.
    Download,
}

/// Whether redirects are followed or returned to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectMode {
    Follow,
    /// The 3xx comes back as is, e.g. to read a tag from `releases/latest`'s `Location`.
    Manual,
}

/// One HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// A short label for `net.log`, e.g. `release-json` or `asset`.
    pub purpose: &'static str,
    pub timeout: TimeoutKind,
    pub redirect: RedirectMode,
    /// Whether to use HTTP validators (net-resilience).
    pub validators: bool,
    /// The redirect hop this request is (0 for the first).
    pub(crate) hop: usize,
}

impl Request {
    /// A GET with the API timeout, following redirects.
    pub fn get(url: impl Into<String>, purpose: &'static str) -> Self {
        Self {
            method: Method::Get,
            url: url.into(),
            headers: Vec::new(),
            purpose,
            timeout: TimeoutKind::Api,
            redirect: RedirectMode::Follow,
            validators: false,
            hop: 0,
        }
    }

    /// A `PROPFIND` of the collection at `url`, one level deep, for its children's
    /// `version-name`: the only way a request with a method other than GET or HEAD is
    /// made.
    pub fn propfind_version_names(url: impl Into<String>, purpose: &'static str) -> Self {
        Self {
            method: Method::Propfind,
            ..Self::get(url, purpose)
        }
        .header("Depth", "1")
        .header("Content-Type", "text/xml; charset=utf-8")
    }

    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Uses the download timeout.
    #[must_use]
    pub fn download(mut self) -> Self {
        self.timeout = TimeoutKind::Download;
        self
    }

    /// Returns redirects to the caller instead of following them.
    #[must_use]
    pub fn manual_redirects(mut self) -> Self {
        self.redirect = RedirectMode::Manual;
        self
    }
}

/// Where a response's body came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheState {
    /// Over the network.
    #[default]
    Network,
    /// The validator cache, after the server answered 304 Not Modified.
    Revalidated,
}

/// One HTTP response. The body streams; read it with [`Response::bytes`] or
/// [`Response::into_reader`]. Its `net.log` line is written when the body ends or the
/// response is dropped.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// The URL this response came from.
    pub final_url: String,
    pub cache: CacheState,
    body: Box<dyn Read + Send>,
    /// Compressed bytes read off the wire, when [`decode::Decoding`] decodes the body
    /// (`net.log`'s `wire_bytes`).
    pub(crate) wire_bytes: Option<Arc<AtomicU64>>,
    /// How many wire requests this response took, retries included (set by
    /// [`retry::Retrying`]; 1 below it).
    pub(crate) attempts: u32,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("final_url", &redact(&self.final_url))
            .finish_non_exhaustive()
    }
}

impl Response {
    pub fn new(
        status: u16,
        headers: Vec<(String, String)>,
        final_url: String,
        body: Box<dyn Read + Send>,
    ) -> Self {
        Self {
            status,
            headers,
            final_url,
            cache: CacheState::Network,
            body,
            wire_bytes: None,
            attempts: 1,
        }
    }

    /// How many wire requests this response took, retries included.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// A header value, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn location(&self) -> Option<&str> {
        self.header("location")
    }

    /// The response itself on 2xx; otherwise `http_status` (retryable for 5xx).
    pub fn error_for_status(self, context: &str) -> Result<Self> {
        if (200..300).contains(&self.status) {
            return Ok(self);
        }
        Err(Error::http_status(
            self.status,
            format!(
                "{context}: HTTP {} from {}",
                self.status,
                redact(&self.final_url)
            ),
            Suggestion::text("retry later; if it persists, check the source"),
        ))
    }

    /// The body as a stream (downloads). Read errors from the size cap map through
    /// [`body_error`].
    pub fn into_reader(self) -> Box<dyn Read + Send> {
        self.body
    }

    /// The whole body (API calls).
    pub fn bytes(self) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        let mut reader = self.body;
        reader.read_to_end(&mut body).map_err(body_error)?;
        Ok(body)
    }

    pub(crate) fn take_body(&mut self) -> Box<dyn Read + Send> {
        std::mem::replace(&mut self.body, Box::new(std::io::empty()))
    }

    pub(crate) fn set_body(&mut self, body: Box<dyn Read + Send>) {
        self.body = body;
    }
}

/// Maps a body read error: the size cap becomes `archive_too_large`, a failure to write
/// the request's `net.log` line keeps its own error, anything else is `network`.
pub fn body_error(err: std::io::Error) -> Error {
    let inner = err.get_ref();
    if let Some(cap) = inner.and_then(|e| e.downcast_ref::<guard::SizeCapExceeded>()) {
        return guard::too_large(cap.max_bytes, cap.api_host.as_deref());
    }
    if let Some(logged) = inner.and_then(|e| e.downcast_ref::<Error>()) {
        return logged.clone();
    }
    Error::new(
        ErrorCode::Network,
        format!("reading the response failed: {err}"),
        Suggestion::text("check the network connection, then retry"),
    )
}

/// What the command layer decides about networking for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetConfig {
    /// `--offline` or `OMADDONS_OFFLINE=1`.
    pub offline: bool,
    pub curseforge: bool,
    pub wago: bool,
    /// The hosts git subprocesses may reach, from config; never HTTP's.
    pub git_hosts: Vec<String>,
    /// `EffectiveLimits::download_max_bytes`.
    pub download_max_bytes: u64,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            offline: false,
            curseforge: false,
            wago: false,
            git_hosts: Vec::new(),
            download_max_bytes: crate::limits::DOWNLOAD_MAX_BYTES,
        }
    }
}

impl NetConfig {
    /// What HTTP may reach: the always-list and the enabled optional sources, never a
    /// `git_hosts` host.
    pub fn http_allowlist(&self) -> Allowlist {
        Allowlist::new(self.curseforge, self.wago)
    }

    /// What the git binary may reach: exactly `git_hosts`.
    pub fn git_allowlist(&self) -> GitHosts {
        GitHosts::new(&self.git_hosts)
    }
}

/// The production stack, outermost first:
/// `Limited<Retrying<Validating<Redirecting<Guarded<Decoding<transport>>>>>>`, over the
/// transport [`select_transport`] picks from the environment.
pub fn stack(config: &NetConfig, log: NetLog, paths: &Paths) -> Box<dyn Http> {
    let env = |name: &str| std::env::var(name).ok();
    let transport = select_transport(&env, cfg!(debug_assertions), paths);
    stack_with(config, log, paths, transport)
}

/// The bottom transport a run uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportChoice {
    Real,
    /// `OMADDONS_HTTP_FIXTURES=<dir>` (debug builds only).
    Replay(std::path::PathBuf),
    /// `OMADDONS_HTTP_FIXTURES=<dir>` with `OMADDONS_RECORD=1` (debug builds only).
    Record(std::path::PathBuf),
}

/// Which transport the environment asks for; release builds always get the real one.
pub fn choose_transport(env: &dyn Fn(&str) -> Option<String>, debug: bool) -> TransportChoice {
    match env("OMADDONS_HTTP_FIXTURES").filter(|d| debug && !d.is_empty()) {
        Some(dir) if env("OMADDONS_RECORD").as_deref() == Some("1") => {
            TransportChoice::Record(dir.into())
        }
        Some(dir) => TransportChoice::Replay(dir.into()),
        None => TransportChoice::Real,
    }
}

/// Builds the chosen transport. A fixture directory that cannot be loaded gives a
/// transport that fails every request with the load error.
pub fn select_transport(
    env: &dyn Fn(&str) -> Option<String>,
    debug: bool,
    paths: &Paths,
) -> Box<dyn Http> {
    let built = match choose_transport(env, debug) {
        TransportChoice::Real => return Box::new(UreqTransport::new()),
        TransportChoice::Replay(dir) => {
            replay::ReplayTransport::load(&dir).map(|t| Box::new(t) as Box<dyn Http>)
        }
        TransportChoice::Record(dir) => {
            record::RecordTransport::new(&dir, Box::new(UreqTransport::new()), paths)
                .map(|t| Box::new(t) as Box<dyn Http>)
        }
    };
    built.unwrap_or_else(|err| Box::new(Failing(err)))
}

/// Fails every request with one error.
struct Failing(Error);

impl Http for Failing {
    fn send(&self, _: Request) -> Result<Response> {
        Err(self.0.clone())
    }
}

/// The same stack over another transport (fixture replay and record mode, tests).
pub fn stack_with(
    config: &NetConfig,
    log: NetLog,
    paths: &Paths,
    transport: Box<dyn Http>,
) -> Box<dyn Http> {
    let validating = Validating::new(core_stack_with(config, log, transport), paths);
    let retrying = Retrying::new(Box::new(validating));
    Box::new(Limited::new(Box::new(retrying)))
}

/// The core without the resilience layers: `Redirecting<Guarded<Decoding<transport>>>`.
pub fn core_stack_with(config: &NetConfig, log: NetLog, transport: Box<dyn Http>) -> Box<dyn Http> {
    let guarded = Guarded::new(
        Box::new(decode::Decoding::new(transport)),
        config.http_allowlist(),
        log,
        config.offline,
        config.download_max_bytes,
        crate::limits::API_RESPONSE_MAX_BYTES,
    );
    Box::new(Redirecting::new(guarded))
}

/// `scheme://host/path` of a URL, without userinfo, query, fragment or download token
/// ([`redact_path`]), for messages.
pub(crate) fn redact(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let rest = match rest.split_once('/') {
        Some((authority, path)) => {
            let authority = strip_userinfo(authority);
            let host = authority.split(':').next().unwrap_or(authority);
            format!("{authority}{}", redact_path(host, &format!("/{path}")))
        }
        None => strip_userinfo(rest).to_owned(),
    };
    if scheme.is_empty() {
        rest
    } else {
        format!("{scheme}://{rest}")
    }
}

fn strip_userinfo(authority: &str) -> &str {
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

/// A URL path on `host` without its secret: a Tukui download token
/// (`/v1/download/<slug>/<token>` on `api.tukui.org`) becomes `/v1/download/<slug>/…`.
/// Messages ([`redact`]) and `net.log` both use it.
pub(crate) fn redact_path(host: &str, path: &str) -> String {
    if host.eq_ignore_ascii_case("api.tukui.org")
        && let Some(rest) = path.strip_prefix("/v1/download/")
        && let Some((slug, _token)) = rest.split_once('/')
    {
        return format!("/v1/download/{slug}/…");
    }
    path.to_owned()
}
