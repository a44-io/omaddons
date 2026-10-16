//! Client discovery: launcher and Battle.net hints, the bounded scan, `.build.info`,
//! `.flavor.info`, `product.db`, `dosdevices`, PE versions, realmlist labels, running
//! clients, and the target registry.
//!
//! Discovery only reads. Every file it reads is client metadata outside omaddons'
//! control, so reads are bounded by [`DISCOVERY_FILE_MAX_BYTES`] and decoded as lossy
//! UTF-8, and a missing or unreadable file is an absent hint, never an error.
//!
//! [`discover`] finds every WoW root with zero configuration (vision §5.2 steps 1–2).
//! Hints come first, in this order: `product.db` per prefix, launcher configs, running
//! processes and extra roots. The bounded scan comes next, and registered roots no
//! route reached come last. A root reachable several ways is kept once, in its first
//! route's terms (source and qualifier).

pub mod battlenet;
pub mod bpsv;
pub mod cache;
pub mod config_wtf;
pub mod custom;
pub mod dosdevices;
pub mod launchers;
pub mod pe;
pub mod product_db;
pub mod realmlist;
pub mod registry;
pub mod running;
pub mod scan;
pub mod target;

use std::io::Read;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

pub use scan::ScanLimits;
pub use target::{DiscoveredTarget, DiscoverySource};

use self::battlenet::BattlenetHints;
use self::scan::{Base, RootKind, Scanner};
use crate::error::{Suggestion, Warning, WarningCode};
use crate::limits::DISCOVERY_FILE_MAX_BYTES;
use crate::rules::RulesTable;

/// What discovery starts from besides the filesystem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryInput {
    pub home: PathBuf,
    /// Config `extra_roots` and the registry's manual roots, scanned like `<home>/Games`.
    pub extra_roots: Vec<PathBuf>,
    /// Prefixes known from elsewhere, e.g. running processes.
    pub extra_prefixes: Vec<ExtraPrefix>,
    /// Launcher hints besides the launcher configs, which discovery reads itself.
    pub launcher_hints: Vec<LauncherHint>,
    /// `$XDG_CONFIG_HOME` and `$XDG_DATA_HOME`, where launchers keep their configs;
    /// `None` is the XDG default under `home`.
    pub config_home: Option<PathBuf>,
    pub data_home: Option<PathBuf>,
    /// Registered targets' roots (install root, id qualifier, how it was found), which
    /// [`cache::DiscoveryMode::NoScan`] parses directly when there is no cache.
    pub registered_roots: Vec<(PathBuf, String, DiscoverySource)>,
}

/// A Wine prefix known from outside the scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraPrefix {
    pub path: PathBuf,
    pub qualifier: String,
    pub source: DiscoverySource,
}

/// What a launcher's configuration says about a game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LauncherHint {
    /// `lutris`, `heroic`, `steam` or `bottles`; also the qualifier.
    pub launcher: String,
    pub flatpak: bool,
    pub prefix: Option<PathBuf>,
    pub exe: Option<PathBuf>,
}

/// Everything discovery found, in a deterministic order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Discovery {
    /// Sorted by install root, then flavor folder.
    pub targets: Vec<DiscoveredTarget>,
    /// Sorted by code, then message.
    pub warnings: Vec<Warning>,
    /// Debug notes (unreadable directories, hints that lead nowhere), shown at `-v`.
    pub diagnostics: Vec<String>,
}

/// Discovers every client under `input.home` and the hinted places, within the
/// default scan limits.
pub fn discover(input: &DiscoveryInput, table: &RulesTable) -> Discovery {
    discover_with(input, table, ScanLimits::default())
}

/// [`discover`] with explicit scan limits.
pub fn discover_with(input: &DiscoveryInput, table: &RulesTable, limits: ScanLimits) -> Discovery {
    let Collected {
        roots,
        mut warnings,
        mut diagnostics,
    } = collect(input, limits);
    let mut targets = Vec::new();
    for root in &roots {
        match parse_root(root, table) {
            Ok((found, w)) => {
                targets.extend(found);
                warnings.extend(w);
            }
            Err(note) => diagnostics.push(note),
        }
    }
    finish(targets, warnings, diagnostics)
}

