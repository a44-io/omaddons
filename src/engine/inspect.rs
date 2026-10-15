//! Archive inspection (vision §5.5 step 3): what a downloaded zip, or a local directory,
//! would install, decided before a single byte reaches the client.
//!
//! - **Hostile shapes are refused** (`unsafe_archive`, exit 4): absolute paths, drive
//!   letters, `..`, control characters, symlinks, devices, FIFOs and sockets, and names
//!   that collide once ASCII case is folded (Wine resolves names case-insensitively).
//! - **Limits** hold on the bytes actually decompressed, not on what headers claim:
//!   entry count and total size (`archive_too_large`), and the compression ratio past
//!   [`ARCHIVE_RATIO_FLOOR`] (`unsafe_archive`: a 200:1 zip is crafted).
//! - **Addon roots** are directories holding a TOC named after them (any known suffix,
//!   `-` or `_`), or `<Name>-main`, `-master` or `-<version>` holding `<Name>.toc`;
//!   single wrapper directories are descended. No root is `bad_archive` (exit 1).
//!
//! Inspection reads and hashes; it never writes. Each folder becomes a
//! [`FolderView`], so [`verify`] applies the client's own TOC load rules to the
//! archive's contents (D5): upstream metadata is a hint, the TOCs decide.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zip::CompressionMethod;

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::limits::{
    ARCHIVE_MAX_ENTRIES, ARCHIVE_MAX_RATIO, ARCHIVE_MAX_UNCOMPRESSED, ARCHIVE_RATIO_FLOOR,
    EffectiveLimits,
};
use crate::model::{FolderName, Target};
use crate::rules::RulesTable;
use crate::toc::{
    FolderView, Verdict, VerdictReport, WrongFlavorReason, judge, loaded_toc, read_bounded,
};

/// The archive limits in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    pub max_entries: u64,
    pub max_uncompressed: u64,
    pub max_ratio: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_entries: ARCHIVE_MAX_ENTRIES,
            max_uncompressed: ARCHIVE_MAX_UNCOMPRESSED,
            max_ratio: ARCHIVE_MAX_RATIO,
        }
    }
}

impl From<&EffectiveLimits> for ArchiveLimits {
    fn from(limits: &EffectiveLimits) -> Self {
        Self {
            max_entries: limits.archive_max_entries,
            max_uncompressed: limits.archive_max_uncompressed,
            max_ratio: limits.archive_max_ratio,
        }
    }
}

/// What an archive installs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Inspected {
    /// The zip's SHA-256; `None` for a directory.
    pub blob_sha256: Option<String>,
    pub folders: Vec<InspectedFolder>,
}

/// One addon folder in an archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct InspectedFolder {
    /// The name it installs as.
    pub name: FolderName,
    /// Its directory in the archive, `/`-separated (`Foo-main`, `Repo-main/A`).
    pub archive_prefix: String,
    pub files: Vec<InspectedFile>,
    /// Regular files directly inside the folder.
    #[serde(skip)]
    root_files: Vec<String>,
    /// The TOCs among them, each line kept to `TOC_LINE_MAX + 1` bytes.
    #[serde(skip)]
    tocs: BTreeMap<String, Vec<u8>>,
}

/// One file of a folder, measured from its decompressed bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct InspectedFile {
    /// Relative to the folder, `/`-separated.
    pub rel_path: String,
    pub size: u64,
    pub sha256: String,
    /// Where the bytes are, for extraction.
    #[serde(skip)]
    pub source: FileSource,
}

/// Where a file's bytes are read from again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSource {
    /// The entry index in the zip.
    ZipEntry(usize),
    Disk(PathBuf),
}

impl InspectedFolder {
    /// The folder as the TOC load rules see it.
    pub fn view(&self) -> ArchiveFolderView<'_> {
        ArchiveFolderView(self)
    }

    /// The TOC files directly inside the folder.
    pub fn toc_files(&self) -> Vec<String> {
        self.tocs.keys().cloned().collect()
    }
}

/// [`FolderView`] over an inspected folder; TOC bytes were captured during inspection.
pub struct ArchiveFolderView<'a>(&'a InspectedFolder);

impl FolderView for ArchiveFolderView<'_> {
    fn folder_name(&self) -> &FolderName {
        &self.0.name
    }

    fn root_files(&self) -> io::Result<Vec<String>> {
        Ok(self.0.root_files.clone())
    }

    fn read_toc(&self, file: &str) -> io::Result<Vec<u8>> {
        self.0.tocs.get(file).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{file:?} is not a TOC of {}", self.0.name.as_str()),
            )
        })
    }
}

// ---------------------------------------------------------------------------------------
// Errors: one mapping from fault to code.

/// Why an archive cannot be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFault {
    /// Not an installable addon (`bad_archive`, exit 1).
    Malformed,
    /// Crafted or dangerous (`unsafe_archive`, exit 4).
    Hostile,
    /// Over a limit (`archive_too_large`, exit 4); `details.config_key` names the limit.
    TooLarge,
}

/// The error for an archive fault.
pub fn archive_error(fault: ArchiveFault, message: impl Into<String>, details: Value) -> Error {
    let (code, fix) = match fault {
        ArchiveFault::Malformed => (
            ErrorCode::BadArchive,
            Suggestion::text(
                "nothing was installed; the archive holds no installable addon, so report it to the addon's author",
            ),
        ),
        ArchiveFault::Hostile => (
            ErrorCode::UnsafeArchive,
            Suggestion::text(
                "nothing was installed; do not install this archive, and report it to the addon's author",
            ),
        ),
        ArchiveFault::TooLarge => {
            let key = details["config_key"].as_str().unwrap_or("limits");
            (
                ErrorCode::ArchiveTooLarge,
                Suggestion::text(format!(
                    "if the archive is genuine, raise the limit: omaddons config set {key} <value>"
                )),
            )
        }
    };
    Error::new(code, message, fix).details(details)
}

fn hostile(entry: &str, why: &str) -> Error {
    archive_error(
        ArchiveFault::Hostile,
        format!("the archive entry {entry:?} {why}"),
        json!({ "entry": entry, "reason": why }),
    )
}

