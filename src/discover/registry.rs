//! Target identity (vision §5.1, §5.2 steps 5 and 7): giving discovered targets their
//! registered keys and ids ([`reconcile`], read-only), registering them under the
//! global lock ([`LockedRegistry`]), and resolving `-t` selectors ([`resolve`]).
//!
//! A key is an immutable ULID, mirrored in `<client dir>/Interface/.omaddons/target.json`
//! so a moved install re-links. A copied install (two markers with one key) leaves the
//! key at the registered path; the copy gets a new key at its first registration. An id
//! is a human alias: the first target to register keeps the bare default id
//! (`retail`), later ones are qualified (`retail@steam`), and registered ids never
//! change except through `rename`. Read commands never write: unregistered targets get
//! `key: null` and a deterministic provisional id, given in discovery's route order
//! (`product.db`, launcher configs, processes, extra roots, the scan; then by path), so
//! the first-discovered target previews the bare id.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;

use super::{
    DiscoveredTarget, DiscoverySource, battlenet, custom, dir_slug, find_child, read_text,
};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{Content, GlobalLockGuard, Op, WriteSession, ensure_dir_all, mutate};
use crate::model::TargetKey;
use crate::rules::{Family, Product, RulesTable};
use crate::state::paths::Paths;
use crate::state::registry::{
    Origin, Registry, RegistryEntry, TargetMarker, load_registry, save_registry,
};
use crate::state::store::{self, Loaded, VersionStatus};

/// The marker of a client dir, under whichever case `Interface` has on disk.
pub fn marker_path(client_dir: &Path) -> PathBuf {
    find_child(client_dir, "Interface")
        .unwrap_or_else(|| client_dir.join("Interface"))
        .join(".omaddons")
        .join("target.json")
}

/// The key in a client dir's marker, read tolerantly: a missing, unreadable or
/// malformed marker is no marker.
pub fn read_marker(client_dir: &Path) -> Option<TargetKey> {
    let text = read_text(&marker_path(client_dir))?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("key")?.as_str()?.parse().ok()
}

/// Whether two paths name the same directory (also through symlinks).
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || std::fs::canonicalize(a)
            .ok()
            .is_some_and(|a| std::fs::canonicalize(b).ok() == Some(a))
}

// ---------------------------------------------------------------------------------------
// Reconciliation (read-only).

/// Discovered targets with their identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolved {
    /// In discovery order. Registered targets carry their key and stored id; the others
    /// have `key: null` and a provisional id.
    pub targets: Vec<DiscoveredTarget>,
    /// `target_key_duplicate` for copied installs.
    pub warnings: Vec<Warning>,
    /// Registered targets this discovery did not find (kept: history refers to them).
    pub missing: Vec<RegistryEntry>,
}

impl Resolved {
    /// Every id a person may name: discovered targets (registered or provisional) and
    /// registered ones this discovery did not find.
    pub fn known_ids(&self) -> Vec<&str> {
        self.targets
            .iter()
            .map(|d| d.target.id.as_str())
            .chain(self.missing.iter().map(|m| m.id.as_str()))
            .collect()
    }
}

/// Gives each discovered target its registered key and id: by client dir, else by
/// marker (a moved install). Never writes.
pub fn reconcile(registry: &Registry, discovered: &[DiscoveredTarget]) -> Resolved {
    let markers: Vec<Option<TargetKey>> = discovered
        .iter()
        .map(|d| read_marker(&d.target.client_dir))
        .collect();
    let mut assigned: Vec<Option<&RegistryEntry>> = discovered
        .iter()
        .map(|d| {
            registry
                .targets
                .iter()
                .find(|e| same_dir(&e.client_dir, &d.target.client_dir))
        })
        .collect();
    let mut duplicates = Vec::new();
    for i in 0..discovered.len() {
        if assigned[i].is_some() {
            continue;
        }
        let Some(entry) = markers[i].and_then(|key| registry.entry(&key)) else {
            continue;
        };
        if assigned.iter().flatten().any(|e| e.key == entry.key) {
            duplicates.push((i, entry.key));
        } else {
            assigned[i] = Some(entry);
        }
    }

    let mut taken: Vec<String> = registry.targets.iter().map(|e| e.id.clone()).collect();
    let mut targets = discovered.to_vec();
    for (d, entry) in targets.iter_mut().zip(&assigned) {
        if let Some(entry) = entry {
            d.target.key = Some(entry.key);
            d.target.id = entry.id.clone();
        }
    }
    // Provisional ids in route order, so the first-discovered target gets the bare id;
    // the sort is stable, so one route's targets keep discovery's path order.
    let mut provisional: Vec<usize> = (0..targets.len())
        .filter(|&i| assigned[i].is_none())
        .collect();
    provisional.sort_by_key(|&i| route_rank(&targets[i].source));
    for i in provisional {
        let d = &mut targets[i];
        d.target.key = None;
        d.target.id = free_id(&d.default_id, &d.qualifier, |id| {
            taken.iter().any(|t| t == id)
        });
        taken.push(d.target.id.clone());
    }

    let warnings = duplicates
        .into_iter()
        .map(|(i, key)| {
            let owner = targets
                .iter()
                .find(|t| t.target.key == Some(key))
                .map_or("another target", |t| t.target.id.as_str());
            Warning::new(
                WarningCode::TargetKeyDuplicate,
                format!(
                    "{} carries the key of {owner} (a copied install); {owner} keeps it",
                    targets[i].target.id
                ),
            )
            .with_fix(Suggestion::text("it gets a new key on its first mutation"))
        })
        .collect();
    let missing = registry
        .targets
        .iter()
        .filter(|e| !assigned.iter().flatten().any(|a| a.key == e.key))
        .cloned()
        .collect();
    Resolved {
        targets,
        warnings,
        missing,
    }
}

