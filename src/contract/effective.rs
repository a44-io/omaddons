//! The values `config.toml` can change, as this run will use them (M4-D4): each limit,
//! source, family range and client-rules row with its effective value and where it comes
//! from, `builtin` or `config`. `capabilities` publishes them, because a user who raised
//! `limits.download_max_bytes` changed what the tool does.
//!
//! The rule for `source`: `config` when `config.toml` sets a key that applies to the
//! value, even to the built-in value; `builtin` otherwise. `check_interval_hours = 0`
//! keeps the default, so it stays `builtin`. Pure: the caller loads the config and says
//! which credentials are present.

use schemars::JsonSchema;
use serde::Serialize;

use crate::limits::{self, EffectiveLimits, LimitInfo, LimitValue, ms};
use crate::net::allowlist::hosts_for;
use crate::rules::{ClientRules, Family, InstallKind, NumRange, RulesTable};
use crate::sources::{SourceConfigView, SourceId};
use crate::state::config::{Config, LIMIT_CONFIG_KEYS};

/// Where an effective value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValueSource {
    Builtin,
    Config,
}

/// One limit of `politeness::REGISTRY` or `limits::REGISTRY`, as this run uses it.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveLimit {
    pub info: &'static LimitInfo,
    /// The registry's value, or the one `config.toml` sets (the same variant).
    pub value: LimitValue,
    /// The `config.toml` key that changes it (the one `omaddons config set` takes);
    /// `None` for a limit config cannot change.
    pub config_key: Option<&'static str>,
    pub source: ValueSource,
}

/// One source, as this run uses it.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveSource {
    pub id: SourceId,
    /// In this build and not turned off (CurseForge and Wago are never enabled here).
    pub enabled: bool,
    pub requires_credential: bool,
    /// A credential is configured: present, not proven valid.
    pub credential_configured: bool,
    /// The hosts it may contact: its built-in ones, then for `git` the `git_hosts` added.
    pub hosts: Vec<String>,
    pub source: ValueSource,
}

/// One family's interface range, as this run uses it.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveFamily {
    pub family: Family,
    pub range: Option<NumRange>,
    pub legacy: bool,
    pub source: ValueSource,
}

/// One client-rules row, as this run uses it.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveRules {
    pub rules: ClientRules,
    pub source: ValueSource,
}

/// Every effective value.
#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    pub limits: Vec<EffectiveLimit>,
    pub sources: Vec<EffectiveSource>,
    pub families: Vec<EffectiveFamily>,
    pub client_rules: Vec<EffectiveRules>,
}

impl Effective {
    /// The built-in values: the default config, no credential.
    pub fn builtin() -> Self {
        Self::of(&Config::default(), &|_| false)
    }

    /// The values `config` gives, `credential` saying which sources have a credential.
    /// `config` is one that parsed, so its limits and rules apply; were they not to, the
    /// built-in values stand.
    pub fn of(config: &Config, credential: &dyn Fn(SourceId) -> bool) -> Self {
        let table = config
            .rules_table(&RulesTable::builtin())
            .unwrap_or_else(|_| RulesTable::builtin());
        Self {
            limits: limits_of(config),
            sources: sources_of(config, credential),
            families: Family::ALL
                .iter()
                .map(|&family| EffectiveFamily {
                    family,
                    range: table.family_range(family),
                    legacy: family.is_legacy(),
                    source: from(config.rules.family_ranges.contains_key(&family)),
                })
                .collect(),
            client_rules: table
                .client_rules()
                .iter()
                .chain([
                    table.fallback(InstallKind::Battlenet),
                    table.fallback(InstallKind::Custom),
                ])
                .map(|rules| EffectiveRules {
                    source: from(
                        config.client_rules.contains_key(&rules.name)
                            || config.rules.client_rules.contains_key(&rules.name),
                    ),
                    rules: rules.clone(),
                })
                .collect(),
        }
    }
}

const fn from(set: bool) -> ValueSource {
    if set {
        ValueSource::Config
    } else {
        ValueSource::Builtin
    }
}

fn limits_of(config: &Config) -> Vec<EffectiveLimit> {
    let effective = config.effective_limits().unwrap_or_default();
    crate::politeness::REGISTRY
        .iter()
        .chain(limits::REGISTRY)
        .map(|info| {
            let keys = LIMIT_CONFIG_KEYS
                .iter()
                .find(|(name, _)| *name == info.name)
                .map_or(&[][..], |(_, keys)| *keys);
            let set = keys.iter().any(|key| config.sets_limit_key(key));
            EffectiveLimit {
                info,
                value: if set {
                    overridden(info, &effective)
                } else {
                    info.value
                },
                config_key: keys.first().copied(),
                source: from(set),
            }
        })
        .collect()
}

