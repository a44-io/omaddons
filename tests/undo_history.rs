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
use serde_json::{Value, json};
use support::{Session, argv_of};

#[cfg(test)]
mod support {
    use super::*;

    pub use super::common::privacy::{ACCOUNT, CHARACTER};

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
            let run = self.install_run(name, version, extra);
            let envelope = run.expect(0);
            envelope["data"]["tx"].as_str().unwrap().to_owned()
        }

        /// [`Session::install`]'s run, whatever its outcome.
        pub fn install_run(&mut self, name: &str, version: &str, extra: &[&str]) -> Run {
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
            self.run(&args)
        }

        /// Moves the client's `WTF/` to a sync folder outside it, symlinked back.
        pub fn link_wtf_away(&self) {
            let wtf = self.home.client("_retail_").join("WTF");
            let away = self.home.env.root().join("sync/WTF");
            fs::create_dir_all(away.parent().unwrap()).unwrap();
            fs::rename(&wtf, &away).unwrap();
            std::os::unix::fs::symlink(&away, &wtf).unwrap();
        }

        /// The managed addons `list` shows on retail.
        pub fn listed(&mut self) -> Vec<String> {
            let envelope = self.run(&["list", "-t", "retail"]).expect(0);
            envelope["data"]["targets"][0]["addons"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["name"].as_str().unwrap().to_owned())
                .collect()
        }

        /// Unfinished transactions in the state directory.
        pub fn journals(&self) -> usize {
            fs::read_dir(self.home.state().join("journal")).map_or(0, |dir| dir.count())
        }

        /// No output so far names the account, realm or character.
        pub fn assert_private(&self) {
            for output in &self.outputs {
                super::common::privacy::assert_private(output);
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
    // A typo is corrected in place, the preview kept: never another actor's real undo.
    let before = tree_hash(&s.addons());
    let r = s.run(&["undo", "--last-by", "humna", "--dry-run"]);
    assert_eq!(
        r.expect(2)["error"]["fix_argv"],
        json!(["omaddons", "undo", "--last-by", "human", "--dry-run"]),
        "{}",
        r.context()
    );
    assert_eq!(tree_hash(&s.addons()), before, "nothing changed");
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

/// An undo that crashed after its swap, before its SavedVariables restore, is finished
/// by the next change while the client runs: folders and records complete, the
/// SavedVariables wait for the follow-up restore, which works once the client exits.
#[test]
fn recovery_defers_saved_variables_while_the_client_runs() {
    let mut s = Session::new();
    let t1 = s.install("Foo", "1", &[]);
    s.play();
    let foo_lua = format!("{}/Foo.lua", support::ACCOUNT);

    let crashed = s.home.exec_with(
        &["undo".to_owned(), "--json".to_owned()],
        &[("OMADDONS_FAILPOINTS", "engine.swapped=abort")],
    );
    assert_eq!(crashed.code, -1, "{}", crashed.context());
    assert!(!s.addons().join("Foo").exists(), "swapped before the crash");
    assert_eq!(s.sv(&foo_lua).as_deref(), Some("-- after"));
    assert_eq!(s.journals(), 1);

    // Adding a folder goes through while the client runs; it recovers the undo first.
    s.home.retail_running();
    let r = s.install_run("Bar", "7", &[]);
    let envelope = r.expect(0);
    let codes: Vec<&str> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"journal_recovered"), "{}", r.context());
    let deferred = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "saved_variables_deferred");
    assert!(deferred.is_some(), "{}", r.context());
    assert_eq!(
        argv_of(&deferred.unwrap()["fix_argv"]),
        [
            "omaddons",
            "undo",
            t1.as_str(),
            "--saved-variables",
            "restore"
        ]
    );
    assert_eq!(
        s.sv(&foo_lua).as_deref(),
        Some("-- after"),
        "never written while the client runs"
    );
    assert_eq!(s.journals(), 0);
    let undo = common::state::history(&s.home)
        .into_iter()
        // ubs:ignore — comparing transaction ids, not secrets
        .find(|e| e.reverts.map(|tx| tx.to_string()) == Some(t1.clone()))
        .unwrap();
    assert!(undo.sv_deferred, "{undo:?}");

    // Once the client exits, the follow-up restores them.
    s.home.nothing_running();
    let r = s.run(&["undo", &t1, "--saved-variables", "restore"]);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["saved_variables"]["restored"],
        2,
        "{}",
        r.context()
    );
    assert_eq!(s.sv(&foo_lua).as_deref(), Some("-- before"));
    assert_eq!(s.sv(&format!("{}/Foo.lua.bak", support::ACCOUNT)), None);
    assert!(s.addons().join("Bar").is_dir(), "Bar stays");
    s.assert_private();
}

