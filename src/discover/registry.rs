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
use crate::vocabulary::TargetVocabulary;

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
            let found = targets.iter().find(|t| t.target.key == Some(key));
            let owner = found.map_or("another target", |t| t.target.id.as_str());
            let copy = targets[i].target.id.as_str();
            Warning::new(
                WarningCode::TargetKeyDuplicate,
                format!("{copy} carries the key of {owner} (a copied install); {owner} keeps it"),
            )
            .with_fix(Suggestion::text("it gets a new key on its first mutation"))
            .about(std::iter::once(copy).chain(found.map(|t| t.target.id.as_str())))
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
    if id == ALL_TARGETS {
        return Some(format!("{id:?} is reserved: -t all selects every client"));
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
    /// origin `manual`; `name` becomes the id when exactly one target results, renaming
    /// a client registered already (or moved) under another.
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
            // Free, unless this client holds it already (by its directory or marker).
            let held = self.loaded.value.entry_by_id(name).is_some_and(|e| {
                found.iter().any(|d| {
                    same_dir(&e.client_dir, &d.target.client_dir)
                        || read_marker(&d.target.client_dir) == Some(e.key)
                })
            });
            if !held {
                self.check_new_id(name, table)?;
            }
        }
        for d in &mut found {
            let mut entry = self.register(s, d, Origin::Manual, name)?;
            if let Some(name) = name
                && entry.id != name
            {
                entry = self.rename(s, table, &entry.id, name)?;
            }
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

pub(crate) fn name_needs_one_target(root: &Path, found: &[DiscoveredTarget], name: &str) -> Error {
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

/// The selector that names every target (M4-D13): `-t all`, any case.
pub const ALL_TARGETS: &str = "all";

/// Whether `selector` spells the `all` keyword (ASCII case ignored).
pub fn is_all_keyword(selector: &str) -> bool {
    selector.eq_ignore_ascii_case(ALL_TARGETS)
}

/// `target_id_reserved`, when `selector` spells `all` and a target is named `all`: a
/// discovered one (registered or provisional), or a registered one whose directory is
/// gone. That target wins over the keyword.
pub fn reserved_id_warning(selector: &str, resolved: &Resolved) -> Option<Warning> {
    let named = resolved
        .known_ids()
        .iter()
        .any(|id| id.eq_ignore_ascii_case(ALL_TARGETS));
    (is_all_keyword(selector) && named).then(|| {
        Warning::new(
            WarningCode::TargetIdReserved,
            "a client has the id \"all\", so -t all selected that client, not every client",
        )
        .with_fix(Suggestion::text(
            "rename it, and -t all selects every client again: omaddons targets rename all <new id>",
        ))
    })
}

/// The targets `selector` names. Precedence, first match wins: key, the keyword `all`
/// (every target, in id order, unless a target is named `all`), id, product code,
/// flavor folder, game mode (`table`'s mode ids and aliases: `sod` selects the Classic
/// Era client), family, then a path (install root, client dir or AddOns path).
/// [`SelectorForm::ALL`] enumerates the forms and drives the individual predicates.
/// `target_not_found` when nothing matches, naming the missing client for a known mode.
pub fn resolve<'a>(
    selector: &str,
    targets: &'a [DiscoveredTarget],
    table: &RulesTable,
) -> Result<Vec<&'a DiscoveredTarget>> {
    // The key step never matches `all` (not a ULID), so the keyword comes first. Sorted:
    // its set is everything, and discovery order depends on routes the user cannot see.
    let named_all = targets
        .iter()
        .any(|d| d.target.id.eq_ignore_ascii_case(ALL_TARGETS));
    if is_all_keyword(selector) && !named_all {
        if targets.is_empty() {
            return Err(Error::new(
                ErrorCode::TargetNotFound,
                "-t all selects every client, and none was found on this machine",
                Suggestion::command(["omaddons", "targets"]),
            )
            .details(serde_json::json!({ "selector": selector })));
        }
        let mut all: Vec<&DiscoveredTarget> = targets.iter().collect();
        all.sort_by(|a, b| a.target.id.cmp(&b.target.id));
        return Ok(all);
    }
    let path = std::fs::canonicalize(selector).ok();
    first_match(selector, targets, table, path.as_deref())
        .ok_or_else(|| no_match(selector, targets, table))
}

/// A form a `-t` selector may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorForm {
    Key,
    Id,
    ProductCode,
    FlavorFolder,
    GameMode,
    Family,
    Path,
    All,
}

impl SelectorForm {
    /// The first matching form wins. `resolve` handles `All` before the other forms
    /// when no client id shadows it; no other form could then match that keyword.
    pub const ALL: [Self; 8] = [
        Self::Key,
        Self::Id,
        Self::ProductCode,
        Self::FlavorFolder,
        Self::GameMode,
        Self::Family,
        Self::Path,
        Self::All,
    ];

