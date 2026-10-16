//! What a command sees of the machine: the state paths, the configuration, the rules,
//! and every discovered target with its identity. Read commands build it without the
//! lock; the only write is the derived discovery cache. A broken config.toml fails here
//! (`state_corrupt`), which is why `help` and `config path` never build a world.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::cli::Context;
use crate::discover::cache::{CacheStats, DiscoveryMode, discover_cached};
use crate::discover::registry::{Resolved, reconcile, resolve, resolve_one};
use crate::discover::{DiscoveredTarget, DiscoveryInput, DiscoverySource, ScanLimits};
use crate::error::{Error, Result, Warning};
use crate::model::TargetKey;
use crate::rules::RulesTable;
use crate::state::config::Config;
use crate::state::paths::Paths;
use crate::state::records::{TargetRecord, load_record};
use crate::state::registry::{Origin, Registry, load_registry};

/// The discovered targets, reconciled with the registry.
pub struct World {
    pub paths: Paths,
    pub config: Config,
    /// The builtin rules with config overrides applied.
    pub table: RulesTable,
    pub registry: Registry,
    pub resolved: Resolved,
    /// Config, discovery, registry and reconciliation warnings, for the outcome.
    pub warnings: Vec<Warning>,
    /// Debug notes from discovery.
    pub diagnostics: Vec<String>,
    /// Whether discovery scanned, reused the cache, or wrote it.
    pub discovery: CacheStats,
}

impl World {
    /// Discovers every target under the user's home, config's `extra_roots` and the
    /// registry's manual roots, reusing unchanged roots from the discovery cache, and
    /// gives each its registered or provisional identity and config overrides.
    pub fn load() -> Result<Self> {
        Self::load_with(DiscoveryMode::Full)
    }

    /// [`World::load`] with an explicit cache mode (`status --cached`, `--rescan`).
    pub fn load_with(mode: DiscoveryMode) -> Result<Self> {
        let paths = Paths::from_env()?;
        let loaded_config = Config::load(&paths)?;
        let config = loaded_config.config;
        let mut warnings = loaded_config.warnings;
        let table = config
            .rules_table(&RulesTable::builtin())
            .map_err(|e| Error::internal(format!("validated rules overrides failed: {e}")))?;
        let loaded = load_registry(&paths)?;
        warnings.extend(loaded.warning(&paths.registry_file()));
        let registry = loaded.value;
        let mut extra_roots = config.extra_roots.clone();
        extra_roots.extend(
            registry
                .targets
                .iter()
                .filter(|e| e.origin == Origin::Manual)
                .map(|e| e.install_root.clone()),
        );
        let mut registered_roots: Vec<(PathBuf, String, DiscoverySource)> = Vec::new();
        for e in &registry.targets {
            if !registered_roots
                .iter()
                .any(|(root, ..)| *root == e.install_root)
            {
                let source = match e.origin {
                    Origin::Manual => DiscoverySource::Manual,
                    Origin::Discovered => DiscoverySource::Scan,
                };
                registered_roots.push((e.install_root.clone(), e.qualifier.clone(), source));
            }
        }
        let input = DiscoveryInput {
            home: paths.home().to_path_buf(),
            extra_roots,
            registered_roots,
            config_home: Some(paths.config_home().to_path_buf()),
            data_home: Some(paths.data_home().to_path_buf()),
            ..DiscoveryInput::default()
        };
        let (found, discovery) =
            discover_cached(&input, &table, &paths, ScanLimits::default(), mode);
        warnings.extend(found.warnings);
        let mut resolved = reconcile(&registry, &found.targets);
        warnings.extend(resolved.warnings.iter().cloned());
        apply_target_overrides(&mut resolved, &config, &table);
        // Unfinished transactions, reported without recovering (D23). A mutating
        // command recovers them under the lock and reports `journal_recovered` instead.
        warnings.extend(crate::engine::journal::pending_warnings(&paths)?);
        Ok(Self {
            paths,
            config,
            table,
            registry,
            resolved,
            warnings,
            diagnostics: found.diagnostics,
            discovery,
        })
    }

    /// The record of every registered discovered target, by target id.
    pub fn records(&self) -> Result<BTreeMap<String, TargetRecord>> {
        let mut records = BTreeMap::new();
        for d in &self.resolved.targets {
            if let Some(key) = d.target.key {
                records.insert(d.target.id.clone(), load_record(&self.paths, &key)?.value);
            }
        }
        Ok(records)
    }

    /// The id of the target registered as `key`: discovered now, else as the registry
    /// remembers it, else the key itself.
    pub fn target_id(&self, key: TargetKey) -> String {
        self.resolved
            .targets
            .iter()
            .find(|d| d.target.key == Some(key))
            .map(|d| d.target.id.clone())
            .or_else(|| {
                self.registry
                    .targets
                    .iter()
                    .find(|e| e.key == key)
                    .map(|e| e.id.clone())
            })
            .unwrap_or_else(|| key.to_string())
    }

    /// The key `-t` names for commands that read history: a discovered target's key,
    /// else a registry entry's (by id or key) whose client is gone. `None` for a
    /// discovered target that was never registered (it has no history).
    pub fn history_key(&self, ctx: &Context) -> Result<Option<Option<TargetKey>>> {
        let Some(selector) = &ctx.target else {
            return Ok(None);
        };
        match resolve_one(selector, &self.resolved.targets, &self.table, &ctx.argv) {
            Ok(found) => Ok(Some(found.target.key)),
            Err(error) => {
                let key = selector.parse::<TargetKey>().ok();
                self.registry
                    .targets
                    .iter()
                    .find(|e| Some(e.key) == key || e.id.eq_ignore_ascii_case(selector))
                    .map(|e| Some(Some(e.key)))
                    .ok_or(error)
            }
        }
    }

