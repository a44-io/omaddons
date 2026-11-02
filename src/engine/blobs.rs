//! The content-addressed blob cache (vision §5.5 step 2): every downloaded archive is
//! stored once as `<cache>/omaddons/blobs/<sha256>`, however many targets or
//! transactions use it, so reinstalls, undo and cross-target copies work offline.
//!
//! A download streams into `<cache>/omaddons/tmp/<tx>/blob-<n>` through fsx's atomic
//! write, hashed and size-capped on the way (`DOWNLOAD_MAX_BYTES`), and an expected
//! hash is checked at the end of the stream: any failure aborts the atomic write, so
//! nothing is left behind. The finished file is renamed onto its hash name; a blob
//! already there has the same bytes by construction, so the rename simply replaces it
//! and concurrent stores of the same content end as one file.
//!
//! Least-recently-used eviction keeps the cache under `BLOB_CACHE_MAX_BYTES`, never
//! touching pinned blobs: those the installed releases of every registered target came
//! from (`engine::retention`). Last use lives in
//! `blobs/index.json` (atime is often off); the index is derived and rebuilt from the
//! directory when missing or corrupt. CurseForge and Wago archives (M5) must never be
//! cached: [`Storage::Uncached`] writes them to the transaction's stage instead.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use md5::Md5;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::FileStamp;
use crate::fsx::{
    self, Content, GlobalLock, InflightLock, LockUse, Op, PruneReason, RenameMode, ScopeTable,
    WriteSession, Writer, ensure_dir_all, mutate,
};
use crate::limits::{DOCTOR_SCAN_MAX_ENTRIES, DOWNLOAD_MAX_BYTES};
use crate::model::{ExpectedHash, HashFromHeader, TxId};
use crate::net::retry::{RealSleeper, Rng, Sleeper, XorShift, backoff_delay};
use crate::net::{Http, Request};
use crate::politeness::MAX_RETRIES;
use crate::state::paths::Paths;

/// A stored archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    /// Lowercase hex.
    pub sha256: String,
    pub size: u64,
    pub path: PathBuf,
}

/// A downloaded archive, and why its bytes went unchecked when the hash its source
/// normally sends (a Tukui `ETag` MD5) could not be had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub blob: BlobRef,
    pub unverified: Option<String>,
}

/// Where one download attempt's bytes are, and what they must hash to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadTarget {
    pub url: String,
    pub declared: Option<ExpectedHash>,
    pub from_header: Option<HashFromHeader>,
}

/// Where a download may be kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storage {
    /// In the shared blob cache.
    Cache,
    /// At `dest` only (CurseForge and Wago, whose terms forbid caching): typically a
    /// file in the transaction's stage dir.
    Uncached { dest: PathBuf },
}

/// What an eviction pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictReport {
    /// Evicted blobs, least recently used first.
    pub evicted: Vec<String>,
    pub freed_bytes: u64,
    /// Bytes left in the cache.
    pub remaining_bytes: u64,
    /// Still over the cap because the rest is pinned.
    pub over_cap: bool,
    /// Temp dirs of dead transactions removed.
    pub orphans_removed: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct IndexEntry {
    size: u64,
    last_used_ns: i128,
}

/// The cache under one set of paths.
pub struct BlobCache {
    paths: Paths,
    max_download: u64,
    clock: fn() -> i128,
    counter: AtomicU64,
    /// Waits out the backoff before a download restarts.
    sleeper: Box<dyn Sleeper>,
    rng: Mutex<XorShift>,
}

impl BlobCache {
    pub fn new(paths: &Paths) -> Self {
        Self {
            paths: paths.clone(),
            max_download: DOWNLOAD_MAX_BYTES,
            clock: now_ns,
            counter: AtomicU64::new(0),
            sleeper: Box::new(RealSleeper),
            rng: Mutex::new(XorShift::seeded()),
        }
    }

    /// Another sleeper for download restarts (tests).
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Box<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// The full-jitter backoff before download restart `restart` (0-based).
    fn backoff(&self, restart: u32) -> std::time::Duration {
        let random = match self.rng.lock() {
            Ok(mut rng) => rng.next_u64(),
            Err(poisoned) => poisoned.into_inner().next_u64(),
        };
        backoff_delay(restart, random)
    }

    /// The download cap in force (`limits.download_max_bytes`; the engine passes the
    /// configured value, raised or lowered).
    #[must_use]
    pub fn with_max_download(mut self, bytes: u64) -> Self {
        self.max_download = bytes;
        self
    }

