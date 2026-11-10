//! `backup import` (M3-D35): an archive `backup export` wrote, turned back into an
//! ordinary backup of a client: kind `imported`, `integrity: verified` with
//! `baseline: import`, its original capture time, never a qualifying checkpoint.
//!
//! An archive is untrusted input that becomes files inside a client dir, so only an
//! exact match with what omaddons' exporter writes is accepted. [`scan`] (pass 1) reads
//! the whole stream once and writes nothing: the zstd decoder's window is bounded before
//! it allocates; every entry is a regular file, a directory, a symlink, or a GNU long-name
//! or PAX record carrying a path, all bounded; names are relative paths under `WTF/` or
//! the three metadata names, with no `..`, absolute path, drive letter, backslash,
//! control character or duplicate (case-folded too); counts, sizes and the expansion
//! ratio are bounded; the payload and `manifest.json` must be a bijection (path, kind,
//! size, mode, link target, and every file's SHA-256, hashed here); every entry's parent
//! is a directory of the archive (so nothing can be written through a link); and the
//! symlinks' graph stays inside `WTF/`, through no other link's parent, without a cycle.
//! A refusal there costs no disk.
//!
//! [`import`] (pass 2) decompresses again into `<meta>/stage/<tx>/backup/`, owned by an
//! in-flight lock: directories and files first (each file hashed again as it is written,
//! so a file that changed between the two reads is refused), symlinks last, then one
//! `syncfs`, the description, and one no-replace rename into `<meta>/backups/<id>`. On
//! any failure the stage is removed and nothing is listed. Messages name a path only
//! below `Account/`, through a redactor learned from the archive's own names.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::create::{self, Created, DIR_MODE};
use super::export::{ExportedMeta, Hashing};
use super::inventory::{self, Change, Ring};
use super::{
    AddonsFile, BackupKind, BackupMeta, BackupSource, Baseline, EntryKind, ImportSource, Integrity,
    Manifest, ManifestEntry, Reflinked,
};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{
    self, Content, Durability, InflightLock, MutateOpts, Op, ScopeTable, WriteSession, Writer,
    ensure_dir_all, mutate, mutate_with,
};
use crate::limits::{
    ARCHIVE_MAX_RATIO, ARCHIVE_RATIO_FLOOR, BACKUP_IMPORT_MAX_BYTES, BACKUP_IMPORT_MAX_ENTRIES,
    BACKUP_IMPORT_MAX_MANIFEST_BYTES, BACKUP_IMPORT_MAX_META_BYTES, BACKUP_IMPORT_MAX_PATH,
    BACKUP_IMPORT_MAX_WINDOW_LOG, DISCOVERY_FILE_MAX_BYTES,
};
use crate::model::{Actor, BackupId, Target, TxId};
use crate::output::{Phase, ProgressSink};
use crate::privacy::WtfPathRedactor;
use crate::state::paths::{ClientPaths, Paths};
use crate::state::store;

/// The progress label: never a path (M3-D21).
pub const IMPORT_LABEL: &str = "importing";
/// Report progress every this many files.
const PROGRESS_EVERY: u64 = 64;

/// The metadata files an archive holds beside `WTF/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MetaName {
    Backup,
    Manifest,
    Addons,
}

impl MetaName {
    fn of(name: &str) -> Option<Self> {
        match name {
            "backup.json" => Some(Self::Backup),
            "manifest.json" => Some(Self::Manifest),
            "addons.json" => Some(Self::Addons),
            _ => None,
        }
    }

    /// The largest this file may be, and that bound's name: a manifest grows with the
    /// entries it lists, the others do not.
    fn max_bytes(self) -> (&'static str, u64) {
        match self {
            Self::Manifest => (
                "BACKUP_IMPORT_MAX_MANIFEST_BYTES",
                BACKUP_IMPORT_MAX_MANIFEST_BYTES,
            ),
            Self::Backup | Self::Addons => ("DISCOVERY_FILE_MAX_BYTES", DISCOVERY_FILE_MAX_BYTES),
        }
    }
}

/// What an entry's name is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Name {
    Meta(MetaName),
    /// Relative to `WTF/`, with `/`.
    Wtf(String),
}

/// One payload entry, as its header and body say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    /// Relative to `WTF/`.
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mode: u32,
    pub link: Option<String>,
    /// A file's SHA-256, hashed as it was read.
    pub sha256: Option<String>,
}

/// An archive read whole and found exact (pass 1).
#[derive(Debug, Clone)]
pub struct Scanned {
    /// The archive's `backup.json`: the exported backup's description.
    pub source: ExportedMeta,
    /// Its `manifest.json`, in path order, every file's hash checked.
    pub manifest: Manifest,
    pub addons: Option<AddonsFile>,
    /// The payload, in archive order.
    pub entries: Vec<Payload>,
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    /// The files' bytes.
    pub bytes: u64,
}

/// Pass 1 over the archive at `file`.
pub fn scan(file: &Path) -> Result<Scanned> {
    let (reader, len) = open(file)?;
    scan_reader(reader, len)
}