/// The roots to parse, in route order, with the scan's truncation warning.
pub(crate) struct Collected {
    pub roots: Vec<RootRequest>,
    pub warnings: Vec<Warning>,
    pub diagnostics: Vec<String>,
}

/// Hints, then the bounded scan: every root to parse, once each.
pub(crate) fn collect(input: &DiscoveryInput, limits: ScanLimits) -> Collected {
    let dirs = launchers::LauncherDirs::new(
        &input.home,
        input.config_home.as_deref(),
        input.data_home.as_deref(),
    );
    let (read, mut diagnostics) = launchers::collect_hints(&dirs);
    let hints: Vec<LauncherHint> = input.launcher_hints.iter().cloned().chain(read).collect();
    let prefixes = collect_prefixes(input, &hints, &mut diagnostics);
    let mut roots = Candidates::default();

    // (a) product.db: install paths named directly, on any drive.
    for prefix in &prefixes {
        let db = product_db::product_db_path(&prefix.path);
        for entry in product_db::read_product_db(&prefix.path) {
            let Some(path) = dosdevices::map_windows_path(&prefix.path, &entry.install_path) else {
                diagnostics.push(format!(
                    "product.db in {} names {}, which does not resolve",
                    prefix.path.display(),
                    entry.install_path
                ));
                continue;
            };
            let mut hints = BattlenetHints::default();
            if let (Some(folder), Some(product)) = (&entry.flavor_folder, &entry.product) {
                hints
                    .product_by_folder
                    .insert(folder.clone(), product.clone());
            }
            if let (Some(product), Some(version)) = (&entry.product, &entry.version) {
                hints
                    .version_by_product
                    .insert(product.clone(), version.clone());
            }
            let route = Route::new(&prefix.qualifier, DiscoverySource::ProductDb);
            roots.add(path, None, route, hints, Some(db.clone()), &mut diagnostics);
        }
    }

    // (b) launcher exes: the exe's directory, or its flavor folder's parent.
    for hint in &hints {
        let Some(exe) = &hint.exe else { continue };
        match root_of_exe(exe) {
            Some((path, kind)) => {
                let source = DiscoverySource::Launcher {
                    name: hint.launcher.clone(),
                };
                let route = Route::new(&slug(&hint.launcher), source);
                roots.add(
                    path,
                    Some(kind),
                    route,
                    BattlenetHints::default(),
                    None,
                    &mut diagnostics,
                );
            }
            None => diagnostics.push(format!(
                "{} names {}, which is not in a WoW root",
                hint.launcher,
                exe.display()
            )),
        }
    }

    // Launcher and process prefixes, extra roots, then the plain scan.
    let mut scanner = Scanner::new(limits);
    for prefix in prefixes.iter().filter(|p| !p.scanned) {
        let route = Route::new(&prefix.qualifier, prefix.source.clone());
        for (path, kind) in scanner.walk(&prefix.path.join("drive_c"), Base::DriveC) {
            roots.add(
                path,
                Some(kind),
                route.clone(),
                BattlenetHints::default(),
                None,
                &mut diagnostics,
            );
        }
    }
    for extra in &input.extra_roots {
        for (path, kind) in scanner.walk(extra, Base::Root) {
            let route = Route::new(&dir_slug(&path), DiscoverySource::ExtraRoot);
            roots.add(
                path,
                Some(kind),
                route,
                BattlenetHints::default(),
                None,
                &mut diagnostics,
            );
        }
    }
    for prefix in prefixes.iter().filter(|p| p.scanned) {
        let route = Route::new(&prefix.qualifier, prefix.source.clone());
        for (path, kind) in scanner.walk(&prefix.path.join("drive_c"), Base::DriveC) {
            roots.add(
                path,
                Some(kind),
                route.clone(),
                BattlenetHints::default(),
                None,
                &mut diagnostics,
            );
        }
    }
    let games = input.home.join("Games");
    if games.is_dir() {
        for (path, kind) in scanner.walk(&games, Base::Games) {
            let route = Route::new(&dir_slug(&path), DiscoverySource::Scan);
            roots.add(
                path,
                Some(kind),
                route,
                BattlenetHints::default(),
                None,
                &mut diagnostics,
            );
        }
    }
    // Last, registered roots no route reached that are still WoW roots: a client found
    // once (while it ran, say) is found after it exits, under its recorded qualifier.
    for (path, qualifier, source) in &input.registered_roots {
        let kind = if battlenet::is_battlenet_root(path) {
            RootKind::Battlenet
        } else if custom::is_custom_root(path) {
            RootKind::Custom
        } else {
            continue;
        };
        roots.add(
            path.clone(),
            Some(kind),
            Route::new(qualifier, source.clone()),
            BattlenetHints::default(),
            None,
            &mut diagnostics,
        );
    }

    let mut warnings = Vec::new();
    if let Some(base) = &scanner.truncated {
        warnings.push(
            Warning::new(
                WarningCode::DiscoveryTruncated,
                format!(
                    "discovery stopped after visiting {} directories while scanning {}",
                    scanner.count,
                    base.display()
                ),
            )
            .with_fix(Suggestion::text(
                "add the client with: omaddons targets add <path>",
            )),
        );
    }
    diagnostics.append(&mut scanner.diagnostics);
    Collected {
        roots: roots.0,
        warnings,
        diagnostics,
    }
}