    pub const fn wire(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Id => "id",
            Self::ProductCode => "product_code",
            Self::FlavorFolder => "flavor_folder",
            Self::GameMode => "game_mode",
            Self::Family => "family",
            Self::Path => "path",
            Self::All => "all",
        }
    }
}

/// The targets the first matching form in [`SelectorForm::ALL`] selects, `None` when
/// no form matches. The `all` keyword's sorting and id-shadow check live in `resolve`.
fn first_match<'a>(
    selector: &str,
    targets: &'a [DiscoveredTarget],
    table: &RulesTable,
    path: Option<&Path>,
) -> Option<Vec<&'a DiscoveredTarget>> {
    let pick = |test: &dyn Fn(&DiscoveredTarget) -> bool| -> Vec<&'a DiscoveredTarget> {
        targets.iter().filter(|d| test(d)).collect()
    };
    let key = selector.parse::<TargetKey>().ok();
    let family = Family::from_name(selector);
    let modes = table.products_with_mode(selector);
    let hosts_mode = |d: &DiscoveredTarget| {
        d.target
            .product
            .as_deref()
            .is_some_and(|p| modes.iter().any(|(m, _)| m.code.eq_ignore_ascii_case(p)))
    };
    let matches = |form, d: &DiscoveredTarget| match form {
        SelectorForm::Key => key.is_some() && d.target.key == key,
        SelectorForm::Id => d.target.id.eq_ignore_ascii_case(selector),
        SelectorForm::ProductCode => d
            .target
            .product
            .as_deref()
            .is_some_and(|p| p.eq_ignore_ascii_case(selector)),
        SelectorForm::FlavorFolder => d
            .target
            .flavor_dir
            .as_deref()
            .is_some_and(|f| f.eq_ignore_ascii_case(selector)),
        // A game mode selects the client that runs it (`sod` → Classic Era).
        SelectorForm::GameMode => hosts_mode(d),
        SelectorForm::Family => family.is_some_and(|f| d.target.family == f),
        SelectorForm::Path => path.is_some_and(|p| path_names(p, d)),
        SelectorForm::All => is_all_keyword(selector),
    };
    SelectorForm::ALL
        .iter()
        .map(|form| pick(&|d| matches(*form, d)))
        .find(|found| !found.is_empty())
}

/// `target_not_found` for a selector no step matched. A known mode whose client is not
/// here names that client; any other selector names its closest words that select a
/// client here (M4-D14), recorded for the command boundary to put into the user's argv.
fn no_match(selector: &str, targets: &[DiscoveredTarget], table: &RulesTable) -> Error {
    if let Some((product, mode)) = table.products_with_mode(selector).first() {
        return Error::new(
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
            "game_mode": mode.id,
            "product": product.code,
        }));
    }
    let closest = closest_selectors(selector, targets, table);
    let message = match closest.first() {
        Some(best) => format!("no target matches {selector:?}; the closest selector is {best:?}"),
        None => format!("no target matches {selector:?}"),
    };
    Error::new(
        ErrorCode::TargetNotFound,
        message,
        Suggestion::command(["omaddons", "targets"]),
    )
    .details(serde_json::json!({ "selector": selector }))
    .closest_selectors(selector, closest)
}

/// The words close to `selector` that select at least one client here (M4-D14): ids of
/// the clients here first among equals, never a path, and never `all`, which would widen
/// a selection from one client to every client.
fn closest_selectors(
    selector: &str,
    targets: &[DiscoveredTarget],
    table: &RulesTable,
) -> Vec<String> {
    let here = targets.iter().map(|d| d.target.id.as_str());
    TargetVocabulary::new(table, here).closest(selector, &|word| {
        !is_all_keyword(word) && first_match(word, targets, table, None).is_some()
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

/// The registered client `selector` names by key or id whose directory is gone or that
/// Battle.net uninstalled (`missing`), when no discovered target answers to it by either.
/// Only `backup list -t` and `backup export` reach it (omad-hz6h); every other command
/// refuses it through [`missing_named`].
pub fn missing_entry<'a>(
    selector: &str,
    targets: &[DiscoveredTarget],
    missing: &'a [RegistryEntry],
) -> Option<&'a RegistryEntry> {
    let key = selector.parse::<TargetKey>().ok();
    let names = |k: Option<TargetKey>, id: &str| {
        (key.is_some() && k == key) || id.eq_ignore_ascii_case(selector)
    };
    if targets.iter().any(|d| names(d.target.key, &d.target.id)) {
        return None;
    }
    missing.iter().find(|e| names(Some(e.key), &e.id))
}

/// `target_not_found` when `selector` names, by key or id, a registered client whose
/// directory is gone or that Battle.net uninstalled (`missing`) and no discovered target
/// by either ([`missing_entry`]): a later kind of selector (a family its id shadows, a
/// mode) would pick another client.
pub fn missing_named(
    selector: &str,
    targets: &[DiscoveredTarget],
    missing: &[RegistryEntry],
) -> Result<()> {
    match missing_entry(selector, targets, missing) {
        Some(e) => Err(Error::new(
            ErrorCode::TargetNotFound,
            format!(
                "{} is registered, but its client is gone or no longer installed ({})",
                e.id,
                e.client_dir.display()
            ),
            Suggestion::command(["omaddons", "targets"]),
        )
        .details(serde_json::json!({ "selector": selector, "key": e.key, "missing": true }))),
        None => Ok(()),
    }
}

/// [`resolve`] over what discovery found, a missing client named by key or id refused
/// ([`missing_named`]).
pub fn resolve_in<'a>(
    selector: &str,
    resolved: &'a Resolved,
    table: &RulesTable,
) -> Result<Vec<&'a DiscoveredTarget>> {
    missing_named(selector, &resolved.targets, &resolved.missing)?;
    resolve(selector, &resolved.targets, table)
}

