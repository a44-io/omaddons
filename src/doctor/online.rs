//! The online checks' client (M4-D51): what the `network` rows send under `--online`, and
//! nothing else. Each probe is exactly one wire request with one `net.log` line
//! (`"purpose":"doctor-probe"`), sent on the core stack (the allowlist on every hop, the
//! offline guard, the size caps, gzip and `net.log`) with manual redirects and no retry,
//! no validators and no concurrency limiter: a retried or followed probe would be up to
//! four requests and stretch a run on a failing network to minutes, and a doctor reports
//! what it saw. Each request ends within its check's remaining budget (M4-D43).
//!
//! The client is built once per view, at the first check that asks, and only when the run
//! was asked for `--online`; the run refuses `--online` with `--offline` before it starts,
//! so it is built online. The GitHub token resolves as for any REST request (a `command`
//! credential runs at the first `value()`), and its resolution warnings are dropped: the
//! doctor reports an exposed credential file as a finding of its own.

use std::fmt;
use std::sync::Arc;

use super::budget::Budget;
use crate::error::Error;
use crate::net::{self, Http, NetConfig, NetLog, Request};
use crate::sources::github_token::GithubToken;
use crate::sources::{SourceConfigView, SourceId};
use crate::state::config::Config;
use crate::state::paths::Paths;

/// Every probe's `net.log` purpose.
pub const PURPOSE: &str = "doctor-probe";

/// The probe client of one doctor run.
pub struct Online {
    http: Box<dyn Http>,
    token: Arc<GithubToken>,
    sources: SourceConfigView,
}

impl fmt::Debug for Online {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Online")
            .field("token", &self.token.source())
            .field("sources", &self.sources)
            .finish_non_exhaustive()
    }
}

/// Why a run has no probe client: its checks are then unanswered, never clean.
#[derive(Debug, Clone)]
pub enum OnlineUnavailable {
    /// The run was not asked for `--online`.
    Disabled,
    /// `config.toml` could not be read. The online rows require it, so a run that skips
    /// them for it never asks.
    NoConfig,
    /// `net.log` cannot be written: a request that cannot be logged is never sent.
    NetLog(Error),
}

/// What one probe got.
#[derive(Debug)]
pub enum Probe {
    /// An HTTP answer, its body read to the end.
    Answered {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// No answer: the transport failed (`network`), or omaddons did (an `internal` from
    /// the replay transport, a `net.log` line that could not be written).
    Failed(Error),
    /// The check's deadline passed before the request was sent, or while it was waiting.
    OutOfTime,
}

impl Probe {
    /// A header of an answer, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        match self {
            Self::Answered { headers, .. } => headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str()),
            Self::Failed(_) | Self::OutOfTime => None,
        }
    }
}

impl Online {
    /// The client of a run online, from `config`: the core stack over the transport the
    /// environment selects (fixture replay in debug builds), this machine's GitHub token
    /// and the sources config.toml leaves on.
    pub fn build(paths: &Paths, config: &Config) -> Result<Self, OnlineUnavailable> {
        let log = NetLog::new(paths).map_err(OnlineUnavailable::NetLog)?;
        let net_config = NetConfig {
            offline: false,
            git_hosts: config.git_hosts.clone(),
            ..NetConfig::default()
        };
        Ok(Self::with(
            net::core_stack(&net_config, log, paths),
            GithubToken::resolve(paths, &config.sources),
            SourceConfigView::from_config(&config.sources),
        ))
    }

    /// A client over `http` (tests give a recording fake).
    pub fn with(http: Box<dyn Http>, token: Arc<GithubToken>, sources: SourceConfigView) -> Self {
        Self {
            http,
            token,
            sources,
        }
    }

    /// A probe GET of `url`: the doctor's purpose, its redirect returned, not followed.
    pub fn get(url: impl Into<String>) -> Request {
        Request::get(url, PURPOSE).manual_redirects()
    }

