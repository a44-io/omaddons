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
use std::process::{Command, Stdio};
use std::time::Instant;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use super::paths::Paths;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::limits::{CREDENTIAL_COMMAND_TIMEOUT, EffectiveLimits};
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
    /// The target for commands without `-t`: an id, key or any `-t` form.
    pub default_target: Option<String>,
    pub default_channel: Option<Channel>,
    pub running_policy: Option<RunningPolicy>,
    pub auto_backup: Option<AutoBackup>,
    /// 0–48; 0 checks only when the panel opens.
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

/// Automatic WTF backups (used in M3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutoBackup {
    Off,
    Daily,
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
}

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
    Bool,
    Credential,
    Family,
    /// A field of a client-rules row.
    RulesField,
}

impl KeyKind {
    /// What `config set` accepts, for error messages.
    pub fn accepts(self) -> &'static str {
        match self {
            Self::PathList => "a TOML array of absolute paths, e.g. '[\"/games/wow\"]'",
            Self::Text => "a string",
            Self::Channel => "stable, beta or alpha",
            Self::RunningPolicy => "safe, refuse or allow",
            Self::AutoBackup => "off or daily",
            Self::Hours => "an integer from 0 to 48",
            Self::HostList => "a TOML array of host names, e.g. '[\"codeberg.org\"]'",
            Self::Count => "a positive integer",
            Self::Size => "a positive number of bytes, or one with a KiB, MiB or GiB suffix",
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
        "The target for commands without -t",
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
        "Hours between background update checks",
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
    "game",
    "game_types",
    "multi_toc",
    "comma_interface",
    "game_suffixes",
    "family_suffixes",
    "legacy_suffixes",
    "directive_conditionals",
    "path_variables",
    "interface_includes_patch",
    "legacy",
];

/// The spec a dotted key matches.
pub fn spec_for(key: &str) -> Option<&'static KeySpec> {
    let parts: Vec<&str> = key.split('.').collect();
    KEYS.iter().find(|spec| {
        let pattern: Vec<&str> = spec.key.split('.').collect();
        pattern.len() == parts.len()
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
                let mut c: Vec<&str> = parts[..parts.len() - 1].to_vec();
                c.push(field);
                candidates.push(c.join("."));
            }
            continue;
        }
        let filled: Vec<&str> = pattern
            .iter()
            .zip(&parts)
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

    /// Parses and validates config text; `path` is for messages.
    pub fn parse(text: &str, path: &Path) -> Result<(Self, Vec<Warning>)> {
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
        if let Some(bad) = config.git_hosts.iter().find(|h| !is_host_name(h)) {
            return Err(invalid(
                path,
                text,
                "git_hosts",
                &format!("{bad:?} is not a bare host name (no scheme, port or path)"),
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
        let mut apply = |key: &str, value: toml::Value| {
            limits
                .apply(key, &value)
                .map_err(|e| (format!("limits.{key}"), e.to_string()))
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
                apply(key, int(size.0))?;
            }
        }
        for (key, n) in [
            ("archive_max_entries", l.archive_max_entries),
            ("archive_max_ratio", l.archive_max_ratio),
        ] {
            if let Some(n) = n {
                apply(key, int(n))?;
            }
        }
        if l.tx_retention_count.is_some() || l.tx_retention_days.is_some() {
            let mut table = toml::Table::new();
            if let Some(n) = l.tx_retention_count {
                table.insert("keep_last".into(), int(n));
            }
            if let Some(n) = l.tx_retention_days {
                table.insert("window_days".into(), int(n));
            }
            apply("tx_retention", toml::Value::Table(table))?;
        }
        if let Some(hours) = self.check_interval_hours.filter(|h| *h > 0) {
            apply(
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
        .map(|key| {
            let hint = closest_key(&key)
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

fn check(key: &str, unknown: &mut Vec<String>) {
    if spec_for(key).is_none() {
        unknown.push(key.to_owned());
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

/// `state_corrupt` for a TOML or type error, citing line and column.
fn corrupt(path: &Path, text: &str, err: &toml::de::Error) -> Error {
    let (line, column) = err
        .span()
        .map_or((1, 1), |span| line_column(text, span.start));
    let message = err.message().trim().to_owned();
    Error::new(
        ErrorCode::StateCorrupt,
        format!("{} line {line}, column {column}: {message}", path.display()),
        edit_fix(path),
    )
    .details(serde_json::json!({ "path": path, "line": line, "column": column }))
}

/// `state_corrupt` for a value out of range, citing the key's line.
fn invalid(path: &Path, text: &str, key: &str, message: &str) -> Error {
    let leaf = key.rsplit('.').next().unwrap_or(key);
    let line = text
        .lines()
        .position(|l| l.trim_start().starts_with(leaf))
        .map_or(1, |i| i + 1);
    Error::new(
        ErrorCode::StateCorrupt,
        format!("{} line {line}: {key} {message}", path.display()),
        edit_fix(path),
    )
    .details(serde_json::json!({ "path": path, "line": line, "column": 1, "key": key }))
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
                if meta.permissions().mode() & 0o077 != 0 {
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

/// Runs `argv` without a shell, stdin closed, stderr discarded, for at most
/// [`CREDENTIAL_COMMAND_TIMEOUT`]; its trimmed stdout is the secret.
fn run_credential_command(argv: &[String]) -> Result<Secret> {
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
    // without a shell, with stdin closed and stderr discarded.
    let mut command = Command::new(program); // ubs:ignore — user-configured credential argv
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            rejected(format!(
                "cannot run the credential command {program}: {err}"
            ))
        })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| rejected("the credential command has no stdout".into()))?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    // ubs:ignore — a timeout clock, not randomness
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < CREDENTIAL_COMMAND_TIMEOUT => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(rejected(format!(
                    "the credential command {program} timed out"
                )));
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(rejected(format!(
            "the credential command {program} failed ({status})"
        )));
    }
    Ok(Secret(String::from_utf8_lossy(&out).trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(parse("").unwrap().0, Config::default());
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
        assert!(err.message.contains("archive_max_ratio"), "{}", err.message);
        assert_eq!(err.fix.argv(), None);
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

    use std::os::unix::fs::PermissionsExt;
}
