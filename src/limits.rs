//! Safety limits. Change a value here or nowhere.
//!
//! These are the vision §5.13 `src/limits.rs` table (D22). Changing a default means
//! updating that table in a comment on the vision bead. Only the constants marked
//! "(config)" may be overridden, through the `[limits]` table of `config.toml` keyed by
//! the lowercase constant name; [`EffectiveLimits`] holds the result, and subsystems take
//! it as a parameter instead of reading these constants directly.

use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;

use crate::politeness::DEFAULT_CHECK_INTERVAL;

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Most entries an archive may hold. (config)
pub const ARCHIVE_MAX_ENTRIES: u64 = 20_000;
/// Most bytes an archive may expand to. (config)
pub const ARCHIVE_MAX_UNCOMPRESSED: u64 = GIB;
/// Zip-bomb guard: the N of the largest allowed N:1 compression ratio, checked per entry
/// and in total. (config)
pub const ARCHIVE_MAX_RATIO: u64 = 200;
/// The ratio applies once an entry, or the whole archive, has expanded past this many
/// bytes: repetitive files are not bombs (10 KB of padding deflates over 300:1, and a
/// mostly transparent 512x512 texture about 1000:1). 32 MiB covers a 2048x2048 RGBA
/// texture; `ARCHIVE_MAX_UNCOMPRESSED` still bounds every archive.
pub const ARCHIVE_RATIO_FLOOR: u64 = 32 * MIB;
/// Largest download accepted. (config)
pub const DOWNLOAD_MAX_BYTES: u64 = 512 * MIB;
/// How long to wait for the global lock, then `locked` (exit 5), or `concurrency_lost`
/// in the doctor.
pub const LOCK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// Snapshot retention: always at least 24 h; beyond that, the last 10 transactions
/// within 7 days per target. (config, except the 24 h minimum age)
pub const TX_RETENTION: TxRetention = TxRetention {
    min_age: DAY,
    keep_last: 10,
    window: Duration::from_secs(7 * 24 * 60 * 60),
};
/// Snapshot bytes per target beyond the 24 h protection window. (config)
pub const SNAPSHOT_MAX_BYTES: u64 = 2 * GIB;
/// Blob cache size; LRU with pins. (config)
pub const BLOB_CACHE_MAX_BYTES: u64 = GIB;
/// How long a stored plan stays applicable.
pub const PLAN_TTL: Duration = DAY;
/// Retention sweeps a stage dir or in-flight lock whose transaction is dead (lock free,
/// no journal) only once it is this old: a margin against clocks and races.
pub const ORPHAN_MIN_AGE: Duration = Duration::from_secs(60 * 60);
/// Longest `--reason`, in Unicode scalar values.
pub const REASON_MAX_CHARS: usize = 200;
/// `net.log` size before it rotates once, keeping one previous file.
pub const NET_LOG_MAX_BYTES: u64 = 10 * MIB;
/// Lua parser input cap (`errors`).
pub const SAVEDVARIABLES_MAX_BYTES: u64 = 64 * MIB;
/// Client rule: the client reads only the first 1024 characters of each TOC line.
pub const TOC_LINE_MAX: usize = 1024;
/// Time budget per doctor `--quick` / `health` detector.
pub const QUICK_DETECTOR_BUDGET: Duration = Duration::from_millis(5);
/// Most bytes discovery reads from one client metadata file (`.build.info`,
/// `.flavor.info`, `Config.wtf`, `realmlist.wtf`, …); the rest is ignored.
pub const DISCOVERY_FILE_MAX_BYTES: u64 = MIB;
/// How long a credential `command` may run before it is killed.
pub const CREDENTIAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// Most entries (and most candidate facts) the release memo keeps; past it, the oldest
/// go.
pub const RELEASE_MEMO_MAX_ENTRIES: usize = 4_096;
/// Most other releases a memo entry remembers beside the newest, for telling an
/// installed release that is ahead of the source's newest.
pub const MEMO_KNOWN_RELEASES: usize = 10;

/// Snapshot retention: keep everything younger than `min_age`; beyond that, keep the
/// last `keep_last` transactions per target that are younger than `window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TxRetention {
    pub min_age: Duration,
    pub keep_last: usize,
    pub window: Duration,
}

