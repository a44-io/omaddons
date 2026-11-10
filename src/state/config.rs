//! `<config>/omaddons/config.toml` (vision §5.6): extra discovery roots, the default
//! target and channel, the running policy, `auto_backup`, the check interval, git
//! hosts, the config-overridable limits, per-source switches and credential
//! *references*, target overrides and client-rules overrides.
//!
//! One table, [`KEYS`], drives the `config` listing, `config set` validation and the
//! closest-key suggestions. Loading is typed (`toml`); a broken file fails commands
//! loudly with `state_corrupt` citing line and column, while unknown keys only warn.
//! `config set` edits with `toml_edit`, so comments and key order survive. Secrets
//! never live here: a credential is an environment variable name, a file path or an
//! argv run without a shell, resolved only when used and never printed.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use super::paths::Paths;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::limits::{
    BACKUP_AUTO_KEEP_DAYS_MAX, CREDENTIAL_COMMAND_TIMEOUT, CREDENTIAL_KILL_GRACE,
    CREDENTIAL_OUTPUT_MAX_BYTES, EffectiveLimits,
};
use crate::model::Channel;
use crate::rules::{ClientRulesOverride, Family, RulesOverrides, RulesTable};

// ---------------------------------------------------------------------------------------
// The model.

/// The configuration; every key is optional and defaults apply in code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Config {
    /// Extra discovery roots (absolute paths).
    pub extra_roots: Vec<PathBuf>,
    /// `install`'s target when `-t` is absent, and the client `search`'s install hints
    /// prefer: an id, key or any `-t` form.
    pub default_target: Option<String>,
    pub default_channel: Option<Channel>,
    pub running_policy: Option<RunningPolicy>,
    pub auto_backup: Option<AutoBackup>,
    /// Hours before `status` calls an update check stale (`updates.stale`), 0–48; 0
    /// keeps the default.
    pub check_interval_hours: Option<u32>,
    /// Extra https git hosts allowed for `git:` refs.
    pub git_hosts: Vec<String>,
    pub limits: LimitsConfig,
    pub sources: BTreeMap<String, SourceConfig>,
    /// Keyed by target id or key.
    pub targets: BTreeMap<String, TargetOverride>,
    /// Client-rules row patches by row name (shorthand for `[rules.client_rules]`).
    pub client_rules: BTreeMap<String, ClientRulesOverride>,
    /// Full rules-table overrides.
    pub rules: RulesOverrides,
}

/// What `running` does when a client runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunningPolicy {
    #[default]
    Safe,
    Refuse,
    Allow,
}

/// Automatic `WTF/` backups (M3-D30): a checkpoint before the day's first change and after
/// each play session that changed settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutoBackup {
    Off,
    /// Clones only: where the filesystem cannot clone files, none is made.
    #[default]
    Daily,
    /// Byte copies too, within `limits.backup_copy_max_mb`.
    #[serde(rename = "daily-copy")]
    DailyCopy,
}

/// `[limits]`: only the constants marked config-overridable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LimitsConfig {
    pub archive_max_entries: Option<u64>,
    pub archive_max_uncompressed: Option<Size>,
    pub archive_max_ratio: Option<u64>,
    pub download_max_bytes: Option<Size>,
    pub tx_retention_count: Option<u64>,
    pub tx_retention_days: Option<u64>,
    pub snapshot_max_bytes: Option<Size>,
    pub blob_cache_max_bytes: Option<Size>,
    /// MiB free beside a client's backups below which backups warn or skip,
    /// 0–[`BACKUP_MIN_FREE_MB_MAX`]; 0 turns the check off.
    pub backup_min_free_mb: Option<u64>,
    /// MiB of automatic byte-copy backups per target (`daily-copy`), 0–
    /// [`BACKUP_MIN_FREE_MB_MAX`]; 0 turns the budget off.
    pub backup_copy_max_mb: Option<u64>,
    /// Local days of automatic backups kept per target, 1–[`BACKUP_AUTO_KEEP_DAYS_MAX`].
    pub backup_auto_keep_days: Option<u64>,
}

/// The largest `limits.backup_min_free_mb`: 1 TiB.
pub const BACKUP_MIN_FREE_MB_MAX: u64 = 1_048_576;

/// A byte size: an integer, or a string with a `KiB`, `MiB` or `GiB` suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(into = "u64")]
pub struct Size(#[schemars(with = "u64")] pub u64);

impl From<Size> for u64 {
    fn from(size: Size) -> Self {
        size.0
    }
}

impl Size {
    /// `1073741824`, `"1GiB"`, `"512 MiB"`; `None` for anything else (`"12MB"` too).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (number, unit) = match text.find(|c: char| !c.is_ascii_digit()) {
            Some(i) => (text.get(..i)?, text.get(i..)?.trim()),
            None => (text, ""),
        };
        let n: u64 = number.parse().ok()?;
        let factor: u64 = match unit {
            "" => 1,
            "KiB" => 1 << 10,
            "MiB" => 1 << 20,
            "GiB" => 1 << 30,
            _ => return None,
        };
        n.checked_mul(factor).map(Self)
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(u64),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Int(n) => Ok(Self(n)),
            Raw::Text(t) => Self::parse(&t).ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "{t:?} is not a size: use bytes or a KiB, MiB or GiB suffix"
                ))
            }),
        }
    }
}

/// `[sources.<name>]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SourceConfig {
    pub enabled: Option<bool>,
    pub credential: Option<CredentialRef>,
}

/// Where a secret comes from: never the secret itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum CredentialRef {
    /// An environment variable.
    Env { env: String },
    /// A file, read and trimmed (it should be mode 0600).
    File { file: PathBuf },
    /// An argv run without a shell; its trimmed stdout is the secret.
    Command {
        #[serde(alias = "key_command")]
        command: Vec<String>,
    },
}

/// `[targets.<id or key>]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TargetOverride {
    pub label: Option<String>,
    /// Overrides a misdetected family (and so the client-rules row).
    pub family: Option<Family>,
    /// The name `status` and `targets` show (`display_name`), overriding the rule; an
    /// empty one restores it.
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------------------
// The key table.

/// What a key holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    PathList,
    Text,
    Channel,
    RunningPolicy,
    AutoBackup,
    Hours,
    HostList,
    Count,
    Size,
    /// MiB, where 0 turns a check off.
    Mebibytes,
    Bool,
    Credential,
    Family,
    /// A field of a client-rules row.
    RulesField,
}

impl KeyKind {
    /// Its name in the contract manifest (`config_type`): a name per kind, never the
    /// `accepts` prose, whose wording may be polished without a contract change.
    pub const fn name(self) -> &'static str {
        match self {
            Self::PathList => "path_list",
            Self::Text => "text",
            Self::Channel => "channel",
            Self::RunningPolicy => "running_policy",
            Self::AutoBackup => "auto_backup",
            Self::Hours => "hours",
            Self::HostList => "host_list",
            Self::Count => "count",
            Self::Size => "size",
            Self::Mebibytes => "mebibytes",
            Self::Bool => "bool",
            Self::Credential => "credential",
            Self::Family => "family",
            Self::RulesField => "rules_field",
        }
    }

    /// What `config set` accepts, for error messages.
    pub fn accepts(self) -> &'static str {
        match self {
            Self::PathList => "a TOML array of absolute paths, e.g. '[\"/games/wow\"]'",
            Self::Text => "a string",
            Self::Channel => "stable, beta or alpha",
            Self::RunningPolicy => "safe, refuse or allow",
            Self::AutoBackup => "off, daily (copy-on-write only) or daily-copy",
            Self::Hours => "an integer from 0 to 48",
            Self::HostList => "a TOML array of host names, e.g. '[\"codeberg.org\"]'",
            Self::Count => "a positive integer",
            Self::Size => "a positive number of bytes, or one with a KiB, MiB or GiB suffix",
            Self::Mebibytes => "a number of MiB from 0 to 1048576, where 0 turns the check off",
            Self::Bool => "true or false",
            Self::Credential => {
                "{ env = \"VAR\" }, { file = \"/path\" } or { command = [\"argv\", …] }"
            }
            Self::Family => "a family name such as mainline, forever or wrath-legacy",
            Self::RulesField => "a value of the client-rules field's type",
        }
    }
}

/// One documented key; `*` matches one segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    pub key: &'static str,
    pub kind: KeyKind,
    pub about: &'static str,
}

const fn spec(key: &'static str, kind: KeyKind, about: &'static str) -> KeySpec {
    KeySpec { key, kind, about }
}