    /// Another clock for last-use times (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: fn() -> i128) -> Self {
        self.clock = clock;
        self
    }

    /// Streams `reader` into storage, hashing it, capping its size and checking
    /// `expected`. Errors: `archive_too_large` (exit 4), `hash_mismatch` (exit 1,
    /// retryable: the caller may download once more and should mark a second failure
    /// not retryable), and the reader's own failure as a response body's
    /// (`net::body_error`: `network`, the streamed cap, a `net.log` line not written).
    pub fn store_stream(
        &self,
        scopes: &ScopeTable,
        tx: &TxId,
        reader: impl Read,
        expected: Option<&ExpectedHash>,
        storage: &Storage,
    ) -> Result<BlobRef> {
        let expected: Vec<ExpectedHash> = expected.into_iter().cloned().collect();
        self.store_checked(scopes, tx, reader, &expected, storage)
    }

    /// [`BlobCache::store_stream`] checking every hash in `expected`. SHA-256 is always
    /// computed (it names the blob); MD5 only when one is expected.
    pub fn store_checked(
        &self,
        scopes: &ScopeTable,
        tx: &TxId,
        reader: impl Read,
        expected: &[ExpectedHash],
        storage: &Storage,
    ) -> Result<BlobRef> {
        let wants_md5 = expected.iter().any(|e| matches!(e, ExpectedHash::Md5(_)));
        let mut guard = Guard {
            inner: reader,
            hasher: Sha256::new(),
            md5: wants_md5.then(Md5::new),
            len: 0,
            max: self.max_download,
            expected: expected
                .iter()
                .map(|e| match e {
                    ExpectedHash::Sha256(hex) => ExpectedHash::Sha256(hex.to_ascii_lowercase()),
                    ExpectedHash::Md5(hex) => ExpectedHash::Md5(hex.to_ascii_lowercase()),
                })
                .collect(),
            failure: None,
        };
        let (written_to, writer) = match storage {
            Storage::Cache => {
                let n = self.counter.fetch_add(1, Ordering::Relaxed);
                (
                    self.paths.cache_tmp(tx).join(format!("blob-{n}")),
                    Writer::Derived,
                )
            }
            Storage::Uncached { dest } => (dest.clone(), Writer::Mutating),
        };
        let mut s = WriteSession::untracked(writer, scopes, "blob");
        if let Some(parent) = written_to.parent() {
            ensure_dir_all(&mut s, parent)?;
        }
        let op = Op::WriteFile {
            content: Content::Reader(&mut guard),
            mode: Some(0o644),
        };
        if let Err(err) = mutate(&mut s, &written_to, op) {
            return Err(guard.failure.take().unwrap_or_else(|| err.into()));
        }
        let sha256 = guard.hex();
        let size = guard.len;
        let path = match storage {
            Storage::Uncached { dest } => dest.clone(),
            Storage::Cache => {
                let blobs = self.paths.blobs_dir();
                let mut s = WriteSession::untracked(Writer::Derived, scopes, "blob");
                ensure_dir_all(&mut s, &blobs)?;
                let path = self.paths.blob(&sha256);
                let op = Op::Rename {
                    to: &path,
                    mode: RenameMode::Replace,
                };
                mutate(&mut s, &written_to, op)?;
                self.touch(scopes, &sha256, size)?;
                path
            }
        };
        Ok(BlobRef { sha256, size, path })
    }

    /// Downloads `url` into the cache, checking `declared` and, when `from_header` says
    /// so and no MD5 is declared, the MD5 the response's header carries. A hash
    /// mismatch is retried once; a second one is not retryable. A header that cannot be
    /// used is no check, said in [`Fetched::unverified`].
    pub fn download(
        &self,
        scopes: &ScopeTable,
        tx: &TxId,
        http: &dyn Http,
        url: &str,
        declared: Option<&ExpectedHash>,
        from_header: Option<HashFromHeader>,
    ) -> Result<Fetched> {
        let target = DownloadTarget {
            url: url.to_owned(),
            declared: declared.cloned(),
            from_header,
        };
        self.download_resolving(scopes, tx, http, &mut || Ok(target.clone()), false)
    }

    /// [`BlobCache::download`] from a target `resolve` gives afresh before each attempt
    /// (a deferred asset's token URL). One retry follows a hash mismatch or, when
    /// `refreshable`, a 403 or 404 (the token died). A connection that fails mid-body
    /// restarts the download from the first byte, up to `MAX_RETRIES` times with
    /// full-jitter backoff. What made the attempt that succeeded unverified is its own.
    pub fn download_resolving(
        &self,
        scopes: &ScopeTable,
        tx: &TxId,
        http: &dyn Http,
        resolve: &mut dyn FnMut() -> Result<DownloadTarget>,
        refreshable: bool,
    ) -> Result<Fetched> {
        let mut attempt = 1;
        let mut restarts = 0;
        loop {
            let target = resolve()?;
            let resp = http.send(Request::get(&target.url, "asset").download())?;
            if refreshable && matches!(resp.status, 403 | 404) && attempt == 1 {
                attempt += 1;
                continue;
            }
            let resp = resp.error_for_status("asset")?;
            let declared_md5 = matches!(target.declared, Some(ExpectedHash::Md5(_)));
            let mut expected: Vec<ExpectedHash> = target.declared.into_iter().collect();
            let mut unverified = None;
            if let (Some(HashFromHeader::EtagMd5), false) = (target.from_header, declared_md5) {
                match etag_md5(resp.header("etag")) {
                    Ok(md5) => expected.push(md5),
                    Err(why) => {
                        unverified =
                            Some(format!("{} sent {why}", crate::net::redact(&target.url)));
                    }
                }
            }
            match self.store_checked(scopes, tx, resp.into_reader(), &expected, &Storage::Cache) {
                Err(e) if e.code == ErrorCode::HashMismatch && attempt == 1 => attempt += 1,
                // ubs:ignore — comparing error codes, not secrets
                Err(e) if e.code == ErrorCode::HashMismatch => return Err(e.retryable(false)),
                // The aborted write left nothing; the GET again, from the first byte.
                Err(e) if matches!(e.code, ErrorCode::Network) && restarts < MAX_RETRIES => {
                    self.sleeper.sleep(self.backoff(restarts));
                    restarts += 1;
                }
                done => return done.map(|blob| Fetched { blob, unverified }),
            }
        }
    }

    /// The cached blob with this hash, marking it used.
    pub fn get(&self, scopes: &ScopeTable, sha256: &str) -> Result<Option<BlobRef>> {
        let sha256 = sha256.to_ascii_lowercase();
        if !is_sha256(&sha256) {
            return Ok(None);
        }
        let path = self.paths.blob(&sha256);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return Ok(None);
        };
        if !meta.is_file() {
            return Ok(None);
        }
        self.touch(scopes, &sha256, meta.len())?;
        Ok(Some(BlobRef {
            sha256,
            size: meta.len(),
            path,
        }))
    }

    /// Opens a blob for reading.
    pub fn open(&self, blob: &BlobRef) -> Result<std::fs::File> {
        std::fs::File::open(&blob.path).map_err(|e| {
            Error::io(
                &blob.path,
                &e,
                Suggestion::text("download the archive again"),
            )
        })
    }

    /// Evicts least-recently-used unpinned blobs until the cache is at most `cap`
    /// bytes, and removes temp dirs of transactions that are no longer alive. The
    /// victims and `pins` (the blobs records name) are chosen under the global lock, so
    /// a blob a commit records or uses meanwhile is never one of them; the lock is taken
    /// only when something may go.
    pub fn evict(
        &self,
        scopes: &ScopeTable,
        pins: &dyn Fn() -> Result<BTreeSet<String>>,
        cap: u64,
    ) -> Result<EvictReport> {
        // Pins only keep blobs: without them, an upper bound of what goes.
        let quick = self.selection(&BTreeSet::new(), cap);
        if quick.evicted.is_empty() && self.orphans().is_empty() {
            return Ok(quick);
        }
        let lock = GlobalLock::acquire(&self.paths, "evict")?;
        let mut report = self.selection(&pins()?, cap);
        let orphans = self.orphans();
        if !report.evicted.is_empty() {
            let victims: Vec<PathBuf> = report
                .evicted
                .iter()
                .map(|sha| self.paths.blob(sha))
                .collect();
            fsx::prune(
                scopes,
                PruneReason::Retention,
                &victims,
                LockUse::Held(&lock),
            )?;
            let mut index = self.index();
            for sha in &report.evicted {
                index.remove(sha);
            }
            self.save_index(scopes, &index)?;
        }
        if !orphans.is_empty() {
            let pruned = fsx::prune(
                scopes,
                PruneReason::PostCommitCleanup,
                &orphans,
                LockUse::Held(&lock),
            )?;
            report.orphans_removed = pruned.removed.len();
        }
        Ok(report)
    }

    /// The least-recently-used blobs but `pins` that bring the cache to at most `cap`
    /// bytes, as the directory and the last-use index say now.
    fn selection(&self, pins: &BTreeSet<String>, cap: u64) -> EvictReport {
        let index = self.index();
        let mut blobs: Vec<(String, u64, i128)> = list(&self.paths.blobs_dir())
            .into_iter()
            .filter(|(name, _)| is_sha256(name))
            .map(|(name, meta)| {
                let last = index
                    .get(&name)
                    .map_or_else(|| mtime_ns(&meta), |e| e.last_used_ns);
                (name, meta.len(), last)
            })
            .collect();
        blobs.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
        let mut report = EvictReport {
            remaining_bytes: blobs.iter().map(|b| b.1).sum(),
            ..EvictReport::default()
        };
        for (name, size, _) in &blobs {
            if report.remaining_bytes <= cap {
                break;
            }
            if pins.contains(name) {
                continue;
            }
            report.evicted.push(name.clone());
            report.freed_bytes += size;
            report.remaining_bytes -= size;
        }
        report.over_cap = report.remaining_bytes > cap;
        report
    }

    /// The temp dirs of transactions that are no longer alive.
    fn orphans(&self) -> Vec<PathBuf> {
        list(&self.paths.cache_dir().join("tmp"))
            .into_iter()
            .filter_map(|(name, _)| name.parse::<TxId>().ok())
            .filter(|tx| !InflightLock::is_live(&self.paths, tx).unwrap_or(true))
            .map(|tx| self.paths.cache_tmp(&tx))
            .collect()
    }

    /// The last-use index; rebuilt from the directory (mtimes) when missing or corrupt.
    fn index(&self) -> BTreeMap<String, IndexEntry> {
        let path = self.paths.blobs_dir().join("index.json");
        if let Some(index) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            return index;
        }
        list(&self.paths.blobs_dir())
            .into_iter()
            .filter(|(name, _)| is_sha256(name))
            .map(|(name, meta)| {
                let entry = IndexEntry {
                    size: meta.len(),
                    last_used_ns: mtime_ns(&meta),
                };
                (name, entry)
            })
            .collect()
    }

    fn touch(&self, scopes: &ScopeTable, sha256: &str, size: u64) -> Result<()> {
        let mut index = self.index();
        index.insert(
            sha256.to_owned(),
            IndexEntry {
                size,
                last_used_ns: (self.clock)(),
            },
        );
        self.save_index(scopes, &index)
    }

    fn save_index(&self, scopes: &ScopeTable, index: &BTreeMap<String, IndexEntry>) -> Result<()> {
        let bytes = serde_json::to_vec(index)
            .map_err(|e| Error::internal(format!("serializing the blob index: {e}")))?;
        let mut s = WriteSession::untracked(Writer::Derived, scopes, "blob index");
        ensure_dir_all(&mut s, &self.paths.blobs_dir())?;
        let op = Op::WriteFile {
            content: Content::Bytes(&bytes),
            mode: Some(0o644),
        };
        mutate(&mut s, &self.paths.blobs_dir().join("index.json"), op)?;
        Ok(())
    }
}