    /// A probe `PROPFIND` of the collection at `url`, as [`Online::get`].
    pub fn propfind(url: impl Into<String>) -> Request {
        Request::propfind_version_names(url, PURPOSE).manual_redirects()
    }

    /// Sends `req` once, bounded by what is left of `budget`; nothing is sent once it has
    /// run out. The body is read to the end, so its `net.log` line is written before this
    /// returns, and a line that cannot be written is the probe's failure.
    pub fn send(&self, req: Request, budget: &Budget) -> Probe {
        if budget.expired() {
            return Probe::OutOfTime;
        }
        let failed = |error: Error| {
            if budget.expired() {
                Probe::OutOfTime
            } else {
                Probe::Failed(error)
            }
        };
        match self.http.send(req.within(budget.remaining())) {
            Ok(resp) => {
                let (status, headers) = (resp.status, resp.headers.clone());
                match resp.bytes() {
                    Ok(body) => Probe::Answered {
                        status,
                        headers,
                        body,
                    },
                    Err(error) => failed(error),
                }
            }
            Err(error) => failed(error),
        }
    }

    /// The GitHub token the checks send, resolved as for a REST request.
    pub fn token(&self) -> &GithubToken {
        &self.token
    }

    /// Whether config.toml leaves source `id` on.
    pub fn enabled(&self, id: SourceId) -> bool {
        self.sources.enabled(id)
    }
}

/// A recording fake `Http` for the online checks' unit tests: every request it is sent,
/// its header values redacted to their names, and an answer from a table.
#[cfg(test)]
pub(crate) mod fake {
    use std::io::Cursor;
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::error::{ErrorCode, Result, Suggestion};
    use crate::net::{Method, RedirectMode, Response};

