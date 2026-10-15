//! `omaddons undo` and `omaddons history` end to end: `--file` installs into a synthetic
//! retail client with a `WTF/` tree under fake account, realm and character names, a
//! fake `/proc` for the running client, and every output checked for those names.

mod common;

use std::fs;
use std::path::PathBuf;

use common::addons::{addon_folder, toc};
use common::archives::zip_text;
use common::home::{Home, Run};
use common::trees::tree_hash;
use serde_json::Value;
use support::{Session, argv_of};

#[cfg(test)]
mod support {
    use super::*;

    pub const ACCOUNT: &str = "WTF/Account/FAKEACCT/SavedVariables";
    pub const CHARACTER: &str = "WTF/Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables";
    pub const PRIVATE: &[&str] = &["FAKEACCT", "FakeRealm", "FakeChar"];

    /// A retail client with an unrelated addon (so AddOns exists), SavedVariables for
    /// Foo, and every run's output kept for the privacy check.
    pub struct Session {
        pub home: Home,
        pub outputs: Vec<String>,
    }

    impl Session {
        pub fn new() -> Self {
            let home = Home::new(&["wow"]);
            let addons = home.addons("_retail_");
            addon_folder(&addons, "Keep", &[("Keep.toc", &toc("120105", &[]))], &[]);
            for (dir, text) in [(ACCOUNT, "-- before"), (CHARACTER, "-- char before")] {
                let dir = home.client("_retail_").join(dir);
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join("Foo.lua"), text).unwrap();
            }
            Self {
                home,
                outputs: Vec::new(),
            }
        }

        pub fn addons(&self) -> PathBuf {
            self.home.addons("_retail_")
        }

        pub fn sv(&self, rel: &str) -> Option<String> {
            fs::read_to_string(self.home.client("_retail_").join(rel)).ok()
        }

        /// The new code in game: one file changed, one created.
        pub fn play(&self) {
            let dir = self.home.client("_retail_").join(ACCOUNT);
            fs::write(dir.join("Foo.lua"), "-- after").unwrap();
            fs::write(dir.join("Foo.lua.bak"), "-- created").unwrap();
        }

        /// `omaddons <args> --json`, kept for the privacy check.
        pub fn run(&mut self, args: &[&str]) -> Run {
            let r = self.home.run(args);
            self.outputs.push(format!("{}\n{}", r.stdout, r.stderr));
            r
        }

        /// Installs `<name>.zip` (one folder `<name>`) as gh:o/<name>; returns the tx.
        pub fn install(&mut self, name: &str, version: &str, extra: &[&str]) -> String {
            let dl = self.home.env.root().join("dl");
            fs::create_dir_all(&dl).unwrap();
            let zip = dl.join(format!("{name}-{version}.zip"));
            let text = toc("120105", &[&format!("## Version: {version}")]);
            let toc_path = format!("{name}/{name}.toc");
            let lua_path = format!("{name}/core.lua");
            fs::write(&zip, zip_text(&[(&toc_path, &text), (&lua_path, version)])).unwrap();
            let r = format!("gh:o/{}", name.to_ascii_lowercase());
            let path = zip.display().to_string();
            let mut args = vec!["install", &r, "--file", &path, "-t", "retail"];
            args.extend_from_slice(extra);
            let run = self.run(&args);
            let envelope = run.expect(0);
            envelope["data"]["tx"].as_str().unwrap().to_owned()
        }