/// Pass 1 over `compressed`, `len` bytes of an archive (the fuzz target's entry point).
pub fn scan_reader(compressed: impl Read, len: u64) -> Result<Scanned> {
    let mut metas: BTreeMap<MetaName, Vec<u8>> = BTreeMap::new();
    let mut entries: Vec<Payload> = Vec::new();
    read_entries(compressed, len, &mut |item, body| {
        match item.name {
            Name::Meta(meta) => {
                if metas.contains_key(&meta) {
                    return Err(refused("duplicate_metadata", None));
                }
                let (limit, max) = meta.max_bytes();
                if item.size > max {
                    return Err(too_large(limit, max));
                }
                let mut bytes = Vec::new();
                body.take(item.size)
                    .read_to_end(&mut bytes)
                    .map_err(read_error)?;
                metas.insert(meta, bytes);
            }
            Name::Wtf(path) => {
                let sha256 = if matches!(item.kind, EntryKind::File) {
                    let mut hashed = Hashing::new(body.take(item.size));
                    io::copy(&mut hashed, &mut io::sink()).map_err(read_error)?;
                    let (read, sha256) = hashed.finish();
                    if read != item.size {
                        return Err(bad("truncated"));
                    }
                    Some(sha256)
                } else {
                    None
                };
                entries.push(Payload {
                    path,
                    kind: item.kind,
                    size: item.size,
                    mode: item.mode,
                    link: item.link,
                    sha256,
                });
            }
        }
        Ok(())
    })?;
    let (Some(backup), Some(manifest)) =
        (metas.get(&MetaName::Backup), metas.get(&MetaName::Manifest))
    else {
        return Err(refused("missing_metadata", None));
    };
    let source = store::load_bytes::<ExportedMeta>(Path::new("backup.json"), backup)
        .map_err(|_| bad("metadata_unreadable"))?
        .value;
    let mut manifest = store::load_bytes::<Manifest>(Path::new("manifest.json"), manifest)
        .map_err(|_| bad("metadata_unreadable"))?
        .value;
    let addons = metas
        .get(&MetaName::Addons)
        .map(|bytes| {
            store::load_bytes::<AddonsFile>(Path::new("addons.json"), bytes)
                .map(|loaded| loaded.value)
                .map_err(|_| bad("metadata_unreadable"))
        })
        .transpose()?;
    let names = WtfPathRedactor::for_paths(entries.iter().map(|e| e.path.as_str()));
    check_tree(&entries, &names)?;
    check_modes(&entries, &names)?;
    check_bijection(&entries, &mut manifest, &names)?;
    check_links(&entries, &names)?;
    let count = |kind| entries.iter().filter(|e| e.kind == kind).count() as u64;
    Ok(Scanned {
        files: count(EntryKind::File),
        dirs: count(EntryKind::Dir),
        symlinks: count(EntryKind::Symlink),
        bytes: entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::File))
            .map(|e| e.size)
            .sum(),
        source,
        manifest,
        addons,
        entries,
    })
}

/// The archive's reader and size.
fn open(file: &Path) -> Result<(std::fs::File, u64)> {
    let fail = |e: &io::Error| {
        Error::io(
            file,
            e,
            Suggestion::text("check the file's path, or that it is readable"),
        )
    };
    let reader = std::fs::File::open(file).map_err(|e| fail(&e))?;
    let len = reader.metadata().map_err(|e| fail(&e))?.len();
    Ok((reader, len))
}

/// One payload or metadata entry, its records applied.
struct Item {
    name: Name,
    kind: EntryKind,
    size: u64,
    mode: u32,
    link: Option<String>,
}