fn malformed(message: String, reason: &str) -> Error {
    archive_error(
        ArchiveFault::Malformed,
        message,
        json!({ "reason": reason }),
    )
}

fn too_large(what: &str, measured: u64, limit: u64, key: &str) -> Error {
    archive_error(
        ArchiveFault::TooLarge,
        format!("the archive has {measured} {what}, over the limit of {limit}"),
        json!({ "measured": measured, "limit": limit, "config_key": key }),
    )
}

fn ratio_exceeded(entry: &str, expanded: u64, compressed: u64, max_ratio: u64) -> Error {
    archive_error(
        ArchiveFault::Hostile,
        format!(
            "{entry} expands {expanded} bytes from {compressed}, past the {max_ratio}:1 compression ratio"
        ),
        json!({ "entry": entry, "measured": expanded, "compressed": compressed, "limit": max_ratio, "reason": "ratio" }),
    )
}

/// Whether `expanded` bytes from `compressed` break the ratio (past the floor).
fn over_ratio(expanded: u64, compressed: u64, max_ratio: u64) -> bool {
    expanded > ARCHIVE_RATIO_FLOOR && expanded > compressed.saturating_mul(max_ratio)
}

// ---------------------------------------------------------------------------------------
// Entry names and the case-folded tree.

const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFLNK: u32 = 0o120_000;

/// A zip entry name as a `/`-separated relative path, and whether it names a directory;
/// `None` for the archive root itself.
pub fn normalize_entry(raw: &str) -> Result<Option<(String, bool)>> {
    let unified = raw.replace('\\', "/");
    if unified.chars().any(|c| c.is_ascii_control()) {
        return Err(hostile(raw, "contains a control character"));
    }
    if unified.starts_with('/') {
        return Err(hostile(raw, "is an absolute path"));
    }
    let bytes = unified.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(hostile(raw, "starts with a drive letter"));
    }
    let is_dir = unified.ends_with('/');
    let mut parts = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(hostile(raw, "climbs out of its folder with '..'")),
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some((parts.join("/"), is_dir)))
}

/// Refuses symlinks, devices, FIFOs and sockets by their unix mode.
fn check_mode(mode: Option<u32>, raw: &str) -> Result<()> {
    match mode.map(|m| m & S_IFMT) {
        None | Some(0 | S_IFREG | S_IFDIR) => Ok(()),
        Some(S_IFLNK) => Err(hostile(raw, "is a symlink")),
        Some(_) => Err(hostile(raw, "is a device, FIFO or socket")),
    }
}

#[derive(Debug, Default)]
struct Node {
    dirs: BTreeSet<String>,
    files: BTreeSet<String>,
}

/// Every path of an archive, refusing case-folded collisions.
#[derive(Debug, Default)]
struct Tree {
    /// Folded path → (spelling, is a directory).
    seen: HashMap<String, (String, bool)>,
    /// Directory path (`""` for the root) → its children.
    nodes: BTreeMap<String, Node>,
}

impl Tree {
    fn add(&mut self, path: &str, is_dir: bool) -> Result<()> {
        self.nodes.entry(String::new()).or_default();
        let parts: Vec<&str> = path.split('/').collect();
        let mut spelled = String::new();
        for (i, part) in parts.iter().enumerate() {
            let parent = spelled.clone();
            if !spelled.is_empty() {
                spelled.push('/');
            }
            spelled.push_str(part);
            let dir = i + 1 < parts.len() || is_dir;
            self.register(&spelled, dir)?;
            let node = self.nodes.entry(parent).or_default();
            if dir {
                node.dirs.insert((*part).to_owned());
                self.nodes.entry(spelled.clone()).or_default();
            } else {
                node.files.insert((*part).to_owned());
            }
        }
        Ok(())
    }

