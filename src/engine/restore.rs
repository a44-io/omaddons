//! A whole-`WTF/` restore (M3-D24): one journaled `RENAME_EXCHANGE` of the client's
//! `WTF` directory with a staged copy of a backup, so `WTF/` is wholly old or wholly new
//! at every instant, the old tree kept as the transaction's `WTF.previous` for undo.
//!
//! The topology preflight runs before anything is copied: a restore needs `WTF` to be a
//! plain directory on the client's own mount, the stage on that mount too (a rename
//! across mounts fails, whatever their devices say), and a filesystem that can exchange.
//! There is no fallback to two renames: it would leave a moment with no `WTF/`, in which
//! a game launched writes a fresh default one.

use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{ScopeTable, WriteSession, Writer, mount_id, probe_exchange};
use crate::state::paths::ClientPaths;

/// Whether the exchange a restore needs was proved where it will run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Exchange {
    /// Probed with two files in the transaction's stage.
    Verified,
    /// Not probed: a dry run writes nothing.
    Unverified,
}

/// What the preflight established; the commit window compares the mounts again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    /// The mount of the client dir.
    pub client_mount: u64,
    /// The live `WTF/`, `None` when there is none yet (the exchange is then a plain
    /// `Noreplace` into place).
    pub wtf: Option<PathBuf>,
    /// The mount the stage is on, which is the client's.
    pub stage_mount: u64,
    pub exchange: Exchange,
}

/// Where a real run probes the exchange: the transaction's own stage, which goes with
/// it (the orphan sweep reclaims it after a crash), with the run's scopes.
pub struct Probe<'a> {
    pub scopes: &'a ScopeTable,
    pub dir: &'a Path,
}

/// The topology preflight (M3-D24 step 2), before any copying: `WTF` a plain directory
/// on the client dir's mount (`wtf_is_mount_point`), the stage on that mount too
/// (`stage_cross_mount`), and, on a real run, an exchange the filesystem supports
/// (`no_rename_exchange`). A dry run passes no `probe`: it writes nothing and reports
/// the exchange `unverified`. `backup_dir` is the backup's `WTF/`, named by the refusal
/// as what a user can copy by hand.
pub fn preflight(
    client: &ClientPaths,
    backup_dir: &Path,
    probe: Option<Probe<'_>>,
) -> Result<Topology> {
    let topology = topology(client, backup_dir, &mount_id)?;
    let exchange = match probe {
        None => Exchange::Unverified,
        Some(probe) => {
            let mut s = WriteSession::untracked(Writer::BackupRestore, probe.scopes, "probe");
            match probe_exchange(&mut s, probe.dir) {
                Ok(true) => Exchange::Verified,
                Ok(false) => return Err(unsupported("no_rename_exchange", backup_dir)),
                Err(err) => return Err(err.into()),
            }
        }
    };
    Ok(Topology {
        exchange,
        ..topology
    })
}

/// [`preflight`]'s mount checks, with the mount ids `mount` gives.
fn topology(
    client: &ClientPaths,
    backup_dir: &Path,
    mount: &dyn Fn(&Path) -> io::Result<u64>,
) -> Result<Topology> {
    let read = |path: &Path| {
        mount(path)
            .map_err(|err| Error::io(path, &err, Suggestion::command(["omaddons", "status"])))
    };
    let client_mount = read(client.client_dir())?;
    let wtf = client.wtf();
    let present = match std::fs::symlink_metadata(&wtf) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(Error::new(
                ErrorCode::ScopeDenied,
                "the client's WTF/ is a symlink, which a restore would replace by a directory",
                Suggestion::text(
                    "copy what you need from the backup folder into the folder the link points to, with the game closed",
                ),
            )
            .details(serde_json::json!({
                "reason": "wtf_symlinked",
                "backup_dir": backup_dir.display().to_string(),
            })));
        }
        Ok(meta) if meta.is_dir() => true,
        Ok(_) => {
            return Err(Error::new(
                ErrorCode::ScopeDenied,
                "the client's WTF is not a directory",
                Suggestion::text("move it aside, then restore"),
            ));
        }
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => {
            return Err(Error::io(
                &wtf,
                &err,
                Suggestion::command(["omaddons", "status"]),
            ));
        }
    };
    if present && read(&wtf)? != client_mount {
        return Err(unsupported("wtf_is_mount_point", backup_dir));
    }
    // The stage lives under `Interface/.omaddons/stage`, created on first use: what
    // decides its mount is its nearest existing ancestor.
    let stage = client.meta_dir().join("stage");
    let existing = stage
        .ancestors()
        .find(|p| p.exists())
        .unwrap_or(client.client_dir());
    let stage_mount = read(existing)?;
    if stage_mount != client_mount {
        return Err(unsupported("stage_cross_mount", backup_dir));
    }
    Ok(Topology {
        client_mount,
        wtf: present.then_some(wtf),
        stage_mount,
        exchange: Exchange::Unverified,
    })
}