        /// No output so far names the account, realm or character.
        pub fn assert_private(&self) {
            for output in &self.outputs {
                for name in PRIVATE {
                    assert!(!output.contains(name), "{name} in output:\n{output}");
                }
            }
        }
    }

    pub fn argv_of(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn undo_reverts_an_install_and_undo_reapplies_it() {
    let mut s = Session::new();
    let before = tree_hash(&s.addons());
    let t1 = s.install("Foo", "1", &[]);
    let installed = tree_hash(&s.addons());

    let r = s.run(&["undo"]);
    let envelope = r.expect(0);
    let data = &envelope["data"];
    assert_eq!(data["reverted"]["tx"], t1.as_str(), "{}", r.context());
    assert_eq!(
        (&data["reverted"]["kind"], &data["reverted"]["target"]["id"]),
        (&Value::from("install"), &Value::from("retail"))
    );
    assert_eq!(data["folders"]["removed"], serde_json::json!(["Foo"]));
    assert_eq!(
        tree_hash(&s.addons()),
        before,
        "byte-identical to before the install"
    );
    let undo_tx = data["tx"].as_str().unwrap().to_owned();
    assert_eq!(
        argv_of(&envelope["commands"][0]["argv"]),
        ["omaddons", "undo", undo_tx.as_str()]
    );

    let history = s.run(&["history"]).expect(0);
    assert_eq!(history["data"]["entries"][0]["kind"], "undo");
    assert_eq!(history["data"]["entries"][0]["reverts"], t1.as_str());

    // Undo is undoable.
    let r = s.run(&["undo"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["data"]["reverted"]["tx"], undo_tx.as_str());
    assert_eq!(
        envelope["data"]["folders"]["restored"],
        serde_json::json!(["Foo"])
    );
    assert_eq!(tree_hash(&s.addons()), installed);
    s.assert_private();
}

#[test]
fn conflicts_prefixes_and_last_by() {
    let mut s = Session::new();
    let t1 = s.install("Foo", "1", &["--actor", "agent"]);
    let t2 = s.install("Foo", "2", &["--actor", "agent"]);
    let r = s.run(&["undo", &t1]);
    let envelope = r.expect(4);
    assert_eq!(
        envelope["error"]["code"],
        "undo_conflict",
        "{}",
        r.context()
    );
    assert_eq!(
        envelope["error"]["details"]["conflicting"][0]["tx"],
        t2.as_str()
    );
    assert_eq!(
        argv_of(&envelope["error"]["fix_argv"]),
        ["omaddons", "undo", t2.as_str()]
    );

    // Disjoint folders: a newer human install stands; --last-by agent reverts t2 only.
    let t3 = s.install("Bar", "7", &["--actor", "human"]);
    let r = s.run(&["undo", "--last-by", "agent"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["reverted"]["tx"],
        t2.as_str(),
        "{}",
        r.context()
    );
    assert!(s.addons().join("Bar").is_dir());
    let core = fs::read_to_string(s.addons().join("Foo/core.lua")).unwrap();
    assert_eq!(core, "1", "back to the first release");

    // Prefixes: 12 characters resolve, any case; short is usage; unknown not found.
    let prefix = t3[..12].to_ascii_lowercase();
    let r = s.run(&["undo", &prefix, "--dry-run"]);
    assert_eq!(
        r.expect(0)["data"]["reverted"]["tx"],
        t3.as_str(),
        "{}",
        r.context()
    );
    let r = s.run(&["undo", &t3[..7]]);
    assert_eq!(r.expect(2)["error"]["code"], "usage");
    let r = s.run(&["undo", "ZZZZZZZZZZ"]);
    let envelope = r.expect(3);
    assert_eq!(envelope["error"]["code"], "not_found");
    assert_eq!(
        argv_of(&envelope["error"]["fix_argv"]),
        ["omaddons", "history"]
    );
    // Transactions within ~256 ms share their first 8 characters.
    if t3.starts_with(&t1[..8]) {
        let r = s.run(&["undo", &t1[..8]]);
        let envelope = r.expect(3);
        assert!(
            envelope["error"]["details"]["candidates"]
                .as_array()
                .unwrap()
                .len()
                > 1
        );
    }
    let r = s.run(&["undo", &t1, "--last-by", "agent"]);
    assert_eq!(r.expect(2)["error"]["code"], "usage");
    let r = s.run(&["undo", "--last-by", "robot"]);
    assert_eq!(r.expect(2)["error"]["code"], "usage");
    s.assert_private();
}

#[test]
fn saved_variables_while_the_client_runs_and_after() {
    let mut s = Session::new();
    let t1 = s.install("Foo", "1", &[]);
    s.play();
    s.home.retail_running();

    // auto: folders revert (--while-running), SavedVariables wait for the client.
    let r = s.run(&["undo", "--while-running"]);
    let envelope = r.expect(0);
    let sv = &envelope["data"]["saved_variables"];
    assert_eq!(
        (&sv["policy"], &sv["deferred"], &sv["restored"]),
        (&Value::from("auto"), &Value::from(2), &Value::from(0)),
        "{}",
        r.context()
    );
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "saved_variables_deferred");
    assert!(warning.is_some(), "{}", r.context());
    assert_eq!(
        argv_of(&warning.unwrap()["fix_argv"]),
        [
            "omaddons",
            "undo",
            t1.as_str(),
            "--saved-variables",
            "restore"
        ]
    );
    assert_eq!(
        s.sv(&format!("{}/Foo.lua", support::ACCOUNT)).as_deref(),
        Some("-- after")
    );

    // restore refuses while the client runs.
    let r = s.run(&["undo", &t1, "--saved-variables", "restore"]);
    assert_eq!(
        r.expect(4)["error"]["code"],
        "game_running",
        "{}",
        r.context()
    );

    // Once it exits, the follow-up restores the SavedVariables only.
    s.home.nothing_running();
    let r = s.run(&["undo", &t1, "--saved-variables", "restore"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["saved_variables"]["restored"],
        2,
        "{}",
        r.context()
    );
    assert_eq!(
        envelope["data"]["folders"],
        serde_json::json!({"restored": [], "removed": []})
    );
    assert_eq!(
        s.sv(&format!("{}/Foo.lua", support::ACCOUNT)).as_deref(),
        Some("-- before")
    );
    assert_eq!(s.sv(&format!("{}/Foo.lua.bak", support::ACCOUNT)), None);
    assert_eq!(
        s.sv(&format!("{}/Foo.lua", support::CHARACTER)).as_deref(),
        Some("-- char before")
    );

    // keep touches nothing.
    let mut k = Session::new();
    k.install("Foo", "1", &[]);
    k.play();
    let r = k.run(&["undo", "--saved-variables", "keep"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["saved_variables"]["kept"],
        2,
        "{}",
        r.context()
    );
    assert_eq!(
        k.sv(&format!("{}/Foo.lua", support::ACCOUNT)).as_deref(),
        Some("-- after")
    );

    // Human output names no account either.
    let human = s.home.exec(&["history".to_owned()]);
    s.outputs
        .push(format!("{}\n{}", human.stdout, human.stderr));
    s.assert_private();
    k.assert_private();
}

#[test]
fn force_over_an_unmanaged_folder_then_undo_restores_it() {
    let mut s = Session::new();
    let foo = s.addons().join("Foo");
    addon_folder(
        &s.addons(),
        "Foo",
        &[("Foo.toc", &toc("120105", &[]))],
        &[("mine.lua", b"mine")],
    );
    let mine = tree_hash(&foo);
    s.install("Foo", "1", &["--force"]);
    assert!(!foo.join("mine.lua").exists());
    s.run(&["undo"]).expect(0);
    assert_eq!(
        tree_hash(&foo),
        mine,
        "the unmanaged folder is back, byte for byte"
    );
    let list = s.run(&["list"]).expect(0);
    assert_eq!(
        list["data"]["targets"][0]["addons"],
        serde_json::json!([]),
        "unmanaged again"
    );
}

#[test]
fn history_filters_orders_and_truncates() {
    let mut s = Session::new();
    let t1 = s.install("Foo", "1", &["--actor", "agent", "--reason", "first"]);
    let t2 = s.install("Bar", "1", &["--actor", "human"]);
    let t3 = s.install("Baz", "1", &["--actor", "agent"]);

    let r = s.run(&["history", "--actor", "agent", "--since", "1d"]);
    let envelope = r.expect(0);
    let txs: Vec<&str> = envelope["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["tx"].as_str().unwrap())
        .collect();
    assert_eq!(
        txs,
        [t3.as_str(), t1.as_str()],
        "agent only, newest first\n{}",
        r.context()
    );
    assert_eq!(envelope["data"]["truncated"], false);
    let first = &envelope["data"]["entries"][1];
    assert_eq!(
        (
            &first["reason"],
            &first["changes"],
            &first["target"]["id"],
            &first["result"]
        ),
        (
            &Value::from("first"),
            &serde_json::json!({"added": 1, "replaced": 0, "removed": 0}),
            &Value::from("retail"),
            &Value::from("committed")
        )
    );

    let envelope = s.run(&["history", "--limit", "2"]).expect(0);
    assert_eq!(envelope["data"]["entries"].as_array().unwrap().len(), 2);
    assert_eq!(envelope["data"]["entries"][1]["tx"], t2.as_str());
    assert_eq!(envelope["data"]["truncated"], true);

    let envelope = s.run(&["history", "-v", "--limit", "1"]).expect(0);
    let entry = &envelope["data"]["entries"][0];
    assert!(entry.get("changes").is_none(), "{entry}");
    assert_eq!(entry["ops"][0]["folder"], "Baz", "{entry}");

    let none = s
        .run(&["history", "--since", "2030-01-01T00:00:00Z"])
        .expect(0);
    assert_eq!(none["data"]["entries"], serde_json::json!([]));

    // Bad input is usage with a fix; an unknown target is not found.
    for (args, code) in [
        (&["history", "--since", "yesterday"][..], "usage"),
        (&["history", "--actor", "robot"][..], "usage"),
        (&["history", "--limit", "0"][..], "usage"),
    ] {
        let r = s.run(args);
        let envelope = r.expect(2);
        assert_eq!(envelope["error"]["code"], code, "{}", r.context());
        assert!(
            envelope["error"]["fix"]
                .as_str()
                .is_some_and(|f| !f.is_empty())
        );
    }
    let r = s.run(&["history", "--since", "yesterday"]);
    assert_eq!(
        argv_of(&r.envelope()["error"]["fix_argv"]),
        ["omaddons", "history", "--since", "1d"]
    );
    let r = s.run(&["history", "-t", "nonesuch"]);
    assert_eq!(r.expect(3)["error"]["code"], "target_not_found");

    // Human: one line per entry.
    let human = s.home.exec(&["history".to_owned()]);
    assert_eq!(human.code, 0, "{}", human.context());
    let lines: Vec<&str> = human.stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{}", human.context());
    assert!(
        lines[0].starts_with(&t3[..10])
            && lines[0].contains("agent")
            && lines[0].contains("install")
    );
    assert!(
        lines[2].contains("+1 ~0 -0") && lines[2].ends_with("\"first\""),
        "{}",
        lines[2]
    );
    s.assert_private();
}
