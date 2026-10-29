//! Resources (M4-D62): what an operation reads or writes, and whether two operations
//! touch the same thing.
//!
//! A [`Resource`] is either a canonical path with its kind (a [`PathKind::File`], or a
//! [`PathKind::Subtree`] standing for a directory and everything under it), or a named
//! resource that is no path: a transaction's journal by its id, a target's record by its
//! key, or one backup input of a doctor run. A path is canonical when it is absolute,
//! normalized, and its parents are resolved without following its final component, as
//! `mutate()` resolves the paths it writes, so a resource compares with what fsx logs.
//! [`Resource::canonical`] resolves a path against the filesystem (reading, never
//! writing). [`Resource::from_canonical`] and [`Resource::parse`] take back a resource
//! recorded earlier without resolving it again: the symlinks it was resolved through may
//! have changed since.
//!
//! **Overlap.** Two path resources overlap when one is the other or an ancestor of it,
//! compared by whole components (`AddOns/X` and `AddOns/XY` are siblings). Two named
//! resources overlap when they are the same. Two refinements decide the rest:
//!
//! - The kind never narrows overlap: a `File` ancestor still overlaps a descendant path.
//!   A file has no descendants, so this can only refuse two operations that would
//!   collide anyway, and a caller that declared the wrong kind is never under-protected.
//!   The kind is kept for display, gc's pins and the text form.
//! - A named resource never overlaps a path. A journal file a primitive renames is a
//!   path resource, while `Journal(tx)` stands for the transaction as a whole; a
//!   consumer that needs both declares both.
//!
//! **Sets.** A [`ResourceSet`] is what one operation reads and what it writes, and a
//! rename writes both its ends. Two sets conflict when one's writes overlap the other's
//! reads or writes, never read against read. A set holds one entry per resource, with
//! the widest kind and the strongest access declared for it: a resource written is never
//! also listed as read, since a write has every conflict a read has.
//!
//! **Text.** [`Resource::text`] and [`Resource::parse`] give each resource one line: a
//! path as itself (a subtree with a trailing `/`, the root as `/`), and `journal:<tx>`,
//! `record:<key>` or `backup:<run dir>/<seq>`. A path starts with `/` and a named
//! resource with a lowercase word and `:`, so the two never collide. A path that is not
//! UTF-8 has no text form: written lossily, a fence would protect another path.
//!
//! The consumers: the repair transaction declares its primitives' sets and records their
//! union, `doctor fix` refuses fixes whose sets conflict, fences refuse only the
//! mutations that overlap a stuck repair's resources, undo of a repair refuses when a
//! later change overlaps what it touched, and `doctor gc` pins with them. Nothing here
//! is serialized: each consumer owns its wire form, and maps a [`ResourceError`] to the
//! error its caller deserves.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::fsx::canonical_lenient;
use crate::model::{TargetKey, TxId};

/// What a path resource stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathKind {
    /// The entry at the path itself.
    File,
    /// The directory at the path and everything under it.
    Subtree,
}

/// One thing an operation reads or writes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Resource {
    /// A canonical path.
    Path { path: PathBuf, kind: PathKind },
    /// A transaction as a whole, by its journal's id.
    Journal(TxId),
    /// A target's record, by its key.
    Record(TargetKey),
    /// The backup input `<run_dir>/backups/<seq>` of a doctor run directory.
    Backup { run_dir: String, seq: u32 },
}

/// Why a path or a text is not a resource.
#[derive(Debug, thiserror::Error)]
pub enum ResourceError {
    #[error("{} is not absolute", .0.display())]
    Relative(PathBuf),
    #[error("{} cannot be resolved: a `..` follows a component that does not exist", .0.display())]
    Unresolvable(PathBuf),
    #[error("{} is not in canonical form", .0.display())]
    NotCanonical(PathBuf),
    #[error("{} is not valid UTF-8, so it has no text form", .0.display())]
    NotUtf8(PathBuf),
    #[error("{0} is not a resource")]
    Malformed(String),
}

/// How an operation touches a resource. A write is the stronger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Access {
    Read,
    Write,
}

/// Two resources of two sets that overlap, at least one of them written: `ours` from the
/// set asked, `theirs` from the other.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Conflict {
    pub ours: Resource,
    pub ours_access: Access,
    pub theirs: Resource,
    pub theirs_access: Access,
}