/// Reads every entry of the archive, checking types, names, duplicates and bounds, and
/// hands each payload or metadata entry with its body to `visit`.
fn read_entries(
    compressed: impl Read,
    len: u64,
    visit: &mut dyn FnMut(Item, &mut dyn Read) -> Result<()>,
) -> Result<()> {
    let mut decoder = zstd::stream::read::Decoder::new(compressed).map_err(read_error)?;
    decoder
        .window_log_max(BACKUP_IMPORT_MAX_WINDOW_LOG)
        .map_err(read_error)?;
    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().map_err(read_error)?.raw(true);
    let mut count = 0u64;
    let mut expanded = 0u64;
    let mut folded: BTreeSet<String> = BTreeSet::new();
    let mut long_name: Option<Vec<u8>> = None;
    let mut long_link: Option<Vec<u8>> = None;
    for entry in entries {
        let mut entry = entry.map_err(read_error)?;
        count += 1;
        if count > BACKUP_IMPORT_MAX_ENTRIES {
            return Err(too_large(
                "BACKUP_IMPORT_MAX_ENTRIES",
                BACKUP_IMPORT_MAX_ENTRIES,
            ));
        }
        let header = entry.header().clone();
        let size = header.entry_size().map_err(read_error)?;
        expanded = expanded.saturating_add(size);
        if expanded > BACKUP_IMPORT_MAX_BYTES {
            return Err(too_large(
                "BACKUP_IMPORT_MAX_BYTES",
                BACKUP_IMPORT_MAX_BYTES,
            ));
        }
        if expanded > ARCHIVE_RATIO_FLOOR && expanded / len.max(1) > ARCHIVE_MAX_RATIO {
            return Err(too_large("ARCHIVE_MAX_RATIO", ARCHIVE_MAX_RATIO));
        }
        let kind = header.entry_type();
        let record = |entry: &mut dyn Read| -> Result<Vec<u8>> {
            if size > BACKUP_IMPORT_MAX_META_BYTES {
                return Err(too_large(
                    "BACKUP_IMPORT_MAX_META_BYTES",
                    BACKUP_IMPORT_MAX_META_BYTES,
                ));
            }
            let mut bytes = Vec::new();
            entry
                .take(size)
                .read_to_end(&mut bytes)
                .map_err(read_error)?;
            while bytes.last() == Some(&0) {
                bytes.pop();
            }
            Ok(bytes)
        };
        let entry_kind = match kind {
            tar::EntryType::GNULongName => {
                long_name = Some(record(&mut entry)?);
                continue;
            }
            tar::EntryType::GNULongLink => {
                long_link = Some(record(&mut entry)?);
                continue;
            }
            tar::EntryType::XHeader => {
                let pax = record(&mut entry)?;
                for (key, value) in pax_records(&pax)? {
                    match key {
                        "path" => long_name = Some(value.to_vec()),
                        "linkpath" => long_link = Some(value.to_vec()),
                        "size" => return Err(refused("unsupported_entry", None)),
                        _ if key.starts_with("GNU.sparse") => {
                            return Err(refused("unsupported_entry", None));
                        }
                        _ => {}
                    }
                }
                continue;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => EntryKind::File,
            tar::EntryType::Directory => EntryKind::Dir,
            tar::EntryType::Symlink => EntryKind::Symlink,
            _ => return Err(refused("unsupported_entry", None)),
        };
        let raw_name = long_name
            .take()
            .unwrap_or_else(|| header.path_bytes().into_owned());
        let raw_link = long_link
            .take()
            .or_else(|| header.link_name_bytes().map(std::borrow::Cow::into_owned));
        let name = name_of(&raw_name, entry_kind)?;
        if !folded.insert(fold(&name)) {
            return Err(match name {
                Name::Meta(_) => refused("duplicate_metadata", None),
                Name::Wtf(_) => refused("duplicate_name", None),
            });
        }
        let link = match entry_kind {
            EntryKind::Symlink => {
                let link = raw_link
                    .filter(|l| !l.is_empty() && l.len() as u64 <= BACKUP_IMPORT_MAX_PATH)
                    .and_then(|l| String::from_utf8(l).ok())
                    .filter(|l| !l.chars().any(char::is_control))
                    .ok_or_else(|| refused("symlink_escape", None))?;
                Some(link)
            }
            _ => None,
        };
        if !matches!(entry_kind, EntryKind::File) && size != 0 {
            return Err(refused("unsupported_entry", None));
        }
        if matches!(name, Name::Meta(_)) && !matches!(entry_kind, EntryKind::File) {
            return Err(refused("unsupported_entry", None));
        }
        let mode = header.mode().map_err(read_error)? & 0o7777;
        let item = Item {
            name,
            kind: entry_kind,
            size,
            mode,
            link,
        };
        visit(item, &mut entry)?;
    }
    if long_name.is_some() || long_link.is_some() {
        return Err(bad("truncated"));
    }
    Ok(())
}

/// A name's case-folded identity, for duplicates.
fn fold(name: &Name) -> String {
    match name {
        Name::Meta(m) => format!("{m:?}").to_lowercase(),
        Name::Wtf(path) => format!("wtf/{}", path.to_lowercase()),
    }
}

/// An entry's name, checked: a metadata name, or a plain relative path under `WTF/` (a
/// directory's trailing `/` dropped) of at most `BACKUP_IMPORT_MAX_PATH` bytes.
fn name_of(raw: &[u8], kind: EntryKind) -> Result<Name> {
    if raw.len() as u64 > BACKUP_IMPORT_MAX_PATH {
        return Err(too_large("BACKUP_IMPORT_MAX_PATH", BACKUP_IMPORT_MAX_PATH));
    }
    let bad_name = || refused("bad_name", None);
    let text = std::str::from_utf8(raw).map_err(|_| bad_name())?;
    let text = match kind {
        EntryKind::Dir => text.strip_suffix('/').unwrap_or(text),
        _ => text,
    };
    if let Some(meta) = MetaName::of(text) {
        return Ok(Name::Meta(meta));
    }
    let rel = text.strip_prefix("WTF/").ok_or_else(bad_name)?;
    if !plain_path(rel) {
        return Err(bad_name());
    }
    Ok(Name::Wtf(rel.to_owned()))
}

/// A non-empty relative path whose components are all plain names: no empty, `.` or
/// `..` component, no backslash, colon (a drive letter) or control character.
fn plain_path(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && rel.split('/').all(|c| {
            !c.is_empty()
                && c != "."
                && c != ".."
                && !c.contains(['\\', ':'])
                && !c.chars().any(char::is_control)
        })
}

/// PAX records, `"<len> <key>=<value>\n"` each.
fn pax_records(body: &[u8]) -> Result<Vec<(&str, &[u8])>> {
    let mut out = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let malformed = || refused("unsupported_entry", None);
        let space = rest.iter().position(|b| *b == b' ').ok_or_else(malformed)?;
        let len: usize = std::str::from_utf8(&rest[..space])
            .ok()
            .and_then(|n| n.parse().ok())
            .filter(|n| *n > space + 1 && *n <= rest.len())
            .ok_or_else(malformed)?;
        let record = &rest[space + 1..len];
        let record = record.strip_suffix(b"\n").ok_or_else(malformed)?;
        let eq = record
            .iter()
            .position(|b| *b == b'=')
            .ok_or_else(malformed)?;
        let key = std::str::from_utf8(&record[..eq]).map_err(|_| malformed())?;
        out.push((key, &record[eq + 1..]));
        rest = &rest[len..];
    }
    Ok(out)
}

