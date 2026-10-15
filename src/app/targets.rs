//! `omaddons targets [add <path> [--name N] | rm <id> | rename <id> <new> | default <id>]
//! [--rescan]`: list the clients and manage their identities (vision §5.1, §5.9).
//!
//! Listing is read-only apart from the derived discovery cache (`--rescan` bypasses
//! and refreshes it). The rest take the global lock, write the registry and markers
//! through fsx, and are idempotent; they are not transactions and not undoable.
//! `--dry-run` reports what would change and writes nothing.

use std::io::Write;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

use super::world::World;
use crate::cli::{Context, fix_argv};
use crate::discover::cache::DiscoveryMode;
use crate::discover::registry::{
    LockedRegistry, check_user_id, manual_targets, reconcile, registered_at, unknown_id,
};
use crate::discover::{DiscoveredTarget, DiscoverySource};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::fsx::{GlobalLock, ScopeTable, WriteSession, Writer};
use crate::model::TargetKey;
use crate::output::{Outcome, Style, table};
use crate::rules::{ClientVersion, Family, InstallKind, RulesTable, TargetKind};
use crate::state::paths::Paths;
use crate::state::records::load_record;
use crate::state::registry::{Origin, Registry, RegistryEntry, load_registry};

/// `targets`' flags and actions.
#[derive(Debug, Clone, clap::Args)]
pub struct TargetsArgs {
    #[command(subcommand)]
    pub action: Option<TargetsAction>,
    /// Ignore the discovery cache and rescan every root
    #[arg(long)]
    pub rescan: bool,
}

/// What `targets` does besides listing.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum TargetsAction {
    /// Register a client that discovery cannot find
    Add {
        /// A Battle.net root, a flavor folder, a custom client root, or its Interface or AddOns directory
        path: PathBuf,
        /// The id to give it (one client only)
        #[arg(long)]
        name: Option<String>,
    },
    /// Unregister a target (nothing in its directory is touched)
    Rm {
        /// The target's id
        id: String,
    },
    /// Give a target a new id
    Rename {
        /// The target's current id
        id: String,
        /// The new id
        new: String,
    },
    /// Make a target the default for commands without -t
    Default {
        /// The target's id
        id: String,
    },
}

impl TargetsAction {
    /// The envelope `command`.
    pub fn command(&self) -> &'static str {
        match self {
            Self::Add { .. } => "targets add",
            Self::Rm { .. } => "targets rm",
            Self::Rename { .. } => "targets rename",
            Self::Default { .. } => "targets default",
        }
    }
}

// ---------------------------------------------------------------------------------------
// Data.

/// One target as `targets` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TargetInfo {
    pub id: String,
    pub key: Option<TargetKey>,
    pub registered: bool,
    pub default: bool,
    pub family: Family,
    pub kind: TargetKind,
    pub install_kind: InstallKind,
    pub product: Option<String>,
    pub version: ClientVersion,
    pub interface: u32,
    pub label: Option<String>,
    pub client_dir: PathBuf,
    /// Registered, but its directory was not found.
    pub missing: bool,
    /// `product_db`, `scan`, `launcher:<name>`, `process`, `extra_root`, `manual`, or
    /// `registry` for a missing target.
    pub found_by: String,
    /// `-v` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<TargetDetails>,
}

/// `-v` extras.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TargetDetails {
    pub install_root: PathBuf,
    pub flavor_dir: Option<String>,
    pub exe_name: Option<String>,
    /// The client-rules row.
    pub rules: String,
    pub aliases: Vec<String>,
}

/// `targets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TargetsData {
    /// Sorted by id.
    pub targets: Vec<TargetInfo>,
    /// The default target's id.
    pub default: Option<String>,
    pub rescanned: bool,
}

/// `targets add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AddData {
    pub changed: bool,
    pub dry_run: bool,
    pub targets: Vec<TargetInfo>,
}

/// `targets rm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RmData {
    pub id: String,
    pub key: TargetKey,
    /// Addons its record still lists (the record is kept).
    pub managed_addons: usize,
    pub was_default: bool,
    pub dry_run: bool,
}

/// `targets rename`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RenameData {
    /// `null` only on a dry run for an unregistered target.
    pub key: Option<TargetKey>,
    pub from: String,
    pub to: String,
    pub dry_run: bool,
}

/// `targets default`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DefaultData {
    pub id: String,
    /// `null` only on a dry run for an unregistered target.
    pub key: Option<TargetKey>,
    /// config.toml's `default_target` names another target and wins.
    pub overridden_by_config: bool,
    pub dry_run: bool,
}

/// A `targets` outcome: its command, data and human rendering.
pub struct Done<D> {
    command: &'static str,
    data: D,
    warnings: Vec<Warning>,
    render: fn(&D, &mut dyn Write, &Style) -> std::io::Result<()>,
}