/// The root's kind: as found, else by looking.
pub(crate) fn root_kind(root: &RootRequest) -> Option<RootKind> {
    root.kind.or_else(|| {
        if battlenet::is_battlenet_root(&root.path) {
            Some(RootKind::Battlenet)
        } else if custom::is_custom_root(&root.path) {
            Some(RootKind::Custom)
        } else {
            None
        }
    })
}

/// The targets and warnings of one root; a diagnostic when it is not a WoW root.
pub(crate) fn parse_root(
    root: &RootRequest,
    table: &RulesTable,
) -> Result<(Vec<DiscoveredTarget>, Vec<Warning>), String> {
    let (q, s) = (&root.route.qualifier, &root.route.source);
    match root_kind(root) {
        Some(RootKind::Battlenet) => Ok(battlenet::discover_root(
            &root.path,
            table,
            &root.hints,
            q,
            s,
        )),
        Some(RootKind::Custom) => {
            let (found, w) = custom::discover_custom_root(&root.path, table, q, s);
            Ok((found.into_iter().collect(), w))
        }
        None => Err(format!("{} is not a WoW root", root.path.display())),
    }
}

/// Sorts targets by install root then flavor folder, and warnings by code then
/// message, so the same filesystem always gives the same output.
pub(crate) fn finish(
    mut targets: Vec<DiscoveredTarget>,
    mut warnings: Vec<Warning>,
    diagnostics: Vec<String>,
) -> Discovery {
    targets.sort_by(|a, b| {
        let key = |t: &DiscoveredTarget| {
            (
                t.target
                    .install_root
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec(),
                t.target.flavor_dir.clone(),
            )
        };
        key(a).cmp(&key(b))
    });
    warnings.sort_by(|a, b| (a.code.wire(), &a.message).cmp(&(b.code.wire(), &b.message)));
    Discovery {
        targets,
        warnings,
        diagnostics,
    }
}

/// How a root was reached: what the registry appends on an id collision, and the
/// source recorded with the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Route {
    pub qualifier: String,
    pub source: DiscoverySource,
}

impl Route {
    pub(crate) fn new(qualifier: &str, source: DiscoverySource) -> Self {
        Self {
            qualifier: qualifier.to_owned(),
            source,
        }
    }
}

/// A Wine prefix to read `product.db` from and scan.
struct Prefix {
    path: PathBuf,
    qualifier: String,
    source: DiscoverySource,
    /// Found by looking (`<home>/Games/*`, `~/.wine`), not from a hint.
    scanned: bool,
}