/// Every key config.toml understands.
pub const KEYS: &[KeySpec] = &[
    spec("extra_roots", KeyKind::PathList, "Extra discovery roots"),
    spec(
        "default_target",
        KeyKind::Text,
        "install's target when -t is absent",
    ),
    spec(
        "default_channel",
        KeyKind::Channel,
        "The release channel for new installs",
    ),
    spec(
        "running_policy",
        KeyKind::RunningPolicy,
        "What to do while a client runs",
    ),
    spec("auto_backup", KeyKind::AutoBackup, "Automatic WTF backups"),
    spec(
        "check_interval_hours",
        KeyKind::Hours,
        "Hours before status calls an update check stale",
    ),
    spec(
        "git_hosts",
        KeyKind::HostList,
        "Extra https git hosts for git: refs",
    ),
    spec(
        "limits.archive_max_entries",
        KeyKind::Count,
        "Most entries an archive may hold",
    ),
    spec(
        "limits.archive_max_uncompressed",
        KeyKind::Size,
        "Most bytes an archive may expand to",
    ),
    spec(
        "limits.archive_max_ratio",
        KeyKind::Count,
        "Highest compression ratio accepted",
    ),
    spec(
        "limits.download_max_bytes",
        KeyKind::Size,
        "Largest download",
    ),
    spec(
        "limits.tx_retention_count",
        KeyKind::Count,
        "Snapshots kept per target in the window",
    ),
    spec(
        "limits.tx_retention_days",
        KeyKind::Count,
        "Days the retention window spans",
    ),
    spec(
        "limits.snapshot_max_bytes",
        KeyKind::Size,
        "Snapshot bytes per target beyond the 24-hour window",
    ),
    spec(
        "limits.blob_cache_max_bytes",
        KeyKind::Size,
        "Blob cache size cap",
    ),
    spec(
        "limits.backup_min_free_mb",
        KeyKind::Mebibytes,
        "MiB free beside backups below which they warn or skip",
    ),
    spec(
        "limits.backup_copy_max_mb",
        KeyKind::Mebibytes,
        "MiB of automatic byte-copy backups per target (daily-copy)",
    ),
    spec(
        "limits.backup_auto_keep_days",
        KeyKind::Count,
        "Days of automatic backups kept per target",
    ),
    spec(
        "sources.*.enabled",
        KeyKind::Bool,
        "Whether a source is used",
    ),
    spec(
        "sources.*.credential",
        KeyKind::Credential,
        "Where a source's secret comes from",
    ),
    spec("targets.*.label", KeyKind::Text, "A target's label"),
    spec(
        "targets.*.name",
        KeyKind::Text,
        "A target's display name, overriding the rule (empty restores it)",
    ),
    spec(
        "targets.*.family",
        KeyKind::Family,
        "A target's family, overriding detection",
    ),
    spec(
        "client_rules.*.*",
        KeyKind::RulesField,
        "A client-rules row field",
    ),
];

/// The fields of a client-rules row patch.
const RULES_FIELDS: &[&str] = &[
    "family",
    "install_kind",
    "version_min",
    "version_max",
    "game_types",
    "multi_toc",
    "comma_interface",
    "game_suffixes",
    "family_suffixes",
    "legacy_suffixes",
    "directive_conditionals",
    "interface_includes_patch",
    "legacy",
    "reload_rereads_toc",
    "interface_floor",
];

/// Each limit `config.toml` can override (by its registry name, `limits::REGISTRY`) and
/// the keys that override it, in [`KEYS`] order. The first key is the one `capabilities`
/// publishes as the limit's `config_key`: the one `omaddons config set` takes.
pub const LIMIT_CONFIG_KEYS: &[(&str, &[&str])] = &[
    ("DEFAULT_CHECK_INTERVAL", &["check_interval_hours"]),
    ("ARCHIVE_MAX_ENTRIES", &["limits.archive_max_entries"]),
    (
        "ARCHIVE_MAX_UNCOMPRESSED",
        &["limits.archive_max_uncompressed"],
    ),
    ("ARCHIVE_MAX_RATIO", &["limits.archive_max_ratio"]),
    ("DOWNLOAD_MAX_BYTES", &["limits.download_max_bytes"]),
    (
        "TX_RETENTION",
        &["limits.tx_retention_count", "limits.tx_retention_days"],
    ),
    ("SNAPSHOT_MAX_BYTES", &["limits.snapshot_max_bytes"]),
    ("BLOB_CACHE_MAX_BYTES", &["limits.blob_cache_max_bytes"]),
    ("BACKUP_MIN_FREE", &["limits.backup_min_free_mb"]),
    ("BACKUP_COPY_MAX", &["limits.backup_copy_max_mb"]),
    ("BACKUP_AUTO_KEEP_DAYS", &["limits.backup_auto_keep_days"]),
];

impl Config {
    /// Whether `config.toml` sets `key`, one of [`LIMIT_CONFIG_KEYS`]'s keys
    /// (`check_interval_hours` counts only above 0, which keeps the default).
    pub fn sets_limit_key(&self, key: &str) -> bool {
        let l = &self.limits;
        match key {
            "check_interval_hours" => self.check_interval_hours.is_some_and(|h| h > 0),
            "limits.archive_max_entries" => l.archive_max_entries.is_some(),
            "limits.archive_max_uncompressed" => l.archive_max_uncompressed.is_some(),
            "limits.archive_max_ratio" => l.archive_max_ratio.is_some(),
            "limits.download_max_bytes" => l.download_max_bytes.is_some(),
            "limits.tx_retention_count" => l.tx_retention_count.is_some(),
            "limits.tx_retention_days" => l.tx_retention_days.is_some(),
            "limits.snapshot_max_bytes" => l.snapshot_max_bytes.is_some(),
            "limits.blob_cache_max_bytes" => l.blob_cache_max_bytes.is_some(),
            "limits.backup_min_free_mb" => l.backup_min_free_mb.is_some(),
            "limits.backup_copy_max_mb" => l.backup_copy_max_mb.is_some(),
            "limits.backup_auto_keep_days" => l.backup_auto_keep_days.is_some(),
            _ => false,
        }
    }
}

/// The spec a dotted key matches.
pub fn spec_for(key: &str) -> Option<&'static KeySpec> {
    let parts: Vec<&str> = key.split('.').collect();
    KEYS.iter().find(|spec| {
        let pattern: Vec<&str> = spec.key.split('.').collect();
        pattern.len() == parts.len()
            // ubs:ignore — comparing config key segments, not secrets
            && pattern.iter().zip(&parts).all(|(p, k)| *p == "*" || p == k)
            && (spec.kind != KeyKind::RulesField
                || parts.last().is_some_and(|f| RULES_FIELDS.contains(f)))
    })
}

/// The closest documented key within edit distance 2, filling `*` segments from `key`.
pub fn closest_key(key: &str) -> Option<String> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut candidates: Vec<String> = Vec::new();
    for spec in KEYS {
        let pattern: Vec<&str> = spec.key.split('.').collect();
        if pattern.len() != parts.len() {
            continue;
        }
        if spec.kind == KeyKind::RulesField {
            for field in RULES_FIELDS {
                let mut c: Vec<&str> = pattern[..pattern.len() - 1]
                    .iter()
                    .zip(&parts)
                    // ubs:ignore — filling config key segments, not comparing secrets
                    .map(|(p, k)| if *p == "*" { *k } else { *p })
                    .collect();
                c.push(field);
                candidates.push(c.join("."));
            }
            continue;
        }
        let filled: Vec<&str> = pattern
            .iter()
            .zip(&parts)
            // ubs:ignore — filling config key segments, not comparing secrets
            .map(|(p, k)| if *p == "*" { *k } else { *p })
            .collect();
        candidates.push(filled.join("."));
    }
    candidates
        .into_iter()
        .map(|c| (crate::cli::levenshtein(key, &c), c))
        .filter(|(d, _)| *d <= 2)
        .min()
        .map(|(_, c)| c)
}

// ---------------------------------------------------------------------------------------
// Loading.

/// The configuration of this invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedConfig {
    pub config: Config,
    /// `config_unknown_key` warnings.
    pub warnings: Vec<Warning>,
    pub path: PathBuf,
    pub exists: bool,
    /// The file's text, when it exists.
    pub text: Option<String>,
}

