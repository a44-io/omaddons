//! What a command sees of the machine: the state paths, the configuration, the rules,
//! and every discovered target with its identity. Read commands build it without the
//! lock; the only write is the derived discovery cache. A broken config.toml fails here
//! (`state_corrupt`), which is why `help` and `config path` never build a world.

use crate::cli::Context;
use crate::discover::cache::{DiscoveryMode, discover_cached};
use crate::discover::registry::{Resolved, reconcile, resolve_one};
use crate::discover::{DiscoveredTarget, DiscoveryInput, ScanLimits};
use crate::error::{Error, Result, Warning};
use crate::model::TargetKey;
use crate::rules::RulesTable;
use crate::state::config::Config;
use crate::state::paths::Paths;
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
        let input = DiscoveryInput {
            home: paths.home().to_path_buf(),
            extra_roots,
            ..DiscoveryInput::default()
        };
        let (found, _) = discover_cached(&input, &table, &paths, ScanLimits::default(), mode);
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
        })
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
        match resolve_one(selector, &self.resolved.targets, &ctx.argv) {
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

    /// The targets a read command acts on: the `-t` target, or every target sorted by
    /// id.
    pub fn select(&self, ctx: &Context) -> Result<Vec<&DiscoveredTarget>> {
        if let Some(selector) = &ctx.target {
            return resolve_one(selector, &self.resolved.targets, &ctx.argv).map(|t| vec![t]);
        }
        let mut all: Vec<&DiscoveredTarget> = self.resolved.targets.iter().collect();
        all.sort_by(|a, b| a.target.id.cmp(&b.target.id));
        Ok(all)
    }
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