impl Resource {
    /// The resource at `path`, resolved as `mutate()` resolves a target: its parent
    /// through the filesystem (a symlinked parent followed, a `..` after it going to the
    /// link target's parent, as the kernel does, and a missing tail appended), its final
    /// component joined without being followed, so a symlink stays the link. A path whose
    /// last component is `..` names a directory, and is resolved whole. The filesystem
    /// root is always a [`PathKind::Subtree`].
    pub fn canonical(path: &Path, kind: PathKind) -> Result<Self, ResourceError> {
        if !path.is_absolute() {
            return Err(ResourceError::Relative(path.to_path_buf()));
        }
        let resolved = match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => canonical_lenient(parent).map(|p| p.join(name)),
            _ => canonical_lenient(path),
        }
        .ok_or_else(|| ResourceError::Unresolvable(path.to_path_buf()))?;
        Ok(Self::path_of(resolved, kind))
    }

    /// A path recorded in canonical form, taken back without touching the filesystem:
    /// absolute, every component after the root a plain name, and written exactly as its
    /// components rejoin (no trailing slash, no doubled separator).
    pub fn from_canonical(path: &Path, kind: PathKind) -> Result<Self, ResourceError> {
        if !path.is_absolute() {
            return Err(ResourceError::Relative(path.to_path_buf()));
        }
        let plain = path
            .components()
            .skip(1)
            .all(|c| matches!(c, Component::Normal(_)));
        let rejoined: PathBuf = path.components().collect();
        if !plain || rejoined.as_os_str() != path.as_os_str() {
            return Err(ResourceError::NotCanonical(path.to_path_buf()));
        }
        Ok(Self::path_of(path.to_path_buf(), kind))
    }

    fn path_of(path: PathBuf, kind: PathKind) -> Self {
        let kind = if path.parent().is_none() {
            PathKind::Subtree
        } else {
            kind
        };
        Self::Path { path, kind }
    }

    pub const fn journal(tx: TxId) -> Self {
        Self::Journal(tx)
    }

    pub const fn record(key: TargetKey) -> Self {
        Self::Record(key)
    }

    /// One backup input of a doctor run. `run_dir` is the run directory's name, so it is
    /// refused empty, as `.` or `..`, or holding a `/` or a NUL.
    pub fn backup(run_dir: &str, seq: u32) -> Result<Self, ResourceError> {
        if run_dir.is_empty() || run_dir == "." || run_dir == ".." || run_dir.contains(['/', '\0'])
        {
            return Err(ResourceError::Malformed(format!("backup:{run_dir}/{seq}")));
        }
        Ok(Self::Backup {
            run_dir: run_dir.to_owned(),
            seq,
        })
    }

    /// The path of a path resource.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Path { path, .. } => Some(path),
            _ => None,
        }
    }

    /// The kind of a path resource.
    pub const fn kind(&self) -> Option<PathKind> {
        match self {
            Self::Path { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// Whether the two touch the same thing: paths when one is the other or an ancestor
    /// of it, whatever their kinds; named resources when they are equal; a path and a
    /// named resource never.
    pub fn overlaps(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Path { path, .. }, Self::Path { path: theirs, .. }) => nested(path, theirs),
            (Self::Path { .. }, _) | (_, Self::Path { .. }) => false,
            _ => self == other,
        }
    }

    /// [`Resource::overlaps`] against a path already in canonical form (an
    /// `ActionRecord`'s path, or a target `mutate()` normalized), without touching the
    /// filesystem. Never true for a named resource.
    pub fn overlaps_path(&self, canonical: &Path) -> bool {
        self.path().is_some_and(|path| nested(path, canonical))
    }

    /// The text form: refused for a path that is not UTF-8.
    pub fn text(&self) -> Result<String, ResourceError> {
        match self {
            Self::Path { path, kind } => {
                let text = path
                    .to_str()
                    .ok_or_else(|| ResourceError::NotUtf8(path.clone()))?;
                Ok(path_text(text, *kind))
            }
            _ => Ok(self.to_string()),
        }
    }

    /// A resource back from its text form. A path must be canonical as written, never
    /// resolved again; anything else is [`ResourceError::Malformed`].
    pub fn parse(text: &str) -> Result<Self, ResourceError> {
        let malformed = || ResourceError::Malformed(text.to_owned());
        if text.starts_with('/') {
            if text == "/" {
                return Ok(Self::path_of(PathBuf::from("/"), PathKind::Subtree));
            }
            return match text.strip_suffix('/') {
                Some(dir) => Self::from_canonical(Path::new(dir), PathKind::Subtree),
                None => Self::from_canonical(Path::new(text), PathKind::File),
            };
        }
        let (scheme, rest) = text.split_once(':').ok_or_else(malformed)?;
        match scheme {
            "journal" => rest.parse().map(Self::Journal).map_err(|_| malformed()),
            "record" => rest.parse().map(Self::Record).map_err(|_| malformed()),
            "backup" => {
                let (run_dir, seq) = rest.rsplit_once('/').ok_or_else(malformed)?;
                // Decimal digits written as `u32` writes them: no sign, no leading zero.
                let seq = seq
                    .parse::<u32>()
                    .ok()
                    .filter(|n| n.to_string() == seq)
                    .ok_or_else(malformed)?;
                Self::backup(run_dir, seq).map_err(|_| malformed())
            }
            _ => Err(malformed()),
        }
    }
}