impl Config {
    /// Loads `<config>/omaddons/config.toml`; a missing file is the default config. A
    /// broken file is `state_corrupt` (exit 1) citing line and column.
    pub fn load(paths: &Paths) -> Result<LoadedConfig> {
        let path = paths.config_file();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LoadedConfig {
                    path,
                    ..LoadedConfig::default()
                });
            }
            Err(err) => return Err(Error::io(&path, &err, edit_fix(&path))),
        };
        let (config, warnings) = Self::parse(&text, &path)?;
        Ok(LoadedConfig {
            config,
            warnings,
            path,
            exists: true,
            text: Some(text),
        })
    }

    /// Parses and validates config text; `path` is for messages. A broken file is
    /// `state_corrupt` citing line and column ([`ConfigProblem`]'s error).
    pub fn parse(text: &str, path: &Path) -> Result<(Self, Vec<Warning>)> {
        Self::parse_checked(text, path).map_err(ConfigProblem::into_error)
    }

    /// `config.toml` as `capabilities` reads it (M4-D4): never an error. A missing file is
    /// the default config; a file that cannot be read, or does not parse or validate, is
    /// the default config and the problem.
    pub fn load_tolerant(paths: &Paths) -> TolerantConfig {
        let path = paths.config_file();
        let failed = |problem: ConfigProblem| TolerantConfig {
            loaded: LoadedConfig {
                path: path.clone(),
                ..LoadedConfig::default()
            },
            problem: Some(problem),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return TolerantConfig {
                    loaded: LoadedConfig {
                        path,
                        ..LoadedConfig::default()
                    },
                    problem: None,
                };
            }
            Err(err) => {
                return failed(ConfigProblem::unlocated(
                    Some(path.clone()),
                    err.to_string(),
                ));
            }
        };
        match Self::parse_checked(&text, &path) {
            Ok((config, warnings)) => TolerantConfig {
                loaded: LoadedConfig {
                    config,
                    warnings,
                    path,
                    exists: true,
                    text: Some(text),
                },
                problem: None,
            },
            Err(problem) => failed(problem),
        }
    }

    /// [`Config::parse`], its failure typed rather than an error.
    pub fn parse_checked(
        text: &str,
        path: &Path,
    ) -> std::result::Result<(Self, Vec<Warning>), ConfigProblem> {
        let raw: toml::Table = toml::from_str(text).map_err(|e| corrupt(path, text, &e))?;
        let warnings = unknown_keys(&raw, path);
        let config: Self = toml::from_str(text).map_err(|e| corrupt(path, text, &e))?;
        if let Some(hours) = config.check_interval_hours
            && hours > 48
        {
            return Err(invalid(
                path,
                text,
                "check_interval_hours",
                "must be from 0 to 48",
            ));
        }
        for (key, mb) in [
            (
                "limits.backup_min_free_mb",
                config.limits.backup_min_free_mb,
            ),
            (
                "limits.backup_copy_max_mb",
                config.limits.backup_copy_max_mb,
            ),
        ] {
            if mb.is_some_and(|mb| mb > BACKUP_MIN_FREE_MB_MAX) {
                return Err(invalid(
                    path,
                    text,
                    key,
                    &format!("must be from 0 to {BACKUP_MIN_FREE_MB_MAX}"),
                ));
            }
        }
        if config
            .limits
            .backup_auto_keep_days
            .is_some_and(|days| days == 0 || days > BACKUP_AUTO_KEEP_DAYS_MAX as u64)
        {
            return Err(invalid(
                path,
                text,
                "limits.backup_auto_keep_days",
                &format!("must be from 1 to {BACKUP_AUTO_KEEP_DAYS_MAX}"),
            ));
        }
        if let Some(bad) = config.git_hosts.iter().find(|h| !is_host_name(h)) {
            return Err(invalid(
                path,
                text,
                "git_hosts",
                &format!("{bad:?} is not a bare host name (no scheme, port or path)"),
            ));
        }
        // D13: HTTP and the git binary share no host, optional sources' hosts included
        // whether enabled or not.
        if let Some(http) = config
            .git_hosts
            .iter()
            .find(|h| crate::net::allowlist::is_http_host(h))
        {
            return Err(invalid(
                path,
                text,
                "git_hosts",
                &format!(
                    "{http:?} is a host omaddons reaches over HTTPS only, never with the git binary: remove it from git_hosts"
                ),
            ));
        }
        config
            .effective_limits()
            .map_err(|(key, message)| invalid(path, text, &key, &message))?;
        config
            .rules_table(&RulesTable::builtin())
            .map_err(|message| invalid(path, text, "client_rules", &message))?;
        Ok((config, warnings))
    }

    /// The limits after `[limits]` and `check_interval_hours`; `(key, message)` when a
    /// value is out of range.
    pub fn effective_limits(&self) -> std::result::Result<EffectiveLimits, (String, String)> {
        let mut limits = EffectiveLimits::default();
        // `shown` is the config key a message names; `limit` the one `apply` takes.
        let mut apply = |shown: &str, limit: &str, value: toml::Value| {
            limits
                .apply(limit, &value)
                .map_err(|e| (shown.to_owned(), limit_message(&e)))
        };
        let int = |n: u64| toml::Value::Integer(i64::try_from(n).unwrap_or(i64::MAX));
        let l = &self.limits;
        let sizes = [
            ("archive_max_uncompressed", l.archive_max_uncompressed),
            ("download_max_bytes", l.download_max_bytes),
            ("snapshot_max_bytes", l.snapshot_max_bytes),
            ("blob_cache_max_bytes", l.blob_cache_max_bytes),
        ];
        for (key, size) in sizes {
            if let Some(size) = size {
                apply(&format!("limits.{key}"), key, int(size.0))?;
            }
        }
        for (key, n) in [
            ("archive_max_entries", l.archive_max_entries),
            ("archive_max_ratio", l.archive_max_ratio),
        ] {
            if let Some(n) = n {
                apply(&format!("limits.{key}"), key, int(n))?;
            }
        }
        // One field at a time, so an error names the key written: `apply` starts each
        // from the retention so far.
        for (key, field, n) in [
            ("tx_retention_count", "keep_last", l.tx_retention_count),
            ("tx_retention_days", "window_days", l.tx_retention_days),
        ] {
            if let Some(n) = n {
                let table = toml::Table::from_iter([(field.to_owned(), int(n))]);
                apply(
                    &format!("limits.{key}"),
                    "tx_retention",
                    toml::Value::Table(table),
                )?;
            }
        }
        if let Some(mb) = l.backup_min_free_mb {
            apply(
                "limits.backup_min_free_mb",
                "backup_min_free",
                int(mb.saturating_mul(1 << 20)),
            )?;
        }
        if let Some(mb) = l.backup_copy_max_mb {
            apply(
                "limits.backup_copy_max_mb",
                "backup_copy_max",
                int(mb.saturating_mul(1 << 20)),
            )?;
        }
        if let Some(days) = l.backup_auto_keep_days {
            apply(
                "limits.backup_auto_keep_days",
                "backup_auto_keep_days",
                int(days),
            )?;
        }
        if let Some(hours) = self.check_interval_hours.filter(|h| *h > 0) {
            apply(
                "check_interval_hours",
                "default_check_interval",
                toml::Value::String(format!("{hours}h")),
            )?;
        }
        Ok(limits)
    }

    /// `table` with `[rules]` and `[client_rules.*]` applied.
    pub fn rules_table(&self, table: &RulesTable) -> std::result::Result<RulesTable, String> {
        let mut overrides = self.rules.clone();
        for (row, patch) in &self.client_rules {
            overrides
                .client_rules
                .entry(row.clone())
                .or_insert_with(|| patch.clone());
        }
        if overrides == RulesOverrides::default() {
            return Ok(table.clone());
        }
        table.with_overrides(&overrides).map_err(|e| e.to_string())
    }
}

/// `config_unknown_key` for every key [`KEYS`] does not describe.
fn unknown_keys(raw: &toml::Table, path: &Path) -> Vec<Warning> {
    let mut unknown = Vec::new();
    for (key, value) in raw {
        match (key.as_str(), value) {
            ("rules", toml::Value::Table(rules)) => unknown_rules_keys(rules, &mut unknown),
            ("rules", _) => {}
            ("limits" | "sources" | "targets" | "client_rules", toml::Value::Table(t)) => {
                for (sub, inner) in t {
                    match (key.as_str(), inner) {
                        ("limits", _) => check(&format!("limits.{sub}"), &mut unknown),
                        (_, toml::Value::Table(fields)) => {
                            for field in fields.keys() {
                                check(&format!("{key}.{sub}.{field}"), &mut unknown);
                            }
                        }
                        _ => check(&format!("{key}.{sub}"), &mut unknown),
                    }
                }
            }
            _ => check(key, &mut unknown),
        }
    }
    unknown
        .into_iter()
        .map(|(key, closest)| {
            let hint = closest
                .map(|c| format!("; did you mean {c}?"))
                .unwrap_or_default();
            Warning::new(
                WarningCode::ConfigUnknownKey,
                format!("{} has an unknown key {key}{hint}", path.display()),
            )
            .with_fix(Suggestion::command(["omaddons", "config"]))
        })
        .collect()
}

/// An unknown key, and the documented key it likely meant.
type Unknown = (String, Option<String>);

fn check(key: &str, unknown: &mut Vec<Unknown>) {
    if spec_for(key).is_none() {
        unknown.push((key.to_owned(), closest_key(key)));
    }
}

/// The tables of `[rules]` (`config get`/`set` do not reach them, so they are not in
/// [`KEYS`]); each list equals its struct's fields (a test reads their schemas).
const RULES_TABLES: &[&str] = &[
    "products",
    "version_families",
    "family_ranges",
    "client_rules",
];
const PRODUCT_FIELDS: &[&str] = &["code", "folder", "exe", "kind", "default_id", "modes"];
const MODE_FIELDS: &[&str] = &["id", "name", "aliases"];
const VERSION_RULE_FIELDS: &[&str] = &["install_kind", "major", "minor", "family", "interface"];
const RANGE_FIELDS: &[&str] = &["min", "max"];

/// The unknown keys anywhere in `[rules]`, array elements by index
/// (`rules.products[0].modes[1].nmae`). A value of the wrong type is the parse's error.
fn unknown_rules_keys(rules: &toml::Table, unknown: &mut Vec<Unknown>) {
    let tables = |table: &toml::Table, key: &str| -> Vec<toml::Table> {
        match table.get(key) {
            Some(toml::Value::Array(items)) => {
                items.iter().filter_map(|v| v.as_table().cloned()).collect()
            }
            _ => Vec::new(),
        }
    };
    unknown_fields(rules, "rules", RULES_TABLES, unknown);
    for (i, product) in tables(rules, "products").iter().enumerate() {
        let at = format!("rules.products[{i}]");
        unknown_fields(product, &at, PRODUCT_FIELDS, unknown);
        for (j, mode) in tables(product, "modes").iter().enumerate() {
            unknown_fields(mode, &format!("{at}.modes[{j}]"), MODE_FIELDS, unknown);
        }
    }
    for (i, rule) in tables(rules, "version_families").iter().enumerate() {
        let at = format!("rules.version_families[{i}]");
        unknown_fields(rule, &at, VERSION_RULE_FIELDS, unknown);
        for part in ["major", "minor"] {
            if let Some(range) = rule.get(part).and_then(toml::Value::as_table) {
                unknown_fields(range, &format!("{at}.{part}"), RANGE_FIELDS, unknown);
            }
        }
    }
    let named = |key: &str| rules.get(key).and_then(toml::Value::as_table);
    for (family, range) in named("family_ranges").into_iter().flatten() {
        if let Some(range) = range.as_table() {
            let at = format!("rules.family_ranges.{family}");
            unknown_fields(range, &at, RANGE_FIELDS, unknown);
        }
    }
    for (row, fields) in named("client_rules").into_iter().flatten() {
        if let Some(fields) = fields.as_table() {
            let at = format!("rules.client_rules.{row}");
            unknown_fields(fields, &at, RULES_FIELDS, unknown);
        }
    }
}

/// `at.<key>` for each key of `table` not in `fields`, with the field within edit
/// distance 2 it likely meant.
fn unknown_fields(table: &toml::Table, at: &str, fields: &[&str], unknown: &mut Vec<Unknown>) {
    for key in table.keys().filter(|k| !fields.contains(&k.as_str())) {
        let closest = fields
            .iter()
            .map(|f| (crate::cli::levenshtein(key, f), *f))
            .filter(|(d, _)| *d <= 2)
            .min()
            .map(|(_, f)| format!("{at}.{f}"));
        unknown.push((format!("{at}.{key}"), closest));
    }
}

