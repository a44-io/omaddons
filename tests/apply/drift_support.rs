//! D7's per-command homes (omad-m4-agent-surface-doctor-c6cm.4.7.2), which the lifecycle
//! scenarios (omad-m4-agent-surface-doctor-c6cm.4.7.3) reuse: one home for each storable
//! command, ready for its preview, with the trees a refusal must leave unchanged and the
//! privacy check its `WTF/` calls for; the runs on it; and the assertions of a stale
//! refusal.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use omaddons::app::precondition::preview_argv;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::common::addons::unpack;
use crate::common::archives::zip_text;
use crate::common::backups::seed_backup;
use crate::common::engine::Scene;
use crate::common::env::TestEnv;
use crate::common::home::Home;
use crate::common::http::{Asset, gh_release};
use crate::common::plans::plan_files;
use crate::common::tlog::{Ran, Runner, TestLog};
use crate::common::wtf::Wtf;

/// `gh:o/foo`.
pub(crate) const FOO: &str = "https://github.com/o/foo";

/// The clock of `backup prune`'s rows, on every run: the seeded backups' ages and the
/// plan's expiry agree (M3's `at_noon`).
pub(crate) const PRUNE_NOW: &str = "2026-10-10T12:00:00Z";

/// Foo's and Bar's flavors: retail's and Mists Classic's interfaces.
const FLAVORS: &[(&str, u32)] = &[("mainline", 120_105), ("mists", 50_504)];

/// Foo `v1` and `v2`: one folder that retail and Mists Classic load.
pub(crate) const FOO_V1: &[(&str, &str)] = &[
    (
        "Foo/Foo.toc",
        "## Interface: 120105, 50504\n## Title: Foo\n## Version: v1\ncore.lua\n",
    ),
    ("Foo/core.lua", "-- v1\n"),
];
pub(crate) const FOO_V2: &[(&str, &str)] = &[
    (
        "Foo/Foo.toc",
        "## Interface: 120105, 50504\n## Title: Foo\n## Version: v2\ncore.lua\n",
    ),
    ("Foo/core.lua", "-- v2\n"),
];

/// Bar: the unrelated addon of the interfering commits, as `bar.zip` in every home.
const BAR: &[(&str, &str)] = &[(
    "Bar/Bar.toc",
    "## Interface: 120105, 50504\n## Title: Bar\n## Version: 7\n",
)];

fn foo(entries: &'static [(&'static str, &'static str)]) -> [Asset<'static>; 1] {
    [("Foo.zip", FLAVORS, entries.to_vec())]
}

/// Where a row's runs happen: a [`Home`], or an engine [`Scene`] (its `--file` archives
/// and SavedVariables helpers).
pub(crate) enum On {
    Home(Home),
    Scene(Scene),
}

impl On {
    pub(crate) fn home(&self) -> &Home {
        match self {
            Self::Home(home) => home,
            Self::Scene(scene) => &scene.home,
        }
    }

    pub(crate) fn scene(&self) -> &Scene {
        match self {
            Self::Scene(scene) => scene,
            Self::Home(_) => panic!("this row's home is no Scene"), // ubs:ignore — a builder bug
        }
    }
}

impl Runner for On {
    fn test_env(&self) -> &TestEnv {
        &self.home().env
    }

    fn command(&self, argv: &[String]) -> Command {
        self.home().std_command(argv, &[])
    }
}

/// What a row's `WTF/` holds, for the privacy check on every run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Privacy {
    /// No `WTF/`: still checked against the fake names, which must appear nowhere.
    NoWtf,
    /// `common::wtf`'s `FAKEACCT`, `FakeRealm` and `FakeChar`.
    FakeNames,
    /// The engine Scene's own `ACCT`, `Realm` and `Char`.
    SceneNames,
}

/// One storable command's home, ready for its preview.
pub(crate) struct Built {
    pub(crate) on: On,
    /// The storable argv, without `--dry-run` and `--json`.
    pub(crate) preview: Vec<String>,
    /// Labelled trees a refusal leaves unchanged.
    pub(crate) roots: Vec<(String, PathBuf)>,
    /// Set on every run (`backup prune`: `OMADDONS_NOW`).
    pub(crate) env: Vec<(String, String)>,
    pub(crate) privacy: Privacy,
}