// ---------------------------------------------------------------------------------------
// The registry published by `capabilities.limits` (M4).

/// One named constant, for `capabilities.limits` and config overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LimitInfo {
    /// The constant's name, e.g. `DOWNLOAD_MAX_BYTES`.
    pub name: &'static str,
    pub value: LimitValue,
    /// `bytes`, `ms`, `count`, `ratio`, `chars`, `text`, `list` or `retention`.
    pub unit: &'static str,
    /// The `[limits]` key for "(config)" constants; `None` for every other.
    pub config_key: Option<&'static str>,
    pub notes: &'static str,
}

/// A constant's typed default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LimitValue {
    Count(u64),
    Bytes(u64),
    Millis(u64),
    Ratio(u64),
    Text(&'static str),
    List(&'static [&'static str]),
    Retention {
        min_age_ms: u64,
        keep_last: u64,
        window_ms: u64,
    },
}

/// Milliseconds of a constant duration. Every duration here is far below `u64::MAX` ms.
pub(crate) const fn ms(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

/// One registry entry; shared with `politeness::REGISTRY`.
pub(crate) const fn limit(
    name: &'static str,
    value: LimitValue,
    unit: &'static str,
    config_key: Option<&'static str>,
    notes: &'static str,
) -> LimitInfo {
    LimitInfo {
        name,
        value,
        unit,
        config_key,
        notes,
    }
}

/// Every safety limit, in table order.
pub const REGISTRY: &[LimitInfo] = &[
    limit(
        "ARCHIVE_MAX_ENTRIES",
        LimitValue::Count(ARCHIVE_MAX_ENTRIES),
        "count",
        Some("archive_max_entries"),
        "Most entries an archive may hold",
    ),
    limit(
        "ARCHIVE_MAX_UNCOMPRESSED",
        LimitValue::Bytes(ARCHIVE_MAX_UNCOMPRESSED),
        "bytes",
        Some("archive_max_uncompressed"),
        "Most bytes an archive may expand to",
    ),
    limit(
        "ARCHIVE_MAX_RATIO",
        LimitValue::Ratio(ARCHIVE_MAX_RATIO),
        "ratio",
        Some("archive_max_ratio"),
        "Zip-bomb guard: largest N of an N:1 compression ratio",
    ),
    limit(
        "ARCHIVE_RATIO_FLOOR",
        LimitValue::Bytes(ARCHIVE_RATIO_FLOOR),
        "bytes",
        None,
        "Expansion below which the compression ratio is not checked",
    ),
    limit(
        "DOWNLOAD_MAX_BYTES",
        LimitValue::Bytes(DOWNLOAD_MAX_BYTES),
        "bytes",
        Some("download_max_bytes"),
        "Largest download accepted",
    ),
    limit(
        "LOCK_ACQUIRE_TIMEOUT",
        LimitValue::Millis(ms(LOCK_ACQUIRE_TIMEOUT)),
        "ms",
        None,
        "Then locked (exit 5), or concurrency_lost in the doctor",
    ),
    limit(
        "TX_RETENTION",
        LimitValue::Retention {
            min_age_ms: ms(TX_RETENTION.min_age),
            keep_last: TX_RETENTION.keep_last as u64,
            window_ms: ms(TX_RETENTION.window),
        },
        "retention",
        Some("tx_retention"),
        "Always at least 24 h; beyond that, 10 transactions within 7 days per target",
    ),
    limit(
        "SNAPSHOT_MAX_BYTES",
        LimitValue::Bytes(SNAPSHOT_MAX_BYTES),
        "bytes",
        Some("snapshot_max_bytes"),
        "Per target, beyond the 24 h protection window",
    ),
    limit(
        "BLOB_CACHE_MAX_BYTES",
        LimitValue::Bytes(BLOB_CACHE_MAX_BYTES),
        "bytes",
        Some("blob_cache_max_bytes"),
        "LRU with pins",
    ),
    limit(
        "PLAN_TTL",
        LimitValue::Millis(ms(PLAN_TTL)),
        "ms",
        None,
        "Stored plans",
    ),
    limit(
        "ORPHAN_MIN_AGE",
        LimitValue::Millis(ms(ORPHAN_MIN_AGE)),
        "ms",
        None,
        "Orphan stage dirs and in-flight locks retention sweeps",
    ),
    limit(
        "REASON_MAX_CHARS",
        LimitValue::Count(REASON_MAX_CHARS as u64),
        "chars",
        None,
        "--reason, in Unicode scalar values",
    ),
    limit(
        "NET_LOG_MAX_BYTES",
        LimitValue::Bytes(NET_LOG_MAX_BYTES),
        "bytes",
        None,
        "Rotate once, keep one previous",
    ),
    limit(
        "SAVEDVARIABLES_MAX_BYTES",
        LimitValue::Bytes(SAVEDVARIABLES_MAX_BYTES),
        "bytes",
        None,
        "Lua parser input cap (errors)",
    ),
    limit(
        "TOC_LINE_MAX",
        LimitValue::Count(TOC_LINE_MAX as u64),
        "chars",
        None,
        "Client rule: only the first 1024 characters of each TOC line are read",
    ),
    limit(
        "QUICK_DETECTOR_BUDGET",
        LimitValue::Millis(ms(QUICK_DETECTOR_BUDGET)),
        "ms",
        None,
        "Doctor --quick / health, per detector",
    ),
    limit(
        "DISCOVERY_FILE_MAX_BYTES",
        LimitValue::Bytes(DISCOVERY_FILE_MAX_BYTES),
        "bytes",
        None,
        "Most bytes read from one client metadata file during discovery",
    ),
    limit(
        "CREDENTIAL_COMMAND_TIMEOUT",
        LimitValue::Millis(ms(CREDENTIAL_COMMAND_TIMEOUT)),
        "ms",
        None,
        "A credential command is killed after this long",
    ),
    limit(
        "RELEASE_MEMO_MAX_ENTRIES",
        LimitValue::Count(RELEASE_MEMO_MAX_ENTRIES as u64),
        "count",
        None,
        "Release memo entries and candidate facts; the oldest go first",
    ),
    limit(
        "MEMO_KNOWN_RELEASES",
        LimitValue::Count(MEMO_KNOWN_RELEASES as u64),
        "count",
        None,
        "Other releases a memo entry remembers beside the newest",
    ),
];

// ---------------------------------------------------------------------------------------
// Effective values after config overrides.

/// The config-overridable values, after `[limits]` overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EffectiveLimits {
    pub default_check_interval: Duration,
    pub archive_max_entries: u64,
    pub archive_max_uncompressed: u64,
    pub archive_max_ratio: u64,
    pub download_max_bytes: u64,
    pub tx_retention: TxRetention,
    pub snapshot_max_bytes: u64,
    pub blob_cache_max_bytes: u64,
}