/// Discovery's route order (vision §5.2): `product.db`, launcher configs, running
/// processes, extra roots, the scan, then registered and remembered roots.
const fn route_rank(source: &DiscoverySource) -> u8 {
    match source {
        DiscoverySource::ProductDb => 0,
        DiscoverySource::Launcher { .. } => 1,
        DiscoverySource::Process => 2,
        DiscoverySource::ExtraRoot => 3,
        DiscoverySource::Scan => 4,
        DiscoverySource::Manual => 5,
        DiscoverySource::Registered => 6,
        DiscoverySource::Remembered => 7,
    }
}

/// The first free id of `default` (made id-safe), `default@qualifier`, then
/// `default@qualifier-2`, `-3`, …
pub(crate) fn free_id(default: &str, qualifier: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = id_safe(default);
    if !taken(&base) {
        return base;
    }
    let qualifier = id_safe(qualifier);
    let qualified = format!("{base}@{qualifier}");
    if !taken(&qualified) {
        return qualified;
    }
    (2u32..)
        .map(|n| format!("{qualified}-{n}"))
        .find(|id| !taken(id))
        .unwrap_or(qualified)
}

/// Lowercase, characters outside `[a-z0-9@._-]` as `-`, starting with `[a-z0-9]`;
/// `client` when nothing is left.
fn id_safe(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || "@._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    if trimmed.is_empty() {
        "client".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Why `id` cannot name a target, if it cannot: the grammar
/// `^[a-z0-9][a-z0-9@._-]*$`, and not a key, family name or product code, so `-t`
/// stays unambiguous.
pub fn id_problem(id: &str, table: &RulesTable) -> Option<String> {
    let mut chars = id.chars();
    let grammar = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "@._-".contains(c));
    if !grammar {
        return Some(format!(
            "{id:?} is not a valid id: use lowercase letters, digits and @._-, starting with a letter or digit"
        ));
    }
    if id.parse::<TargetKey>().is_ok() {
        return Some(format!("{id:?} looks like a target key"));
    }
    if Family::from_name(id).is_some() {
        return Some(format!("{id:?} is a family name"));
    }
    if table.product_by_code(id).is_some() {
        return Some(format!("{id:?} is a product code"));
    }
    None
}

// ---------------------------------------------------------------------------------------
// Registration (under the lock).

/// The registry, loaded under the global lock for changes; each change is saved at once.
/// It can only be had with a [`GlobalLockGuard`], so registration cannot happen
/// without the lock:
///
/// ```compile_fail
/// use omaddons::discover::registry::LockedRegistry;
/// fn register(paths: &omaddons::state::paths::Paths) {
///     let _ = LockedRegistry::load(paths); // the lock guard is missing
/// }
/// ```
pub struct LockedRegistry<'l> {
    _lock: &'l GlobalLockGuard,
    paths: Paths,
    loaded: Loaded<Registry>,
}

impl<'l> LockedRegistry<'l> {
    pub fn load(lock: &'l GlobalLockGuard, paths: &Paths) -> Result<Self> {
        Ok(Self {
            _lock: lock,
            paths: paths.clone(),
            loaded: load_registry(paths)?,
        })
    }

