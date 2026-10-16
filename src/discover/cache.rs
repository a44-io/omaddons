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
use crate::rules::RulesTable;
use crate::state::paths::Paths;
use crate::state::store::{self, SchemaVersion, VersionStatus, Versioned};

/// How much of the cache to trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryMode {
    /// No scan: re-stat the cached roots only. New installs wait for a non-cached run.
    /// Without a cache this is `Full`.
    Cached,
    /// No scan and no cache write (`status --cached`): the cached roots re-stat'ed, or
    /// without a cache only the registered roots, parsed directly.
    NoScan,
    /// Scan for roots, reusing each unchanged root's cached parse.
    Full,
    /// Ignore the cache and parse every root (`targets --rescan`).
    Rescan,
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
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 1);
}

/// SHA-256 of this binary's version and the effective rules table (builtin plus
/// `[rules]` and `[client_rules.*]`): a rules edit or an upgrade re-parses every root
/// once, as `targets --rescan` would.
pub fn basis(table: &RulesTable) -> String {
    let mut hash = Sha256::new();
    hash.update(env!("CARGO_PKG_VERSION").as_bytes());
    hash.update([0]);
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
    let cache = match mode {
        DiscoveryMode::Rescan => None,
        DiscoveryMode::Cached | DiscoveryMode::NoScan | DiscoveryMode::Full => load_cache(paths),
    };
    let basis = basis(table);
    // The cache's roots are still where to look (no scan for `Cached` and `NoScan`), but
    // none of its parses is reused on another basis.
    let reusable = cache.as_ref().filter(|c| c.basis == basis);
    let mut diagnostics = Vec::new();
    let (requests, mut warnings) = match (mode, &cache) {
        (DiscoveryMode::Cached | DiscoveryMode::NoScan, Some(cache)) => (
            cache.roots.iter().map(CachedRoot::request).collect(),
            Vec::new(),
        ),
        (DiscoveryMode::NoScan, None) => (
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
        _ => {
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
    if changed && mode != DiscoveryMode::NoScan {
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
