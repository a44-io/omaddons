//! `omaddons backup restore <id> --addon` end to end (M3-D27, M3-D28 rule 3, M3-D29,
//! M3-D46, M3-D47): one addon's SavedVariables come back from a backup, account-wide and
//! per character, and nothing else in `WTF/` changes; the folders come from the backup's
//! own addon record (after a removal, after a folder rename), a folder another addon owns
//! now is refused rather than broadened, an unknown name offers the backup's folders; a
//! file created since is moved aside and undo returns every file, undoably; every refusal
//! (a running client, a filesystem that cannot exchange, a damaged backup, a file the
//! game rewrote before the lock, a stale preview) comes before anything live moves; and
//! partial restores conflict with undo where SavedVariables are written, both ways. Every
//! run is checked for the fake account, realm and character names on both streams.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::home::{Home, Run};
use common::privacy::assert_private_all;
use serde_json::{Value, json};
use support::{Scene, bound, files_of, rest_hash, run};

/// WeakAuras's account-wide and character files, as `Wtf::saved_variables` lays them.
const WA_ACCOUNT: &str = "Account/FAKEACCT/SavedVariables/WeakAuras.lua";
const WA_CHAR: &str = "Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/WeakAuras.lua";
/// WeakAurasOptions's character file, which a play session creates after the backup.
const OPTIONS: &str = "Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/WeakAurasOptions.lua";

#[cfg(test)]
mod support {
    use super::*;
    use common::addons::toc;
    use common::archives::zip_text;
    use common::wtf::Wtf;

    /// `omaddons <args> --json --progress json` with `extra` set, checked for privacy.
    pub fn run(home: &Home, args: &[&str], extra: &[(&str, &str)]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let r = home.exec_with(&argv, extra);
        assert_private_all(&r.stdout, &r.stderr);
        r
    }

    /// A retail client whose `WTF/` holds WeakAuras's and Foo's settings (account-wide
    /// and for one character) and `Config.wtf`, with WeakAuras 5.19.2 installed (its two
    /// folders, WeakAuras and WeakAurasOptions), backed up as `id`.
    pub struct Scene {
        pub home: Home,
        pub id: String,
        pub wtf: PathBuf,
    }

    impl Scene {
        pub fn new() -> Self {
            Self::with_home(Home::new(&["wow"]))
        }

        pub fn with_home(home: Home) -> Self {
            let wtf = Wtf::at(home.client("_retail_"))
                .config_wtf("SET locale \"enUS\"\n")
                .saved_variables("WeakAuras", 400)
                .saved_variables("Foo", 200)
                .build();
            let s = Self {
                home,
                id: String::new(),
                wtf,
            };
            s.install_weakauras("5.19.2", &["WeakAuras", "WeakAurasOptions"]);
            let created = run(&s.home, &["backup", "create", "-t", "retail"], &[]).expect(0);
            let id = created["data"]["targets"][0]["backup"]["id"]
                .as_str()
                .unwrap()
                .to_owned();
            Self { id, ..s }
        }

        /// Installs (or updates to) WeakAuras `version`, shipping `folders`.
        pub fn install_weakauras(&self, version: &str, folders: &[&str]) {
            self.install("gh:o/WeakAuras", version, folders);
        }

        /// Installs `r` at `version`, shipping `folders`.
        pub fn install(&self, r: &str, version: &str, folders: &[&str]) {
            let dl = self.home.env.root().join("dl");
            fs::create_dir_all(&dl).unwrap();
            let zip = dl.join(format!("{}-{version}.zip", r.replace([':', '/'], "-")));
            let line = format!("## Version: {version}");
            let tocs: Vec<(String, String)> = folders
                .iter()
                .map(|f| (format!("{f}/{f}.toc"), toc("120105", &[&line])))
                .collect();
            let entries: Vec<(&str, &str)> =
                tocs.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
            fs::write(&zip, zip_text(&entries)).unwrap();
            let zip = zip.display().to_string();
            run(
                &self.home,
                &["install", r, "--file", &zip, "-t", "retail"],
                &[],
            )
            .expect(0);
        }

