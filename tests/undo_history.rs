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

        /// Unfinished transactions in the state directory.
        pub fn journals(&self) -> usize {
            fs::read_dir(self.home.state().join("journal")).map_or(0, |dir| dir.count())
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
            &serde_json::json!({"added": 1, "replaced": 0, "removed": 0, "recorded": 0}),
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
        assert_eq!(
            usage["error"]["fix_argv"],
            serde_json::json!(["omaddons", "history", "--by", "agent", "--json"]),
            "{}",
            r.context()
        );
        let unknown = c.home.run(&["history", "--run", "01ZZZZZZZZ"]).expect(3);
        assert_eq!(unknown["error"]["code"], "not_found");
        let human = c.home.exec(&["history".to_owned()]);
        assert!(
            human.stdout.contains(&format!("{}…", &run[..10])),
            "{}",
            human.stdout
        );
    }
}