impl<D: Serialize + JsonSchema> Outcome for Done<D> {
    type Data = D;

    fn command(&self) -> &'static str {
        self.command
    }

    fn data(&self) -> &D {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        (self.render)(&self.data, w, style)
    }
}

// ---------------------------------------------------------------------------------------
// Listing.

/// `targets [--rescan]`.
pub fn list(ctx: &Context, rescan: bool) -> Result<Done<TargetsData>> {
    let mode = if rescan {
        DiscoveryMode::Rescan
    } else {
        DiscoveryMode::Full
    };
    let world = World::load_with(mode)?;
    let verbose = ctx.output.style.verbose > 0;
    let default = world.registry.default;
    let mut targets: Vec<TargetInfo> = world
        .resolved
        .targets
        .iter()
        .map(|d| info(d, &world.registry, verbose))
        .collect();
    targets.extend(
        world
            .resolved
            .missing
            .iter()
            .map(|e| missing_info(e, default, verbose)),
    );
    targets.sort_by(|a, b| a.id.cmp(&b.id));
    let default = targets.iter().find(|t| t.default).map(|t| t.id.clone());
    Ok(Done {
        command: "targets",
        data: TargetsData {
            targets,
            default,
            rescanned: rescan,
        },
        warnings: world.warnings,
        render: render_list,
    })
}

fn found_by(d: &DiscoveredTarget, registry: &Registry) -> String {
    if registered_at(registry, &d.target.client_dir).is_some_and(|e| e.origin == Origin::Manual) {
        return "manual".to_owned();
    }
    match &d.source {
        DiscoverySource::ProductDb => "product_db".to_owned(),
        DiscoverySource::Launcher { name } => format!("launcher:{name}"),
        DiscoverySource::Process => "process".to_owned(),
        DiscoverySource::ExtraRoot => "extra_root".to_owned(),
        DiscoverySource::Scan => "scan".to_owned(),
        DiscoverySource::Manual => "manual".to_owned(),
    }
}

fn info(d: &DiscoveredTarget, registry: &Registry, verbose: bool) -> TargetInfo {
    let t = &d.target;
    TargetInfo {
        id: t.id.clone(),
        key: t.key,
        registered: t.key.is_some(),
        // ubs:ignore — a target key is a public ULID identifier, not a secret
        default: t.key.is_some() && t.key == registry.default,
        family: t.family,
        kind: t.kind,
        install_kind: t.install_kind,
        product: t.product.clone(),
        version: t.version,
        interface: t.interface,
        label: t.label.clone(),
        client_dir: t.client_dir.clone(),
        missing: false,
        found_by: found_by(d, registry),
        details: verbose.then(|| TargetDetails {
            install_root: t.install_root.clone(),
            flavor_dir: t.flavor_dir.clone(),
            exe_name: t.exe_name.clone(),
            rules: t.rules.name.clone(),
            aliases: vec![t.id.clone()],
        }),
    }
}

fn missing_info(e: &RegistryEntry, default: Option<TargetKey>, verbose: bool) -> TargetInfo {
    TargetInfo {
        id: e.id.clone(),
        key: Some(e.key),
        registered: true,
        // ubs:ignore — a target key is a public ULID identifier, not a secret
        default: default == Some(e.key),
        family: Family::Unknown,
        kind: TargetKind::Live,
        install_kind: if e.flavor_dir.is_some() {
            InstallKind::Battlenet
        } else {
            InstallKind::Custom
        },
        product: e.product.clone(),
        version: ClientVersion::new(0, 0, 0),
        interface: 0,
        label: None,
        client_dir: e.client_dir.clone(),
        missing: true,
        found_by: "registry".to_owned(),
        details: verbose.then(|| TargetDetails {
            install_root: e.install_root.clone(),
            flavor_dir: e.flavor_dir.clone(),
            exe_name: None,
            rules: String::new(),
            aliases: vec![e.id.clone()],
        }),
    }
}

// ---------------------------------------------------------------------------------------
// Changes.

/// Paths and rules without a scan, for the changes that do not need one.
fn base() -> Result<(Paths, RulesTable)> {
    Ok((Paths::from_env()?, RulesTable::builtin()))
}

/// Runs `f` with the global lock held and a session whose scope covers `dirs`. A lock
/// timeout's fix is the caller's exact command.
fn locked<T>(
    ctx: &Context,
    paths: &Paths,
    command: &str,
    dirs: &[PathBuf],
    f: impl FnOnce(&mut LockedRegistry<'_>, &mut WriteSession<'_>) -> Result<T>,
) -> Result<T> {
    let guard = GlobalLock::acquire(paths, command).map_err(|mut err| {
        if err.code == ErrorCode::Locked {
            err.fix = Suggestion::command(fix_argv(&ctx.argv));
        }
        err
    })?;
    let scopes = ScopeTable::new(paths, dirs, None)?;
    let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "registry");
    let mut registry = LockedRegistry::load(&guard, paths)?;
    f(&mut registry, &mut s)
}

