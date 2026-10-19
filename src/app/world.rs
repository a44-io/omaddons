//! What a command sees of the machine: the state paths, the configuration, the rules,
//! and every discovered target with its identity. Read commands build it without the
//! lock; the only write is the derived discovery cache. A broken config.toml fails here
//! (`state_corrupt`), which is why `help` and `config path` never build a world.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::cli::Context;
use crate::discover::cache::{CacheStats, DiscoveryMode, discover_cached};
use crate::discover::registry::{Resolved, reconcile, resolve, resolve_one};
use crate::discover::running::process_prefixes;
use crate::discover::{DiscoveredTarget, DiscoveryInput, DiscoverySource, ScanLimits};
use crate::error::{Error, Result, Warning};
use crate::model::{AddonRecord, Target, TargetKey};
use crate::resolve::catalog::Catalog;
use crate::rules::RulesTable;
use crate::sources::SourceConfigView;
use crate::sources::github_token::GithubToken;
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
    /// The GitHub token REST requests carry, if any.
    pub github_token: Arc<GithubToken>,
}

impl World {
    /// Discovers every target under the user's home, config's `extra_roots` and the
    /// registry's manual roots, reusing unchanged roots from the discovery cache, and
    /// gives each its registered or provisional identity and config overrides. Each
    /// batch of warnings also goes to the run's notes as soon as it is found, so a
    /// failure later in the command still reports it.
    pub fn load(ctx: &Context) -> Result<Self> {
        Self::load_with(ctx, DiscoveryMode::Full)
    }

    /// [`World::load`] with an explicit cache mode (`status --cached`, `--rescan`).
    pub fn load_with(ctx: &Context, mode: DiscoveryMode) -> Result<Self> {
        let paths = Paths::from_env()?;
        let mut warnings: Vec<Warning> = Vec::new();
        let mut found = |batch: &[Warning]| {
            ctx.notes.warn_all(batch);
            warnings.extend_from_slice(batch);
        };
        let loaded_config = Config::load(&paths)?;
        found(&loaded_config.warnings);
        let config = loaded_config.config;
        let table = config
            .rules_table(&RulesTable::builtin())
            .map_err(|e| Error::internal(format!("validated rules overrides failed: {e}")))?;
        let loaded = load_registry(&paths)?;
        found(loaded.warning(&paths.registry_file()).as_slice());
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
        // Running clients' prefixes, for the modes that look for roots.
        let extra_prefixes = match mode {
            DiscoveryMode::Full | DiscoveryMode::Rescan => process_prefixes(&table, paths.home()),
            DiscoveryMode::NoScan => Vec::new(),
        };
        let input = DiscoveryInput {
            home: paths.home().to_path_buf(),
            extra_roots,
            extra_prefixes,
            registered_roots,
            config_home: Some(paths.config_home().to_path_buf()),
            data_home: Some(paths.data_home().to_path_buf()),
            ..DiscoveryInput::default()
        };
        let (discovered, discovery) =
            discover_cached(&input, &table, &paths, ScanLimits::default(), mode);
        found(&discovered.warnings);
        let mut resolved = reconcile(&registry, &discovered.targets);
        found(&resolved.warnings);
        apply_target_overrides(&mut resolved, &config, &table);
        // A client whose family is still unknown, once its override (if any) applied.
        let unknown: Vec<Warning> = resolved
            .targets
            .iter()
            .filter_map(|d| crate::discover::unknown_flavor(&d.target))
            .collect();
        found(&unknown);
        // Unfinished transactions, reported without recovering (D23). A mutating
        // command recovers them under the lock and reports `journal_recovered` instead:
        // the run's notes read them again when it ends.
        let journal = crate::engine::journal::pending_warnings(&paths)?;
        found(&journal);
        if !journal.is_empty() {
            let at = paths.clone();
            ctx.notes.journal_found(&journal, move || {
                crate::engine::journal::pending_warnings(&at).ok()
            });
        }
        // Resolved once; a `command` credential runs only when REST first needs it.
        let github_token = GithubToken::resolve(&paths, &config.sources);
        found(&github_token.warnings);
        Ok(Self {
            paths,
            config,
            table,
            registry,
            resolved,
            warnings,
            diagnostics: discovered.diagnostics,
            discovery,
            github_token,
        })
    }