    pub fn registry(&self) -> &Registry {
        &self.loaded.value
    }

    /// The target's key, registering it first if needed: its marker's key when that
    /// is free (or the install moved), else a new one. Writes the marker and the
    /// registry through fsx; idempotent. The session's scope table must contain the
    /// target's client dir.
    pub fn ensure_registered(
        &mut self,
        s: &mut WriteSession<'_>,
        target: &DiscoveredTarget,
    ) -> Result<TargetKey> {
        self.register(s, target, Origin::Discovered, None)
            .map(|entry| entry.key)
    }

    fn register(
        &mut self,
        s: &mut WriteSession<'_>,
        d: &DiscoveredTarget,
        origin: Origin,
        name: Option<&str>,
    ) -> Result<RegistryEntry> {
        let t = &d.target;
        let registry = &mut self.loaded.value;
        if let Some(entry) = registry
            .targets
            .iter()
            .find(|e| same_dir(&e.client_dir, &t.client_dir))
            .cloned()
        {
            if read_marker(&t.client_dir) != Some(entry.key) {
                write_marker(s, &t.client_dir, entry.key)?;
            }
            return Ok(entry);
        }

        let marker = read_marker(&t.client_dir);
        if let Some(key) = marker
            && let Some(index) = registry.targets.iter().position(|e| e.key == key)
        {
            let original = &registry.targets[index].client_dir;
            if read_marker(original) != Some(key) {
                // Moved: re-link the key to the new paths.
                let entry = &mut registry.targets[index];
                entry.install_root = t.install_root.clone();
                entry.client_dir = t.client_dir.clone();
                entry.flavor_dir = t.flavor_dir.clone();
                entry.product = t.product.clone();
                let entry = entry.clone();
                self.save(s)?;
                return Ok(entry);
            }
        }

        let registry = &self.loaded.value;
        let key = match marker {
            Some(key) if registry.entry(&key).is_none() => key,
            _ => TargetKey::new(),
        };
        let taken = |id: &str| registry.entry_by_id(id).is_some();
        let id = match name {
            Some(name) => name.to_owned(),
            None if !t.id.is_empty() && !taken(&t.id) && is_id_safe(&t.id) => t.id.clone(),
            None => free_id(&d.default_id, &d.qualifier, taken),
        };
        let entry = RegistryEntry {
            key,
            id,
            install_root: t.install_root.clone(),
            client_dir: t.client_dir.clone(),
            flavor_dir: t.flavor_dir.clone(),
            product: t.product.clone(),
            qualifier: d.qualifier.clone(),
            origin,
            registered_at: Timestamp::now(),
        };
        write_marker(s, &t.client_dir, key)?;
        self.loaded.value.targets.push(entry.clone());
        self.save(s)?;
        Ok(entry)
    }

    /// Registers the client at `path` (a root, a flavor folder or an AddOns path) with
    /// origin `manual`; `name` becomes the id when exactly one target results.
    pub fn add_manual(
        &mut self,
        s: &mut WriteSession<'_>,
        table: &RulesTable,
        path: &Path,
        name: Option<&str>,
    ) -> Result<(Vec<DiscoveredTarget>, Vec<Warning>)> {
        let (mut found, warnings) = manual_targets(table, path)?;
        if let Some(name) = name {
            if found.len() > 1 {
                let root = found[0].target.install_root.clone();
                return Err(name_needs_one_target(&root, &found, name));
            }
            let already = found
                .iter()
                .all(|d| self.entry_at(&d.target.client_dir).is_some());
            if !already {
                self.check_new_id(name, table)?;
            }
        }
        for d in &mut found {
            let entry = self.register(s, d, Origin::Manual, name)?;
            d.target.key = Some(entry.key);
            d.target.id = entry.id;
        }
        Ok((found, warnings))
    }

    /// Unregisters the target `id`; its client dir and marker are left alone, so a
    /// later `add` re-links the same key.
    pub fn remove(&mut self, s: &mut WriteSession<'_>, id: &str) -> Result<RegistryEntry> {
        let registry = &mut self.loaded.value;
        let index = registry
            .targets
            .iter()
            .position(|e| e.id == id)
            .ok_or_else(|| unknown_id(id))?;
        let entry = registry.targets.remove(index);
        if registry.default == Some(entry.key) {
            registry.default = None;
        }
        self.save(s)?;
        Ok(entry)
    }

