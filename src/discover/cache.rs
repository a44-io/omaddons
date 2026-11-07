//! The discovery cache, `<cache>/omaddons/discovery.json` (vision §5.2 step 6): each
//! root's parsed targets, keyed by the (path, size, mtime) of every file that feeds
//! them and a hash of the root's listing. A directory's mtime does not change when
//! `.build.info` is rewritten in place, so files are stamped individually.
//!
//! Derived data: deleting it is always safe, and a corrupt, old or unreadable cache is
//! ignored and rewritten. The only write is the cache file, through fsx.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::battlenet::BattlenetHints;
use super::scan::{RootKind, ScanLimits};
use super::{
    DiscoveredTarget, Discovery, DiscoveryInput, DiscoverySource, RootRequest, Route, collect,
    find_child, find_path, finish, parse_root, root_kind,
};
use crate::error::Warning;
use crate::fsx::{Content, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::output::trace;
use crate::rules::RulesTable;
use crate::state::paths::Paths;
use crate::state::store::{self, SchemaVersion, VersionStatus, Versioned};

/// How much of the cache to trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryMode {
    /// No scan and no cache write (`status --cached`): the cached roots re-stat'ed, or
    /// without a cache only the registered roots, parsed directly.
    NoScan,
    /// Scan for roots, reusing each unchanged root's cached parse.
    Full,
    /// Ignore the cache and parse every root (`targets --rescan`).
    Rescan,
    /// Scan for roots, reusing each unchanged root's cached parse; never write the cache
    /// (the doctor's view: a diagnosis writes nothing outside its run directory).
    FullNoWrite,
}

impl DiscoveryMode {
    /// Whether it looks for roots on disk: every mode but `NoScan`.
    pub fn scans(self) -> bool {
        self != Self::NoScan
    }

    /// Whether it saves the cache when the cache changed: `Full` and `Rescan`.
    pub fn writes_cache(self) -> bool {
        matches!(self, Self::Full | Self::Rescan)
    }
}

/// The cache file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiscoveryCache {
    /// What every parse rests on besides the root's files: [`basis`] of the binary and
    /// the effective rules. A cache built on another basis reuses no root. Empty before
    /// 1.1.
    #[serde(default)]
    pub basis: String,
    /// Sorted by canonical path.
    pub roots: Vec<CachedRoot>,
}

impl Versioned for DiscoveryCache {
    const KIND: &'static str = "discovery_cache";
    /// 1.2: a root's `source` may be `registered` or `remembered`, which a 1.1 reader
    /// does not know (it ignores a newer cache).
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 2);
}

/// What a root's parse yields, beyond the rules: bumped when it changes while the
/// package version does not. 2: `unknown_flavor` is no longer the parse's but the
/// World's, said once config overrides applied. 3: other Blizzard games' flavor folders
/// are no targets. 4: neither are the folders Battle.net leaves of a client it
/// uninstalled (omad-launch-week-fixes-bmcx.1).
const PARSE_REVISION: u8 = 4;

/// SHA-256 of this binary's version, [`PARSE_REVISION`] and the effective rules table
/// (builtin plus `[rules]` and `[client_rules.*]`): a rules edit or an upgrade re-parses
/// every root once, as `targets --rescan` would.
pub fn basis(table: &RulesTable) -> String {
    basis_at(table, PARSE_REVISION)
}

/// [`basis`] as a build whose parse is at `revision` computes it.
fn basis_at(table: &RulesTable, revision: u8) -> String {
    let mut hash = Sha256::new();
    hash.update(env!("CARGO_PKG_VERSION").as_bytes());
    hash.update([0, revision, 0]);
    // A table that does not encode still yields a basis, just one shared by every such
    // table; the builtin and validated overrides always encode.
    hash.update(serde_json::to_vec(table).unwrap_or_default());
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// One root's parse and what it rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CachedRoot {
    pub path: PathBuf,
    pub canonical: PathBuf,
    pub kind: RootKind,
    pub source: DiscoverySource,
    pub qualifier: String,
    pub hints: BattlenetHints,
    pub hint_files: Vec<PathBuf>,
    /// Sorted by path.
    pub stamps: Vec<FileStamp>,
    /// SHA-256 of the root's sorted entry names, NUL-joined.
    pub listing_sha256: String,
    pub targets: Vec<DiscoveredTarget>,
    pub warnings: Vec<Warning>,
}