        /// A play session since the backup: both WeakAuras files rewritten, a
        /// WeakAurasOptions file created, and Foo's file and `Config.wtf` changed too.
        pub fn play(&self) {
            fs::write(self.wtf.join(WA_ACCOUNT), "WeakAurasDB = { played = 1 }\n").unwrap();
            fs::write(self.wtf.join(WA_CHAR), "WeakAurasDB = { played = 2 }\n").unwrap();
            fs::write(self.wtf.join(OPTIONS), "WeakAurasOptionsDB = {}\n").unwrap();
            fs::write(
                self.wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua"),
                "FooDB = { played = true }\n",
            )
            .unwrap();
            fs::write(self.wtf.join("Config.wtf"), "SET locale \"deDE\"\n").unwrap();
        }

        /// `backup restore <id> -t retail --addon …` with `more` arguments.
        pub fn restore(&self, addons: &[&str], more: &[&str], extra: &[(&str, &str)]) -> Run {
            let mut args = self.restore_args(addons);
            args.extend(more.iter().map(|a| (*a).to_owned()));
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            run(&self.home, &args, extra)
        }

        /// `backup restore <id> -t retail --addon …`'s arguments.
        pub fn restore_args(&self, addons: &[&str]) -> Vec<String> {
            let mut args: Vec<String> = ["backup", "restore", self.id.as_str(), "-t", "retail"]
                .map(str::to_owned)
                .to_vec();
            for addon in addons {
                args.extend(["--addon".to_owned(), (*addon).to_owned()]);
            }
            args
        }

        /// The history lines, oldest first.
        pub fn history(&self) -> Vec<Value> {
            fs::read_to_string(self.home.state().join("history.jsonl"))
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }

        /// What a restore's scratch places hold: `<meta>/stage` and `<state>/inflight`.
        pub fn leftovers(&self) -> Vec<PathBuf> {
            [
                self.home
                    .client("_retail_")
                    .join("Interface/.omaddons/stage"),
                self.home.state().join("inflight"),
            ]
            .iter()
            .flat_map(|dir| {
                fs::read_dir(dir)
                    .map(|l| l.map(|e| e.unwrap().path()).collect::<Vec<_>>())
                    .unwrap_or_default()
            })
            .collect()
        }
    }

    /// Every regular file under `wtf` whose name is one of `names` (any case) or, with
    /// `names` empty, every file, with its bytes.
    pub fn files_of(wtf: &Path, names: &[&str]) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        let mut pending = vec![PathBuf::new()];
        while let Some(rel) = pending.pop() {
            let Ok(listing) = fs::read_dir(wtf.join(&rel)) else {
                continue;
            };
            for entry in listing {
                let entry = entry.unwrap();
                let child = rel.join(entry.file_name());
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    pending.push(child);
                } else if kind.is_file() {
                    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                    if names.is_empty() || names.iter().any(|n| n.to_ascii_lowercase() == name) {
                        let bytes = fs::read(wtf.join(&child)).unwrap();
                        out.insert(child.to_string_lossy().into_owned(), bytes);
                    }
                }
            }
        }
        out
    }

    /// Every file under `wtf` but WeakAuras's and WeakAurasOptions's SavedVariables:
    /// what an `--addon WeakAuras` restore must leave byte for byte.
    pub fn rest_hash(wtf: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut all = files_of(wtf, &[]);
        all.retain(|path, _| {
            let name = path.rsplit('/').next().unwrap().to_ascii_lowercase();
            !(name.starts_with("weakauras") && path.contains("/SavedVariables/"))
        });
        all
    }

    /// `omaddons <args>` with `dir` bind-mounted over itself in a user and mount
    /// namespace, so it is a mount point of its own; `None` where unprivileged bind mounts
    /// are refused.
    pub fn bound(home: &Home, dir: &Path, args: &[String]) -> Option<Run> {
        let try_bind = std::process::Command::new("unshare")
            .args(["-r", "-m", "mount", "--bind"])
            .arg(dir)
            .arg(dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !try_bind {
            return None;
        }
        let mut argv = args.to_vec();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let inner = home.std_command(&argv, &[]);
        let mut cmd = std::process::Command::new("unshare");
        cmd.args([
            "-r",
            "-m",
            "sh",
            "-c",
            "mount --bind \"$0\" \"$0\" && exec \"$@\"",
        ])
        .arg(dir)
        .arg(inner.get_program())
        .args(inner.get_args());
        for (key, value) in inner.get_envs() {
            match value {
                Some(value) => cmd.env(key, value),
                None => cmd.env_remove(key),
            };
        }
        let output = cmd.output().unwrap();
        let r = Run {
            argv,
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        };
        assert_private_all(&r.stdout, &r.stderr);
        Some(r)
    }
}

