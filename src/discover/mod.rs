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
//! processes and extra roots. The bounded scan comes last. A root reachable several ways
//! is kept once, in its first route's terms (source and qualifier).

pub mod battlenet;
pub mod bpsv;
pub mod config_wtf;
pub mod custom;
pub mod dosdevices;
pub mod pe;
pub mod product_db;
pub mod realmlist;
pub mod registry;
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
    /// Launcher configuration hints (the launcher readers fill these).
    pub launcher_hints: Vec<LauncherHint>,
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
    let mut diagnostics = Vec::new();
    let prefixes = collect_prefixes(input, &mut diagnostics);
    let mut roots = Candidates::default();

    // (a) product.db: install paths named directly, on any drive.
    for prefix in &prefixes {
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
            roots.add(path, None, route, hints, &mut diagnostics);
        }
    }

    // (b) launcher exes: the exe's directory, or its flavor folder's parent.
    for hint in &input.launcher_hints {
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
                &mut diagnostics,
            );
        }
    }

    let mut targets = Vec::new();
    let mut warnings = Vec::new();
    for root in roots.0 {
        let kind = root.kind.or_else(|| {
            if battlenet::is_battlenet_root(&root.path) {
                Some(RootKind::Battlenet)
            } else if custom::is_custom_root(&root.path) {
                Some(RootKind::Custom)
            } else {
                None
            }
        });
        let (q, s) = (&root.route.qualifier, &root.route.source);
        match kind {
            Some(RootKind::Battlenet) => {
                let (found, w) = battlenet::discover_root(&root.path, table, &root.hints, q, s);
                targets.extend(found);
                warnings.extend(w);
            }
            Some(RootKind::Custom) => {
                let (found, w) = custom::discover_custom_root(&root.path, table, q, s);
                targets.extend(found);
                warnings.extend(w);
            }
            None => diagnostics.push(format!("{} is not a WoW root", root.path.display())),
        }
    }
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
#[derive(Debug, Clone)]
struct Route {
    qualifier: String,
    source: DiscoverySource,
}

impl Route {
    fn new(qualifier: &str, source: DiscoverySource) -> Self {
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
    /// Found by looking (`<home>/Games/*`, `~/.wine`, Steam), not from a hint.
    scanned: bool,
}

/// Launcher prefixes, then extra (process) prefixes, then `<home>/Games/*`,
/// `<home>/.wine` and the default Steam library's `compatdata/*/pfx`; each prefix once.
fn collect_prefixes(input: &DiscoveryInput, diagnostics: &mut Vec<String>) -> Vec<Prefix> {
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
    for hint in &input.launcher_hints {
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
    let compatdata = input.home.join(".local/share/Steam/steamapps/compatdata");
    for app in sorted_dirs(&compatdata) {
        add(
            app.join("pfx"),
            "steam".to_owned(),
            DiscoverySource::Scan,
            true,
        );
    }
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

/// One root to discover, reached first through `route`.
struct Candidate {
    path: PathBuf,
    canonical: PathBuf,
    kind: Option<RootKind>,
    route: Route,
    hints: BattlenetHints,
}

/// Roots in route order, each kept once by canonical path. Hints from later routes
/// still merge in (one product.db lists every product of a root separately).
#[derive(Default)]
struct Candidates(Vec<Candidate>);

impl Candidates {
    fn add(
        &mut self,
        path: PathBuf,
        kind: Option<RootKind>,
        route: Route,
        hints: BattlenetHints,
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
            existing.kind = existing.kind.or(kind);
            return;
        }
        self.0.push(Candidate {
            path,
            canonical,
            kind,
            route,
            hints,
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
    let mut matches: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
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
    matches.into_iter().next()
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