    /// Renames the target `id` to `new`.
    pub fn rename(
        &mut self,
        s: &mut WriteSession<'_>,
        table: &RulesTable,
        id: &str,
        new: &str,
    ) -> Result<RegistryEntry> {
        let index = self
            .loaded
            .value
            .targets
            .iter()
            .position(|e| e.id == id)
            .ok_or_else(|| unknown_id(id))?;
        if id != new {
            self.check_new_id(new, table)?;
        }
        let entry = &mut self.loaded.value.targets[index];
        entry.id = new.to_owned();
        let entry = entry.clone();
        self.save(s)?;
        Ok(entry)
    }

    /// Makes `id` the registry's default target. A `default_target` in config.toml
    /// still wins over it.
    pub fn set_default(&mut self, s: &mut WriteSession<'_>, id: &str) -> Result<TargetKey> {
        let key = self
            .loaded
            .value
            .entry_by_id(id)
            .ok_or_else(|| unknown_id(id))?
            .key;
        self.loaded.value.default = Some(key);
        self.save(s)?;
        Ok(key)
    }

    /// The registry entry at `client_dir`, if registered.
    pub fn entry_at(&self, client_dir: &Path) -> Option<&RegistryEntry> {
        registered_at(&self.loaded.value, client_dir)
    }

    fn check_new_id(&self, new: &str, table: &RulesTable) -> Result<()> {
        check_user_id(&self.loaded.value, new, table)
    }

    fn save(&mut self, s: &mut WriteSession<'_>) -> Result<()> {
        save_registry(s, &self.paths, &self.loaded.value, &self.loaded.status)?;
        self.loaded.status = VersionStatus::Current;
        Ok(())
    }
}

/// Whether `id` already satisfies the id grammar (a provisional id from reconcile).
fn is_id_safe(id: &str) -> bool {
    id_safe(id) == id
}

fn write_marker(s: &mut WriteSession<'_>, client_dir: &Path, key: TargetKey) -> Result<()> {
    let path = marker_path(client_dir);
    if let Some(dir) = path.parent() {
        ensure_dir_all(s, dir)?;
    }
    let bytes = store::encode(&TargetMarker { key })?;
    let op = Op::WriteFile {
        content: Content::Bytes(&bytes),
        mode: Some(0o644),
    };
    mutate(s, &path, op)?;
    Ok(())
}

/// The root to discover for `targets add <path>`, and the flavor folder when the path
/// named one: an AddOns path becomes its client dir, a flavor folder its Battle.net root.
fn manual_root(path: &Path) -> Result<(PathBuf, Option<String>)> {
    let mut dir = path.to_path_buf();
    let names = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
    };
    if names(&dir).as_deref() == Some("addons")
        && dir.parent().and_then(names).as_deref() == Some("interface")
        && let Some(client) = dir.parent().and_then(Path::parent)
    {
        dir = client.to_path_buf();
    } else if names(&dir).as_deref() == Some("interface")
        && let Some(client) = dir.parent()
    {
        dir = client.to_path_buf();
    }
    if battlenet::is_battlenet_root(&dir) {
        return Ok((dir, None));
    }
    if let Some(parent) = dir.parent()
        && battlenet::is_battlenet_root(parent)
        && let Some(folder) = dir.file_name().map(|n| n.to_string_lossy().into_owned())
    {
        return Ok((parent.to_path_buf(), Some(folder)));
    }
    if custom::is_custom_root(&dir) {
        return Ok((dir, None));
    }
    Err(not_a_client(path))
}

fn not_a_client(path: &Path) -> Error {
    Error::new(
        ErrorCode::TargetNotFound,
        format!(
            "{} holds no WoW client: looked for .build.info (a Battle.net root or its flavor folder) and Wow*.exe beside Interface/ (a custom client)",
            path.display()
        ),
        Suggestion::command(["omaddons", "targets"]),
    )
    .details(serde_json::json!({ "path": path }))
}

/// The targets at `path` (a Battle.net root, a flavor folder, a custom client root, or
/// an `Interface` or `Interface/AddOns` directory), discovered with source `manual`.
/// Read-only; `target_not_found` when there is no client.
pub fn manual_targets(
    table: &RulesTable,
    path: &Path,
) -> Result<(Vec<DiscoveredTarget>, Vec<Warning>)> {
    let (root, folder) = manual_root(path)?;
    let qualifier = dir_slug(&root);
    let source = DiscoverySource::Manual;
    let (mut found, warnings) = if battlenet::is_battlenet_root(&root) {
        battlenet::discover_root(&root, table, &Default::default(), &qualifier, &source)
    } else {
        let (found, warnings) = custom::discover_custom_root(&root, table, &qualifier, &source);
        (found.into_iter().collect(), warnings)
    };
    if let Some(folder) = &folder {
        found.retain(|d| d.target.flavor_dir.as_ref() == Some(folder));
    }
    if found.is_empty() {
        return Err(not_a_client(path));
    }
    Ok((found, warnings))
}