#[test]
fn an_addon_restore_puts_its_files_back_and_undo_returns_each_state() {
    let s = Scene::new();
    let backed_up = files_of(&s.wtf, &["WeakAuras.lua", "WeakAurasOptions.lua"]);
    s.play();
    s.install_weakauras("5.20.0", &["WeakAuras", "WeakAurasOptions"]);
    let played = files_of(&s.wtf, &["WeakAuras.lua", "WeakAurasOptions.lua"]);
    let rest = rest_hash(&s.wtf);

    let dry = s.restore(&["WeakAuras"], &["--dry-run"], &[]).expect(0);
    let data = &dry["data"];
    assert_eq!(
        (
            &data["mode"],
            &data["files"],
            &data["before_restore"],
            &data["exchange"]
        ),
        (
            &json!("addons"),
            &Value::Null,
            &Value::Null,
            &json!("unverified")
        ),
        "{dry:#}"
    );
    assert_eq!(
        data["addons"],
        json!([{
            "addon": "WeakAuras",
            "folders": ["WeakAuras", "WeakAurasOptions"],
            "folders_from": "backup",
            "captured_version": "5.19.2",
            "installed_version": "5.20.0",
            "not_in_backup": [],
            "files": {"replaced": 2, "restored": 0, "created_since": 1},
        }])
    );
    assert_eq!(data["reload_required"], "none");
    assert!(data["precondition"].as_str().unwrap().starts_with("pc1:"));
    assert_eq!(
        files_of(&s.wtf, &["WeakAuras.lua", "WeakAurasOptions.lua"]),
        played
    );
    let human = s.home.exec(
        &[
            "backup",
            "restore",
            s.id.as_str(),
            "-t",
            "retail",
            "--addon",
            "WeakAuras",
            "--dry-run",
        ]
        .map(str::to_owned),
    );
    assert_private_all(&human.stdout, &human.stderr);
    for line in [
        "retail: would restore WeakAuras's SavedVariables from backup ",
        "  settings written by WeakAuras 5.19.2; installed now: 5.20.0\n",
        "  3 files: 2 replaced, 0 restored (absent now), 1 created since (moved aside)\n",
    ] {
        assert!(human.stdout.contains(line), "{line:?} in {}", human.stdout);
    }

    let done = s.restore(&["WeakAuras"], &[], &[]).expect(0);
    let tx = done["data"]["tx"].as_str().unwrap().to_owned();
    assert_eq!(
        files_of(&s.wtf, &["WeakAuras.lua", "WeakAurasOptions.lua"]),
        backed_up,
        "the backup's bytes, and the file created since moved aside"
    );
    assert_eq!(rest_hash(&s.wtf), rest, "nothing else changed");
    assert_eq!(done["commands"][0]["argv"], json!(["omaddons", "undo", tx]));
    assert!(s.leftovers().is_empty(), "{:?}", s.leftovers());
    let line = s.history().pop().unwrap();
    assert_eq!(
        (&line["kind"], &line["tx"], &line["ops"]),
        (
            &json!("restore"),
            &json!(tx),
            &json!([
                {"op": "saved_variables", "folder": "WeakAuras", "addon": "WeakAuras",
                    "backup": s.id, "from": "backup", "files": 2},
                {"op": "saved_variables", "folder": "WeakAurasOptions", "addon": "WeakAuras",
                    "backup": s.id, "from": "backup", "files": 1},
            ])
        ),
        "{line:#}"
    );

    // Undo: every touched file back as it was, the moved-aside one too; and undoable.
    let undone = run(&s.home, &["undo", &tx], &[]).expect(0);
    assert_eq!(
        files_of(&s.wtf, &["WeakAuras.lua", "WeakAurasOptions.lua"]),
        played
    );
    assert_eq!(rest_hash(&s.wtf), rest);
    assert_eq!(
        undone["data"]["saved_variables"]["restored"], 3,
        "{undone:#}"
    );
    let undo_tx = undone["data"]["tx"].as_str().unwrap().to_owned();
    // Settings only: the addon stays as installed, its folders unchanged on disk.
    let status = run(&s.home, &["status", "-t", "retail"], &[]).expect(0);
    let retail = &status["data"]["targets"][0];
    assert_eq!(
        (&retail["drift"], &retail["addons"]["total"]),
        (&json!([]), &json!(1)),
        "{status:#}"
    );
    let listed = run(&s.home, &["list", "-t", "retail"], &[]).expect(0);
    assert!(listed.to_string().contains("\"5.20.0\""), "{listed:#}");
    let line = s.history().pop().unwrap();
    assert_eq!(
        line["ops"],
        json!([
            {"op": "saved_variables", "folder": "WeakAuras", "addon": "WeakAuras",
                "from": "snapshot"},
            {"op": "saved_variables", "folder": "WeakAurasOptions", "addon": "WeakAuras",
                "from": "snapshot"},
        ]),
        "{line:#}"
    );
    run(&s.home, &["undo", &undo_tx], &[]).expect(0);
    assert_eq!(
        files_of(&s.wtf, &["WeakAuras.lua", "WeakAurasOptions.lua"]),
        backed_up
    );
    assert_eq!(rest_hash(&s.wtf), rest);
}

