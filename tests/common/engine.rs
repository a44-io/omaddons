//! End-to-end engine scenes: a synthetic retail client, `--file` fixture archives (no
//! network), and the invariant oracle's views of disk and state: per-folder tree hashes,
//! the record generation, history kinds, leftover journals and the lock file's inode.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use serde_json::Value;

use super::archives::zip_text;
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

    /// The kinds in history.jsonl, oldest first (torn lines skipped).
    pub fn history(&self) -> Vec<String> {
        fs::read_to_string(self.home.state().join("history.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .map(|v| v["kind"].as_str().unwrap_or_default().to_owned())
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