/// A DNS host name: dot-separated labels of ASCII letters, digits and inner hyphens.
pub fn is_host_name(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

fn edit_fix(path: &Path) -> Suggestion {
    Suggestion::text(format!("edit {}", path.display()))
}

/// Why config.toml could not be used: what `capabilities` reports as `config_error`
/// instead of failing (M4-D4), and what every other command fails with as
/// `state_corrupt` ([`ConfigProblem::into_error`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProblem {
    /// The file; `None` when HOME gives no config path at all.
    pub path: Option<PathBuf>,
    /// 1-based; `None` when the file could not be read at all.
    pub line: Option<u64>,
    pub column: Option<u64>,
    /// What is wrong, without the location: the TOML parser's message, `<key> <rule>`, or
    /// the OS error.
    pub message: String,
    /// The key a value out of range is under, which the error's `details` names.
    key: Option<String>,
}

/// config.toml as `capabilities` reads it: never an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TolerantConfig {
    /// The default config when there is a problem.
    pub loaded: LoadedConfig,
    pub problem: Option<ConfigProblem>,
}

#[cfg(test)]
impl TolerantConfig {
    /// What a missing file loads as (the default config, no problem), at a fixed path.
    pub fn default_for_tests() -> Self {
        Self {
            loaded: LoadedConfig {
                path: PathBuf::from("/x/config.toml"),
                ..LoadedConfig::default()
            },
            problem: None,
        }
    }
}

impl ConfigProblem {
    /// A problem with no line or column: a file that could not be read at all, or (with
    /// `path` `None`) no config path, because HOME gives none.
    pub fn unlocated(path: Option<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            path,
            line: None,
            column: None,
            message: message.into(),
            key: None,
        }
    }

    /// The `state_corrupt` error every command but `capabilities` fails with for a file
    /// that does not parse or validate, byte for byte as before the type existed. (A
    /// file that cannot be read stays [`Config::load`]'s `io`.)
    fn into_error(self) -> Error {
        let path = self.path.unwrap_or_default();
        let (line, column) = (self.line.unwrap_or(1), self.column.unwrap_or(1));
        match self.key {
            Some(key) => Error::new(
                ErrorCode::StateCorrupt,
                format!("{} line {line}: {}", path.display(), self.message),
                edit_fix(&path),
            )
            .details(
                serde_json::json!({ "path": path, "line": line, "column": 1, "toml_key": key }),
            ),
            None => Error::new(
                ErrorCode::StateCorrupt,
                format!(
                    "{} line {line}, column {column}: {}",
                    path.display(),
                    self.message
                ),
                edit_fix(&path),
            )
            .details(serde_json::json!({ "path": path, "line": line, "column": column })),
        }
    }
}

/// A TOML or type error, citing line and column.
fn corrupt(path: &Path, text: &str, err: &toml::de::Error) -> ConfigProblem {
    let (line, column) = err
        .span()
        .map_or((1, 1), |span| line_column(text, span.start));
    ConfigProblem {
        path: Some(path.to_path_buf()),
        line: Some(line as u64),
        column: Some(column as u64),
        message: err.message().trim().to_owned(),
        key: None,
    }
}

/// `state_corrupt` for a value out of range, citing the key's line.
/// A `[limits]` override error without its leading `limits.<key>`: [`invalid`] names
/// the config key itself.
fn limit_message(err: &crate::limits::LimitOverrideError) -> String {
    use crate::limits::LimitOverrideError as E;
    let (E::Unknown { key }
    | E::NotOverridable { key }
    | E::WrongType { key, .. }
    | E::NotPositive { key }) = err;
    let text = err.to_string();
    text.strip_prefix(&format!("limits.{key} "))
        .map_or_else(|| text.clone(), str::to_owned)
}

/// A value out of range, citing the key's line.
fn invalid(path: &Path, text: &str, key: &str, message: &str) -> ConfigProblem {
    let leaf = key.rsplit('.').next().unwrap_or(key);
    let line = text
        .lines()
        .position(|l| l.trim_start().starts_with(leaf))
        .map_or(1, |i| i + 1);
    ConfigProblem {
        path: Some(path.to_path_buf()),
        line: Some(line as u64),
        column: Some(1),
        message: format!("{key} {message}"),
        key: Some(key.to_owned()),
    }
}

/// 1-based line and column of a byte offset.
fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, column)
}

// ---------------------------------------------------------------------------------------
// Editing.

/// `text` with `key` set to `raw`, comments and order kept elsewhere; `usage` when the
/// key is unknown or the value invalid.
pub fn set_value(text: &str, path: &Path, key: &str, raw: &str) -> Result<String> {
    let Some(spec) = spec_for(key) else {
        let fix = closest_key(key).map_or_else(
            || Suggestion::command(["omaddons", "config"]),
            |c| {
                Suggestion::command([
                    "omaddons".to_owned(),
                    "config".into(),
                    "set".into(),
                    c,
                    raw.to_owned(),
                ])
            },
        );
        return Err(Error::new(
            ErrorCode::Usage,
            format!("{key} is not a config key"),
            fix,
        ));
    };
    let bad_value = |why: &str| {
        Error::new(
            ErrorCode::Usage,
            format!(
                "{raw:?} is not valid for {key}: {why}; it takes {}",
                spec.kind.accepts()
            ),
            Suggestion::command(["omaddons", "config", "get", key]),
        )
    };
    let value = toml_value(raw);
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|_| {
        Error::new(
            ErrorCode::StateCorrupt,
            format!("{} is not valid TOML", path.display()),
            edit_fix(path),
        )
    })?;
    let segments: Vec<&str> = key.split('.').collect();
    let Some((leaf, tables)) = segments.split_last() else {
        return Err(bad_value("empty key"));
    };
    let mut item = doc.as_item_mut();
    for segment in tables {
        let table = item
            .as_table_like_mut()
            .ok_or_else(|| bad_value("a parent of the key is not a table"))?;
        if table.get(segment).is_none() {
            let mut fresh = toml_edit::Table::new();
            fresh.set_implicit(true);
            table.insert(segment, toml_edit::Item::Table(fresh));
        }
        item = table
            .get_mut(segment)
            .ok_or_else(|| bad_value("a parent of the key is missing"))?;
    }
    let table = item
        .as_table_like_mut()
        .ok_or_else(|| bad_value("a parent of the key is not a table"))?;
    match table.get_mut(leaf) {
        // Replace only the value: the key's comments and the value's spacing and
        // trailing comment stay.
        Some(toml_edit::Item::Value(existing)) => {
            let decor = existing.decor().clone();
            *existing = value;
            *existing.decor_mut() = decor;
        }
        Some(other) => *other = toml_edit::Item::Value(value),
        None => {
            table.insert(leaf, toml_edit::Item::Value(value));
        }
    }
    // A file of only comments keeps them in the document's trailing text; keep them
    // above the first key rather than letting them slide below it.
    let trailing = doc.trailing().as_str().unwrap_or_default().to_owned();
    if doc.len() == 1 && tables.is_empty() && !trailing.trim().is_empty() {
        if let Some(mut key) = doc.as_table_mut().key_mut(leaf) {
            key.leaf_decor_mut().set_prefix(trailing);
        }
        doc.set_trailing("");
    }
    let updated = doc.to_string();
    Config::parse(&updated, path).map_err(|e| bad_value(&e.message))?;
    Ok(updated)
}

/// The TOML value `raw` denotes: a TOML literal when it parses as one, else a string.
fn toml_value(raw: &str) -> toml_edit::Value {
    let literal = format!("v = {raw}");
    literal
        .parse::<toml_edit::DocumentMut>()
        .ok()
        .and_then(|doc| doc.get("v").and_then(|v| v.as_value().cloned()))
        .map(|mut v| {
            v.decor_mut().clear();
            v
        })
        .unwrap_or_else(|| toml_edit::Value::from(raw))
}

/// The value at a dotted key in a raw table, if set.
pub fn lookup<'a>(raw: &'a toml::Table, key: &str) -> Option<&'a toml::Value> {
    let mut parts = key.split('.');
    let first = raw.get(parts.next()?)?;
    parts.try_fold(first, |value, part| value.as_table()?.get(part))
}

// ---------------------------------------------------------------------------------------
// Credentials.

/// A secret: never printed, logged or serialized.
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Whether group or others hold any permission on a credential file of `mode`: the
/// `credential_file_mode` warning and the doctor's `fm-secrets-credential-file-mode` both
/// ask it, so they never disagree.
pub const fn credential_exposed(mode: u32) -> bool {
    mode & 0o077 != 0
}

/// Whether a credential reference names a credential that is there, found without reading
/// a secret or running anything (M4-D4: `capabilities` says a credential is configured,
/// never that it works): a set, non-empty variable; a non-empty regular file, never
/// opened (one of mode 000 counts); a command, never run.
pub fn credential_present(credential: &CredentialRef) -> bool {
    credential_present_with(credential, &|name| std::env::var_os(name))
}

/// [`credential_present`] with the environment looked up through `var`.
pub fn credential_present_with(
    credential: &CredentialRef,
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> bool {
    match credential {
        CredentialRef::Env { env } => var(env).is_some_and(|value| !value.is_empty()),
        CredentialRef::File { file } => non_empty_file(file),
        CredentialRef::Command { .. } => true,
    }
}

/// A regular file of non-zero length at `path`, by its metadata alone.
pub fn non_empty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

/// Resolves a credential reference; a readable-by-others file adds a warning.
pub fn resolve_credential(credential: &CredentialRef) -> Result<(Secret, Vec<Warning>)> {
    match credential {
        CredentialRef::Env { env } => std::env::var(env)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|v| (Secret(v.trim().to_owned()), Vec::new()))
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::AuthRequired,
                    format!("the environment variable {env} is not set"),
                    Suggestion::text(format!("set {env}")),
                )
            }),
        CredentialRef::File { file } => {
            let text = std::fs::read_to_string(file).map_err(|err| {
                Error::new(
                    ErrorCode::AuthRequired,
                    format!("cannot read the credential file {}: {err}", file.display()),
                    Suggestion::text(format!("create {} with mode 0600", file.display())),
                )
            })?;
            let mut warnings = Vec::new();
            if let Ok(meta) = std::fs::metadata(file) {
                use std::os::unix::fs::PermissionsExt;
                if credential_exposed(meta.permissions().mode()) {
                    warnings.push(
                        Warning::new(
                            WarningCode::CredentialFileMode,
                            format!("{} is readable by other users", file.display()),
                        )
                        .with_fix(Suggestion::text(format!("chmod 600 {}", file.display()))),
                    );
                }
            }
            Ok((Secret(text.trim().to_owned()), warnings))
        }
        CredentialRef::Command { command } => {
            run_credential_command(command).map(|s| (s, Vec::new()))
        }
    }
}