/// A partial restore and its undo keep both snapshot halves as `<tx>.wtf`, which a pre-M3
/// build's orphan sweep never takes (omad-aepa). One committed before that, its halves
/// under the bare id, is undone all the same.
#[test]
fn partial_restore_snapshots_are_named_wtf_and_found_under_either_name() {
    let s = Scene::new();
    s.play();
    let played = files_of(&s.wtf, &["WeakAuras.lua", "WeakAurasOptions.lua"]);
    let client = s
        .home
        .client("_retail_")
        .join("Interface/.omaddons/snapshots");
    let state = s.home.state().join("snapshots");
    let halves = |tx: &str| {
        let wtf = format!("{tx}.wtf");
        (
            client.join(&wtf).is_dir() && state.join(&wtf).is_dir(),
            client.join(tx).exists() || state.join(tx).exists(),
        )
    };
    let done = s.restore(&["WeakAuras"], &[], &[]).expect(0);
    let tx = done["data"]["tx"].as_str().unwrap().to_owned();
    assert_eq!(halves(&tx), (true, false), "the restore's");
    // As a build before omad-aepa named them.
    for dir in [&client, &state] {
        fs::rename(dir.join(format!("{tx}.wtf")), dir.join(&tx)).unwrap();
    }
    let undone = run(&s.home, &["undo", &tx], &[]).expect(0);
    assert_eq!(
        files_of(&s.wtf, &["WeakAuras.lua", "WeakAurasOptions.lua"]),
        played,
        "{undone:#}"
    );
    let undo_tx = undone["data"]["tx"].as_str().unwrap();
    assert_eq!(halves(undo_tx), (true, false), "the undo's");
}

