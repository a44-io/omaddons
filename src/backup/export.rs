//! `backup export` (M3-D34): one backup as a deterministic `tar`, streamed through `zstd`
//! (level 3) straight into the destination, never through `<cache>`. The producer runs on
//! a scoped thread and hands the compressed bytes over a bounded channel to fsx, which
//! writes them into its hidden temp sibling of the destination and publishes it only if
//! nothing is there (`file_exists`), so the destination is never half-written and never
//! replaced. Memory stays flat for any backup size.
//!
//! The archive holds, in this order: `backup.json` (the backup's description with
//! `verified_against`, and no export time, so one backup always exports to the same
//! bytes); `addons.json` when the backup has one; every `WTF/…` entry in path order, with
//! the mode and mtime its manifest recorded, uid and gid 0 and no owner names, symlinks as
//! symlinks; and `manifest.json` last, with the SHA-256 of every file as it streamed. A
//! file the local manifest has a hash for is checked against it (M3-D47): a mismatch is
//! `hash_mismatch`, a backup damaged on disk. The backup is held with a reader
//! reservation for the whole stream (M3-D48), so a prune reports it busy. Progress is the
//! `copy` phase under a fixed label, never a path (M3-D21).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{BackupMeta, BackupRef, Baseline, EntryKind, Manifest, ManifestEntry};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{
    Content, DirLock, Expect, FsxError, MutateOpts, Op, ScopeTable, WriteSession, Writer,
    mutate_with,
};
use crate::output::{Phase, ProgressSink};
use crate::privacy::WtfPathRedactor;
use crate::state::store::{self, SchemaVersion, Versioned};

/// The progress label: never a path, which would name an account (M3-D21).
pub const EXPORT_LABEL: &str = "exporting";
/// The zstd level (M3-D36).
const LEVEL: i32 = 3;
/// Compressed chunks in flight between the producer and the write.
const CHANNEL_DEPTH: usize = 8;
/// The size of one chunk handed over.
const CHUNK: usize = 256 * 1024;
/// The archive's mode.
const ARCHIVE_MODE: u32 = 0o600;
/// Report progress every this many files.
const PROGRESS_EVERY: u64 = 64;

/// Where an archive's hashes come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedAgainst {
    /// The backup's manifest held hashes from its capture (`create --verify`).
    Capture,
    /// … from the export it was imported from.
    Import,
    /// … from a later `backup verify`.
    Later,
    /// The manifest held none: the archive's are computed as it streams.
    ComputedAtExport,
}

/// An archive's `backup.json`: the backup's description, and where the archive's hashes
/// come from. It holds no export time, so exports are deterministic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportedMeta {
    #[serde(flatten)]
    pub meta: BackupMeta,
    pub verified_against: VerifiedAgainst,
}

impl Versioned for ExportedMeta {
    const KIND: &'static str = "backup_export";
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 0);
}

/// What an export streams, read before anything is written.
#[derive(Debug, Clone)]
pub struct ExportPlan {
    pub meta: BackupMeta,
    pub manifest: Manifest,
    /// `addons.json`, as the backup holds it.
    pub addons: Option<Vec<u8>>,
    pub verified_against: VerifiedAgainst,
    /// Regular files, and their apparent bytes.
    pub files: u64,
    pub bytes: u64,
}

/// What an export wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub file: PathBuf,
    pub files: u64,
    pub bytes: u64,
    /// The archive's size, and its SHA-256 as it was written.
    pub compressed_bytes: u64,
    pub sha256: String,
    pub verified_against: VerifiedAgainst,
}

/// The backup's reader reservation (M3-D48): a backup moved or deleted under it is
/// `not_found`.
pub fn reserve(backup: &BackupRef) -> Result<DirLock> {
    let gone = || {
        Error::new(
            ErrorCode::NotFound,
            format!("backup {} is gone", backup.id),
            Suggestion::command(["omaddons", "backup", "list"]),
        )
        .details(serde_json::json!({ "backup": backup.id }))
    };
    let lock = DirLock::shared(&backup.dir).map_err(|_| gone())?;
    if !lock.same_dir(&backup.dir) {
        return Err(gone());
    }
    Ok(lock)
}