/// Hashes and counts the stream, failing past `max` bytes or, at its end, on a hash
/// other than one `expected`; the failure is kept for the caller.
struct Guard<R> {
    inner: R,
    hasher: Sha256,
    /// Only when an MD5 is expected.
    md5: Option<Md5>,
    len: u64,
    max: u64,
    expected: Vec<ExpectedHash>,
    failure: Option<Error>,
}

impl<R> Guard<R> {
    fn hex(&self) -> String {
        hex(&self.hasher.clone().finalize())
    }

    /// What the stream hashed to in `expected`'s algorithm.
    fn actual(&self, expected: &ExpectedHash) -> Option<String> {
        match expected {
            ExpectedHash::Sha256(_) => Some(self.hex()),
            ExpectedHash::Md5(_) => self.md5.as_ref().map(|m| hex(&m.clone().finalize())),
        }
    }

    fn fail(&mut self, error: Error) -> io::Error {
        let message = error.message.clone();
        self.failure = Some(error);
        io::Error::other(message)
    }
}

impl<R: Read> Read for Guard<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = match self.inner.read(buf) {
            Ok(n) => n,
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) if err.kind() == io::ErrorKind::Interrupted => return Err(err),
            // The network stack's own failure (the streamed download cap, a reset, its
            // net.log line), classified as `Response::bytes` does, never an io error on
            // the temp file it was being written to.
            Err(err) => return Err(self.fail(crate::net::body_error(err))),
        };
        if n == 0 {
            for expected in self.expected.clone() {
                let actual = self.actual(&expected).unwrap_or_default();
                let published = expected.hex();
                // ubs:ignore — comparing a download's digest with its published one, not a secret
                if actual != published {
                    let algorithm = expected.algorithm();
                    return Err(self.fail(
                        Error::hash_mismatch(
                            1,
                            format!("the download's {algorithm} is {actual}, but the source published {published}"),
                            Suggestion::text("retry once; if it persists, the upstream file changed or is corrupt"),
                        )
                        .details(json!({ "expected": published, "actual": actual })),
                    ));
                }
            }
            return Ok(0);
        }
        self.len += n as u64;
        if self.len > self.max {
            return Err(self.fail(crate::net::download_too_large(self.max)));
        }
        let chunk = buf.get(..n).unwrap_or_default();
        self.hasher.update(chunk);
        if let Some(md5) = &mut self.md5 {
            md5.update(chunk);
        }
        Ok(n)
    }
}

/// The MD5 a strong, quoted, 32-hex `ETag` carries; otherwise why there is none to
/// check (absent, weak, unquoted, or not an MD5).
fn etag_md5(etag: Option<&str>) -> std::result::Result<ExpectedHash, String> {
    let Some(etag) = etag.map(str::trim) else {
        return Err("no ETag".to_owned());
    };
    if etag.starts_with("W/") {
        return Err(format!("a weak ETag ({etag})"));
    }
    let quoted = etag
        .strip_prefix('"')
        .and_then(|e| e.strip_suffix('"'))
        .ok_or_else(|| format!("an unquoted ETag ({etag})"))?;
    ExpectedHash::md5(quoted).ok_or_else(|| format!("an ETag that is not an MD5 ({etag})"))
}

pub(crate) fn is_sha256(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Entry names and metadata of `dir`; nothing when it cannot be read.
fn list(dir: &Path) -> Vec<(String, std::fs::Metadata)> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    listing
        .filter_map(std::result::Result::ok)
        .filter_map(|e| Some((e.file_name().into_string().ok()?, e.metadata().ok()?)))
        .collect()
}

fn mtime_ns(meta: &std::fs::Metadata) -> i128 {
    i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec())
}

fn now_ns() -> i128 {
    jiff::Timestamp::now().as_nanosecond()
}

// ---------------------------------------------------------------------------------------
// Verification (F6, M4-D52): a cached blob's bytes against its name.

/// What re-hashing a cached blob found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// Its bytes hash to its name.
    Matches { size: u64 },
    /// They hash to `actual` (64 lowercase hex).
    Differs { size: u64, actual: String },
    /// A regular file that cannot be read.
    Unreadable { kind: io::ErrorKind },
    /// No regular file at the path (missing, a directory, a symlink, which is never
    /// followed), or a name that is no blob's.
    Gone,
    /// `stop` asked to stop; nothing is remembered.
    Stopped,
}

/// The bytes read at a time.
const VERIFY_CHUNK: usize = 64 * 1024;

/// The bytes read between two consultations of `stop`.
const VERIFY_STOP_EVERY: u64 = 1024 * 1024;