/// Every entry's parent is a directory of the archive: nothing is written through a link
/// or into a file. A parent that is a symlink is `symlink_escape`; one missing or a file,
/// `manifest_mismatch`.
fn check_tree(entries: &[Payload], names: &WtfPathRedactor) -> Result<()> {
    let kinds: BTreeMap<&str, EntryKind> =
        entries.iter().map(|e| (e.path.as_str(), e.kind)).collect();
    for e in entries {
        let Some((parent, _)) = e.path.rsplit_once('/') else {
            continue;
        };
        match kinds.get(parent) {
            Some(EntryKind::Dir) => {}
            Some(EntryKind::Symlink) => {
                return Err(refused("symlink_escape", Some((&e.path, names))));
            }
            _ => return Err(refused("manifest_mismatch", Some((&e.path, names)))),
        }
    }
    Ok(())
}

/// No regular file carries the setuid or setgid bit: no `WTF/` file needs one, and the
/// import would write it as it is, into the backup and, restored, into `WTF/`. A
/// directory keeps every bit (setgid is a group-shared folder's), and the sticky bit
/// means nothing on a file. Decided on the first read, before a byte is written.
fn check_modes(entries: &[Payload], names: &WtfPathRedactor) -> Result<()> {
    match entries
        .iter()
        .find(|e| e.kind == EntryKind::File && e.mode & 0o6000 != 0)
    {
        Some(e) => Err(special_mode(e, names)),
        None => Ok(()),
    }
}

/// `unsafe_archive` with `details.reason: "special_mode"`: which bit file `e` carries,
/// its path as every refusal shows one, and how to clear it at the source.
fn special_mode(e: &Payload, names: &WtfPathRedactor) -> Error {
    let bit = match (e.mode & 0o4000 != 0, e.mode & 0o2000 != 0) {
        (true, true) => "setuid and setgid bits",
        (true, false) => "setuid bit",
        _ => "setgid bit",
    };
    let path = shown(&e.path, names);
    let mut message = format!(
        "the archive is not an exact omaddons export: it holds a file with the {bit} (mode {:04o}), which no WTF/ file needs",
        e.mode
    );
    if let Some(path) = &path {
        message.push_str(&format!(" ({path})"));
    }
    Error::new(
        ErrorCode::UnsafeArchive,
        message,
        Suggestion::text(format!(
            "clear the {bit} on that file in the client's WTF/ (chmod u-s,g-s), then make and export a new backup"
        )),
    )
    .details(serde_json::json!({ "reason": "special_mode", "path": path }))
}

/// The payload and the manifest name the same entries, each with the same kind, size
/// (files), mode and link target, and every file the hash it was just read with. The
/// manifest comes back in path order.
fn check_bijection(
    entries: &[Payload],
    manifest: &mut Manifest,
    names: &WtfPathRedactor,
) -> Result<()> {
    let mismatch = |path: &str| refused("manifest_mismatch", Some((path, names)));
    let mut listed: BTreeMap<&str, &ManifestEntry> = BTreeMap::new();
    for m in &manifest.entries {
        if listed.insert(m.path.as_str(), m).is_some() {
            return Err(mismatch(&m.path));
        }
    }
    if listed.len() != entries.len() {
        let extra = entries
            .iter()
            .find(|e| !listed.contains_key(e.path.as_str()))
            .map(|e| e.path.as_str())
            .or_else(|| {
                let paid: BTreeSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
                listed.keys().copied().find(|p| !paid.contains(p))
            })
            .unwrap_or_default();
        return Err(mismatch(extra));
    }
    for e in entries {
        let Some(m) = listed.get(e.path.as_str()) else {
            return Err(mismatch(&e.path));
        };
        if !describes(m, e) {
            return Err(mismatch(&e.path));
        }
    }
    manifest
        .entries
        .sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(())
}

/// Whether manifest entry `m` describes payload entry `e` exactly: kind, mode and link
/// target, and for a file its size and the hash it was read with.
fn describes(m: &ManifestEntry, e: &Payload) -> bool {
    // ubs:ignore — comparing entry metadata, not secrets
    let fields = (m.kind, m.mode & 0o7777, &m.link) == (e.kind, e.mode, &e.link);
    let content = match e.kind {
        // ubs:ignore — comparing file content hashes, not secrets
        EntryKind::File => m.size == e.size && m.sha256.is_some() && m.sha256 == e.sha256,
        EntryKind::Dir | EntryKind::Symlink => m.sha256.is_none(),
    };
    fields && content
}