/// With a process table omaddons cannot read, undo treats the client as running:
/// SavedVariables wait (`auto`) or are refused (`restore`), and it says why.
#[test]
fn an_unreadable_process_table_defers_saved_variables() {
    use std::os::unix::fs::PermissionsExt;

    if common::procfs::running_as_root() {
        return; // chmod 000 hides nothing from root
    }
    let mut s = Session::new();
    let t1 = s.install("Foo", "1", &[]);
    s.play();
    let locked = s.home.env.root().join("proc-locked");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let run = |s: &Session, args: &[&str]| {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.push("--json".to_owned());
        s.home
            .exec_with(&argv, &[("OMADDONS_PROC_ROOT", locked.to_str().unwrap())])
    };
    let codes = |envelope: &Value| -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap().to_owned())
            .collect()
    };

    let r = run(&s, &["undo", "--while-running"]);
    let envelope = r.expect(0);
    let warnings = codes(&envelope);
    for code in ["running_unknown", "saved_variables_deferred"] {
        assert!(
            warnings.contains(&code.to_owned()),
            "{code}: {}",
            r.context()
        );
    }
    assert_eq!(envelope["data"]["saved_variables"]["deferred"], 2);
    assert_eq!(
        s.sv(&format!("{}/Foo.lua", support::ACCOUNT)).as_deref(),
        Some("-- after")
    );

    let r = run(&s, &["undo", &t1, "--saved-variables", "restore"]);
    let envelope = r.expect(4);
    assert_eq!(envelope["error"]["code"], "game_running", "{}", r.context());
    assert_eq!(envelope["error"]["details"]["running"], "unknown");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
}

/// `WTF/` kept in a sync folder and symlinked into the client: SavedVariables there are
/// never snapshotted or written, and undo succeeds whether the link came before the
/// install or after it (a snapshot that still lists those files).
#[test]
fn saved_variables_outside_the_client_are_left_alone() {
    let codes = |envelope: &Value| -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap().to_owned())
            .collect()
    };
    for link_first in [true, false] {
        let mut s = Session::new();
        if link_first {
            s.link_wtf_away();
        }
        let r = s.install_run("Foo", "1", &[]);
        let installed = r.expect(0);
        assert_eq!(
            codes(&installed).contains(&"saved_variables_out_of_scope".to_owned()),
            link_first,
            "{}",
            r.context()
        );
        if !link_first {
            s.link_wtf_away();
        }
        s.play();

        let r = s.run(&["undo"]);
        let envelope = r.expect(0);
        let context = format!("link first: {link_first}\n{}", r.context());
        assert!(
            codes(&envelope).contains(&"saved_variables_out_of_scope".to_owned()),
            "{context}"
        );
        assert_eq!(
            envelope["data"]["saved_variables"]["restored"], 0,
            "{context}"
        );
        assert!(!s.addons().join("Foo").exists(), "{context}");
        assert_eq!(
            s.sv(&format!("{}/Foo.lua", support::ACCOUNT)).as_deref(),
            Some("-- after"),
            "{context}"
        );
        let history = s.run(&["history"]).expect(0);
        assert_eq!(history["data"]["entries"][0]["kind"], "undo", "{context}");
        assert_eq!(s.listed(), Vec::<String>::new(), "{context}");
        assert_eq!(s.journals(), 0, "{context}");
        s.assert_private();
    }
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

/// A line that does not read (here a hand edit) loses its transaction to `history` and
/// `undo`, and both say so; the lines around it still read.
#[test]
fn an_unreadable_history_line_is_warned_about_by_history_and_undo() {
    let mut s = Session::new();
    s.install("Foo", "1", &[]);
    let history = s.home.state().join("history.jsonl");
    let mut text = fs::read_to_string(&history).unwrap();
    text.push_str("not json\n");
    fs::write(&history, text).unwrap();
    let bar = s.install("Bar", "1", &[]);
    let skipped = |envelope: &Value| {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] == "history_lines_skipped")
            .map(|w| w["message"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };

    let listed = s.run(&["history"]).expect(0);
    assert_eq!(listed["data"]["entries"].as_array().unwrap().len(), 2);
    let warned = skipped(&listed);
    assert_eq!(warned.len(), 1, "{listed}");
    assert!(
        warned[0].starts_with("1 line of history.jsonl"),
        "{warned:?}"
    );

    let undone = s.run(&["undo"]).expect(0);
    assert_eq!(undone["data"]["reverted"]["tx"], bar.as_str());
    assert_eq!(skipped(&undone).len(), 1, "{undone}");
    // Most of all when the transaction asked for is not found.
    let missing = s.run(&["undo", "01JB8Q2VZ00000000000000000"]).expect(3);
    assert_eq!(skipped(&missing).len(), 1, "{missing}");
}

