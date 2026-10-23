//! `Retrying`: transient failures (HTTP 429, any 5xx, transport `network` errors) are
//! retried up to [`MAX_RETRIES`] times with full-jitter exponential backoff, and a
//! `Retry-After` up to [`RETRY_AFTER_CAP`] replaces the computed delay (vision §5.13,
//! D22). An API body is read here, so a connection that fails while it arrives is
//! retried too, within the same budget; a download streams on, and its caller restarts
//! it ([`backoff_delay`] is shared). A longer `Retry-After`, or a primary rate limit
//! (`X-RateLimit-Remaining: 0`), fails at once with `rate_limited` and its reset time.
//! The net layer only has GET, HEAD and a `PROPFIND` with one fixed body, so every
//! retried request is idempotent; each attempt is its own wire request and its own
//! `net.log` line.

use std::io::Read;
use std::sync::Mutex;
use std::time::Duration;

use jiff::Timestamp;

use super::{Http, Request, Response, TimeoutKind, redact};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::politeness::{BACKOFF_BASE, BACKOFF_MAX, MAX_RETRIES, RETRY_AFTER_CAP};

/// Waits between attempts; tests record instead of sleeping.
pub trait Sleeper: Send + Sync {
    fn sleep(&self, duration: Duration);
}

/// The time for `Retry-After` dates and reset times.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// Jitter randomness; no cryptographic quality needed.
pub trait Rng: Send {
    fn next_u64(&mut self) -> u64;
}

/// `std::thread::sleep`.
pub struct RealSleeper;

impl Sleeper for RealSleeper {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The system clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// xorshift64*, seeded from the time and the process id.
pub struct XorShift(u64);

impl XorShift {
    pub fn seeded() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Self::new(nanos ^ u64::from(std::process::id()).rotate_left(32))
    }

    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }
}

impl Rng for XorShift {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

/// Retries transient failures of the stack below it.
pub struct Retrying {
    inner: Box<dyn Http>,
    sleeper: Box<dyn Sleeper>,
    clock: Box<dyn Clock>,
    rng: Mutex<Box<dyn Rng>>,
}

impl Retrying {
    pub fn new(inner: Box<dyn Http>) -> Self {
        Self::with(
            inner,
            Box::new(RealSleeper),
            Box::new(SystemClock),
            Box::new(XorShift::seeded()),
        )
    }

