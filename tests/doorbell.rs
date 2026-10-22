//! The doorbell end to end (M3-D4): `<cache>/omaddons/doorbell.json` rings after every
//! command that can change what `status --cached` reports, naming the command and its
//! actor, and never after a read. Each step starts from a sentinel doorbell an hour old:
//! a ring replaces its bytes and moves its mtime, and no ring leaves both as they were.

mod common;

use std::fs;

use common::addons::{toc, unpack};
use common::engine::Scene;
use serde_json::Value;
use support::{Bell, Steps, journals, read_only, warning_codes};

#[cfg(test)]
mod support {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use serde_json::Value;

    use super::common::addons::toc;
    use super::common::archives::zip_text;
    use super::common::home::{Home, Run};
    use super::common::http::{Asset, gh_release};

    const SENTINEL: &str = "{\"schema_version\":\"1.0\",\"at\":\"2000-01-01T00:00:00Z\",\"command\":\"sentinel\",\"actor\":\"human\"}\n";
    const MAINLINE: &[(&str, u32)] = &[("mainline", 120_105)];

    /// `<cache>/omaddons/doorbell.json` of a home.
    pub struct Bell {
        pub path: PathBuf,
    }

    impl Bell {
        pub fn of(home: &Home) -> Self {
            Self {
                path: home.env.cache().join("omaddons/doorbell.json"),
            }
        }

        /// Puts the sentinel back, an hour old.
        pub fn arm(&self) {
            fs::create_dir_all(self.path.parent().unwrap()).unwrap();
            fs::write(&self.path, SENTINEL).unwrap();
            let past = SystemTime::now() - Duration::from_secs(3600);
            fs::File::options()
                .write(true)
                .open(&self.path)
                .unwrap()
                .set_modified(past)
                .unwrap();
        }

        /// The doorbell when it rang since [`Bell::arm`], `None` when it did not.
        pub fn rung(&self, run: &Run) -> Option<Value> {
            let bytes = fs::read(&self.path).unwrap();
            let mtime = fs::metadata(&self.path).unwrap().modified().unwrap();
            let armed = SystemTime::now() - Duration::from_secs(1800);
            if bytes == SENTINEL.as_bytes() {
                assert!(mtime < armed, "rewritten as it was\n{}", run.context());
                return None;
            }
            assert!(mtime > armed, "new bytes, old mtime\n{}", run.context());
            Some(serde_json::from_slice(&bytes).unwrap())
        }
    }

    /// A retail client, its doorbell armed before each step.
    pub struct Steps {
        pub home: Home,
        pub bell: Bell,
    }

    impl Steps {
        pub fn new() -> Self {
            let home = Home::new(&["wow"]);
            let bell = Bell::of(&home);
            Self { home, bell }
        }

        /// `argv` (with `--json`, run with `extra` set), which must exit 0, rings the
        /// doorbell for `command` and `actor`; returns the doorbell.
        pub fn rings_with(
            &self,
            argv: &[&str],
            extra: &[(&str, &str)],
            command: &str,
            actor: &str,
        ) -> Value {
            self.bell.arm();
            let mut args: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
            args.push("--json".to_owned());
            let run = self.home.exec_with(&args, extra);
            run.expect(0);
            let bell = self.bell.rung(&run);
            assert!(bell.is_some(), "it did not ring\n{}", run.context());
            let bell = bell.unwrap();
            assert_eq!(
                (&bell["schema_version"], &bell["command"], &bell["actor"]),
                (
                    &Value::from("1.0"),
                    &Value::from(command),
                    &Value::from(actor)
                ),
                "{}",
                run.context()
            );
            bell
        }

        /// `argv` rings the doorbell for `command`, run by an agent (stdin is no
        /// terminal).
        pub fn rings(&self, argv: &[&str], command: &str) -> Value {
            self.rings_with(argv, &[], command, "agent")
        }