    /// Which sources config.toml turns off, and the GitHub token: every registry a
    /// command builds honours them.
    pub fn sources(&self) -> SourceConfigView {
        SourceConfigView::from_config(&self.config.sources)
            .with_github_token(Arc::clone(&self.github_token))
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

    /// The records of the registered clients sharing `target`'s AddOns directory, by
    /// target id: what [`crate::inventory::inventory_shared`] classifies `shared` by.
    pub fn sibling_records(&self, target: &Target) -> Result<Vec<(String, TargetRecord)>> {
        crate::inventory::sibling_records(
            &self.paths,
            target,
            self.resolved.targets.iter().map(|d| &d.target),
        )
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
            // The family's rules, and the interface they give, as discovery would have.
            let t = &mut d.target;
            t.family = family;
            t.refresh_rules(table);
            t.interface = crate::discover::client_interface(
                table,
                &t.client_dir,
                &t.version,
                t.family,
                t.install_kind,
                &t.rules,
            );
        }
    }
}

/// Whether `name`, as a user typed it, names the installed `addon`: its name in any
/// case, or a catalog name or alias whose ref is the addon's (`update`, `pin`, `unpin`).
pub fn names_addon(name: &str, addon: &AddonRecord, catalog: &Catalog) -> bool {
    addon.name.eq_ignore_ascii_case(name)
        || catalog
            .by_name(name)
            .is_some_and(|e| e.r#ref.identity() == addon.r#ref.identity())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::rules::{ClientVersion, Family, InstallKind};

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

    /// The overridden family's rules, rules row and interface, after `last_addon_version`
    /// (when `Some`) is written to the client's `WTF/Config.wtf`.
    fn overridden(
        mut d: DiscoveredTarget,
        family: Family,
        last_addon_version: Option<u32>,
    ) -> (String, u32, Option<String>) {
        let dir = tempfile::tempdir().unwrap();
        d.target.client_dir = dir.path().to_path_buf();
        if let Some(last) = last_addon_version {
            std::fs::create_dir_all(dir.path().join("WTF")).unwrap();
            std::fs::write(
                dir.path().join("WTF/Config.wtf"),
                format!("SET lastAddonVersion \"{last}\"\n"),
            )
            .unwrap();
        }
        let id = d.target.id.clone();
        let mut resolved = Resolved {
            targets: vec![d],
            warnings: Vec::new(),
            missing: Vec::new(),
        };
        let mut config = Config::default();
        config.targets.insert(
            id,
            crate::state::config::TargetOverride {
                label: Some("mine".into()),
                family: Some(family),
            },
        );
        apply_target_overrides(&mut resolved, &config, &RulesTable::builtin());
        let t = &resolved.targets[0].target;
        assert_eq!(t.family, family);
        (t.rules.name.clone(), t.interface, t.label.clone())
    }

    #[test]
    fn a_family_override_recomputes_the_rules_and_the_interface() {
        // A 1.12 client stamped 1.18.1, read as vanilla: its own lastAddonVersion wins
        // on the legacy row it is moved to.
        let mut turtle = found("turtle", Family::Vanilla, None);
        turtle.target.install_kind = InstallKind::Custom;
        turtle.target.version = "1.18.1.7272".parse().unwrap();
        turtle.target.interface = 11_801;
        let legacy = Family::VanillaLegacy;
        assert_eq!(
            overridden(turtle.clone(), legacy, Some(11_201)),
            ("vanilla-legacy".into(), 11_201, Some("mine".into()))
        );
        assert_eq!(
            overridden(turtle, legacy, None),
            ("vanilla-legacy".into(), 11_200, Some("mine".into())),
            "without Config.wtf, the 1.16-1.49 version rule's pinned interface"
        );

        // A Battle.net folder with no readable version takes the family's current row;
        // its lastAddonVersion counts only inside the family's interfaces.
        let mut era = found("era", Family::Unknown, Some("wow_classic_era"));
        era.target.version = ClientVersion::new(0, 0, 0);
        era.target.interface = 0;
        let vanilla = |last| overridden(era.clone(), Family::Vanilla, last);
        assert_eq!(
            vanilla(Some(11_508)),
            ("vanilla".into(), 11_508, Some("mine".into()))
        );
        assert_eq!(vanilla(Some(120_105)).1, 0, "not a vanilla interface");
        assert_eq!(vanilla(None).1, 0);
        assert_eq!(overridden(era, Family::Forever, None).0, "forever");
    }
}