/// The value `effective` holds for `info`, a limit config can change, in the registry's
/// variant.
fn overridden(info: &LimitInfo, effective: &EffectiveLimits) -> LimitValue {
    let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    match info.config_key {
        Some("default_check_interval") => LimitValue::Millis(ms(effective.default_check_interval)),
        Some("archive_max_entries") => LimitValue::Count(effective.archive_max_entries),
        Some("archive_max_uncompressed") => LimitValue::Bytes(effective.archive_max_uncompressed),
        Some("archive_max_ratio") => LimitValue::Ratio(effective.archive_max_ratio),
        Some("download_max_bytes") => LimitValue::Bytes(effective.download_max_bytes),
        Some("tx_retention") => LimitValue::Retention {
            min_age_ms: ms(effective.tx_retention.min_age),
            keep_last: count(effective.tx_retention.keep_last),
            window_ms: ms(effective.tx_retention.window),
        },
        Some("snapshot_max_bytes") => LimitValue::Bytes(effective.snapshot_max_bytes),
        Some("blob_cache_max_bytes") => LimitValue::Bytes(effective.blob_cache_max_bytes),
        Some("backup_min_free") => LimitValue::Bytes(effective.backup_min_free),
        Some("backup_copy_max") => LimitValue::Bytes(effective.backup_copy_max),
        Some("backup_auto_keep_days") => LimitValue::Count(count(effective.backup_auto_keep_days)),
        _ => info.value,
    }
}