/// Launcher prefixes (Steam's `compatdata/*/pfx` among them), then extra (process)
/// prefixes, then `<home>/Games/*` and `<home>/.wine`; each prefix once.
fn collect_prefixes(
    input: &DiscoveryInput,
    hints: &[LauncherHint],
    diagnostics: &mut Vec<String>,
) -> Vec<Prefix> {
    let mut found: Vec<Prefix> = Vec::new();
    let mut seen = Vec::new();
    let mut add = |path: PathBuf, qualifier: String, source: DiscoverySource, scanned: bool| {
        if !scan::is_prefix(&path) {
            if !scanned {
                diagnostics.push(format!("{} is not a Wine prefix", path.display()));
            }
            return;
        }
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.contains(&canonical) {
            return;
        }
        seen.push(canonical);
        found.push(Prefix {
            path,
            qualifier,
            source,
            scanned,
        });
    };
    for hint in hints {
        if let Some(prefix) = &hint.prefix {
            let source = DiscoverySource::Launcher {
                name: hint.launcher.clone(),
            };
            add(prefix.clone(), slug(&hint.launcher), source, false);
        }
    }
    for extra in &input.extra_prefixes {
        add(
            extra.path.clone(),
            extra.qualifier.clone(),
            extra.source.clone(),
            false,
        );
    }
    for dir in sorted_dirs(&input.home.join("Games")) {
        let qualifier = dir_slug(&dir);
        add(dir, qualifier, DiscoverySource::Scan, true);
    }
    add(
        input.home.join(".wine"),
        "wine".to_owned(),
        DiscoverySource::Scan,
        true,
    );
    found
}

/// The subdirectories of `dir`, sorted; none when it cannot be listed.
fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The WoW root an exe belongs to: the Battle.net root above its flavor folder, else
/// its directory when that is a root.
fn root_of_exe(exe: &Path) -> Option<(PathBuf, RootKind)> {
    let dir = exe.parent()?;
    if let Some(parent) = dir.parent()
        && battlenet::is_battlenet_root(parent)
    {
        return Some((parent.to_path_buf(), RootKind::Battlenet));
    }
    if battlenet::is_battlenet_root(dir) {
        return Some((dir.to_path_buf(), RootKind::Battlenet));
    }
    custom::is_custom_root(dir).then(|| (dir.to_path_buf(), RootKind::Custom))
}

/// One root to parse, reached first through `route`.
#[derive(Debug, Clone)]
pub(crate) struct RootRequest {
    pub path: PathBuf,
    pub canonical: PathBuf,
    pub kind: Option<RootKind>,
    pub route: Route,
    pub hints: BattlenetHints,
    /// The `product.db` files the hints came from.
    pub hint_files: Vec<PathBuf>,
}

/// Roots in route order, each kept once by canonical path. Hints from later routes
/// still merge in (one product.db lists every product of a root separately).
#[derive(Default)]
struct Candidates(Vec<RootRequest>);

impl Candidates {
    fn add(
        &mut self,
        path: PathBuf,
        kind: Option<RootKind>,
        route: Route,
        hints: BattlenetHints,
        hint_file: Option<PathBuf>,
        diagnostics: &mut Vec<String>,
    ) {
        let canonical = match std::fs::canonicalize(&path) {
            Ok(canonical) => canonical,
            Err(err) => {
                diagnostics.push(format!("cannot resolve {}: {err}", path.display()));
                return;
            }
        };
        if let Some(existing) = self.0.iter_mut().find(|c| c.canonical == canonical) {
            for (folder, product) in hints.product_by_folder {
                existing
                    .hints
                    .product_by_folder
                    .entry(folder)
                    .or_insert(product);
            }
            for (product, version) in hints.version_by_product {
                existing
                    .hints
                    .version_by_product
                    .entry(product)
                    .or_insert(version);
            }
            if let Some(file) = hint_file
                && !existing.hint_files.contains(&file)
            {
                existing.hint_files.push(file);
            }
            existing.kind = existing.kind.or(kind);
            return;
        }
        self.0.push(RootRequest {
            path,
            canonical,
            kind,
            route,
            hints,
            hint_files: hint_file.into_iter().collect(),
        });
    }
}