/// `targets add <path> [--name N]`.
pub fn add(ctx: &Context, path: &Path, name: Option<&str>) -> Result<Done<AddData>> {
    let world = World::load()?;
    let (paths, table) = (&world.paths, &world.table);
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let (found, warnings) = manual_targets(table, &path)?;
    let registry = load_registry(paths)?.value;
    let changed = found
        .iter()
        .any(|d| registered_at(&registry, &d.target.client_dir).is_none());
    if let Some(name) = name
        && changed
    {
        let own: Vec<&Path> = found
            .iter()
            .map(|d| d.target.client_dir.as_path())
            .collect();
        check_free(&world, name, &own)?;
    }
    let render = render_add as fn(&AddData, &mut dyn Write, &Style) -> std::io::Result<()>;
    if ctx.dry_run {
        let shown = reconcile(&registry, &found).targets;
        let targets = shown.iter().map(|d| info(d, &registry, false)).collect();
        return Ok(Done {
            command: "targets add",
            data: AddData {
                changed,
                dry_run: true,
                targets,
            },
            warnings,
            render,
        });
    }
    let dirs: Vec<PathBuf> = found.iter().map(|d| d.target.client_dir.clone()).collect();
    let (added, after) = locked(ctx, paths, "targets add", &dirs, |reg, s| {
        let (added, _) = reg.add_manual(s, table, &path, name)?;
        Ok((added, reg.registry().clone()))
    })?;
    let targets = added.iter().map(|d| info(d, &after, false)).collect();
    Ok(Done {
        command: "targets add",
        data: AddData {
            changed,
            dry_run: false,
            targets,
        },
        warnings,
        render,
    })
}

/// `targets rm <id>`.
pub fn rm(ctx: &Context, id: &str) -> Result<Done<RmData>> {
    let (paths, _) = base()?;
    let registry = load_registry(&paths)?.value;
    let entry = registry
        .entry_by_id(id)
        .ok_or_else(|| unknown_id(id))?
        .clone();
    let managed_addons = load_record(&paths, &entry.key)?.value.addons.len();
    // ubs:ignore — a target key is a public ULID identifier, not a secret
    let was_default = registry.default == Some(entry.key);
    if !ctx.dry_run {
        locked(ctx, &paths, "targets rm", &[], |reg, s| reg.remove(s, id))?;
    }
    Ok(Done {
        command: "targets rm",
        data: RmData {
            id: entry.id,
            key: entry.key,
            managed_addons,
            was_default,
            dry_run: ctx.dry_run,
        },
        warnings: Vec::new(),
        render: render_rm,
    })
}

/// A user-chosen id must satisfy the registry's rules and must not be any id shown
/// right now, provisional ones included (except the target's own: `own` client dirs).
fn check_free(world: &World, new: &str, own: &[&Path]) -> Result<()> {
    check_user_id(&world.registry, new, &world.table)?;
    if let Some(holder) = world
        .resolved
        .targets
        .iter()
        .find(|d| d.target.id == new && !own.contains(&d.target.client_dir.as_path()))
    {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "{new:?} is already the id of the target at {}",
                holder.target.client_dir.display()
            ),
            Suggestion::text(format!(
                "pick another id, or rename that target first: omaddons targets rename {new} <new id>"
            )),
        ));
    }
    Ok(())
}

/// The target `id` names exactly: a discovered target (registered or provisional) or
/// a registered one that is missing.
enum Named {
    Found(Box<DiscoveredTarget>),
    Missing(RegistryEntry),
}

fn named(world: &World, id: &str) -> Result<Named> {
    if let Some(d) = world.resolved.targets.iter().find(|d| d.target.id == id) {
        return Ok(Named::Found(Box::new(d.clone())));
    }
    world
        .registry
        .entry_by_id(id)
        .map(|e| Named::Missing(e.clone()))
        .ok_or_else(|| unknown_id(id))
}

/// Registers the target if needed (under the lock) and returns its registered id.
fn registered_id(
    reg: &mut LockedRegistry<'_>,
    s: &mut WriteSession<'_>,
    target: &Named,
) -> Result<String> {
    match target {
        Named::Missing(e) => Ok(e.id.clone()),
        Named::Found(d) => {
            let key = reg.ensure_registered(s, d)?;
            Ok(reg
                .registry()
                .entry(&key)
                .map_or_else(|| d.target.id.clone(), |e| e.id.clone()))
        }
    }
}