/// `filesystem_unsupported` for `reason`, naming the backup folder a user can copy by
/// hand. Not retryable, and no command helps: a partial restore needs the same.
pub fn unsupported(reason: &str, backup_dir: &Path) -> Error {
    let why = match reason {
        "wtf_is_mount_point" => "the client's WTF/ is a mount point",
        "stage_cross_mount" => "the client's Interface/.omaddons/ is on another mount than WTF/",
        _ => "the client's filesystem cannot exchange two directories atomically",
    };
    Error::new(
        ErrorCode::FilesystemUnsupported,
        format!("a restore cannot run here: {why}"),
        Suggestion::text(
            "a restore needs WTF/ to be a plain directory on the same mount as Interface/, on a filesystem with atomic exchange (btrfs, ext4, xfs, tmpfs); otherwise copy what you need from the backup folder by hand with the game closed",
        ),
    )
    .details(serde_json::json!({
        "reason": reason,
        "backup_dir": backup_dir.display().to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use super::*;

    /// A client with `WTF/` (unless `wtf` is false) and `Interface/`.
    fn client(wtf: bool) -> (tempfile::TempDir, ClientPaths) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir_all(root.join("Interface/AddOns")).unwrap();
        if wtf {
            fs::create_dir_all(root.join("WTF/Account")).unwrap();
        }
        (dir, ClientPaths::new(root))
    }

    /// Mount ids by path; any other path is on mount 1.
    fn mounts(planted: &[(&Path, u64)]) -> impl Fn(&Path) -> io::Result<u64> {
        let planted: BTreeMap<PathBuf, u64> =
            planted.iter().map(|(p, m)| (p.to_path_buf(), *m)).collect();
        move |path: &Path| Ok(planted.get(path).copied().unwrap_or(1))
    }

    fn reason(err: &Error) -> (ErrorCode, String) {
        (
            err.code,
            err.details["reason"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
    }

    #[test]
    fn a_plain_wtf_on_the_clients_mount_passes() {
        let (_dir, c) = client(true);
        let backup = Path::new("/backups/01M6K2VA/WTF");
        let t = topology(&c, backup, &mounts(&[])).unwrap();
        assert_eq!(
            (t.client_mount, t.wtf, t.stage_mount, t.exchange),
            (1, Some(c.wtf()), 1, Exchange::Unverified)
        );
        // No WTF yet: allowed, the exchange is then a Noreplace.
        let (_dir, bare) = client(false);
        assert_eq!(topology(&bare, backup, &mounts(&[])).unwrap().wtf, None);
    }

    #[test]
    fn a_mount_point_or_a_cross_mount_stage_is_refused() {
        let (_dir, c) = client(true);
        let backup = Path::new("/backups/01M6K2VA/WTF");
        let wtf = c.wtf();
        let err = topology(&c, backup, &mounts(&[(&wtf, 2)])).unwrap_err();
        assert_eq!(
            reason(&err),
            (
                ErrorCode::FilesystemUnsupported,
                "wtf_is_mount_point".into()
            )
        );
        assert_eq!(err.details["backup_dir"], "/backups/01M6K2VA/WTF");
        assert_eq!(err.fix.argv(), None, "no command helps");
        // The stage's nearest existing ancestor is Interface/ here.
        let interface = c.interface();
        let err = topology(&c, backup, &mounts(&[(&interface, 3)])).unwrap_err();
        assert_eq!(
            reason(&err),
            (ErrorCode::FilesystemUnsupported, "stage_cross_mount".into())
        );
    }

    #[test]
    fn a_symlinked_wtf_is_refused() {
        let (dir, c) = client(false);
        let elsewhere = dir.path().join("sync");
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, c.client_dir().join("WTF")).unwrap();
        let err = topology(&c, Path::new("/b"), &mounts(&[])).unwrap_err();
        assert_eq!(
            reason(&err),
            (ErrorCode::ScopeDenied, "wtf_symlinked".into())
        );
    }

    #[test]
    fn a_real_run_probes_the_exchange_and_a_dry_run_writes_nothing() {
        let (_dir, c) = client(true);
        let paths = crate::state::paths::Paths::with_roots(
            c.client_dir(),
            c.client_dir().join("c"),
            c.client_dir().join("s"),
            c.client_dir().join("k"),
        );
        let scopes = ScopeTable::new(&paths, &[c.client_dir().to_path_buf()], None).unwrap();
        let backup = Path::new("/backups/x/WTF");
        let dry = preflight(&c, backup, None).unwrap();
        assert_eq!(dry.exchange, Exchange::Unverified);
        assert!(!c.meta_dir().exists(), "a dry run writes nothing");
        let probe_dir = c.meta_dir().join("stage/T/probe");
        let probe = Probe {
            scopes: &scopes,
            dir: &probe_dir,
        };
        assert_eq!(
            preflight(&c, backup, Some(probe)).unwrap().exchange,
            Exchange::Verified
        );
        #[cfg(debug_assertions)]
        {
            let again = c.meta_dir().join("stage/U/probe");
            let probe = Probe {
                scopes: &scopes,
                dir: &again,
            };
            let err = crate::fsx::failpoint::with_spec("fsx.renameat2.einval", || {
                preflight(&c, backup, Some(probe))
            })
            .unwrap_err();
            assert_eq!(
                reason(&err),
                (
                    ErrorCode::FilesystemUnsupported,
                    "no_rename_exchange".into()
                )
            );
        }
    }
}