    pub fn with(
        inner: Box<dyn Http>,
        sleeper: Box<dyn Sleeper>,
        clock: Box<dyn Clock>,
        rng: Box<dyn Rng>,
    ) -> Self {
        Self {
            inner,
            sleeper,
            clock,
            rng: Mutex::new(rng),
        }
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let random = match self.rng.lock() {
            Ok(mut rng) => rng.next_u64(),
            Err(poisoned) => poisoned.into_inner().next_u64(),
        };
        backoff_delay(attempt, random)
    }
}

/// The full-jitter delay before retry `attempt` (0-based): uniformly random, from
/// `random`, in `0..=min(BACKOFF_MAX, BACKOFF_BASE × 2^attempt)`.
pub fn backoff_delay(attempt: u32, random: u64) -> Duration {
    let cap = BACKOFF_BASE
        .saturating_mul(2u32.saturating_pow(attempt))
        .min(BACKOFF_MAX);
    let cap_ms = u64::try_from(cap.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(random % cap_ms.saturating_add(1))
}

/// Reads an API response's body whole into memory (every caller reads it whole anyway,
/// under `API_RESPONSE_MAX_BYTES`), so a connection that fails while it arrives fails
/// here, where it can be retried. A download's body is left to stream.
fn buffer_api_body(req: &Request, resp: &mut Response) -> Result<()> {
    if req.timeout != TimeoutKind::Api {
        return Ok(());
    }
    let mut body = Vec::new();
    resp.take_body()
        .read_to_end(&mut body)
        .map_err(super::body_error)?;
    resp.set_body(Box::new(std::io::Cursor::new(body)));
    Ok(())
}

impl Http for Retrying {
    fn send(&self, req: Request) -> Result<Response> {
        let mut attempt = 0u32;
        loop {
            match self.inner.send(req.clone()) {
                Ok(mut resp) => {
                    let now = self.clock.now();
                    if let Some(err) = primary_limit(&resp, now) {
                        return Err(err);
                    }
                    // A 403 asking to come back later (GitHub's secondary limits) is a
                    // rate-limit answer, as a 429 is.
                    let limit = resp.status == 429
                        || (resp.status == 403 && resp.header("retry-after").is_some());
                    if !limit && !(500..600).contains(&resp.status) {
                        // A connection that fails mid-body is retried from the start, as
                        // one that fails before the headers.
                        match buffer_api_body(&req, &mut resp) {
                            Ok(()) => {
                                resp.attempts = attempt + 1;
                                return Ok(resp);
                            }
                            Err(err) if retries(&err) && attempt < MAX_RETRIES => {
                                drop(resp);
                                self.sleeper.sleep(self.backoff(attempt));
                                attempt += 1;
                                continue;
                            }
                            Err(err) => return Err(err),
                        }
                    }
                    let wait = resp.header("retry-after").and_then(|v| retry_after(v, now));
                    if let Some(wait) = wait
                        && wait > RETRY_AFTER_CAP
                    {
                        return Err(limited(&resp, now, wait));
                    }
                    if attempt >= MAX_RETRIES {
                        return Err(if limit {
                            limited(&resp, now, wait.unwrap_or_default())
                        } else {
                            Error::http_status(
                                resp.status,
                                format!(
                                    "HTTP {} from {} after {} attempts",
                                    resp.status,
                                    redact(&resp.final_url),
                                    attempt + 1
                                ),
                                Suggestion::text("retry later; if it persists, check the source"),
                            )
                        });
                    }
                    drop(resp);
                    self.sleeper
                        .sleep(wait.unwrap_or_else(|| self.backoff(attempt)));
                }
                Err(err) if retries(&err) && attempt < MAX_RETRIES => {
                    self.sleeper.sleep(self.backoff(attempt));
                }
                Err(err) => return Err(err),
            }
            attempt += 1;
        }
    }
}

/// A connection that failed, timed out or was reset; never one said to be final (too
/// many redirects, an encoding omaddons does not decode), which fails alike again.
fn retries(err: &Error) -> bool {
    err.code == ErrorCode::Network && err.retryable
}

/// `rate_limited` for a 403/429 carrying `X-RateLimit-Remaining: 0`, reset from
/// `X-RateLimit-Reset` (epoch seconds).
fn primary_limit(resp: &Response, now: Timestamp) -> Option<Error> {
    if !matches!(resp.status, 403 | 429)
        || resp.header("x-ratelimit-remaining").map(str::trim) != Some("0")
    {
        return None;
    }
    let reset = resp
        .header("x-ratelimit-reset")
        .and_then(|v| v.trim().parse::<i64>().ok())
        .and_then(|s| Timestamp::from_second(s).ok())
        .unwrap_or(now);
    Some(rate_limited(resp, reset))
}

fn limited(resp: &Response, now: Timestamp, wait: Duration) -> Error {
    let reset = jiff::SignedDuration::try_from(wait)
        .ok()
        .and_then(|d| now.checked_add(d).ok())
        .unwrap_or(now);
    rate_limited(resp, reset)
}

fn rate_limited(resp: &Response, reset: Timestamp) -> Error {
    let host = redact(&resp.final_url);
    Error::rate_limited(
        reset,
        format!("{host} is rate limiting requests until {reset}"),
        Suggestion::text(format!("retry after {reset}")),
    )
}

/// A `Retry-After` value (delta-seconds or an HTTP-date) as a delay from `now`; a date
/// in the past is no delay.
pub(crate) fn retry_after(value: &str, now: Timestamp) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = jiff::fmt::rfc2822::parse(value).ok()?.timestamp();
    Some(Duration::try_from(at.duration_since(now)).unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;

    use super::*;
    use crate::net::Method;

    /// Answers from a script and counts the attempts.
    struct Script {
        answers: Mutex<VecDeque<Answer>>,
        seen: Arc<Mutex<Vec<Method>>>,
    }

    impl Http for Script {
        fn send(&self, req: Request) -> Result<Response> {
            self.seen.lock().unwrap().push(req.method);
            match self.answers.lock().unwrap().pop_front().unwrap() {
                Ok((status, headers)) => Ok(Response::new(
                    status,
                    headers
                        .into_iter()
                        .map(|(k, v)| (k.into(), v.into()))
                        .collect(),
                    req.url,
                    Box::new(std::io::empty()),
                )),
                Err(code) => Err(Error::new(code, "transport", Suggestion::text("x"))),
            }
        }
    }

    struct Recorder(Arc<Mutex<Vec<Duration>>>);

    impl Sleeper for Recorder {
        fn sleep(&self, duration: Duration) {
            self.0.lock().unwrap().push(duration);
        }
    }

    struct Fixed(Timestamp);

    impl Clock for Fixed {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    const NOW: &str = "2026-10-14T12:00:00Z";

    type Answer = std::result::Result<(u16, Vec<(&'static str, &'static str)>), ErrorCode>;

    fn run(answers: Vec<Answer>) -> (Result<Response>, Vec<Duration>, usize) {
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let retrying = Retrying::with(
            Box::new(Script {
                answers: Mutex::new(answers.into()),
                seen: Arc::clone(&seen),
            }),
            Box::new(Recorder(Arc::clone(&sleeps))),
            Box::new(Fixed(NOW.parse().unwrap())),
            Box::new(XorShift::new(42)),
        );
        let result = retrying.send(Request::get("https://example.org/x", "test"));
        let attempts = seen.lock().unwrap().len();
        let sleeps = sleeps.lock().unwrap().clone();
        (result, sleeps, attempts)
    }

    #[test]
    fn transient_failures_are_retried_with_jitter() {
        let (result, sleeps, attempts) = run(vec![
            Ok((500, vec![])),
            Ok((500, vec![])),
            Ok((200, vec![])),
        ]);
        let resp = result.unwrap();
        assert_eq!(
            (resp.status, attempts, resp.attempts()),
            (200, 3, 3),
            "the response counts its retries"
        );
        assert_eq!(sleeps.len(), 2);
        assert!(sleeps[0] <= BACKOFF_BASE, "{sleeps:?}");
        assert!(sleeps[1] <= BACKOFF_BASE * 2, "{sleeps:?}");

        let (result, sleeps, attempts) = run(vec![Ok((503, vec![])); 4]);
        let err = result.unwrap_err();
        assert_eq!(
            (err.code, err.retryable, attempts, sleeps.len()),
            (ErrorCode::HttpStatus, true, 4, 3)
        );
        assert!(sleeps[2] <= BACKOFF_BASE * 4);

        let (result, _, attempts) = run(vec![Err(ErrorCode::Network), Ok((200, vec![]))]);
        assert_eq!(
            (result.unwrap().status, attempts),
            (200, 2),
            "a transport error is retried"
        );
        let (result, _, attempts) = run(vec![Err(ErrorCode::Network); 4]);
        assert_eq!(
            (result.unwrap_err().code, attempts),
            (ErrorCode::Network, 4)
        );
        let (result, _, attempts) = run(vec![Err(ErrorCode::HostBlocked)]);
        assert_eq!(
            (result.unwrap_err().code, attempts),
            (ErrorCode::HostBlocked, 1),
            "refusals are final"
        );
        let (result, _, attempts) = run(vec![Ok((404, vec![]))]);
        let resp = result.unwrap();
        assert_eq!(
            (resp.status, attempts, resp.attempts()),
            (404, 1, 1),
            "a 4xx is the caller's to judge"
        );
    }

    #[test]
    fn retry_after_is_honored_up_to_the_cap() {
        let (result, sleeps, _) = run(vec![
            Ok((429, vec![("Retry-After", "2")])),
            Ok((200, vec![])),
        ]);
        assert_eq!(
            (result.unwrap().status, sleeps),
            (200, vec![Duration::from_secs(2)])
        );

        let (result, sleeps, attempts) = run(vec![Ok((503, vec![("Retry-After", "120")]))]);
        let err = result.unwrap_err();
        assert_eq!(
            (err.code, attempts, sleeps.len()),
            (ErrorCode::RateLimited, 1, 0)
        );
        assert_eq!(err.details["reset_at"], "2026-10-14T12:02:00Z");

        let (result, _, attempts) = run(vec![Ok((429, vec![])); 4]);
        assert_eq!(
            (result.unwrap_err().code, attempts),
            (ErrorCode::RateLimited, 4)
        );

        // A 403 asking to come back later (GitHub's secondary limits) is a rate limit.
        let (result, sleeps, _) = run(vec![
            Ok((403, vec![("Retry-After", "2")])),
            Ok((200, vec![])),
        ]);
        assert_eq!(
            (result.unwrap().status, sleeps),
            (200, vec![Duration::from_secs(2)])
        );
        let (result, _, attempts) = run(vec![Ok((
            403,
            vec![("Retry-After", "120"), ("X-RateLimit-Remaining", "4000")],
        ))]);
        assert_eq!(
            (result.unwrap_err().code, attempts),
            (ErrorCode::RateLimited, 1)
        );
    }

    /// A network error said to be final (too many redirects) fails alike again: never
    /// retried.
    #[test]
    fn a_final_network_error_is_never_retried() {
        struct Final(Arc<Mutex<usize>>);
        impl Http for Final {
            fn send(&self, _: Request) -> Result<Response> {
                *self.0.lock().unwrap() += 1;
                Err(Error::new(ErrorCode::Network, "loop", Suggestion::text("x")).retryable(false))
            }
        }
        let sent = Arc::new(Mutex::new(0));
        let retrying = Retrying::with(
            Box::new(Final(Arc::clone(&sent))),
            Box::new(Recorder(Arc::new(Mutex::new(Vec::new())))),
            Box::new(Fixed(NOW.parse().unwrap())),
            Box::new(XorShift::new(42)),
        );
        let err = retrying
            .send(Request::get("https://example.org/x", "test"))
            .unwrap_err();
        assert_eq!((err.code, *sent.lock().unwrap()), (ErrorCode::Network, 1));
    }

    #[test]
    fn http_dates() {
        let now: Timestamp = NOW.parse().unwrap();
        assert_eq!(
            retry_after("Wed, 14 Oct 2026 12:00:30 GMT", now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            retry_after("Wed, 14 Oct 2026 11:00:00 GMT", now),
            Some(Duration::ZERO)
        );
        assert_eq!(retry_after(" 7 ", now), Some(Duration::from_secs(7)));
        assert_eq!(retry_after("soon", now), None);
    }

    #[test]
    fn primary_rate_limits_fail_at_once() {
        let headers = vec![
            ("X-RateLimit-Remaining", "0"),
            ("X-RateLimit-Reset", "1792152000"),
        ];
        let (result, sleeps, attempts) = run(vec![Ok((403, headers))]);
        let err = result.unwrap_err();
        assert_eq!(
            (err.code, attempts, sleeps.len()),
            (ErrorCode::RateLimited, 1, 0)
        );
        assert_eq!(err.details["reset_at"], "2026-10-16T12:00:00Z");
        let (result, _, _) = run(vec![Ok((403, vec![("X-RateLimit-Remaining", "5")]))]);
        assert_eq!(
            result.unwrap().status,
            403,
            "a plain 403 is not a rate limit"
        );
    }

    #[test]
    fn backoff_stays_in_bounds() {
        let retrying = Retrying::new(Box::new(Script {
            answers: Mutex::new(VecDeque::new()),
            seen: Arc::new(Mutex::new(Vec::new())),
        }));
        for attempt in 0..10 {
            let cap = BACKOFF_BASE
                .saturating_mul(2u32.saturating_pow(attempt))
                .min(BACKOFF_MAX);
            for _ in 0..50 {
                assert!(retrying.backoff(attempt) <= cap);
            }
        }
    }

    /// A body that yields its bytes, then fails as `then` says (or ends).
    struct Flaky {
        bytes: std::io::Cursor<Vec<u8>>,
        then: Option<std::io::Error>,
    }

    impl Read for Flaky {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.bytes.read(buf)?;
            match (n, self.then.take()) {
                (0, Some(err)) => Err(err),
                (_, then) => {
                    self.then = then;
                    Ok(n)
                }
            }
        }
    }

    fn whole(text: &str) -> std::result::Result<Flaky, ErrorCode> {
        Ok(Flaky {
            bytes: std::io::Cursor::new(text.as_bytes().to_vec()),
            then: None,
        })
    }

    /// 100 bytes, then the connection resets.
    fn reset() -> std::result::Result<Flaky, ErrorCode> {
        Ok(Flaky {
            bytes: std::io::Cursor::new(vec![b'x'; 100]),
            then: Some(std::io::ErrorKind::ConnectionReset.into()),
        })
    }

    /// Answers 200 with scripted bodies (or a transport error) and counts the attempts.
    struct Bodies {
        answers: Mutex<VecDeque<std::result::Result<Flaky, ErrorCode>>>,
        attempts: Arc<Mutex<usize>>,
    }

    impl Http for Bodies {
        fn send(&self, req: Request) -> Result<Response> {
            *self.attempts.lock().unwrap() += 1;
            match self.answers.lock().unwrap().pop_front().unwrap() {
                Ok(body) => Ok(Response::new(200, Vec::new(), req.url, Box::new(body))),
                Err(code) => Err(Error::new(code, "transport", Suggestion::text("x"))),
            }
        }
    }

    /// Sends `req` over `answers`: the result with its body read, the sleeps, and the
    /// attempts.
    fn read_through(
        req: Request,
        answers: Vec<std::result::Result<Flaky, ErrorCode>>,
    ) -> (Result<Vec<u8>>, usize, usize) {
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let attempts = Arc::new(Mutex::new(0));
        let retrying = Retrying::with(
            Box::new(Bodies {
                answers: Mutex::new(answers.into()),
                attempts: Arc::clone(&attempts),
            }),
            Box::new(Recorder(Arc::clone(&sleeps))),
            Box::new(Fixed(NOW.parse().unwrap())),
            Box::new(XorShift::new(42)),
        );
        let result = retrying.send(req).and_then(Response::bytes);
        let sleeps = sleeps.lock().unwrap().len();
        let attempts = *attempts.lock().unwrap();
        (result, sleeps, attempts)
    }

    #[test]
    fn an_api_body_that_fails_midway_is_requested_again() {
        let api = || Request::get("https://example.org/x", "test");
        let (result, sleeps, attempts) = read_through(api(), vec![reset(), whole("all of it")]);
        assert_eq!(
            (result.unwrap(), sleeps, attempts),
            (b"all of it".to_vec(), 1, 2)
        );
        // One budget for both phases: a transport error, then three resets.
        let answers = vec![Err(ErrorCode::Network), reset(), reset(), reset()];
        let (result, sleeps, attempts) = read_through(api(), answers);
        let err = result.unwrap_err();
        assert_eq!(
            (err.code, err.retryable, sleeps, attempts),
            (ErrorCode::Network, true, 3, 4)
        );
        // The size cap is final.
        let capped = Flaky {
            bytes: std::io::Cursor::new(Vec::new()),
            then: Some(std::io::Error::other(
                super::super::guard::SizeCapExceeded {
                    max_bytes: 10,
                    api_host: Some("example.org".into()),
                },
            )),
        };
        let (result, sleeps, attempts) = read_through(api(), vec![Ok(capped)]);
        assert_eq!(
            (result.unwrap_err().code, sleeps, attempts),
            (ErrorCode::ArchiveTooLarge, 0, 1)
        );
        // A download streams on: its caller restarts it.
        let download = api().download();
        let (result, sleeps, attempts) = read_through(download, vec![reset(), whole("unused")]);
        assert_eq!(
            (result.unwrap_err().code, sleeps, attempts),
            (ErrorCode::Network, 0, 1)
        );
    }

    #[test]
    fn backoff_delay_is_full_jitter_under_the_cap() {
        assert_eq!(backoff_delay(0, 0), Duration::ZERO);
        let base = u64::try_from(BACKOFF_BASE.as_millis()).unwrap();
        assert_eq!(backoff_delay(0, base), BACKOFF_BASE);
        assert_eq!(backoff_delay(0, base + 1), Duration::ZERO);
        assert!(backoff_delay(30, u64::MAX) <= BACKOFF_MAX);
    }
}