impl Built {
    pub(crate) fn home(&self) -> &Home {
        self.on.home()
    }

    /// `bar.zip`, under the home's tempdir.
    pub(crate) fn bar(&self) -> String {
        self.home().env.root().join("bar.zip").display().to_string()
    }

    /// `<client>/Interface/AddOns` of the client `folder` (`_retail_`).
    pub(crate) fn addons(&self, folder: &str) -> PathBuf {
        self.home().addons(folder)
    }

    /// Records every root's tree in `log`.
    pub(crate) fn record_trees(&self, log: &TestLog) {
        for (label, root) in &self.roots {
            log.tree(label, root);
        }
    }

    /// Every root is as [`Built::record_trees`] recorded it.
    pub(crate) fn assert_trees_unchanged(&self, log: &TestLog) {
        for (label, root) in &self.roots {
            log.assert_unchanged(label, root);
        }
    }

    /// The plan files, each with its bytes' digest.
    pub(crate) fn plan_files(&self) -> Vec<(String, String)> {
        plan_files(&self.home().env)
            .iter()
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let digest = Sha256::digest(fs::read(path).unwrap());
                (name, digest.iter().map(|b| format!("{b:02x}")).collect())
            })
            .collect()
    }

    /// Lines in `history.jsonl` (0 before the first).
    pub(crate) fn history_lines(&self) -> usize {
        fs::read_to_string(self.home().state().join("history.jsonl"))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    /// The storable argv with `extra` (`--dry-run`, `--if-match T`).
    pub(crate) fn preview_with(&self, extra: &[&str]) -> Vec<String> {
        let mut argv = self.preview.clone();
        argv.extend(extra.iter().map(|word| (*word).to_owned()));
        argv
    }
}

/// `omaddons <args>` on `built` with `--json --progress json` (placed before any `--`,
/// after which every word is a positional) and the row's variables; checks the run's
/// privacy, whatever its exit.
#[track_caller]
pub(crate) fn run<A: AsRef<str>>(log: &TestLog, built: &Built, args: &[A]) -> Ran {
    let mut argv: Vec<String> = args.iter().map(|a| a.as_ref().to_owned()).collect();
    let at = argv
        .iter()
        .position(|word| word == "--")
        .unwrap_or(argv.len());
    argv.splice(at..at, ["--json", "--progress", "json"].map(str::to_owned));
    let mut cmd = log.cmd(&built.on, &argv);
    for (key, value) in &built.env {
        cmd = cmd.env(key, value);
    }
    let ran = cmd.run();
    private(&ran, built.privacy);
    ran
}

/// No output on either stream names the row's account, realm or character.
#[track_caller]
fn private(ran: &Ran, privacy: Privacy) {
    match privacy {
        Privacy::NoWtf | Privacy::FakeNames => {
            ran.assert_private();
        }
        Privacy::SceneNames => {
            for (stream, text) in [("stdout", ran.stdout()), ("stderr", ran.stderr())] {
                for segment in ["/ACCT/", "/Realm/", "/Char/"] {
                    assert!(
                        !text.contains(segment),
                        "{stream} names the Scene's {segment}: {text}"
                    );
                }
            }
        }
    }
}

/// A home of `products`, Foo v1 published as `gh:o/foo`, and `bar.zip`.
fn published(log: &TestLog, products: &[&str]) -> Home {
    let home = Home::new(products);
    log.env(&home);
    gh_release(&home.http, FOO, "v1", &foo(FOO_V1));
    with_bar(&home);
    home
}

fn with_bar(home: &Home) {
    fs::write(home.env.root().join("bar.zip"), zip_text(BAR)).unwrap();
}

fn scene(log: &TestLog) -> Scene {
    let scene = Scene::new();
    log.env(&scene.home);
    with_bar(&scene.home);
    scene
}

