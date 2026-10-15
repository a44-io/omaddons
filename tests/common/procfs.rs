//! A fake `/proc` for running-client detection: `<dir>/<pid>/{comm,cmdline,environ}`.
//! Tests cannot chown, so another user's process is simulated by making its files
//! unreadable; that only works when the tests do not run as root ([`running_as_root`]).

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Whether the test process runs as root, where chmod 000 does not hide files.
pub fn running_as_root() -> bool {
    fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

pub struct FakeProc {
    dir: PathBuf,
}

impl FakeProc {
    pub fn new(dir: PathBuf) -> Self {
        fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    pub fn root(&self) -> &Path {
        &self.dir
    }

    /// A process. `comm` gets a trailing newline; `cmdline` and `environ` are
    /// NUL-separated and NUL-terminated, as the kernel writes them. `own = false` makes
    /// the files unreadable, like another user's process.
    pub fn process(
        self,
        pid: u32,
        own: bool,
        comm: &str,
        cmdline: &[&str],
        environ: &[(&str, &str)],
    ) -> Self {
        let dir = self.dir.join(pid.to_string());
        fs::create_dir_all(&dir).unwrap();
        let joined = |items: Vec<String>| {
            let mut bytes = Vec::new();
            for item in items {
                bytes.extend_from_slice(item.as_bytes());
                bytes.push(0);
            }
            bytes
        };
        fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        fs::write(
            dir.join("cmdline"),
            joined(cmdline.iter().map(|s| (*s).to_owned()).collect()),
        )
        .unwrap();
        fs::write(
            dir.join("environ"),
            joined(environ.iter().map(|(k, v)| format!("{k}={v}")).collect()),
        )
        .unwrap();
        if !own {
            for file in ["comm", "cmdline", "environ"] {
                fs::set_permissions(dir.join(file), fs::Permissions::from_mode(0o000)).unwrap();
            }
        }
        self
    }
}
