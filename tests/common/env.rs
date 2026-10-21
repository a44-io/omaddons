//! An isolated environment (DEC-6): every path omaddons derives comes from `HOME`,
//! `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME`, and launcher discovery
//! also reads `XDG_DATA_HOME`; all of them point inside one tempdir.

use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// Variables cleared from every child, so the developer's settings never leak in.
const CLEARED: &[&str] = &["CI", "NO_COLOR", "TERM", "XDG_DATA_DIRS", "XDG_CONFIG_DIRS"];

/// Settings every child gets: local times in UTC (M3 common rule 13), and a filesystem
/// that clones no file (a debug-build hint), so automatic backups, which clone or do
/// nothing, write nothing in a scene that does not ask for them with
/// `OMADDONS_FS_CLONE=yes`.
const SETTINGS: &[(&str, &str)] = &[("TZ", "UTC"), ("OMADDONS_FS_CLONE", "no")];

/// A tempdir holding `home/`, `config/`, `state/`, `cache/` and `home/.local/share`.
pub struct TestEnv {
    /// `None` for an environment at a fixed directory, which is kept.
    dir: Option<tempfile::TempDir>,
    root: PathBuf,
}

impl TestEnv {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
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