fn sources_of(config: &Config, credential: &dyn Fn(SourceId) -> bool) -> Vec<EffectiveSource> {
    let view = SourceConfigView::from_config(&config.sources);
    SourceId::ALL
        .iter()
        .map(|&id| {
            let present = id.later().is_none();
            let mut hosts: Vec<String> = hosts_for(id).iter().map(|h| (*h).to_owned()).collect();
            if id == SourceId::Git {
                for host in &config.git_hosts {
                    if !hosts.contains(host) {
                        hosts.push(host.clone());
                    }
                }
            }
            let configured = config.sources.iter().any(|(name, source)| {
                SourceId::from_wire(name) == Some(id) && source.enabled.is_some()
            });
            EffectiveSource {
                id,
                enabled: present && view.enabled(id),
                requires_credential: id.requires_credential(),
                credential_configured: credential(id),
                hosts,
                source: from(
                    present
                        && (configured || (id == SourceId::Git && !config.git_hosts.is_empty())),
                ),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn parsed(text: &str) -> Config {
        Config::parse(text, Path::new("/x/config.toml")).unwrap().0
    }

    #[test]
    fn builtin_effective_values_equal_the_registries() {
        let e = Effective::builtin();
        let registry: Vec<&LimitInfo> = crate::politeness::REGISTRY
            .iter()
            .chain(limits::REGISTRY)
            .collect();
        assert_eq!(e.limits.len(), registry.len());
        for (limit, info) in e.limits.iter().zip(registry) {
            assert_eq!(limit.info.name, info.name);
            assert_eq!(limit.value, info.value, "{}", info.name);
            assert_eq!(limit.source, ValueSource::Builtin, "{}", info.name);
        }
        assert_eq!(
            e.sources.iter().map(|s| s.id).collect::<Vec<_>>(),
            SourceId::ALL.to_vec()
        );
        for source in &e.sources {
            let off = matches!(source.id, SourceId::Curseforge | SourceId::Wago);
            assert_eq!(source.enabled, !off, "{:?}", source.id);
            assert!(!source.credential_configured, "{:?}", source.id);
            assert_eq!(source.source, ValueSource::Builtin, "{:?}", source.id);
            assert_eq!(
                source.hosts,
                hosts_for(source.id)
                    .iter()
                    .map(|h| (*h).to_owned())
                    .collect::<Vec<_>>()
            );
        }
        let table = RulesTable::builtin();
        for family in &e.families {
            assert_eq!(family.range, table.family_range(family.family));
            assert_eq!(family.legacy, family.family.is_legacy());
            assert_eq!(family.source, ValueSource::Builtin);
        }
        assert_eq!(e.families.len(), Family::ALL.len());
        let rows: Vec<&str> = e
            .client_rules
            .iter()
            .map(|r| r.rules.name.as_str())
            .collect();
        let mut expected: Vec<&str> = table
            .client_rules()
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        expected.push(table.fallback(InstallKind::Battlenet).name.as_str());
        expected.push(table.fallback(InstallKind::Custom).name.as_str());
        assert_eq!(rows, expected);
        assert!(
            e.client_rules
                .iter()
                .all(|r| r.source == ValueSource::Builtin)
        );
    }

    #[test]
    fn config_overrides_are_effective_and_marked() {
        let config = parsed(
            "check_interval_hours = 12\ngit_hosts = [\"codeberg.org\"]\n\
             [limits]\ndownload_max_bytes = \"1GiB\"\ntx_retention_days = 3\n\
             [sources.wowi]\nenabled = false\n\
             [sources.curseforge]\nenabled = true\n\
             [rules.family_ranges.wrath]\nmin = 30400\nmax = 30499\n\
             [client_rules.mainline]\nmulti_toc = true\n",
        );
        let e = Effective::of(&config, &|id| id == SourceId::Github);
        let limit = |name: &str| e.limits.iter().find(|l| l.info.name == name).unwrap();
        assert_eq!(
            limit("DOWNLOAD_MAX_BYTES").value,
            LimitValue::Bytes(1 << 30)
        );
        assert_eq!(limit("DOWNLOAD_MAX_BYTES").source, ValueSource::Config);
        assert_eq!(
            limit("DOWNLOAD_MAX_BYTES").config_key,
            Some("limits.download_max_bytes")
        );
        assert_eq!(
            limit("DEFAULT_CHECK_INTERVAL").value,
            LimitValue::Millis(12 * 3_600_000)
        );
        let LimitValue::Retention { window_ms, .. } = limit("TX_RETENTION").value else {
            panic!("TX_RETENTION is a retention"); // ubs:ignore — the test's own failure
        };
        assert_eq!(window_ms, 3 * 86_400_000);
        assert_eq!(
            limit("TX_RETENTION").config_key,
            Some("limits.tx_retention_count")
        );
        assert_eq!(limit("ARCHIVE_MAX_ENTRIES").source, ValueSource::Builtin);
        let changed: Vec<&str> = e
            .limits
            .iter()
            .filter(|l| l.source == ValueSource::Config)
            .map(|l| l.info.name)
            .collect();
        assert_eq!(
            changed,
            [
                "DEFAULT_CHECK_INTERVAL",
                "DOWNLOAD_MAX_BYTES",
                "TX_RETENTION"
            ]
        );
        for limit in &e.limits {
            assert_eq!(
                std::mem::discriminant(&limit.value),
                std::mem::discriminant(&limit.info.value),
                "{} keeps its registry variant",
                limit.info.name
            );
        }

        let source = |id: SourceId| e.sources.iter().find(|s| s.id == id).unwrap();
        assert!(!source(SourceId::Wowi).enabled);
        assert_eq!(source(SourceId::Wowi).source, ValueSource::Config);
        assert!(!source(SourceId::Curseforge).enabled, "never in this build");
        assert_eq!(source(SourceId::Curseforge).source, ValueSource::Builtin);
        assert_eq!(source(SourceId::Git).source, ValueSource::Config);
        assert_eq!(
            source(SourceId::Git).hosts.last().map(String::as_str),
            Some("codeberg.org")
        );
        assert!(source(SourceId::Github).credential_configured);
        assert_eq!(source(SourceId::Github).source, ValueSource::Builtin);

        let family = |f: Family| e.families.iter().find(|x| x.family == f).unwrap();
        assert_eq!(family(Family::Wrath).source, ValueSource::Config);
        assert_eq!(family(Family::Mists).source, ValueSource::Builtin);
        let row = |name: &str| {
            e.client_rules
                .iter()
                .find(|r| r.rules.name == name)
                .unwrap()
        };
        assert_eq!(row("mainline").source, ValueSource::Config);
        assert_eq!(row("forever").source, ValueSource::Builtin);
    }

    #[test]
    fn a_zero_check_interval_keeps_the_default() {
        let e = Effective::of(&parsed("check_interval_hours = 0\n"), &|_| false);
        let interval = e
            .limits
            .iter()
            .find(|l| l.info.name == "DEFAULT_CHECK_INTERVAL")
            .unwrap();
        assert_eq!(interval.source, ValueSource::Builtin);
        assert_eq!(interval.value, interval.info.value);
    }

    #[test]
    fn a_key_set_to_the_builtin_value_is_still_config() {
        let builtin = match limits::REGISTRY
            .iter()
            .find(|l| l.name == "ARCHIVE_MAX_ENTRIES")
            .map(|l| l.value)
        {
            Some(LimitValue::Count(n)) => n,
            other => panic!("ARCHIVE_MAX_ENTRIES is a count: {other:?}"), // ubs:ignore — the test's own failure
        };
        let e = Effective::of(
            &parsed(&format!("[limits]\narchive_max_entries = {builtin}\n")),
            &|_| false,
        );
        let limit = e
            .limits
            .iter()
            .find(|l| l.info.name == "ARCHIVE_MAX_ENTRIES")
            .unwrap();
        assert_eq!(limit.source, ValueSource::Config);
        assert_eq!(limit.value, LimitValue::Count(builtin));
    }
}