    /// The targets a command that acts on several clients covers (M2-D24): every
    /// discovered target without `-t`, else every match of the first selector kind
    /// that matches (`-t vanilla` is every vanilla client, `-t sod` the Classic Era
    /// client); sorted by id, without duplicates. For `status`, `list`, `info`, and
    /// later `outdated`, `update`, `export`, `sync`, `adopt`, `pin`/`unpin` and
    /// `search`. Commands that must land on one client (`install`, `remove`, `copy`'s
    /// sides, `adopt --ref`, `history` and `undo`) use `resolve_one`, whose ambiguity is
    /// an error.
    pub fn select_many(&self, ctx: &Context) -> Result<Vec<&DiscoveredTarget>> {
        select_many(ctx.target.as_deref(), &self.resolved.targets, &self.table)
    }
}

/// [`World::select_many`] over `targets`.
fn select_many<'a>(
    selector: Option<&str>,
    targets: &'a [DiscoveredTarget],
    table: &RulesTable,
) -> Result<Vec<&'a DiscoveredTarget>> {
    let mut selected: Vec<&DiscoveredTarget> = match selector {
        Some(selector) => resolve(selector, targets, table)?,
        None => targets.iter().collect(),
    };
    selected.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    // The same client twice (by key, else by directory) is listed once.
    let mut seen: Vec<(Option<TargetKey>, &std::path::Path)> = Vec::new();
    selected.retain(|d| {
        let identity = (d.target.key, d.target.client_dir.as_path());
        let duplicate = seen.iter().any(|(key, dir)| match (key, identity.0) {
            (Some(key), Some(other)) => *key == other,
            _ => *dir == identity.1,
        });
        if !duplicate {
            seen.push(identity);
        }
        !duplicate
    });
    Ok(selected)
}

/// `[targets.<id or key>]`: a label, and a family that also picks the rules row.
fn apply_target_overrides(resolved: &mut Resolved, config: &Config, table: &RulesTable) {
    for d in &mut resolved.targets {
        let key = d.target.key.map(|k| k.to_string());
        let found = config
            .targets
            .get(&d.target.id)
            .or_else(|| key.as_ref().and_then(|k| config.targets.get(k)));
        let Some(o) = found else { continue };
        if let Some(label) = &o.label {
            d.target.label = Some(label.clone());
        }
        if let Some(family) = o.family {
            d.target.family = family;
            d.target.refresh_rules(table);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::rules::Family;

    fn found(id: &str, family: Family, product: Option<&str>) -> DiscoveredTarget {
        let mut target = crate::engine::testing::target(&PathBuf::from(format!("/c/{id}")));
        target.id = id.into();
        target.family = family;
        target.product = product.map(Into::into);
        target.key = Some(TargetKey::new());
        DiscoveredTarget {
            target,
            default_id: id.into(),
            qualifier: "test".into(),
            source: DiscoverySource::Scan,
        }
    }

    fn ids(selected: &[&DiscoveredTarget]) -> Vec<String> {
        selected.iter().map(|d| d.target.id.clone()).collect()
    }

    #[test]
    fn many_targets_fan_out_by_the_first_selector_kind_that_matches() {
        let table = RulesTable::builtin();
        let targets = vec![
            found("retail", Family::Mainline, Some("wow")),
            found("turtle", Family::Vanilla, None),
            found("era", Family::Vanilla, Some("wow_classic_era")),
            found("ptr", Family::Mainline, Some("wowt")),
        ];
        let select = |s: Option<&str>| select_many(s, &targets, &table).map(|t| ids(&t));
        assert_eq!(
            select(None).unwrap(),
            ["era", "ptr", "retail", "turtle"],
            "everything, by id"
        );
        assert_eq!(select(Some("retail")).unwrap(), ["retail"]);
        assert_eq!(
            select(Some("vanilla")).unwrap(),
            ["era", "turtle"],
            "a family"
        );
        assert_eq!(select(Some("mainline")).unwrap(), ["ptr", "retail"]);
        assert_eq!(select(Some("sod")).unwrap(), ["era"], "a mode's client");
        let key = targets[1].target.key.unwrap().to_string();
        assert_eq!(select(Some(&key)).unwrap(), ["turtle"], "a key");
        let error = select(Some("nosuch")).unwrap_err();
        assert_eq!(error.code, ErrorCode::TargetNotFound);
        let one = resolve_one("nosuch", &targets, &table, &[]).unwrap_err();
        assert_eq!(
            (&error.message, &error.fix, &error.details),
            (&one.message, &one.fix, &one.details),
            "the same error as a single-target command's"
        );

        // An id beats a family: a 1.12.1 server keeps its default id `vanilla`.
        let mut with_legacy = targets.clone();
        with_legacy.push(found("vanilla", Family::VanillaLegacy, None));
        let selected = select_many(Some("vanilla"), &with_legacy, &table).unwrap();
        assert_eq!(ids(&selected), ["vanilla"]);
    }

    #[test]
    fn the_same_client_is_selected_once() {
        let table = RulesTable::builtin();
        let era = found("era", Family::Vanilla, Some("wow_classic_era"));
        let mut again = era.clone();
        again.target.id = "era@wine".into();
        let mut unregistered = found("turtle", Family::Vanilla, None);
        unregistered.target.key = None;
        let mut same_dir = unregistered.clone();
        same_dir.target.id = "turtle@steam".into();
        let targets = vec![era, again, unregistered, same_dir];
        let selected = select_many(Some("vanilla"), &targets, &table).unwrap();
        assert_eq!(ids(&selected), ["era", "turtle"]);
    }
}
