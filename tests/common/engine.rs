//! End-to-end engine scenes: a synthetic retail client, `--file` fixture archives (no
//! network), and the invariant oracle's views of disk and state: per-folder tree hashes,
//! the record generation, history kinds, leftover journals and snapshot dirs, and the
//! lock file's inode.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use serde_json::Value;

use super::archives::zip_text;
use super::graph::{Folder, Node, release_graph};
use super::home::{Home, Run};
use super::trees::tree_hash;

/// Foo 1: `Foo` and `FooOld`.
pub const V1: &[(&str, &str)] = &[
    ("Foo/Foo.toc", "## Interface: 120105\n## Version: 1\n"),
    ("Foo/core.lua", "-- v1\n"),
    ("FooOld/FooOld.toc", "## Interface: 120105\n"),
];
/// Foo 2: `Foo` changed and grown; `FooOld` dropped.
pub const V2: &[(&str, &str)] = &[
    ("Foo/Foo.toc", "## Interface: 120105\n## Version: 2\n"),
    ("Foo/core.lua", "-- v2\n"),
    ("Foo/lib/extra.lua", "-- new in v2\n"),
];
/// A small unrelated addon, for the command that recovers.
pub const BAR: &[(&str, &str)] = &[("Bar/Bar.toc", "## Interface: 120105\n## Version: 7\n")];
/// Every folder the Foo scenarios touch.
pub const FOLDERS: &[&str] = &["Foo", "FooOld"];

/// A retail client with the fixture archives on disk.
pub struct Scene {
    pub home: Home,
    pub dl: PathBuf,
}

impl Scene {
    pub fn new() -> Self {
        let home = Home::new(&["wow"]);
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        for (name, entries) in [("v1", V1), ("v2", V2), ("bar", BAR)] {
            fs::write(dl.join(format!("{name}.zip")), zip_text(entries)).unwrap();
        }
        Self { home, dl }
    }

    pub fn zip(&self, name: &str) -> String {
        self.dl.join(format!("{name}.zip")).display().to_string()
    }

    /// The argv (without `--json`) of installing a fixture as `r`.
    pub fn install_args(&self, r: &str, zip: &str) -> Vec<String> {
        [
            "install",
            r,
            "--file",
            &self.zip(zip),
            "-t",
            "retail",
            "--offline",
            "--json",
        ]
        .iter()
        .map(|a| (*a).to_owned())
        .collect()
    }

    /// Runs `args` with `OMADDONS_FAILPOINTS=<spec>` (empty: none).
    pub fn run(&self, args: &[String], failpoints: &str) -> Run {
        let extra: Vec<(&str, &str)> = if failpoints.is_empty() {
            Vec::new()
        } else {
            vec![("OMADDONS_FAILPOINTS", failpoints)]
        };
        self.home.exec_with(args, &extra)
    }

    /// Installs a fixture, which must succeed.
    pub fn install(&self, r: &str, zip: &str) -> Value {
        self.run(&self.install_args(r, zip), "").expect(0)
    }

    pub fn addons(&self) -> PathBuf {
        self.home.addons("_retail_")
    }

    /// Each Foo folder's tree hash, `None` when absent.
    pub fn folders(&self) -> BTreeMap<String, Option<String>> {
        FOLDERS
            .iter()
            .map(|f| {
                let dir = self.addons().join(f);
                ((*f).to_owned(), dir.is_dir().then(|| tree_hash(&dir)))
            })
            .collect()
    }

    /// The target record's generation; 0 before the first commit.
    pub fn generation(&self) -> u64 {
        self.record_file()
            .map(|p| {
                let value: Value = serde_json::from_slice(&fs::read(p).unwrap()).unwrap();
                value["generation"].as_u64().unwrap()
            })
            .unwrap_or(0)
    }