/// The Scene's fixture archive `zip` installed as `r` (offline), logged; returns the run.
fn scene_install(log: &TestLog, scene: &Scene, r: &str, zip: &str) -> Ran {
    let mut argv = scene.install_args(r, zip);
    argv.extend(strings(&["--progress", "json"]));
    let ran = log.run(scene, &argv);
    ran.ok().assert_private();
    ran
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// Each client's AddOns (`(label, folder)`), the state's records, and `extra`.
fn roots(
    home: &Home,
    clients: &[(&str, &str)],
    extra: &[(&str, PathBuf)],
) -> Vec<(String, PathBuf)> {
    let mut roots: Vec<(String, PathBuf)> = clients
        .iter()
        .map(|(label, folder)| (format!("{label} AddOns"), home.addons(folder)))
        .collect();
    roots.push(("state targets".to_owned(), home.state().join("targets")));
    roots.extend(
        extra
            .iter()
            .map(|(label, root)| ((*label).to_owned(), root.clone())),
    );
    roots
}

/// A run made while building, which must succeed.
#[track_caller]
fn setup(log: &TestLog, on: &On, args: &[&str]) -> Ran {
    let mut argv = strings(args);
    argv.extend(strings(&["--json", "--progress", "json"]));
    let ran = log.run(on, &argv);
    ran.ok().assert_private();
    ran
}

/// One storable command's home, as D7's table specifies.
pub(crate) fn built(log: &TestLog, command: &str) -> Built {
    log.step("seed home");
    let retail = [("retail", "_retail_")];
    let plain = |on: On, preview: &[&str]| {
        let roots = roots(on.home(), &retail, &[]);
        Built {
            on,
            preview: strings(preview),
            roots,
            env: Vec::new(),
            privacy: Privacy::NoWtf,
        }
    };
    match command {
        "install" => {
            let on = On::Home(published(log, &["wow"]));
            plain(on, &["install", "gh:o/foo", "-t", "retail"])
        }
        "update" => {
            let mut home = Home::new(&["wow"]);
            log.env(&home);
            with_bar(&home);
            home.installed_behind(FOO, ("v1", &foo(FOO_V1)), ("v2", &foo(FOO_V2)), &["retail"]);
            plain(On::Home(home), &["update", "foo", "-t", "retail"])
        }
        "pin" => {
            let on = On::Home(published(log, &["wow"]));
            setup(log, &on, &["install", "gh:o/foo", "-t", "retail"]);
            plain(on, &["pin", "foo", "-t", "retail"])
        }
        "unpin" => {
            let on = On::Home(published(log, &["wow"]));
            setup(
                log,
                &on,
                &["install", "gh:o/foo", "--version", "v1", "-t", "retail"],
            );
            plain(on, &["unpin", "foo", "-t", "retail"])
        }
        "adopt" => {
            let home = published(log, &["wow"]);
            unpack(&home.addons("_retail_"), FOO_V1);
            plain(
                On::Home(home),
                &["adopt", "Foo", "--ref", "gh:o/foo", "-t", "retail"],
            )
        }
        "sync" => {
            let home = published(log, &["wow"]);
            home.write_desired(
                "schema_version = \"1.0\"\n[family.mainline]\naddons = [\"gh:o/foo\"]\n",
            );
            plain(On::Home(home), &["sync"])
        }
        "copy" => {
            let on = On::Home(published(log, &["wow", "wow_classic"]));
            setup(log, &on, &["install", "gh:o/foo", "-t", "retail"]);
            let roots = roots(
                on.home(),
                &[("retail", "_retail_"), ("classic", "_classic_")],
                &[],
            );
            Built {
                on,
                preview: strings(&["copy", "--from", "retail", "--to", "classic"]),
                roots,
                env: Vec::new(),
                privacy: Privacy::NoWtf,
            }
        }
        "remove" => {
            let scene = scene(log);
            scene_install(log, &scene, "gh:o/foo", "v1");
            plain(On::Scene(scene), &["remove", "Foo", "-t", "retail"])
        }
        "undo" => {
            let scene = scene(log);
            let installed = scene_install(log, &scene, "gh:o/foo", "v1");
            let tx = installed.data()["tx"].as_str().unwrap().to_owned();
            plain(On::Scene(scene), &["undo", &tx])
        }
        "backup restore" => {
            let scene = scene(log);
            let client = scene.home.client("_retail_");
            let wtf = Wtf::at(client.clone()).saved_variables("Foo", 64).build();
            let at = "2026-10-05T08:00:00Z".parse().unwrap();
            let id = seed_backup(&client, omaddons::backup::BackupKind::Manual, at, 2);
            scene_install(log, &scene, "gh:o/foo", "v1");
            let roots = roots(
                &scene.home,
                &retail,
                &[
                    ("WTF", wtf),
                    ("backups", client.join("Interface/.omaddons/backups")),
                ],
            );
            Built {
                on: On::Scene(scene),
                preview: strings(&["backup", "restore", &id.to_string(), "-t", "retail"]),
                roots,
                env: Vec::new(),
                privacy: Privacy::FakeNames,
            }
        }
        "backup prune" => {
            let scene = scene(log);
            let client = scene.home.client("_retail_");
            for day in ["01", "02", "03"] {
                let at = format!("2026-10-{day}T08:00:00Z").parse().unwrap();
                seed_backup(&client, omaddons::backup::BackupKind::Auto, at, 1);
            }
            let at = "2026-10-05T08:00:00Z".parse().unwrap();
            seed_backup(&client, omaddons::backup::BackupKind::Manual, at, 1);
            let roots = roots(
                &scene.home,
                &retail,
                &[("backups", client.join("Interface/.omaddons/backups"))],
            );
            Built {
                on: On::Scene(scene),
                preview: strings(&["backup", "prune", "-t", "retail", "--keep", "2"]),
                roots,
                env: vec![("OMADDONS_NOW".to_owned(), PRUNE_NOW.to_owned())],
                privacy: Privacy::FakeNames,
            }
        }
        other => panic!("no home for {other}"), // ubs:ignore — a table bug
    }
}

/// `undo`'s `played` row: Foo v1 then v2 on a Scene with SavedVariables, a play session,
/// and a bare `undo`, which would put the settings from before v2 back.
pub(crate) fn built_undo_played(log: &TestLog) -> Built {
    log.step("seed home");
    let scene = scene(log).with_saved_variables(&["Foo"]);
    for zip in ["v1", "v2"] {
        let ran = scene_install(log, &scene, "gh:o/foo", zip);
        private(&ran, Privacy::SceneNames);
    }
    scene.play(&["Foo"]);
    let wtf = scene.home.client("_retail_").join("WTF");
    let roots = roots(&scene.home, &[("retail", "_retail_")], &[("WTF", wtf)]);
    Built {
        on: On::Scene(scene),
        preview: strings(&["undo"]),
        roots,
        env: Vec::new(),
        privacy: Privacy::SceneNames,
    }
}

/// How a refusal's `details.changed` must read.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Changed {
    Exactly(&'static [&'static str]),
    Contains(&'static [&'static str]),
}

