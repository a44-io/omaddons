//! An isolated environment (DEC-6): every path omaddons derives comes from `HOME`,
//! `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME`, and launcher discovery
//! also reads `XDG_DATA_HOME`; all of them point inside one tempdir.

use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// Variables cleared from every child, so the developer's settings never leak in.
const CLEARED: &[&str] = &[
    "CI",
    "NO_COLOR",
    "TERM",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "OMADDONS_UPDATE_WIRE_NAMES",
    "OMADDONS_ADVANCE_RELEASED_BASELINE",
];

/// Settings every child gets: local times in UTC (M3 common rule 13), a filesystem that
/// clones no file (a debug-build hint), so automatic backups, which clone or do nothing,
/// write nothing in a scene that does not ask for them with `OMADDONS_FS_CLONE=yes`, and
/// 64 GiB free (another), so `status` and the backup commands report the same space on
/// every run; a test about space sets its own `OMADDONS_FS_FREE_MB`.
const SETTINGS: &[(&str, &str)] = &[
    ("TZ", "UTC"),
    ("OMADDONS_FS_CLONE", "no"),
    ("OMADDONS_FS_FREE_MB", "65536"),
];

/// A tempdir holding `home/`, `config/`, `state/`, `cache/` and `home/.local/share`.
pub struct TestEnv {
    /// `None` for an environment at a fixed directory, which is kept.
    dir: Option<tempfile::TempDir>,
    root: PathBuf,
}

impl TestEnv {
    pub fn new() -> Self {
        Self::owning(tempfile::tempdir().unwrap())
    }

    /// [`TestEnv::new`] in a new tempdir under `parent` (the target's tmp dir, on btrfs on
    /// the development machine, where files clone), removed with the environment.
    pub fn new_in(parent: &Path) -> Self {
        Self::owning(tempfile::tempdir_in(parent).unwrap())
    }

    fn owning(dir: tempfile::TempDir) -> Self {
        let root = dir.path().to_path_buf();
        Self::layout(&root);
        Self {
            dir: Some(dir),
            root,
        }
    }

    /// An environment at `root`, which outlives it (`argc bench`'s home).
    pub fn at(root: &Path) -> Self {
        Self::layout(root);
        Self {
            dir: None,
            root: root.to_path_buf(),
        }
    }

    fn layout(root: &Path) {
        for sub in ["home/.local/share", "config", "state", "cache", "no-proc"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
    }

    /// The tempdir itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The variables every child gets, as `KEY=value` lines (for running the binary
    /// outside a test, as `argc bench` does).
    pub fn env_lines(&self) -> Vec<String> {
        self.vars()
            .iter()
            .map(|(key, value)| format!("{key}={}", value.display()))
            .chain(SETTINGS.iter().map(|(key, value)| format!("{key}={value}")))
            .collect()
    }

    /// `HOME`.
    pub fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    /// `XDG_CONFIG_HOME`.
    pub fn config(&self) -> PathBuf {
        self.root().join("config")
    }

    /// `XDG_STATE_HOME`.
    pub fn state(&self) -> PathBuf {
        self.root().join("state")
    }

    /// `XDG_CACHE_HOME`.
    pub fn cache(&self) -> PathBuf {
        self.root().join("cache")
    }

    /// `XDG_DATA_HOME` (`home/.local/share`).
    pub fn data(&self) -> PathBuf {
        self.home().join(".local/share")
    }

    /// The `omaddons` binary with this environment applied.
    pub fn command(&self) -> Command {
        let mut cmd = Command::cargo_bin("omaddons").unwrap();
        for (key, value) in self.vars() {
            cmd.env(key, value);
        }
        cmd.envs(SETTINGS.iter().copied());
        for key in self.removed() {
            cmd.env_remove(key);
        }
        cmd
    }

    /// Applies this environment to any child process.
    pub fn apply_to(&self, cmd: &mut std::process::Command) {
        cmd.envs(self.vars());
        cmd.envs(SETTINGS.iter().copied());
        for key in self.removed() {
            cmd.env_remove(key);
        }
    }

    fn vars(&self) -> [(&'static str, PathBuf); 6] {
        [
            ("HOME", self.home()),
            ("XDG_CONFIG_HOME", self.config()),
            ("XDG_STATE_HOME", self.state()),
            ("XDG_CACHE_HOME", self.cache()),
            ("XDG_DATA_HOME", self.data()),
            // An empty `/proc` (debug builds read it): a WoW client running on this
            // machine is never a test's running client, nor its process-found target.
            // A test with a running client sets its own after this.
            ("OMADDONS_PROC_ROOT", self.root().join("no-proc")),
        ]
    }

    /// [`CLEARED`] plus every other `OMADDONS_*` variable of the test process.
    fn removed(&self) -> Vec<String> {
        let mut keys: Vec<String> = CLEARED.iter().map(|k| (*k).to_owned()).collect();
        keys.extend(
            std::env::vars_os()
                .filter_map(|(k, _)| k.into_string().ok())
                .filter(|k| {
                    k.starts_with("OMADDONS_")
                        && k != "OMADDONS_PROC_ROOT"
                        && !SETTINGS.iter().any(|(set, _)| set == k)
                }),
        );
        keys
    }
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TestEnv {
    /// Gives the owner write permission back on every directory of an environment it owns,
    /// just before the tempdir is removed. Tests seal directories on purpose (a fixture's
    /// `0o500` directory, and the exact copies omaddons keeps of it in snapshots and
    /// backups), and `TempDir` gives up silently at the first entry it cannot unlink, which
    /// left the whole tree in `/tmp` or `target/tmp` on every run. A kept environment
    /// ([`TestEnv::at`]) is left as it is.
    fn drop(&mut self) {
        if self.dir.is_some() {
            unseal(&self.root);
        }
    }
}

/// Adds `u+rwx` to `dir` and to every directory under it, never following a symlink.
/// Best effort: a directory it cannot open is left behind, as before.
fn unseal(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode | 0o700)).ok();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            unseal(&entry.path());
        }
    }
}