        /// `argv` (with `--json`), which must exit 0, leaves the doorbell as it was.
        pub fn silent(&self, argv: &[&str]) {
            self.bell.arm();
            let run = self.home.run(argv);
            run.expect(0);
            assert_eq!(self.bell.rung(&run), None, "it rang\n{}", run.context());
        }

        /// `<root>/dl/<name>.zip`: one folder `<name>` loading on retail; returns its
        /// path.
        pub fn local_zip(&self, name: &str) -> String {
            let dl = self.home.env.root().join("dl");
            fs::create_dir_all(&dl).unwrap();
            let path = dl.join(format!("{name}.zip"));
            let toc_text = toc("120105", &["## Version: 1"]);
            let toc_path = format!("{name}/{name}.toc");
            fs::write(&path, zip_text(&[(&toc_path, &toc_text)])).unwrap();
            path.display().to_string()
        }

        /// Serves `tag` of `gh:o/<repo>` as its latest release: one folder, the
        /// repository's name capitalised, loading on retail.
        pub fn release(&self, repo: &str, tag: &str) {
            let name = capitalised(repo);
            let toc_path = format!("{name}/{name}.toc");
            let toc_text = toc("120105", &[&format!("## Version: {tag}")]);
            let file = format!("{name}.zip");
            let assets: [Asset<'_>; 1] = [(&file, MAINLINE, vec![(&toc_path, &toc_text)])];
            gh_release(
                &self.home.http,
                &format!("https://github.com/o/{repo}"),
                tag,
                &assets,
            );
        }
    }

    fn capitalised(s: &str) -> String {
        let mut chars = s.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    }

    /// `dir` made read-only, or writable again.
    pub fn read_only(dir: &Path, on: bool) {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = if on { 0o555 } else { 0o755 };
        fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Unfinished journals in `<state>/omaddons/journal`.
    pub fn journals(state: &Path) -> Vec<PathBuf> {
        fs::read_dir(state.join("journal"))
            .map(|l| l.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default()
    }

    /// The envelope's warning codes, in order.
    pub fn warning_codes(envelope: &Value) -> Vec<&str> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap())
            .collect()
    }
}

#[test]
fn every_change_rings_with_its_command_and_actor_and_no_read_does() {
    let s = Steps::new();
    s.release("foo", "v1");
    s.release("qux", "v1");
    let bar = s.local_zip("Bar");
    let baz = s.local_zip("Baz");

    let bell = s.rings_with(
        &["install", "gh:o/foo", "-t", "retail", "--actor", "widget"],
        &[],
        "install",
        "widget",
    );
    assert!(bell["at"].is_string());
    s.rings(&["install", &bar, "-t", "retail"], "install");

    // Reads, and changes that end with nothing to do.
    s.silent(&["status"]);
    s.silent(&["status", "--cached"]);
    s.silent(&["list"]);
    s.silent(&["history"]);
    s.silent(&["net-log"]);
    s.silent(&["targets"]);
    s.silent(&["config"]);
    s.silent(&["config", "get", "default_channel"]);
    s.silent(&["config", "path"]);
    s.silent(&["help"]);
    s.silent(&["info", "gh:o/foo"]);
    s.silent(&["install", &baz, "-t", "retail", "--dry-run"]);
    s.silent(&["remove", "Bar", "-t", "retail", "--dry-run"]);
    s.silent(&["install", &bar, "-t", "retail"]);
    s.silent(&["install", "gh:o/foo", "-t", "retail", "--offline"]);
    // An online repeat revalidates (README: install always asks the source), and the
    // probe moves the memo's `fetched_at`, which `status` reports as `checked_at`.
    s.rings(&["install", "gh:o/foo", "-t", "retail"], "install");

    // Update checks: the first one fills an empty memo, a repeat inside the window
    // answers from it.
    fs::remove_file(s.home.memo()).unwrap();
    s.rings(&["outdated"], "outdated");
    s.silent(&["outdated"]);

    s.release("foo", "v2");
    s.rings(&["update", "--refresh"], "update");
    s.rings(&["pin", "Foo", "-t", "retail"], "pin");
    s.rings(&["unpin", "Foo", "-t", "retail"], "unpin");
    s.rings(&["remove", "Bar", "-t", "retail"], "remove");
    s.rings(&["undo"], "undo");

    // Its own file: `export --write` below refuses an `addons.toml` it did not write.
    let desired = s.home.env.root().join("desired.toml");
    fs::write(
        &desired,
        "schema_version = \"1.0\"\n\n[target.\"retail\"]\naddons = [\"gh:o/qux\"]\n",
    )
    .unwrap();
    let desired = desired.display().to_string();
    s.rings(&["sync", &desired], "sync");
    s.silent(&["sync", &desired]);

    let catalog = s.home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&catalog).unwrap();
    fs::write(
        catalog.join("50-doorbell.toml"),
        "schema_version = \"1.0\"\n\n[[addon]]\nname = \"Adoptee\"\nref = \"gh:o/adoptee\"\nfolders = [\"Adoptee\"]\n",
    )
    .unwrap();
    let adoptee = toc("120105", &["## Title: Adoptee", "## Version: 3"]);
    unpack(
        &s.home.addons("_retail_"),
        &[("Adoptee/Adoptee.toc", &adoptee)],
    );
    s.rings(
        &["adopt", "--apply", "--no-verify", "-t", "retail"],
        "adopt",
    );

