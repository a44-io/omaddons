//! Persisted formats fail closed (M3-D45), end to end. A change to a client whose state
//! a newer omaddons wrote is refused with `state_too_new` before any file of the client
//! changes. A journal this build cannot read is left as it is: it fails its own target's
//! changes with its `journal_pending` details, other targets go on, and `status` says it
//! needs manual repair.

use crate::common;

use common::state::key_of;
use common::trees::tree_hash;
use serde_json::{Value, json};
use support::{Machine, assert_too_new, plant_journal};

#[cfg(test)]
mod support {
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::common::archives::zip_text;
    use super::common::home::{Home, Run};
    use super::common::http::{Asset, gh_release};

    /// Retail and Mists Classic, `gh:o/foo` v1 published, and local zips Foo and Bar.
    pub struct Machine {
        pub home: Home,
    }

    impl Machine {
        pub fn new() -> Self {
            let home = Home::new(&["wow", "wow_classic"]);
            let dl = home.env.root().join("dl");
            fs::create_dir_all(&dl).unwrap();
            for name in ["Foo", "Bar"] {
                let toc = "## Interface: 120105, 50504\n## Version: 1\n";
                let path = format!("{name}/{name}.toc");
                let zip = zip_text(&[(path.as_str(), toc)]);
                fs::write(dl.join(format!("{name}.zip")), zip).unwrap();
            }
            let m = Self { home };
            m.publish("v1");
            m
        }

        /// Publishes `gh:o/foo` at `tag` as its latest release.
        pub fn publish(&self, tag: &str) {
            let toc = format!("## Interface: 120105, 50504\n## Version: {tag}\n");
            let release: [Asset<'_>; 1] = [(
                "Foo.zip",
                &[("mainline", 120_105), ("mists", 50_504)],
                vec![("Foo/Foo.toc", toc.as_str())],
            )];
            gh_release(&self.home.http, "https://github.com/o/foo", tag, &release);
        }

        pub fn zip(&self, name: &str) -> String {
            let path = self.home.env.root().join(format!("dl/{name}.zip"));
            path.display().to_string()
        }

        pub fn run(&self, args: &[&str]) -> Run {
            self.home.run(args)
        }

        pub fn retail(&self) -> PathBuf {
            self.home.client("_retail_")
        }

        /// Rewrites the `schema_version` of the state file at `path`, as a newer
        /// omaddons would have written it (it still decodes).
        pub fn age(path: &Path, version: &str) {
            let mut doc: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            doc["schema_version"] = json!(version);
            fs::write(path, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
        }

        pub fn state_file(&self, name: &str) -> PathBuf {
            self.home.state().join(name)
        }
    }

    /// `state_too_new`, from a change refused up front.
    pub fn assert_too_new(run: &Run) {
        let envelope = run.expect(4);
        assert_eq!(
            envelope["error"]["code"],
            "state_too_new",
            "{}",
            run.context()
        );
    }

    /// Plants a journal of target `key` whose `prepared` line holds `prepared` (with the
    /// type, the transaction and the key); returns the transaction.
    pub fn plant_journal(home: &Home, key: &str, prepared: &Value) -> String {
        let tx = "01M5ZZZZZZZZZZZZZZZZZZZZZZ";
        let mut line = json!({"type": "prepared", "tx": tx, "target_key": key});
        for (k, v) in prepared.as_object().unwrap() {
            line[k] = v.clone();
        }
        let dir = home.state().join("journal");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{tx}.jsonl")), format!("{line}\n")).unwrap();
        tx.to_owned()
    }
}

#[test]
fn a_newer_record_is_refused_before_any_file_of_the_client_changes() {
    let m = Machine::new();
    m.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let local = m.run(&[
        "install",
        "gh:o/bar",
        "--file",
        &m.zip("Bar"),
        "-t",
        "retail",
    ]);
    let tx = local.expect(0)["data"]["tx"].as_str().unwrap().to_owned();
    m.publish("v2");
    let key = key_of(&m.home, "retail");
    Machine::age(&m.state_file(&format!("targets/{key}.json")), "1.99");
    let before = tree_hash(&m.retail());

    let changes: [&[&str]; 4] = [
        &[
            "install",
            "gh:o/baz",
            "--file",
            &m.zip("Foo"),
            "-t",
            "retail",
            "--force",
        ],
        &["update", "foo", "-t", "retail", "--refresh"],
        &["remove", "Bar", "-t", "retail"],
        &["undo", &tx],
    ];
    for args in changes {
        assert_too_new(&m.run(args));
        assert_eq!(tree_hash(&m.retail()), before, "{args:?} changed nothing");
    }
    // Reading still works, and says why.
    let status = m.run(&["status", "-t", "retail"]).expect(0);
    assert_eq!(
        status["data"]["targets"][0]["attention"],
        json!(["state_version_newer"])
    );
    // Another client is not affected.
    m.run(&["install", "gh:o/foo", "-t", "classic"]).expect(0);
}