/// Every symlink's target is relative, resolves from its own directory without leaving
/// `WTF/` or passing through another link's parent, and the links' graph has no cycle (a
/// link to its own ancestor is one too). Otherwise `symlink_escape`, naming the link.
fn check_links(entries: &[Payload], names: &WtfPathRedactor) -> Result<()> {
    let links: BTreeMap<&str, &str> = entries
        .iter()
        .filter_map(|e| Some((e.path.as_str(), e.link.as_deref()?)))
        .collect();
    for &path in links.keys() {
        let escape = || refused("symlink_escape", Some((path, names)));
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut at = path.to_owned();
        loop {
            if !seen.insert(at.clone()) {
                return Err(escape());
            }
            let Some(target) = links.get(at.as_str()) else {
                break;
            };
            let resolved = resolve(&at, target, &links).ok_or_else(escape)?;
            // A link to an ancestor of itself makes a directory cycle.
            if resolved.is_empty() || at.starts_with(&format!("{resolved}/")) {
                return Err(escape());
            }
            at = resolved;
        }
    }
    Ok(())
}

/// `target`, a link at `link`, resolved lexically from the link's directory: `None` when
/// it is absolute, leaves `WTF/`, or passes through another link on the way.
fn resolve(link: &str, target: &str, links: &BTreeMap<&str, &str>) -> Option<String> {
    if target.starts_with('/') || target.contains(['\\', ':']) {
        return None;
    }
    let mut parts: Vec<&str> = link.split('/').collect();
    parts.pop();
    let steps: Vec<&str> = target.split('/').filter(|c| !c.is_empty()).collect();
    for (i, step) in steps.iter().enumerate() {
        match *step {
            "." => {}
            ".." => {
                parts.pop()?;
            }
            name => {
                parts.push(name);
                let last = i + 1 == steps.len();
                if !last && links.contains_key(parts.join("/").as_str()) {
                    return None;
                }
            }
        }
    }
    Some(parts.join("/"))
}

/// What to import, and as whom.
#[derive(Debug, Clone)]
pub struct ImportRequest<'a> {
    /// A registered target: its key goes in `backup.json`.
    pub target: &'a Target,
    pub actor: Actor,
    pub reason: Option<String>,
    /// The command the inventory projection's write rings the doorbell for.
    pub command: &'a str,
}

/// Pass 2: stages the archive at `file`, found exact by [`scan`] as `scanned`, checks it
/// again as it is written, and publishes it as a new backup of `req.target`.
pub fn import(
    paths: &Paths,
    scopes: &ScopeTable,
    file: &Path,
    scanned: &Scanned,
    req: &ImportRequest<'_>,
    progress: &ProgressSink,
) -> Result<Created> {
    let key = req.target.key.ok_or_else(|| {
        Error::internal(format!(
            "import into {} before it was registered",
            req.target.id
        ))
    })?;
    let client = ClientPaths::new(&req.target.client_dir);
    let tx = TxId::new();
    let inflight = InflightLock::create(paths, &tx)?;
    let result = (|| {
        let staged = stage(scopes, file, scanned, &client, &tx, progress)?;
        publish(paths, scopes, scanned, req, &client, key, &staged, tx)
    })();
    create::cleanup(paths, scopes, &client, &tx, inflight);
    result
}

/// Pass 2 into `<meta>/stage/<tx>/backup/WTF`; returns `<stage>/backup`.
fn stage(
    scopes: &ScopeTable,
    file: &Path,
    scanned: &Scanned,
    client: &ClientPaths,
    tx: &TxId,
    progress: &ProgressSink,
) -> Result<PathBuf> {
    let dir = client.stage(tx).join("backup");
    let wtf = dir.join("WTF");
    let mut s = WriteSession::untracked(Writer::Backup, scopes, "backup import");
    ensure_dir_all(&mut s, &wtf)?;
    let deferred = || MutateOpts {
        durability: Durability::Deferred,
        ..MutateOpts::default()
    };
    let names = WtfPathRedactor::for_paths(scanned.entries.iter().map(|e| e.path.as_str()));
    let changed = |path: &str| {
        Error::new(
            ErrorCode::BadArchive,
            "the archive changed while it was imported",
            Suggestion::text("import it again once nothing else writes it"),
        )
        .details(serde_json::json!({ "reason": "changed", "path": shown(path, &names) }))
    };
    // An fsx error names the absolute path, account, realm and character included: a
    // write below `WTF/` fails naming its path only through the redactor.
    let failed = |path: &str, e: fsx::FsxError| {
        let cause = match &e {
            fsx::FsxError::Io { source, .. } => super::tree::io_cause(source),
            _ => "the filesystem writer refused the path".to_owned(),
        };
        Error::new(
            ErrorCode::Io,
            format!("staging the import failed: {cause}"),
            Suggestion::text("free some space or check permissions, then import again"),
        )
        .details(serde_json::json!({ "path": shown(path, &names) }))
    };
    let (reader, len) = open(file)?;
    let mut at = 0usize;
    let mut links: Vec<&Payload> = Vec::new();
    let mut files = 0u64;
    read_entries(reader, len, &mut |item, body| {
        let Name::Wtf(path) = item.name else {
            return io::copy(body, &mut io::sink())
                .map(drop)
                .map_err(read_error);
        };
        let expected = scanned
            .entries
            .get(at)
            .filter(|e| e.path == path && e.kind == item.kind && e.size == item.size)
            .ok_or_else(|| changed(&path))?;
        at += 1;
        let target = wtf.join(&path);
        match item.kind {
            EntryKind::Dir => {
                mutate_with(&mut s, &target, Op::CreateDir, deferred())
                    .map_err(|e| failed(&path, e))?;
            }
            EntryKind::Symlink => links.push(expected),
            EntryKind::File => {
                let mut hashed = Hashing::new(body.take(item.size));
                mutate_with(
                    &mut s,
                    &target,
                    Op::WriteFile {
                        content: Content::Reader(&mut hashed),
                        mode: Some(expected.mode),
                    },
                    deferred(),
                )
                .map_err(|e| failed(&path, e))?;
                let (read, sha256) = hashed.finish();
                if read != item.size || Some(&sha256) != expected.sha256.as_ref() {
                    return Err(changed(&path));
                }
                files += 1;
                if files.is_multiple_of(PROGRESS_EVERY) {
                    progress.count(Phase::Copy, files, scanned.files, IMPORT_LABEL);
                }
            }
        }
        Ok(())
    })?;
    if at != scanned.entries.len() {
        return Err(changed(""));
    }
    progress.count(Phase::Copy, files, scanned.files, IMPORT_LABEL);
    // Links last: nothing was written through one.
    for link in links {
        let target = PathBuf::from(link.link.clone().unwrap_or_default());
        mutate(
            &mut s,
            &wtf.join(&link.path),
            Op::SymlinkAtomic { target: &target },
        )
        .map_err(|e| failed(&link.path, e))?;
    }
    // Directory modes last, deepest first: stored owner-writable, as every backup's.
    for e in scanned
        .entries
        .iter()
        .rev()
        .filter(|e| matches!(e.kind, EntryKind::Dir))
    {
        mutate(
            &mut s,
            &wtf.join(&e.path),
            Op::Chmod {
                mode: (e.mode & 0o777) | 0o700,
            },
        )
        .map_err(|err| failed(&e.path, err))?;
    }
    fsx::sync_filesystem(&dir)?;
    Ok(dir)
}