/// Re-hashes the blob at `path` against its `name`, consulting `stop` after each MiB.
/// Read-only. One hash per blob per process while it is unchanged: the process-wide
/// [`VerifyMemo`] answers again for a file whose stamp is the one it hashed.
pub fn verify_blob(path: &Path, name: &str, stop: &mut dyn FnMut() -> bool) -> Verification {
    MEMO.verify(path, name, stop, now_ns())
}

static MEMO: VerifyMemo = VerifyMemo::new();

/// What a verification hashed, remembered while its file's stamp is unchanged.
#[derive(Debug, Clone, Copy)]
struct Seen {
    stamp: FileStamp,
    size: u64,
    sha256: [u8; 32],
}

/// Verdicts remembered by path, each guarded by the [`FileStamp`] the file had when it
/// was hashed: an equal stamp of a stamp that was settled when read proves the bytes did
/// not change (`ctime` moves on every write, truncation, rename and chmod), and an
/// unsettled stamp is never remembered, as the swap's `predigest` reuses a digest. In
/// memory only, keyed by path, and capped at [`DOCTOR_SCAN_MAX_ENTRIES`] entries (the
/// most blobs one detector run visits): past the cap, verification runs and is not
/// remembered.
#[derive(Debug)]
pub struct VerifyMemo {
    seen: Mutex<BTreeMap<PathBuf, Seen>>,
    cap: usize,
    reads: AtomicU64,
}

impl Default for VerifyMemo {
    fn default() -> Self {
        Self::new()
    }
}

impl VerifyMemo {
    pub const fn new() -> Self {
        // A `u64` cap far below `usize::MAX` on every target omaddons builds for.
        Self::with_cap(DOCTOR_SCAN_MAX_ENTRIES as usize)
    }

    #[doc(hidden)]
    pub const fn with_cap(cap: usize) -> Self {
        Self {
            seen: Mutex::new(BTreeMap::new()),
            cap,
            reads: AtomicU64::new(0),
        }
    }

    /// The files opened and hashed so far.
    #[doc(hidden)]
    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }

    fn seen(&self) -> std::sync::MutexGuard<'_, BTreeMap<PathBuf, Seen>> {
        match self.seen.lock() {
            Ok(guard) => guard,
            // A verdict is a pure function's; a panicked holder left nothing half-done.
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// [`verify_blob`] with this memo and the wall clock read as `now_ns`.
    pub fn verify(
        &self,
        path: &Path,
        name: &str,
        stop: &mut dyn FnMut() -> bool,
        now_ns: i128,
    ) -> Verification {
        if !is_sha256(name) {
            return Verification::Gone;
        }
        let stamp = match FileStamp::read(path) {
            Ok(Some(stamp)) if stamp.is_file() => stamp,
            Ok(_) => return Verification::Gone,
            Err(err) => return Verification::Unreadable { kind: err.kind() },
        };
        // ubs:ignore — comparing file stamps (inode, size, times), not secrets
        let remembered = self.seen().get(path).copied().filter(|s| s.stamp == stamp);
        if let Some(seen) = remembered {
            return verdict(name, seen.size, &seen.sha256);
        }
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Verification::Gone,
            Err(err) => return Verification::Unreadable { kind: err.kind() },
        };
        self.reads.fetch_add(1, Ordering::Relaxed);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; VERIFY_CHUNK];
        let (mut size, mut since_stop) = (0u64, 0u64);
        loop {
            let read = match file.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => read,
                // ubs:ignore — an io::ErrorKind, not a secret
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Verification::Unreadable { kind: err.kind() },
            };
            hasher.update(&buf[..read]);
            let read = read as u64;
            size += read;
            since_stop += read;
            if since_stop >= VERIFY_STOP_EVERY {
                since_stop -= VERIFY_STOP_EVERY;
                if stop() {
                    return Verification::Stopped;
                }
            }
        }
        let sha256: [u8; 32] = hasher.finalize().into();
        // ubs:ignore — comparing file stamps (inode, size, times), not secrets
        let unchanged = matches!(FileStamp::read(path), Ok(Some(after)) if after == stamp);
        if unchanged && stamp.settled(now_ns) {
            let mut seen = self.seen();
            if seen.len() < self.cap || seen.contains_key(path) {
                seen.insert(
                    path.to_path_buf(),
                    Seen {
                        stamp,
                        size,
                        sha256,
                    },
                );
            }
        }
        verdict(name, size, &sha256)
    }
}