/// Whether one path is the other or an ancestor of it, by whole components.
fn nested(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn path_text(path: &str, kind: PathKind) -> String {
    match kind {
        PathKind::Subtree if path != "/" => format!("{path}/"),
        _ => path.to_owned(),
    }
}

impl fmt::Display for Resource {
    /// The text form, lossy for a path that is not UTF-8: for messages only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path { path, kind } => f.write_str(&path_text(&path.to_string_lossy(), *kind)),
            Self::Journal(tx) => write!(f, "journal:{tx}"),
            Self::Record(key) => write!(f, "record:{key}"),
            Self::Backup { run_dir, seq } => write!(f, "backup:{run_dir}/{seq}"),
        }
    }
}

/// What one operation reads and writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceSet {
    reads: BTreeSet<Resource>,
    writes: BTreeSet<Resource>,
}

impl ResourceSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read(&mut self, resource: Resource) -> &mut Self {
        self.insert(Access::Read, resource);
        self
    }

    pub fn write(&mut self, resource: Resource) -> &mut Self {
        self.insert(Access::Write, resource);
        self
    }

    /// A rename writes both its ends.
    pub fn rename(&mut self, from: Resource, to: Resource) -> &mut Self {
        self.write(from).write(to)
    }

    /// The union with `other`.
    pub fn extend(&mut self, other: &Self) -> &mut Self {
        for resource in &other.reads {
            self.insert(Access::Read, resource.clone());
        }
        for resource in &other.writes {
            self.insert(Access::Write, resource.clone());
        }
        self
    }

    pub const fn reads(&self) -> &BTreeSet<Resource> {
        &self.reads
    }

    pub const fn writes(&self) -> &BTreeSet<Resource> {
        &self.writes
    }

    pub fn is_empty(&self) -> bool {
        self.reads.is_empty() && self.writes.is_empty()
    }

    /// Adds `resource`, merged with what the set holds for the same resource: the widest
    /// kind at a path (a subtree over a file) and the strongest access (a write over a
    /// read), so each resource is held once.
    fn insert(&mut self, access: Access, resource: Resource) {
        let mut access = access;
        let resource = match resource {
            Resource::Path { path, kind } => {
                let mut kind = kind;
                for held in [PathKind::File, PathKind::Subtree] {
                    let probe = Resource::Path {
                        path: path.clone(),
                        kind: held,
                    };
                    if self.reads.remove(&probe) {
                        kind = kind.max(held);
                    }
                    if self.writes.remove(&probe) {
                        kind = kind.max(held);
                        access = Access::Write;
                    }
                }
                Resource::Path { path, kind }
            }
            named => {
                self.reads.remove(&named);
                if self.writes.remove(&named) {
                    access = Access::Write;
                }
                named
            }
        };
        match access {
            Access::Read => self.reads.insert(resource),
            Access::Write => self.writes.insert(resource),
        };
    }

    /// Every pair of an overlapping resource of this set and one of `other`, at least one
    /// of them written, sorted and without duplicates whatever order they were declared
    /// in. Empty exactly when `other.conflicts(self)` is, with each pair's roles swapped.
    pub fn conflicts(&self, other: &Self) -> Vec<Conflict> {
        self.pairs(other)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Whether any pair conflicts, stopping at the first.
    pub fn conflicts_with(&self, other: &Self) -> bool {
        self.pairs(other).next().is_some()
    }

    /// This set's members that overlap `resource`, in set order, with how each is
    /// touched: to name a fenced resource or a pin.
    pub fn overlapping(&self, resource: &Resource) -> Vec<(&Resource, Access)> {
        let mut found: Vec<(&Resource, Access)> = self
            .members()
            .filter(|(member, _)| member.overlaps(resource))
            .collect();
        found.sort();
        found
    }

    fn members(&self) -> impl Iterator<Item = (&Resource, Access)> {
        let reads = self.reads.iter().map(|r| (r, Access::Read));
        let writes = self.writes.iter().map(|r| (r, Access::Write));
        reads.chain(writes)
    }

    fn pairs<'a>(&'a self, other: &'a Self) -> impl Iterator<Item = Conflict> + 'a {
        self.members().flat_map(move |(ours, ours_access)| {
            other
                .members()
                .filter(move |(theirs, theirs_access)| {
                    (ours_access == Access::Write || *theirs_access == Access::Write)
                        && ours.overlaps(theirs)
                })
                .map(move |(theirs, theirs_access)| Conflict {
                    ours: ours.clone(),
                    ours_access,
                    theirs: theirs.clone(),
                    theirs_access,
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    use proptest::prelude::*;
    use ulid::Ulid;

    use super::*;

    /// `<root>/a/b`, `<root>/a/x`, `<root>/t/u`, `<root>/link → t/u` and
    /// `<root>/dangling → nowhere`, under a canonical temporary root.
    fn tree() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        for sub in ["a/b", "a/x", "t/u"] {
            fs::create_dir_all(root.join(sub)).unwrap();
        }
        symlink("t/u", root.join("link")).unwrap();
        symlink("nowhere", root.join("dangling")).unwrap();
        (dir, root)
    }

    fn file(path: impl Into<PathBuf>) -> Resource {
        Resource::Path {
            path: path.into(),
            kind: PathKind::File,
        }
    }

    fn subtree(path: impl Into<PathBuf>) -> Resource {
        Resource::Path {
            path: path.into(),
            kind: PathKind::Subtree,
        }
    }

    fn tx(n: u64) -> TxId {
        TxId::from_ulid(Ulid::from_parts(n, u128::from(n)))
    }

    fn key(n: u64) -> TargetKey {
        TargetKey::from_ulid(Ulid::from_parts(n, u128::from(n)))
    }

    fn backup(run_dir: &str, seq: u32) -> Resource {
        Resource::backup(run_dir, seq).unwrap()
    }

    /// `(case, input, kind asked, expected)`, `<root>` standing for the tree's root.
    fn canonical_cases() -> Vec<(&'static str, &'static str, PathKind, Resource)> {
        let f = |p: &str| file(p);
        vec![
            ("plain", "<root>/a/b", PathKind::File, f("<root>/a/b")),
            (
                "trailing_slash_dropped",
                "<root>/a/b/",
                PathKind::File,
                f("<root>/a/b"),
            ),
            (
                "dot_dropped",
                "<root>/a/./b",
                PathKind::File,
                f("<root>/a/b"),
            ),
            (
                "dotdot_in_existing_prefix",
                "<root>/a/x/../b",
                PathKind::File,
                f("<root>/a/b"),
            ),
            (
                "dotdot_after_symlinked_parent_goes_to_the_targets_parent",
                "<root>/link/../b",
                PathKind::File,
                f("<root>/t/b"),
            ),
            (
                "symlinked_parent_resolved",
                "<root>/link/f",
                PathKind::File,
                f("<root>/t/u/f"),
            ),
            (
                "final_symlink_not_followed",
                "<root>/link",
                PathKind::File,
                f("<root>/link"),
            ),
            (
                "final_dangling_symlink_kept",
                "<root>/dangling",
                PathKind::File,
                f("<root>/dangling"),
            ),
            (
                "missing_tail_appended",
                "<root>/a/new/deeper",
                PathKind::Subtree,
                subtree("<root>/a/new/deeper"),
            ),
            (
                "missing_tail_under_symlinked_parent",
                "<root>/link/new/x",
                PathKind::File,
                f("<root>/t/u/new/x"),
            ),
            (
                "final_dotdot_resolves_the_whole_path",
                "<root>/a/b/..",
                PathKind::Subtree,
                subtree("<root>/a"),
            ),
            ("root_is_a_subtree", "/", PathKind::File, subtree("/")),
        ]
    }

    fn rooted(root: &Path, text: &str) -> PathBuf {
        PathBuf::from(text.replace("<root>", &root.to_string_lossy()))
    }

    fn rooted_resource(root: &Path, resource: &Resource) -> Resource {
        match resource {
            Resource::Path { path, kind } => Resource::Path {
                path: rooted(root, &path.to_string_lossy()),
                kind: *kind,
            },
            named => named.clone(),
        }
    }

    #[test]
    fn canonicalizes_paths() {
        let (_dir, root) = tree();
        for (case, input, kind, want) in canonical_cases() {
            let input = rooted(&root, input);
            let want = rooted_resource(&root, &want);
            let got = Resource::canonical(&input, kind);
            assert!(
                matches!(&got, Ok(r) if *r == want),
                "{case}: canonical({input:?}) = {got:?}, expected {want:?}"
            );
        }
    }

    #[test]
    fn refuses_unresolvable_paths() {
        let (_dir, root) = tree();
        let got = Resource::canonical(Path::new("a/b"), PathKind::File);
        assert!(
            matches!(got, Err(ResourceError::Relative(_))),
            "relative: canonical(\"a/b\") = {got:?}, expected Relative"
        );
        let input = rooted(&root, "<root>/missing/../b");
        let got = Resource::canonical(&input, PathKind::File);
        assert!(
            matches!(got, Err(ResourceError::Unresolvable(_))),
            "dotdot_after_missing_component: canonical({input:?}) = {got:?}, expected Unresolvable"
        );
    }

    #[test]
    fn canonicalization_is_idempotent() {
        let (_dir, root) = tree();
        for (case, input, kind, _) in canonical_cases() {
            let once = Resource::canonical(&rooted(&root, input), kind).unwrap();
            let twice = Resource::canonical(once.path().unwrap(), kind).unwrap();
            assert_eq!(twice, once, "{case}: canonical(canonical({input:?}))");
        }
    }

    #[test]
    fn from_canonical_accepts_only_canonical_paths() {
        let (_dir, root) = tree();
        let accepted = rooted(&root, "<root>/a/b");
        let got = Resource::from_canonical(&accepted, PathKind::File);
        assert!(
            matches!(&got, Ok(r) if *r == file(&accepted)),
            "from_canonical({accepted:?}) = {got:?}"
        );
        let root_text = root.to_string_lossy();
        for (case, input) in [
            ("relative", "a/b".to_owned()),
            ("dot", format!("{root_text}/a/./b")),
            ("dotdot", format!("{root_text}/a/../b")),
            ("trailing_slash", format!("{root_text}/a/b/")),
            ("doubled_separator", format!("{root_text}//a")),
            ("empty", String::new()),
        ] {
            let got = Resource::from_canonical(Path::new(&input), PathKind::File);
            assert!(
                matches!(
                    got,
                    Err(ResourceError::NotCanonical(_) | ResourceError::Relative(_))
                ),
                "{case}: from_canonical({input:?}) = {got:?}, expected a refusal"
            );
        }
    }

    #[test]
    fn path_overlap() {
        let r = "/r";
        for (case, a, b, want) in [
            (
                "itself",
                file(format!("{r}/a")),
                file(format!("{r}/a")),
                true,
            ),
            (
                "ancestor_then_descendant",
                subtree(format!("{r}/a")),
                file(format!("{r}/a/b")),
                true,
            ),
            (
                "descendant_then_ancestor",
                file(format!("{r}/a/b")),
                subtree(format!("{r}/a")),
                true,
            ),
            (
                "siblings",
                file(format!("{r}/a/b")),
                file(format!("{r}/a/x")),
                false,
            ),
            (
                "string_prefix_sibling",
                subtree(format!("{r}/a/b")),
                file(format!("{r}/a/bc")),
                false,
            ),
            (
                "root_overlaps_everything",
                subtree("/"),
                file(format!("{r}/a/b")),
                true,
            ),
            (
                "file_kind_does_not_narrow",
                file(format!("{r}/a")),
                file(format!("{r}/a/b")),
                true,
            ),
            (
                "subtree_and_child_file",
                subtree(format!("{r}/a")),
                file(format!("{r}/a/b.toc")),
                true,
            ),
            (
                "unrelated_trees",
                subtree(format!("{r}/a")),
                subtree(format!("{r}/t")),
                false,
            ),
        ] {
            let got = a.overlaps(&b);
            assert_eq!(
                got, want,
                "{case}: {a} overlaps {b} = {got}, expected {want}"
            );
            let path = b.path().unwrap();
            let got = a.overlaps_path(path);
            assert_eq!(
                got, want,
                "{case}: {a} overlaps_path {path:?} = {got}, expected {want}"
            );
        }
    }

    #[test]
    fn named_overlap() {
        let path = file("/r/a");
        for (case, a, b, want) in [
            (
                "same_journal",
                Resource::journal(tx(1)),
                Resource::journal(tx(1)),
                true,
            ),
            (
                "other_journal",
                Resource::journal(tx(1)),
                Resource::journal(tx(2)),
                false,
            ),
            (
                "same_record",
                Resource::record(key(1)),
                Resource::record(key(1)),
                true,
            ),
            ("same_backup", backup("run", 1), backup("run", 1), true),
            (
                "same_run_other_seq",
                backup("run", 1),
                backup("run", 2),
                false,
            ),
            (
                "journal_and_record_with_the_same_ulid",
                Resource::journal(tx(1)),
                Resource::record(key(1)),
                false,
            ),
            (
                "named_and_path",
                Resource::journal(tx(1)),
                path.clone(),
                false,
            ),
            ("path_and_named", path.clone(), backup("run", 1), false),
        ] {
            let got = a.overlaps(&b);
            assert_eq!(
                got, want,
                "{case}: {a} overlaps {b} = {got}, expected {want}"
            );
        }
        let got = Resource::journal(tx(1)).overlaps_path(Path::new("/"));
        assert!(
            !got,
            "overlaps_path_of_a_named_resource: journal overlaps_path / = {got}"
        );
    }

    /// Paths of up to 4 components under `/r` from a small alphabet, both kinds, and the
    /// three named kinds over a few ids, so that equal and nested resources are common.
    fn any_resource() -> impl Strategy<Value = Resource> {
        let component = prop_oneof![Just("a"), Just("b"), Just("ab"), Just(".omaddons-x")];
        let path =
            (prop::collection::vec(component, 0..=4), any::<bool>()).prop_map(|(parts, wide)| {
                let mut path = PathBuf::from("/r");
                path.extend(parts);
                let kind = if wide {
                    PathKind::Subtree
                } else {
                    PathKind::File
                };
                Resource::Path { path, kind }
            });
        prop_oneof![
            4 => path,
            1 => (0u64..3).prop_map(|n| Resource::journal(tx(n))),
            1 => (0u64..3).prop_map(|n| Resource::record(key(n))),
            1 => (0u32..2).prop_map(|seq| backup("run", seq)),
        ]
    }

    /// The oracle: paths as component lists compared by prefix, named resources by
    /// equality, a path and a named resource never.
    fn oracle(a: &Resource, b: &Resource) -> bool {
        let components = |p: &Path| -> Vec<String> {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect()
        };
        match (a.path(), b.path()) {
            (Some(a), Some(b)) => {
                let (a, b) = (components(a), components(b));
                a.starts_with(&b) || b.starts_with(&a)
            }
            (None, None) => a == b,
            _ => false,
        }
    }

    fn any_set() -> impl Strategy<Value = ResourceSet> {
        prop::collection::vec((any::<bool>(), any_resource()), 0..6).prop_map(|members| {
            let mut set = ResourceSet::new();
            for (written, resource) in members {
                if written {
                    set.write(resource);
                } else {
                    set.read(resource);
                }
            }
            set
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn overlap_matches_its_oracle(a in any_resource(), b in any_resource()) {
            prop_assert!(a.overlaps(&a), "{} does not overlap itself", a);
            prop_assert_eq!(a.overlaps(&b), b.overlaps(&a), "{} and {} not symmetric", a, b);
            prop_assert_eq!(a.overlaps(&b), oracle(&a, &b), "{} overlaps {}", a, b);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn conflicts_are_symmetric(a in any_set(), b in any_set()) {
            let ours = a.conflicts(&b);
            let mut swapped: Vec<Conflict> = b
                .conflicts(&a)
                .into_iter()
                .map(|c| Conflict {
                    ours: c.theirs,
                    ours_access: c.theirs_access,
                    theirs: c.ours,
                    theirs_access: c.ours_access,
                })
                .collect();
            swapped.sort();
            prop_assert_eq!(&ours, &swapped, "{:?} against {:?}", a, b);
            prop_assert_eq!(a.conflicts_with(&b), !ours.is_empty());
        }
    }

    #[test]
    fn sets_normalize() {
        let p = "/r/a";
        let set = |build: &dyn Fn(&mut ResourceSet)| {
            let mut set = ResourceSet::new();
            build(&mut set);
            set
        };
        for (case, got, reads, writes) in [
            (
                "write_then_read_keeps_only_the_write",
                set(&|s| {
                    s.write(file(p)).read(file(p));
                }),
                vec![],
                vec![file(p)],
            ),
            (
                "read_then_write_keeps_only_the_write",
                set(&|s| {
                    s.read(file(p)).write(file(p));
                }),
                vec![],
                vec![file(p)],
            ),
            (
                "file_then_subtree_keeps_the_subtree",
                set(&|s| {
                    s.read(file(p)).read(subtree(p));
                }),
                vec![subtree(p)],
                vec![],
            ),
            (
                "subtree_then_file_keeps_the_subtree",
                set(&|s| {
                    s.write(subtree(p)).write(file(p));
                }),
                vec![],
                vec![subtree(p)],
            ),
            (
                "read_subtree_then_write_file_keeps_a_written_subtree",
                set(&|s| {
                    s.read(subtree(p)).write(file(p));
                }),
                vec![],
                vec![subtree(p)],
            ),
            (
                "named_write_then_read_keeps_only_the_write",
                set(&|s| {
                    s.write(Resource::journal(tx(1)))
                        .read(Resource::journal(tx(1)));
                }),
                vec![],
                vec![Resource::journal(tx(1))],
            ),
        ] {
            let want_reads: BTreeSet<Resource> = reads.into_iter().collect();
            let want_writes: BTreeSet<Resource> = writes.into_iter().collect();
            assert_eq!(got.reads(), &want_reads, "{case}: reads of {got:?}");
            assert_eq!(got.writes(), &want_writes, "{case}: writes of {got:?}");
        }
    }

    #[test]
    fn rename_contributes_both_endpoints() {
        let (from, to) = (subtree("/c/Interface/AddOns/X"), subtree("/q/X"));
        let mut set = ResourceSet::new();
        set.rename(from.clone(), to.clone());
        let want: BTreeSet<Resource> = [from, to].into_iter().collect();
        assert_eq!(set.writes(), &want, "rename: writes of {set:?}");
        assert!(set.reads().is_empty(), "rename: reads of {set:?}");
    }

    #[test]
    fn extend_is_the_normalized_union() {
        let mut a = ResourceSet::new();
        a.read(file("/c/a")).write(file("/c/b"));
        let mut b = ResourceSet::new();
        b.write(subtree("/c/a"))
            .read(file("/c/b"))
            .read(file("/c/z"));
        a.extend(&b);
        let reads: BTreeSet<Resource> = [file("/c/z")].into_iter().collect();
        let writes: BTreeSet<Resource> = [subtree("/c/a"), file("/c/b")].into_iter().collect();
        assert_eq!(a.reads(), &reads, "extend: reads of {a:?}");
        assert_eq!(a.writes(), &writes, "extend: writes of {a:?}");
        assert!(!a.is_empty() && ResourceSet::new().is_empty());
    }

    fn conflict(ours: Resource, oa: Access, theirs: Resource, ta: Access) -> Conflict {
        Conflict {
            ours,
            ours_access: oa,
            theirs,
            theirs_access: ta,
        }
    }

    #[test]
    fn conflicts() {
        use Access::{Read, Write};
        let set = |members: &[(Access, Resource)]| {
            let mut set = ResourceSet::new();
            for (access, resource) in members {
                match access {
                    Read => set.read(resource.clone()),
                    Write => set.write(resource.clone()),
                };
            }
            set
        };
        let x = subtree("/c/AddOns/X");
        let toc = file("/c/AddOns/X/X.toc");
        let y = subtree("/c/AddOns/Y");
        let j = Resource::journal(tx(1));
        let mut rename_to_y = ResourceSet::new();
        rename_to_y.rename(subtree("/q/Y"), y.clone());
        let mut rename_out_of_x = ResourceSet::new();
        rename_out_of_x.rename(subtree("/c/AddOns/X/Y"), subtree("/q/Y"));
        for (case, ours, theirs, want) in [
            (
                "read_read",
                set(&[(Read, x.clone())]),
                set(&[(Read, toc.clone())]),
                vec![],
            ),
            (
                "write_read",
                set(&[(Write, x.clone())]),
                set(&[(Read, toc.clone())]),
                vec![conflict(x.clone(), Write, toc.clone(), Read)],
            ),
            (
                "read_write",
                set(&[(Read, x.clone())]),
                set(&[(Write, toc.clone())]),
                vec![conflict(x.clone(), Read, toc.clone(), Write)],
            ),
            (
                "quarantine_of_a_directory_against_a_write_into_its_child",
                set(&[(Write, x.clone())]),
                set(&[(Write, toc.clone())]),
                vec![conflict(x.clone(), Write, toc.clone(), Write)],
            ),
            (
                "rename_onto_another_fixs_path",
                rename_to_y.clone(),
                set(&[(Write, y.clone())]),
                vec![conflict(y.clone(), Write, y.clone(), Write)],
            ),
            (
                "rename_onto_a_path_another_fix_reads",
                rename_to_y.clone(),
                set(&[(Read, y.clone())]),
                vec![conflict(y.clone(), Write, y.clone(), Read)],
            ),
            (
                "rename_onto_a_directory_whose_child_another_fix_writes",
                rename_to_y.clone(),
                set(&[(Write, file("/c/AddOns/Y/Y.toc"))]),
                vec![conflict(y.clone(), Write, file("/c/AddOns/Y/Y.toc"), Write)],
            ),
            (
                "rename_out_of_a_quarantined_directory",
                rename_out_of_x,
                set(&[(Write, x.clone())]),
                vec![conflict(subtree("/c/AddOns/X/Y"), Write, x.clone(), Write)],
            ),
            (
                "sibling_writes",
                set(&[(Write, x.clone())]),
                set(&[(Write, y.clone())]),
                vec![],
            ),
            (
                "same_journal_write_and_read",
                set(&[(Write, j.clone())]),
                set(&[(Read, j.clone())]),
                vec![conflict(j.clone(), Write, j.clone(), Read)],
            ),
            (
                "other_journals",
                set(&[(Write, j.clone())]),
                set(&[(Write, Resource::journal(tx(2)))]),
                vec![],
            ),
        ] {
            let got = ours.conflicts(&theirs);
            assert_eq!(got, want, "{case}: {ours:?} against {theirs:?}");
            assert_eq!(
                ours.conflicts_with(&theirs),
                !want.is_empty(),
                "{case}: conflicts_with {ours:?} against {theirs:?}"
            );
        }
    }

    #[test]
    fn conflicts_are_sorted_and_deduplicated() {
        let members = [
            (Access::Write, subtree("/c/AddOns/X")),
            (Access::Read, file("/c/AddOns/Y/Y.toc")),
            (Access::Write, file("/c/AddOns/X/X.toc")),
        ];
        let theirs = {
            let mut set = ResourceSet::new();
            set.write(subtree("/c/AddOns"))
                .read(file("/c/AddOns/X/X.toc"));
            set
        };
        let build = |order: [usize; 3]| {
            let mut set = ResourceSet::new();
            for i in order {
                let (access, resource) = &members[i];
                match access {
                    Access::Read => set.read(resource.clone()),
                    Access::Write => set.write(resource.clone()),
                };
            }
            set.conflicts(&theirs)
        };
        let first = build([0, 1, 2]);
        for order in [[2, 1, 0], [1, 2, 0]] {
            assert_eq!(build(order), first, "insertion order {order:?}");
        }
        let mut sorted = first.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(first, sorted, "conflicts sorted and deduplicated");
        assert_eq!(first.len(), 5, "{first:?}");
    }

    #[test]
    fn overlapping_names_the_members_in_set_order() {
        let mut set = ResourceSet::new();
        set.write(file("/c/AddOns/X/X.toc"))
            .read(subtree("/c/AddOns"))
            .read(file("/c/other"))
            .write(Resource::journal(tx(1)));
        let got = set.overlapping(&subtree("/c/AddOns/X"));
        let (addons, toc) = (subtree("/c/AddOns"), file("/c/AddOns/X/X.toc"));
        let want = vec![(&addons, Access::Read), (&toc, Access::Write)];
        assert_eq!(got, want, "overlapping /c/AddOns/X/ in {set:?}");
    }

    #[test]
    fn text_round_trips() {
        for (case, resource, text) in [
            (
                "file",
                file("/s/journal/x.repair.jsonl"),
                "/s/journal/x.repair.jsonl",
            ),
            (
                "subtree",
                subtree("/c/Interface/AddOns/X"),
                "/c/Interface/AddOns/X/",
            ),
            ("root", subtree("/"), "/"),
            (
                "journal",
                Resource::journal(tx(1)),
                "journal:00000000010000000000000001",
            ),
            (
                "record",
                Resource::record(key(2)),
                "record:00000000020000000000000002",
            ),
            (
                "backup",
                backup("20261029T120000Z__01M6Q2VA", 3),
                "backup:20261029T120000Z__01M6Q2VA/3",
            ),
        ] {
            let got = resource.text().unwrap();
            assert_eq!(got, text, "{case}: text of {resource:?}");
            assert_eq!(
                resource.to_string(),
                text,
                "{case}: Display of {resource:?}"
            );
            let back = Resource::parse(&got);
            assert!(
                matches!(&back, Ok(r) if *r == resource),
                "{case}: parse({got:?}) = {back:?}, expected {resource:?}"
            );
        }
    }

    #[test]
    fn parse_refuses() {
        for (case, input) in [
            ("relative", "a/b"),
            ("bad_ulid", "journal:not-a-ulid"),
            ("backup_without_seq", "backup:run"),
            ("slash_in_run_dir", "backup:a/b/1"),
            ("dotdot_run_dir", "backup:../1"),
            ("signed_seq", "backup:run/+1"),
            ("leading_zero_seq", "backup:run/01"),
            ("empty_record", "record:"),
            ("unknown_scheme", "unknown:x"),
            ("not_canonical", "/r/a/./b"),
            ("dotdot_path", "/r/a/../b/"),
            ("doubled_trailing_slash", "/r/a//"),
            ("empty", ""),
        ] {
            let got = Resource::parse(input);
            assert!(
                matches!(
                    got,
                    Err(ResourceError::Malformed(_) | ResourceError::NotCanonical(_))
                ),
                "{case}: parse({input:?}) = {got:?}, expected Malformed or NotCanonical"
            );
        }
    }

    #[test]
    fn backup_refuses_a_run_dir_that_is_no_name() {
        for run_dir in ["", ".", "..", "a/b", "a\0b"] {
            let got = Resource::backup(run_dir, 1);
            assert!(
                matches!(got, Err(ResourceError::Malformed(_))),
                "backup({run_dir:?}, 1) = {got:?}, expected Malformed"
            );
        }
    }

    #[test]
    fn text_refuses_non_utf8_paths() {
        let path = Path::new("/r").join(OsStr::from_bytes(&[0x66, 0xff]));
        let resource = file(&path);
        let got = resource.text();
        assert!(
            matches!(got, Err(ResourceError::NotUtf8(_))),
            "text of {path:?} = {got:?}, expected NotUtf8"
        );
        assert_eq!(resource.to_string(), "/r/f\u{fffd}", "the lossy Display");
    }
}