/// Runs `argv` without a shell, stdin closed, stderr discarded, in a process group of
/// its own; its trimmed stdout is the secret. One deadline,
/// [`CREDENTIAL_COMMAND_TIMEOUT`], covers both its exit and the end of its output, so a
/// process it left holding the output cannot keep omaddons waiting. Past that deadline,
/// or past [`CREDENTIAL_OUTPUT_MAX_BYTES`] of output, the whole group is stopped and the
/// credential refused. A command that succeeds is never stopped: a helper it started
/// with its output elsewhere keeps running.
fn run_credential_command(argv: &[String]) -> Result<Secret> {
    run_credential_command_for(argv, CREDENTIAL_COMMAND_TIMEOUT)
}

/// [`run_credential_command`] with its deadline `timeout` after the start.
fn run_credential_command_for(argv: &[String], timeout: Duration) -> Result<Secret> {
    use std::os::unix::process::CommandExt;
    use std::sync::mpsc::{TryRecvError, channel};
    let rejected = |message: String| {
        Error::new(
            ErrorCode::AuthRejected,
            message,
            Suggestion::text("check the credential command in config.toml"),
        )
    };
    let Some((program, args)) = argv.split_first() else {
        return Err(rejected("the credential command is empty".into()));
    };
    // The user's own config.toml names this argv (credentials by reference); it runs
    // without a shell, with stdin closed and stderr discarded, in its own process group,
    // so stopping it stops what it started too.
    let mut command = Command::new(program); // ubs:ignore — user-configured credential argv
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|err| {
            rejected(format!(
                "cannot run the credential command {program}: {err}"
            ))
        })?;
    // ubs:ignore — a timeout clock, not randomness
    let deadline = Instant::now() + timeout;
    let Some(mut stdout) = child.stdout.take() else {
        stop_credential_command(&mut child);
        return Err(rejected("the credential command has no stdout".into()));
    };
    let (sender, output) = channel();
    std::thread::spawn(move || {
        // A seam (DEC-7): a panic here is recovered below (the channel closes with no
        // output), so it must print nothing.
        let _ = crate::fsx::failpoint::hit("config.credential.reader");
        let mut out = Vec::new();
        // One byte past the cap tells an output over it from one exactly at it.
        let _ = (&mut stdout)
            .take(CREDENTIAL_OUTPUT_MAX_BYTES + 1)
            .read_to_end(&mut out);
        let _ = sender.send(out);
    });
    // The output ends (EOF, or the cap) and the command exits, in either order; neither
    // waits past the deadline. The exit is observed without reaping the command, so its
    // process group's id stays its own until the group is stopped.
    let mut out = None;
    let mut exited = false;
    loop {
        if out.is_none() {
            match output.try_recv() {
                Ok(bytes) => out = Some(bytes),
                // The reader panicked (the seam): no output.
                Err(TryRecvError::Disconnected) => out = Some(Vec::new()),
                Err(TryRecvError::Empty) => {}
            }
        }
        if out
            .as_ref()
            .is_some_and(|bytes: &Vec<u8>| bytes.len() as u64 > CREDENTIAL_OUTPUT_MAX_BYTES)
        {
            stop_credential_command(&mut child);
            return Err(rejected(format!(
                "the credential command {program} printed more than {CREDENTIAL_OUTPUT_MAX_BYTES} bytes; a token is one short line"
            )));
        }
        if !exited {
            exited = match has_exited(&child) {
                Ok(exited) => exited,
                Err(err) => {
                    stop_credential_command(&mut child);
                    return Err(rejected(format!(
                        "waiting for the credential command {program} failed: {err}"
                    )));
                }
            };
        }
        if exited && out.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            stop_credential_command(&mut child);
            let seconds = timeout.as_secs_f64();
            return Err(rejected(if exited {
                format!(
                    "the credential command {program} exited, but a process it started still held its output after {seconds} s; it was stopped (start that process with its output redirected, such as to /dev/null)"
                )
            } else {
                format!("the credential command {program} timed out after {seconds} s")
            }));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().map_err(|err| {
        rejected(format!(
            "waiting for the credential command {program} failed: {err}"
        ))
    })?;
    let out = out.unwrap_or_default();
    if !status.success() {
        return Err(rejected(format!(
            "the credential command {program} failed ({status})"
        )));
    }
    Ok(Secret(String::from_utf8_lossy(&out).trim().to_owned()))
}

/// Whether `child` has exited, observed without reaping it (`WNOWAIT`).
fn has_exited(child: &Child) -> std::io::Result<bool> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
    let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    waitid(WaitId::Pid(Pid::from_child(child)), options)
        .map(|status| status.is_some())
        .map_err(std::io::Error::from)
}