/// What exporting `backup` streams: its description, manifest and `addons.json`, and
/// where the archive's hashes will come from. A backup whose description or manifest
/// does not read cannot be exported.
pub fn plan(backup: &BackupRef) -> Result<ExportPlan> {
    let unreadable = || {
        Error::new(
            ErrorCode::StateCorrupt,
            format!(
                "backup {} cannot be exported: its description or manifest does not read",
                backup.id
            ),
            Suggestion::command(["omaddons", "backup", "list"]),
        )
        .details(serde_json::json!({ "backup": backup.id }))
    };
    let meta = backup.meta.clone().ok_or_else(unreadable)?;
    let mut manifest = store::load::<Manifest>(&backup.dir.join("manifest.json"))
        .ok()
        .flatten()
        .ok_or_else(unreadable)?
        .value;
    manifest
        .entries
        .sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    let addons = crate::discover::read_bytes(&backup.dir.join("addons.json"));
    let files: Vec<&ManifestEntry> = manifest
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .collect();
    let hashed = !files.is_empty() && files.iter().all(|e| e.sha256.is_some());
    let verified_against = match (hashed, meta.baseline) {
        (true, Some(Baseline::Capture)) => VerifiedAgainst::Capture,
        (true, Some(Baseline::Import)) => VerifiedAgainst::Import,
        (true, Some(Baseline::Later)) => VerifiedAgainst::Later,
        _ => VerifiedAgainst::ComputedAtExport,
    };
    Ok(ExportPlan {
        files: files.len() as u64,
        bytes: files.iter().map(|e| e.size).sum(),
        meta,
        manifest,
        addons,
        verified_against,
    })
}

/// Streams `backup` (reserved by the caller with [`reserve`]) as planned into `file`, a
/// path `scopes` admits ([`ScopeTable::with_export_file`]).
pub fn export(
    scopes: &ScopeTable,
    backup: &BackupRef,
    plan: &ExportPlan,
    file: &Path,
    (names, progress): (&WtfPathRedactor, &ProgressSink),
) -> Result<Exported> {
    if std::fs::symlink_metadata(file).is_ok() {
        return Err(file_exists(file));
    }
    let (tx, rx) = sync_channel::<Chunk>(CHANNEL_DEPTH);
    let (written, produced, reader) = std::thread::scope(|scope| {
        let producer = scope.spawn(move || produce(backup, plan, tx, (names, progress)));
        let mut reader = ChannelReader::new(rx);
        let mut s = WriteSession::untracked(Writer::Backup, scopes, "backup export");
        let written = mutate_with(
            &mut s,
            file,
            Op::WriteFile {
                content: Content::Reader(&mut reader),
                mode: Some(ARCHIVE_MODE),
            },
            MutateOpts {
                expect: Some(Expect::Absent),
                ..MutateOpts::default()
            },
        );
        // Unblocks a producer still sending after a failed write.
        let reader = reader.finish();
        let produced = producer
            .join()
            .unwrap_or(Err(Produced::Failed(Error::internal(
                "the export's producer stopped",
            ))));
        (written, produced, reader)
    });
    match (written, produced) {
        (Ok(_), Ok(())) => Ok(Exported {
            file: file.to_path_buf(),
            files: plan.files,
            bytes: plan.bytes,
            compressed_bytes: reader.len,
            sha256: reader.sha256,
            verified_against: plan.verified_against,
        }),
        // What the backup did wrong comes first: the write failed because of it.
        (_, Err(Produced::Failed(error))) => Err(error),
        (Err(FsxError::Precondition(_)), _) => Err(file_exists(file)),
        (Err(error), _) => Err(error.into()),
        (Ok(_), Err(Produced::Abandoned)) => Err(Error::internal(
            "the export was written while its producer was abandoned",
        )),
    }
}