    /// The retail target's record file, once it exists.
    fn record_file(&self) -> Option<PathBuf> {
        fs::read_dir(self.home.state().join("targets"))
            .ok()?
            .map(|e| e.unwrap().path())
            .find(|p| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                name.ends_with(".json") && !name.ends_with(".files.json")
            })
    }

    /// The record of `addon` (its key), `null` when it has none.
    pub fn record(&self, addon: &str) -> Value {
        self.record_file()
            .map(|p| {
                let value: Value = serde_json::from_slice(&fs::read(p).unwrap()).unwrap();
                value["addons"][addon].clone()
            })
            .unwrap_or(Value::Null)
    }

    /// The folder keys the files index holds.
    pub fn indexed(&self) -> Vec<String> {
        let Some(record) = self.record_file() else {
            return Vec::new();
        };
        let index = record.with_extension("files.json");
        fs::read(&index)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|v| {
                v["folders"]
                    .as_object()
                    .map(|m| m.keys().cloned().collect())
            })
            .unwrap_or_default()
    }

    /// Each history line's kind and its ops as `(op, folder)`, oldest first (torn lines
    /// skipped).
    pub fn history_ops(&self) -> Vec<(String, Vec<(String, String)>)> {
        fs::read_to_string(self.home.state().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .map(|v| {
                let ops = v["ops"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| {
                        (
                            o["op"].as_str().unwrap_or_default().to_owned(),
                            o["folder"].as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect();
                (v["kind"].as_str().unwrap_or_default().to_owned(), ops)
            })
            .collect()
    }

    /// AddOns entries other than `allowed`.
    pub fn stray(&self, allowed: &[&str]) -> Vec<String> {
        let mut stray: Vec<String> = fs::read_dir(self.addons())
            .map(|l| {
                l.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        stray.retain(|n| !allowed.contains(&n.as_str()));
        stray
    }

    /// The argv (with `--json`) of `omaddons <args> -t retail --offline`.
    pub fn args(&self, args: &[&str]) -> Vec<String> {
        args.iter()
            .copied()
            .chain(["-t", "retail", "--offline", "--json"])
            .map(str::to_owned)
            .collect()
    }

    /// `omaddons <args> --json`, which must succeed.
    pub fn ok(&self, args: &[&str]) -> Value {
        self.home.run(args).expect(0)
    }

    /// Each of `names`' folder tree hash, `None` when absent.
    pub fn folders_of(&self, names: &[&str]) -> BTreeMap<String, Option<String>> {
        names
            .iter()
            .map(|f| {
                let dir = self.addons().join(f);
                ((*f).to_owned(), dir.is_dir().then(|| tree_hash(&dir)))
            })
            .collect()
    }

    /// Every record but the recovery command's (`bar`), without what differs between
    /// two runs of one transaction (`tx_id`, `installed_at`).
    pub fn view(&self) -> BTreeMap<String, Value> {
        let Some(file) = self.record_file() else {
            return BTreeMap::new();
        };
        let value: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        value["addons"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, _)| *key != "bar")
            .map(|(key, record)| {
                let mut record = record.clone();
                if let Some(map) = record.as_object_mut() {
                    map.remove("tx_id");
                    map.remove("installed_at");
                }
                (key.clone(), record)
            })
            .collect()
    }

    /// `<config>/omaddons/catalog.d/50-scene.toml`: one entry per (name, repository
    /// under `o/`, folders).
    pub fn catalog(&self, entries: &[(&str, &str, &[&str])]) {
        let mut text = String::from("schema_version = \"1.0\"\n");
        for (name, repo, folders) in entries {
            let folders: Vec<String> = folders.iter().map(|f| format!("{f:?}")).collect();
            text.push_str(&format!(
                "\n[[addon]]\nname = {name:?}\nref = \"gh:o/{repo}\"\nfolders = [{}]\n",
                folders.join(", ")
            ));
        }
        let dir = self.home.env.config().join("omaddons/catalog.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("50-scene.toml"), text).unwrap();
    }

    /// Release `tag` of `gh:o/<repo>`, shipping `folders`, as the latest.
    pub fn release(&self, repo: &str, tag: &str, folders: &[Folder<'_>]) {
        let base = format!("https://github.com/o/{repo}");
        release_graph(
            &self.home.http,
            &[Node {
                base: &base,
                tag,
                folders,
            }],
        );
    }

    /// The kinds in history.jsonl, oldest first (torn lines skipped).
    pub fn history(&self) -> Vec<String> {
        fs::read_to_string(self.home.state().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .map(|v| v["kind"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// The transaction ids in history.jsonl, oldest first (torn lines skipped).
    pub fn history_txs(&self) -> Vec<String> {
        fs::read_to_string(self.home.state().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .map(|v| v["tx"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// Snapshot dirs, client and state halves, of transactions history does not name:
    /// once recovered, a transaction that never committed leaves none.
    pub fn orphan_snapshots(&self) -> Vec<PathBuf> {
        let committed = self.history_txs();
        [
            self.home
                .client("_retail_")
                .join("Interface/.omaddons/snapshots"),
            self.home.state().join("snapshots"),
        ]
        .iter()
        .flat_map(|dir| {
            fs::read_dir(dir)
                .map(|l| l.map(|e| e.unwrap().path()).collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .filter(|p| {
            !committed
                .iter()
                .any(|tx| p.file_name() == Some(tx.as_ref()))
        })
        .collect()
    }

    /// Transactions in history whose state snapshot is gone. Retention keeps a day of
    /// them, so a test sees none.
    pub fn lost_snapshots(&self) -> Vec<String> {
        let snapshots = self.home.state().join("snapshots");
        self.history_txs()
            .into_iter()
            .filter(|tx| !snapshots.join(tx).is_dir())
            .collect()
    }

    /// Unfinished journals left in `<state>/omaddons/journal`.
    pub fn journals(&self) -> Vec<PathBuf> {
        fs::read_dir(self.home.state().join("journal"))
            .map(|l| l.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default()
    }

    /// The global lock file's inode, if it exists.
    pub fn lock_inode(&self) -> Option<u64> {
        fs::metadata(self.home.state().join("lock"))
            .ok()
            .map(|m| m.ino())
    }

    /// Hashes of `<state>/omaddons` and the client dir, for "a reader wrote nothing".
    pub fn snapshot(&self) -> (String, String) {
        let state = self.home.state();
        (
            if state.exists() {
                tree_hash(&state)
            } else {
                String::new()
            },
            tree_hash(&self.home.client("_retail_")),
        )
    }
}

/// A retail folder requiring `deps`.
pub const fn folder<'a>(name: &'a str, deps: &'a [&'a str]) -> Folder<'a> {
    Folder {
        name,
        interface: "120105",
        deps,
        optional: &[],
    }
}

fn json_argv(args: &[&str]) -> Vec<String> {
    args.iter()
        .copied()
        .chain(["--json"])
        .map(str::to_owned)
        .collect()
}

/// M2's transaction shapes (H2), each prepared on a fresh scene from release fixtures:
/// the baseline is installed, and the argv (with `--json`) of the one transaction is
/// returned. `FOLDERS_*` are the folders each one touches.
pub mod shapes {
    use super::super::addons::addon_folder;
    use super::{Scene, folder, json_argv};

    pub const UPDATE_MANY: &[&str] = &["Alpha", "Beta", "Gamma"];
    pub const INSTALL_WITH_DEP: &[&str] = &["Little", "Big", "Lib"];
    pub const SYNC_MIXED: &[&str] = &["Kilo", "Lima", "Mike"];
    pub const HANDOVER: &[&str] = &["Alpha", "Beta", "Shared"];
    pub const ADOPT: &[&str] = &["Echo", "Fox"];

    /// Alpha, Beta and Gamma installed at v1, with v2 out for each: one `update` of three.
    pub fn update_many(s: &Scene) -> Vec<String> {
        let addons = [("alpha", "Alpha"), ("beta", "Beta"), ("gamma", "Gamma")];
        for (repo, name) in addons {
            s.release(repo, "v1", &[folder(name, &[])]);
        }
        s.ok(&[
            "install",
            "gh:o/alpha",
            "gh:o/beta",
            "gh:o/gamma",
            "-t",
            "retail",
        ]);
        for (repo, name) in addons {
            s.release(repo, "v2", &[folder(name, &[])]);
        }
        json_argv(&["update", "-t", "retail", "--refresh"])
    }

    /// Little requires Big (not installed: a dependency round installs it) and Lib
    /// (installed by name: its record gains the requester).
    pub fn install_with_dep(s: &Scene) -> Vec<String> {
        s.release("little", "v1", &[folder("Little", &["Big", "Lib"])]);
        s.release("big", "v1", &[folder("Big", &[])]);
        s.release("lib", "v1", &[folder("Lib", &[])]);
        s.catalog(&[
            ("Little", "little", &["Little"]),
            ("Big", "big", &["Big"]),
            ("Lib", "lib", &["Lib"]),
        ]);
        s.ok(&["install", "gh:o/lib", "-t", "retail"]);
        json_argv(&["install", "gh:o/little", "-t", "retail"])
    }

    /// One `sync --prune`: Kilo installs, Lima (no longer listed) goes, and pinned Mike,
    /// listed without a version, is unpinned (records only).
    pub fn sync_mixed(s: &Scene) -> Vec<String> {
        for (repo, name) in [("kilo", "Kilo"), ("lima", "Lima"), ("mike", "Mike")] {
            s.release(repo, "v1", &[folder(name, &[])]);
        }
        s.ok(&["install", "gh:o/lima", "gh:o/mike", "-t", "retail"]);
        s.ok(&["pin", "Mike", "-t", "retail"]);
        s.home
            .write_desired("[family.mainline]\naddons = [\"gh:o/kilo\", \"gh:o/mike\"]\n");
        json_argv(&["sync", "--prune", "-t", "retail"])
    }

    /// Beta v1 ships Shared, which Beta v2 drops and Alpha v2 ships: one `update`, in
    /// which Alpha (the claimant) is planned first.
    pub fn handover(s: &Scene) -> Vec<String> {
        s.release("alpha", "v1", &[folder("Alpha", &[])]);
        s.release("beta", "v1", &[folder("Beta", &[]), folder("Shared", &[])]);
        s.ok(&["install", "gh:o/alpha", "gh:o/beta", "-t", "retail"]);
        s.release(
            "alpha",
            "v2",
            &[folder("Alpha", &[]), folder("Shared", &[])],
        );
        s.release("beta", "v2", &[folder("Beta", &[])]);
        json_argv(&["update", "-t", "retail", "--refresh"])
    }

    /// Echo and Fox unpacked by hand, both in the catalog: one records-only
    /// `adopt --apply`.
    pub fn adopt(s: &Scene) -> Vec<String> {
        for name in ["Echo", "Fox"] {
            let toc = format!("{name}.toc");
            let text = "## Interface: 120105\n## Version: 1\n";
            addon_folder(&s.addons(), name, &[(toc.as_str(), text)], &[]);
        }
        s.catalog(&[("Echo", "echo", &["Echo"]), ("Fox", "fox", &["Fox"])]);
        json_argv(&["adopt", "--apply", "-t", "retail", "--no-verify"])
    }
}

/// The codes of an envelope's warnings.
pub fn warning_codes(envelope: &Value) -> Vec<String> {
    envelope["warnings"]
        .as_array()
        .map(|w| {
            w.iter()
                .filter_map(|w| w["code"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// What a reboot does to the journals in `dir` on btrfs, FUSE or device-mapper: every
/// device number their steps recorded changes, while inode numbers stay.
pub fn renumber_journal_devices(dir: &std::path::Path) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let mut text = String::new();
        for line in fs::read_to_string(&path).unwrap().lines() {
            let mut value: Value = serde_json::from_str(line).unwrap();
            if value["type"] == "step" {
                for key in ["from_id", "to_id"] {
                    if let Some(dev) = value.get_mut(key).and_then(|id| id.get_mut("dev")) {
                        *dev = (dev.as_u64().unwrap() + 1000).into();
                    }
                }
                text.push_str(&value.to_string());
            } else {
                text.push_str(line);
            }
            text.push('\n');
        }
        fs::write(&path, text).unwrap();
    }
}
