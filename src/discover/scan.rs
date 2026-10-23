//! The bounded scan (vision §5.2 step 2): breadth-first walks under each prefix's
//! `drive_c`, under `<home>/Games` and under each extra root, sharing one budget of
//! visited directories. Each directory is listed once and classified from that listing.
//! Symlinked directories are followed with a `(dev, ino)` loop guard. Reads only.

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::custom::pick_exe;
use crate::politeness::{DISCOVERY_MAX_DEPTH, DISCOVERY_MAX_DIRS, DISCOVERY_SKIP_DIRS};

/// The scan's bounds; [`Default`] is the politeness constants. Tests shrink them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanLimits {
    /// Directories deeper than this below a base are not visited.
    pub max_depth: usize,
    /// Directories visited across all bases before the scan stops.
    pub max_dirs: usize,
    /// Children of `drive_c` never entered, matched ignoring ASCII case.
    pub skip_dirs: &'static [&'static str],
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_depth: DISCOVERY_MAX_DEPTH,
            max_dirs: DISCOVERY_MAX_DIRS,
            skip_dirs: DISCOVERY_SKIP_DIRS,
        }
    }
}

/// What a directory is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    Battlenet,
    Custom,
}

/// Where a walk starts, which decides what it skips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Base {
    /// A prefix's `drive_c`: [`ScanLimits::skip_dirs`] apply to its children.
    DriveC,
    /// `<home>/Games`: Wine prefixes below it are scanned as their own `drive_c`.
    Games,
    /// An extra root: nothing is skipped.
    Root,
}

/// One directory entry, with symlinks resolved for the kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_file: bool,
}