/// `file_exists`: the destination is never replaced.
fn file_exists(file: &Path) -> Error {
    Error::new(
        ErrorCode::FileExists,
        format!("{} exists; an export never replaces a file", file.display()),
        Suggestion::text("choose another file name, or delete that file yourself"),
    )
    .details(serde_json::json!({ "file": file.display().to_string() }))
}

/// What travels from the producer to the write.
enum Chunk {
    Data(Vec<u8>),
    /// The archive is complete. A channel that closes without it is a failed export,
    /// never a short archive.
    End,
}

/// Why the producer stopped early.
enum Produced {
    /// The backup or its reading failed: this error is the export's.
    Failed(Error),
    /// The write stopped first (its error is the export's).
    Abandoned,
}

/// The producer: the archive, tar into zstd into the channel.
fn produce(
    backup: &BackupRef,
    plan: &ExportPlan,
    tx: SyncSender<Chunk>,
    (names, progress): (&WtfPathRedactor, &ProgressSink),
) -> std::result::Result<(), Produced> {
    let sink = ChannelWriter {
        tx: &tx,
        buf: Vec::with_capacity(CHUNK),
    };
    let abandoned = |_: io::Error| Produced::Abandoned;
    let encoder = zstd::stream::write::Encoder::new(sink, LEVEL).map_err(abandoned)?;
    let mut tar = tar::Builder::new(encoder);
    let at = u64::try_from(plan.meta.captured_at.as_second()).unwrap_or(0);

    let described = store::encode(&ExportedMeta {
        meta: plan.meta.clone(),
        verified_against: plan.verified_against,
    })
    .map_err(Produced::Failed)?;
    append_bytes(&mut tar, "backup.json", &described, at).map_err(abandoned)?;
    if let Some(addons) = &plan.addons {
        append_bytes(&mut tar, "addons.json", addons, at).map_err(abandoned)?;
    }

    let wtf = backup.wtf();
    let mut entries = plan.manifest.entries.clone();
    let mut done = 0u64;
    for entry in &mut entries {
        let path = format!("WTF/{}", entry.path);
        let mut header = tar::Header::new_gnu();
        header.set_mode(entry.mode & 0o7777);
        header.set_mtime(u64::try_from(entry.mtime_ns.div_euclid(1_000_000_000)).unwrap_or(0));
        header.set_uid(0);
        header.set_gid(0);
        match entry.kind {
            EntryKind::Dir => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                tar.append_data(&mut header, format!("{path}/"), io::empty())
                    .map_err(abandoned)?;
            }
            EntryKind::Symlink => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                let link = entry.link.clone().unwrap_or_default();
                tar.append_link(&mut header, &path, &link)
                    .map_err(abandoned)?;
            }
            EntryKind::File => {
                let source = wtf.join(&entry.path);
                let changed = || Produced::Failed(changed(backup, entry, names));
                let file = std::fs::File::open(&source).map_err(|_| changed())?;
                let len = file.metadata().map_err(|_| changed())?.len();
                if len != entry.size {
                    return Err(changed());
                }
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(entry.size);
                let mut hashed = Hashing::new(file.take(entry.size));
                tar.append_data(&mut header, &path, &mut hashed)
                    .map_err(abandoned)?;
                let (read, sha256) = hashed.finish();
                if read != entry.size {
                    return Err(changed());
                }
                // Ignoring case, as verify does: a manifest's hex may be either.
                // ubs:ignore — comparing a file's content hash, not a secret
                if entry
                    .sha256
                    .as_ref()
                    .is_some_and(|recorded| !recorded.eq_ignore_ascii_case(&sha256))
                {
                    return Err(Produced::Failed(damaged(backup, entry, names)));
                }
                entry.sha256 = Some(sha256);
                done += 1;
                if done.is_multiple_of(PROGRESS_EVERY) {
                    progress.count(Phase::Copy, done, plan.files, EXPORT_LABEL);
                }
            }
        }
    }
    progress.count(Phase::Copy, done, plan.files, EXPORT_LABEL);

    let manifest = store::encode(&Manifest { entries }).map_err(Produced::Failed)?;
    append_bytes(&mut tar, "manifest.json", &manifest, at).map_err(abandoned)?;
    let encoder = tar.into_inner().map_err(abandoned)?;
    let sink = encoder.finish().map_err(abandoned)?;
    sink.end().map_err(abandoned)
}