/// A file's size and mtime; a missing file is size 0, mtime -1.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct FileStamp {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
}

/// What the cache did, for tests and `-v`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// The filesystem was scanned for roots.
    pub scanned: bool,
    pub parsed: usize,
    pub reused: usize,
    /// The cache file was (re)written.
    pub written: bool,
}

impl CachedRoot {
    fn request(&self) -> RootRequest {
        RootRequest {
            path: self.path.clone(),
            canonical: self.canonical.clone(),
            kind: Some(self.kind),
            route: Route::new(&self.qualifier, self.source.clone()),
            hints: self.hints.clone(),
            hint_files: self.hint_files.clone(),
        }
    }

    fn matches(
        &self,
        req: &RootRequest,
        kind: RootKind,
        stamps: &[FileStamp],
        listing: &str,
    ) -> bool {
        self.canonical == req.canonical
            && self.kind == kind
            && self.qualifier == req.route.qualifier
            && self.source == req.route.source
            && self.hints == req.hints
            && self.stamps == stamps
            && self.listing_sha256 == listing
    }
}

/// Discovery through the cache: like [`super::discover_with`] but reusing unchanged
/// roots, then saving the cache when it changed. A failed cache write is a diagnostic.
pub fn discover_cached(
    input: &DiscoveryInput,
    table: &RulesTable,
    paths: &Paths,
    limits: ScanLimits,
    mode: DiscoveryMode,
) -> (Discovery, CacheStats) {
    let mut stats = CacheStats::default();
    let cache = load_cache(paths);
    let basis = basis(table);
    // The cache's roots are still where to look (no scan for `NoScan`), but none of its
    // parses is reused on another basis, or by `Rescan`.
    let reusable = cache
        .as_ref()
        .filter(|c| c.basis == basis && mode != DiscoveryMode::Rescan);
    // Roots found while their client ran stay a route, `Rescan` included; only under
    // this home, so a throwaway home sharing the cache never reaches the user's clients.
    // The process route finds canonical paths: under home's, or `~/Games`' own when it
    // links elsewhere.
    let canonical = |path: PathBuf| std::fs::canonicalize(&path).unwrap_or(path);
    let homes = [
        input.home.clone(),
        canonical(input.home.clone()),
        canonical(input.home.join("Games")),
    ];
    let remembered = cache
        .iter()
        .flat_map(|c| &c.roots)
        .filter(|r| {
            matches!(
                r.source,
                DiscoverySource::Process | DiscoverySource::Remembered
            ) && homes.iter().any(|home| r.path.starts_with(home))
        })
        .map(|r| (r.path.clone(), r.qualifier.clone()))
        .collect();
    let input = &DiscoveryInput {
        remembered_roots: remembered,
        ..input.clone()
    };
    let mut diagnostics = Vec::new();
    let (requests, mut warnings) = match (mode.scans(), &cache) {
        (false, Some(cache)) => (
            cache.roots.iter().map(CachedRoot::request).collect(),
            Vec::new(),
        ),
        (false, None) => (
            input
                .registered_roots
                .iter()
                .map(|(path, qualifier, source)| RootRequest {
                    path: path.clone(),
                    canonical: std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()),
                    kind: None,
                    route: Route::new(qualifier, source.clone()),
                    hints: BattlenetHints::default(),
                    hint_files: Vec::new(),
                })
                .collect(),
            Vec::new(),
        ),
        (true, _) => {
            stats.scanned = true;
            let collected = collect(input, limits);
            diagnostics = collected.diagnostics;
            (collected.roots, collected.warnings)
        }
    };

    let mut targets = Vec::new();
    let mut fresh = DiscoveryCache {
        basis,
        roots: Vec::new(),
    };
    for req in &requests {
        if !req.path.is_dir() {
            diagnostics.push(format!("{} is gone", req.path.display()));
            continue;
        }
        let Some(kind) = root_kind(req) else {
            diagnostics.push(format!("{} is not a WoW root", req.path.display()));
            continue;
        };
        let (stamps, listing) = fingerprint(req, kind);
        let hit = reusable.and_then(|c| {
            c.roots
                .iter()
                .find(|r| r.matches(req, kind, &stamps, &listing))
        });
        let (found, root_warnings) = match hit {
            Some(cached) => {
                trace::emit(|| {
                    format!(
                        "discovery root {}: reused from the discovery cache",
                        req.path.display()
                    )
                });
                stats.reused += 1;
                let found = cached
                    .targets
                    .iter()
                    .cloned()
                    .map(|mut d| {
                        d.target.refresh_rules(table);
                        d
                    })
                    .collect();
                (found, cached.warnings.clone())
            }
            None => {
                trace::emit(|| format!("discovery root {}: parsed", req.path.display()));
                stats.parsed += 1;
                match parse_root(req, table) {
                    Ok(parsed) => parsed,
                    Err(note) => {
                        diagnostics.push(note);
                        continue;
                    }
                }
            }
        };
        fresh.roots.push(CachedRoot {
            path: req.path.clone(),
            canonical: req.canonical.clone(),
            kind,
            source: req.route.source.clone(),
            qualifier: req.route.qualifier.clone(),
            hints: req.hints.clone(),
            hint_files: req.hint_files.clone(),
            stamps,
            listing_sha256: listing,
            targets: found.clone(),
            warnings: root_warnings.clone(),
        });
        targets.extend(found);
        warnings.extend(root_warnings);
    }
    fresh.roots.sort_by(|a, b| a.canonical.cmp(&b.canonical));

    let changed = match (&cache, store::encode(&fresh)) {
        (Some(old), Ok(new)) => store::encode(old).ok().as_ref() != Some(&new),
        _ => true,
    };
    if changed && mode.writes_cache() {
        match write_cache(paths, &fresh) {
            Ok(()) => stats.written = true,
            Err(note) => diagnostics.push(format!("discovery cache not written: {note}")),
        }
    }
    (finish(targets, warnings, diagnostics), stats)
}