    fn register(&mut self, spelled: &str, dir: bool) -> Result<()> {
        let key = spelled.to_ascii_lowercase();
        match self.seen.get(&key) {
            None => {
                self.seen.insert(key, (spelled.to_owned(), dir));
                Ok(())
            }
            Some((other, other_dir)) if dir && *other_dir && other == spelled => Ok(()),
            Some((other, other_dir)) => {
                let why = match (dir, *other_dir) {
                    (true, false) | (false, true) => {
                        format!("is both a file and a directory (with {other:?})")
                    }
                    _ if other == spelled => "appears twice".to_owned(),
                    _ => format!("collides with {other:?} once case is ignored"),
                };
                Err(hostile(spelled, &why))
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Inspection.

/// A measured file, by its full path in the archive.
struct Measured {
    path: String,
    size: u64,
    sha256: String,
    source: FileSource,
}

/// Inspects a zip blob.
pub fn inspect(blob: &Path, limits: &ArchiveLimits, table: &RulesTable) -> Result<Inspected> {
    let open = || {
        std::fs::File::open(blob)
            .map_err(|e| Error::io(blob, &e, Suggestion::text("download the archive again")))
    };
    let (_, blob_sha256) = hash_stream(
        open()?,
        |_| Ok(()),
        |e| Error::io(blob, &e, Suggestion::text("download the archive again")),
    )?;
    let mut inspected = inspect_zip(open()?, limits, table)?;
    inspected.blob_sha256 = Some(blob_sha256);
    Ok(inspected)
}

/// Inspects a zip from any seekable reader (an in-memory `Cursor` for fuzzing).
pub fn inspect_zip<R: Read + Seek>(
    mut reader: R,
    limits: &ArchiveLimits,
    table: &RulesTable,
) -> Result<Inspected> {
    let listed = central_directory_count(&mut reader);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| {
        malformed(
            format!("the download is not a readable zip: {e}"),
            "not_a_zip",
        )
    })?;
    let indexed = archive.len() as u64;
    let count = listed.unwrap_or(indexed).max(indexed);
    if count > limits.max_entries {
        return Err(too_large(
            "entries",
            count,
            limits.max_entries,
            "limits.archive_max_entries",
        ));
    }
    // The zip crate keeps one entry per name; other extractors may keep the other one.
    if count > indexed {
        return Err(archive_error(
            ArchiveFault::Hostile,
            format!("the archive lists {count} entries but only {indexed} distinct names"),
            json!({ "reason": "duplicate_entry", "measured": count, "distinct": indexed }),
        ));
    }

    // Headers first: names, types, collisions and declared sizes, before decompressing.
    let mut tree = Tree::default();
    let mut files: Vec<(String, usize, u64)> = Vec::new();
    let (mut declared, mut compressed_total) = (0u64, 0u64);
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(zip_error)?;
        let raw = entry.name().map_err(zip_error)?.into_owned();
        let mode = entry.unix_mode();
        check_mode(mode, &raw)?;
        let Some((path, slash)) = normalize_entry(&raw)? else {
            continue;
        };
        let is_dir = slash || entry.is_dir() || mode.is_some_and(|m| m & S_IFMT == S_IFDIR);
        tree.add(&path, is_dir)?;
        if is_dir {
            continue;
        }
        if entry.encrypted() {
            return Err(malformed(format!("{path} is encrypted"), "encrypted"));
        }
        let method = entry.compression();
        if !matches!(
            method,
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(malformed(
                format!("{path} uses {method} compression, which omaddons does not read"),
                "unsupported_compression",
            ));
        }
        let (size, compressed) = (entry.size(), entry.compressed_size());
        declared = declared.saturating_add(size);
        compressed_total = compressed_total.saturating_add(compressed);
        if declared > limits.max_uncompressed {
            return Err(too_large(
                "bytes uncompressed",
                declared,
                limits.max_uncompressed,
                "limits.archive_max_uncompressed",
            ));
        }
        if over_ratio(size, compressed, limits.max_ratio) {
            return Err(ratio_exceeded(&path, size, compressed, limits.max_ratio));
        }
        files.push((path, index, compressed));
    }

    // Then the bytes, counted as they decompress: lying headers stop at the real limits.
    let mut measured = Vec::with_capacity(files.len());
    let mut total = 0u64;
    for (path, index, compressed) in files {
        let entry = archive.by_index(index).map_err(zip_error)?;
        let (size, sha256) = hash_stream(
            entry,
            |n| {
                total = total.saturating_add(n);
                if total > limits.max_uncompressed {
                    return Err(too_large(
                        "bytes uncompressed",
                        total,
                        limits.max_uncompressed,
                        "limits.archive_max_uncompressed",
                    ));
                }
                if over_ratio(total, compressed_total, limits.max_ratio) {
                    return Err(ratio_exceeded(
                        "the archive",
                        total,
                        compressed_total,
                        limits.max_ratio,
                    ));
                }
                Ok(())
            },
            |e| read_error(&path, &e),
        )?;
        if over_ratio(size, compressed, limits.max_ratio) {
            return Err(ratio_exceeded(&path, size, compressed, limits.max_ratio));
        }
        measured.push(Measured {
            path,
            size,
            sha256,
            source: FileSource::ZipEntry(index),
        });
    }

    let mut folders = build_folders(&tree, measured, table)?;
    for folder in &mut folders {
        for (name, source) in toc_sources(folder) {
            let FileSource::ZipEntry(index) = source else {
                continue;
            };
            let entry = archive.by_index(index).map_err(zip_error)?;
            let bytes = read_bounded(entry).map_err(|e| read_error(&name, &e))?;
            folder.tocs.insert(name, bytes);
        }
    }
    Ok(Inspected {
        blob_sha256: None,
        folders,
    })
}

/// Inspects a directory (`file:` directories, git checkouts) under the same rules; the
/// directory itself may be the addon. `.git` directories are skipped; symlinks are
/// refused, never followed.
pub fn inspect_dir(dir: &Path, limits: &ArchiveLimits, table: &RulesTable) -> Result<Inspected> {
    let io_fix = || Suggestion::text("check the directory, then retry");
    let dir = std::fs::canonicalize(dir).map_err(|e| Error::io(dir, &e, io_fix()))?;
    let top = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            malformed(
                format!("{} cannot be an addon folder", dir.display()),
                "no_addon_root",
            )
        })?
        .to_owned();
    let mut tree = Tree::default();
    tree.add(&top, true)?;
    let mut measured = Vec::new();
    let (mut count, mut total) = (0u64, 0u64);
    let mut stack = vec![(dir.clone(), top)];
    while let Some((at, rel)) = stack.pop() {
        let listing = std::fs::read_dir(&at).map_err(|e| Error::io(&at, &e, io_fix()))?;
        for item in listing {
            let item = item.map_err(|e| Error::io(&at, &e, io_fix()))?;
            count += 1;
            if count > limits.max_entries {
                return Err(too_large(
                    "entries",
                    count,
                    limits.max_entries,
                    "limits.archive_max_entries",
                ));
            }
            let full = item.path();
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                return Err(hostile(
                    &full.display().to_string(),
                    "has a name that is not UTF-8",
                ));
            };
            let path = format!("{rel}/{name}");
            if name.chars().any(|c| c.is_ascii_control() || c == '\\') {
                return Err(hostile(&path, "contains a control character or backslash"));
            }
            let kind = std::fs::symlink_metadata(&full)
                .map_err(|e| Error::io(&full, &e, io_fix()))?
                .file_type();
            if kind.is_symlink() {
                return Err(hostile(&path, "is a symlink"));
            }
            if kind.is_dir() {
                if name == ".git" {
                    continue;
                }
                tree.add(&path, true)?;
                stack.push((full, path));
            } else if kind.is_file() {
                tree.add(&path, false)?;
                let file =
                    std::fs::File::open(&full).map_err(|e| Error::io(&full, &e, io_fix()))?;
                let (size, sha256) = hash_stream(
                    file,
                    |n| {
                        total = total.saturating_add(n);
                        if total > limits.max_uncompressed {
                            return Err(too_large(
                                "bytes",
                                total,
                                limits.max_uncompressed,
                                "limits.archive_max_uncompressed",
                            ));
                        }
                        Ok(())
                    },
                    |e| Error::io(&full, &e, io_fix()),
                )?;
                measured.push(Measured {
                    path,
                    size,
                    sha256,
                    source: FileSource::Disk(full),
                });
            } else {
                return Err(hostile(&path, "is a device, FIFO or socket"));
            }
        }
    }

    let mut folders = build_folders(&tree, measured, table)?;
    for folder in &mut folders {
        for (name, source) in toc_sources(folder) {
            let FileSource::Disk(path) = source else {
                continue;
            };
            let file = std::fs::File::open(&path).map_err(|e| Error::io(&path, &e, io_fix()))?;
            let bytes = read_bounded(file).map_err(|e| Error::io(&path, &e, io_fix()))?;
            folder.tocs.insert(name, bytes);
        }
    }
    Ok(Inspected {
        blob_sha256: None,
        folders,
    })
}

