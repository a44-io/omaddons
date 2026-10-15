//! What a command sees of the machine: the state paths, the rules, and every discovered
//! target with its identity. Read commands build it without the lock; the only write
//! is the derived discovery cache.

use crate::cli::Context;
use crate::discover::cache::{DiscoveryMode, discover_cached};
use crate::discover::registry::{Resolved, reconcile, resolve_one};
use crate::discover::{DiscoveredTarget, DiscoveryInput, ScanLimits};
use crate::error::{Result, Warning};
use crate::rules::RulesTable;
use crate::state::paths::Paths;
use crate::state::registry::{Origin, Registry, load_registry};

/// The discovered targets, reconciled with the registry.
pub struct World {
    pub paths: Paths,
    pub table: RulesTable,
    pub registry: Registry,
    pub resolved: Resolved,
    /// Discovery, registry and reconciliation warnings, for the outcome.
    pub warnings: Vec<Warning>,
    /// Debug notes from discovery.
    pub diagnostics: Vec<String>,
}

impl World {
    /// Discovers every target under the user's home and the registry's manual roots,
    /// reusing unchanged roots from the discovery cache, and gives each its registered
    /// or provisional identity.
    pub fn load() -> Result<Self> {
        Self::load_with(DiscoveryMode::Full)
    }

    /// [`World::load`] with an explicit cache mode (`status --cached`, `--rescan`).
    pub fn load_with(mode: DiscoveryMode) -> Result<Self> {
        let paths = Paths::from_env()?;
        let table = RulesTable::builtin();
        let loaded = load_registry(&paths)?;
        let mut warnings: Vec<Warning> =
            loaded.warning(&paths.registry_file()).into_iter().collect();
        let registry = loaded.value;
        let input = DiscoveryInput {
            home: paths.home().to_path_buf(),
            extra_roots: registry
                .targets
                .iter()
                .filter(|e| e.origin == Origin::Manual)
                .map(|e| e.install_root.clone())
                .collect(),
            ..DiscoveryInput::default()
        };
        let (found, _) = discover_cached(&input, &table, &paths, ScanLimits::default(), mode);
        warnings.extend(found.warnings);
        let resolved = reconcile(&registry, &found.targets);
        warnings.extend(resolved.warnings.iter().cloned());
        Ok(Self {
            paths,
            table,
            registry,
            resolved,
            warnings,
            diagnostics: found.diagnostics,
        })
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