    s.silent(&["export"]);
    s.rings(&["export", "--write"], "export");

    // A prune that deletes a backup; one with nothing to delete, and a preview, do not.
    let retail = s.home.client("_retail_");
    for captured in ["2025-01-01T08:00:00Z", "2025-01-01T20:00:00Z"] {
        let at = captured.parse().unwrap();
        common::backups::seed_backup(&retail, omaddons::backup::BackupKind::Auto, at, 1);
    }
    s.silent(&["backup", "prune", "-t", "retail", "--dry-run"]);
    s.rings(&["backup", "prune", "-t", "retail"], "backup prune");
    s.silent(&["backup", "prune", "-t", "retail"]);
}

#[test]
fn settings_commands_ring_and_omaddons_now_sets_the_time() {
    let s = Steps::new();
    let bell = s.rings_with(
        &[
            "config",
            "set",
            "default_channel",
            "beta",
            "--actor",
            "widget",
        ],
        &[("OMADDONS_NOW", "2026-10-21T12:00:00Z")],
        "config set",
        "widget",
    );
    assert_eq!(bell["at"], "2026-10-21T12:00:00Z");
    s.silent(&["config", "set", "default_channel", "beta"]);

    s.rings(&["targets", "rename", "retail", "main"], "targets rename");
    s.rings(&["targets", "default", "main"], "targets default");

    s.bell.arm();
    let set = s.home.exec_input(
        &[
            "auth".into(),
            "github".into(),
            "set".into(),
            "--json".into(),
        ],
        &[],
        b"ghp_doorbell\n",
    );
    set.expect(0);
    assert_eq!(
        s.bell.rung(&set).map(|b| b["command"].clone()),
        Some(Value::from("auth github")),
        "{}",
        set.context()
    );
    s.silent(&["auth", "github", "--offline"]);
    s.rings(&["auth", "github", "clear"], "auth github");
}