/// The folders come from the backup's addon record: after the addon was removed, after
/// it renamed a folder (the new one is reported and left alone), and by one of its
/// folders; a bare folder of an addon never managed is its own.
#[test]
fn the_folders_are_those_the_addon_owned_when_the_backup_was_taken() {
    let addons = |envelope: &Value| envelope["data"]["addons"].clone();

    // Removed since: still found, nothing installed now.
    let s = Scene::new();
    s.play();
    run(&s.home, &["remove", "WeakAuras", "-t", "retail"], &[]).expect(0);
    let dry = s.restore(&["WeakAuras"], &["--dry-run"], &[]).expect(0);
    let wa = &addons(&dry)[0];
    assert_eq!(
        (
            &wa["folders"],
            &wa["folders_from"],
            &wa["installed_version"],
            &wa["files"]
        ),
        (
            &json!(["WeakAuras", "WeakAurasOptions"]),
            &json!("backup"),
            &Value::Null,
            &json!({"replaced": 2, "restored": 0, "created_since": 1})
        ),
        "{dry:#}"
    );

    // A folder renamed by an update: the capture-time one comes back, the new one is
    // reported and left as it is.
    let s = Scene::new();
    s.install_weakauras("5.20.0", &["WeakAuras", "WeakAurasConfig"]);
    let config = "Account/FAKEACCT/SavedVariables/WeakAurasConfig.lua";
    fs::write(s.wtf.join(config), "WeakAurasConfigDB = {}\n").unwrap();
    fs::write(s.wtf.join(WA_ACCOUNT), "WeakAurasDB = { played = 1 }\n").unwrap();
    let done = s.restore(&["WeakAuras"], &[], &[]).expect(0);
    let wa = &addons(&done)[0];
    assert_eq!(
        (&wa["folders"], &wa["not_in_backup"]),
        (
            &json!(["WeakAuras", "WeakAurasOptions"]),
            &json!(["WeakAurasConfig"])
        ),
        "{done:#}"
    );
    assert_eq!(
        fs::read_to_string(s.wtf.join(config)).unwrap(),
        "WeakAurasConfigDB = {}\n"
    );

    // One of its folders names the addon, as `remove` reads it; `folder:` names that
    // folder alone; a folder of no addon is its own.
    let s = Scene::new();
    let by_folder = s
        .restore(&["WeakAurasOptions"], &["--dry-run"], &[])
        .expect(0);
    assert_eq!(addons(&by_folder)[0]["addon"], "WeakAuras");
    // Neither the backup nor the client holds a file of it: nothing to restore.
    let alone = s
        .restore(&["folder:WeakAurasOptions"], &["--dry-run"], &[])
        .expect(3);
    assert_eq!(alone["error"]["code"], "not_found", "{alone:#}");
    let foo = s.restore(&["Foo"], &["--dry-run"], &[]).expect(0);
    assert_eq!(
        (
            &addons(&foo)[0]["folders"],
            &addons(&foo)[0]["folders_from"]
        ),
        (&json!(["Foo"]), &json!("folder"))
    );
}