/// [`resolve_one`] over what discovery found, a missing client named by key or id
/// refused ([`missing_named`]).
pub fn resolve_one_in<'a>(
    selector: &str,
    resolved: &'a Resolved,
    table: &RulesTable,
    argv: &[String],
) -> Result<&'a DiscoveredTarget> {
    missing_named(selector, &resolved.targets, &resolved.missing)?;
    resolve_one(selector, &resolved.targets, table, argv)
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

/// Where a token carries a selector value: in the next token, or attached after the
/// token's first `n` bytes.
enum SelectorAt {
    Next,
    Attached(usize),
}

/// Whether `arg` is a selector flag, in every spelling clap accepts: `-t`, `--target`,
/// `--flavor` (and `copy`'s `--from`, `--to`), their `=` forms, `-tV`, `-t=V`, and a
/// cluster of the value-less `-v` and `-h` ending in `t` (`-vt V`, `-vtV`).
fn selector_at(arg: &str) -> Option<SelectorAt> {
    if let Some(long) = arg.strip_prefix("--") {
        return ["target", "flavor", "from", "to"].iter().find_map(|name| {
            let rest = long.strip_prefix(name)?;
            match rest {
                "" => Some(SelectorAt::Next),
                _ if rest.starts_with('=') => Some(SelectorAt::Attached(2 + name.len() + 1)),
                _ => None,
            }
        });
    }
    let short = arg.strip_prefix('-')?;
    let switches = short.len() - short.trim_start_matches(['v', 'h']).len();
    let rest = short.get(switches..)?.strip_prefix('t')?;
    Some(match rest {
        "" => SelectorAt::Next,
        _ if rest.starts_with('=') => SelectorAt::Attached(1 + switches + 2),
        _ => SelectorAt::Attached(1 + switches + 1),
    })
}

/// `argv` (without presentation flags) with `typed` replaced by `with` wherever it is the
/// value of a selector flag, the flag kept as the user spelled it; `None` when no
/// selector flag holds `typed` (it came from config's `default_target`). Every such
/// occurrence is replaced; a positional, another flag's value, and anything after `--`
/// are left alone.
pub(crate) fn with_selector(argv: &[String], typed: &str, with: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::with_capacity(argv.len());
    let mut replaced = false;
    let mut value_next = false;
    let mut rest = argv.iter();
    while let Some(arg) = rest.next() {
        if std::mem::take(&mut value_next) {
            // ubs:ignore — comparing command-line words, not secrets
            let hit = arg == typed;
            out.push(if hit { with.to_owned() } else { arg.clone() });
            replaced |= hit;
            continue;
        }
        // ubs:ignore — comparing command-line words, not secrets
        if arg == "--" {
            out.push(arg.clone());
            out.extend(rest.by_ref().cloned());
            break;
        }
        match selector_at(arg) {
            Some(SelectorAt::Next) => {
                value_next = true;
                out.push(arg.clone());
            }
            Some(SelectorAt::Attached(at)) => match (arg.get(..at), arg.get(at..)) {
                // ubs:ignore — comparing command-line words, not secrets
                (Some(flag), Some(value)) if value == typed => {
                    out.push(format!("{flag}{with}"));
                    replaced = true;
                }
                _ => out.push(arg.clone()),
            },
            None => out.push(arg.clone()),
        }
    }
    replaced.then(|| crate::cli::fix_argv(&out))
}