fn verdict(name: &str, size: u64, sha256: &[u8; 32]) -> Verification {
    let actual = hex(sha256);
    // ubs:ignore — a content digest against the file's own name, not a secret
    if actual == name {
        Verification::Matches { size }
    } else {
        Verification::Differs { size, actual }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        scopes: ScopeTable,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        Fixture {
            _dir: dir,
            paths,
            scopes,
        }
    }

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    /// Every file under `dir`, relative.
    fn files(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(at) = pending.pop() {
            for (name, meta) in list(&at) {
                let path = at.join(&name);
                if meta.is_dir() {
                    pending.push(path);
                } else {
                    out.push(path.strip_prefix(dir).unwrap().display().to_string());
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn identical_content_is_stored_once() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let tx = TxId::new();
        let a = cache
            .store_stream(
                &f.scopes,
                &tx,
                Cursor::new(b"zip bytes".to_vec()),
                None,
                &Storage::Cache,
            )
            .unwrap();
        let b = cache
            .store_stream(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"zip bytes".to_vec()),
                None,
                &Storage::Cache,
            )
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.sha256, sha(b"zip bytes"));
        assert_eq!(a.size, 9);
        assert_eq!(
            files(&f.paths.blobs_dir()),
            [a.sha256.clone(), "index.json".to_owned()]
        );
        assert!(
            files(&f.paths.cache_dir().join("tmp")).is_empty(),
            "no temp file left"
        );
        assert_eq!(
            cache.get(&f.scopes, &a.sha256.to_uppercase()).unwrap(),
            Some(a.clone())
        );
        assert_eq!(cache.get(&f.scopes, &"0".repeat(64)).unwrap(), None);
        let mut read = String::new();
        cache.open(&a).unwrap().read_to_string(&mut read).unwrap();
        assert_eq!(read, "zip bytes");
    }

    /// Yields `n` bytes without allocating them.
    struct Zeros(u64);

    impl Read for Zeros {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(usize::try_from(self.0).unwrap_or(usize::MAX));
            buf[..n].fill(0);
            self.0 -= n as u64;
            Ok(n)
        }
    }

    #[test]
    fn oversized_downloads_leave_nothing() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths).with_max_download(1_000);
        let err = cache
            .store_stream(&f.scopes, &TxId::new(), Zeros(1_001), None, &Storage::Cache)
            .unwrap_err();
        assert_eq!(
            (err.code, err.exit().code()),
            (ErrorCode::ArchiveTooLarge, 4)
        );
        assert_eq!(err.details["config_key"], "limits.download_max_bytes");
        assert!(
            files(f.paths.cache_dir()).is_empty(),
            "{:?}",
            files(f.paths.cache_dir())
        );
        assert!(
            cache
                .store_stream(&f.scopes, &TxId::new(), Zeros(1_000), None, &Storage::Cache)
                .is_ok()
        );
        assert_eq!(DOWNLOAD_MAX_BYTES, 512 << 20, "the production cap");
    }

    /// Yields `n` bytes, then fails with `error` (a response body failing mid-stream).
    struct Failing(u64, Option<io::Error>);

    impl Read for Failing {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0 == 0 {
                return Err(self.1.take().unwrap_or_else(|| io::Error::other("again")));
            }
            Zeros(self.0).read(buf).inspect(|n| self.0 -= *n as u64)
        }
    }

    /// The reader's failure is the network's, never an io error on the temp file: a
    /// reset is `network` (retryable), and an error the stack carries inside it (its
    /// `net.log` line not written) is that error.
    #[test]
    fn a_body_that_fails_midway_is_the_networks_failure() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let store = |error: io::Error| {
            cache
                .store_stream(
                    &f.scopes,
                    &TxId::new(),
                    Failing(5_000, Some(error)),
                    None,
                    &Storage::Cache,
                )
                .unwrap_err()
        };
        let reset = store(io::Error::from(io::ErrorKind::ConnectionReset));
        assert_eq!((reset.code, reset.retryable), (ErrorCode::Network, true));
        let logged = Error::io(
            &f.paths.net_log(),
            &io::Error::from(io::ErrorKind::IsADirectory),
            Suggestion::command(["omaddons", "status"]),
        );
        let carried = store(io::Error::other(logged.clone()));
        assert_eq!(carried, logged);
        for err in [&reset, &carried] {
            assert!(!err.message.contains("blob-"), "{}", err.message);
            assert!(
                !err.details.to_string().contains("blob-"),
                "{}",
                err.details
            );
        }
        assert!(
            files(f.paths.cache_dir()).is_empty(),
            "{:?}",
            files(f.paths.cache_dir())
        );
    }

    #[test]
    fn a_raised_download_cap_from_config_is_honoured() {
        use crate::engine::inspect::ArchiveLimits;
        use crate::limits::EffectiveLimits;

        // config.toml raised limits.download_max_bytes past what was the cap.
        let raised = EffectiveLimits {
            download_max_bytes: 2_000,
            ..EffectiveLimits::default()
        };
        let limits = ArchiveLimits::from(&raised);
        assert_eq!(limits.max_download, 2_000);
        let f = fixture();
        let cache = BlobCache::new(&f.paths).with_max_download(limits.max_download);
        let stored =
            cache.store_stream(&f.scopes, &TxId::new(), Zeros(1_500), None, &Storage::Cache);
        assert!(stored.is_ok(), "{stored:?}");
        let err = cache
            .store_stream(&f.scopes, &TxId::new(), Zeros(2_001), None, &Storage::Cache)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ArchiveTooLarge);
        assert_eq!(ArchiveLimits::default().max_download, DOWNLOAD_MAX_BYTES);
    }

    #[test]
    fn expected_hashes() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let good = ExpectedHash::Sha256(sha(b"abc").to_uppercase());
        assert!(
            cache
                .store_stream(
                    &f.scopes,
                    &TxId::new(),
                    Cursor::new(b"abc".to_vec()),
                    Some(&good),
                    &Storage::Cache
                )
                .is_ok()
        );
        let bad = ExpectedHash::Sha256(sha(b"xyz"));
        let err = cache
            .store_stream(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"def".to_vec()),
                Some(&bad),
                &Storage::Cache,
            )
            .unwrap_err();
        assert_eq!(
            (err.code, err.exit().code(), err.retryable),
            (ErrorCode::HashMismatch, 1, true)
        );
        assert_eq!(err.details["actual"], sha(b"def"));
        assert!(files(&f.paths.cache_dir().join("tmp")).is_empty());
        assert!(!f.paths.blob(&sha(b"def")).exists());
    }

    fn md5(bytes: &[u8]) -> String {
        hex(&Md5::digest(bytes))
    }

    #[test]
    fn md5_expectations_are_checked_and_blobs_stay_keyed_by_sha256() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let good = ExpectedHash::md5(&md5(b"abc").to_uppercase()).unwrap();
        let blob = cache
            .store_stream(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"abc".to_vec()),
                Some(&good),
                &Storage::Cache,
            )
            .unwrap();
        assert_eq!(blob.sha256, sha(b"abc"));
        assert!(f.paths.blob(&sha(b"abc")).is_file());

        let bad = ExpectedHash::md5(&md5(b"xyz")).unwrap();
        let err = cache
            .store_stream(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"def".to_vec()),
                Some(&bad),
                &Storage::Cache,
            )
            .unwrap_err();
        assert_eq!((err.code, err.retryable), (ErrorCode::HashMismatch, true));
        assert!(err.message.contains("MD5"), "{}", err.message);
        assert_eq!(
            (&err.details["expected"], &err.details["actual"]),
            (&json!(md5(b"xyz")), &json!(md5(b"def")))
        );
        assert!(!f.paths.blob(&sha(b"def")).exists());

        // Both algorithms at once: each is checked.
        let both = [
            ExpectedHash::sha256(&sha(b"ghi")).unwrap(),
            ExpectedHash::md5(&md5(b"nope")).unwrap(),
        ];
        let err = cache
            .store_checked(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"ghi".to_vec()),
                &both,
                &Storage::Cache,
            )
            .unwrap_err();
        assert!(err.message.contains("MD5"), "{}", err.message);
    }

    /// Serves `body` with `etag`, counting requests.
    struct Serve {
        body: Vec<u8>,
        etag: Option<String>,
        sent: AtomicU64,
    }

    impl Serve {
        fn new(body: &[u8], etag: Option<&str>) -> Self {
            Self {
                body: body.to_vec(),
                etag: etag.map(str::to_owned),
                sent: AtomicU64::new(0),
            }
        }
    }

    impl Http for Serve {
        fn send(&self, req: Request) -> Result<crate::net::Response> {
            self.sent.fetch_add(1, Ordering::Relaxed);
            let headers = self
                .etag
                .iter()
                .map(|e| ("ETag".to_owned(), e.clone()))
                .collect();
            Ok(crate::net::Response::new(
                200,
                headers,
                req.url,
                Box::new(Cursor::new(self.body.clone())),
            ))
        }
    }

    #[test]
    fn a_strong_etag_md5_is_checked_and_an_unusable_one_is_reported() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let url = "https://www.tukui.org/download/elvui.zip";
        let download = |http: &Serve, declared: Option<&ExpectedHash>| {
            cache.download(
                &f.scopes,
                &TxId::new(),
                http,
                url,
                declared,
                Some(HashFromHeader::EtagMd5),
            )
        };
        // A matching ETag verifies.
        let quoted = format!("\"{}\"", md5(b"elvui"));
        let got = download(&Serve::new(b"elvui", Some(&quoted)), None).unwrap();
        assert_eq!(got.blob.sha256, sha(b"elvui"));
        assert_eq!(got.unverified, None);

        // A wrong one fails, retried once; the second failure is not retryable.
        let wrong = format!("\"{}\"", md5(b"other"));
        let http = Serve::new(b"elvui", Some(&wrong));
        let err = download(&http, None).unwrap_err();
        assert_eq!((err.code, err.retryable), (ErrorCode::HashMismatch, false));
        assert_eq!(http.sent.load(Ordering::Relaxed), 2);

        // Weak, absent, unquoted or not an MD5: no check, and the download says why.
        let plain = md5(b"elvui");
        for (etag, why) in [
            (Some("W/\"abc\""), "a weak ETag"),
            (None, "no ETag"),
            (Some(plain.as_str()), "an unquoted ETag"),
            (Some("\"abc\""), "an ETag that is not an MD5"),
        ] {
            let got = download(&Serve::new(b"elvui", etag), None).unwrap();
            let reason = got.unverified.unwrap();
            assert!(
                reason.starts_with(&format!("{url} sent {why}")),
                "{etag:?}: {reason}"
            );
        }

        // A declared MD5 wins; the header is not consulted.
        let declared = ExpectedHash::md5(&md5(b"elvui")).unwrap();
        let got = download(&Serve::new(b"elvui", Some(&wrong)), Some(&declared)).unwrap();
        assert_eq!(got.unverified, None);
    }

    /// Answers each URL its route names (404 otherwise): `(status, ETag)`, serving
    /// `elvui` on a 200.
    struct Routes(Vec<(String, u16, Option<String>)>);

    impl Http for Routes {
        fn send(&self, req: Request) -> Result<crate::net::Response> {
            // ubs:ignore — matching fixture URLs, not secrets
            let route = self.0.iter().find(|(url, ..)| *url == req.url);
            let (status, etag) = route.map_or((404, None), |(_, s, e)| (*s, e.clone()));
            let body = if status == 200 {
                b"elvui".to_vec()
            } else {
                Vec::new()
            };
            Ok(crate::net::Response::new(
                status,
                etag.map(|e| ("ETag".to_owned(), e)).into_iter().collect(),
                req.url,
                Box::new(Cursor::new(body)),
            ))
        }
    }

    /// Answers its first `resets` requests with a body that resets midway, then `body`.
    struct Resets {
        resets: AtomicU64,
        body: Vec<u8>,
        sent: AtomicU64,
    }

    impl Http for Resets {
        fn send(&self, req: Request) -> Result<crate::net::Response> {
            self.sent.fetch_add(1, Ordering::Relaxed);
            let body: Box<dyn Read + Send> = if self.resets.load(Ordering::Relaxed) > 0 {
                self.resets.fetch_sub(1, Ordering::Relaxed);
                let reset = io::Error::from(io::ErrorKind::ConnectionReset);
                Box::new(Failing(5_000, Some(reset)))
            } else {
                Box::new(Cursor::new(self.body.clone()))
            };
            Ok(crate::net::Response::new(200, Vec::new(), req.url, body))
        }
    }

    /// Counts the sleeps instead of sleeping.
    struct Sleeps(std::sync::Arc<AtomicU64>);

    impl Sleeper for Sleeps {
        fn sleep(&self, _: std::time::Duration) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A connection that fails mid-body restarts the download from a fresh `resolve`, up
    /// to `MAX_RETRIES` times, leaving nothing behind.
    #[test]
    fn a_download_that_fails_midway_starts_over() {
        for (resets, succeeds) in [(1, true), (3, true), (4, false)] {
            let f = fixture();
            let slept = std::sync::Arc::new(AtomicU64::new(0));
            let cache = BlobCache::new(&f.paths)
                .with_sleeper(Box::new(Sleeps(std::sync::Arc::clone(&slept))));
            let http = Resets {
                resets: AtomicU64::new(resets),
                body: b"elvui".to_vec(),
                sent: AtomicU64::new(0),
            };
            let mut resolves = 0;
            let mut resolve = || {
                resolves += 1;
                Ok(DownloadTarget {
                    url: format!("https://api.tukui.org/v1/download/elvui/token{resolves}"),
                    declared: None,
                    from_header: None,
                })
            };
            let got = cache.download_resolving(&f.scopes, &TxId::new(), &http, &mut resolve, true);
            let attempts = resets.min(4) + u64::from(succeeds);
            let counts = (
                http.sent.load(Ordering::Relaxed),
                resolves,
                slept.load(Ordering::Relaxed),
            );
            let expected_resolves = usize::try_from(attempts).unwrap();
            assert_eq!(
                counts,
                (attempts, expected_resolves, attempts - 1),
                "{resets} resets"
            );
            if succeeds {
                assert_eq!(got.unwrap().blob.sha256, sha(b"elvui"), "{resets} resets");
            } else {
                let err = got.unwrap_err();
                assert_eq!((err.code, err.retryable), (ErrorCode::Network, true));
                assert!(
                    files(f.paths.cache_dir()).is_empty(),
                    "{:?}",
                    files(f.paths.cache_dir())
                );
            }
        }
    }

    #[test]
    fn a_resolved_download_resolves_again_before_its_one_retry() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let good = format!("\"{}\"", md5(b"elvui"));
        let wrong = format!("\"{}\"", md5(b"other"));
        let base = "https://api.tukui.org/v1/download/elvui/token";
        // Token n answers `answers[n - 1]`; returns the result and how many resolves.
        let run = |answers: &[(u16, Option<&str>)], refreshable: bool| {
            let routes = answers
                .iter()
                .enumerate()
                .map(|(i, (status, etag))| {
                    (format!("{base}{}", i + 1), *status, etag.map(str::to_owned))
                })
                .collect();
            let http = Routes(routes);
            let mut resolves = 0;
            let mut resolve = || {
                resolves += 1;
                Ok(DownloadTarget {
                    url: format!("{base}{resolves}"),
                    declared: None,
                    from_header: Some(HashFromHeader::EtagMd5),
                })
            };
            let got =
                cache.download_resolving(&f.scopes, &TxId::new(), &http, &mut resolve, refreshable);
            (got, resolves)
        };

        // A dead token (403 or 404), or a hash mismatch, is resolved again once.
        for first in [(403, None), (404, None), (200, Some(wrong.as_str()))] {
            let (got, resolves) = run(&[first, (200, Some(&good))], true);
            assert_eq!(got.unwrap().blob.sha256, sha(b"elvui"), "{first:?}");
            assert_eq!(resolves, 2, "{first:?}");
        }
        // Dead twice: the second answer is the error, after two resolves.
        let (got, resolves) = run(&[(403, None), (403, None), (200, Some(&good))], true);
        assert_eq!(got.unwrap_err().code, ErrorCode::HttpStatus);
        assert_eq!(resolves, 2);
        // A mismatch twice is not retryable.
        let (got, resolves) = run(&[(200, Some(&wrong)), (200, Some(&wrong))], true);
        let err = got.unwrap_err();
        assert_eq!(
            (err.code, err.retryable, resolves),
            (ErrorCode::HashMismatch, false, 2)
        );
        // A fixed URL is not refreshed: a 403 fails at once.
        let (got, resolves) = run(&[(403, None), (200, Some(&good))], false);
        assert_eq!(
            (got.unwrap_err().code, resolves),
            (ErrorCode::HttpStatus, 1)
        );
        // What went unchecked is the attempt that succeeded (a mismatch, then no ETag),
        // its token redacted.
        let (got, _) = run(&[(200, Some(&wrong)), (200, None)], true);
        let reason = got.unwrap().unverified.unwrap();
        assert_eq!(
            reason,
            "https://api.tukui.org/v1/download/elvui/… sent no ETag"
        );
        let (got, _) = run(&[(403, None), (200, Some(&good))], true);
        assert_eq!(got.unwrap().unverified, None);
    }

    #[test]
    fn uncached_storage_never_touches_the_cache() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let client = tempfile::tempdir().unwrap();
        let client_dir = std::fs::canonicalize(client.path())
            .unwrap()
            .join("_retail_");
        std::fs::create_dir_all(client_dir.join("Interface/AddOns")).unwrap();
        let scopes = ScopeTable::new(&f.paths, std::slice::from_ref(&client_dir), None).unwrap();
        let tx = TxId::new();
        let dest = crate::state::paths::ClientPaths::new(&client_dir)
            .stage(&tx)
            .join("download.zip");
        let blob = cache
            .store_stream(
                &scopes,
                &tx,
                Cursor::new(b"curse".to_vec()),
                None,
                &Storage::Uncached { dest: dest.clone() },
            )
            .unwrap();
        assert_eq!((blob.path, blob.sha256), (dest.clone(), sha(b"curse")));
        assert!(dest.is_file());
        assert!(
            !f.paths.blobs_dir().exists() && !f.paths.cache_dir().join("tmp").exists(),
            "{:?}",
            files(f.paths.cache_dir())
        );
    }

    fn at_1() -> i128 {
        1
    }

    fn at_2() -> i128 {
        2
    }

    fn at_3() -> i128 {
        3
    }

    #[test]
    fn least_recently_used_unpinned_blobs_go_first() {
        let f = fixture();
        let store = |clock: fn() -> i128, bytes: &[u8]| {
            BlobCache::new(&f.paths)
                .with_clock(clock)
                .store_stream(
                    &f.scopes,
                    &TxId::new(),
                    Cursor::new(bytes.to_vec()),
                    None,
                    &Storage::Cache,
                )
                .unwrap()
                .sha256
        };
        let (old, mid, new) = (
            store(at_1, b"aaaa"),
            store(at_2, b"bbbb"),
            store(at_3, b"cccc"),
        );
        let cache = BlobCache::new(&f.paths);
        // The pins are read under the global lock, so no commit records a blob between
        // them and the eviction.
        let pins = || {
            let held = crate::fsx::probe_global(&f.paths);
            assert!(
                matches!(held, Ok(crate::fsx::LockState::Held { .. })),
                "{held:?}"
            );
            Ok(BTreeSet::from([old.clone()]))
        };
        let report = cache.evict(&f.scopes, &pins, 8).unwrap();
        assert_eq!(
            report.evicted,
            std::slice::from_ref(&mid),
            "old is pinned, mid is next"
        );
        assert_eq!(
            (report.freed_bytes, report.remaining_bytes, report.over_cap),
            (4, 8, false)
        );
        assert!(
            f.paths.blob(&old).exists()
                && !f.paths.blob(&mid).exists()
                && f.paths.blob(&new).exists()
        );

        let report = cache
            .evict(
                &f.scopes,
                &|| Ok(BTreeSet::from([old.clone(), new.clone()])),
                1,
            )
            .unwrap();
        assert!(report.evicted.is_empty());
        assert!(report.over_cap, "pinned blobs stay even over the cap");
    }

    #[test]
    fn a_corrupt_index_is_rebuilt() {
        let f = fixture();
        let cache = BlobCache::new(&f.paths);
        let blob = cache
            .store_stream(
                &f.scopes,
                &TxId::new(),
                Cursor::new(b"x".to_vec()),
                None,
                &Storage::Cache,
            )
            .unwrap();
        let index = f.paths.blobs_dir().join("index.json");
        std::fs::write(&index, "{ not json").unwrap();
        assert_eq!(cache.index().keys().collect::<Vec<_>>(), [&blob.sha256]);
        std::fs::remove_file(&index).unwrap();
        assert!(cache.get(&f.scopes, &blob.sha256).unwrap().is_some());
        assert!(index.is_file());
        assert_eq!(
            cache
                .evict(&f.scopes, &|| Ok(BTreeSet::new()), 0)
                .unwrap()
                .evicted,
            [blob.sha256]
        );
    }

    // -----------------------------------------------------------------------------------
    // Verification.

    const MIB: usize = 1024 * 1024;

    /// A file holding `bytes` under `dir`, named `name`.
    fn planted(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// A moment `after_ns` past `path`'s ctime.
    fn after_ctime(path: &Path, after_ns: i128) -> i128 {
        FileStamp::read(path).unwrap().unwrap().ctime_ns() + after_ns
    }

    /// Settled whatever the filesystem's timestamp grain (the coarse slack is 2 s).
    const SETTLED_NS: i128 = 3_000_000_000;

    fn never() -> impl FnMut() -> bool {
        || false
    }

    fn is_root() -> bool {
        rustix::process::geteuid().is_root()
    }

    #[test]
    fn verification_cases() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let good = b"PK\x03\x04 a release archive".to_vec();
        let name = sha(&good);
        let mut flipped = good.clone();
        flipped[7] ^= 0x20;
        let empty_zip = [b"PK\x05\x06".as_slice(), &[0u8; 18]].concat();
        let big: Vec<u8> = (0..3 * MIB).map(|i| (i % 251) as u8).collect();
        let big_name = sha(&big);
        type Case = (&'static str, PathBuf, String, Verification);
        let mut cases: Vec<Case> = Vec::new();
        let mut case = |label: &'static str, sub: &str, bytes: Option<&[u8]>, name: &str, want| {
            let sub = d.join(sub);
            std::fs::create_dir_all(&sub).unwrap();
            let path = match bytes {
                Some(bytes) => planted(&sub, name, bytes),
                None => sub.join(name),
            };
            cases.push((label, path, name.to_owned(), want));
        };
        let size = good.len() as u64;
        case(
            "matches",
            "a",
            Some(&good),
            &name,
            Verification::Matches { size },
        );
        case(
            "same_size_flip",
            "b",
            Some(&flipped),
            &name,
            Verification::Differs {
                size,
                actual: sha(&flipped),
            },
        );
        case(
            "truncated",
            "c",
            Some(&good[..5]),
            &name,
            Verification::Differs {
                size: 5,
                actual: sha(&good[..5]),
            },
        );
        case(
            "another_archive",
            "d",
            Some(&empty_zip),
            &name,
            Verification::Differs {
                size: 22,
                actual: sha(&empty_zip),
            },
        );
        let empty = sha(b"");
        case(
            "empty_named_by_its_hash",
            "e",
            Some(b""),
            &empty,
            Verification::Matches { size: 0 },
        );
        case("missing", "f", None, &name, Verification::Gone);
        std::fs::create_dir_all(d.join("g").join(&name)).unwrap();
        case("a_directory", "g", None, &name, Verification::Gone);
        let target = planted(d, "target", &good);
        std::fs::create_dir_all(d.join("h")).unwrap();
        symlink(&target, d.join("h").join(&name)).unwrap();
        case(
            "a_symlink_to_a_matching_file",
            "h",
            None,
            &name,
            Verification::Gone,
        );
        case(
            "index_json",
            "i",
            Some(&good),
            "index.json",
            Verification::Gone,
        );
        case(
            "uppercase_hex",
            "j",
            Some(&good),
            &name.to_uppercase(),
            Verification::Gone,
        );
        case(
            "63_digits",
            "k",
            Some(&good),
            &name[..63],
            Verification::Gone,
        );
        case(
            "stop_is_consulted_per_mib",
            "l",
            Some(&big),
            &big_name,
            Verification::Matches {
                size: big.len() as u64,
            },
        );
        for (label, path, name, want) in cases {
            let memo = VerifyMemo::new();
            let mut calls = 0;
            let got = memo.verify(
                &path,
                &name,
                &mut || {
                    calls += 1;
                    false
                },
                after_ctime_or_now(&path),
            );
            assert_eq!(got, want, "{label}: {} named {name}", path.display());
            if label == "stop_is_consulted_per_mib" {
                assert_eq!(calls, 3, "{label}: 3 MiB, stop consulted {calls} times");
            }
        }

        let memo = VerifyMemo::new();
        let path = planted(&d.join("a"), &big_name, &big);
        let got = memo.verify(&path, &big_name, &mut || true, SETTLED_NS);
        assert_eq!(
            got,
            Verification::Stopped,
            "stopped: 3 MiB, stop true at once"
        );
        let got = memo.verify(
            &path,
            &big_name,
            &mut never(),
            after_ctime(&path, SETTLED_NS),
        );
        let reads = memo.reads();
        assert!(
            matches!(got, Verification::Matches { .. }) && matches!(reads, 2),
            "stopped: nothing remembered, so the next verify reads ({got:?}, {reads} reads)"
        );

        if is_root() {
            // Root reads a mode-000 file: the case cannot be made.
            return;
        }
        let path = planted(&d.join("a"), &name, &good);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let got = VerifyMemo::new().verify(&path, &name, &mut never(), SETTLED_NS);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            got,
            Verification::Unreadable {
                kind: io::ErrorKind::PermissionDenied
            },
            "unreadable: mode 000"
        );
    }

    /// A moment well past `path`'s ctime when it is a file, else any.
    fn after_ctime_or_now(path: &Path) -> i128 {
        match FileStamp::read(path) {
            Ok(Some(stamp)) => stamp.ctime_ns() + SETTLED_NS,
            _ => now_ns(),
        }
    }

    #[test]
    fn the_memo_answers_an_unchanged_settled_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let good = b"a blob".to_vec();
        let name = sha(&good);
        for (label, bytes) in [("matching", good.clone()), ("damaged", b"a blub".to_vec())] {
            let path = planted(dir.path(), &name, &bytes);
            let memo = VerifyMemo::new();
            let now = after_ctime(&path, SETTLED_NS);
            let first = memo.verify(&path, &name, &mut never(), now);
            let second = memo.verify(&path, &name, &mut never(), now);
            assert_eq!(first, second, "{label}: equal verdicts");
            assert_eq!(
                memo.reads(),
                1,
                "{label}: one read for two verifies ({first:?})"
            );
        }
    }

    #[test]
    fn the_memo_never_trusts_an_unsettled_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let good = b"a blob".to_vec();
        let name = sha(&good);
        let path = planted(dir.path(), &name, &good);
        let memo = VerifyMemo::new();
        let now = after_ctime(&path, 1_000_000);
        for _ in 0..3 {
            let got = memo.verify(&path, &name, &mut never(), now);
            assert_eq!(got, Verification::Matches { size: 6 }, "unsettled: {got:?}");
        }
        assert_eq!(memo.reads(), 3, "1 ms past its ctime: every verify reads");
    }

    #[test]
    fn the_memo_misses_after_any_change() {
        use std::os::unix::fs::PermissionsExt;
        let good = b"a blob".to_vec();
        let other = b"a blub".to_vec();
        let name = sha(&good);
        for label in [
            "rewritten_in_place_with_the_same_bytes",
            "replaced_by_rename",
            "chmod",
            "mtime_set_back",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = planted(dir.path(), &name, &good);
            let memo = VerifyMemo::new();
            let first = memo.verify(&path, &name, &mut never(), after_ctime(&path, SETTLED_NS));
            assert_eq!(first, Verification::Matches { size: 6 }, "{label}: before");
            let old_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
            let want = match label {
                "rewritten_in_place_with_the_same_bytes" => {
                    std::fs::write(&path, &good).unwrap();
                    Verification::Matches { size: 6 }
                }
                "replaced_by_rename" => {
                    let next = planted(dir.path(), "next", &other);
                    std::fs::rename(next, &path).unwrap();
                    Verification::Differs {
                        size: 6,
                        actual: sha(&other),
                    }
                }
                "chmod" => {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    Verification::Matches { size: 6 }
                }
                _ => {
                    // Same size, other bytes, and the mtime put back: only ctime tells.
                    std::fs::write(&path, &other).unwrap();
                    std::fs::File::options()
                        .write(true)
                        .open(&path)
                        .unwrap()
                        .set_modified(old_mtime)
                        .unwrap();
                    Verification::Differs {
                        size: 6,
                        actual: sha(&other),
                    }
                }
            };
            let got = memo.verify(&path, &name, &mut never(), after_ctime(&path, SETTLED_NS));
            assert_eq!(got, want, "{label}: the verdict follows the new bytes");
            assert_eq!(memo.reads(), 2, "{label}: read again after the change");
        }
    }

    #[test]
    fn the_memo_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let memo = VerifyMemo::with_cap(2);
        let files: Vec<(PathBuf, String)> = ["one", "two", "three"]
            .iter()
            .map(|text| {
                let name = sha(text.as_bytes());
                (planted(dir.path(), &name, text.as_bytes()), name)
            })
            .collect();
        for round in 0..2 {
            for (path, name) in &files {
                let now = after_ctime(path, SETTLED_NS);
                let got = memo.verify(path, name, &mut never(), now);
                assert!(
                    matches!(got, Verification::Matches { .. }),
                    "round {round}: {got:?}"
                );
            }
        }
        assert_eq!(memo.reads(), 4, "two remembered, the third read both times");
    }
}