/// One metadata file of the archive: mode 0600, the backup's capture time.
fn append_bytes<W: Write>(
    tar: &mut tar::Builder<W>,
    name: &str,
    bytes: &[u8],
    mtime: u64,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o600);
    header.set_mtime(mtime);
    header.set_uid(0);
    header.set_gid(0);
    header.set_size(bytes.len() as u64);
    tar.append_data(&mut header, name, bytes)
}

/// A file of the backup vanished or changed size while it was exported.
fn changed(backup: &BackupRef, entry: &ManifestEntry, names: &WtfPathRedactor) -> Error {
    let path = names.rel(&entry.path);
    Error::new(
        ErrorCode::Io,
        format!(
            "backup {} changed while it was exported: {path} is not as its manifest records",
            backup.id
        ),
        Suggestion::command(["omaddons", "backup", "list"]),
    )
    .details(serde_json::json!({ "backup": backup.id, "path": path }))
}

/// A file of the backup differs from the hash it was recorded with: damaged on disk,
/// which retrying does not mend.
fn damaged(backup: &BackupRef, entry: &ManifestEntry, names: &WtfPathRedactor) -> Error {
    super::verify::mismatch(&backup.id, &names.rel(&entry.path))
}

/// Hashes and counts what is read through it (also `backup import`'s).
pub(super) struct Hashing<R> {
    inner: R,
    hasher: Sha256,
    read: u64,
}

impl<R: Read> Hashing<R> {
    pub(super) fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            read: 0,
        }
    }

    /// Bytes read, and their lowercase hex SHA-256.
    pub(super) fn finish(self) -> (u64, String) {
        (self.read, hex(&self.hasher.finalize()))
    }
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.read += n as u64;
        Ok(n)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The producer's end of the channel: fills chunks and sends them.
struct ChannelWriter<'a> {
    tx: &'a SyncSender<Chunk>,
    buf: Vec<u8>,
}

impl ChannelWriter<'_> {
    fn send(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK));
        self.tx
            .send(Chunk::Data(chunk))
            .map_err(|_| io::Error::other("the export's write stopped"))
    }

    /// Sends what is left, then the end marker.
    fn end(mut self) -> io::Result<()> {
        self.send()?;
        self.tx
            .send(Chunk::End)
            .map_err(|_| io::Error::other("the export's write stopped"))
    }
}

impl Write for ChannelWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.send()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send()
    }
}

/// The write's end of the channel: what fsx reads, counted and hashed (the archive's
/// own size and digest).
struct ChannelReader {
    rx: Receiver<Chunk>,
    chunk: Vec<u8>,
    at: usize,
    ended: bool,
    hasher: Sha256,
    len: u64,
}

/// What the write read: the archive's size and digest.
struct ReadArchive {
    len: u64,
    sha256: String,
}

impl ChannelReader {
    fn new(rx: Receiver<Chunk>) -> Self {
        Self {
            rx,
            chunk: Vec::new(),
            at: 0,
            ended: false,
            hasher: Sha256::new(),
            len: 0,
        }
    }