/// Describes the staged import and renames it to `<meta>/backups/<id>` (`0700`), then
/// records it in the inventory projection.
#[allow(clippy::too_many_arguments)]
fn publish(
    paths: &Paths,
    scopes: &ScopeTable,
    scanned: &Scanned,
    req: &ImportRequest<'_>,
    client: &ClientPaths,
    key: crate::model::TargetKey,
    staged: &Path,
    tx: TxId,
) -> Result<Created> {
    let source = &scanned.source.meta;
    let now = crate::clock::now();
    let id = BackupId::new();
    let meta = BackupMeta {
        id,
        target: key,
        target_id: req.target.id.clone(),
        family: req.target.family,
        created_at: now,
        captured_at: source.captured_at,
        kind: BackupKind::Imported,
        trigger: None,
        actor: req.actor,
        reason: req.reason.clone(),
        client_version: source.client_version.clone(),
        client_running: false,
        files: scanned.files,
        dirs: scanned.dirs,
        symlinks: scanned.symlinks,
        skipped: 0,
        bytes: scanned.bytes,
        reflinked: Reflinked::None,
        unsettled: 0,
        consistency: source.consistency,
        // Every hash was just checked (M3-D47).
        integrity: Integrity::Verified,
        verified_at: Some(now),
        baseline: Some(Baseline::Import),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        source: Some(BackupSource::Import(ImportSource {
            id: source.id,
            target_id: source.target_id.clone(),
            family: source.family,
            created_at: source.created_at,
            captured_at: source.captured_at,
            tool_version: source.tool_version.clone(),
        })),
        partial: source.partial,
        wtf_outside_client: false,
    };
    let mut s = WriteSession::untracked(Writer::Backup, scopes, "backup import");
    create::write(&mut s, &staged.join("manifest.json"), &scanned.manifest)?;
    if let Some(addons) = &scanned.addons {
        create::write(&mut s, &staged.join("addons.json"), addons)?;
    }
    create::write(&mut s, &super::meta_file(staged), &meta)?;
    let backups = client.backups();
    ensure_dir_all(&mut s, &backups)?;
    mutate(&mut s, &backups, Op::Chmod { mode: DIR_MODE })?;
    mutate(&mut s, staged, Op::Chmod { mode: DIR_MODE })?;
    let dir = super::backup_dir(client, &id);
    create::rename_new(&mut s, staged, &dir)?;
    inventory::record(
        paths,
        client,
        &key,
        Change::Added(Box::new(meta.clone())),
        Ring {
            command: req.command,
            actor: req.actor,
        },
    );
    Ok(Created {
        meta,
        dir,
        tx,
        warnings: Vec::new(),
    })
}

/// A path for a message: only below `Account/`, and of it only what cannot name an
/// account, realm or character, since a hostile archive's names can be anything: each
/// component the redactor replaced, `Account`, `SavedVariables` and the file in it (an
/// addon's name); every other component becomes `…`.
fn shown(path: &str, names: &WtfPathRedactor) -> Option<String> {
    if !path.starts_with("Account/") {
        return None;
    }
    let redacted = names.rel(path);
    let original: Vec<&str> = path.split('/').collect();
    let replaced: Vec<&str> = redacted.split('/').collect();
    if original.len() != replaced.len() {
        return Some("Account/…".to_owned());
    }
    let last = original.len() - 1;
    let parts: Vec<&str> = original
        .iter()
        .zip(&replaced)
        .enumerate()
        .map(|(i, (o, r))| {
            let saved_variables_file = i == last && i > 0 && original[i - 1] == "SavedVariables";
            if o != r || *o == "Account" || *o == "SavedVariables" || saved_variables_file {
                *r
            } else {
                "…"
            }
        })
        .collect();
    Some(parts.join("/"))
}

