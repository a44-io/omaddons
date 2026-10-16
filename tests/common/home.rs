//! End-to-end runs of the binary: synthetic clients in one Wine prefix, a replayed HTTP
//! fixture scenario (`OMADDONS_HTTP_FIXTURES`) and, optionally, a fake `/proc`
//! (`OMADDONS_PROC_ROOT`) with a running client.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::env::TestEnv;
use super::http::{Asset, gh_latest, gh_release_v, paths};
use super::procfs::FakeProc;
use super::trees::{BattlenetRoot, CustomRoot, Prefix};

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

    /// `<cache>/omaddons/releases.json`, the release memo.
    pub fn memo(&self) -> PathBuf {
        self.env.cache().join("omaddons/releases.json")
    }

    /// Writes `memo` as the release memo (built with `common::memo`, whose entries carry
    /// chosen `fetched_at` times).
    pub fn seed_memo(&self, memo: &omaddons::state::memo::ReleaseMemo) {
        let path = self.memo();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, omaddons::state::store::encode(memo).unwrap()).unwrap();
    }

    /// A fresh fixture scenario; later runs replay it.
    pub fn scenario(&mut self, name: &str) -> PathBuf {
        self.http = self.env.root().join(name);
        fs::create_dir_all(&self.http).unwrap();
        self.http.clone()
    }

    /// Installs `old` (a tag and its assets) of the repository at `base`
    /// (`https://github.com/o/r`, installed as `gh:o/r`) on each target id with the
    /// binary, then switches to a fresh scenario where both tags exist and
    /// `releases/latest` points at `new`; returns its directory. Each scenario starts as
    /// a copy of the current one, so earlier fixtures (other addons) keep answering.
    /// The release memo is then aged by a day, as if the install had happened
    /// yesterday, so the next check asks the source again (candidate facts are kept).
    pub fn installed_behind(
        &mut self,
        base: &str,
        old: (&str, &[Asset<'_>]),
        new: (&str, &[Asset<'_>]),
        targets: &[&str],
    ) -> PathBuf {
        let r = format!("gh:{}", base.trim_start_matches("https://github.com/"));
        let installing = self.next_scenario();
        gh_release_v(&installing, base, old.0, old.1);
        gh_latest(&installing, base, old.0);
        for target in targets {
            self.run(&["install", &r, "-t", target]).expect(0);
        }
        let behind = self.next_scenario();
        gh_release_v(&behind, base, new.0, new.1);
        gh_latest(&behind, base, new.0);
        let (mut memo, _) = omaddons::state::memo::load(&paths(&self.env));
        for entry in memo.entries.values_mut() {
            entry.fetched_at -= jiff::SignedDuration::from_hours(24);
        }
        self.seed_memo(&memo);
        behind
    }

    /// Writes `text` as `<config>/omaddons/addons.toml`, the default desired-state file;
    /// returns its path.
    pub fn write_desired(&self, text: &str) -> PathBuf {
        let path = self.env.config().join("omaddons/addons.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    /// Builds the custom client `root` and registers it with `omaddons targets add`;
    /// returns its client dir. Build it under this home's tempdir
    /// (`CustomRoot::at(home.env.root().join("pserver"))`).
    pub fn with_custom(&mut self, root: CustomRoot) -> PathBuf {
        let dir = root.build();
        assert!(
            dir.starts_with(self.env.root()),
            "{} is outside the home",
            dir.display()
        );
        self.run(&["targets", "add", &dir.display().to_string()])
            .expect(0);
        dir
    }

    /// A fresh scenario holding a copy of the current one's fixtures.
    fn next_scenario(&mut self) -> PathBuf {
        let mut n = 1;
        while self.env.root().join(format!("http-{n}")).exists() {
            n += 1;
        }
        let dir = self.env.root().join(format!("http-{n}"));
        copy_dir(&self.http, &dir);
        self.http = dir.clone();
        dir
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
        self.exec_with(args, &[])
    }

    /// Runs `omaddons <args>` with extra environment variables (`OMADDONS_FAILPOINTS`).
    /// A process killed by a signal (a failpoint `abort`) reports code -1.
    pub fn exec_with(&self, args: &[String], extra: &[(&str, &str)]) -> Run {
        let output = self.std_command(args, extra).output().unwrap();
        Run {
            argv: args.to_vec(),
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    /// Starts `omaddons <args> --json` in the background.
    pub fn spawn(&self, args: &[&str], extra: &[(&str, &str)]) -> std::process::Child {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.push("--json".to_owned());
        self.std_command(&argv, extra)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn std_command(&self, args: &[String], extra: &[(&str, &str)]) -> std::process::Command {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_omaddons"));
        self.env.apply_to(&mut cmd);
        cmd.env("OMADDONS_HTTP_FIXTURES", &self.http);
        if let Some(proc) = &self.proc {
            cmd.env("OMADDONS_PROC_ROOT", proc);
        }
        for (key, value) in extra {
            cmd.env(key, value);
        }
        cmd.args(args);
        cmd
    }
}

/// Copies the tree at `from` to `to` (which must not exist yet).
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for item in fs::read_dir(from).unwrap() {
        let item = item.unwrap();
        let target = to.join(item.file_name());
        if item.file_type().unwrap().is_dir() {
            copy_dir(&item.path(), &target);
        } else {
            fs::copy(item.path(), target).unwrap();
        }
    }
}

/// The finished background run of [`Home::spawn`].
pub fn wait(child: std::process::Child, argv: &[&str]) -> Run {
    let output = child.wait_with_output().unwrap();
    Run {
        argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
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