#[test]
fn a_recovery_that_rolled_a_journal_rings_and_one_with_nothing_to_do_does_not() {
    let scene = Scene::new();
    scene.install("gh:o/foo", "v1");
    let Scene { home, dl } = scene;
    let s = Steps {
        bell: Bell::of(&home),
        home,
    };
    s.rings(
        &["config", "set", "default_channel", "stable"],
        "config set",
    );

    // A commit that dies after `prepared` leaves a journal for the next change command.
    let crash = |zip: &str| {
        let args: Vec<String> = [
            "install",
            "gh:o/foo",
            "--file",
            &dl.join(format!("{zip}.zip")).display().to_string(),
            "-t",
            "retail",
            "--offline",
            "--json",
        ]
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
        let run = s.home.exec_with(
            &args,
            &[("OMADDONS_FAILPOINTS", "engine.journal.prepared=abort")],
        );
        assert_eq!(run.code, -1, "{}", run.context());
        assert_eq!(journals(&s.home.state()).len(), 1);
    };

    // Recovered by a settings command that changes nothing else.
    crash("v2");
    s.bell.arm();
    let run = s.home.run(&["config", "set", "default_channel", "stable"]);
    let envelope = run.expect(0);
    assert_eq!(
        warning_codes(&envelope),
        ["journal_recovered"],
        "{}",
        run.context()
    );
    let bell = s.bell.rung(&run).expect("recovery rang");
    assert_eq!(bell["command"], "config set");
    assert!(journals(&s.home.state()).is_empty());
    s.silent(&["config", "set", "default_channel", "stable"]);

    // Recovered by a change command whose plan then has nothing to commit.
    crash("v2");
    let v1 = dl.join("v1.zip").display().to_string();
    let install = [
        "install",
        "gh:o/foo",
        "--file",
        &v1,
        "-t",
        "retail",
        "--offline",
    ];
    s.bell.arm();
    let run = s.home.run(&install);
    let envelope = run.expect(0);
    assert_eq!(
        envelope["data"]["items"][0]["status"],
        "unchanged",
        "{}",
        run.context()
    );
    assert_eq!(
        warning_codes(&envelope),
        ["journal_recovered"],
        "{}",
        run.context()
    );
    assert_eq!(
        s.bell.rung(&run).map(|b| b["command"].clone()),
        Some(Value::from("install"))
    );
    s.silent(&install);
}

#[test]
fn a_doorbell_that_cannot_be_written_changes_no_outcome() {
    // The same commands in two homes, one whose doorbell cannot be written.
    let (ok, broken) = (Steps::new(), Steps::new());
    // A directory where the doorbell belongs: the ring's rename onto it fails.
    let blocked = broken.bell.path.join("keep");
    fs::create_dir_all(&blocked).unwrap();
    let outcome = |s: &Steps, argv: &[&str]| {
        let run = s.home.run(argv);
        let envelope = run.envelope();
        let codes: Vec<String> = warning_codes(&envelope)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let statuses: Vec<Value> = envelope["data"]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| i["status"].clone())
            .collect();
        (run.code, envelope["ok"].clone(), codes, statuses)
    };
    for s in [&ok, &broken] {
        let bar = s.local_zip("Bar");
        let installed = outcome(s, &["install", &bar, "-t", "retail", "-v"]);
        assert_eq!(installed.3, [Value::from("installed")]);
        let removed = outcome(s, &["remove", "Bar", "-t", "retail"]);
        assert_eq!(removed.0, 0);
    }
    for argv in [
        &["install", "./nowhere.zip", "-t", "retail"][..],
        &["config", "set", "default_channel", "beta"],
        &["undo"],
    ] {
        assert_eq!(outcome(&ok, argv), outcome(&broken, argv), "{argv:?}");
    }
    assert!(blocked.is_dir() && broken.bell.path.is_dir());
    assert!(ok.bell.path.is_file());

    // A read-only cache directory (unless this runs as root, which writes anyway).
    fs::remove_dir_all(&broken.bell.path).unwrap();
    let cache = broken.bell.path.parent().unwrap().to_path_buf();
    read_only(&cache, true);
    let writable = fs::write(cache.join("probe"), b"").is_ok();
    let argv = ["config", "set", "default_channel", "alpha"];
    let (expected, got) = (outcome(&ok, &argv), outcome(&broken, &argv));
    read_only(&cache, false);
    assert_eq!(expected, got);
    if !writable {
        assert!(!broken.bell.path.exists());
    }
}