/// A stale `apply`: `plan_stale`, not retryable, `details.changed` as `changed` says,
/// the plan's preview (`preview_argv` of its `display_argv`) as its fix, and no data.
#[track_caller]
pub(crate) fn stale(log: &TestLog, ran: &Ran, plan: &Value, changed: Changed) {
    let display: Vec<String> = serde_json::from_value(plan["display_argv"].clone()).unwrap();
    stale_with_fix(log, ran, &preview_argv(&display), changed);
}

/// A stale run whose fix is `fix`: a typed bound run's is its own argv previewed.
#[track_caller]
pub(crate) fn stale_with_fix(log: &TestLog, ran: &Ran, fix: &[String], changed: Changed) {
    ran.fails(1, "plan_stale");
    let error = &ran.envelope()["error"];
    log.json("refusal", error);
    log.assert_eq("not retryable", &error["retryable"], &json!(false));
    let got: Vec<String> = error["details"]["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part.as_str().map(str::to_owned))
        .collect();
    match changed {
        Changed::Exactly(parts) => log.assert_eq("changed", &got, &strings(parts)),
        Changed::Contains(parts) => {
            let missing: Vec<&str> = parts
                .iter()
                .copied()
                .filter(|part| !got.iter().any(|g| g == part))
                .collect();
            log.json("changed must contain", &json!({"parts": parts, "got": got}));
            log.assert_eq("changed contains", &missing, &Vec::<&str>::new());
        }
    }
    log.assert_eq("the fix previews again", &error["fix_argv"], &json!(fix));
    log.assert_eq(
        "no data",
        &ran.envelope().get("data").cloned().unwrap_or(Value::Null),
        &Value::Null,
    );
}
