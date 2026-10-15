//! `net.log`: one JSON line per HTTP request or git subprocess (vision §5.7, G5).
//!
//! The query string and request headers are never logged, which strips tokens, API
//! keys and signed URL parameters in one rule. Lines are appended through an fsx
//! untracked session; above `NET_LOG_MAX_BYTES` the file rotates to `net.log.1`, keeping
//! one previous file.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{Op, RenameMode, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate};
use crate::limits::NET_LOG_MAX_BYTES;
use crate::state::paths::Paths;

/// How a logged request went out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    Http,
    Git,
}

/// One `net.log` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NetLogEntry {
    #[schemars(with = "String")]
    pub time: Timestamp,
    /// `GET`, `HEAD`, or `GIT` for subprocesses.
    pub method: String,
    pub host: String,
    /// The URL path, never the query.
    pub path: String,
    /// The HTTP status, or a git subprocess's exit code; null when nothing came back.
    pub status: Option<u16>,
    /// Body bytes actually read.
    pub bytes: Option<u64>,
    pub duration_ms: u64,
    pub purpose: String,
    pub via: Via,
    /// The error code when the request failed.
    pub error: Option<String>,
}

/// A git subprocess to log (source-git-hosts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitLogEntry {
    pub host: String,
    /// The repository path on the host, without any query or credentials.
    pub path: String,
    /// The exit code; `None` when the process was killed by a signal.
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// `ls-remote`, `clone` or `fetch`.
    pub purpose: &'static str,
    pub error: Option<ErrorCode>,
}

struct Inner {
    path: PathBuf,
    previous: PathBuf,
    scopes: ScopeTable,
    max_bytes: u64,
    /// Serializes rotation and appends within this process.
    write: Mutex<()>,
}

/// The request log of this machine. Cheap to clone.
#[derive(Clone)]
pub struct NetLog {
    inner: Arc<Inner>,
}

impl NetLog {
    pub fn new(paths: &Paths) -> Result<Self> {
        Self::with_max_bytes(paths, NET_LOG_MAX_BYTES)
    }

    /// With a different rotation threshold, so tests need not write 10 MiB.
    pub fn with_max_bytes(paths: &Paths, max_bytes: u64) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                path: paths.net_log(),
                previous: paths.net_log_previous(),
                scopes: ScopeTable::new(paths, &[], None)?,
                max_bytes,
                write: Mutex::new(()),
            }),
        })
    }

    /// Appends one line, rotating first when the file has grown past the threshold.
    pub fn append(&self, entry: &NetLogEntry) -> Result<()> {
        let mut line = serde_json::to_vec(entry).map_err(|err| log_error(&self.inner.path, err))?;
        line.push(b'\n');
        let _guard = self
            .inner
            .write
            .lock()
            .map_err(|_| Error::internal("the net.log writer lock is poisoned"))?;
        let mut session = WriteSession::untracked(Writer::Derived, &self.inner.scopes, "net.log");
        if let Some(dir) = self.inner.path.parent() {
            ensure_dir_all(&mut session, dir)?;
        }
        let size = fs::metadata(&self.inner.path).map_or(0, |m| m.len());
        if size > self.inner.max_bytes {
            let rotate = Op::Rename {
                to: &self.inner.previous,
                mode: RenameMode::Replace,
            };
            mutate(&mut session, &self.inner.path, rotate)?;
        }
        mutate(
            &mut session,
            &self.inner.path,
            Op::AppendFile { content: &line },
        )?;
        Ok(())
    }

    /// Logs a git subprocess as `via: "git"`, `method: "GIT"`.
    pub fn record_git(&self, entry: GitLogEntry) -> Result<()> {
        self.append(&NetLogEntry {
            time: now(),
            method: "GIT".to_owned(),
            host: entry.host,
            path: entry.path,
            status: entry.exit_code.and_then(|code| u16::try_from(code).ok()),
            bytes: None,
            duration_ms: entry.duration_ms,
            purpose: entry.purpose.to_owned(),
            via: Via::Git,
            error: entry.error.map(|code| code.wire().to_owned()),
        })
    }

    /// Every entry in `net.log.1` then `net.log`, at or after `since`. Lines that do
    /// not parse (a torn last line after a crash) are skipped.
    pub fn read(&self, since: Option<Timestamp>) -> Result<Vec<NetLogEntry>> {
        read_entries(&[&self.inner.previous, &self.inner.path], since)
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }
}