/// The entries of `dir`, sorted by name; names that are not UTF-8 are kept lossily.
pub(crate) fn list(dir: &Path) -> std::io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| {
            let kind = entry.file_type().ok();
            let followed = || std::fs::metadata(entry.path()).ok();
            let (is_dir, is_file) = match kind {
                Some(k) if k.is_symlink() => {
                    followed().map_or((false, false), |m| (m.is_dir(), m.is_file()))
                }
                Some(k) => (k.is_dir(), k.is_file()),
                None => (false, false),
            };
            Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir,
                is_file,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// A Battle.net root has `.build.info`; a custom root has a client exe and an
/// `Interface/` directory and no `.build.info` (names ignore ASCII case).
pub(crate) fn classify(entries: &[Entry]) -> Option<RootKind> {
    if entries
        .iter()
        .any(|e| e.is_file && e.name.eq_ignore_ascii_case(".build.info"))
    {
        return Some(RootKind::Battlenet);
    }
    let interface = entries
        .iter()
        .any(|e| e.is_dir && e.name.eq_ignore_ascii_case("Interface"));
    (interface && has_client_exe(entries)).then_some(RootKind::Custom)
}

fn has_client_exe(entries: &[Entry]) -> bool {
    let files: Vec<String> = entries
        .iter()
        .filter(|e| e.is_file)
        .map(|e| e.name.clone())
        .collect();
    pick_exe(&files).is_some()
}

/// One scan across several bases, sharing the visited set and the budget.
pub(crate) struct Scanner {
    limits: ScanLimits,
    /// Each directory visited, with the most depth any visit had left below it (`MAX`
    /// for a root, never descended into): reached again with more left, as another
    /// base may reach it nearer its top, it is walked again.
    visited: HashMap<(u64, u64), usize>,
    /// Directories visited so far.
    pub count: usize,
    /// The base being walked when the budget ran out.
    pub truncated: Option<PathBuf>,
    /// Directories that could not be listed.
    pub diagnostics: Vec<String>,
}

impl Scanner {
    pub fn new(limits: ScanLimits) -> Self {
        Self {
            limits,
            visited: HashMap::new(),
            count: 0,
            truncated: None,
            diagnostics: Vec::new(),
        }
    }

    /// The WoW roots below `base`, in breadth-first, name-sorted order. A root is not
    /// descended into.
    pub fn walk(&mut self, base: &Path, kind: Base) -> Vec<(PathBuf, RootKind)> {
        let mut found = Vec::new();
        let mut queue = VecDeque::from([(base.to_path_buf(), 0usize)]);
        while let Some((dir, depth)) = queue.pop_front() {
            if self.truncated.is_some() {
                break;
            }
            let Ok(meta) = std::fs::metadata(&dir) else {
                continue;
            };
            let id = (meta.dev(), meta.ino());
            let left = self.limits.max_depth.saturating_sub(depth);
            if self.visited.get(&id).is_some_and(|&had| had >= left) {
                continue;
            }
            if self.count >= self.limits.max_dirs {
                self.truncated = Some(base.to_path_buf());
                break;
            }
            self.visited.insert(id, left);
            self.count += 1;
            let entries = match list(&dir) {
                Ok(entries) => entries,
                Err(err) => {
                    self.diagnostics
                        .push(format!("cannot list {}: {err}", dir.display()));
                    continue;
                }
            };
            // A flavor folder reached directly (e.g. an extra root) looks like a custom
            // root; its parent is the Battle.net root. Reached through a link, the
            // parent is the target's.
            let real = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
            if has_client_exe(&entries)
                && let Some(parent) = [dir.parent(), real.parent()]
                    .into_iter()
                    .flatten()
                    .find(|p| p.join(".build.info").is_file())
            {
                self.visited.insert(id, usize::MAX);
                found.push((parent.to_path_buf(), RootKind::Battlenet));
                continue;
            }
            if let Some(root) = classify(&entries) {
                self.visited.insert(id, usize::MAX);
                found.push((dir, root));
                continue;
            }
            if depth >= self.limits.max_depth {
                continue;
            }
            for entry in entries.iter().filter(|e| e.is_dir) {
                if depth == 0
                    && kind == Base::DriveC
                    && self
                        .limits
                        .skip_dirs
                        .iter()
                        .any(|skip| entry.name.eq_ignore_ascii_case(skip))
                {
                    continue;
                }
                let child = dir.join(&entry.name);
                if kind == Base::Games && is_prefix(&child) {
                    continue;
                }
                queue.push_back((child, depth + 1));
            }
        }
        found
    }
}

/// A Wine prefix: a directory holding `drive_c/`.
pub(crate) fn is_prefix(dir: &Path) -> bool {
    dir.join("drive_c").is_dir()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn entry(name: &str, is_dir: bool) -> Entry {
        Entry {
            name: name.to_owned(),
            is_dir,
            is_file: !is_dir,
        }
    }

    #[test]
    fn classification_from_a_listing() {
        let bnet = [entry(".build.info", false), entry("_retail_", true)];
        assert_eq!(classify(&bnet), Some(RootKind::Battlenet));
        let custom = [
            entry("interface", true),
            entry("WOW.EXE", false),
            entry("Data", true),
        ];
        assert_eq!(classify(&custom), Some(RootKind::Custom));
        let crash = [entry("Interface", true), entry("WowError.exe", false)];
        assert_eq!(classify(&crash), None);
        let no_interface = [entry("Wow.exe", false)];
        assert_eq!(classify(&no_interface), None);
        let interface_file = [entry("Interface", false), entry("Wow.exe", false)];
        assert_eq!(classify(&interface_file), None);
    }

    #[test]
    fn depth_skips_and_loops() {
        let dir = tempfile::tempdir().unwrap();
        let drive_c = dir.path().join("drive_c");
        let custom = |at: &Path| {
            fs::create_dir_all(at.join("Interface/AddOns")).unwrap();
            fs::write(at.join("Wow.exe"), "").unwrap();
        };
        custom(&drive_c.join("a/b/c/d")); // depth 4: found
        custom(&drive_c.join("a/b/c/d2/e")); // depth 5: not visited
        custom(&drive_c.join("WINDOWS/wow")); // skipped, any case
        custom(&drive_c.join("Users/x/wow"));
        custom(&drive_c.join("programdata/wow"));
        std::os::unix::fs::symlink("..", drive_c.join("a/loop")).unwrap();

        let mut scan = Scanner::new(ScanLimits::default());
        let found = scan.walk(&drive_c, Base::DriveC);
        assert_eq!(found, [(drive_c.join("a/b/c/d"), RootKind::Custom)]);
        assert!(scan.truncated.is_none());

        let limits = ScanLimits {
            max_dirs: 3,
            ..ScanLimits::default()
        };
        let mut scan = Scanner::new(limits);
        assert!(scan.walk(&drive_c, Base::DriveC).is_empty());
        assert_eq!(
            (scan.count, scan.truncated.as_deref()),
            (3, Some(drive_c.as_path()))
        );

        // A directory one base reached at its depth limit is walked again from a base
        // that reaches it nearer its top.
        let mut scan = Scanner::new(ScanLimits::default());
        assert_eq!(
            scan.walk(&drive_c, Base::DriveC),
            [(drive_c.join("a/b/c/d"), RootKind::Custom)]
        );
        assert_eq!(
            scan.walk(&drive_c.join("a/b/c/d2"), Base::Root),
            [(drive_c.join("a/b/c/d2/e"), RootKind::Custom)]
        );
        // A root found once is never found again.
        assert!(scan.walk(&drive_c.join("a/b"), Base::Root).is_empty());
    }

    /// A flavor folder reached through a link belongs to the Battle.net root its target
    /// is in, not a custom root of its own.
    #[test]
    fn a_linked_flavor_folder_is_its_battlenet_roots() {
        let dir = tempfile::tempdir().unwrap();
        let bnet = dir.path().join("World of Warcraft");
        fs::create_dir_all(bnet.join("_retail_/Interface/AddOns")).unwrap();
        fs::write(bnet.join("_retail_/Wow.exe"), "").unwrap();
        fs::write(bnet.join(".build.info"), "").unwrap();
        let games = dir.path().join("Games");
        fs::create_dir_all(&games).unwrap();
        std::os::unix::fs::symlink(bnet.join("_retail_"), games.join("retail")).unwrap();
        let mut scan = Scanner::new(ScanLimits::default());
        assert_eq!(
            scan.walk(&games.join("retail"), Base::Root),
            [(bnet.clone(), RootKind::Battlenet)]
        );
    }
}