    /// Drops the receiving end (a producer blocked on a full channel then stops).
    fn finish(self) -> ReadArchive {
        ReadArchive {
            len: self.len,
            sha256: hex(&self.hasher.finalize()),
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.at == self.chunk.len() {
            if self.ended {
                return Ok(0);
            }
            match self.rx.recv() {
                Ok(Chunk::Data(chunk)) => {
                    self.chunk = chunk;
                    self.at = 0;
                }
                Ok(Chunk::End) => self.ended = true,
                Err(_) => return Err(io::Error::other("the export stopped before its end")),
            }
        }
        let n = (self.chunk.len() - self.at).min(buf.len());
        buf[..n].copy_from_slice(&self.chunk[self.at..self.at + n]);
        self.at += n;
        self.hasher.update(&buf[..n]);
        self.len += n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::testing::{meta, seed};
    use crate::backup::{BackupKind, list};
    use crate::state::paths::ClientPaths;

    const A: &str = "01M6Q2VA000000000000000000";

    /// A seeded backup of a client under a temp dir, with `baseline` and, when
    /// `hashed`, its one file's hash in the manifest.
    fn seeded(baseline: Option<Baseline>, hashed: bool) -> (tempfile::TempDir, BackupRef) {
        let dir = tempfile::tempdir().unwrap();
        let client = ClientPaths::new(dir.path());
        let mut m = meta(
            A.parse().unwrap(),
            "2026-10-14T21:00:00Z",
            BackupKind::Manual,
        );
        m.baseline = baseline;
        let backup = seed(&client, &m);
        if hashed {
            let mut manifest = store::load::<Manifest>(&backup.join("manifest.json"))
                .unwrap()
                .unwrap()
                .value;
            manifest.entries[0].sha256 = Some(hex(&Sha256::digest(b"SET a 1")));
            std::fs::write(
                backup.join("manifest.json"),
                store::encode(&manifest).unwrap(),
            )
            .unwrap();
        }
        let found = list(&client).0.remove(0);
        (dir, found)
    }

    #[test]
    fn verified_against_follows_the_manifest_and_its_baseline() {
        for (baseline, hashed, want) in [
            (Some(Baseline::Capture), true, VerifiedAgainst::Capture),
            (Some(Baseline::Import), true, VerifiedAgainst::Import),
            (Some(Baseline::Later), true, VerifiedAgainst::Later),
            (None, true, VerifiedAgainst::ComputedAtExport),
            (
                Some(Baseline::Capture),
                false,
                VerifiedAgainst::ComputedAtExport,
            ),
        ] {
            let (_dir, backup) = seeded(baseline, hashed);
            let plan = plan(&backup).unwrap();
            assert_eq!(plan.verified_against, want, "{baseline:?} {hashed}");
            assert_eq!((plan.files, plan.bytes), (1, 7));
        }
    }

    #[test]
    fn the_stream_is_ordered_described_and_ends_only_when_complete() {
        let (_dir, backup) = seeded(Some(Baseline::Capture), true);
        let plan = plan(&backup).unwrap();
        let (tx, rx) = sync_channel(CHANNEL_DEPTH);
        let names = WtfPathRedactor::for_client(&ClientPaths::new(_dir.path()));
        let produced = std::thread::scope(|scope| {
            let producer =
                scope.spawn(|| produce(&backup, &plan, tx, (&names, &ProgressSink::none())));
            let mut reader = ChannelReader::new(rx);
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            assert!(producer.join().unwrap().is_ok());
            bytes
        });
        let decoded = zstd::stream::decode_all(&produced[..]).unwrap();
        let mut archive = tar::Archive::new(&decoded[..]);
        let entries: Vec<(String, u32, u64)> = archive
            .entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let h = e.header();
                (
                    e.path().unwrap().display().to_string(),
                    h.mode().unwrap(),
                    h.mtime().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            entries
                .iter()
                .map(|(p, _, _)| p.as_str())
                .collect::<Vec<_>>(),
            ["backup.json", "WTF/Config.wtf", "manifest.json"]
        );
        assert_eq!(entries[1].1, 0o644, "the recorded mode");

        // A channel that closes without its end marker is an error, never a short read.
        let (tx, rx) = sync_channel(1);
        tx.send(Chunk::Data(b"partial".to_vec())).unwrap();
        drop(tx);
        let mut short = Vec::new();
        assert!(ChannelReader::new(rx).read_to_end(&mut short).is_err());
    }
}