impl Default for EffectiveLimits {
    fn default() -> Self {
        Self {
            default_check_interval: DEFAULT_CHECK_INTERVAL,
            archive_max_entries: ARCHIVE_MAX_ENTRIES,
            archive_max_uncompressed: ARCHIVE_MAX_UNCOMPRESSED,
            archive_max_ratio: ARCHIVE_MAX_RATIO,
            download_max_bytes: DOWNLOAD_MAX_BYTES,
            tx_retention: TX_RETENTION,
            snapshot_max_bytes: SNAPSHOT_MAX_BYTES,
            blob_cache_max_bytes: BLOB_CACHE_MAX_BYTES,
        }
    }
}

/// Why a `[limits]` entry was not applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LimitOverrideError {
    #[error("limits.{key} is not a known limit")]
    Unknown { key: String },
    #[error("limits.{key} is not overridable; only the (config) limits are")]
    NotOverridable { key: String },
    #[error("limits.{key} must be {expected}")]
    WrongType { key: String, expected: &'static str },
    #[error("limits.{key} must be greater than zero")]
    NotPositive { key: String },
}

impl EffectiveLimits {
    /// Applies one `[limits]` entry. Values must be positive. Byte, count and ratio
    /// limits take integers; `default_check_interval` takes a duration string such as
    /// `"6h"` or `"PT6H"`; `tx_retention` takes `{ keep_last = N, window_days = N }`
    /// (either key may be left out). Its 24 h minimum age is not overridable.
    pub fn apply(&mut self, key: &str, value: &toml::Value) -> Result<(), LimitOverrideError> {
        match key {
            "default_check_interval" => {
                self.default_check_interval = positive_duration(key, value)?;
            }
            "archive_max_entries" => self.archive_max_entries = positive_integer(key, value)?,
            "archive_max_uncompressed" => {
                self.archive_max_uncompressed = positive_integer(key, value)?;
            }
            "archive_max_ratio" => self.archive_max_ratio = positive_integer(key, value)?,
            "download_max_bytes" => self.download_max_bytes = positive_integer(key, value)?,
            "snapshot_max_bytes" => self.snapshot_max_bytes = positive_integer(key, value)?,
            "blob_cache_max_bytes" => self.blob_cache_max_bytes = positive_integer(key, value)?,
            "tx_retention" => self.tx_retention = retention(key, value, self.tx_retention)?,
            _ => {
                let known = REGISTRY
                    .iter()
                    .chain(crate::politeness::REGISTRY)
                    .any(|info| info.name.eq_ignore_ascii_case(key));
                return Err(if known {
                    LimitOverrideError::NotOverridable {
                        key: key.to_owned(),
                    }
                } else {
                    LimitOverrideError::Unknown {
                        key: key.to_owned(),
                    }
                });
            }
        }
        Ok(())
    }
}

