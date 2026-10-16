//! Network and filesystem-scan politeness constants. Change a value here or nowhere.
//!
//! These are the vision §5.13 `src/politeness.rs` table (D22), plus `MAX_REDIRECTS`
//! (decided with the net-core bead). Changing a default means updating that table in a
//! comment on the vision bead. Only `DEFAULT_CHECK_INTERVAL` may be overridden from
//! config; see [`crate::limits::EffectiveLimits`].

use std::time::Duration;

use crate::limits::{LimitInfo, LimitValue, limit, ms};

/// Sent with every request.
pub const USER_AGENT: &str = concat!("omaddons/", env!("CARGO_PKG_VERSION"));
/// Size of the global request worker pool.
pub const MAX_CONCURRENT_REQUESTS: usize = 6;
/// Concurrent requests per host; also protects the github.com web endpoints'
/// undocumented secondary limits.
pub const MAX_CONCURRENT_PER_HOST: usize = 3;
/// TCP and TLS connect timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Read timeout for JSON API calls.
pub const API_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for a whole archive download.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Retries for idempotent GETs only, on 429, 5xx, timeout or connection reset.
pub const MAX_RETRIES: u32 = 3;
/// First backoff step; exponential ×2 with full jitter.
pub const BACKOFF_BASE: Duration = Duration::from_millis(500);
/// Largest backoff step.
pub const BACKOFF_MAX: Duration = Duration::from_secs(8);
/// A longer `Retry-After` fails with `rate_limited` and the reset time.
pub const RETRY_AFTER_CAP: Duration = Duration::from_secs(60);
/// `outdated` reuses the last per-source result inside this window unless `--refresh`.
pub const SOURCE_RECHECK_MIN_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Widget background check interval. (config)
pub const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Minimum age of the WoWInterface file list before a refetch, or the server's
/// `max-age`, whichever is larger.
pub const WOWI_FILELIST_MIN_AGE: Duration = Duration::from_secs(60 * 60);
/// For the REST-only GitHub paths (beta/alpha, repos without `release.json`, search):
/// at or below this `X-RateLimit-Remaining`, stop and return `rate_limited` with the
/// reset time.
pub const GITHUB_REST_RESERVE: u32 = 5;
/// `POST /v1/mods` ids per request (server maximum UNVERIFIED).
pub const CURSEFORGE_BATCH_SIZE: usize = 50;
/// WoWInterface `filedetails` ids per request (server maximum UNVERIFIED).
pub const WOWI_DETAILS_BATCH_SIZE: usize = 50;
/// Scan depth under each `drive_c` and under `~/Games`.
pub const DISCOVERY_MAX_DEPTH: usize = 4;
/// Directories skipped under `drive_c`, matched case-insensitively.
/// `ProgramData/Battle.net/Agent/product.db` is read directly as a hint.
pub const DISCOVERY_SKIP_DIRS: &[&str] = &["windows", "users", "ProgramData"];
/// Directories visited per scan before stopping with a warning.
pub const DISCOVERY_MAX_DIRS: usize = 50_000;
/// Redirect hops followed per request before failing with `http_status`; every hop is
/// checked against the host allowlist.
pub const MAX_REDIRECTS: usize = 5;
/// Repositories one `search` asks GitHub's search API for (one request per run).
pub const GITHUB_SEARCH_PER_PAGE: usize = 20;