/// A name as an id or qualifier: lowercase, anything outside `[a-z0-9-]` becomes `-`.
pub(crate) fn slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// [`slug`] of a directory's name.
pub(crate) fn dir_slug(dir: &Path) -> String {
    slug(
        &dir.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
    )
}

/// The first [`DISCOVERY_FILE_MAX_BYTES`] of a regular file as lossy UTF-8; `None` when
/// it is missing, not a file or unreadable.
pub(crate) fn read_text(path: &Path) -> Option<String> {
    let file = open_regular(path)?;
    let mut bytes = Vec::new();
    file.take(DISCOVERY_FILE_MAX_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The bytes of a regular file of at most [`DISCOVERY_FILE_MAX_BYTES`]; `None` when it
/// is missing, not a file, unreadable or larger (a binary format is not cut short).
pub(crate) fn read_bytes(path: &Path) -> Option<Vec<u8>> {
    let file = open_regular(path)?;
    let mut bytes = Vec::new();
    file.take(DISCOVERY_FILE_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= DISCOVERY_FILE_MAX_BYTES).then_some(bytes)
}

/// Opens `path` only if it is a regular file (after following symlinks): opening a FIFO
/// planted under a client name would block discovery until a writer appears.
pub(crate) fn open_regular(path: &Path) -> Option<std::fs::File> {
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

/// `dir/name`, matched ignoring ASCII case when the exact name is absent: Wine
/// prefixes are case-sensitive on disk, but clients and launchers are not.
pub(crate) fn find_child(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.symlink_metadata().is_ok() {
        return Some(exact);
    }
    find_children(dir, name).into_iter().next()
}

/// Every entry of `dir` named `name` ignoring ASCII case, sorted; Wine would read only
/// one of several.
pub(crate) fn find_children(dir: &Path, name: &str) -> Vec<PathBuf> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut matches: Vec<PathBuf> = listing
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .map(|entry| entry.path())
        .collect();
    matches.sort();
    matches
}

/// `dir` joined with each component of `rel`, each matched by [`find_child`].
pub(crate) fn find_path(dir: &Path, rel: &[&str]) -> Option<PathBuf> {
    rel.iter()
        .try_fold(dir.to_path_buf(), |at, part| find_child(&at, part))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn reads_are_bounded_lossy_and_optional() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big");
        fs::write(&big, vec![b'a'; DISCOVERY_FILE_MAX_BYTES as usize + 10]).unwrap();
        assert_eq!(
            read_text(&big).unwrap().len() as u64,
            DISCOVERY_FILE_MAX_BYTES
        );
        let bad = dir.path().join("bad");
        fs::write(&bad, b"ok\xffok").unwrap();
        assert_eq!(read_text(&bad).unwrap(), "ok\u{fffd}ok");
        assert_eq!(read_text(&dir.path().join("missing")), None);
        assert_eq!(read_text(dir.path()), None, "a directory is not a file");
        assert_eq!(
            read_bytes(&big),
            None,
            "binary reads refuse oversized files"
        );
        let fifo = dir.path().join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success());
        if made {
            assert_eq!(read_text(&fifo), None, "a FIFO is never opened");
            assert_eq!(read_bytes(&fifo), None);
        }
        let exact = dir.path().join("exact");
        fs::write(&exact, vec![1u8; DISCOVERY_FILE_MAX_BYTES as usize]).unwrap();
        assert_eq!(
            read_bytes(&exact).map(|b| b.len() as u64),
            Some(DISCOVERY_FILE_MAX_BYTES)
        );
    }

    #[test]
    fn children_match_ignoring_ascii_case() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("wtf")).unwrap();
        fs::write(dir.path().join("wtf").join("CONFIG.WTF"), "x").unwrap();
        assert_eq!(
            find_path(dir.path(), &["WTF", "Config.wtf"]),
            Some(dir.path().join("wtf").join("CONFIG.WTF"))
        );
        assert_eq!(find_child(dir.path(), "nope"), None);
        fs::write(dir.path().join("WTF"), "exact wins").unwrap();
        assert_eq!(find_child(dir.path(), "WTF"), Some(dir.path().join("WTF")));
    }
}