fn name_needs_one_target(root: &Path, found: &[DiscoveredTarget], name: &str) -> Error {
    let add = |d: &DiscoveredTarget| {
        Suggestion::command([
            "omaddons".to_owned(),
            "targets".to_owned(),
            "add".to_owned(),
            d.target.client_dir.display().to_string(),
            "--name".to_owned(),
            name.to_owned(),
        ])
    };
    let mut error = Error::new(
        ErrorCode::Usage,
        format!(
            "{} holds {} clients; --name needs one: give a flavor folder",
            root.display(),
            found.len()
        ),
        add(&found[0]),
    );
    for d in &found[1..] {
        error = error.alternative(add(d));
    }
    error
}

/// `target_not_found` for an id no registered target has.
pub fn unknown_id(id: &str) -> Error {
    Error::new(
        ErrorCode::TargetNotFound,
        format!("no registered target has the id {id:?}"),
        Suggestion::command(["omaddons", "targets"]),
    )
    .details(serde_json::json!({ "selector": id }))
}

/// The registry entry at `client_dir`, if registered (also through symlinks).
pub fn registered_at<'a>(registry: &'a Registry, client_dir: &Path) -> Option<&'a RegistryEntry> {
    registry
        .targets
        .iter()
        .find(|e| same_dir(&e.client_dir, client_dir))
}

/// A name a user picks (`--name`, `rename`): the id rules, at most 32 characters, no
/// `@` (reserved for qualified collisions such as `retail@steam`), and not taken;
/// `usage` (exit 2) otherwise, naming the target that holds a taken id.
pub fn check_user_id(registry: &Registry, new: &str, table: &RulesTable) -> Result<()> {
    let fix = Suggestion::text(
        "choose an id of up to 32 lowercase letters, digits and ._- that no target, family or product uses",
    );
    let problem = id_problem(new, table)
        .or_else(|| (new.len() > 32).then(|| format!("{new:?} is longer than 32 characters")))
        .or_else(|| {
            new.contains('@')
                .then(|| format!("{new:?} contains @, which marks qualified ids"))
        });
    if let Some(message) = problem {
        return Err(Error::new(ErrorCode::Usage, message, fix));
    }
    if let Some(holder) = registry.entry_by_id(new) {
        return Err(Error::new(
            ErrorCode::Usage,
            format!("{new:?} is already the id of the target at {}", holder.client_dir.display()),
            Suggestion::text(format!(
                "pick another id, or rename that target first: omaddons targets rename {new} <new id>"
            )),
        )
        .details(serde_json::json!({ "holder": holder.key })));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Selectors.

/// The targets `selector` names. Precedence, first match wins: key, id, product code,
/// flavor folder, game mode (`table`'s mode ids and aliases: `sod` selects the Classic
/// Era client), family, then a path (install root, client dir or AddOns path).
/// `target_not_found` when nothing matches, naming the missing client for a known mode.
pub fn resolve<'a>(
    selector: &str,
    targets: &'a [DiscoveredTarget],
    table: &RulesTable,
) -> Result<Vec<&'a DiscoveredTarget>> {
    let pick = |test: &dyn Fn(&DiscoveredTarget) -> bool| -> Vec<&'a DiscoveredTarget> {
        targets.iter().filter(|d| test(d)).collect()
    };
    let key = selector.parse::<TargetKey>().ok();
    let family = Family::from_name(selector);
    let path = std::fs::canonicalize(selector).ok();
    let modes = table.products_with_mode(selector);
    let hosts_mode = |d: &DiscoveredTarget| {
        d.target
            .product
            .as_deref()
            .is_some_and(|p| modes.iter().any(|(m, _)| m.code.eq_ignore_ascii_case(p)))
    };
    let steps: [&dyn Fn(&DiscoveredTarget) -> bool; 7] = [
        &|d| key.is_some() && d.target.key == key,
        &|d| d.target.id.eq_ignore_ascii_case(selector),
        &|d| {
            d.target
                .product
                .as_deref()
                .is_some_and(|p| p.eq_ignore_ascii_case(selector))
        },
        &|d| {
            d.target
                .flavor_dir
                .as_deref()
                .is_some_and(|f| f.eq_ignore_ascii_case(selector))
        },
        // A game mode selects the client that runs it (`sod` → Classic Era).
        &|d| hosts_mode(d),
        &|d| family.is_some_and(|f| d.target.family == f),
        &|d| path.as_ref().is_some_and(|p| path_names(p, d)),
    ];
    steps
        .iter()
        .map(|step| pick(*step))
        .find(|found| !found.is_empty())
        .ok_or_else(|| match modes.first() {
            // A known mode whose client is not on this machine.
            Some((product, mode)) => Error::new(
                ErrorCode::TargetNotFound,
                format!(
                    "{} runs in the {} client ({}), which was not found on this machine",
                    mode.name,
                    host_name(product),
                    product.folder
                ),
                Suggestion::command(["omaddons", "targets", "--rescan"]),
            )
            .details(serde_json::json!({
                "selector": selector,
                "mode": mode.id,
                "product": product.code,
            })),
            None => Error::new(
                ErrorCode::TargetNotFound,
                format!("no target matches {selector:?}"),
                Suggestion::command(["omaddons", "targets"]),
            )
            .details(serde_json::json!({ "selector": selector })),
        })
}