/// The cache on disk, when it is readable and not newer than this binary.
fn load_cache(paths: &Paths) -> Option<DiscoveryCache> {
    match store::load::<DiscoveryCache>(&paths.discovery_cache()) {
        Ok(Some(loaded)) if loaded.status != VersionStatus::Newer => Some(loaded.value),
        _ => None,
    }
}

/// Writes the cache through an untracked, derived-data session.
fn write_cache(paths: &Paths, cache: &DiscoveryCache) -> Result<(), String> {
    let bytes = store::encode(cache).map_err(|e| e.message.clone())?;
    let scopes = ScopeTable::new(paths, &[], None).map_err(|e| e.message.clone())?;
    let mut s = WriteSession::untracked(Writer::Derived, &scopes, "discovery-cache");
    ensure_dir_all(&mut s, paths.cache_dir()).map_err(|e| e.to_string())?;
    let op = Op::WriteFile {
        content: Content::Bytes(&bytes),
        mode: Some(0o644),
    };
    mutate(&mut s, &paths.discovery_cache(), op).map_err(|e| e.to_string())?;
    Ok(())
}

/// The stamps of every file that feeds the root's targets, and its listing hash.
fn fingerprint(req: &RootRequest, kind: RootKind) -> (Vec<FileStamp>, String) {
    let root = &req.path;
    let names = sorted_names(root);
    let mut hash = Sha256::new();
    for name in &names {
        hash.update(name.as_bytes());
        hash.update([0]);
    }
    let listing = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();

    let mut stamps: Vec<FileStamp> = req.hint_files.iter().map(|f| stamp(f)).collect();
    let exes = |dir: &Path| -> Vec<FileStamp> {
        sorted_names(dir)
            .into_iter()
            .filter(|n| {
                let lower = n.to_ascii_lowercase();
                lower.starts_with("wow") && lower.ends_with(".exe")
            })
            .map(|n| stamp(&dir.join(n)))
            .collect()
    };
    let config_wtf = |dir: &Path| {
        stamp(&find_path(dir, &["WTF", "Config.wtf"]).unwrap_or_else(|| dir.join("WTF/Config.wtf")))
    };
    match kind {
        RootKind::Battlenet => {
            stamps.push(stamp(&root.join(".build.info")));
            for folder in names
                .iter()
                .filter(|n| n.len() > 2 && n.starts_with('_') && n.ends_with('_'))
            {
                let dir = root.join(folder);
                stamps.push(stamp(&dir.join(".flavor.info")));
                stamps.extend(exes(&dir));
                stamps.push(config_wtf(&dir));
            }
        }
        RootKind::Custom => {
            stamps.extend(exes(root));
            stamps.push(config_wtf(root));
            stamps.push(stamp(
                &find_child(root, "realmlist.wtf").unwrap_or_else(|| root.join("realmlist.wtf")),
            ));
            if let Some(data) = find_child(root, "Data") {
                for locale in sorted_names(&data) {
                    let dir = data.join(locale);
                    if dir.is_dir() {
                        stamps.push(stamp(
                            &find_child(&dir, "realmlist.wtf")
                                .unwrap_or_else(|| dir.join("realmlist.wtf")),
                        ));
                    }
                }
            }
        }
    }
    stamps.sort();
    (stamps, listing)
}