/// Streams `reader` through SHA-256, reporting each chunk's length to `on_bytes`.
fn hash_stream(
    mut reader: impl Read,
    mut on_bytes: impl FnMut(u64) -> Result<()>,
    map_io: impl Fn(io::Error) -> Error,
) -> Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(map_io(e)),
        };
        let chunk = buf.get(..n).unwrap_or_default();
        hasher.update(chunk);
        size = size.saturating_add(n as u64);
        on_bytes(n as u64)?;
    }
    Ok((size, hex(&hasher.finalize())))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The entry count the end-of-central-directory record declares, read from the tail;
/// `None` when there is no record or the count is in a zip64 record. Leaves the reader
/// at the start.
fn central_directory_count<R: Read + Seek>(reader: &mut R) -> Option<u64> {
    const EOCD_LEN: u64 = 22;
    let len = reader.seek(SeekFrom::End(0)).ok()?;
    let tail_len = len.min(EOCD_LEN + u64::from(u16::MAX));
    reader.seek(SeekFrom::Start(len - tail_len)).ok()?;
    let mut tail = vec![0u8; usize::try_from(tail_len).ok()?];
    let read = reader.read_exact(&mut tail);
    reader.seek(SeekFrom::Start(0)).ok()?;
    read.ok()?;
    let u16_at = |at: usize| Some(u16::from_le_bytes([*tail.get(at)?, *tail.get(at + 1)?]));
    // The record is the signature whose comment length runs exactly to the end.
    let at = (0..tail.len().saturating_sub(21)).rev().find(|&at| {
        tail.get(at..at + 4) == Some(b"PK\x05\x06")
            && u16_at(at + 20).is_some_and(|comment| at + 22 + usize::from(comment) == tail.len())
    })?;
    let count = u16_at(at + 10)?;
    (count != u16::MAX).then_some(u64::from(count))
}

fn zip_error(e: zip::result::ZipError) -> Error {
    malformed(format!("the zip cannot be read: {e}"), "corrupt")
}

/// A read failure while decompressing: an entry past its declared size is crafted,
/// anything else is corruption.
fn read_error(path: &str, e: &io::Error) -> Error {
    if e.kind() == io::ErrorKind::InvalidData && e.to_string().contains("declared") {
        return hostile(path, "expands past the size its header declares");
    }
    malformed(format!("{path} cannot be read: {e}"), "corrupt")
}

/// The TOC files directly inside a folder, with where to read them.
fn toc_sources(folder: &InspectedFolder) -> Vec<(String, FileSource)> {
    folder
        .files
        .iter()
        .filter(|f| !f.rel_path.contains('/') && toc_stem(&f.rel_path).is_some())
        .map(|f| (f.rel_path.clone(), f.source.clone()))
        .collect()
}

// ---------------------------------------------------------------------------------------
// Addon roots.

fn build_folders(
    tree: &Tree,
    measured: Vec<Measured>,
    table: &RulesTable,
) -> Result<Vec<InspectedFolder>> {
    let suffixes = table.toc_suffixes();
    let roots = find_roots(tree, &suffixes);
    if roots.is_empty() {
        return Err(malformed(
            "the archive has no addon folder: no directory holds a TOC named after it".to_owned(),
            "no_addon_root",
        ));
    }
    let mut folders: Vec<InspectedFolder> = Vec::new();
    for (prefix, name) in roots {
        let name = FolderName::new(&name)
            .map_err(|e| hostile(&prefix, &format!("cannot be an addon folder: {e}")))?;
        if let Some(other) = folders.iter().find(|f| f.name == name) {
            return Err(malformed(
                format!(
                    "{prefix} and {} would both install as {}",
                    other.archive_prefix,
                    name.as_str()
                ),
                "duplicate_folder",
            ));
        }
        let root_files = tree
            .nodes
            .get(&prefix)
            .map(|n| n.files.iter().cloned().collect())
            .unwrap_or_default();
        folders.push(InspectedFolder {
            name,
            archive_prefix: prefix,
            files: Vec::new(),
            root_files,
            tocs: BTreeMap::new(),
        });
    }
    for m in measured {
        let owner = folders.iter_mut().find_map(|f| {
            let rel = m.path.strip_prefix(&f.archive_prefix)?.strip_prefix('/')?;
            Some((f, rel.to_owned()))
        });
        if let Some((folder, rel_path)) = owner {
            folder.files.push(InspectedFile {
                rel_path,
                size: m.size,
                sha256: m.sha256,
                source: m.source,
            });
        }
    }
    Ok(folders)
}

/// The addon roots: (directory path, folder name), from the first level that has any,
/// descending through single wrapper directories.
fn find_roots(tree: &Tree, suffixes: &[&str]) -> Vec<(String, String)> {
    let mut level = String::new();
    loop {
        let Some(node) = tree.nodes.get(&level) else {
            return Vec::new();
        };
        let join = |name: &str| {
            if level.is_empty() {
                name.to_owned()
            } else {
                format!("{level}/{name}")
            }
        };
        let roots: Vec<(String, String)> = node
            .dirs
            .iter()
            .filter_map(|name| {
                let path = join(name);
                let child = tree.nodes.get(&path)?;
                root_name(name, child, suffixes).map(|install| (path, install))
            })
            .collect();
        if !roots.is_empty() {
            return roots;
        }
        let mut dirs = node.dirs.iter();
        match (dirs.next(), dirs.next()) {
            (Some(only), None) => level = join(only),
            _ => return Vec::new(),
        }
    }
}

