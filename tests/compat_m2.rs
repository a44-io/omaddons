//! The M2 binary against state M3 wrote (M3-D45, D12): the release build of `af10e34`
//! (the last M2 commit), cached by commit under `target/compat/`, run on homes the current
//! binary prepared. M3 claims no downgrade barrier, so this characterizes, scene by scene,
//! what that binary refuses and what it would do, and prints the table README Limitations
//! quotes: committed whole and partial restores then an M2 `undo` of an older install, an
//! interrupted whole and partial restore then an M2 change (at once and two hours on), and
//! M3 settings lines then M2 `history` and `undo`. What M3 controls is asserted: M2 never
//! deletes an M3 journal unread (it recovers it, or never sees one named `<tx>.wtf`), and
//! M3 recovers the rest. What it does not, the blind SavedVariables undo, is asserted as
//! observed, so a change in it is noticed. Ignored by default: it builds an old commit.
//! The current binary must be a debug build (its failpoints interrupt the restores):
//! `cargo test --test compat_m2 -- --ignored --nocapture`
#![cfg(debug_assertions)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::home::{Home, Run};
use serde_json::Value;
use support::{Row, Scene, m2_binary};

/// The last M2 commit.
const M2: &str = "af10e34";

#[cfg(test)]
mod support {
    use super::*;
    use common::addons::{addon_folder, toc};

    /// The M2 release binary, built once into `target/compat/<commit>` (a detached
    /// worktree) and cached there; `None` when it cannot be built.
    pub fn m2_binary() -> Option<PathBuf> {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = repo.join("target/compat").join(M2);
        let bin = root.join("target/release/omaddons");
        if bin.is_file() {
            return Some(bin);
        }
        if !root.exists() {
            let added = Command::new("git")
                .current_dir(repo)
                .args(["worktree", "add", "--detach"])
                .arg(&root)
                .arg(M2)
                .status()
                .is_ok_and(|s| s.success());
            if !added {
                return None;
            }
        }
        let built = Command::new("cargo")
            .current_dir(&root)
            .args(["build", "--release"])
            .status()
            .is_ok_and(|s| s.success());
        (built && bin.is_file()).then_some(bin)
    }

    /// One scene's record: what the M2 command did, what was left, and what the current
    /// binary's next run did.
    #[derive(Debug, Clone)]
    pub struct Row {
        pub scene: &'static str,
        pub m2: String,
        pub exit: i32,
        pub code: String,
        pub wtf_changed: Vec<String>,
        pub journal: bool,
        pub stage: bool,
        /// The M3 transaction's snapshot halves (state, client) that survive.
        pub snapshots: String,
        pub m3_next: String,
    }

    /// A retail client with Foo's settings (account-wide and for one character) and Foo
    /// installed from a local folder (the older install), its settings then played on,
    /// backed up, and played on again: the backup holds `v1`, the install's snapshot `v0`.
    pub struct Scene {
        pub home: Home,
        pub install_tx: String,
        pub backup: String,
        pub m2: PathBuf,
        /// Foo's account-wide file when Foo was installed: the install's snapshot.
        pub v0: String,
    }

    pub const FOO: &str = "Account/FAKEACCT/SavedVariables/Foo.lua";