/// `unsafe_archive` with `details.reason`, naming the offending path when it is below
/// `Account/` (redacted).
fn refused(reason: &str, path: Option<(&str, &WtfPathRedactor)>) -> Error {
    let path = path.and_then(|(p, names)| shown(p, names));
    let what = match reason {
        "unsupported_entry" => {
            "an entry of a kind an export never holds (a hard link, a device, a sparse or oversized record)"
        }
        "bad_name" => "an entry whose name is not a plain path under WTF/",
        "duplicate_name" => "two entries of one name (ignoring case)",
        "duplicate_metadata" => "two copies of one of its description files",
        "missing_metadata" => "no backup.json or manifest.json",
        "manifest_mismatch" => "entries that differ from its manifest",
        "symlink_escape" => "a symlink that leaves WTF/, passes through another link, or loops",
        _ => "an entry it may not hold",
    };
    let mut message = format!("the archive is not an exact omaddons export: it holds {what}");
    if let Some(path) = &path {
        message.push_str(&format!(" ({path})"));
    }
    Error::new(
        ErrorCode::UnsafeArchive,
        message,
        Suggestion::text(UNMODIFIED),
    )
    .details(serde_json::json!({ "reason": reason, "path": path }))
}

/// Every refusal's fix: prose, naming no command.
const UNMODIFIED: &str = "use the file a backup export wrote, unmodified";

/// `archive_too_large` past the bound `max` of the constant `name`.
fn too_large(name: &str, max: u64) -> Error {
    Error::new(
        ErrorCode::ArchiveTooLarge,
        format!("the archive is larger than {name} allows ({max})"),
        Suggestion::text(UNMODIFIED),
    )
    .details(serde_json::json!({ "limit": max, "constant": name }))
}

/// `bad_archive` with `details.reason`: a corrupt or truncated stream, a decoder window
/// past the bound, description files that do not read.
fn bad(reason: &str) -> Error {
    let what = match reason {
        "decoder_window" => {
            "it asks for a larger decompression window than an export uses".to_owned()
        }
        "truncated" => "it ends early".to_owned(),
        "metadata_unreadable" => "its description files do not read".to_owned(),
        _ => "it is not a readable .tar.zst".to_owned(),
    };
    Error::new(
        ErrorCode::BadArchive,
        format!("the archive cannot be imported: {what}"),
        Suggestion::text(UNMODIFIED),
    )
    .details(serde_json::json!({ "reason": reason }))
}