/// Stops the credential command and every process of the group it leads: SIGTERM, then
/// SIGKILL after [`CREDENTIAL_KILL_GRACE`], then reaps it. It is not reaped before the
/// SIGKILL, so the group's id cannot have passed to another process meanwhile.
fn stop_credential_command(child: &mut Child) {
    use rustix::process::{Pid, Signal, kill_process_group};
    let group = Pid::from_child(child);
    let _ = kill_process_group(group, Signal::TERM);
    std::thread::sleep(CREDENTIAL_KILL_GRACE);
    let _ = kill_process_group(group, Signal::KILL);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_kind_names_are_distinct_snake_case_and_total() {
        // `name` is an exhaustive match, so a new kind is named before it compiles.
        let all = [
            KeyKind::PathList,
            KeyKind::Text,
            KeyKind::Channel,
            KeyKind::RunningPolicy,
            KeyKind::AutoBackup,
            KeyKind::Hours,
            KeyKind::HostList,
            KeyKind::Count,
            KeyKind::Size,
            KeyKind::Mebibytes,
            KeyKind::Bool,
            KeyKind::Credential,
            KeyKind::Family,
            KeyKind::RulesField,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for kind in all {
            let name = kind.name();
            let snake = !name.is_empty()
                && !name.starts_with('_')
                && !name.ends_with('_')
                && !name.contains("__")
                && name.bytes().all(|b| matches!(b, b'a'..=b'z' | b'_'));
            assert!(snake, "{kind:?}: {name:?} is not snake case");
            assert!(seen.insert(name), "{kind:?}: {name:?} is not distinct");
        }
        for key in KEYS {
            assert!(
                all.contains(&key.kind),
                "{}: its kind is listed above",
                key.key
            );
        }
    }

    const FULL: &str = r#"
# My omaddons setup.
extra_roots = ["/games/wow"]
default_target = "retail"
default_channel = "beta"
running_policy = "refuse"
auto_backup = "daily"
check_interval_hours = 12
git_hosts = ["codeberg.org"]

[limits]
archive_max_entries = 30000
archive_max_uncompressed = "2GiB"
archive_max_ratio = 150
download_max_bytes = 104857600
tx_retention_count = 20
tx_retention_days = 14
snapshot_max_bytes = "3GiB"
blob_cache_max_bytes = "512MiB"
backup_min_free_mb = 512
backup_auto_keep_days = 10

[sources.github]
enabled = true

[sources.curseforge]
enabled = false
credential = { env = "OMADDONS_CURSEFORGE_KEY" }

[targets.retail]
label = "Main"
family = "mainline"

[client_rules.mainline]
multi_toc = true
"#;

    fn parse(text: &str) -> Result<(Config, Vec<Warning>)> {
        Config::parse(text, Path::new("/c/omaddons/config.toml"))
    }

    #[test]
    fn every_key_loads() {
        let (c, warnings) = parse(FULL).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(c.extra_roots, [PathBuf::from("/games/wow")]);
        assert_eq!(
            (
                c.default_target.as_deref(),
                c.default_channel,
                c.running_policy,
                c.auto_backup,
                c.check_interval_hours
            ),
            (
                Some("retail"),
                Some(Channel::Beta),
                Some(RunningPolicy::Refuse),
                Some(AutoBackup::Daily),
                Some(12)
            )
        );
        assert_eq!(c.git_hosts, ["codeberg.org"]);
        assert_eq!(c.limits.archive_max_uncompressed, Some(Size(2 << 30)));
        assert_eq!(c.limits.download_max_bytes, Some(Size(104_857_600)));
        assert_eq!(
            c.sources["curseforge"].credential,
            Some(CredentialRef::Env {
                env: "OMADDONS_CURSEFORGE_KEY".into()
            })
        );
        assert_eq!(c.targets["retail"].family, Some(Family::Mainline));
        let limits = c.effective_limits().unwrap();
        assert_eq!(
            (
                limits.archive_max_entries,
                limits.snapshot_max_bytes,
                limits.tx_retention.keep_last
            ),
            (30_000, 3 << 30, 20)
        );
        assert_eq!(
            limits.default_check_interval,
            std::time::Duration::from_secs(12 * 3600)
        );
        assert_eq!(
            limits.backup_min_free,
            512 << 20,
            "MiB in the file, bytes inside"
        );
        assert_eq!(limits.backup_auto_keep_days, 10);
        assert_eq!(parse("").unwrap().0, Config::default());
        let off = parse("[limits]\nbackup_min_free_mb = 0\n").unwrap().0;
        assert_eq!(
            off.effective_limits().unwrap().backup_min_free,
            0,
            "0 turns the check off"
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(Size::parse("1GiB"), Some(Size(1 << 30)));
        assert_eq!(Size::parse("512 MiB"), Some(Size(512 << 20)));
        assert_eq!(Size::parse("1073741824"), Some(Size(1 << 30)));
        for bad in ["12MB", "1.5GiB", "GiB", "", "-1"] {
            assert_eq!(Size::parse(bad), None, "{bad}");
        }
        let err = parse("[limits]\nsnapshot_max_bytes = \"12MB\"\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::StateCorrupt);
        assert_eq!(
            (err.details["line"].as_u64(), err.details["column"].as_u64()),
            (Some(2), Some(22))
        );
    }

    #[test]
    fn broken_and_out_of_range_files_are_corrupt() {
        let err = parse("default_channel = \"nightly\"\n").unwrap_err();
        assert_eq!(
            (err.code, err.details["line"].as_u64()),
            (ErrorCode::StateCorrupt, Some(1))
        );
        let err = parse("x = [\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::StateCorrupt);
        let err = parse("\ncheck_interval_hours = 49\n").unwrap_err();
        assert_eq!(
            (err.code, err.details["line"].as_u64()),
            (ErrorCode::StateCorrupt, Some(2))
        );
        let err = parse("[limits]\narchive_max_ratio = 0\n").unwrap_err();
        assert!(
            err.message
                .ends_with("line 2: limits.archive_max_ratio must be greater than zero"),
            "{}",
            err.message
        );
        assert_eq!(err.fix.argv(), None);
        // The retention keys are reported as written, on their own line.
        let err = parse("[limits]\ntx_retention_days = 7\ntx_retention_count = 0\n").unwrap_err();
        assert!(
            err.message
                .ends_with("line 3: limits.tx_retention_count must be greater than zero"),
            "{}",
            err.message
        );
        assert_eq!(err.details["toml_key"], "limits.tx_retention_count");
        assert!(err.details.get("key").is_none(), "{:?}", err.details);
        let err = parse("[limits]\ntx_retention_days = 99999999999\n").unwrap_err();
        assert!(
            err.message
                .ends_with("limits.tx_retention_days must be a smaller integer"),
            "{}",
            err.message
        );
        let err = parse("[limits]\nbackup_min_free_mb = 1048577\n").unwrap_err();
        assert!(
            err.message.contains("backup_min_free_mb"),
            "{}",
            err.message
        );
        let err = parse("[limits]\nbackup_min_free_mb = -1\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::StateCorrupt);
        for days in [0, 101] {
            let err = parse(&format!("[limits]\nbackup_auto_keep_days = {days}\n")).unwrap_err();
            assert!(
                err.message.contains("from 1 to 100"),
                "{days}: {}",
                err.message
            );
        }
    }

    #[test]
    fn unknown_keys_only_warn() {
        let (_, warnings) = parse("defualt_target = \"x\"\n[limits]\nsnapshot_max = 1\n[sources.github]\nenabeld = true\n[client_rules.mainline]\nmulti_tok = true\n").unwrap();
        let messages: Vec<&str> = warnings.iter().map(|w| w.message.as_str()).collect();
        assert_eq!(warnings.len(), 4, "{messages:?}");
        assert!(
            warnings
                .iter()
                .all(|w| w.code == WarningCode::ConfigUnknownKey)
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("did you mean default_target?")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("did you mean sources.github.enabled?"))
        );
    }

    /// A rules field under another section is never "corrected" to itself: only the
    /// `*` segments of `client_rules.*.*` come from the key.
    #[test]
    fn a_rules_field_elsewhere_suggests_no_key_of_its_own_spelling() {
        assert_eq!(closest_key("targets.retail.legacy"), None);
        assert_eq!(closest_key("sources.github.multi_toc"), None);
        assert_eq!(
            closest_key("client_rules.vanilla.multi_tok").as_deref(),
            Some("client_rules.vanilla.multi_toc")
        );
    }

    /// The unknown-key messages of `text`, each `key; did you mean …?` part only.
    fn unknown_in(text: &str) -> Vec<String> {
        let (_, warnings) = parse(text).unwrap();
        assert!(
            warnings
                .iter()
                .all(|w| matches!(w.code, WarningCode::ConfigUnknownKey))
        );
        warnings
            .iter()
            .filter_map(|w| {
                w.message
                    .split_once("has an unknown key ")
                    .map(|(_, k)| k.to_owned())
            })
            .collect()
    }

    #[test]
    fn unknown_keys_under_rules_warn_with_a_suggestion() {
        let found = unknown_in(
            "[rules]\nprodcuts = []\n\n[rules.client_rules.vanilla]\nmulti_tocs = false\n\n[client_rules.mainline]\ncomma_interfaces = true\n\n[targets.retail]\nlabl = \"x\"\n",
        );
        // Sections in key order.
        assert_eq!(
            found,
            [
                "client_rules.mainline.comma_interfaces; did you mean client_rules.mainline.comma_interface?",
                "rules.prodcuts; did you mean rules.products?",
                "rules.client_rules.vanilla.multi_tocs; did you mean rules.client_rules.vanilla.multi_toc?",
                "targets.retail.labl; did you mean targets.retail.label?",
            ]
        );
        let found = unknown_in(
            r#"[[rules.products]]
code = "wow_x"
folder = "_x_"
exe = "WowX.exe"
kind = "live"
default_id = "x"
colour = "red"
[[rules.products.modes]]
id = "a"
name = "A"
nmae = "typo"

[[rules.version_families]]
major = { min = 1, max = 1, mid = 2 }
minor = { min = 0 }
family = "vanilla"
famly = "vanilla"

[rules.family_ranges.vanilla]
min = 11_200
maks = 11_599
"#,
        );
        assert_eq!(
            found,
            [
                "rules.products[0].colour",
                "rules.products[0].modes[0].nmae; did you mean rules.products[0].modes[0].name?",
                "rules.version_families[0].famly; did you mean rules.version_families[0].family?",
                "rules.version_families[0].major.mid; did you mean rules.version_families[0].major.min?",
                "rules.family_ranges.vanilla.maks; did you mean rules.family_ranges.vanilla.max?",
            ]
        );
    }

    #[test]
    fn every_documented_rules_field_is_quiet() {
        let rows: String = RULES_FIELDS
            .iter()
            .map(|f| match *f {
                "family" => "family = \"vanilla\"\n".to_owned(),
                "install_kind" => "install_kind = \"battlenet\"\n".to_owned(),
                "version_min" | "version_max" => format!("{f} = \"1.15.0\"\n"),
                "interface_floor" => format!("{f} = 11_500\n"),
                "game_types" | "game_suffixes" | "family_suffixes" | "legacy_suffixes" => {
                    format!("{f} = [\"Vanilla\"]\n")
                }
                _ => format!("{f} = true\n"),
            })
            .collect();
        let text = format!(
            r#"[[rules.products]]
code = "wow_x"
folder = "_x_"
exe = "WowX.exe"
kind = "live"
default_id = "x"
[[rules.products.modes]]
id = "a"
name = "A"
aliases = ["b"]

[[rules.version_families]]
install_kind = "custom"
major = {{ min = 1, max = 1 }}
minor = {{ min = 70, max = 70 }}
family = "vanilla-legacy"
interface = 11_200

[rules.family_ranges.vanilla]
min = 11_200
max = 11_599

[rules.client_rules.vanilla]
{rows}
[client_rules.mists]
{rows}"#
        );
        assert_eq!(unknown_in(&text), Vec::<String>::new(), "{text}");
    }

    /// The `[rules]` field lists are the structs' fields, as their schemas name them.
    #[test]
    fn the_rules_field_lists_are_the_structs_fields() {
        fn fields(schema: schemars::Schema) -> Vec<String> {
            let schema = serde_json::to_value(schema).unwrap();
            let mut names: Vec<String> = schema["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            names.sort();
            names
        }
        fn sorted(list: &[&str]) -> Vec<String> {
            let mut names: Vec<String> = list.iter().map(|s| (*s).to_owned()).collect();
            names.sort();
            names
        }
        use crate::rules::{
            ClientRulesOverride, GameMode, NumRange, Product, RulesOverrides, VersionFamilyRule,
        };
        use schemars::schema_for;
        assert_eq!(fields(schema_for!(RulesOverrides)), sorted(RULES_TABLES));
        assert_eq!(fields(schema_for!(Product)), sorted(PRODUCT_FIELDS));
        assert_eq!(fields(schema_for!(GameMode)), sorted(MODE_FIELDS));
        assert_eq!(
            fields(schema_for!(VersionFamilyRule)),
            sorted(VERSION_RULE_FIELDS)
        );
        assert_eq!(fields(schema_for!(NumRange)), sorted(RANGE_FIELDS));
        assert_eq!(
            fields(schema_for!(ClientRulesOverride)),
            sorted(RULES_FIELDS)
        );
    }

    #[test]
    fn git_hosts_are_bare_host_names() {
        let path = Path::new("/c/omaddons/config.toml");
        for good in [
            "codeberg.org",
            "git.example.co.uk",
            "gitea-1.lan",
            "Codeberg.org",
        ] {
            assert!(is_host_name(good), "{good}");
        }
        for bad in [
            "",
            "https://codeberg.org",
            "codeberg.org/o",
            "codeberg.org:443",
            "-x.org",
            "a..b",
            "a b",
            "user@host",
        ] {
            assert!(!is_host_name(bad), "{bad}");
        }
        let set = set_value("", path, "git_hosts", "[\"codeberg.org\"]").unwrap();
        assert_eq!(
            Config::parse(&set, path).unwrap().0.git_hosts,
            ["codeberg.org"]
        );
        let err = set_value("", path, "git_hosts", "[\"https://codeberg.org\"]").unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert!(err.message.contains("bare host name"), "{}", err.message);
    }

    /// D13: the git binary never gets an HTTP host, an optional source's (enabled or not)
    /// or any case of one; by `config set` a usage error, by hand `state_corrupt` with
    /// the line and key.
    #[test]
    fn git_hosts_never_name_an_http_host() {
        let path = Path::new("/c/omaddons/config.toml");
        for http in [
            "CodeLoad.GitHub.com",
            "github.com",
            "api.tukui.org",
            "api.curseforge.com",
            "addons.wago.io",
        ] {
            let value = format!("[\"codeberg.org\", \"{http}\"]");
            let err = set_value("", path, "git_hosts", &value).unwrap_err();
            assert_eq!(err.code, ErrorCode::Usage, "{http}");
            assert!(
                err.message.contains(&format!("{http:?}"))
                    && err.message.contains("remove it from git_hosts"),
                "{}",
                err.message
            );
            let text = format!("schema_version = \"1.0\"\ngit_hosts = {value}\n");
            let err = Config::parse(&text, path).unwrap_err();
            assert_eq!(err.code, ErrorCode::StateCorrupt, "{http}");
            assert_eq!(
                (&err.details["line"], &err.details["toml_key"]),
                (&serde_json::json!(2), &serde_json::json!("git_hosts")),
                "{http}"
            );
        }
        let set = set_value("", path, "git_hosts", "[\"codeberg.org\"]").unwrap();
        assert!(Config::parse(&set, path).is_ok());
    }

    #[test]
    fn set_preserves_everything_else() {
        let path = Path::new("/c/omaddons/config.toml");
        let updated = set_value(FULL, path, "limits.snapshot_max_bytes", "4GiB").unwrap();
        assert_eq!(
            updated,
            FULL.replace(
                "snapshot_max_bytes = \"3GiB\"",
                "snapshot_max_bytes = \"4GiB\""
            )
        );
        let fresh = set_value("", path, "sources.github.enabled", "false").unwrap();
        assert_eq!(
            parse(&fresh).unwrap().0.sources["github"].enabled,
            Some(false)
        );
        let roots = set_value("", path, "extra_roots", "[\"/a\", \"/b\"]").unwrap();
        assert_eq!(parse(&roots).unwrap().0.extra_roots.len(), 2);
        let target = set_value("# keep me\n", path, "default_target", "retail").unwrap();
        assert_eq!(target, "# keep me\ndefault_target = \"retail\"\n");
        insta::assert_snapshot!(
            "config_set_new_tables",
            set_value(FULL, path, "targets.era.label", "Era").unwrap()
        );
    }

    #[test]
    fn set_rejects_unknown_keys_and_bad_values() {
        let path = Path::new("/c/omaddons/config.toml");
        let err = set_value(FULL, path, "limits.snapshot_max_byte", "1GiB").unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert_eq!(err.fix.argv().unwrap()[3], "limits.snapshot_max_bytes");
        let err = set_value(FULL, path, "nope.nothing", "1").unwrap_err();
        assert_eq!(err.fix.argv().unwrap(), ["omaddons", "config"]);
        for (key, value) in [
            ("default_channel", "nightly"),
            ("check_interval_hours", "99"),
            ("limits.snapshot_max_bytes", "12MB"),
            ("sources.github.enabled", "maybe"),
            ("targets.x.family", "retail"),
        ] {
            let err = set_value(FULL, path, key, value).unwrap_err();
            assert_eq!(err.code, ErrorCode::Usage, "{key} {value}");
            assert!(err.message.contains("it takes"), "{}", err.message);
        }
    }

    #[test]
    fn secrets_never_show() {
        let (secret, _) = resolve_credential(&CredentialRef::Command {
            command: vec!["echo".into(), "s3cret".into()],
        })
        .unwrap();
        assert_eq!(secret.expose(), "s3cret");
        assert_eq!(format!("{secret} {secret:?}"), "<redacted> <redacted>");
        let err = resolve_credential(&CredentialRef::Command {
            command: vec!["false".into()],
        })
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::AuthRejected);
        let err = resolve_credential(&CredentialRef::Env {
            env: "OMADDONS_TEST_SURELY_UNSET_VAR".into(),
        })
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::AuthRequired);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("key");
        std::fs::write(&file, "  filesecret\n").unwrap();
        let (secret, warnings) =
            resolve_credential(&CredentialRef::File { file: file.clone() }).unwrap();
        assert_eq!(secret.expose(), "filesecret");
        assert_eq!(
            warnings.len(),
            usize::from(std::fs::metadata(&file).unwrap().permissions().mode() & 0o077 != 0)
        );
        let key_command: Config =
            toml::from_str("[sources.x]\ncredential = { key_command = [\"echo\", \"y\"] }\n")
                .unwrap();
        assert!(matches!(
            key_command.sources["x"].credential,
            Some(CredentialRef::Command { .. })
        ));
    }

    #[test]
    fn credential_exposed_cases() {
        for (mode, exposed) in [
            (0o600, false),
            (0o400, false),
            (0o700, false),
            (0o640, true),
            (0o604, true),
            (0o602, true),
            (0o610, true),
            (0o644, true),
            (0o666, true),
        ] {
            assert_eq!(credential_exposed(mode), exposed, "{mode:04o}");
        }
    }

    const PATH: &str = "/c/omaddons/config.toml";

    fn problem(text: &str) -> ConfigProblem {
        Config::parse_checked(text, Path::new(PATH)).unwrap_err()
    }

    #[test]
    fn parse_checked_types_each_problem() {
        let toml_error = problem("limits = [\n");
        assert_eq!(
            (toml_error.line, toml_error.path.as_deref()),
            (Some(1), Some(Path::new(PATH)))
        );
        assert!(toml_error.column.is_some(), "{toml_error:?}");
        assert!(!toml_error.message.contains(PATH), "{toml_error:?}");
        for (case, text, line, message) in [
            (
                "check_interval_over_48",
                "default_channel = \"stable\"\ncheck_interval_hours = 99\n",
                2,
                "check_interval_hours must be from 0 to 48",
            ),
            (
                "git_host_with_scheme",
                "git_hosts = [\"https://x\"]\n",
                1,
                "git_hosts \"https://x\" is not a bare host name (no scheme, port or path)",
            ),
            (
                "limit_not_positive",
                "[limits]\ndownload_max_bytes = 0\n",
                2,
                "limits.download_max_bytes must be greater than zero",
            ),
            (
                "keep_days_out_of_range",
                "[limits]\nbackup_auto_keep_days = 0\n",
                2,
                "limits.backup_auto_keep_days must be from 1 to 100",
            ),
        ] {
            let got = problem(text);
            assert_eq!(
                (got.line, got.column, got.message.as_str()),
                (Some(line), Some(1), message),
                "case {case}: {text:?}"
            );
        }
        let http = problem("git_hosts = [\"github.com\"]\n");
        assert!(
            http.message
                .starts_with("git_hosts \"github.com\" is a host"),
            "{http:?}"
        );
        let rules = problem("[client_rules.brand_new]\nmulti_toc = true\n");
        assert!(rules.message.starts_with("client_rules "), "{rules:?}");
    }

    /// The errors `Config::parse` returns through the problem, pinned as they were before
    /// it existed.
    #[test]
    fn parse_errors_are_unchanged_through_the_problem() {
        let err = parse("[limits]\ndownload_max_bytes = 0\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::StateCorrupt);
        assert_eq!(
            err.message,
            format!("{PATH} line 2: limits.download_max_bytes must be greater than zero")
        );
        assert_eq!(err.fix.display(), format!("edit {PATH}"));
        assert_eq!(
            err.details,
            serde_json::json!({"path": PATH, "line": 2, "column": 1, "toml_key": "limits.download_max_bytes"})
        );
        let err = parse("default_channel = \"nightly\"\n").unwrap_err();
        let (line, column) = (
            err.details["line"].as_u64().unwrap(),
            err.details["column"].as_u64().unwrap(),
        );
        assert_eq!((line, column), (1, 19), "{}", err.message);
        assert!(
            err.message
                .starts_with(&format!("{PATH} line 1, column 19: ")),
            "{}",
            err.message
        );
        assert_eq!(
            err.details
                .as_object()
                .map(|d| d.keys().cloned().collect::<Vec<_>>()),
            Some(vec![
                "column".to_owned(),
                "line".to_owned(),
                "path".to_owned()
            ])
        );
        assert_eq!(err.fix.display(), format!("edit {PATH}"));
    }

    #[test]
    fn load_tolerant_never_fails() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path().join("home"),
            dir.path().join("config"),
            dir.path().join("state"),
            dir.path().join("cache"),
        );
        let file = paths.config_file();
        let reset = || {
            let _ = std::fs::remove_file(&file);
            let _ = std::fs::remove_dir(&file);
        };
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        /// What is at the config path.
        enum At {
            Nothing,
            Bytes(&'static [u8]),
            Directory,
        }
        // (case, what is there, whether a problem is expected: with its line known)
        let cases: [(&str, At, Option<bool>); 6] = [
            ("missing", At::Nothing, None),
            ("valid", At::Bytes(b"default_channel = \"beta\"\n"), None),
            (
                "unknown_key",
                At::Bytes(b"defualt_channel = \"beta\"\n"),
                None,
            ),
            ("a_directory", At::Directory, Some(false)),
            ("invalid_utf8", At::Bytes(&[0xff, 0xfe]), Some(false)),
            ("broken", At::Bytes(b"limits = [\n"), Some(true)),
        ];
        for (case, at, problem) in cases {
            reset();
            match at {
                At::Nothing => {}
                At::Bytes(bytes) => std::fs::write(&file, bytes).unwrap(),
                At::Directory => std::fs::create_dir(&file).unwrap(),
            }
            let got = Config::load_tolerant(&paths);
            assert_eq!(got.loaded.path, file, "{case}");
            match problem {
                None => assert_eq!(got.problem, None, "{case}"),
                Some(located) => {
                    let p = got.problem.clone().unwrap();
                    assert_eq!(p.line.is_some(), located, "{case}: {p:?}");
                    assert_eq!(p.column.is_some(), located, "{case}: {p:?}");
                    assert_eq!(p.path.as_deref(), Some(file.as_path()), "{case}");
                    assert_eq!(got.loaded.config, Config::default(), "{case}");
                    assert!(got.loaded.warnings.is_empty(), "{case}");
                }
            }
            match case {
                "missing" => assert!(!got.loaded.exists, "{case}"),
                "valid" => assert_eq!(got.loaded.config.default_channel, Some(Channel::Beta)),
                "unknown_key" => assert_eq!(got.loaded.warnings.len(), 1, "{case}"),
                _ => {}
            }
        }
        // A file only its owner could read but cannot: skipped as root, which reads it.
        reset();
        std::fs::write(&file, "default_channel = \"beta\"\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&file).is_ok() {
            eprintln!("load_tolerant_never_fails: unreadable case skipped (running as root)"); // ubs:ignore — a test's skip note
        } else {
            let got = Config::load_tolerant(&paths);
            let p = got.problem.unwrap();
            assert_eq!((p.line, p.column), (None, None), "{p:?}");
            assert_eq!(got.loaded.config, Config::default());
        }
    }

    #[test]
    fn limit_config_keys_cover_every_overridable_limit() {
        let mut overridable: Vec<&str> = crate::politeness::REGISTRY
            .iter()
            .chain(crate::limits::REGISTRY)
            .filter(|info| info.config_key.is_some())
            .map(|info| info.name)
            .collect();
        overridable.sort_unstable();
        let mut listed: Vec<&str> = LIMIT_CONFIG_KEYS.iter().map(|(name, _)| *name).collect();
        listed.sort_unstable();
        assert_eq!(listed, overridable);
    }

    #[test]
    fn limit_config_keys_are_config_keys() {
        let keys: Vec<&str> = LIMIT_CONFIG_KEYS
            .iter()
            .flat_map(|(_, keys)| keys.iter().copied())
            .collect();
        for key in &keys {
            assert!(spec_for(key).is_some(), "{key} is no config key");
        }
        let expected: Vec<&str> = KEYS
            .iter()
            .map(|spec| spec.key)
            .filter(|key| key.starts_with("limits.") || *key == "check_interval_hours")
            .collect();
        assert_eq!(keys, expected, "every limit key once, in KEYS order");
        let config = Config::default();
        assert!(keys.iter().all(|key| !config.sets_limit_key(key)));
    }

    #[test]
    fn credential_present_reads_and_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, text: &str, mode: u32| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        };
        let locked = file("locked", "secret\n", 0o000);
        let empty = file("empty", "", 0o600);
        let marker = dir.path().join("ran");
        let env = |name: &str| match name {
            "SET" => Some(std::ffi::OsString::from("x")),
            "EMPTY" => Some(std::ffi::OsString::new()),
            _ => None,
        };
        let cases = [
            ("env_set", CredentialRef::Env { env: "SET".into() }, true),
            (
                "env_unset",
                CredentialRef::Env {
                    env: "UNSET".into(),
                },
                false,
            ),
            (
                "env_empty",
                CredentialRef::Env {
                    env: "EMPTY".into(),
                },
                false,
            ),
            (
                "file_present_mode_000",
                CredentialRef::File { file: locked },
                true,
            ),
            (
                "file_missing",
                CredentialRef::File {
                    file: dir.path().join("missing"),
                },
                false,
            ),
            ("file_empty", CredentialRef::File { file: empty }, false),
            (
                "command_never_runs",
                CredentialRef::Command {
                    command: vec![
                        "sh".into(),
                        "-c".into(),
                        format!("touch {}", marker.display()),
                    ],
                },
                true,
            ),
        ];
        for (case, credential, want) in cases {
            assert_eq!(credential_present_with(&credential, &env), want, "{case}");
        }
        assert!(!marker.exists(), "a credential command ran");
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    fn read_pid(path: &Path) -> i32 {
        std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// `/proc` no longer lists `pid`, or lists it only as a zombie its reaper has not
    /// collected yet.
    fn process_gone(pid: i32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
            // The state follows the command's name, which is in parentheses.
            stat.rsplit_once(')').is_some_and(|(_, rest)| {
                matches!(rest.trim_start().chars().next(), Some('Z' | 'X'))
            })
        })
    }

    /// [`process_gone`] within 5 s: a killed process's reaper may take a moment.
    fn gone_soon(pid: i32) -> bool {
        let until = Instant::now() + Duration::from_secs(5);
        while !process_gone(pid) {
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }

    /// The command exits at once, but a process it started in the background still holds
    /// its stdout: omaddons stops the group at the deadline instead of waiting on it.
    #[test]
    fn a_credential_command_whose_helper_holds_its_output_is_stopped_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("helper.pid");
        let argv = sh(&format!(
            "sleep 30 & echo $! > '{}'; echo tok",
            pid_file.display()
        ));
        let timeout = Duration::from_secs(2);
        let started = Instant::now();
        let err = run_credential_command_for(&argv, timeout).unwrap_err();
        let took = started.elapsed();
        assert_eq!(err.code, ErrorCode::AuthRejected, "{}", err.message);
        assert!(
            err.message.contains("still held its output"),
            "{argv:?} → {}",
            err.message
        );
        assert!(
            took < timeout + Duration::from_secs(3),
            "{argv:?} took {took:?}"
        );
        let helper = read_pid(&pid_file);
        assert!(gone_soon(helper), "the helper {helper} outlived the stop");
    }

    /// A command still running at the deadline is stopped with everything it started.
    #[test]
    fn a_credential_command_past_its_deadline_is_stopped_with_its_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("helper.pid");
        let argv = sh(&format!(
            "sleep 30 & echo $! > '{}'; sleep 30",
            pid_file.display()
        ));
        let timeout = Duration::from_secs(1);
        let started = Instant::now();
        let err = run_credential_command_for(&argv, timeout).unwrap_err();
        let took = started.elapsed();
        assert_eq!(err.code, ErrorCode::AuthRejected, "{}", err.message);
        assert!(
            err.message.contains("timed out"),
            "{argv:?} → {}",
            err.message
        );
        assert!(
            took < timeout + Duration::from_secs(3),
            "{argv:?} took {took:?}"
        );
        let helper = read_pid(&pid_file);
        assert!(gone_soon(helper), "the helper {helper} outlived the stop");
    }

    /// Output past the cap stops the command at once, long before the deadline.
    #[test]
    fn a_credential_command_printing_past_the_cap_is_stopped_without_waiting() {
        let argv = vec!["yes".to_owned()];
        let timeout = Duration::from_secs(60);
        let started = Instant::now();
        let err = run_credential_command_for(&argv, timeout).unwrap_err();
        let took = started.elapsed();
        assert_eq!(err.code, ErrorCode::AuthRejected, "{}", err.message);
        assert!(
            err.message.contains(&format!(
                "printed more than {CREDENTIAL_OUTPUT_MAX_BYTES} bytes"
            )),
            "{argv:?} → {}",
            err.message
        );
        assert!(took < Duration::from_secs(10), "{argv:?} took {took:?}");
    }

    /// A command that succeeds is never stopped: a helper it started with its output
    /// elsewhere keeps running, and the answer comes at once.
    #[test]
    fn a_successful_credential_command_leaves_a_detached_helper_running() {
        use rustix::process::{Pid, Signal, kill_process};
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("helper.pid");
        let argv = sh(&format!(
            "sleep 30 > /dev/null & echo $! > '{}'; echo tok",
            pid_file.display()
        ));
        let started = Instant::now();
        let got = run_credential_command_for(&argv, Duration::from_secs(10));
        let took = started.elapsed();
        let helper = read_pid(&pid_file);
        let running = !process_gone(helper);
        let _ = kill_process(Pid::from_raw(helper).unwrap(), Signal::KILL);
        assert_eq!(got.unwrap().expose(), "tok", "{argv:?}");
        assert!(took < Duration::from_secs(5), "{argv:?} took {took:?}");
        assert!(running, "the helper {helper} was stopped after a success");
    }

    /// Well-behaved commands: the trimmed output (up to exactly the cap) is the secret,
    /// and a failure says why.
    #[test]
    fn a_credential_command_answers_with_its_trimmed_output_or_why_it_failed() {
        let cap = usize::try_from(CREDENTIAL_OUTPUT_MAX_BYTES).unwrap();
        let cases: Vec<(&str, Vec<String>, std::result::Result<String, &str>)> = vec![
            ("plain", vec!["echo".into(), "tok".into()], Ok("tok".into())),
            ("trimmed", sh("printf '  tok \\n\\n'"), Ok("tok".into())),
            (
                "exactly_the_cap",
                sh(&format!("head -c {cap} /dev/zero | tr '\\0' a")),
                Ok("a".repeat(cap)),
            ),
            ("exit_status", sh("exit 3"), Err("failed (exit status: 3)")),
            (
                "missing_program",
                vec!["/nonexistent/omaddons-credential".into()],
                Err("cannot run the credential command"),
            ),
            ("empty", Vec::new(), Err("the credential command is empty")),
        ];
        for (case, argv, want) in cases {
            let got = run_credential_command_for(&argv, Duration::from_secs(10));
            match (got, want) {
                (Ok(secret), Ok(want)) => {
                    assert!(
                        secret.expose() == want, // ubs:ignore — a test fixture's value, no credential check
                        "{case}: {argv:?} answered another secret"
                    );
                }
                (Err(err), Err(want)) => {
                    assert_eq!(err.code, ErrorCode::AuthRejected, "{case}: {}", err.message);
                    assert!(
                        err.message.contains(want),
                        "{case}: {argv:?} → {}",
                        err.message
                    );
                }
                (got, want) => panic!("{case}: {argv:?} → {got:?}, want {want:?}"),
            }
        }
    }

    use std::os::unix::fs::PermissionsExt;
}