/// A client as players name it: its first mode (`Classic Era` for the client that also
/// runs Hardcore and Season of Discovery), else its product code.
fn host_name(product: &Product) -> &str {
    product
        .modes
        .first()
        .map_or(product.code.as_str(), |m| m.name.as_str())
}

/// Whether the canonical `path` is the target's install root, client dir or AddOns dir.
fn path_names(path: &Path, d: &DiscoveredTarget) -> bool {
    let t = &d.target;
    let addons = super::find_path(&t.client_dir, &["Interface", "AddOns"]);
    [
        Some(t.install_root.clone()),
        Some(t.client_dir.clone()),
        addons,
    ]
    .into_iter()
    .flatten()
    .any(|p| std::fs::canonicalize(p).ok().as_deref() == Some(path))
}

/// The one target `selector` names; `target_ambiguous` when it names several, with
/// `argv` rewritten to name each candidate by id.
pub fn resolve_one<'a>(
    selector: &str,
    targets: &'a [DiscoveredTarget],
    table: &RulesTable,
    argv: &[String],
) -> Result<&'a DiscoveredTarget> {
    let found = resolve(selector, targets, table)?;
    let [first, rest @ ..] = found.as_slice() else {
        return Err(Error::internal("resolve returned no targets"));
    };
    if rest.is_empty() {
        return Ok(first);
    }
    let ids: Vec<&str> = found.iter().map(|d| d.target.id.as_str()).collect();
    let mut error = Error::new(
        ErrorCode::TargetAmbiguous,
        format!(
            "{selector:?} matches {} targets: {}",
            ids.len(),
            ids.join(", ")
        ),
        Suggestion::command(with_target(argv, ids[0])),
    )
    .details(serde_json::json!({ "selector": selector, "candidates": ids }));
    for id in &ids[1..] {
        error = error.alternative(Suggestion::command(with_target(argv, id)));
    }
    Err(error)
}