    impl Scene {
        pub fn new(m2: &Path) -> Self {
            let home = Home::new(&["wow"]);
            common::wtf::Wtf::at(home.client("_retail_"))
                .config_wtf("SET locale \"enUS\"\n")
                .saved_variables("Foo", 200)
                .build();
            let src = home.env.root().join("src");
            let foo = addon_folder(&src, "Foo", &[("Foo.toc", &toc("120105", &[]))], &[]);
            let installed = home
                .run(&["install", &foo.display().to_string(), "-t", "retail"])
                .expect(0);
            let install_tx = installed["data"]["transactions"][0]["tx"]
                .as_str()
                .or_else(|| installed["data"]["tx"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| last_tx(&home, "install"));
            let mut s = Self {
                home,
                install_tx,
                backup: String::new(),
                m2: m2.to_path_buf(),
                v0: String::new(),
            };
            s.v0 = s.foo();
            s.write_foo("FooDB = { v = 1 }\n");
            let created = s.home.run(&["backup", "create", "-t", "retail"]).expect(0);
            let backup = created["data"]["targets"][0]["backup"]["id"]
                .as_str()
                .unwrap()
                .to_owned();
            s.write_foo("FooDB = { v = 2 }\n");
            Self { backup, ..s }
        }

        pub fn wtf(&self) -> PathBuf {
            self.home.client("_retail_").join("WTF")
        }

        pub fn write_foo(&self, text: &str) {
            fs::write(self.wtf().join(FOO), text).unwrap();
        }

        pub fn foo(&self) -> String {
            fs::read_to_string(self.wtf().join(FOO)).unwrap_or_default()
        }

        /// Every file under `WTF/` with its bytes.
        pub fn files(&self) -> BTreeMap<String, Vec<u8>> {
            let mut out = BTreeMap::new();
            let mut pending = vec![PathBuf::new()];
            while let Some(rel) = pending.pop() {
                let Ok(listing) = fs::read_dir(self.wtf().join(&rel)) else {
                    continue;
                };
                for entry in listing.map(Result::unwrap) {
                    let child = rel.join(entry.file_name());
                    if entry.file_type().unwrap().is_dir() {
                        pending.push(child);
                    } else if let Ok(bytes) = fs::read(self.wtf().join(&child)) {
                        out.insert(child.to_string_lossy().into_owned(), bytes);
                    }
                }
            }
            out
        }

        /// `omaddons <args> --json` by the M2 binary, in this home's environment.
        pub fn m2(&self, args: &[&str]) -> Run {
            let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            argv.push("--json".to_owned());
            let inner = self.home.std_command(&argv, &[]);
            let mut cmd = Command::new(&self.m2);
            cmd.args(inner.get_args());
            for (key, value) in inner.get_envs() {
                match value {
                    Some(value) => cmd.env(key, value),
                    None => cmd.env_remove(key),
                };
            }
            let output = cmd.output().unwrap();
            Run {
                argv,
                code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }
        }

        /// The current binary: `omaddons <args> --json` with `failpoints` armed.
        pub fn m3(&self, args: &[&str], failpoints: &str) -> Run {
            let argv: Vec<String> = args
                .iter()
                .map(|a| (*a).to_owned())
                .chain(["--json".to_owned()])
                .collect();
            let extra: Vec<(&str, &str)> = if failpoints.is_empty() {
                Vec::new()
            } else {
                vec![("OMADDONS_FAILPOINTS", failpoints)]
            };
            self.home.exec_with(&argv, &extra)
        }

        pub fn journals(&self) -> usize {
            fs::read_dir(self.home.state().join("journal")).map_or(0, |l| l.count())
        }

        pub fn stages(&self) -> usize {
            fs::read_dir(
                self.home
                    .client("_retail_")
                    .join("Interface/.omaddons/stage"),
            )
            .map_or(0, |l| l.count())
        }

        /// Which halves of `tx`'s snapshot exist.
        pub fn snapshots(&self, tx: &str) -> String {
            let state = self.home.state().join("snapshots").join(tx).exists();
            let client = self
                .home
                .client("_retail_")
                .join("Interface/.omaddons/snapshots")
                .join(tx)
                .exists();
            match (state, client) {
                (true, true) => "both",
                (true, false) => "state",
                (false, true) => "client",
                (false, false) => "none",
            }
            .to_owned()
        }

        /// The one journal's tx.
        pub fn journal_tx(&self) -> String {
            fs::read_dir(self.home.state().join("journal"))
                .unwrap()
                .map(Result::unwrap)
                .find_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".jsonl"))
                        .map(|n| n.strip_suffix(".wtf").unwrap_or(n).to_owned())
                })
                .unwrap()
        }

        /// Moves the mtime of every stage, in-flight lock and `tx` snapshot dir two hours
        /// back, past either build's orphan age.
        pub fn age_everything_left(&self, tx: &str) {
            let past = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
            let meta = self.home.client("_retail_").join("Interface/.omaddons");
            let state = self.home.state();
            let mut dirs: Vec<PathBuf> = [meta.join("stage"), state.join("inflight")]
                .iter()
                .filter_map(|dir| fs::read_dir(dir).ok())
                .flat_map(|listing| listing.map(|e| e.unwrap().path()))
                .collect();
            dirs.extend([
                meta.join("snapshots").join(tx),
                state.join("snapshots").join(tx),
            ]);
            for dir in dirs.iter().filter(|d| d.exists()) {
                fs::File::open(dir).unwrap().set_modified(past).unwrap();
            }
        }

        /// Whether some file under the stage holds `text`.
        pub fn stage_holds(&self, text: &str) -> bool {
            let mut pending = vec![
                self.home
                    .client("_retail_")
                    .join("Interface/.omaddons/stage"),
            ];
            while let Some(dir) = pending.pop() {
                let Ok(listing) = fs::read_dir(&dir) else {
                    continue;
                };
                for entry in listing.map(Result::unwrap) {
                    if entry.file_type().unwrap().is_dir() {
                        pending.push(entry.path());
                    } else if fs::read(entry.path()).is_ok_and(|b| b == text.as_bytes()) {
                        return true;
                    }
                }
            }
            false
        }

        /// A local addon folder `name`, installable by either binary.
        pub fn local(&self, name: &str) -> String {
            let src = self.home.env.root().join("src");
            addon_folder(
                &src,
                name,
                &[(&format!("{name}.toc"), &toc("120105", &[]))],
                &[],
            )
            .display()
            .to_string()
        }

        /// What changed under `WTF/` between `before` and now, by path.
        pub fn changed(&self, before: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
            let after = self.files();
            let mut paths: Vec<String> = before
                .keys()
                .chain(after.keys())
                .filter(|p| before.get(*p) != after.get(*p))
                .cloned()
                .collect();
            paths.sort();
            paths.dedup();
            paths
        }
    }

    /// The newest history line of `kind`'s tx.
    fn last_tx(home: &Home, kind: &str) -> String {
        fs::read_to_string(home.state().join("history.jsonl"))
            .unwrap()
            .lines()
            .rev()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["kind"] == kind)
            .and_then(|v| v["tx"].as_str().map(str::to_owned))
            .unwrap()
    }

    /// The error code of a run, or `ok`.
    pub fn code(r: &Run) -> String {
        serde_json::from_str::<Value>(&r.stdout)
            .ok()
            .and_then(|v| v["error"]["code"].as_str().map(str::to_owned))
            .unwrap_or_else(|| {
                if r.code == 0 {
                    "ok".to_owned()
                } else {
                    "-".to_owned()
                }
            })
    }

    /// The warning codes of a run.
    pub fn warnings(r: &Run) -> Vec<String> {
        serde_json::from_str::<Value>(&r.stdout)
            .ok()
            .and_then(|v| v["warnings"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|w| w["code"].as_str().map(str::to_owned))
            .collect()
    }
}