/// The folder name a directory installs as, when it is an addon root.
fn root_name(dir: &str, node: &Node, suffixes: &[&str]) -> Option<String> {
    let bases: Vec<&str> = node
        .files
        .iter()
        .filter_map(|f| toc_stem(f))
        .flat_map(|stem| toc_bases(stem, suffixes))
        .collect();
    if bases.iter().any(|b| b.eq_ignore_ascii_case(dir)) {
        return Some(dir.to_owned());
    }
    wrapped_names(dir).into_iter().find_map(|wrapped| {
        bases
            .iter()
            .find(|b| b.eq_ignore_ascii_case(wrapped))
            .map(|b| (*b).to_owned())
    })
}

/// `Foo_Mainline` for `Foo_Mainline.toc` (any case of `.toc`).
fn toc_stem(file: &str) -> Option<&str> {
    let cut = file.len().checked_sub(4)?;
    let (stem, ext) = (file.get(..cut)?, file.get(cut..)?);
    (!stem.is_empty() && ext.eq_ignore_ascii_case(".toc")).then_some(stem)
}

/// The names a TOC stem can be the TOC of: itself, and itself minus a known suffix
/// after `-` or `_`.
fn toc_bases<'a>(stem: &'a str, suffixes: &[&str]) -> Vec<&'a str> {
    let mut bases = vec![stem];
    for suffix in suffixes {
        let Some(cut) = stem.len().checked_sub(suffix.len() + 1) else {
            continue;
        };
        let (Some(base), Some(sep), Some(tail)) =
            (stem.get(..cut), stem.get(cut..=cut), stem.get(cut + 1..))
        else {
            continue;
        };
        if !base.is_empty() && (sep == "-" || sep == "_") && tail.eq_ignore_ascii_case(suffix) {
            bases.push(base);
        }
    }
    bases
}