fn named_parts(target: &Named) -> (Option<TargetKey>, Vec<PathBuf>) {
    match target {
        Named::Found(d) => (d.target.key, vec![d.target.client_dir.clone()]),
        Named::Missing(e) => (Some(e.key), Vec::new()),
    }
}

/// `targets rename <id> <new>`; an unregistered target is registered first.
pub fn rename(ctx: &Context, id: &str, new: &str) -> Result<Done<RenameData>> {
    let world = World::load()?;
    let target = named(&world, id)?;
    let (key, dirs) = named_parts(&target);
    if new != id {
        let own: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
        check_free(&world, new, &own)?;
    }
    let key = if ctx.dry_run {
        key
    } else {
        Some(locked(
            ctx,
            &world.paths,
            "targets rename",
            &dirs,
            |reg, s| {
                let current = registered_id(reg, s, &target)?;
                Ok(reg.rename(s, &world.table, &current, new)?.key)
            },
        )?)
    };
    Ok(Done {
        command: "targets rename",
        data: RenameData {
            key,
            from: id.to_owned(),
            to: new.to_owned(),
            dry_run: ctx.dry_run,
        },
        warnings: Vec::new(),
        render: render_rename,
    })
}

/// `targets default <id>`; an unregistered target is registered first.
pub fn default(ctx: &Context, id: &str) -> Result<Done<DefaultData>> {
    let world = World::load()?;
    let target = named(&world, id)?;
    let (key, dirs) = named_parts(&target);
    let (key, id) = if ctx.dry_run {
        (key, id.to_owned())
    } else {
        locked(ctx, &world.paths, "targets default", &dirs, |reg, s| {
            let current = registered_id(reg, s, &target)?;
            Ok((Some(reg.set_default(s, &current)?), current))
        })?
    };
    let overridden_by_config = world
        .config
        .default_target
        .as_ref()
        .is_some_and(|c| *c != id);
    Ok(Done {
        command: "targets default",
        data: DefaultData {
            id,
            key,
            overridden_by_config,
            dry_run: ctx.dry_run,
        },
        warnings: Vec::new(),
        render: render_default,
    })
}

// ---------------------------------------------------------------------------------------
// Human output.

fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn rows(targets: &[TargetInfo], style: &Style) -> Vec<Vec<String>> {
    targets
        .iter()
        .map(|t| {
            let key = t
                .key
                .map_or_else(|| "-".to_owned(), |k| k.to_string()[..6].to_owned() + "…");
            let mut notes = Vec::new();
            notes.extend(
                t.label
                    .clone()
                    .filter(|l| *l != format!("{} (battlenet)", t.id)),
            );
            if !t.registered {
                notes.push("(unregistered)".to_owned());
            }
            if t.missing {
                notes.push(style.warn("(missing)"));
            }
            notes.push(t.client_dir.display().to_string());
            vec![
                format!("{} {}", if t.default { "*" } else { " " }, t.id),
                key,
                wire(&t.family),
                t.version.to_string(),
                wire(&t.kind),
                notes.join("  "),
            ]
        })
        .collect()
}

fn render_list(data: &TargetsData, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
    if data.targets.is_empty() {
        return writeln!(
            w,
            "No WoW client found. Add one with: omaddons targets add <path>"
        );
    }
    w.write_all(table(&rows(&data.targets, style)).as_bytes())
}

fn render_add(data: &AddData, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
    let verb = match (data.dry_run, data.changed) {
        (true, true) => "would register",
        (true, false) | (false, false) => "already registered",
        (false, true) => "registered",
    };
    writeln!(w, "{verb}:")?;
    w.write_all(table(&rows(&data.targets, style)).as_bytes())
}

fn render_rm(data: &RmData, w: &mut dyn Write, _: &Style) -> std::io::Result<()> {
    let verb = if data.dry_run {
        "would unregister"
    } else {
        "unregistered"
    };
    writeln!(w, "{verb} {} (its files are untouched)", data.id)?;
    if data.managed_addons > 0 {
        writeln!(
            w,
            "its record still lists {} addons; `omaddons targets add` restores it",
            data.managed_addons
        )?;
    }
    Ok(())
}

fn render_rename(data: &RenameData, w: &mut dyn Write, _: &Style) -> std::io::Result<()> {
    let verb = if data.dry_run {
        "would rename"
    } else {
        "renamed"
    };
    writeln!(w, "{verb} {} to {}", data.from, data.to)
}

fn render_default(data: &DefaultData, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
    let verb = if data.dry_run { "would make" } else { "made" };
    writeln!(w, "{verb} {} the default target", data.id)?;
    if data.overridden_by_config {
        writeln!(
            w,
            "{}",
            style.warn("config.toml's default_target names another target and wins")
        )?;
    }
    Ok(())
}