/// `argv` with its `-t`/`--target`/`--flavor` value replaced by `id` (or `-t <id>` added
/// before any `--`), without presentation flags.
pub(crate) fn with_target(argv: &[String], id: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len() + 2);
    let mut replaced = false;
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => {
                if !replaced {
                    out.extend(["-t".to_owned(), id.to_owned()]);
                    replaced = true;
                }
                out.push(arg.clone());
                out.extend(args.by_ref().cloned());
            }
            "-t" | "--target" | "--flavor" => {
                out.push(arg.clone());
                args.next();
                out.push(id.to_owned());
                replaced = true;
            }
            a if a.starts_with("--target=") || a.starts_with("--flavor=") => {
                let flag = a.split('=').next().unwrap_or("--target");
                out.push(format!("{flag}={id}"));
                replaced = true;
            }
            a if a.starts_with("-t") && !a.starts_with("--") => {
                out.extend(["-t".to_owned(), id.to_owned()]);
                replaced = true;
            }
            _ => out.push(arg.clone()),
        }
    }
    if !replaced {
        out.extend(["-t".to_owned(), id.to_owned()]);
    }
    crate::cli::fix_argv(&out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Target;
    use crate::rules::{ClientVersion, InstallKind, TargetKind};

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("omaddons")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    fn discovered(
        id: &str,
        family: Family,
        product: Option<&str>,
        folder: Option<&str>,
        root: &str,
    ) -> DiscoveredTarget {
        let root = PathBuf::from(root);
        let client_dir = folder.map_or_else(|| root.clone(), |f| root.join(f));
        DiscoveredTarget {
            target: Target {
                key: None,
                id: id.to_owned(),
                install_root: root,
                client_dir,
                flavor_dir: folder.map(str::to_owned),
                product: product.map(str::to_owned),
                install_kind: InstallKind::Battlenet,
                family,
                kind: TargetKind::Live,
                version: ClientVersion::new(1, 0, 0),
                interface: 10_000,
                exe_name: None,
                label: None,
                rules: Default::default(),
            },
            default_id: id.to_owned(),
            qualifier: "battlenet".to_owned(),
            source: DiscoverySource::Scan,
        }
    }

    #[test]
    fn free_ids() {
        let taken = |list: &'static [&'static str]| move |id: &str| list.contains(&id);
        assert_eq!(free_id("retail", "steam", taken(&[])), "retail");
        assert_eq!(
            free_id("retail", "steam", taken(&["retail"])),
            "retail@steam"
        );
        assert_eq!(
            free_id("retail", "steam", taken(&["retail", "retail@steam"])),
            "retail@steam-2"
        );
        assert_eq!(
            free_id(
                "retail",
                "steam",
                taken(&["retail", "retail@steam", "retail@steam-2"])
            ),
            "retail@steam-3"
        );
        assert_eq!(free_id("My Server!", "x", taken(&[])), "my-server-");
        assert_eq!(free_id("--", "x", taken(&[])), "client");
        assert_eq!(free_id("new_ptr", "battlenet", taken(&[])), "new_ptr");
    }

    #[test]
    fn provisional_ids_follow_route_order_then_path_order() {
        let with = |root: &str, qualifier: &str, source: DiscoverySource| {
            let mut d = discovered(
                "retail",
                Family::Mainline,
                Some("wow"),
                Some("_retail_"),
                root,
            );
            d.qualifier = qualifier.to_owned();
            d.source = source;
            d
        };
        let steam = DiscoverySource::Launcher {
            name: "steam".to_owned(),
        };
        // Path order puts Steam first; the product.db route still comes first.
        let found = [
            with("/h/.local/share/Steam/pfx/wow", "steam", steam.clone()),
            with(
                "/h/Games/battlenet/wow",
                "battlenet",
                DiscoverySource::ProductDb,
            ),
            with("/h/Games/other/wow", "other", DiscoverySource::Scan),
            with("/h/Games/third/wow", "third", DiscoverySource::Scan),
        ];
        let ids: Vec<String> = reconcile(&Registry::default(), &found)
            .targets
            .into_iter()
            .map(|d| d.target.id)
            .collect();
        assert_eq!(
            ids,
            ["retail@steam", "retail", "retail@other", "retail@third"]
        );
    }

    #[test]
    fn id_rules() {
        let table = RulesTable::builtin();
        assert_eq!(id_problem("main", &table), None);
        assert_eq!(id_problem("retail@steam-2", &table), None);
        for bad in [
            "",
            "Main",
            "-x",
            "a b",
            "_retail_",
            "01jb8q2v9w0000000000000000",
            "wrath-legacy",
            "mainline",
            "wow_classic_era",
        ] {
            assert!(id_problem(bad, &table).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn selector_precedence() {
        let targets = vec![
            discovered(
                "forever",
                Family::Forever,
                Some("wowf"),
                Some("_forever_"),
                "/g/wow",
            ),
            discovered(
                "classic-beta",
                Family::Forever,
                Some("wow_classic_beta"),
                Some("_classic_beta_"),
                "/g/wow",
            ),
            discovered("wotlk", Family::WrathLegacy, None, None, "/g/a"),
            discovered("wotlk@b", Family::WrathLegacy, None, None, "/g/b"),
        ];
        let table = RulesTable::builtin();
        let ids = |selector: &str| -> Vec<String> {
            resolve(selector, &targets, &table).map_or_else(
                |e| vec![e.code.wire().to_owned()],
                |found| found.iter().map(|d| d.target.id.clone()).collect(),
            )
        };
        assert_eq!(ids("forever"), ["forever"], "an id beats a family");
        assert_eq!(ids("FOREVER"), ["forever"]);
        assert_eq!(ids("wow_classic_beta"), ["classic-beta"]);
        assert_eq!(ids("_Classic_Beta_"), ["classic-beta"]);
        assert_eq!(ids("wrath-legacy"), ["wotlk", "wotlk@b"]);
        assert_eq!(ids("nope"), ["target_not_found"]);

        let mut keyed = targets.clone();
        let key = TargetKey::new();
        keyed[2].target.key = Some(key);
        assert_eq!(
            resolve(&key.to_string().to_lowercase(), &keyed, &table).unwrap()[0]
                .target
                .id,
            "wotlk"
        );
    }

    #[test]
    fn game_modes_select_the_client_that_runs_them() {
        let table = RulesTable::builtin();
        let era = |id: &str, root: &str| {
            discovered(
                id,
                Family::Vanilla,
                Some("wow_classic_era"),
                Some("_classic_era_"),
                root,
            )
        };
        let classic = discovered(
            "classic",
            Family::Mists,
            Some("wow_classic"),
            Some("_classic_"),
            "/g/wow",
        );
        let targets = vec![era("era", "/g/wow"), classic.clone()];
        let ids = |selector: &str, targets: &[DiscoveredTarget]| -> Vec<String> {
            resolve(selector, targets, &table).map_or_else(
                |e| vec![e.code.wire().to_owned()],
                |found| found.iter().map(|d| d.target.id.clone()).collect(),
            )
        };
        for selector in ["sod", "SoD", "season-of-discovery", "hardcore", "hc"] {
            assert_eq!(ids(selector, &targets), ["era"], "{selector}");
        }
        assert_eq!(ids("mop", &targets), ["classic"]);

        // Two Classic Era installs both run the mode.
        let two = vec![era("era", "/g/wow"), era("era@steam", "/g/steam")];
        assert_eq!(ids("sod", &two), ["era", "era@steam"]);
        let err =
            resolve_one("sod", &two, &table, &argv(&["install", "x", "-t", "sod"])).unwrap_err();
        assert_eq!(err.code, ErrorCode::TargetAmbiguous);

        // A target registered under a mode's name keeps it.
        let named = vec![era("era", "/g/wow"), era("sod", "/g/other")];
        assert_eq!(ids("sod", &named), ["sod"]);

        // A known mode whose client is not here names that client.
        let err = resolve("sod", std::slice::from_ref(&classic), &table).unwrap_err();
        assert_eq!(err.code, ErrorCode::TargetNotFound);
        assert_eq!(
            err.message,
            "Season of Discovery runs in the Classic Era client (_classic_era_), which was not found on this machine"
        );
        assert_eq!(err.fix.argv().unwrap(), argv(&["targets", "--rescan"]));
    }

    #[test]
    fn ambiguity_fixes_rewrite_argv() {
        let targets = vec![
            discovered("wotlk", Family::WrathLegacy, None, None, "/g/a"),
            discovered("wotlk@b", Family::WrathLegacy, None, None, "/g/b"),
        ];
        let table = RulesTable::builtin();
        let err = resolve_one(
            "wrath-legacy",
            &targets,
            &table,
            &argv(&["install", "x", "-t", "wrath-legacy", "--json"]),
        )
        .unwrap_err();
        assert_eq!(
            (err.code, err.exit().code()),
            (ErrorCode::TargetAmbiguous, 3)
        );
        assert_eq!(
            err.fix.argv().unwrap(),
            argv(&["install", "x", "-t", "wotlk"])
        );
        assert_eq!(err.alternatives.len(), 1);
        assert_eq!(
            err.alternatives[0].argv().unwrap(),
            argv(&["install", "x", "-t", "wotlk@b"])
        );
        assert_eq!(
            err.details["candidates"],
            serde_json::json!(["wotlk", "wotlk@b"])
        );
        assert_eq!(
            resolve_one("wotlk@b", &targets, &table, &[])
                .unwrap()
                .target
                .id,
            "wotlk@b"
        );

        for (args, expected) in [
            (
                &["install", "x", "--target=wrath-legacy"][..],
                &["install", "x", "--target=wotlk"][..],
            ),
            (
                &["install", "x", "--flavor", "wrath-legacy"][..],
                &["install", "x", "--flavor", "wotlk"][..],
            ),
            (
                &["install", "x", "-twrath-legacy"][..],
                &["install", "x", "-t", "wotlk"][..],
            ),
            (&["install", "x"][..], &["install", "x", "-t", "wotlk"][..]),
            (
                &["install", "--", "-t"][..],
                &["install", "-t", "wotlk", "--", "-t"][..],
            ),
        ] {
            assert_eq!(
                with_target(&argv(args), "wotlk"),
                argv(expected),
                "{args:?}"
            );
        }
    }
}