/// Every politeness constant, in table order.
pub const REGISTRY: &[LimitInfo] = &[
    limit(
        "USER_AGENT",
        LimitValue::Text(USER_AGENT),
        "text",
        None,
        "All requests",
    ),
    limit(
        "MAX_CONCURRENT_REQUESTS",
        LimitValue::Count(MAX_CONCURRENT_REQUESTS as u64),
        "count",
        None,
        "Global worker pool",
    ),
    limit(
        "MAX_CONCURRENT_PER_HOST",
        LimitValue::Count(MAX_CONCURRENT_PER_HOST as u64),
        "count",
        None,
        "Also protects github.com web endpoints' undocumented secondary limits",
    ),
    limit(
        "CONNECT_TIMEOUT",
        LimitValue::Millis(ms(CONNECT_TIMEOUT)),
        "ms",
        None,
        "Connect timeout",
    ),
    limit(
        "API_READ_TIMEOUT",
        LimitValue::Millis(ms(API_READ_TIMEOUT)),
        "ms",
        None,
        "JSON API calls",
    ),
    limit(
        "DOWNLOAD_TIMEOUT",
        LimitValue::Millis(ms(DOWNLOAD_TIMEOUT)),
        "ms",
        None,
        "Whole archive download",
    ),
    limit(
        "MAX_RETRIES",
        LimitValue::Count(MAX_RETRIES as u64),
        "count",
        None,
        "Idempotent GET only; 429, 5xx, timeout or connection reset",
    ),
    limit(
        "BACKOFF_BASE",
        LimitValue::Millis(ms(BACKOFF_BASE)),
        "ms",
        None,
        "Exponential x2, full jitter",
    ),
    limit(
        "BACKOFF_MAX",
        LimitValue::Millis(ms(BACKOFF_MAX)),
        "ms",
        None,
        "Largest backoff step",
    ),
    limit(
        "RETRY_AFTER_CAP",
        LimitValue::Millis(ms(RETRY_AFTER_CAP)),
        "ms",
        None,
        "A longer Retry-After fails with rate_limited and the reset time",
    ),
    limit(
        "SOURCE_RECHECK_MIN_INTERVAL",
        LimitValue::Millis(ms(SOURCE_RECHECK_MIN_INTERVAL)),
        "ms",
        None,
        "outdated reuses the last per-source result inside the window unless --refresh",
    ),
    limit(
        "DEFAULT_CHECK_INTERVAL",
        LimitValue::Millis(ms(DEFAULT_CHECK_INTERVAL)),
        "ms",
        Some("default_check_interval"),
        "Widget background check",
    ),
    limit(
        "WOWI_FILELIST_MIN_AGE",
        LimitValue::Millis(ms(WOWI_FILELIST_MIN_AGE)),
        "ms",
        None,
        "Or the server's max-age, whichever is larger",
    ),
    limit(
        "GITHUB_REST_RESERVE",
        LimitValue::Count(GITHUB_REST_RESERVE as u64),
        "count",
        None,
        "REST-only paths stop at or below this X-RateLimit-Remaining with rate_limited",
    ),
    limit(
        "CURSEFORGE_BATCH_SIZE",
        LimitValue::Count(CURSEFORGE_BATCH_SIZE as u64),
        "count",
        None,
        "POST /v1/mods ids per request (server maximum UNVERIFIED)",
    ),
    limit(
        "WOWI_DETAILS_BATCH_SIZE",
        LimitValue::Count(WOWI_DETAILS_BATCH_SIZE as u64),
        "count",
        None,
        "filedetails ids per request (server maximum UNVERIFIED)",
    ),
    limit(
        "DISCOVERY_MAX_DEPTH",
        LimitValue::Count(DISCOVERY_MAX_DEPTH as u64),
        "count",
        None,
        "Under each drive_c and under ~/Games",
    ),
    limit(
        "DISCOVERY_SKIP_DIRS",
        LimitValue::List(DISCOVERY_SKIP_DIRS),
        "list",
        None,
        "Under drive_c, case-insensitive; product.db is read directly as a hint",
    ),
    limit(
        "DISCOVERY_MAX_DIRS",
        LimitValue::Count(DISCOVERY_MAX_DIRS as u64),
        "count",
        None,
        "Directories visited per scan before stopping with a warning",
    ),
    limit(
        "MAX_REDIRECTS",
        LimitValue::Count(MAX_REDIRECTS as u64),
        "count",
        None,
        "Hops followed per request; every hop is checked against the allowlist",
    ),
    limit(
        "GITHUB_SEARCH_PER_PAGE",
        LimitValue::Count(GITHUB_SEARCH_PER_PAGE as u64),
        "count",
        None,
        "Repositories one search asks GitHub's search API for (10 requests a minute unauthenticated)",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_names_the_crate_version() {
        assert_eq!(
            USER_AGENT.strip_prefix("omaddons/"),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn spot_check_values() {
        assert_eq!(BACKOFF_BASE.as_millis(), 500);
        assert_eq!(DEFAULT_CHECK_INTERVAL.as_secs(), 6 * 3600);
        assert_eq!(SOURCE_RECHECK_MIN_INTERVAL.as_secs(), 15 * 60);
        assert_eq!(DISCOVERY_SKIP_DIRS, ["windows", "users", "ProgramData"]);
        assert_eq!(MAX_CONCURRENT_REQUESTS, 6);
        assert_eq!(MAX_CONCURRENT_PER_HOST, 3);
    }
}