fn read_entries(files: &[&Path], since: Option<Timestamp>) -> Result<Vec<NetLogEntry>> {
    let mut entries = Vec::new();
    for file in files {
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue, // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) => {
                return Err(Error::io(
                    file,
                    &err,
                    Suggestion::command(["omaddons", "status"]),
                ));
            }
        };
        entries.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<NetLogEntry>(line).ok())
                .filter(|entry| since.is_none_or(|since| entry.time >= since)),
        );
    }
    Ok(entries)
}

/// Now, to the millisecond.
pub(crate) fn now() -> Timestamp {
    Timestamp::now()
        .round(jiff::Unit::Millisecond)
        .unwrap_or_else(|_| Timestamp::now())
}

fn log_error(path: &Path, err: serde_json::Error) -> Error {
    Error::io(
        path,
        &std::io::Error::other(err),
        Suggestion::command(["omaddons", "status"]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tempdir is the (existing) home; fsx never creates home itself.
    fn paths(dir: &Path) -> Paths {
        let home = dir.to_path_buf();
        Paths::with_roots(
            &home,
            home.join(".config"),
            home.join(".local/state"),
            home.join(".cache"),
        )
    }

    fn entry(path: &str) -> NetLogEntry {
        NetLogEntry {
            time: "2026-10-14T21:00:00Z".parse().unwrap(),
            method: "GET".into(),
            host: "github.com".into(),
            path: path.into(),
            status: Some(200),
            bytes: Some(12),
            duration_ms: 34,
            purpose: "release-json".into(),
            via: Via::Http,
            error: None,
        }
    }

    #[test]
    fn lines_have_the_documented_field_order() {
        let line = serde_json::to_string(&entry("/WeakAuras/WeakAuras2/releases/latest")).unwrap();
        assert_eq!(
            line,
            r#"{"time":"2026-10-14T21:00:00Z","method":"GET","host":"github.com","path":"/WeakAuras/WeakAuras2/releases/latest","status":200,"bytes":12,"duration_ms":34,"purpose":"release-json","via":"http","error":null}"#
        );
    }

    #[test]
    fn append_creates_the_state_dir_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let log = NetLog::new(&paths(dir.path())).unwrap();
        log.append(&entry("/a")).unwrap();
        log.record_git(GitLogEntry {
            host: "codeberg.org".into(),
            path: "/user/repo.git".into(),
            exit_code: Some(0),
            duration_ms: 5,
            purpose: "ls-remote",
            error: None,
        })
        .unwrap();
        let entries = log.read(None).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "/a");
        assert_eq!(
            (
                entries[1].method.as_str(),
                entries[1].via,
                entries[1].status
            ),
            ("GIT", Via::Git, Some(0))
        );
    }

    #[test]
    fn rotates_once_above_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let log = NetLog::with_max_bytes(&p, 300).unwrap();
        for i in 0..10 {
            log.append(&entry(&format!("/{i}"))).unwrap();
        }
        assert!(p.net_log_previous().exists());
        assert!(fs::metadata(p.net_log()).unwrap().len() <= 300 + 400);
        let paths_read: Vec<String> = log
            .read(None)
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(paths_read.last().map(String::as_str), Some("/9"));
        let files: Vec<_> = fs::read_dir(p.state_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(
            files.iter().all(|f| f == "net.log" || f == "net.log.1"),
            "{files:?}"
        );
    }

    #[test]
    fn reader_skips_a_torn_last_line_and_filters_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let log = NetLog::new(&p).unwrap();
        log.append(&entry("/old")).unwrap();
        let mut later = entry("/new");
        later.time = "2026-10-15T00:00:00Z".parse().unwrap();
        log.append(&later).unwrap();
        // A crash mid-append leaves a torn line; written through fsx like every write.
        crate::fsx::sys::append_durable(&p.net_log(), br#"{"time":"2026-10-1"#).unwrap();
        assert_eq!(log.read(None).unwrap().len(), 2);
        let since = "2026-10-14T22:00:00Z".parse().unwrap();
        let recent = log.read(Some(since)).unwrap();
        assert_eq!(
            recent.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
            ["/new"]
        );
    }
}