fn stamp(path: &Path) -> FileStamp {
    let meta = std::fs::metadata(path).ok();
    let mtime_ns = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(-1);
    FileStamp {
        path: path.to_path_buf(),
        size: meta.as_ref().map_or(0, std::fs::Metadata::len),
        mtime_ns: if meta.is_some() { mtime_ns } else { -1 },
    }
}

fn sorted_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_and_listings() {
        let dir = tempfile::tempdir().unwrap();
        let missing = stamp(&dir.path().join("nope"));
        assert_eq!((missing.size, missing.mtime_ns), (0, -1));
        std::fs::write(dir.path().join("b"), "12345").unwrap();
        std::fs::write(dir.path().join("a"), "").unwrap();
        let present = stamp(&dir.path().join("b"));
        assert_eq!(present.size, 5);
        assert!(present.mtime_ns > 0);
        assert_eq!(sorted_names(dir.path()), ["a", "b"]);
    }

    #[test]
    fn corrupt_and_future_caches_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        std::fs::create_dir_all(paths.cache_dir()).unwrap();
        assert_eq!(load_cache(&paths), None);
        for text in [
            "{",
            "[]",
            "{\"schema_version\":\"1.0\",\"roots\":7}",
            "{\"schema_version\":\"9.0\",\"roots\":[]}",
        ] {
            std::fs::write(paths.discovery_cache(), text).unwrap();
            assert_eq!(load_cache(&paths), None, "{text}");
        }
        std::fs::write(
            paths.discovery_cache(),
            "{\"schema_version\":\"1.0\",\"roots\":[]}",
        )
        .unwrap();
        // A 1.0 cache has no basis: it loads, and reuses nothing.
        assert_eq!(load_cache(&paths), Some(DiscoveryCache::default()));
    }

    #[test]
    fn discovery_mode_predicates() {
        let cases = [
            (DiscoveryMode::NoScan, false, false),
            (DiscoveryMode::Full, true, true),
            (DiscoveryMode::Rescan, true, true),
            (DiscoveryMode::FullNoWrite, true, false),
        ];
        for (mode, scans, writes_cache) in cases {
            assert_eq!(
                (mode.scans(), mode.writes_cache()),
                (scans, writes_cache),
                "case {mode:?}: got (scans, writes_cache) = ({}, {}), expected ({scans}, {writes_cache})",
                mode.scans(),
                mode.writes_cache()
            );
        }
    }

    #[test]
    fn full_no_write_scans_but_never_writes_the_cache() {
        // A tempdir home without clients.
        let home = || {
            let dir = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();
            let paths = Paths::with_roots(
                &root,
                root.join("config"),
                root.join("state"),
                root.join("cache"),
            );
            (dir, paths)
        };
        let run = |paths: &Paths, mode| {
            let input = DiscoveryInput {
                home: paths.home().to_path_buf(),
                ..DiscoveryInput::default()
            };
            let (_, stats) = discover_cached(
                &input,
                &RulesTable::builtin(),
                paths,
                ScanLimits::default(),
                mode,
            );
            stats
        };

        let (_first, written) = home();
        let stats = run(&written, DiscoveryMode::Full);
        assert!(
            stats.scanned && stats.written,
            "case full_writes: {stats:?}"
        );
        assert!(written.discovery_cache().exists(), "case full_writes");

        let (_second, paths) = home();
        let stats = run(&paths, DiscoveryMode::FullNoWrite);
        assert!(
            stats.scanned && !stats.written,
            "case full_no_write_never_writes: {stats:?}"
        );
        assert!(
            !paths.discovery_cache().exists(),
            "case full_no_write_never_writes: {} was written",
            paths.discovery_cache().display()
        );

        // An existing cache stays as it was, though it is stale (built on another basis)
        // and the home's listing changed: `Full` would rewrite it.
        let stale = store::encode(&DiscoveryCache {
            basis: "another basis".to_owned(),
            roots: Vec::new(),
        })
        .unwrap();
        std::fs::create_dir_all(paths.cache_dir()).unwrap();
        std::fs::write(paths.discovery_cache(), &stale).unwrap();
        std::fs::create_dir(paths.home().join("Games")).unwrap();
        let stats = run(&paths, DiscoveryMode::FullNoWrite);
        assert!(
            stats.scanned && !stats.written,
            "case full_no_write_keeps_an_existing_cache: {stats:?}"
        );
        assert_eq!(
            std::fs::read(paths.discovery_cache()).unwrap(),
            stale,
            "case full_no_write_keeps_an_existing_cache: the bytes changed"
        );
        let stats = run(&paths, DiscoveryMode::Full);
        assert!(
            stats.written,
            "case full_rewrites_the_stale_cache (the control): {stats:?}"
        );
        assert_ne!(std::fs::read(paths.discovery_cache()).unwrap(), stale);
    }

    /// A cache parsed before revision 4, which still listed an uninstalled client's
    /// folder as a target, reuses nothing: `status --cached` (`NoScan`) parses the root
    /// again without writing, and one plain `status` (`Full`) writes the new parse,
    /// which `--cached` then reuses. The control: on this build's basis the same
    /// stand-in is reused, so the test would see a stale parse if the revision did not
    /// move.
    #[test]
    fn a_cache_parsed_before_revision_4_is_parsed_again() {
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(
            &home,
            home.join("config"),
            home.join("state"),
            home.join("cache"),
        );
        let root = home.join("wow");
        let write = |path: PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            root.join(".build.info"),
            "Active!DEC:1|Version!STRING:0|Product!STRING:0\n1|1.60.1.70790|wowf\n",
        );
        write(
            root.join("_forever_").join(".flavor.info"),
            "Product Flavor!STRING:0\nwowf\n",
        );
        write(root.join("_forever_").join("Wow.exe"), "MZ");
        std::fs::create_dir_all(root.join("_forever_/Interface/AddOns")).unwrap();
        // The ghost: no `.flavor.info`, no executable, no `.build.info` row.
        std::fs::create_dir_all(root.join("_classic_/Interface/AddOns")).unwrap();
        write(root.join("_classic_").join("WowClassic_loader.dll"), "MZ");

        let table = RulesTable::builtin();
        let input = DiscoveryInput {
            home: home.clone(),
            extra_roots: vec![root.clone()],
            ..DiscoveryInput::default()
        };
        let run = |mode| discover_cached(&input, &table, &paths, ScanLimits::default(), mode);
        let folders = |discovery: &Discovery| -> Vec<String> {
            discovery
                .targets
                .iter()
                .filter_map(|d| d.target.flavor_dir.clone())
                .collect()
        };
        let (found, stats) = run(DiscoveryMode::Full);
        assert_eq!(folders(&found), ["_forever_"], "case first_scan: {stats:?}");
        assert!(stats.written, "case first_scan: {stats:?}");

        // A cache as a revision-3 build wrote it: the same stamps, its own basis, and
        // the ghost among the root's targets.
        let plant = |revision: u8| {
            let mut cache = load_cache(&paths).unwrap();
            cache.basis = basis_at(&table, revision);
            let parsed = &mut cache.roots[0];
            let mut ghost = parsed.targets[0].clone();
            ghost.target.flavor_dir = Some("_classic_".to_owned());
            ghost.target.client_dir = root.join("_classic_");
            ghost.target.id = "classic".to_owned();
            ghost.default_id = "classic".to_owned();
            parsed.targets.push(ghost);
            std::fs::write(paths.discovery_cache(), store::encode(&cache).unwrap()).unwrap();
        };
        assert_ne!(basis(&table), basis_at(&table, 3), "the revision moved");

        plant(PARSE_REVISION);
        let (found, stats) = run(DiscoveryMode::NoScan);
        assert_eq!(
            folders(&found),
            ["_classic_", "_forever_"],
            "case control_this_basis_is_reused: {stats:?}"
        );
        assert_eq!(
            (stats.parsed, stats.reused),
            (0, 1),
            "case control: {stats:?}"
        );

        plant(3);
        for case in ["cached_parses_again", "cached_again_still_parses"] {
            let (found, stats) = run(DiscoveryMode::NoScan);
            assert_eq!(folders(&found), ["_forever_"], "case {case}: {stats:?}");
            assert_eq!(
                (stats.parsed, stats.reused, stats.written),
                (1, 0, false),
                "case {case}: --cached parses, and writes nothing"
            );
        }
        let (found, stats) = run(DiscoveryMode::Full);
        assert_eq!(
            folders(&found),
            ["_forever_"],
            "case plain_status: {stats:?}"
        );
        assert_eq!(
            (stats.parsed, stats.reused, stats.written),
            (1, 0, true),
            "case plain_status: parsed once, and written"
        );
        let (found, stats) = run(DiscoveryMode::NoScan);
        assert_eq!(
            folders(&found),
            ["_forever_"],
            "case cached_after: {stats:?}"
        );
        assert_eq!(
            (stats.parsed, stats.reused),
            (0, 1),
            "case cached_after: the new parse is reused"
        );
    }

    #[test]
    fn the_basis_follows_the_effective_rules() {
        let builtin = RulesTable::builtin();
        assert_eq!(basis(&builtin), basis(&RulesTable::builtin()));
        assert_eq!(basis(&builtin).len(), 64);
        let overrides: crate::rules::RulesOverrides = toml::from_str(
            r#"
            [[version_families]]
            install_kind = "custom"
            major = { min = 3, max = 3 }
            minor = { min = 3, max = 3 }
            family = "tbc"
            "#,
        )
        .unwrap();
        let patched = builtin.with_overrides(&overrides).unwrap();
        assert_ne!(basis(&builtin), basis(&patched));
    }
}