/// A `target_not_found` whose selector was a near miss (M4-D14), in the user's own
/// command: the closest word as the fix, the other close words as alternatives in rank
/// order, then the listing it had. The verb is the user's own, only the client word
/// changed ([`UsageFix::UserVerb`](crate::error::UsageFix)). Any other error, and one
/// whose selector is not on the command line, is left as it is.
pub(crate) fn fix_with_closest_selector(error: &mut Error, argv: &[String]) {
    if !matches!(error.code, ErrorCode::TargetNotFound) {
        return;
    }
    let Some(record) = error.closest_selectors.clone() else {
        return;
    };
    let fixes: Option<Vec<Vec<String>>> = record
        .closest
        .iter()
        .map(|word| with_selector(argv, &record.typed, word))
        .collect();
    let Some((first, others)) = fixes.as_deref().and_then(<[Vec<String>]>::split_first) else {
        return;
    };
    let listing = std::mem::replace(&mut error.fix, Suggestion::command(first.clone()));
    let mut alternatives: Vec<Suggestion> = others
        .iter()
        .map(|fix| Suggestion::command(fix.clone()))
        .collect();
    alternatives.append(&mut error.alternatives);
    alternatives.push(listing);
    error.alternatives = alternatives;
    error.usage_fix = crate::error::UsageFix::UserVerb;
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
                judge: String::new(),
            },
            default_id: id.to_owned(),
            qualifier: "battlenet".to_owned(),
            source: DiscoverySource::Scan,
            not_launched_since: None,
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
    fn selector_forms_follow_resolve_precedence() {
        assert_eq!(
            SelectorForm::ALL.map(SelectorForm::wire),
            [
                "key",
                "id",
                "product_code",
                "flavor_folder",
                "game_mode",
                "family",
                "path",
                "all"
            ]
        );
    }

    #[test]
    fn each_selector_form_selects_on_its_own() {
        let temp = tempfile::tempdir().unwrap();
        let mut targets = vec![
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
            discovered(
                "era",
                Family::Vanilla,
                Some("wow_classic_era"),
                Some("_classic_era_"),
                "/g/wow",
            ),
        ];
        let key = TargetKey::new();
        targets[2].target.key = Some(key);
        targets[2].target.client_dir = temp.path().to_path_buf();
        let table = RulesTable::builtin();
        for form in SelectorForm::ALL {
            let (selector, expected): (String, Vec<&str>) = match form {
                SelectorForm::Key => (key.to_string().to_lowercase(), vec!["wotlk"]),
                SelectorForm::Id => ("wotlk".into(), vec!["wotlk"]),
                SelectorForm::ProductCode => ("wowf".into(), vec!["forever"]),
                SelectorForm::FlavorFolder => ("_classic_beta_".into(), vec!["classic-beta"]),
                SelectorForm::GameMode => ("sod".into(), vec!["era"]),
                SelectorForm::Family => ("wrath-legacy".into(), vec!["wotlk", "wotlk@b"]),
                SelectorForm::Path => (temp.path().display().to_string(), vec!["wotlk"]),
                SelectorForm::All => (
                    "all".into(),
                    vec!["classic-beta", "era", "forever", "wotlk", "wotlk@b"],
                ),
            };
            let found = resolve(&selector, &targets, &table)
                .unwrap_or_else(|error| panic!("{form:?}: {selector:?}: {error}")); // ubs:ignore — test-only failure reports the selector and its error
            let ids: Vec<_> = found.iter().map(|d| d.target.id.as_str()).collect();
            assert_eq!(ids, expected, "{form:?}: selector {selector:?}");
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

    /// `retail`, `wotlk`, `era` and the test client `ptr`, in that (discovery) order.
    fn four_targets() -> Vec<DiscoveredTarget> {
        let mut ptr = discovered(
            "ptr",
            Family::Mainline,
            Some("wowt"),
            Some("_ptr_"),
            "/g/wow",
        );
        ptr.target.kind = TargetKind::Test;
        vec![
            discovered(
                "retail",
                Family::Mainline,
                Some("wow"),
                Some("_retail_"),
                "/g/wow",
            ),
            discovered("wotlk", Family::WrathLegacy, None, None, "/g/a"),
            discovered(
                "era",
                Family::Vanilla,
                Some("wow_classic_era"),
                Some("_classic_era_"),
                "/g/wow",
            ),
            ptr,
        ]
    }

    fn ids_of(found: &[&DiscoveredTarget]) -> Vec<String> {
        found.iter().map(|d| d.target.id.clone()).collect()
    }

    #[test]
    fn the_all_keyword_selects_every_target_in_id_order() {
        let targets = four_targets();
        let table = RulesTable::builtin();
        for selector in ["all", "ALL", "All"] {
            let found = resolve(selector, &targets, &table).unwrap();
            assert_eq!(
                ids_of(&found),
                ["era", "ptr", "retail", "wotlk"],
                "{selector}"
            );
        }
    }

    #[test]
    fn a_target_named_all_wins_over_the_keyword() {
        let mut targets = four_targets();
        targets.push(discovered("all", Family::Mainline, None, None, "/g/all"));
        let table = RulesTable::builtin();
        for selector in ["all", "ALL"] {
            let found = resolve(selector, &targets, &table).unwrap();
            assert_eq!(ids_of(&found), ["all"], "{selector}");
        }
    }

    #[test]
    fn the_all_keyword_with_no_target_is_target_not_found() {
        let error = resolve("all", &[], &RulesTable::builtin()).unwrap_err();
        assert_eq!(error.code, ErrorCode::TargetNotFound);
        assert_eq!(
            error.message,
            "-t all selects every client, and none was found on this machine"
        );
        assert_eq!(error.details, serde_json::json!({ "selector": "all" }));
        assert_eq!(
            error.fix.argv(),
            Some(&["omaddons".to_owned(), "targets".to_owned()][..])
        );
    }

    #[test]
    fn resolve_one_with_all_offers_one_fix_per_target() {
        let targets = four_targets();
        let table = RulesTable::builtin();
        let argv = |words: &[&str]| -> Vec<String> {
            std::iter::once("omaddons")
                .chain(words.iter().copied())
                .map(str::to_owned)
                .collect()
        };
        for (case, typed, spelled) in [
            ("-t all", argv(&["install", "x", "-t", "all"]), "-t"),
            (
                "--target=all",
                argv(&["install", "x", "--target=all"]),
                "--target=",
            ),
        ] {
            let error = resolve_one("all", &targets, &table, &typed).unwrap_err();
            assert_eq!(error.code, ErrorCode::TargetAmbiguous, "{case}");
            let ids = ["era", "ptr", "retail", "wotlk"];
            assert_eq!(
                error.details["candidates"],
                serde_json::json!(ids),
                "{case}"
            );
            let fixes: Vec<Vec<String>> = std::iter::once(&error.fix)
                .chain(&error.alternatives)
                .map(|s| s.argv().unwrap_or_default().to_vec())
                .collect();
            let want: Vec<Vec<String>> = ids
                .iter()
                .map(|id| match spelled {
                    "-t" => argv(&["install", "x", "-t", id]),
                    _ => argv(&["install", "x", &format!("--target={id}")]),
                })
                .collect();
            assert_eq!(fixes, want, "{case}");
        }
        let one = vec![four_targets().remove(0)];
        let found = resolve_one("all", &one, &table, &argv(&["install", "x", "-t", "all"]));
        assert_eq!(found.unwrap().target.id, "retail", "a single target");
    }

    #[test]
    fn all_is_a_reserved_id() {
        let table = RulesTable::builtin();
        let problem = id_problem("all", &table).unwrap_or_default();
        assert!(problem.contains("reserved"), "{problem}");
        let error = check_user_id(&Registry::default(), "all", &table).unwrap_err();
        assert_eq!(error.code, ErrorCode::Usage, "{}", error.message);
        for id in ["all-1", "ball", "allx", "all.x"] {
            assert_eq!(id_problem(id, &table), None, "{id}");
        }
    }

    #[test]
    fn the_reserved_id_warning_fires_only_when_the_keyword_is_shadowed() {
        let with_all = |missing: bool| {
            let mut targets = four_targets();
            let mut gone = Vec::new();
            if missing {
                gone.push(RegistryEntry {
                    key: TargetKey::new(),
                    id: "all".to_owned(),
                    install_root: PathBuf::from("/g/all"),
                    client_dir: PathBuf::from("/g/all"),
                    flavor_dir: None,
                    product: None,
                    qualifier: String::new(),
                    origin: Origin::Manual,
                    registered_at: Timestamp::UNIX_EPOCH,
                });
            } else {
                targets.push(discovered("all", Family::Mainline, None, None, "/g/all"));
            }
            Resolved {
                targets,
                warnings: Vec::new(),
                missing: gone,
            }
        };
        let without = Resolved {
            targets: four_targets(),
            warnings: Vec::new(),
            missing: Vec::new(),
        };
        for (case, selector, resolved, fires) in [
            ("keyword_and_a_discovered_all", "all", with_all(false), true),
            ("keyword_in_capitals", "ALL", with_all(false), true),
            ("keyword_and_a_missing_all", "all", with_all(true), true),
            ("another_selector", "retail", with_all(false), false),
            ("keyword_without_an_all", "all", without.clone(), false),
        ] {
            let warning = reserved_id_warning(selector, &resolved);
            assert_eq!(warning.is_some(), fires, "{case}: {warning:?}");
            let Some(warning) = warning else {
                continue;
            };
            assert_eq!(warning.code, WarningCode::TargetIdReserved, "{case}");
            assert_eq!(
                warning.message,
                "a client has the id \"all\", so -t all selected that client, not every client",
                "{case}"
            );
            let fix = warning.fix.unwrap_or_else(|| Suggestion::text(""));
            assert!(
                fix.display()
                    .contains("omaddons targets rename all <new id>"),
                "{case}: {fix:?}"
            );
            assert_eq!(fix.argv(), None, "{case}");
        }
    }

    /// A registered client whose directory is gone, named by its id or key, is not
    /// found, never the family its id shadows on another client.
    #[test]
    fn a_missing_client_is_never_resolved_to_another() {
        let table = RulesTable::builtin();
        let era = discovered("era", Family::Vanilla, None, None, "/g/era");
        let gone = RegistryEntry {
            key: TargetKey::new(),
            id: "vanilla".into(),
            install_root: PathBuf::from("/unplugged/vanilla"),
            client_dir: PathBuf::from("/unplugged/vanilla"),
            flavor_dir: None,
            product: None,
            qualifier: String::new(),
            origin: Origin::Manual,
            registered_at: Timestamp::now(),
        };
        let resolved = Resolved {
            targets: vec![era],
            warnings: Vec::new(),
            missing: vec![gone.clone()],
        };
        assert_eq!(
            resolve("vanilla", &resolved.targets, &table).unwrap()[0]
                .target
                .id,
            "era",
            "a family alone"
        );
        for selector in ["vanilla", &gone.key.to_string()] {
            let err = resolve_in(selector, &resolved, &table).unwrap_err();
            assert_eq!(err.code, ErrorCode::TargetNotFound, "{selector}");
            let err = resolve_one_in(selector, &resolved, &table, &[]).unwrap_err();
            assert_eq!(err.details["missing"], true, "{selector}");
        }
        assert_eq!(
            resolve_in("era", &resolved, &table).unwrap()[0].target.id,
            "era"
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

    /// `retail` (Mainline), `era` (Vanilla, the Classic Era client) and the 3.3.5a
    /// custom client `wotlk`.
    fn typo_targets() -> Vec<DiscoveredTarget> {
        vec![
            discovered(
                "retail",
                Family::Mainline,
                Some("wow"),
                Some("_retail_"),
                "/g/wow",
            ),
            discovered(
                "era",
                Family::Vanilla,
                Some("wow_classic_era"),
                Some("_classic_era_"),
                "/g/wow",
            ),
            discovered("wotlk", Family::WrathLegacy, None, None, "/g/wotlk"),
        ]
    }

    /// The closest words `resolve` recorded, empty for none.
    fn recorded(error: &Error) -> Vec<String> {
        error
            .closest_selectors
            .as_ref()
            .map(|c| c.closest.clone())
            .unwrap_or_default()
    }

    #[test]
    fn a_selector_that_matches_nothing_names_its_closest_words() {
        let targets = typo_targets();
        let table = RulesTable::builtin();
        for (case, typed, closest) in [
            ("transposed_id", "retial", &["retail"][..]),
            ("upper_case", "RETIAL", &["retail"]),
            ("custom_client_id", "wtolk", &["wotlk"]),
            ("family", "vanila", &["vanilla"]),
            ("mode", "sodd", &["sod"]),
            ("short", "er", &["era"]),
            ("folder_typo", "_retail", &["retail"]),
            ("product_code_typo", "wow_classic_ear", &[]),
            ("nothing_close", "nosuch", &[]),
        ] {
            let error = resolve(typed, &targets, &table).unwrap_err();
            let ctx = format!("{case}: {typed:?}: {error:?}");
            assert_eq!(error.code, ErrorCode::TargetNotFound, "{ctx}");
            assert_eq!(error.fix.argv(), Some(&argv(&["targets"])[..]), "{ctx}");
            assert!(error.alternatives.is_empty(), "{ctx}");
            assert_eq!(
                error.details,
                serde_json::json!({ "selector": typed }),
                "{ctx}"
            );
            assert_eq!(recorded(&error), closest, "{ctx}");
            if closest.is_empty() {
                assert_eq!(error.closest_selectors, None, "{ctx}");
            } else {
                let typed_back = error.closest_selectors.as_ref().map(|c| c.typed.as_str());
                assert_eq!(typed_back, Some(typed), "{ctx}");
            }
            let message = match closest.first() {
                Some(best) => {
                    format!("no target matches {typed:?}; the closest selector is {best:?}")
                }
                None => format!("no target matches {typed:?}"),
            };
            assert_eq!(error.message, message, "{ctx}");
        }
    }

    #[test]
    fn a_close_word_that_selects_no_client_here_is_never_offered() {
        let retail = vec![typo_targets().remove(0)];
        let table = RulesTable::builtin();
        for typed in ["sodd", "clasic", "vanila"] {
            let error = resolve(typed, &retail, &table).unwrap_err();
            assert_eq!(recorded(&error), Vec::<String>::new(), "{typed}");
            assert!(
                !error.message.contains("closest"),
                "{typed}: {}",
                error.message
            );
        }
    }

    #[test]
    fn a_known_mode_whose_client_is_missing_keeps_its_own_error() {
        let retail = vec![typo_targets().remove(0)];
        let error = resolve("sod", &retail, &RulesTable::builtin()).unwrap_err();
        assert_eq!(
            error.message,
            "Season of Discovery runs in the Classic Era client (_classic_era_), which was not found on this machine"
        );
        assert_eq!(error.fix.argv(), Some(&argv(&["targets", "--rescan"])[..]));
        assert_eq!(error.closest_selectors, None);
    }

    #[test]
    fn ties_go_to_ids_on_this_machine_then_alphabetical() {
        let table = RulesTable::builtin();
        let era = || {
            discovered(
                "era",
                Family::Vanilla,
                Some("wow_classic_era"),
                Some("_classic_era_"),
                "/g/wow",
            )
        };
        let sad = discovered("sad", Family::Vanilla, None, None, "/g/sad");
        let beta = discovered(
            "beta",
            Family::Mainline,
            Some("wow_beta"),
            Some("_beta_"),
            "/g/wow",
        );
        for (case, targets, typed, want) in [
            (
                "an id before a mode",
                vec![sad, era()],
                "sxd",
                &["sad", "sod"][..],
            ),
            (
                "ids alphabetical",
                vec![beta, era()],
                "bera",
                &["beta", "era"],
            ),
            (
                "built-in words alphabetical",
                vec![era()],
                "sdc",
                &["hc", "sod"],
            ),
        ] {
            let error = resolve(typed, &targets, &table).unwrap_err();
            assert_eq!(recorded(&error), want, "{case}: {typed}");
        }
    }

    #[test]
    fn first_match_is_resolve_without_its_path_step() {
        let targets = typo_targets();
        let table = RulesTable::builtin();
        let ids = |found: &[&DiscoveredTarget]| -> Vec<String> {
            found.iter().map(|d| d.target.id.clone()).collect()
        };
        let words = TargetVocabulary::builtin();
        let spellings: Vec<&str> = words
            .words()
            .map(|(spelling, _)| spelling)
            .filter(|spelling| !Path::new(spelling).exists())
            .collect();
        assert!(spellings.len() > 50, "{}", spellings.len());
        for word in spellings {
            let first = first_match(word, &targets, &table, None).map(|f| ids(&f));
            let resolved = resolve(word, &targets, &table).ok().map(|f| ids(&f));
            assert_eq!(first, resolved, "{word}");
        }
    }

    /// (case, argv after `omaddons`, the typed selector, the expected argv after `omaddons`).
    type SelectorCase<'a> = (&'a str, &'a [&'a str], &'a str, Option<&'a [&'a str]>);

    #[test]
    fn with_selector_replaces_the_value_where_it_was_typed() {
        use clap::Parser as _;
        let cases: &[SelectorCase<'_>] = &[
            (
                "short",
                &["status", "-t", "retial"],
                "retial",
                Some(&["status", "-t", "retail"]),
            ),
            (
                "long",
                &["status", "--target", "retial"],
                "retial",
                Some(&["status", "--target", "retail"]),
            ),
            (
                "long_equals",
                &["status", "--target=retial"],
                "retial",
                Some(&["status", "--target=retail"]),
            ),
            (
                "alias",
                &["status", "--flavor", "retial"],
                "retial",
                Some(&["status", "--flavor", "retail"]),
            ),
            (
                "alias_equals",
                &["status", "--flavor=retial"],
                "retial",
                Some(&["status", "--flavor=retail"]),
            ),
            (
                "attached",
                &["status", "-tretial"],
                "retial",
                Some(&["status", "-tretail"]),
            ),
            (
                "short_equals",
                &["status", "-t=retial"],
                "retial",
                Some(&["status", "-t=retail"]),
            ),
            (
                "cluster",
                &["status", "-vt", "retial"],
                "retial",
                Some(&["status", "-vt", "retail"]),
            ),
            (
                "cluster_attached",
                &["status", "-vtretial"],
                "retial",
                Some(&["status", "-vtretail"]),
            ),
            (
                "before_the_verb",
                &["-t", "retial", "status"],
                "retial",
                Some(&["-t", "retail", "status"]),
            ),
            (
                "presentation_dropped",
                &["status", "-t", "retial", "--json"],
                "retial",
                Some(&["status", "-t", "retail"]),
            ),
            (
                "copy_from",
                &["copy", "--from", "retial", "--to", "era"],
                "retial",
                Some(&["copy", "--from", "retail", "--to", "era"]),
            ),
            (
                "copy_to_equals",
                &["copy", "--from=era", "--to=retial"],
                "retial",
                Some(&["copy", "--from=era", "--to=retail"]),
            ),
            (
                "both_sides",
                &["copy", "--from", "retial", "--to", "retial"],
                "retial",
                Some(&["copy", "--from", "retail", "--to", "retail"]),
            ),
            (
                "other_flag_value",
                &["install", "x", "--reason", "retial", "-t", "retial"],
                "retial",
                Some(&["install", "x", "--reason", "retial", "-t", "retail"]),
            ),
            (
                "positional",
                &["remove", "retial", "-t", "retial"],
                "retial",
                Some(&["remove", "retial", "-t", "retail"]),
            ),
            (
                "after_double_dash",
                &["remove", "-t", "retial", "--", "retial"],
                "retial",
                Some(&["remove", "-t", "retail", "--", "retial"]),
            ),
            ("not_on_the_command_line", &["install", "x"], "retial", None),
            ("exact_only", &["status", "-t", "RETIAL"], "retial", None),
            (
                "as_typed",
                &["status", "-t", "RETIAL"],
                "RETIAL",
                Some(&["status", "-t", "retail"]),
            ),
        ];
        for (case, args, typed, want) in cases {
            let got = with_selector(&argv(args), typed, "retail");
            assert_eq!(got, want.map(argv), "{case}: {args:?}");
            if let Some(fix) = got {
                let parsed = crate::cli::Cli::try_parse_from(&fix);
                assert!(parsed.is_ok(), "{case}: {fix:?} does not parse: {parsed:?}");
            }
        }
    }

    #[test]
    fn fix_with_closest_selector_puts_the_closest_word_in_the_users_command() {
        use crate::error::UsageFix;
        let table = RulesTable::builtin();
        let targets = typo_targets();
        let fixes = |error: &Error| -> Vec<Vec<String>> {
            std::iter::once(&error.fix)
                .chain(&error.alternatives)
                .map(|s| s.argv().unwrap_or_default().to_vec())
                .collect()
        };

        // One close word: the fix, then the listing.
        let before = resolve("retial", &targets, &table).unwrap_err();
        let mut error = before.clone();
        fix_with_closest_selector(
            &mut error,
            &argv(&["remove", "dbm", "-t", "retial", "--json"]),
        );
        assert_eq!(
            fixes(&error),
            [argv(&["remove", "dbm", "-t", "retail"]), argv(&["targets"])],
            "one_close_word"
        );
        assert_eq!(error.usage_fix, UsageFix::UserVerb, "one_close_word");
        assert_eq!(
            (&error.code, error.retryable, &error.message, &error.details),
            (
                &before.code,
                before.retryable,
                &before.message,
                &before.details
            ),
            "one_close_word: the rest unchanged"
        );

        // Two: the others follow in rank order, the listing last.
        let sad = vec![
            discovered("sad", Family::Vanilla, None, None, "/g/sad"),
            typo_targets().remove(1),
        ];
        let mut error = resolve("sxd", &sad, &table).unwrap_err();
        fix_with_closest_selector(&mut error, &argv(&["status", "-t", "sxd"]));
        assert_eq!(
            fixes(&error),
            [
                argv(&["status", "-t", "sad"]),
                argv(&["status", "-t", "sod"]),
                argv(&["targets"])
            ],
            "two_close_words"
        );

        // Left exactly as they were.
        let ambiguous = resolve_one(
            "wrath-legacy",
            &[
                discovered("wotlk", Family::WrathLegacy, None, None, "/g/a"),
                discovered("wotlk@b", Family::WrathLegacy, None, None, "/g/b"),
            ],
            &table,
            &argv(&["install", "x", "-t", "wrath-legacy"]),
        )
        .unwrap_err();
        let retail = vec![typo_targets().remove(0)];
        for (case, before, typed_argv) in [
            (
                "nothing_close",
                resolve("nosuch", &targets, &table).unwrap_err(),
                argv(&["status", "-t", "nosuch"]),
            ),
            (
                "not_on_the_command_line",
                resolve("retial", &targets, &table).unwrap_err(),
                argv(&["install", "x"]),
            ),
            (
                "other_code",
                ambiguous,
                argv(&["install", "x", "-t", "wrath-legacy"]),
            ),
            (
                "mode_arm",
                resolve("sod", &retail, &table).unwrap_err(),
                argv(&["status", "-t", "sod"]),
            ),
        ] {
            let mut error = before.clone();
            fix_with_closest_selector(&mut error, &typed_argv);
            assert_eq!(error, before, "{case}");
        }
        let config = resolve("retial", &targets, &table).unwrap_err();
        assert!(config.message.contains("\"retail\""), "{}", config.message);
    }
}