    /// One request as the fake saw it: header values never kept, so a failing message
    /// never prints a token.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Sent {
        pub method: Method,
        pub url: String,
        pub purpose: &'static str,
        pub redirect: RedirectMode,
        pub deadline: Option<Duration>,
        pub header_names: Vec<String>,
    }

    impl Sent {
        pub(crate) fn host(&self) -> &str {
            let rest = self.url.split_once("://").map_or(&*self.url, |(_, r)| r);
            rest.split(['/', '?']).next().unwrap_or(rest)
        }

        pub(crate) fn authorized(&self) -> bool {
            self.header_names
                .iter()
                .any(|name| name.eq_ignore_ascii_case("authorization"))
        }
    }

    /// What the fake answers one request with.
    #[derive(Debug, Clone)]
    pub(crate) enum Answer {
        Status(u16, Vec<(&'static str, String)>, Vec<u8>),
        /// The transport failed with this code.
        Fails(ErrorCode),
        /// The body cannot be read to the end (a `net.log` line that cannot be written).
        BodyFails(ErrorCode),
        /// Sleeps this long, then fails as a timed-out transport does.
        Stalls(Duration),
    }

    impl Answer {
        pub(crate) fn status(status: u16) -> Self {
            Self::Status(status, Vec::new(), Vec::new())
        }
    }

    /// Answers by `table` (the first row whose URL part the request's URL contains), else
    /// `404`.
    pub(crate) struct Fake {
        pub sent: Arc<Mutex<Vec<Sent>>>,
        table: Vec<(&'static str, Answer)>,
    }

    impl Fake {
        pub(crate) fn new(table: Vec<(&'static str, Answer)>) -> (Self, Arc<Mutex<Vec<Sent>>>) {
            let sent = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    sent: Arc::clone(&sent),
                    table,
                },
                sent,
            )
        }
    }

    fn error(code: ErrorCode) -> Error {
        Error::new(code, "the fake failed", Suggestion::text("none"))
    }

    /// A body whose read fails with `code`'s error, as a guard's `net.log` write does.
    struct FailingBody(ErrorCode);

    impl std::io::Read for FailingBody {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other(error(self.0)))
        }
    }

    impl Http for Fake {
        fn send(&self, req: Request) -> Result<Response> {
            self.sent.lock().unwrap().push(Sent {
                method: req.method,
                url: req.url.clone(),
                purpose: req.purpose,
                redirect: req.redirect,
                deadline: req.deadline,
                header_names: req.headers.iter().map(|(name, _)| name.clone()).collect(),
            });
            let answer = self
                .table
                .iter()
                .find(|(part, _)| req.url.contains(part))
                .map_or(Answer::status(404), |(_, answer)| answer.clone());
            match answer {
                Answer::Status(status, headers, body) => Ok(Response::new(
                    status,
                    headers
                        .into_iter()
                        .map(|(name, value)| (name.to_owned(), value))
                        .collect(),
                    req.url,
                    Box::new(Cursor::new(body)),
                )),
                Answer::Fails(code) => Err(error(code)),
                Answer::BodyFails(code) => Ok(Response::new(
                    200,
                    Vec::new(),
                    req.url,
                    Box::new(FailingBody(code)),
                )),
                Answer::Stalls(wait) => {
                    std::thread::sleep(wait.min(req.deadline.unwrap_or(wait)));
                    Err(error(ErrorCode::Network))
                }
            }
        }
    }

    /// A client over a fake answering by `table`, with `token` and every source on but
    /// `disabled`.
    pub(crate) fn online(
        table: Vec<(&'static str, Answer)>,
        token: Arc<GithubToken>,
        disabled: &[SourceId],
    ) -> (Online, Arc<Mutex<Vec<Sent>>>) {
        let (fake, sent) = Fake::new(table);
        let sources: std::collections::BTreeMap<String, crate::state::config::SourceConfig> =
            disabled
                .iter()
                .map(|id| {
                    (
                        id.wire().to_owned(),
                        crate::state::config::SourceConfig {
                            enabled: Some(false),
                            ..Default::default()
                        },
                    )
                })
                .collect();
        (
            Online::with(
                Box::new(fake),
                token,
                SourceConfigView::from_config(&sources),
            ),
            sent,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::fake::{Answer, online};
    use super::*;
    use crate::error::ErrorCode;
    use crate::net::{Allowlist, Method, RedirectMode};

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn every_probe_is_one_request_with_the_doctor_purpose() {
        let (client, sent) = online(
            vec![("github.com", Answer::status(503))],
            GithubToken::none(),
            &[],
        );
        let budget = Budget::new(HOUR, 10);
        for (case, req, method) in [
            (
                "get",
                Online::get("https://github.com/o/r/releases/latest"),
                Method::Get,
            ),
            (
                "propfind",
                Online::propfind("https://repos.wowace.com/wow/x/tags/"),
                Method::Propfind,
            ),
        ] {
            sent.lock().unwrap().clear();
            let probe = client.send(req, &budget);
            let sent = sent.lock().unwrap().clone();
            assert_eq!(sent.len(), 1, "{case}: {sent:?}");
            assert_eq!(
                (sent[0].method, sent[0].purpose, sent[0].redirect),
                (method, PURPOSE, RedirectMode::Manual),
                "{case}: {sent:?}"
            );
            assert!(matches!(probe, Probe::Answered { .. }), "{case}: {probe:?}");
        }
        let sent_503 = sent.lock().unwrap().clone();
        assert_eq!(sent_503.len(), 1, "a 503 is not retried: {sent_503:?}");
    }

    /// Every host a `network` row can ask is on the always-list: no probe reaches a host
    /// a plain command could not.
    #[test]
    fn probe_hosts_are_allowlisted() {
        let allowlist = Allowlist::new(false, false);
        for url in crate::doctor::detectors::network::tests::probe_urls() {
            let host = url
                .strip_prefix("https://")
                .and_then(|rest| rest.split('/').next())
                .unwrap_or_default();
            assert!(allowlist.permits(host), "{url}: {host} is not allowlisted");
        }
    }

    #[test]
    fn the_budget_bounds_the_request() {
        let (client, sent) = online(
            vec![("api.github.com", Answer::status(200))],
            GithubToken::none(),
            &[],
        );
        let budget = Budget::new(Duration::from_secs(60), 10);
        let _ = client.send(Online::get("https://api.github.com/rate_limit"), &budget);
        let deadline = sent.lock().unwrap()[0].deadline;
        assert!(
            deadline.is_some_and(|d| d <= Duration::from_secs(60) && d > Duration::from_secs(50)),
            "the request ends within what is left: {deadline:?}"
        );
        let spent = Budget::new(Duration::from_millis(1), 10);
        std::thread::sleep(Duration::from_millis(5));
        let probe = client.send(Online::get("https://api.github.com/rate_limit"), &spent);
        assert!(matches!(probe, Probe::OutOfTime), "{probe:?}");
        assert_eq!(
            sent.lock().unwrap().len(),
            1,
            "an expired budget sends nothing"
        );
    }

    #[test]
    fn an_error_after_the_deadline_is_out_of_time() {
        let (client, sent) = online(
            vec![("github.com", Answer::Stalls(Duration::from_millis(50)))],
            GithubToken::none(),
            &[],
        );
        let probe = client.send(
            Online::get("https://github.com/o/r/releases/latest"),
            &Budget::new(Duration::from_millis(20), 10),
        );
        assert!(matches!(probe, Probe::OutOfTime), "{probe:?}");
        assert_eq!(sent.lock().unwrap().len(), 1, "it was sent");
        let (client, _) = online(
            vec![("github.com", Answer::Fails(ErrorCode::Network))],
            GithubToken::none(),
            &[],
        );
        let probe = client.send(
            Online::get("https://github.com/o/r/releases/latest"),
            &Budget::new(HOUR, 10),
        );
        assert!(
            matches!(&probe, Probe::Failed(e) if e.code == ErrorCode::Network),
            "an error inside the deadline is the failure: {probe:?}"
        );
    }

    #[test]
    fn the_body_is_read_to_the_end() {
        let body = b"x".repeat(100_000);
        let (client, _) = online(
            vec![(
                "api.tukui.org",
                Answer::Status(200, vec![("X-Test", "1".to_owned())], body.clone()),
            )],
            GithubToken::none(),
            &[],
        );
        let probe = client.send(
            Online::get("https://api.tukui.org/v1/addons"),
            &Budget::new(HOUR, 10),
        );
        assert_eq!(probe.header("x-test"), Some("1"));
        assert!(
            matches!(&probe, Probe::Answered { status: 200, body: got, .. } if *got == body),
            "{} bytes expected",
            body.len()
        );
        // A body that cannot be read to its end (its net.log line not written) fails.
        let (client, _) = online(
            vec![("api.tukui.org", Answer::BodyFails(ErrorCode::Io))],
            GithubToken::none(),
            &[],
        );
        let probe = client.send(
            Online::get("https://api.tukui.org/v1/addons"),
            &Budget::new(HOUR, 10),
        );
        assert!(
            matches!(&probe, Probe::Failed(e) if e.code == ErrorCode::Io),
            "{probe:?}"
        );
    }

    #[test]
    fn a_netlog_failure_makes_the_client_unavailable() {
        let (_dir, paths) = crate::doctor::view::tests::home();
        // `<root>/state` is a file, so the state directory below it cannot be resolved.
        std::fs::write(paths.state_dir().parent().unwrap(), b"").unwrap();
        let built = Online::build(&paths, &Config::default());
        assert!(
            matches!(built, Err(OnlineUnavailable::NetLog(_))),
            "{built:?}"
        );
    }

    #[test]
    fn offline_options_never_build_a_client() {
        let (_dir, paths) = crate::doctor::view::tests::home();
        let view = crate::doctor::view::DoctorView::load_at(
            paths.clone(),
            crate::doctor::view::ViewOptions::default(),
        );
        assert!(matches!(view.online(), Err(OnlineUnavailable::Disabled)));
        assert!(
            !paths.net_log().exists() && !paths.state_dir().exists(),
            "nothing was opened or written"
        );
    }
}