/// `wtf/` and `account/` in lowercase are the client's own, as Wine matches them
/// ignoring case: undo restores the SavedVariables there. Beside a second spelling of
/// `WTF`, nothing under either is written, and a warning says why without naming them.
#[test]
fn lowercase_wtf_folders_are_restored_and_two_spellings_left_alone() {
    let mut s = Session::new();
    let client = s.home.client("_retail_");
    fs::rename(client.join("WTF"), client.join("wtf")).unwrap();
    fs::rename(client.join("wtf/Account"), client.join("wtf/account")).unwrap();
    let foo = client.join("wtf/account/FAKEACCT/SavedVariables/Foo.lua");
    s.install("Foo", "1", &[]);
    fs::write(&foo, "-- after").unwrap();
    let undone = s.run(&["undo"]).expect(0);
    assert_eq!(
        (
            &undone["data"]["saved_variables"]["restored"],
            &undone["data"]["saved_variables"]["unchanged"]
        ),
        (&Value::from(1), &Value::from(3)),
        "{undone}"
    );
    assert_eq!(fs::read_to_string(&foo).unwrap(), "-- before");

    fs::create_dir_all(client.join("WTF")).unwrap();
    s.install("Foo", "1", &[]);
    fs::write(&foo, "-- after again").unwrap();
    let undone = s.run(&["undo"]).expect(0);
    assert_eq!(fs::read_to_string(&foo).unwrap(), "-- after again");
    assert!(
        fs::read_dir(client.join("WTF")).unwrap().next().is_none(),
        "nothing written under the other spelling"
    );
    let messages: Vec<&str> = undone["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "saved_variables_out_of_scope")
        .map(|w| w["message"].as_str().unwrap())
        .collect();
    assert_eq!(messages.len(), 1, "{undone}");
    assert!(messages[0].contains("spelled two ways"), "{}", messages[0]);
    s.assert_private();
}

/// Outcomes that never carried the run's warnings (undo, targets) report them now, and
/// so does a failure.
#[test]
fn undo_and_targets_report_what_the_run_found() {
    let mut s = Session::new();
    s.install("Foo", "1", &[]);
    let config = s.home.env.config().join("omaddons/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "bogus_key = 1\n").unwrap();
    // Undoing the install of a local archive would move Foo's only copy away.
    for (args, code, expected) in [
        (
            &["undo", "--dry-run"][..],
            0,
            &["config_unknown_key", "snapshot_only_copy"][..],
        ),
        (
            &["targets", "default", "retail"],
            0,
            &["config_unknown_key"],
        ),
        (&["targets", "rm", "nosuch"], 3, &["config_unknown_key"]),
    ] {
        let envelope = s.run(args).expect(code);
        let codes: Vec<&str> = envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, expected, "{args:?}: {envelope}");
    }
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
            &serde_json::json!({"added": 1, "replaced": 0, "removed": 0, "recorded": 0, "restored": 0}),
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

/// Every command that changes a setting leaves a history line of its kind, with who,
/// why and the argv, and none for a dry run or a change to nothing; undo never reverts
/// one and steps over it.
#[test]
fn settings_changes_are_recorded_and_never_undone() {
    let mut s = Session::new();
    let foo = s.install("Foo", "1", &[]);
    let settings = |s: &mut Session| -> Vec<Value> {
        let envelope = s.run(&["history", "--limit", "100"]).expect(0);
        envelope["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| !matches!(l["kind"].as_str(), Some("install" | "undo")))
            .cloned()
            .collect()
    };

    s.run(&[
        "config",
        "set",
        "default_channel",
        "beta",
        "--actor",
        "agent",
        "--reason",
        "x",
    ])
    .expect(0);
    let lines = settings(&mut s);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let config = &lines[0];
    assert_eq!(
        (
            &config["kind"],
            &config["actor"],
            &config["reason"],
            &config["target"]
        ),
        (&json!("config"), &json!("agent"), &json!("x"), &Value::Null)
    );
    assert_eq!(
        config["argv"],
        json!([
            "omaddons",
            "config",
            "set",
            "default_channel",
            "beta",
            "--actor",
            "agent",
            "--reason",
            "x",
            "--json"
        ])
    );
    let config_tx = config["tx"].as_str().unwrap().to_owned();

    // A dry run, and a change to nothing, record nothing.
    s.run(&["config", "set", "default_channel", "alpha", "--dry-run"])
        .expect(0);
    s.run(&["config", "set", "default_channel", "beta"])
        .expect(0);
    assert_eq!(settings(&mut s).len(), 1);

    // Each settings command, one line of its kind.
    s.run(&["targets", "rename", "retail", "main"]).expect(0);
    s.run(&["targets", "default", "main"]).expect(0);
    s.run(&["targets", "default", "main"]).expect(0);
    s.home.exec_input(
        &["auth", "github", "set", "--json"].map(str::to_owned),
        &[],
        b"ghp_x\n",
    );
    s.run(&["auth", "github", "clear"]).expect(0);
    s.run(&["export", "--write"]).expect(0);
    s.run(&["export", "--write"]).expect(0);
    let kinds: Vec<String> = settings(&mut s)
        .iter()
        .rev()
        .map(|l| l["kind"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        kinds,
        ["config", "target", "target", "auth", "auth", "export"],
        "an unchanged default and an unchanged export record nothing"
    );
    let target_lines = settings(&mut s);
    let renamed = target_lines
        .iter()
        .find(|l| matches!(l["kind"].as_str(), Some("target")))
        .unwrap();
    assert_eq!(renamed["target"]["id"], "main");
    let human = s.home.exec(&["history".to_owned()]);
    assert!(
        human.stdout.contains("config set default_channel beta"),
        "{}",
        human.stdout
    );

    // Undo steps over them, and never reverts one.
    let r = s.run(&["undo", &config_tx]);
    let refused = r.expect(3);
    assert_eq!(
        refused["error"]["message"],
        format!("transaction {config_tx} is a config change, which undo does not revert"),
        "{}",
        r.context()
    );
    // The newest change by the agent is a setting; its install of Foo is what reverts.
    let undone = s.run(&["undo", "--last-by", "agent"]).expect(0);
    assert_eq!(undone["data"]["reverted"]["tx"], foo.as_str());
    assert!(!s.addons().join("Foo").exists());
}

/// Runs: one command invocation's transactions across targets, undone together. The
/// three-target run is committed in-process through the engine (one `TxRequest.run`), as
/// `update` will; everything after it goes through the binary.
mod runs {
    use serde_json::Value;

    use runs_support::{Clients, ids, txs};

    #[cfg(test)]
    mod runs_support {
        use std::collections::BTreeSet;
        use std::fs;
        use std::path::PathBuf;

        use omaddons::discover::DiscoverySource;
        use omaddons::discover::battlenet::{BattlenetHints, discover_root};
        use omaddons::discover::registry::reconcile;
        use omaddons::engine::inspect::ArchiveLimits;
        use omaddons::engine::plan::{PinIntent, RequestedChange};
        use omaddons::engine::retention::RetentionLimits;
        use omaddons::engine::{EngineCtx, TxRequest, TxTarget};
        use omaddons::model::{Actor, AddonReason, Channel, RunId, TxKind};
        use omaddons::output::ProgressSink;
        use omaddons::rules::RulesTable;
        use omaddons::sources::{SourceConfigView, SourceRegistry};
        use omaddons::state::config::RunningPolicy;
        use omaddons::state::registry::load_registry;
        use serde_json::Value;

        use super::super::common::archives::zip_text;
        use super::super::common::home::Home;
        use super::super::common::http::{paths, replay};

        /// Foo, loadable on every client of [`Clients`].
        const FOO: &[(&str, &str)] = &[("Foo/Foo.toc", "## Interface: 11509, 50504, 120105\n")];
        const FOO2: &[(&str, &str)] = &[(
            "Foo/Foo.toc",
            "## Interface: 11509, 50504, 120105\n## Version: 2\n",
        )];

        /// Retail, classic and era clients, with Foo's archives on disk.
        pub struct Clients {
            pub home: Home,
            dl: PathBuf,
        }

        impl Clients {
            pub fn new() -> Self {
                let home = Home::new(&["wow", "wow_classic", "wow_classic_era"]);
                let dl = home.env.root().join("dl");
                fs::create_dir_all(&dl).unwrap();
                fs::write(dl.join("foo.zip"), zip_text(FOO)).unwrap();
                fs::write(dl.join("foo2.zip"), zip_text(FOO2)).unwrap();
                fs::write(
                    dl.join("bar.zip"),
                    zip_text(&[("Bar/Bar.toc", "## Interface: 120105\n")]),
                )
                .unwrap();
                Self { home, dl }
            }

            pub fn zip(&self, name: &str) -> String {
                self.dl.join(format!("{name}.zip")).display().to_string()
            }

            /// Installs Foo on all three clients as one run by `actor`, in-process.
            pub fn run_everywhere(&self, actor: Actor) -> RunId {
                let paths = paths(&self.home.env);
                let table = RulesTable::builtin();
                let (found, _) = discover_root(
                    &self.home.root,
                    &table,
                    &BattlenetHints::default(),
                    "battlenet",
                    &DiscoverySource::Scan,
                );
                let registry = load_registry(&paths).unwrap().value;
                let resolved = reconcile(&registry, &found);
                let run = RunId::new();
                let req = TxRequest {
                    targets: resolved
                        .targets
                        .iter()
                        .map(|d| TxTarget {
                            discovered: d.clone(),
                            changes: vec![RequestedChange::Install {
                                r#ref: "gh:o/foo".parse().unwrap(),
                                channel: Channel::Stable,
                                channel_explicit: false,
                                version: None,
                                file: Some(self.dl.join("foo.zip")),
                                name: None,
                                pin: PinIntent::Keep,
                                reason: AddonReason::Explicit,
                                reinstall: false,
                                blob_sha256: None,
                                asset: None,
                                revision: None,
                                any_flavor: omaddons::engine::plan::AnyFlavorIntent::Keep,
                            }],
                            default_kind: TxKind::Install,
                            dependency_locks: std::collections::BTreeMap::new(),
                        })
                        .collect(),
                    force: false,
                    while_running: false,
                    dry_run: false,
                    reason: None,
                    kind: Some(TxKind::Update),
                    run,
                    no_deps: false,
                };
                let http = replay(&self.home.env, &self.home.http);
                let sources = SourceRegistry::builtin(&SourceConfigView::all_enabled());
                let argv: Vec<String> = vec!["omaddons".into(), "update".into()];
                let nothing_runs = BTreeSet::new();
                let known: Vec<_> = resolved.targets.iter().map(|d| d.target.clone()).collect();
                let ctx = EngineCtx {
                    paths: &paths,
                    http: http.as_ref(),
                    registry: &sources,
                    table: &table,
                    policy: RunningPolicy::default(),
                    limits: ArchiveLimits::default(),
                    retention: RetentionLimits::default(),
                    actor,
                    argv: &argv,
                    command: "update",
                    running: Some(&nothing_runs),
                    known_targets: &known,
                    cache: None,
                    progress: &ProgressSink::none(),
                    catalog: None,
                    default_channel: Channel::Stable,
                    downloads: None,
                    indexes: None,
                    notes: None,
                    binding: None,
                    auto_backup: None,
                };
                let done = omaddons::engine::run(&req, &ctx).unwrap();
                let committed = done.outcomes.iter().filter(|o| o.tx.is_some()).count();
                assert_eq!(committed, 3, "{:?}", done.failures);
                run
            }

            /// Whether `folder` is in each client's AddOns, retail, classic, era.
            pub fn present(&self, folder: &str) -> [bool; 3] {
                ["_retail_", "_classic_", "_classic_era_"]
                    .map(|c| self.home.addons(c).join(folder).is_dir())
            }

            /// History lines (newest first) as JSON.
            pub fn history(&self, extra: &[&str]) -> Vec<Value> {
                let mut args = vec!["history"];
                args.extend_from_slice(extra);
                let envelope = self.home.run(&args).expect(0);
                envelope["data"]["entries"].as_array().unwrap().clone()
            }
        }

        /// Each line's `tx`.
        pub fn txs(lines: &[Value]) -> Vec<String> {
            lines
                .iter()
                .map(|l| l["tx"].as_str().unwrap().to_owned())
                .collect()
        }

        /// Each entry's `key` (`reverted`, `tx`, ...) of `undo`'s `transactions`.
        pub fn ids(envelope: &Value, key: &str) -> Vec<String> {
            envelope["data"]["transactions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t[key].as_str().unwrap_or("-").to_owned())
                .collect()
        }
    }

    #[test]
    fn bare_undo_reverts_the_whole_run_and_again_reverts_the_undo() {
        let c = Clients::new();
        let run = c.run_everywhere(omaddons::model::Actor::Agent).to_string();
        assert_eq!(c.present("Foo"), [true; 3]);
        let lines = c.history(&["--run", &run]);
        assert_eq!(lines.len(), 3);
        // ubs:ignore — a run id is a public ULID identifier, not a secret
        assert!(lines.iter().all(|l| l["run"] == run.as_str()));

        let r = c.home.run(&["undo"]);
        let undo = r.expect(0);
        assert_eq!(c.present("Foo"), [false; 3], "{}", r.context());
        assert_eq!(undo["data"]["reverted_run"], run.as_str());
        assert_eq!(
            ids(&undo, "reverted"),
            txs(&lines),
            "every transaction of the run, newest first"
        );
        assert_eq!(ids(&undo, "status"), ["undone"; 3]);
        assert_eq!(undo["partial"], false);
        let undo_run = undo["data"]["run"].as_str().unwrap().to_owned();
        assert_ne!(undo_run, run);
        // M1's fields describe the first transaction undone.
        assert_eq!(undo["data"]["reverted"]["tx"], lines[0]["tx"]);
        assert_eq!(
            undo["data"]["tx"].as_str(),
            Some(ids(&undo, "tx")[0].as_str())
        );
        let hints: Vec<&Value> = undo["commands"].as_array().unwrap().iter().collect();
        assert_eq!(hints.len(), 4);
        assert_eq!(
            (&hints[0]["action"], &hints[0]["argv"]),
            (
                &Value::from("undo_run"),
                &serde_json::json!(["omaddons", "undo", "--run", undo_run])
            )
        );
        let undo_lines = c.history(&["--run", &undo_run]);
        assert_eq!(undo_lines.len(), 3);
        // ubs:ignore — comparing a transaction kind, not a secret
        assert!(undo_lines.iter().all(|l| l["kind"] == "undo"));

        // The undo is a run too: undoing it brings Foo back everywhere.
        let again = c.home.run(&["undo"]).expect(0);
        assert_eq!(again["data"]["reverted_run"], undo_run.as_str());
        assert_eq!(ids(&again, "status"), ["undone"; 3]);
        assert_eq!(c.present("Foo"), [true; 3]);

        // Human output: one block per transaction, then the run's undo hint.
        let r = c.home.exec(&["undo".to_owned(), "--dry-run".to_owned()]);
        assert_eq!(r.code, 0, "{}", r.context());
        assert_eq!(r.stdout.matches("would revert").count(), 3, "{}", r.stdout);
        // What a dry run counts, it would do: no past tense.
        assert_eq!(
            r.stdout.matches("would restore ").count(),
            6,
            "{}",
            r.stdout
        );
        assert!(!r.stdout.contains("restored"), "{}", r.stdout);
    }

    /// The short id human output shows (16 characters: time and randomness) undoes its
    /// own transaction, even among one run's transactions minted in one millisecond.
    #[test]
    fn a_shown_short_id_undoes_its_own_transaction() {
        let c = Clients::new();
        c.run_everywhere(omaddons::model::Actor::Agent);
        let lines = c.history(&[]);
        let updates: Vec<String> = lines
            .iter()
            .filter(|l| matches!(l["kind"].as_str(), Some("update")))
            .filter_map(|l| l["tx"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(updates.len(), 3, "{lines:?}");
        for tx in &updates {
            let r = c.home.run(&["undo", &tx[..16], "--dry-run"]);
            let undone = r.expect(0);
            assert_eq!(
                undone["data"]["reverted"]["tx"],
                tx.as_str(),
                "{}",
                r.context()
            );
        }
    }

    #[test]
    fn undo_t_reverts_only_that_targets_transaction() {
        let c = Clients::new();
        c.run_everywhere(omaddons::model::Actor::Agent);
        let r = c.home.run(&["undo", "-t", "retail"]);
        let undo = r.expect(0);
        assert_eq!(c.present("Foo"), [false, true, true], "{}", r.context());
        assert_eq!(ids(&undo, "status"), ["undone"]);
        assert_eq!(undo["data"]["reverted_run"], Value::Null);
        assert_eq!(undo["commands"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_run_with_a_later_change_on_one_target_is_undone_partially() {
        let c = Clients::new();
        let run = c.run_everywhere(omaddons::model::Actor::Agent).to_string();
        let foo2 = c.zip("foo2");
        c.home
            .run(&["install", "gh:o/foo", "--file", &foo2, "-t", "classic"])
            .expect(0);

        let r = c.home.run(&["undo", "--run", &run[..16]]);
        let undo = r.expect(0);
        assert_eq!(undo["partial"], true, "{}", r.context());
        assert_eq!(c.present("Foo"), [false, true, false]);
        let failed: Vec<&Value> = undo["data"]["transactions"]
            .as_array()
            .unwrap()
            .iter()
            // ubs:ignore — comparing a status name, not a secret
            .filter(|t| t["status"] == "failed")
            .collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["target"]["id"], "classic");
        assert_eq!(failed[0]["error"]["code"], "undo_conflict");
        assert_eq!(failed[0]["tx"], Value::Null);
        let codes: Vec<&str> = undo["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w["code"].as_str())
            .collect();
        assert!(codes.contains(&"undo_conflict"), "{codes:?}");

        // Nothing left to undo in that run: every transaction fails, so the command does.
        let r = c.home.run(&["undo", "--run", &run]);
        let failed = r.expect(4);
        assert_eq!(failed["error"]["code"], "undo_conflict", "{}", r.context());
    }

    #[test]
    fn last_by_reverts_only_that_actors_latest_run() {
        let c = Clients::new();
        c.run_everywhere(omaddons::model::Actor::Agent);
        let bar = c.zip("bar");
        let install = ["install", "gh:o/bar", "--file", &bar, "-t", "retail"];
        c.home
            .run(&[&install[..], &["--actor", "human"]].concat())
            .expect(0);
        let undo = c.home.run(&["undo", "--last-by", "agent"]).expect(0);
        assert_eq!(ids(&undo, "status"), ["undone"; 3]);
        assert_eq!(c.present("Foo"), [false; 3]);
        assert_eq!(
            c.present("Bar"),
            [true, false, false],
            "the human's install stays"
        );
    }

    #[test]
    fn history_filters_by_run_and_by_actor() {
        let c = Clients::new();
        let run = c.run_everywhere(omaddons::model::Actor::Agent).to_string();
        let bar = c.zip("bar");
        let install = ["install", "gh:o/bar", "--file", &bar, "-t", "retail"];
        c.home
            .run(&[&install[..], &["--actor", "human"]].concat())
            .expect(0);
        let all = c.history(&[]);
        assert_eq!(all.len(), 4);
        assert!(all.iter().all(|l| l["run"].is_string()));
        let by_agent = c.history(&["--by", "agent"]);
        assert_eq!(by_agent.len(), 3);
        // ubs:ignore — comparing an actor name, not a secret
        assert!(by_agent.iter().all(|l| l["actor"] == "agent"));
        // A prefix (8 characters cover about a second of ULID time; 16 are unique here).
        assert_eq!(c.history(&["--run", &run[..16]]).len(), 3);
        assert_eq!(c.history(&["--by", "agent", "--actor", "agent"]).len(), 3);

        let r = c
            .home
            .run(&["history", "--by", "agent", "--actor", "human"]);
        let usage = r.expect(2);
        // Suggestions carry no presentation flags: the consumer adds its own.
        assert_eq!(
            usage["error"]["fix_argv"],
            serde_json::json!(["omaddons", "history", "--by", "agent"]),
            "{}",
            r.context()
        );
        let unknown = c.home.run(&["history", "--run", "01ZZZZZZZZ"]).expect(3);
        assert_eq!(unknown["error"]["code"], "not_found");
        let human = c.home.exec(&["history".to_owned()]);
        // The run names itself on each of its rows, and only there.
        let named = format!("run {}…", &run[..16]);
        assert_eq!(human.stdout.matches(&named).count(), 3, "{}", human.stdout);
        assert_eq!(human.stdout.matches("run ").count(), 3, "{}", human.stdout);
    }
}

/// Undo's conflict analysis fails closed on history it cannot read (M3-D28 rule 5): a
/// newer line this build cannot decode is assumed to have written every SavedVariables
/// file of its target and the folders it names, so undo never puts old copies back over
/// what it did. `history` still lists the rest, with `history_lines_skipped`.
#[cfg(test)]
mod unreadable_history {
    use super::support::ACCOUNT;
    use super::*;

    /// Appends `line` to history.jsonl, as another omaddons would have.
    fn plant(s: &Session, line: &str) {
        let path = s.home.state().join("history.jsonl");
        fs::create_dir_all(s.home.state()).unwrap();
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        text.push_str(line);
        text.push('\n');
        fs::write(&path, text).unwrap();
    }

    /// A newer omaddons' restore on `target`, of `folders`: its op (`merge`) is one this
    /// build does not know, so the line does not read.
    fn restore_line(target: &str, folders: &[&str]) -> String {
        let ops: Vec<Value> = folders
            .iter()
            .map(|f| json!({"op": "merge", "folder": f, "addon": f}))
            .collect();
        json!({
            "schema_version": "1.0",
            "tx": "01M5ZZZZZZZZZZZZZZZZZZZZZZ",
            "kind": "restore",
            "at": "2026-10-21T12:00:00Z",
            "target": target,
            "actor": "human",
            "reason": null,
            "argv": ["omaddons", "backup", "restore"],
            "ops": ops,
            "result": "committed",
        })
        .to_string()
    }

    fn retail_key(s: &Session) -> String {
        super::common::state::key_of(&s.home, "retail").to_string()
    }

    /// Foo installed (its snapshot holds Foo's SavedVariables), then played.
    fn installed() -> (Session, String) {
        let mut s = Session::new();
        let tx = s.install("Foo", "1", &[]);
        s.play();
        (s, tx)
    }

    fn refused(s: &mut Session, args: &[&str]) -> Value {
        let envelope = s.run(args).expect(4);
        let error = &envelope["error"];
        assert_eq!(error["code"], "undo_conflict", "{envelope}");
        assert_eq!(
            (&error["details"]["reason"], &error["details"]["lines"]),
            (&json!("history_unreadable"), &json!(1)),
            "{envelope}"
        );
        assert_eq!(error["fix_argv"], Value::Null, "prose: {envelope}");
        assert_eq!(
            error["fix"],
            "a newer omaddons wrote that history; undo it with that version"
        );
        envelope
    }

    #[test]
    fn a_line_naming_the_folder_refuses_the_undo_outright() {
        let (mut s, tx) = installed();
        let key = retail_key(&s);
        plant(&s, &restore_line(&key, &["Foo"]));
        let before = tree_hash(&s.addons());
        for keep in [false, true] {
            let mut args = vec!["undo", tx.as_str()];
            if keep {
                args.extend(["--saved-variables", "keep"]);
            }
            let envelope = refused(&mut s, &args);
            assert_eq!(
                envelope["error"]["alternatives"],
                json!([]),
                "keeping the SavedVariables does not help: {envelope}"
            );
        }
        assert_eq!(tree_hash(&s.addons()), before);
        assert_eq!(s.sv(&format!("{ACCOUNT}/Foo.lua")).unwrap(), "-- after");
        // `history` lists the rest, and says a line was skipped.
        let history = s.run(&["history"]).expect(0);
        let codes: Vec<&Value> = history["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| &w["code"])
            .collect();
        assert!(
            codes.contains(&&json!("history_lines_skipped")),
            "{history}"
        );
        assert_eq!(history["data"]["entries"][0]["tx"], tx.as_str());
        s.assert_private();
    }

    #[test]
    fn a_line_on_other_folders_leaves_keeping_the_saved_variables() {
        let (mut s, tx) = installed();
        let key = retail_key(&s);
        plant(&s, &restore_line(&key, &["Bar"]));
        let envelope = refused(&mut s, &["undo", &tx]);
        let keep = &envelope["error"]["alternatives"][0]["argv"];
        assert_eq!(
            keep,
            &json!(["omaddons", "undo", tx, "--saved-variables", "keep"]),
            "{envelope}"
        );
        let args: Vec<String> = argv_of(keep)[1..].to_vec();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        s.run(&args).expect(0);
        assert!(!s.addons().join("Foo").exists(), "the install is undone");
        assert_eq!(
            s.sv(&format!("{ACCOUNT}/Foo.lua")).unwrap(),
            "-- after",
            "its SavedVariables are kept"
        );
        s.assert_private();
    }

    #[test]
    fn another_targets_line_or_an_older_one_never_counts() {
        // Another target's.
        let (mut s, tx) = installed();
        plant(&s, &restore_line("01M5YYYYYYYYYYYYYYYYYYYYYY", &["Foo"]));
        s.run(&["undo", &tx]).expect(0);
        assert_eq!(s.sv(&format!("{ACCOUNT}/Foo.lua")).unwrap(), "-- before");

        // One older than the transaction undone.
        let mut s = Session::new();
        plant(&s, &restore_line("01M5YYYYYYYYYYYYYYYYYYYYYY", &[]));
        plant(&s, "not json at all");
        let tx = s.install("Foo", "1", &[]);
        s.play();
        s.run(&["undo", &tx]).expect(0);
        assert_eq!(s.sv(&format!("{ACCOUNT}/Foo.lua")).unwrap(), "-- before");
        s.assert_private();
    }

    #[test]
    fn a_line_that_is_not_json_touches_everything() {
        let (mut s, tx) = installed();
        plant(&s, "{\"schema_version\":\"1.0\" this is no JSON");
        refused(&mut s, &["undo", &tx]);
        // Without SavedVariables to restore it still overlaps every folder.
        refused(&mut s, &["undo", &tx, "--saved-variables", "keep"]);
        s.assert_private();
    }
}