#[test]
#[ignore = "builds the M2 commit: cargo test --test compat_m2 -- --ignored --nocapture"]
fn the_m2_binary_against_state_m3_wrote() {
    let Some(m2) = m2_binary() else {
        common::checkpoint::skip(
            "the_m2_binary_against_state_m3_wrote",
            "the M2 commit does not build here",
        );
        return;
    };
    let mut rows: Vec<Row> = Vec::new();

    // 1 and 2: a committed restore, whole or of Foo alone, then M2's undo of the older
    // install with the default `--saved-variables auto`.
    for (scene, more) in [
        (
            "committed whole restore, then M2 undo of the older install",
            &[][..],
        ),
        (
            "committed --addon restore, then M2 undo of the older install",
            &["--addon", "Foo"][..],
        ),
    ] {
        let s = Scene::new(&m2);
        let mut args = vec!["backup", "restore", s.backup.as_str(), "-t", "retail"];
        args.extend_from_slice(more);
        let restored = s.m3(&args, "").expect(0);
        let tx = restored["data"]["tx"].as_str().unwrap().to_owned();
        assert_eq!(s.foo(), "FooDB = { v = 1 }\n", "{scene}: restored");
        let before = s.files();
        let undo = s.m2(&["undo", &s.install_tx]);
        let foo = s.foo();
        let next = s.m3(&["status"], "");
        rows.push(Row {
            scene,
            m2: format!("undo <install> (warnings {:?})", support::warnings(&undo)),
            exit: undo.code,
            code: support::code(&undo),
            wtf_changed: s.changed(&before),
            journal: s.journals() > 0,
            stage: s.stages() > 0,
            snapshots: s.snapshots(&tx),
            m3_next: format!(
                "status: exit {}, warnings {:?}",
                next.code,
                support::warnings(&next)
            ),
        });
        // Observed (M2 cannot be changed): the blind SavedVariables undo. M2 cannot read
        // the restore's history line, so nothing conflicts, and it puts the install-time
        // copy back over the restored settings.
        assert_eq!(
            foo,
            s.v0,
            "{scene}: the install-time copy back over the restored settings\n{}",
            undo.context()
        );
    }

    // 3 and 4: an interrupted restore, whole or of Foo alone, then any M2 change: at
    // once, and with everything the restore left two hours old, which M2's own orphan
    // sweep then looks at too.
    for (scene, more, aged) in [
        (
            "interrupted whole restore, then an M2 install",
            &[][..],
            false,
        ),
        (
            "interrupted --addon restore, then an M2 install",
            &["--addon", "Foo"][..],
            false,
        ),
        (
            "interrupted whole restore, then an M2 install two hours on",
            &[][..],
            true,
        ),
        (
            "interrupted --addon restore, then an M2 install two hours on",
            &["--addon", "Foo"][..],
            true,
        ),
    ] {
        let s = Scene::new(&m2);
        let mut args = vec!["backup", "restore", s.backup.as_str(), "-t", "retail"];
        args.extend_from_slice(more);
        let crashed = s.m3(&args, "engine.restore.exchanged=abort");
        assert_eq!(crashed.code, -1, "{scene}: {}", crashed.context());
        assert_eq!(s.journals(), 1);
        let pre_restore = "FooDB = { v = 2 }\n";
        assert!(
            s.stage_holds(pre_restore),
            "{scene}: the stage holds the pre-restore file"
        );
        let tx = s.journal_tx();
        let before = s.files();
        // M2's reads leave the journal alone.
        for read in [&["status"][..], &["list"][..], &["history"][..]] {
            let r = s.m2(read);
            assert_eq!(s.journals(), 1, "{scene}: M2 {read:?}\n{}", r.context());
        }
        assert!(s.changed(&before).is_empty(), "{scene}: M2's reads");
        if aged {
            s.age_everything_left(&tx);
        }
        let bar = s.local("Bar");
        let install = s.m2(&["install", &bar, "-t", "retail"]);
        let journal = s.journals() > 0;
        let (changed, stage, snapshots) = (s.changed(&before), s.stages() > 0, s.snapshots(&tx));
        let kept = s.stage_holds(pre_restore);
        let baz = s.local("Baz");
        let next = s.m3(&["install", &baz, "-t", "retail"], "");
        let next_warnings = support::warnings(&next);
        // M2 lists only journals and stages named by a bare id, so it never sees the
        // restore's (`<tx>.wtf.jsonl`, `stage/<tx>.wtf`): the journal and the stage
        // holding the pre-restore files survive it, also two hours on, and the next M3
        // change recovers the restore by its phase (before `swapped`: rolled back).
        assert!(journal && kept, "{scene}: {}", install.context());
        assert!(changed.is_empty(), "{scene}: M2 changed {changed:?}");
        assert!(
            next_warnings.iter().any(|w| w == "journal_recovered"),
            "{scene}: {}",
            next.context()
        );
        assert_eq!(s.journals(), 0, "{scene}: {}", next.context());
        assert_eq!(s.foo(), pre_restore, "{scene}: rolled back");
        rows.push(Row {
            scene,
            m2: format!("install Bar (exit {})", install.code),
            exit: install.code,
            code: support::code(&install),
            wtf_changed: changed,
            journal,
            stage,
            snapshots,
            m3_next: format!(
                "install Baz: exit {}, warnings {next_warnings:?}; WTF/ rolled back to before the restore",
                next.code,
            ),
        });
    }

    // 5: M3 settings lines (an `errors dismiss`, planted as its documented shape, and a
    // `widget` line, which no command writes yet), then M2 `history` and `undo`.
    {
        let s = Scene::new(&m2);
        let history = s.home.state().join("history.jsonl");
        let mut text = fs::read_to_string(&history).unwrap();
        for kind in ["dismiss", "widget"] {
            let line = serde_json::json!({
                "schema_version": "1.0",
                "tx": ulid::Ulid::generate().to_string(),
                "kind": kind,
                "at": "2026-10-22T12:00:00Z",
                "target": null,
                "actor": "human",
                "argv": ["omaddons"],
                "ops": [],
                "result": "committed",
                "run": ulid::Ulid::generate().to_string(),
            });
            text.push_str(&line.to_string());
            text.push('\n');
        }
        fs::write(&history, text).unwrap();
        let before = s.files();
        let listed = s.m2(&["history"]);
        let undo = s.m2(&["undo"]);
        let reverted = serde_json::from_str::<Value>(&undo.stdout)
            .ok()
            .and_then(|v| v["data"]["reverted"]["tx"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let foo = s.foo();
        let next = s.m3(&["history"], "");
        // Observed: M2 skips every line it cannot read (the backup's, the dismiss's and
        // the widget's), and a bare `undo` reverts the newest line it can read, the
        // install, with the blind SavedVariables undo.
        assert_eq!(reverted, s.install_tx, "{}", undo.context());
        assert_eq!(foo, s.v0, "{}", undo.context());
        rows.push(Row {
            scene: "M3 dismiss and widget history lines, then M2 history and undo",
            m2: format!(
                "history (exit {}, warnings {:?}); undo, which reverted {}",
                listed.code,
                support::warnings(&listed),
                if reverted == s.install_tx {
                    "the older install"
                } else {
                    "another transaction"
                }
            ),
            exit: undo.code,
            code: support::code(&undo),
            wtf_changed: s.changed(&before),
            journal: s.journals() > 0,
            stage: s.stages() > 0,
            snapshots: "n/a".to_owned(),
            m3_next: format!(
                "history: exit {}, warnings {:?}",
                next.code,
                support::warnings(&next)
            ),
        });
    }

    let mut table = String::from(
        "| scene | M2 command | exit | error code | WTF/ changed | journal | stage | snapshots | M3 next |\n|---|---|---|---|---|---|---|---|---|\n",
    );
    for r in &rows {
        table.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.scene,
            r.m2,
            r.exit,
            r.code,
            if r.wtf_changed.is_empty() {
                "nothing".to_owned()
            } else {
                r.wtf_changed.join(", ")
            },
            r.journal,
            r.stage,
            r.snapshots,
            r.m3_next
        ));
    }
    let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("{table}"));
}