/// `Foo` for `Foo-main`, `Foo-master`, `Foo-1.2.3`, `Foo-v1.2` or `Foo-<commit sha>`
/// (codeload archives of a commit), leftmost cut first.
fn wrapped_names(dir: &str) -> Vec<&str> {
    dir.match_indices('-')
        .filter_map(|(i, _)| {
            let (name, rest) = (dir.get(..i)?, dir.get(i + 1..)?);
            let mut chars = rest.chars();
            let version = match chars.next() {
                Some(c) if c.is_ascii_digit() => true,
                Some('v' | 'V') => chars.next().is_some_and(|c| c.is_ascii_digit()),
                _ => false,
            };
            let branch = rest.eq_ignore_ascii_case("main") || rest.eq_ignore_ascii_case("master");
            let commit = rest.len() == 40 && rest.chars().all(|c| c.is_ascii_hexdigit());
            (!name.is_empty() && (version || branch || commit)).then_some(name)
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Verification.

/// One folder's verdict on a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FolderVerdict {
    pub folder: FolderName,
    pub report: VerdictReport,
    /// The required dependencies of the TOC the client loads (`Dependencies`,
    /// `RequiredDeps`, `Dep*`).
    #[serde(skip)]
    pub dependencies: Vec<FolderName>,
}

/// Each folder judged by the target client's TOC load rules.
pub fn verify(
    inspected: &Inspected,
    target: &Target,
    table: &RulesTable,
) -> Result<Vec<FolderVerdict>> {
    inspected
        .folders
        .iter()
        .map(|folder| {
            let loaded = loaded_toc(&folder.view(), &target.rules)
                .map_err(|e| Error::internal(format!("verifying {}: {e}", folder.name.as_str())))?;
            let mut report = judge(
                folder.name.as_str(),
                loaded.as_ref(),
                &target.rules,
                target.family,
                target.interface,
                table,
            );
            // A root has a TOC by construction; when the client loads none of them, the
            // folder is for other clients.
            if report.verdict == Verdict::NoToc {
                report.verdict = Verdict::WrongFlavor {
                    reason: WrongFlavorReason::NoLoadableToc,
                };
            }
            Ok(FolderVerdict {
                folder: folder.name.clone(),
                report,
                dependencies: loaded
                    .map(|l| l.toc.dependency_folders())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// A folder of an archive that the target's client would not load, left out of the
/// install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SkippedFolder {
    pub name: FolderName,
    #[serde(flatten)]
    pub reason: WrongFlavorReason,
}

/// An archive's folders split by whether the target's client loads them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Partition {
    /// In archive order.
    pub install: Vec<FolderName>,
    pub skip: Vec<SkippedFolder>,
}

/// Splits the verdicts: folders the client loads install, the others are skipped, and
/// so is every loading folder that requires a skipped folder of the same archive
/// (transitively), because the client would not load it either. Dependencies outside
/// the archive are not judged here.
pub fn partition(verdicts: &[FolderVerdict]) -> Partition {
    let mut skip: Vec<SkippedFolder> = Vec::new();
    let mut install: Vec<&FolderVerdict> = Vec::new();
    for v in verdicts {
        match &v.report.verdict {
            Verdict::Compatible { .. } => install.push(v),
            Verdict::WrongFlavor { reason } => skip.push(SkippedFolder {
                name: v.folder.clone(),
                reason: reason.clone(),
            }),
            Verdict::NoToc => skip.push(SkippedFolder {
                name: v.folder.clone(),
                reason: WrongFlavorReason::NoLoadableToc,
            }),
        }
    }
    // FolderName equality is the case-folded key, as the client resolves names.
    while let Some((at, folder)) = install.iter().enumerate().find_map(|(at, v)| {
        v.dependencies
            .iter()
            .find_map(|dep| skip.iter().find(|s| s.name == *dep))
            .map(|s| (at, s.name.clone()))
    }) {
        let blocked = install.remove(at);
        skip.push(SkippedFolder {
            name: blocked.folder.clone(),
            reason: WrongFlavorReason::DependencySkipped { folder },
        });
    }
    Partition {
        install: install.into_iter().map(|v| v.folder.clone()).collect(),
        skip,
    }
}

/// Whether the target installs an archive split as `partition`. Some folder must load.
/// When the publisher's metadata declares this client (`declared`), the archive is
/// meant for it and the folders it does not load are simply skipped. Without that
/// promise the addon's own folder (`primary`) must load, and every skipped folder must
/// be gated away by its own TOCs (`AllowLoadGameType`/`ExcludeLoadGameType`, or TOCs
/// only for other clients) or require such a folder: a folder that merely lists other
/// clients' interfaces marks a build for another client, which is refused whole.
pub fn accepts(partition: &Partition, declared: bool, primary: &FolderName) -> bool {
    if partition.install.is_empty() {
        return false;
    }
    declared
        || (partition.install.contains(primary)
            && partition.skip.iter().all(|s| {
                matches!(
                    s.reason,
                    WrongFlavorReason::GameTypeExcluded
                        | WrongFlavorReason::NoLoadableToc
                        | WrongFlavorReason::DependencySkipped { .. }
                )
            }))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn tree(paths: &[&str]) -> Result<Tree> {
        let mut tree = Tree::default();
        for raw in paths {
            if let Some((path, dir)) = normalize_entry(raw)? {
                tree.add(&path, dir)?;
            }
        }
        Ok(tree)
    }

    fn roots(paths: &[&str]) -> Vec<String> {
        let table = RulesTable::builtin();
        find_roots(&tree(paths).unwrap(), &table.toc_suffixes())
            .into_iter()
            .map(|(path, name)| format!("{path}={name}"))
            .collect()
    }

    #[test]
    fn entry_names() {
        let cases: &[(&str, Option<(&str, bool)>)] = &[
            ("Foo/Foo.toc", Some(("Foo/Foo.toc", false))),
            ("Foo\\Bar\\x.lua", Some(("Foo/Bar/x.lua", false))),
            ("./Foo//./x.lua", Some(("Foo/x.lua", false))),
            ("Foo/", Some(("Foo", true))),
            ("Foo\\", Some(("Foo", true))),
            ("Foo/C:x", Some(("Foo/C:x", false))),
            ("./", None),
            ("", None),
        ];
        for (raw, want) in cases {
            let want = want.map(|(p, d)| (p.to_owned(), d));
            assert_eq!(normalize_entry(raw).unwrap(), want, "{raw:?}");
        }
        for bad in [
            "../x",
            "Foo/../../x",
            "..\\x",
            "Foo\\..\\x",
            "/etc/passwd",
            "\\x",
            "C:/x",
            "c:\\x",
            "Z:x",
            "Foo/a\0b",
            "Foo/\u{7}",
            "Foo/\u{7f}",
        ] {
            let err = normalize_entry(bad).unwrap_err();
            assert_eq!(
                (err.code, err.exit().code()),
                (ErrorCode::UnsafeArchive, 4),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn unix_modes() {
        for ok in [None, Some(0o644), Some(0o100_644), Some(0o040_755)] {
            assert!(check_mode(ok, "x").is_ok(), "{ok:?}");
        }
        for bad in [0o120_777, 0o020_644, 0o060_644, 0o010_644, 0o140_644] {
            assert_eq!(
                check_mode(Some(bad), "x").unwrap_err().code,
                ErrorCode::UnsafeArchive,
                "{bad:o}"
            );
        }
    }

    #[test]
    fn root_detection() {
        let cases: &[(&[&str], &[&str])] = &[
            (&["Foo/Foo.toc", "Foo/x.lua"], &["Foo=Foo"]),
            (&["Foo-main/Foo.toc"], &["Foo-main=Foo"]),
            (&["Foo-master/Foo_Mainline.toc"], &["Foo-master=Foo"]),
            (
                &["Foo-1.2.3/Foo/Foo.toc", "Foo-1.2.3/README.md"],
                &["Foo-1.2.3/Foo=Foo"],
            ),
            (&["Foo-v1.2/Foo-Classic.toc"], &["Foo-v1.2=Foo"]),
            (
                &["Foo-fbf8ed267da1d86e5fa812cc5d7629aedabbe17a/Foo.toc"],
                &["Foo-fbf8ed267da1d86e5fa812cc5d7629aedabbe17a=Foo"],
            ),
            (&["outer/inner-main/X/X.toc"], &["outer/inner-main/X=X"]),
            (
                &[
                    "Repo-main/A/A.toc",
                    "Repo-main/B/B-Classic.toc",
                    "Repo-main/README.md",
                    "Repo-main/.github/ci.yml",
                ],
                &["Repo-main/A=A", "Repo-main/B=B"],
            ),
            (&["README.md", "LICENSE", "Foo/Foo.toc"], &["Foo=Foo"]),
            (
                &["Foo/Foo.toc", "Foo/Libs/LibStub/LibStub.toc"],
                &["Foo=Foo"],
            ),
            (
                &[
                    "DBM-Core/DBM-Core.toc",
                    "DBM-Core/Libs/LibStub/LibStub.toc",
                    "DBM-Core/Libs/LibDropDownMenu/LibDropDownMenu.toc",
                    "DBM-Core/Libs/LibCustomGlow-1.0/LibCustomGlow-1.0.toc",
                    "DBM-GUI/DBM-GUI.toc",
                    "DBM-Midnight/DBM-Midnight.toc",
                ],
                &[
                    "DBM-Core=DBM-Core",
                    "DBM-GUI=DBM-GUI",
                    "DBM-Midnight=DBM-Midnight",
                ],
            ),
            (&["foo/FOO.TOC"], &["foo=foo"]),
            (&["Foo-Bar/Foo-Bar.toc"], &["Foo-Bar=Foo-Bar"]),
            (&["Foo-Bar-1.0/Foo-Bar.toc"], &["Foo-Bar-1.0=Foo-Bar"]),
            (
                &["weakauras-main/WeakAuras.toc"],
                &["weakauras-main=WeakAuras"],
            ),
            (&["Blizzard_X/Blizzard_X.toc"], &["Blizzard_X=Blizzard_X"]),
            (&["Foo.toc", "x.lua"], &[]),
            (&["A/x.lua", "B/y.lua"], &[]),
            (&["Foo-feature/Foo.toc"], &[]),
            (&[], &[]),
        ];
        for (paths, want) in cases {
            assert_eq!(roots(paths), *want, "{paths:?}");
        }
    }

    #[test]
    fn case_folded_collisions() {
        let colliding: &[&[&str]] = &[
            &["Foo/a.lua", "Foo/A.lua"],
            &["Foo/Lib/x", "Foo/lib/y"],
            &["Foo/x", "Foo/X/y"],
            &["Foo/x/", "Foo/x"],
            &["Foo/a", "Foo/a"],
        ];
        for paths in colliding {
            let err = tree(paths).unwrap_err();
            assert_eq!(err.code, ErrorCode::UnsafeArchive, "{paths:?}");
        }
        assert!(
            tree(&["Foo/", "Foo/x.lua", "Foo/Sub/", "Foo/Sub/y.lua"]).is_ok(),
            "explicit and implicit directories agree"
        );
    }

    #[test]
    fn central_directory_count_reads_the_real_record() {
        use std::io::{Cursor, Write};
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.set_comment("a decoy PK\u{5}\u{6} record in the comment")
            .unwrap();
        for name in ["a", "b", "c"] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"x").unwrap();
        }
        let mut bytes = Cursor::new(zip.finish().unwrap().into_inner());
        assert_eq!(central_directory_count(&mut bytes), Some(3));
        assert_eq!(bytes.position(), 0, "the reader is rewound");
        assert_eq!(
            central_directory_count(&mut Cursor::new(b"not a zip".to_vec())),
            None
        );
    }

    #[test]
    fn toc_names() {
        let s = ["Mainline", "Classic", "Vanilla"];
        assert_eq!(toc_bases("Foo_Mainline", &s), ["Foo_Mainline", "Foo"]);
        assert_eq!(toc_bases("Foo-classic", &s), ["Foo-classic", "Foo"]);
        assert_eq!(toc_bases("FooMainline", &s), ["FooMainline"]);
        assert_eq!(toc_bases("_Mainline", &s), ["_Mainline"]);
        assert_eq!(toc_bases("é_Classic", &s), ["é_Classic", "é"]);
        assert_eq!(
            (
                toc_stem("x.TOC"),
                toc_stem(".toc"),
                toc_stem("é.toc"),
                toc_stem("x.lua")
            ),
            (Some("x"), None, Some("é"), None)
        );
        assert_eq!(wrapped_names("Foo-Bar-1.2-beta"), ["Foo-Bar"]);
        assert_eq!(wrapped_names("Foo-MAIN"), ["Foo"]);
        assert!(wrapped_names("-main").is_empty() && wrapped_names("Foo-vx").is_empty());
        // A commit sha only in full: `Foo-cafe` is a name, not a wrapper.
        assert_eq!(wrapped_names(&format!("Foo-{}", "c".repeat(40))), ["Foo"]);
        assert!(wrapped_names("Foo-cafe").is_empty());
    }

    #[test]
    fn one_mapping_from_fault_to_code() {
        let cases = [
            (ArchiveFault::Malformed, ErrorCode::BadArchive, 1),
            (ArchiveFault::Hostile, ErrorCode::UnsafeArchive, 4),
            (ArchiveFault::TooLarge, ErrorCode::ArchiveTooLarge, 4),
        ];
        for (fault, code, exit) in cases {
            let err = archive_error(
                fault,
                "x",
                json!({ "config_key": "limits.archive_max_entries" }),
            );
            assert_eq!(
                (err.code, err.exit().code(), err.retryable),
                (code, exit, false)
            );
        }
        let err = too_large("entries", 9, 3, "limits.archive_max_entries");
        assert!(
            err.fix
                .display()
                .contains("omaddons config set limits.archive_max_entries")
        );
        assert_eq!(
            ArchiveLimits::from(&EffectiveLimits::default()),
            ArchiveLimits::default()
        );
    }

    #[test]
    fn ratios_apply_past_the_floor() {
        assert!(
            !over_ratio(ARCHIVE_RATIO_FLOOR, 1, 200),
            "small and repetitive is fine"
        );
        assert!(over_ratio(ARCHIVE_RATIO_FLOOR + 1, 1, 200));
        assert!(
            !over_ratio(40_000_000, 200_000, 200),
            "exactly 200:1 is allowed"
        );
        assert!(over_ratio(40_000_001, 200_000, 200));
        assert!(
            !over_ratio(u64::MAX, 2, u64::MAX),
            "the product saturates instead of overflowing"
        );
    }

    fn name(n: &str) -> FolderName {
        FolderName::new(n).unwrap()
    }

    fn folder_verdict(folder: &str, verdict: Verdict, deps: &[&str]) -> FolderVerdict {
        FolderVerdict {
            folder: name(folder),
            report: VerdictReport {
                loaded: None,
                verdict,
                warnings: Vec::new(),
            },
            dependencies: deps.iter().map(|d| name(d)).collect(),
        }
    }

    fn loads(folder: &str, deps: &[&str]) -> FolderVerdict {
        folder_verdict(folder, Verdict::Compatible { out_of_date: false }, deps)
    }

    fn wrong(folder: &str, reason: WrongFlavorReason) -> FolderVerdict {
        folder_verdict(folder, Verdict::WrongFlavor { reason }, &[])
    }

    fn not_in_family() -> WrongFlavorReason {
        WrongFlavorReason::InterfaceNotInFamily {
            interfaces: vec![120_105],
            family: crate::rules::Family::Vanilla,
        }
    }

    /// (installed, skipped as `name=reason`), and whether the target installs it with
    /// and without metadata declaring it.
    fn split(verdicts: &[FolderVerdict], primary: &str) -> (Vec<String>, Vec<String>, bool, bool) {
        let p = partition(verdicts);
        let install = p.install.iter().map(|f| f.as_str().to_owned()).collect();
        let skip = p
            .skip
            .iter()
            .map(|s| {
                let reason = serde_json::to_value(&s.reason).unwrap();
                match &s.reason {
                    WrongFlavorReason::DependencySkipped { folder } => {
                        format!("{}=dependency_skipped:{}", s.name.as_str(), folder.as_str())
                    }
                    _ => format!("{}={}", s.name.as_str(), reason["reason"].as_str().unwrap()),
                }
            })
            .collect();
        let primary = name(primary);
        (
            install,
            skip,
            accepts(&p, true, &primary),
            accepts(&p, false, &primary),
        )
    }

    #[test]
    fn partitions() {
        let gated = || WrongFlavorReason::GameTypeExcluded;
        let none: Vec<String> = Vec::new();

        // Every folder loads: all install, with or without metadata.
        let all = [loads("Foo", &[]), loads("FooOptions", &["Foo"])];
        assert_eq!(
            split(&all, "Foo"),
            (
                vec!["Foo".into(), "FooOptions".into()],
                none.clone(),
                true,
                true
            )
        );

        // DBM on era: shared folders load, retail-only modules are gated by game type.
        let dbm = [
            loads("DBM-Core", &[]),
            loads("DBM-GUI", &["DBM-Core"]),
            wrong("DBM-Midnight", gated()),
            wrong("DBM-Brawlers", gated()),
        ];
        assert_eq!(
            split(&dbm, "DBM-Core"),
            (
                vec!["DBM-Core".into(), "DBM-GUI".into()],
                vec![
                    "DBM-Midnight=game_type_excluded".into(),
                    "DBM-Brawlers=game_type_excluded".into()
                ],
                true,
                true
            )
        );

        // A folder for other clients only (TOCs with other suffixes) is a gate too.
        let suffixed = [
            loads("Foo", &[]),
            wrong("Foo_Retail", WrongFlavorReason::NoLoadableToc),
        ];
        assert!(split(&suffixed, "Foo").3);

        // A folder that merely lists other interfaces: skipped only on the publisher's
        // word; without it the archive is a build for another client.
        let listed = [loads("Foo", &[]), wrong("FooRetail", not_in_family())];
        let (install, skip, declared, undeclared) = split(&listed, "Foo");
        assert_eq!(
            (install, skip, declared, undeclared),
            (
                vec!["Foo".into()],
                vec!["FooRetail=interface_not_in_family".into()],
                true,
                false
            )
        );
        for reason in [
            WrongFlavorReason::NoInterface,
            WrongFlavorReason::TargetInterfaceUnknown,
        ] {
            let other = [loads("Foo", &[]), wrong("Bar", reason)];
            assert!(!split(&other, "Foo").3);
        }

        // Only a bundled library loads: the addon itself does not.
        let library = [wrong("Foo", not_in_family()), loads("LibFoo", &[])];
        let (_, _, declared, undeclared) = split(&library, "Foo");
        assert!(declared && !undeclared);
        let gated_primary = [wrong("Foo", gated()), loads("LibFoo", &[])];
        assert!(!split(&gated_primary, "Foo").3);

        // Requiring a skipped folder skips the folder, transitively and ignoring case;
        // a dependency outside the archive is not judged here.
        let chain = [
            loads("A", &["b"]),
            loads("B", &["C"]),
            wrong("C", gated()),
            loads("D", &["Elsewhere"]),
        ];
        assert_eq!(
            split(&chain, "D"),
            (
                vec!["D".into()],
                vec![
                    "C=game_type_excluded".into(),
                    "B=dependency_skipped:C".into(),
                    "A=dependency_skipped:B".into()
                ],
                true,
                true
            )
        );

        // Nothing loads: refused even when declared.
        let nothing = [
            wrong("Foo", gated()),
            folder_verdict("Bar", Verdict::NoToc, &[]),
        ];
        assert_eq!(
            split(&nothing, "Foo"),
            (
                none,
                vec![
                    "Foo=game_type_excluded".into(),
                    "Bar=no_loadable_toc".into()
                ],
                false,
                false
            )
        );
    }

    proptest! {
        /// Every folder lands in exactly one side, and no installed folder requires a
        /// skipped one.
        #[test]
        fn partitions_cover_the_archive(
            folders in proptest::collection::vec(
                (any::<u8>(), proptest::collection::vec(0usize..8, 0..3)),
                0..8,
            ),
        ) {
            let verdicts: Vec<FolderVerdict> = folders
                .iter()
                .enumerate()
                .map(|(i, (kind, deps))| {
                    let deps: Vec<String> = deps.iter().map(|d| format!("f{d}")).collect();
                    let deps: Vec<&str> = deps.iter().map(String::as_str).collect();
                    let verdict = match kind % 3 {
                        0 => Verdict::WrongFlavor { reason: WrongFlavorReason::GameTypeExcluded },
                        1 => Verdict::NoToc,
                        _ => Verdict::Compatible { out_of_date: false },
                    };
                    folder_verdict(&format!("F{i}"), verdict, &deps)
                })
                .collect();
            let p = partition(&verdicts);
            let mut seen: Vec<&FolderName> =
                p.install.iter().chain(p.skip.iter().map(|s| &s.name)).collect();
            seen.sort_by(|a, b| a.key().cmp(b.key()));
            let mut all: Vec<&FolderName> = verdicts.iter().map(|v| &v.folder).collect();
            all.sort_by(|a, b| a.key().cmp(b.key()));
            prop_assert_eq!(seen, all);
            for v in verdicts.iter().filter(|v| p.install.contains(&v.folder)) {
                prop_assert!(v.dependencies.iter().all(|d| !p.skip.iter().any(|s| s.name == *d)));
            }
        }

        #[test]
        fn random_entry_lists_never_panic(names in proptest::collection::vec("[a-zA-Z0-9._/\\\\:-]{1,24}", 0..16)) {
            let table = RulesTable::builtin();
            let paths: Vec<&str> = names.iter().map(String::as_str).collect();
            if let Ok(tree) = tree(&paths) {
                for (path, name) in find_roots(&tree, &table.toc_suffixes()) {
                    prop_assert!(!path.split('/').any(|p| p == ".." || p.is_empty()), "{path}");
                    prop_assert!(!name.contains('/'), "{name}");
                }
            }
        }
    }
}