#[test]
fn a_newer_files_index_is_refused_too() {
    let m = Machine::new();
    m.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let key = key_of(&m.home, "retail");
    Machine::age(&m.state_file(&format!("targets/{key}.files.json")), "1.99");
    let before = tree_hash(&m.retail());
    assert_too_new(&m.run(&[
        "install",
        "gh:o/bar",
        "--file",
        &m.zip("Bar"),
        "-t",
        "retail",
    ]));
    assert_eq!(tree_hash(&m.retail()), before);
}

#[test]
fn a_newer_registry_is_refused_before_registering_a_client() {
    let m = Machine::new();
    m.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    Machine::age(&m.state_file("registry.json"), "1.99");
    let classic = m.home.client("_classic_");
    let before = tree_hash(&classic);
    assert_too_new(&m.run(&["install", "gh:o/foo", "-t", "classic"]));
    assert_eq!(tree_hash(&classic), before, "no marker, no stage, nothing");
}

#[test]
fn a_journal_this_build_cannot_read_is_left_for_manual_repair() {
    for (prepared, reason, code, exit) in [
        (
            json!({"schema_version": "2.0"}),
            "journal_too_new",
            "state_too_new",
            4,
        ),
        (
            json!({"schema_version": "1.1", "kind": "teleport"}),
            "journal_unreadable",
            "state_corrupt",
            1,
        ),
    ] {
        let m = Machine::new();
        m.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
        let key = key_of(&m.home, "retail").to_string();
        let tx = plant_journal(&m.home, &key, &prepared);
        let journal = m.state_file(&format!("journal/{tx}.jsonl"));

        // Its own target's change fails with its details, and leaves it.
        let run = m.run(&[
            "install",
            "gh:o/bar",
            "--file",
            &m.zip("Bar"),
            "-t",
            "retail",
        ]);
        let envelope = run.expect(exit);
        let error = &envelope["error"];
        assert_eq!(error["code"], code, "{}", run.context());
        assert_eq!(
            (
                &error["details"]["reason"],
                &error["details"]["journal_pending"]
            ),
            (&json!(reason), &json!(tx)),
            "{}",
            run.context()
        );
        // The fix is the doctor's explanation of exactly this state, on retail (M4-D53).
        let id = match reason {
            "journal_too_new" => "fm-concurrency_primitives-journal-too-new",
            _ => "fm-concurrency_primitives-journal-unreadable",
        };
        let explain = json!(["omaddons", "doctor", "explain", id, "-t", "retail"]);
        assert_eq!(error["fix_argv"], explain, "{}", run.context());
        assert!(journal.is_file(), "{reason}: kept");
        // It runs, writes nothing, and lists this journal.
        let state = tree_hash(&m.home.env.state());
        let explained = m.run(&["doctor", "explain", id, "-t", "retail"]).expect(0);
        let instances = explained["instances"].as_array().unwrap();
        assert!(
            instances.iter().any(|i| i["instance"]
                .as_str()
                .is_some_and(|key| key.ends_with(&format!(":tx={tx}")))),
            "{reason}: {explained}"
        );
        assert_eq!(
            tree_hash(&m.home.env.state()),
            state,
            "{reason}: explain writes nothing"
        );

        // Another target's change goes on, and says so.
        let run = m.run(&["install", "gh:o/foo", "-t", "classic"]);
        let envelope = run.expect(0);
        let pending: Vec<&Value> = envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] == "journal_pending")
            .collect();
        assert_eq!(pending.len(), 1, "{}", run.context());
        assert!(
            pending[0]["message"].as_str().unwrap().contains(reason),
            "{}",
            run.context()
        );
        assert_eq!(pending[0]["fix_argv"], explain, "{}", run.context());
        assert!(journal.is_file(), "{reason}: still kept");

        // A reader names the state, and its remediation is its forced set-aside (F2),
        // whose evidence its `doctor explain` shows first.
        let status = m.run(&["status", "-t", "retail"]).expect(0);
        let retail = &status["data"]["targets"][0];
        assert_eq!(retail["attention"], json!(["journal_pending"]), "{status}");
        assert_eq!(retail["issues"][0]["id"], id, "{status}");
        assert_eq!(
            retail["issues"][0]["remediation"]["argv"],
            json!([
                "omaddons", "doctor", "fix", "--only", id, "-t", "retail", "--force", "--yes"
            ]),
            "{status}"
        );
    }
}