/// Every M3 journal shape, interrupted right after its `prepared` line, then an M2 change
/// on the same client: M2 either reads the journal and recovers it (its own
/// `journal_recovered`), or never sees it, and the next M3 change recovers it. M2 never
/// deletes one unread.
#[test]
#[ignore = "builds the M2 commit: cargo test --test compat_m2 -- --ignored --nocapture"]
fn every_m3_journal_is_recovered_by_m2_or_left_for_m3() {
    let Some(m2) = m2_binary() else {
        common::checkpoint::skip(
            "every_m3_journal_is_recovered_by_m2_or_left_for_m3",
            "the M2 commit does not build here",
        );
        return;
    };
    let mut table = String::from("| shape | M2 | M3 next |\n|---|---|---|\n");
    let mut unread = Vec::new();
    let shapes = [
        "install",
        "undo of an install",
        "whole restore",
        "--addon restore",
        "undo of a whole restore",
        "undo of an --addon restore",
    ];
    for shape in shapes {
        let s = Scene::new(&m2);
        let qux = s.local("Qux");
        let restore = ["backup", "restore", s.backup.as_str(), "-t", "retail"];
        let partial = [&restore[..], &["--addon", "Foo"]].concat();
        let args: Vec<String> = match shape {
            "install" => vec!["install".into(), qux, "-t".into(), "retail".into()],
            "undo of an install" => vec!["undo".into(), s.install_tx.clone()],
            "whole restore" => restore.iter().map(|a| (*a).to_owned()).collect(),
            "--addon restore" => partial.iter().map(|a| (*a).to_owned()).collect(),
            _ => {
                let first = if shape == "undo of a whole restore" {
                    &restore[..]
                } else {
                    &partial[..]
                };
                let done = s.m3(first, "").expect(0);
                vec![
                    "undo".into(),
                    done["data"]["tx"].as_str().unwrap().to_owned(),
                ]
            }
        };
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let crashed = s.m3(&args, "engine.journal.prepared=abort");
        assert_eq!(crashed.code, -1, "{shape}: {}", crashed.context());
        assert_eq!(s.journals(), 1, "{shape}");
        let bar = s.local("Bar");
        let install = s.m2(&["install", &bar, "-t", "retail"]);
        let m2_recovered = support::warnings(&install)
            .iter()
            .any(|w| w == "journal_recovered");
        let left = s.journals();
        let baz = s.local("Baz");
        let next = s.m3(&["install", &baz, "-t", "retail"], "");
        let m3_recovered = support::warnings(&next)
            .iter()
            .any(|w| w == "journal_recovered");
        table.push_str(&format!(
            "| {shape} | install exit {}, {} | {} |\n",
            install.code,
            match (m2_recovered, left) {
                (true, _) => "recovered it",
                (false, 0) => "deleted it unread",
                (false, _) => "never saw it",
            },
            if m3_recovered {
                "recovered it"
            } else {
                "nothing to recover"
            }
        ));
        if !(m2_recovered || (left == 1 && m3_recovered)) || s.journals() != 0 {
            unread.push(shape);
        }
    }
    let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("{table}"));
    assert!(unread.is_empty(), "not recovered by either: {unread:?}");
}
