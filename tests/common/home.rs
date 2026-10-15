//! End-to-end runs of the binary: synthetic clients in one Wine prefix, a replayed HTTP
//! fixture scenario (`OMADDONS_HTTP_FIXTURES`) and, optionally, a fake `/proc`
//! (`OMADDONS_PROC_ROOT`) with a running client.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use super::env::TestEnv;
use super::procfs::FakeProc;
use super::trees::{BattlenetRoot, Prefix};

/// Synthetic clients in one Wine prefix, a fixture scenario and, optionally, a fake
/// `/proc`.
pub struct Home {
    pub env: TestEnv,
    pub root: PathBuf,
    pub prefix: PathBuf,
    pub http: PathBuf,
    pub proc: Option<PathBuf>,
}

impl Home {
    /// A Battle.net root holding `products` (product codes).
    pub fn new(products: &[&str]) -> Self {
        let env = TestEnv::new();
        let prefix = Prefix::new(&env, "battlenet");
        let mut root = BattlenetRoot::at(prefix.wow_root());
        for code in products {
            root = root.product(code);
        }
        let root = root.build();
        let http = env.root().join("http");
        fs::create_dir_all(&http).unwrap();
        Self {
            root,
            prefix: prefix.path().to_path_buf(),
            http,
            proc: None,
            env,
        }
    }

    /// A product's client dir (`_retail_`, `_classic_`, …).
    pub fn client(&self, folder: &str) -> PathBuf {
        self.root.join(folder)
    }

    pub fn addons(&self, folder: &str) -> PathBuf {
        self.client(folder).join("Interface/AddOns")
    }

    /// `<state>/omaddons`.
    pub fn state(&self) -> PathBuf {
        self.env.state().join("omaddons")
    }

    /// A fresh fixture scenario; later runs replay it.
    pub fn scenario(&mut self, name: &str) -> PathBuf {
        self.http = self.env.root().join(name);
        fs::create_dir_all(&self.http).unwrap();
        self.http.clone()
    }

    /// A retail client process in the fake `/proc`.
    pub fn retail_running(&mut self) {
        let prefix = self.prefix.display().to_string();
        let proc = FakeProc::new(self.env.root().join("proc")).process(
            4242,
            true,
            "Wow.exe",
            &["C:\\Program Files (x86)\\World of Warcraft\\_retail_\\Wow.exe"],
            &[("WINEPREFIX", &prefix)],
        );
        self.proc = Some(proc.root().to_path_buf());
    }

    /// No client runs any more.
    pub fn nothing_running(&mut self) {
        let empty = self.env.root().join("proc-empty");
        fs::create_dir_all(&empty).unwrap();
        self.proc = Some(empty);
    }

    /// Runs `omaddons <args> --json`.
    pub fn run(&self, args: &[&str]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.push("--json".to_owned());
        self.exec(&argv)
    }

    /// Runs `omaddons <args>` verbatim.
    pub fn exec(&self, args: &[String]) -> Run {
        let mut cmd = self.env.command();
        cmd.env("OMADDONS_HTTP_FIXTURES", &self.http);
        if let Some(proc) = &self.proc {
            cmd.env("OMADDONS_PROC_ROOT", proc);
        }
        let output = cmd.args(args).output().unwrap();
        Run {
            argv: args.to_vec(),
            code: output.status.code().unwrap(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

/// One finished run.
pub struct Run {
    pub argv: Vec<String>,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    /// The one stdout envelope; every stderr line must be NDJSON.
    pub fn envelope(&self) -> Value {
        assert_eq!(
            self.stdout.lines().count(),
            1,
            "one envelope\n{}",
            self.context()
        );
        for line in self.stderr.lines() {
            assert!(
                serde_json::from_str::<Value>(line).is_ok(),
                "stderr is NDJSON\n{}",
                self.context()
            );
        }
        serde_json::from_str(&self.stdout).unwrap()
    }

    /// The envelope, asserting the exit code.
    pub fn expect(&self, code: i32) -> Value {
        assert_eq!(self.code, code, "{}", self.context());
        self.envelope()
    }

    pub fn context(&self) -> String {
        format!(
            "argv: {:?}\nexit: {}\nstdout: {}\nstderr: {}",
            self.argv, self.code, self.stdout, self.stderr
        )
    }
}