fn positive_integer(key: &str, value: &toml::Value) -> Result<u64, LimitOverrideError> {
    let n = value
        .as_integer()
        .ok_or_else(|| LimitOverrideError::WrongType {
            key: key.to_owned(),
            expected: "an integer",
        })?;
    u64::try_from(n)
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| LimitOverrideError::NotPositive {
            key: key.to_owned(),
        })
}

fn positive_duration(key: &str, value: &toml::Value) -> Result<Duration, LimitOverrideError> {
    let wrong_type = || LimitOverrideError::WrongType {
        key: key.to_owned(),
        expected: "a duration string such as \"6h\"",
    };
    let parsed: jiff::SignedDuration = value
        .as_str()
        .ok_or_else(wrong_type)?
        .parse()
        .map_err(|_| wrong_type())?;
    Duration::try_from(parsed)
        .ok()
        .filter(|d| !d.is_zero())
        .ok_or_else(|| LimitOverrideError::NotPositive {
            key: key.to_owned(),
        })
}

fn retention(
    key: &str,
    value: &toml::Value,
    current: TxRetention,
) -> Result<TxRetention, LimitOverrideError> {
    let wrong_type = || LimitOverrideError::WrongType {
        key: key.to_owned(),
        expected: "a table with keep_last and/or window_days",
    };
    let table = value.as_table().ok_or_else(wrong_type)?;
    let mut retention = current;
    for (name, value) in table {
        let field = format!("{key}.{name}");
        match name.as_str() {
            "keep_last" => {
                retention.keep_last =
                    usize::try_from(positive_integer(&field, value)?).map_err(|_| wrong_type())?;
            }
            "window_days" => {
                let days = positive_integer(&field, value)?;
                retention.window = DAY
                    .checked_mul(u32::try_from(days).map_err(|_| wrong_type())?)
                    .ok_or_else(wrong_type)?;
            }
            _ => return Err(wrong_type()),
        }
    }
    Ok(retention)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::politeness;

    #[rustfmt::skip]
    const POLITENESS_NAMES: &[&str] = &[
        "USER_AGENT", "MAX_CONCURRENT_REQUESTS", "MAX_CONCURRENT_PER_HOST", "CONNECT_TIMEOUT",
        "API_READ_TIMEOUT", "DOWNLOAD_TIMEOUT", "MAX_RETRIES", "BACKOFF_BASE", "BACKOFF_MAX",
        "RETRY_AFTER_CAP", "SOURCE_RECHECK_MIN_INTERVAL", "DEFAULT_CHECK_INTERVAL",
        "WOWI_FILELIST_MIN_AGE", "GITHUB_REST_RESERVE", "CURSEFORGE_BATCH_SIZE",
        "WOWI_DETAILS_BATCH_SIZE", "DISCOVERY_MAX_DEPTH", "DISCOVERY_SKIP_DIRS",
        "DISCOVERY_MAX_DIRS", "MAX_REDIRECTS",
    ];

    #[rustfmt::skip]
    const LIMIT_NAMES: &[&str] = &[
        "ARCHIVE_MAX_ENTRIES", "ARCHIVE_MAX_UNCOMPRESSED", "ARCHIVE_MAX_RATIO",
        "ARCHIVE_RATIO_FLOOR", "DOWNLOAD_MAX_BYTES", "LOCK_ACQUIRE_TIMEOUT", "TX_RETENTION", "SNAPSHOT_MAX_BYTES",
        "BLOB_CACHE_MAX_BYTES", "PLAN_TTL", "ORPHAN_MIN_AGE", "REASON_MAX_CHARS", "NET_LOG_MAX_BYTES",
        "SAVEDVARIABLES_MAX_BYTES", "TOC_LINE_MAX", "QUICK_DETECTOR_BUDGET",
        "DISCOVERY_FILE_MAX_BYTES", "CREDENTIAL_COMMAND_TIMEOUT", "RELEASE_MEMO_MAX_ENTRIES",
        "MEMO_KNOWN_RELEASES",
    ];

    const CONFIG_NAMES: &[&str] = &[
        "DEFAULT_CHECK_INTERVAL",
        "ARCHIVE_MAX_ENTRIES",
        "ARCHIVE_MAX_UNCOMPRESSED",
        "ARCHIVE_MAX_RATIO",
        "DOWNLOAD_MAX_BYTES",
        "TX_RETENTION",
        "SNAPSHOT_MAX_BYTES",
        "BLOB_CACHE_MAX_BYTES",
    ];

    fn names(registry: &[LimitInfo]) -> Vec<&'static str> {
        registry.iter().map(|info| info.name).collect()
    }

    #[test]
    fn registries_list_every_constant_once() {
        assert_eq!(names(politeness::REGISTRY), POLITENESS_NAMES);
        assert_eq!(names(REGISTRY), LIMIT_NAMES);
        let mut all: Vec<&str> = POLITENESS_NAMES
            .iter()
            .chain(LIMIT_NAMES)
            .copied()
            .collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), POLITENESS_NAMES.len() + LIMIT_NAMES.len());
    }

    #[test]
    fn exactly_the_config_constants_have_config_keys() {
        for info in politeness::REGISTRY.iter().chain(REGISTRY) {
            let expected = CONFIG_NAMES.contains(&info.name);
            assert_eq!(info.config_key.is_some(), expected, "{}", info.name);
            if let Some(key) = info.config_key {
                assert_eq!(key, info.name.to_ascii_lowercase());
                // Every advertised key is accepted by EffectiveLimits::apply.
                let mut limits = EffectiveLimits::default();
                let err = limits.apply(key, &toml::Value::Boolean(true)).unwrap_err();
                assert!(
                    matches!(err, LimitOverrideError::WrongType { .. }),
                    "{key}: {err}"
                );
            }
            assert!(!info.notes.is_empty(), "{}", info.name);
        }
    }

    #[test]
    fn spot_check_values() {
        assert_eq!(ARCHIVE_MAX_UNCOMPRESSED, 1 << 30);
        assert_eq!(DOWNLOAD_MAX_BYTES, 512 << 20);
        assert_eq!(NET_LOG_MAX_BYTES, 10 << 20);
        assert_eq!(SAVEDVARIABLES_MAX_BYTES, 64 << 20);
        assert_eq!(SNAPSHOT_MAX_BYTES, 2 << 30);
        assert_eq!(BLOB_CACHE_MAX_BYTES, 1 << 30);
        assert_eq!(TX_RETENTION.min_age, Duration::from_secs(86_400));
        assert_eq!(TX_RETENTION.keep_last, 10);
        assert_eq!(TX_RETENTION.window, Duration::from_secs(7 * 86_400));
        assert_eq!(PLAN_TTL, Duration::from_secs(86_400));
        assert_eq!(QUICK_DETECTOR_BUDGET.as_millis(), 5);
        assert_eq!(TOC_LINE_MAX, 1024);
    }

    #[test]
    fn effective_limits_apply_one_value() {
        let mut limits = EffectiveLimits::default();
        limits
            .apply("download_max_bytes", &toml::Value::Integer(1000))
            .unwrap();
        assert_eq!(
            limits,
            EffectiveLimits {
                download_max_bytes: 1000,
                ..EffectiveLimits::default()
            }
        );

        limits
            .apply("default_check_interval", &toml::Value::String("90m".into()))
            .unwrap();
        assert_eq!(limits.default_check_interval, Duration::from_secs(90 * 60));

        let retention: toml::Value = toml::from_str("keep_last = 20\nwindow_days = 14").unwrap();
        limits.apply("tx_retention", &retention).unwrap();
        assert_eq!(
            limits.tx_retention,
            TxRetention {
                min_age: DAY,
                keep_last: 20,
                window: 14 * DAY,
            }
        );
    }

    #[test]
    fn effective_limits_reject_bad_overrides() {
        let int = |n: i64| toml::Value::Integer(n);
        let cases: Vec<(&str, toml::Value, LimitOverrideError)> = vec![
            (
                "lock_acquire_timeout",
                int(10),
                LimitOverrideError::NotOverridable {
                    key: "lock_acquire_timeout".into(),
                },
            ),
            (
                "max_retries",
                int(10),
                LimitOverrideError::NotOverridable {
                    key: "max_retries".into(),
                },
            ),
            (
                "no_such_limit",
                int(10),
                LimitOverrideError::Unknown {
                    key: "no_such_limit".into(),
                },
            ),
            (
                "archive_max_ratio",
                int(0),
                LimitOverrideError::NotPositive {
                    key: "archive_max_ratio".into(),
                },
            ),
            (
                "archive_max_entries",
                int(-5),
                LimitOverrideError::NotPositive {
                    key: "archive_max_entries".into(),
                },
            ),
            (
                "download_max_bytes",
                toml::Value::String("1GB".into()),
                LimitOverrideError::WrongType {
                    key: "download_max_bytes".into(),
                    expected: "an integer",
                },
            ),
            (
                "default_check_interval",
                toml::Value::String("0s".into()),
                LimitOverrideError::NotPositive {
                    key: "default_check_interval".into(),
                },
            ),
            (
                "default_check_interval",
                toml::Value::String("-1h".into()),
                LimitOverrideError::NotPositive {
                    key: "default_check_interval".into(),
                },
            ),
        ];
        for (key, value, expected) in cases {
            let mut limits = EffectiveLimits::default();
            assert_eq!(limits.apply(key, &value), Err(expected), "{key} = {value}");
            assert_eq!(limits, EffectiveLimits::default(), "{key} changed a value");
        }

        let mut limits = EffectiveLimits::default();
        for bad in ["min_age = 1", "keep_last = 0", "window_days = \"7\""] {
            let value: toml::Value = toml::from_str(bad).unwrap();
            assert!(limits.apply("tx_retention", &value).is_err(), "{bad}");
        }
        assert!(limits.apply("default_check_interval", &int(6)).is_err());
    }

    #[test]
    fn registries_serialize_deterministically() {
        let first = serde_json::to_string(&REGISTRY).unwrap();
        assert_eq!(first, serde_json::to_string(&REGISTRY).unwrap());
        assert!(
            first.contains(r#""config_key":"download_max_bytes""#),
            "{first}"
        );
        // The crate version is masked so a version bump does not churn the snapshot.
        let politeness: Vec<LimitInfo> = politeness::REGISTRY
            .iter()
            .map(|info| match info.value {
                LimitValue::Text(_) if info.name == "USER_AGENT" => LimitInfo {
                    value: LimitValue::Text("omaddons/<version>"),
                    ..*info
                },
                _ => *info,
            })
            .collect();
        insta::assert_json_snapshot!("limit_registries", (politeness, REGISTRY));
    }
}
