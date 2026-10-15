//! An isolated environment (DEC-6): every path omaddons derives comes from `HOME`,
//! `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME`, and launcher discovery
//! also reads `XDG_DATA_HOME`; all of them point inside one tempdir.

use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// Variables cleared from every child, so the developer's settings never leak in.
const CLEARED: &[&str] = &["CI", "NO_COLOR", "TERM", "XDG_DATA_DIRS", "XDG_CONFIG_DIRS"];

/// A tempdir holding `home/`, `config/`, `state/`, `cache/` and `home/.local/share`.
pub struct TestEnv {
    dir: tempfile::TempDir,
}

impl TestEnv {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["home/.local/share", "config", "state", "cache"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        Self { dir }
    }

    /// The tempdir itself.
    pub fn root(&self) -> &Path {
        self.dir.path()
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
        for key in self.removed() {
            cmd.env_remove(key);
        }
        cmd
    }

    /// Applies this environment to any child process.
    pub fn apply_to(&self, cmd: &mut std::process::Command) {
        cmd.envs(self.vars());
        for key in self.removed() {
            cmd.env_remove(key);
        }
    }

    fn vars(&self) -> [(&'static str, PathBuf); 5] {
        [
            ("HOME", self.home()),
            ("XDG_CONFIG_HOME", self.config()),
            ("XDG_STATE_HOME", self.state()),
            ("XDG_CACHE_HOME", self.cache()),
            ("XDG_DATA_HOME", self.data()),
        ]
    }

    /// [`CLEARED`] plus every `OMADDONS_*` variable of the test process.
    fn removed(&self) -> Vec<String> {
        let mut keys: Vec<String> = CLEARED.iter().map(|k| (*k).to_owned()).collect();
        keys.extend(
            std::env::vars_os()
                .filter_map(|(k, _)| k.into_string().ok())
                .filter(|k| k.starts_with("OMADDONS_")),
        );
        keys
    }
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}