/// A read error of the stream: the decoder's window bound, an early end, or corruption.
fn read_error(e: io::Error) -> Error {
    let text = e.to_string().to_ascii_lowercase();
    if text.contains("too much memory") || text.contains("window") {
        bad("decoder_window")
    } else if e.kind() == io::ErrorKind::UnexpectedEof {
        bad("truncated")
    } else {
        bad("corrupt")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(path: &str, kind: EntryKind, link: Option<&str>) -> Payload {
        Payload {
            path: path.into(),
            kind,
            size: 0,
            mode: 0o755,
            link: link.map(str::to_owned),
            sha256: None,
        }
    }

    fn reason(e: &Error) -> String {
        e.details["reason"].as_str().unwrap_or_default().to_owned()
    }

    #[test]
    fn names_are_plain_paths_under_wtf_or_metadata() {
        for (raw, kind, ok) in [
            ("backup.json", EntryKind::File, true),
            ("WTF/Config.wtf", EntryKind::File, true),
            ("WTF/Account/A/", EntryKind::Dir, true),
            ("WTF/", EntryKind::Dir, false),
            ("WTF/../etc/passwd", EntryKind::File, false),
            ("WTF/a/./b", EntryKind::File, false),
            ("/etc/passwd", EntryKind::File, false),
            ("WTF//a", EntryKind::File, false),
            ("WTF/C:/x", EntryKind::File, false),
            ("WTF/a\\b", EntryKind::File, false),
            ("WTF/a\nb", EntryKind::File, false),
            ("other.json", EntryKind::File, false),
            ("wtf/Config.wtf", EntryKind::File, false),
        ] {
            assert_eq!(name_of(raw.as_bytes(), kind).is_ok(), ok, "{raw:?}");
        }
        let long = format!("WTF/{}", "a".repeat(BACKUP_IMPORT_MAX_PATH as usize));
        assert_eq!(
            name_of(long.as_bytes(), EntryKind::File).unwrap_err().code,
            ErrorCode::ArchiveTooLarge
        );
        assert!(name_of(&[b'W', b'T', b'F', b'/', 0xFF], EntryKind::File).is_err());
    }

    #[test]
    fn messages_show_no_name_an_archive_could_have_planted() {
        let paths = [
            "Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/Foo.lua",
            "Account/FAKEACCT/Escape/FakeChar.lua",
            "Account/FAKEACCT/Fake.Realm/x",
        ];
        let names = WtfPathRedactor::for_paths(paths.iter().copied());
        let sv = shown(paths[0], &names).unwrap();
        assert!(
            sv.starts_with("Account/account 1/realm ")
                && sv.ends_with("/character 1/SavedVariables/Foo.lua"),
            "{sv}"
        );
        for path in &paths[1..] {
            let text = shown(path, &names).unwrap();
            assert!(!text.contains("Fake") && !text.contains("FAKE"), "{text}");
        }
        assert_eq!(shown("FakeChar/x.lua", &names), None, "outside Account/");
    }

    #[test]
    fn pax_records_parse_and_refuse_malformed_ones() {
        let body = b"20 path=WTF/a/b.lua\n";
        assert_eq!(
            pax_records(body).unwrap(),
            [("path", b"WTF/a/b.lua".as_slice())]
        );
        for bad in [
            &b"99 path=x\n"[..],
            b"x path=y\n",
            b"9 path=x",
            b"8 pathx\n",
        ] {
            assert_eq!(reason(&pax_records(bad).unwrap_err()), "unsupported_entry");
        }
    }

    #[test]
    fn symlinks_stay_inside_through_no_link_and_without_cycles() {
        let tree = |links: &[(&str, &str)]| {
            let mut entries = vec![
                payload("Account", EntryKind::Dir, None),
                payload("Account/A", EntryKind::Dir, None),
                payload("Account/B", EntryKind::Dir, None),
            ];
            for (path, target) in links {
                entries.push(payload(path, EntryKind::Symlink, Some(target)));
            }
            entries
        };
        let names = WtfPathRedactor::for_paths(std::iter::empty());
        let ok = |links: &[(&str, &str)]| check_links(&tree(links), &names).is_ok();
        assert!(ok(&[("Account/A/l", "../B/x.lua")]));
        assert!(
            ok(&[("Account/A/l", "m"), ("Account/A/m", "../B")]),
            "a chain inside"
        );
        assert!(!ok(&[("Account/A/l", "/etc/passwd")]));
        assert!(!ok(&[("Account/A/l", "../../../x")]));
        assert!(!ok(&[
            ("Account/A/l", "m"),
            ("Account/A/m", "../../../../x")
        ]));
        assert!(
            !ok(&[("Account/A/l", "m"), ("Account/A/m", "l")]),
            "a cycle"
        );
        assert!(!ok(&[("Account/A/l", ".")]), "its own directory");
        assert!(!ok(&[("Account/A/l", "../..")]), "the root");
        assert!(
            !ok(&[("Account/A/l", "../B"), ("Account/A/m", "l/x")]),
            "through another link"
        );
    }

    #[test]
    fn parents_are_directories_of_the_archive() {
        let names = WtfPathRedactor::for_paths(std::iter::empty());
        let mut entries = vec![
            payload("Account", EntryKind::Dir, None),
            payload("Account/l", EntryKind::Symlink, Some("..")),
            payload("Account/l/x.lua", EntryKind::File, None),
        ];
        assert_eq!(
            reason(&check_tree(&entries, &names).unwrap_err()),
            "symlink_escape"
        );
        entries[1] = payload("Account/l", EntryKind::File, None);
        assert_eq!(
            reason(&check_tree(&entries, &names).unwrap_err()),
            "manifest_mismatch"
        );
        entries.remove(1);
        assert_eq!(
            reason(&check_tree(&entries, &names).unwrap_err()),
            "manifest_mismatch"
        );
    }

    #[test]
    fn the_payload_and_manifest_are_one_set() {
        let names = WtfPathRedactor::for_paths(std::iter::empty());
        let file = Payload {
            path: "Config.wtf".into(),
            kind: EntryKind::File,
            size: 3,
            mode: 0o644,
            link: None,
            sha256: Some("aa".into()),
        };
        let entry = |f: &Payload| ManifestEntry {
            path: f.path.clone(),
            kind: f.kind,
            size: f.size,
            mode: f.mode,
            mtime_ns: 0,
            link: f.link.clone(),
            sha256: f.sha256.clone(),
        };
        let check = |payload: &[Payload], listed: Vec<ManifestEntry>| {
            check_bijection(payload, &mut Manifest { entries: listed }, &names)
        };
        assert!(check(std::slice::from_ref(&file), vec![entry(&file)]).is_ok());
        let mut other = entry(&file);
        for change in [
            |m: &mut ManifestEntry| m.size = 4,
            |m: &mut ManifestEntry| m.mode = 0o600,
            |m: &mut ManifestEntry| m.sha256 = Some("bb".into()),
            |m: &mut ManifestEntry| m.sha256 = None,
            |m: &mut ManifestEntry| m.kind = EntryKind::Dir,
        ] {
            let mut m = entry(&file);
            change(&mut m);
            let err = check(std::slice::from_ref(&file), vec![m]).unwrap_err();
            assert_eq!(reason(&err), "manifest_mismatch");
        }
        other.path = "Other.wtf".into();
        assert!(check(std::slice::from_ref(&file), vec![entry(&file), other]).is_err());
        assert!(check(std::slice::from_ref(&file), Vec::new()).is_err());
        assert!(
            check(
                &[file.clone(), file.clone()],
                vec![entry(&file), entry(&file)]
            )
            .is_err()
        );
    }
}