/// A capture-time folder another addon owns now is refused rather than broadened, with
/// each folder alone as an alternative; an unknown name offers the backup's folders. Each
/// alternative runs.
#[test]
fn an_ambiguous_or_unknown_addon_offers_alternatives_that_run() {
    let s = Scene::new();
    // WeakAurasOptions moved to another addon.
    s.install_weakauras("5.20.0", &["WeakAuras"]);
    s.install("gh:o/Opts", "1.0", &["WeakAurasOptions"]);
    fs::write(s.wtf.join(OPTIONS), "WeakAurasOptionsDB = {}\n").unwrap();
    let before = files_of(&s.wtf, &[]);
    let refused = s.restore(&["WeakAuras"], &["--dry-run"], &[]).expect(3);
    let error = &refused["error"];
    assert_eq!(error["code"], "ref_ambiguous", "{refused:#}");
    let alternatives: Vec<Vec<String>> = error["alternatives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| serde_json::from_value(a["argv"].clone()).unwrap())
        .collect();
    let id = s.id.as_str();
    assert_eq!(
        alternatives,
        [
            [
                "omaddons",
                "backup",
                "restore",
                id,
                "-t",
                "retail",
                "--addon",
                "folder:WeakAuras",
                "--dry-run"
            ],
            [
                "omaddons",
                "backup",
                "restore",
                id,
                "-t",
                "retail",
                "--addon",
                "folder:WeakAurasOptions",
                "--dry-run"
            ],
        ]
        .map(|a| a.map(str::to_owned).to_vec())
    );
    for argv in &alternatives {
        let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        let r = run(&s.home, &args, &[]).expect(0);
        assert_eq!(r["data"]["addons"][0]["folders_from"], "folder", "{r:#}");
    }
    assert_eq!(files_of(&s.wtf, &[]), before, "previews change nothing");

    let unknown = s.restore(&["Nothing"], &["--dry-run"], &[]).expect(3);
    let error = &unknown["error"];
    assert_eq!(error["code"], "not_found", "{unknown:#}");
    let alternatives = error["alternatives"].as_array().unwrap();
    assert!((1..=3).contains(&alternatives.len()), "{alternatives:?}");
    for alternative in alternatives {
        let argv: Vec<String> = serde_json::from_value(alternative["argv"].clone()).unwrap();
        let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        let r = run(&s.home, &args, &[]);
        assert_ne!(r.envelope()["error"]["code"], "usage", "{}", r.context());
    }
}

/// The widget's template (`backup restore -t <t> --addon=<a> -- <id>`) parses whatever
/// fills the slot: a hostile value is a name like any other.
#[test]
fn the_widget_template_parses_any_addon_value() {
    let s = Scene::new();
    for value in [
        "WeakAuras",
        "--version",
        "-t",
        "; rm -rf ~",
        "../../FakeChar",
        "folder:",
        "",
    ] {
        let slot = format!("--addon={value}");
        // The presentation flags before `--`, after which everything is the id.
        let args = [
            "backup",
            "restore",
            "-t",
            "retail",
            "--dry-run",
            "--json",
            "--progress",
            "json",
            &slot,
            "--",
            &s.id,
        ]
        .map(str::to_owned);
        let r = s.home.exec(&args);
        assert_private_all(&r.stdout, &r.stderr);
        let envelope = r.envelope();
        assert_ne!(
            envelope["error"]["code"],
            "usage",
            "{value:?}: {}",
            r.context()
        );
        if value == "WeakAuras" {
            assert_eq!(envelope["ok"], true, "{}", r.context());
        }
    }
}

/// While the client runs, or may: refused before anything is staged, whatever the flag.
#[test]
fn a_running_client_is_refused_before_anything_is_staged() {
    let mut s = Scene::new();
    s.play();
    let before = files_of(&s.wtf, &[]);
    s.home.retail_running();
    for more in [&[][..], &["--while-running"], &["--dry-run"]] {
        let refused = s.restore(&["WeakAuras"], more, &[]).expect(4);
        assert_eq!(
            (
                &refused["error"]["code"],
                &refused["error"]["details"]["reason"]
            ),
            (&json!("game_running"), &json!("saved_variables")),
            "{more:?}: {refused:#}"
        );
    }
    assert_eq!(files_of(&s.wtf, &[]), before);
    assert!(s.leftovers().is_empty(), "{:?}", s.leftovers());
}

/// An account folder symlinked outside the client (a sync folder) is never written:
/// counted, never named.
#[test]
fn a_symlinked_account_is_left_and_counted() {
    let s = Scene::new();
    let outside = s.home.env.root().join("sync-FakeChar");
    fs::create_dir_all(outside.join("SavedVariables")).unwrap();
    fs::write(
        outside.join("SavedVariables/WeakAuras.lua"),
        "WeakAurasDB = {}\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, s.wtf.join("Account/FAKEACCTSYNC")).unwrap();
    s.play();
    let done = s.restore(&["WeakAuras"], &[], &[]).expect(0);
    let warnings: Vec<&Value> = done["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "saved_variables_out_of_scope")
        .collect();
    assert_eq!(warnings.len(), 1, "{done:#}");
    assert!(
        warnings[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("1 SavedVariables files"),
        "{warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(outside.join("SavedVariables/WeakAuras.lua")).unwrap(),
        "WeakAurasDB = {}\n"
    );
}

/// No atomic exchange, a SavedVariables folder on another mount, a damaged backup: each
/// refused before anything live moves, with nothing left behind.
#[test]
fn what_cannot_be_exchanged_or_is_damaged_is_refused_before_anything_moves() {
    let s = Scene::new();
    s.play();
    let before = files_of(&s.wtf, &[]);
    let refused = s
        .restore(
            &["WeakAuras"],
            &[],
            &[("OMADDONS_FAILPOINTS", "fsx.renameat2.einval")],
        )
        .expect(4);
    let error = &refused["error"];
    assert_eq!(
        (&error["code"], &error["details"]["reason"]),
        (
            &json!("filesystem_unsupported"),
            &json!("no_rename_exchange")
        ),
        "{refused:#}"
    );
    assert!(
        error["details"]["backup_dir"]
            .as_str()
            .unwrap()
            .ends_with(&format!("{}/WTF", s.id))
    );
    assert_eq!(files_of(&s.wtf, &[]), before);
    assert!(s.leftovers().is_empty(), "{:?}", s.leftovers());

    // A character's SavedVariables folder a mount of its own.
    let sv = s
        .wtf
        .join("Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables");
    match bound(&s.home, &sv, &s.restore_args(&["WeakAuras"])) {
        Some(r) => {
            let refused = r.expect(4);
            assert_eq!(
                refused["error"]["details"]["reason"], "stage_cross_mount",
                "{refused:#}"
            );
            assert_eq!(files_of(&s.wtf, &[]), before);
            assert!(s.leftovers().is_empty(), "{:?}", s.leftovers());
        }
        None => common::checkpoint::skip(
            "what_cannot_be_exchanged_or_is_damaged_is_refused_before_anything_moves",
            "no unprivileged bind mount here",
        ),
    }

    // A verified backup whose WeakAuras file was changed since, keeping its size.
    let verified = Scene::new();
    let created = run(
        &verified.home,
        &["backup", "create", "-t", "retail", "--verify"],
        &[],
    )
    .expect(0);
    let id = created["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let stored = verified
        .home
        .client("_retail_")
        .join("Interface/.omaddons/backups")
        .join(&id)
        .join("WTF")
        .join(WA_ACCOUNT);
    let mut bytes = fs::read(&stored).unwrap();
    bytes[0] ^= 0x20;
    fs::write(&stored, bytes).unwrap();
    verified.play();
    let before = files_of(&verified.wtf, &[]);
    let refused = run(
        &verified.home,
        &[
            "backup",
            "restore",
            &id,
            "-t",
            "retail",
            "--addon",
            "WeakAuras",
        ],
        &[],
    );
    assert_eq!(
        refused.envelope()["error"]["code"],
        "hash_mismatch",
        "{}",
        refused.context()
    );
    assert_eq!(files_of(&verified.wtf, &[]), before);
    assert!(
        verified.leftovers().is_empty(),
        "{:?}",
        verified.leftovers()
    );
}

/// A file the game rewrote between the plan and the lock refuses the run there
/// (retryable unbound, not bound to a preview); a preview binds: a rewritten file changes
/// what it observed, other addons only its selection.
#[test]
fn a_rewritten_file_or_a_stale_preview_is_refused_with_nothing_moved() {
    use common::pause::Pause;

    let s = Scene::new();
    s.play();
    for bound in [false, true] {
        let preview = s.restore(&["WeakAuras"], &["--dry-run"], &[]).expect(0);
        let token = preview["data"]["precondition"].as_str().unwrap().to_owned();
        let pause = Pause::new(s.home.env.root());
        let mut args = s.restore_args(&["WeakAuras"]);
        if bound {
            args.extend(["--if-match".to_owned(), token]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let child = s.home.spawn(
            &args,
            &[
                ("OMADDONS_FAILPOINTS", "engine.restore.staged=pause"),
                pause.env(),
            ],
        );
        pause.reached("engine.restore.staged");
        let rewritten = format!("WeakAurasDB = {{ again = {bound} }}\n");
        fs::write(s.wtf.join(WA_ACCOUNT), &rewritten).unwrap();
        let before = files_of(&s.wtf, &[]);
        pause.resume("engine.restore.staged");
        let output = child.wait_with_output().unwrap();
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        let error = &envelope["error"];
        assert_eq!(
            (&error["code"], &error["retryable"]),
            (&json!("plan_stale"), &json!(!bound)),
            "bound {bound}: {envelope:#}"
        );
        if bound {
            assert_eq!(error["details"]["changed"], json!(["observed"]));
        }
        assert_eq!(files_of(&s.wtf, &[]), before, "bound {bound}");
        assert!(s.leftovers().is_empty(), "{:?}", s.leftovers());
    }

    // Bound before anything is copied: a rewritten file is observed, other addons are
    // another selection.
    let s = Scene::new();
    s.play();
    let preview = s.restore(&["WeakAuras"], &["--dry-run"], &[]).expect(0);
    let token = preview["data"]["precondition"].as_str().unwrap().to_owned();
    let other = s.restore(&["Foo"], &["--if-match", &token], &[]).expect(1);
    assert_eq!(
        other["error"]["details"]["changed"],
        json!(["selection"]),
        "{other:#}"
    );
    fs::write(s.wtf.join(WA_CHAR), "WeakAurasDB = { later = true }\n").unwrap();
    let before = files_of(&s.wtf, &[]);
    let stale = s
        .restore(&["WeakAuras"], &["--if-match", &token], &[])
        .expect(1);
    assert_eq!(
        (
            &stale["error"]["code"],
            &stale["error"]["details"]["changed"],
            &stale["error"]["retryable"]
        ),
        (&json!("plan_stale"), &json!(["observed"]), &json!(false)),
        "{stale:#}"
    );
    assert_eq!(files_of(&s.wtf, &[]), before);
    // Previewed again, it runs.
    let fresh = s.restore(&["WeakAuras"], &["--dry-run"], &[]).expect(0);
    let token = fresh["data"]["precondition"].as_str().unwrap().to_owned();
    s.restore(&["WeakAuras"], &["--if-match", &token], &[])
        .expect(0);
}

/// M3-D28 rule 3: an older install's undo would put WeakAuras's settings back over a
/// later `--addon WeakAuras` restore; a partial restore's undo would undo a later write
/// of them. Both refused, each naming the transaction to undo first.
#[test]
fn partial_restores_conflict_with_undo_where_settings_are_written() {
    let s = Scene::new();
    let install = s
        .history()
        .iter()
        .rev()
        .find(|line| line["kind"] == "install")
        .unwrap()["tx"]
        .as_str()
        .unwrap()
        .to_owned();
    s.play();
    let first = s.restore(&["WeakAuras"], &[], &[]).expect(0);
    let first = first["data"]["tx"].as_str().unwrap().to_owned();
    let refused = run(&s.home, &["undo", &install], &[]).expect(4);
    assert_eq!(
        (&refused["error"]["code"], &refused["error"]["fix_argv"]),
        (&json!("undo_conflict"), &json!(["omaddons", "undo", first])),
        "{refused:#}"
    );
    s.play();
    let second = s.restore(&["WeakAuras"], &[], &[]).expect(0);
    let second = second["data"]["tx"].as_str().unwrap().to_owned();
    let refused = run(&s.home, &["undo", &first], &[]).expect(4);
    assert_eq!(
        (&refused["error"]["code"], &refused["error"]["fix_argv"]),
        (
            &json!("undo_conflict"),
            &json!(["omaddons", "undo", second])
        ),
        "{refused:#}"
    );
    // Another addon's restore is no conflict: Foo's settings after WeakAuras's.
    run(&s.home, &["undo", &second], &[]).expect(0);
    s.restore(&["Foo"], &[], &[]).expect(0);
    run(&s.home, &["undo", &first], &[]).expect(0);
    // And keeping SavedVariables leaves a partial restore's undo nothing to do.
    let kept = run(&s.home, &["undo", &first, "--saved-variables", "keep"], &[]).expect(2);
    assert_eq!(kept["error"]["code"], "usage", "{kept:#}");
}
