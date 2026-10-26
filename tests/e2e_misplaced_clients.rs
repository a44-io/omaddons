//! e2e suite: misplaced clients (M4-D12): the wrong invocation changes nothing and its fix runs, a registered id becomes a client word, every unusable registry falls back to the built-in words, and the usage path reads no discovery state.
//!
//! A client named where a command or an argument belongs (`omaddons retail update`,
//! `omaddons status era`) is a usage error whose fix is the user's own command with the
//! client moved into `-t`. These scenarios check what only shows on real state and across
//! commands: the usage error is raised before anything is planned, recovered or written
//! (common rule 8), the fix runs, `registry.json`'s ids count only once registered, a
//! registry that cannot be used (missing, unparsable, a directory, unreadable, oversized,
//! a FIFO, newer) falls back without blocking or changing, and a FIFO at the discovery
//! cache proves the usage path never opens it.

mod common;

use std::fs;
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::Duration;

use common::archives::zip_text;
use common::env::TestEnv;
use common::home::Home;
use common::http::{Asset, gh_release};
use common::netlog::net_log;
use common::procfs::running_as_root;
use common::state::record;
use common::tlog::{Ran, Runner, TestLog, tlog};
use common::trees::{CustomRoot, tree_stat};
use omaddons::limits::DISCOVERY_FILE_MAX_BYTES;
use serde_json::{Value, json};

/// How long a usage error may take where a blocked open would hang.
const BOUND: Duration = Duration::from_secs(10);

/// Where a misplaced client was typed: in a command's place (hook 1) or an argument's
/// (hook 2). It decides the message.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Command,
    Argument,
}

/// (argv after `omaddons`, `fix_argv`, `details.target`, `details.command`).
type Case<'a> = (&'a [&'a str], &'a [&'a str], &'a str, &'a str);

/// `args` with the output every run here asks for.
fn json_argv(args: &[&str]) -> Vec<String> {
    args.iter()
        .copied()
        .chain(["--json", "--progress", "json"])
        .map(str::to_owned)
        .collect()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| v.as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Exactly one stderr `error` event, with the envelope's `fix_argv`.
#[track_caller]
fn one_error_event(ran: &Ran) {
    let ctx = ran.run.context();
    let errors: Vec<Value> = ran
        .events()
        .into_iter()
        .filter(|event| event["event"] == "error")
        .collect();
    assert_eq!(errors.len(), 1, "one error event\n{ctx}");
    assert_eq!(
        errors[0]["fix_argv"],
        ran.envelope()["error"]["fix_argv"],
        "{ctx}"
    );
}

/// The misplaced-client usage error: exit 2, the exact fix, `details`, the one help
/// alternative, the hook's message, one stderr `error` event.
#[track_caller]
fn assert_moved(ran: &Ran, slot: Slot, (_, fix, target, command): Case<'_>) {
    ran.fails(2, "usage").assert_private();
    let ctx = ran.run.context();
    let error = &ran.envelope()["error"];
    assert_eq!(strings(&error["fix_argv"]), fix, "{ctx}");
    assert_eq!(
        error["details"],
        json!({ "target": target, "command": command }),
        "{ctx}"
    );
    let help: Vec<&str> = ["omaddons", "help"]
        .into_iter()
        .chain(command.split(' '))
        .collect();
    assert_eq!(
        error["alternatives"],
        json!([{ "display": help.join(" "), "argv": help }]),
        "{ctx}"
    );
    let message = match slot {
        Slot::Command => format!("'{target}' is a client, not a command; the client goes in -t"),
        Slot::Argument => {
            format!("'{target}' is a client, not an argument of {command}; the client goes in -t")
        }
    };
    assert_eq!(error["message"], message, "{ctx}");
    one_error_event(ran);
}

/// The usage error an invocation had before M4-D12: exit 2, `fix`, no `details`.
#[track_caller]
fn assert_today(ran: &Ran, fix: &[&str]) {
    ran.fails(2, "usage").assert_private();
    let ctx = ran.run.context();
    let error = &ran.envelope()["error"];
    assert_eq!(strings(&error["fix_argv"]), fix, "{ctx}");
    assert_eq!(error["details"], json!({}), "{ctx}");
    one_error_event(ran);
}

/// Runs a misplaced-client invocation and asserts its error.
#[track_caller]
fn wrong(log: &TestLog, on: &impl Runner, slot: Slot, case: Case<'_>) {
    let ran = log.run(on, &json_argv(case.0));
    assert_moved(&ran, slot, case);
}

/// A run that must end within [`BOUND`]. The log fails one that does not, and its failure
/// output ends with the note saying what the hang means (`blocked`).
#[track_caller]
fn bounded(log: &TestLog, on: &impl Runner, args: &[&str], blocked: &str) -> Ran {
    let limit = BOUND.as_secs();
    log.note(format!(
        "{args:?} must end within {limit} s: a hang means {blocked}"
    ));
    log.cmd(on, &json_argv(args)).timeout(BOUND).run()
}

/// The trees a usage error must leave alone: `<state>`, `<config>`, `<cache>`, and `HOME`
/// (which holds the clients).
fn roots(env: &TestEnv) -> [(&'static str, std::path::PathBuf); 4] {
    [
        ("state", env.state()),
        ("config", env.config()),
        ("cache", env.cache()),
        ("home", env.home()),
    ]
}

fn baseline(log: &TestLog, env: &TestEnv) {
    for (label, root) in roots(env) {
        log.tree(label, &root);
    }
}

#[track_caller]
fn unchanged(log: &TestLog, env: &TestEnv) {
    for (label, root) in roots(env) {
        log.assert_unchanged(label, &root);
    }
}

fn mkfifo(path: &Path) {
    let made = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .is_ok_and(|status| status.success());
    assert!(made, "mkfifo is required (coreutils)");
}

/// Every path under `root` with its kind and length, relative: a listing that never opens
/// a file (a FIFO among them).
fn stat_listing(root: &Path) -> Vec<String> {
    tree_stat(root)
        .into_iter()
        .map(|(path, dir, len, _)| {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            format!("{rel} {} {len}", if dir { "dir" } else { "entry" })
        })
        .collect()
}

#[test]
fn misplaced_clients_before_the_verb_change_nothing_and_their_fixes_run() {
    let log = tlog!();
    log.step("home");
    let home = Home::new(&["wow", "wow_classic_era", "wow_classic"]);
    log.env(&home);
    baseline(&log, &home.env);

    log.step("wrong invocations");
    let cases: &[Case<'_>] = &[
        (
            &["retail", "update"],
            &["omaddons", "update", "-t", "retail"],
            "retail",
            "update",
        ),
        (
            &["era", "status"],
            &["omaddons", "status", "-t", "era"],
            "era",
            "status",
        ),
        (
            &["retail", "list"],
            &["omaddons", "list", "-t", "retail"],
            "retail",
            "list",
        ),
        (
            &["sod", "install", "questie"],
            &["omaddons", "install", "questie", "-t", "sod"],
            "sod",
            "install",
        ),
        // A word the rules know, though this home has no such client.
        (
            &["wotlk", "list"],
            &["omaddons", "list", "-t", "wotlk"],
            "wotlk",
            "list",
        ),
        (
            &["backup", "retail", "list"],
            &["omaddons", "backup", "list", "-t", "retail"],
            "retail",
            "backup list",
        ),
        (
            &["errors", "era"],
            &["omaddons", "errors", "-t", "era"],
            "era",
            "errors",
        ),
    ];
    for case in cases {
        wrong(&log, &home, Slot::Command, *case);
    }

    log.step("nothing changed");
    unchanged(&log, &home.env);

    log.step("the fixes run");
    log.run(&home, &json_argv(&["status", "-t", "era"]))
        .ok()
        .assert_private();
    log.run(&home, &json_argv(&["list", "-t", "retail"]))
        .ok()
        .assert_private();
}

#[test]
fn misplaced_clients_after_the_verb_change_nothing_and_their_fixes_run() {
    let log = tlog!();
    log.step("home");
    let home = Home::new(&["wow", "wow_classic_era", "wow_classic"]);
    log.env(&home);
    baseline(&log, &home.env);

    log.step("wrong invocations");
    let cases: &[Case<'_>] = &[
        (
            &["status", "retail"],
            &["omaddons", "status", "-t", "retail"],
            "retail",
            "status",
        ),
        (
            &["list", "era"],
            &["omaddons", "list", "-t", "era"],
            "era",
            "list",
        ),
        (
            &["outdated", "retail"],
            &["omaddons", "outdated", "-t", "retail"],
            "retail",
            "outdated",
        ),
        (
            &["history", "retail"],
            &["omaddons", "history", "-t", "retail"],
            "retail",
            "history",
        ),
        (
            &["export", "retail"],
            &["omaddons", "export", "-t", "retail"],
            "retail",
            "export",
        ),
        (
            &["info", "dbm", "era"],
            &["omaddons", "info", "dbm", "-t", "era"],
            "era",
            "info",
        ),
        (
            &["pin", "dbm", "retail"],
            &["omaddons", "pin", "dbm", "-t", "retail"],
            "retail",
            "pin",
        ),
        (
            &["unpin", "dbm", "retail"],
            &["omaddons", "unpin", "dbm", "-t", "retail"],
            "retail",
            "unpin",
        ),
    ];
    for case in cases {
        wrong(&log, &home, Slot::Argument, *case);
    }

    log.step("nothing changed");
    unchanged(&log, &home.env);

    log.step("the fixes run");
    log.run(&home, &json_argv(&["status", "-t", "retail"]))
        .ok()
        .assert_private();
    log.run(&home, &json_argv(&["list", "-t", "era"]))
        .ok()
        .assert_private();
}

#[test]
fn words_that_name_no_client_keep_todays_fix() {
    let log = tlog!();
    log.step("home");
    let home = Home::new(&["wow", "wow_classic_era", "wow_classic"]);
    log.env(&home);
    baseline(&log, &home.env);

    log.step("wrong invocations");
    let cases: &[(&[&str], &[&str])] = &[
        (&["frobnicate", "update"], &["omaddons", "help"]),
        (&["retail"], &["omaddons", "help"]),
        (&["status", "dbm"], &["omaddons", "help", "status"]),
        (&["info", "dbm", "questie"], &["omaddons", "help", "info"]),
        (&["retail", "update", "-t", "era"], &["omaddons", "help"]),
        (&["retail", "config"], &["omaddons", "help"]),
        (
            &["retail", "copy", "--from", "era", "--to", "classic"],
            &["omaddons", "help"],
        ),
        (&["config", "retail"], &["omaddons", "help", "config"]),
        (&["targets", "retail"], &["omaddons", "help", "targets"]),
    ];
    for (args, fix) in cases {
        let ran = log.run(&home, &json_argv(args));
        assert_today(&ran, fix);
        let message = ran.envelope()["error"]["message"]
            .as_str()
            .unwrap_or_default();
        assert!(
            message.starts_with("unrecognized subcommand")
                || message.starts_with("unexpected argument"),
            "{}",
            ran.run.context()
        );
    }

    log.step("nothing changed");
    unchanged(&log, &home.env);
}

/// A retail client and a custom 3.3.5a client outside `Games/`, registered as `turtle`;
/// `turtle` is no client word before its registration.
fn registered_turtle(log: &TestLog) -> Home {
    log.step("home");
    let home = Home::new(&["wow"]);
    log.env(&home);
    let turtle = CustomRoot::at(home.env.root().join("clients/turtle")).build();

    log.step("before registration");
    // The usage path reads only registry.json, which does not exist yet.
    let ran = log.run(&home, &json_argv(&["turtle", "list"]));
    assert_today(&ran, &["omaddons", "help"]);

    log.step("register");
    let path = turtle.display().to_string();
    log.run(
        &home,
        &json_argv(&["targets", "add", &path, "--name", "turtle"]),
    )
    .ok()
    .assert_private();
    home
}

const TURTLE_LIST: Case<'static> = (
    &["turtle", "list"],
    &["omaddons", "list", "-t", "turtle"],
    "turtle",
    "list",
);

#[test]
fn a_registered_custom_id_is_a_client_word() {
    let log = tlog!();
    let home = registered_turtle(&log);
    baseline(&log, &home.env);

    log.step("registered");
    wrong(&log, &home, Slot::Command, TURTLE_LIST);
    wrong(
        &log,
        &home,
        Slot::Argument,
        (
            &["status", "turtle"],
            &["omaddons", "status", "-t", "turtle"],
            "turtle",
            "status",
        ),
    );
    wrong(
        &log,
        &home,
        Slot::Command,
        (
            &["TURTLE", "update"],
            &["omaddons", "update", "-t", "TURTLE"],
            "TURTLE",
            "update",
        ),
    );
    unchanged(&log, &home.env);

    log.step("the fix runs");
    log.run(&home, &json_argv(&["list", "-t", "turtle"]))
        .ok()
        .assert_private();
}

/// A `registry.json` the usage path cannot use, put in place by the test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    Missing,
    Unparsable,
    Directory,
    Unreadable,
    Oversized,
    Fifo,
    Newer,
}

#[test]
fn an_unusable_registry_falls_back_to_the_builtin_words() {
    let log = tlog!();
    let home = registered_turtle(&log);
    let registry = home.state().join("registry.json");
    let good = fs::read(&registry).unwrap();
    let text = String::from_utf8(good.clone()).unwrap();
    let newer = text
        .replacen(
            "\"schema_version\": \"1.0\"",
            "\"schema_version\": \"9.0\"",
            1,
        )
        .into_bytes();
    assert_ne!(newer, good, "the registry names its version");
    // It would still decode: only the bound rejects it.
    let mut oversized = good.clone();
    oversized.resize(usize::try_from(DISCOVERY_FILE_MAX_BYTES).unwrap() + 1, b' ');
    let remove = |path: &Path| match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path).unwrap(),
        Ok(_) => fs::remove_file(path).unwrap(),
        Err(_) => {}
    };

    for variant in [
        Variant::Missing,
        Variant::Unparsable,
        Variant::Directory,
        Variant::Unreadable,
        Variant::Oversized,
        Variant::Fifo,
        Variant::Newer,
    ] {
        log.step(&format!("{variant:?}").to_lowercase());
        if variant == Variant::Unreadable && running_as_root() {
            log.note("unreadable: skipped, root reads mode 000 files");
            continue;
        }
        remove(&registry);
        let (what, bytes): (&str, Option<&[u8]>) = match variant {
            Variant::Missing => ("no registry.json", None),
            Variant::Unparsable => ("`{ not json`", Some(b"{ not json")),
            Variant::Directory => {
                fs::create_dir(&registry).unwrap();
                ("a directory at registry.json", None)
            }
            Variant::Unreadable => ("the registry with mode 000", None),
            Variant::Oversized => ("the registry padded past the bound", Some(&oversized)),
            Variant::Fifo => {
                mkfifo(&registry);
                ("a FIFO at registry.json", None)
            }
            Variant::Newer => ("the registry at schema_version 9.0", Some(&newer)),
        };
        if let Some(bytes) = bytes {
            fs::write(&registry, bytes).unwrap();
        }
        if variant == Variant::Unreadable {
            fs::write(&registry, &good).unwrap();
            fs::set_permissions(&registry, fs::Permissions::from_mode(0o000)).unwrap();
        }
        log.note(what);

        let blocked = "the usage path opened registry.json without checking its type";
        let ran = bounded(&log, &home, &["turtle", "list"], blocked);
        assert_today(&ran, &["omaddons", "help"]);
        let ran = bounded(&log, &home, &["retail", "update"], blocked);
        assert_moved(
            &ran,
            Slot::Command,
            (
                &["retail", "update"],
                &["omaddons", "update", "-t", "retail"],
                "retail",
                "update",
            ),
        );

        // The variant is left as it was.
        let meta = fs::symlink_metadata(&registry);
        match variant {
            Variant::Missing => assert!(meta.is_err(), "{what}: created"),
            Variant::Directory => assert!(meta.unwrap().is_dir(), "{what}: replaced"),
            Variant::Fifo => assert!(meta.unwrap().file_type().is_fifo(), "{what}: replaced"),
            Variant::Unreadable => {
                let mode = meta.unwrap().permissions().mode() & 0o7777;
                assert_eq!(mode, 0, "{what}: its mode changed");
            }
            Variant::Unparsable | Variant::Oversized | Variant::Newer => {
                let now = fs::read(&registry).unwrap();
                assert!(
                    now == bytes.unwrap_or_default(),
                    "{what}: its bytes changed"
                );
            }
        }
        remove(&registry);
        fs::write(&registry, &good).unwrap();
    }

    log.step("restored");
    wrong(&log, &home, Slot::Command, TURTLE_LIST);
}

#[test]
fn the_usage_path_reads_no_discovery_and_writes_nothing() {
    let log = tlog!();
    log.step("home");
    // No Games/, no client, no registry: nothing to discover.
    let env = TestEnv::new();
    log.env(&env);
    let cache = env.cache().join("omaddons");
    fs::create_dir_all(&cache).unwrap();
    // Any reader that opens it blocks, so a run that touched discovery state would hang.
    let discovery = cache.join("discovery.json");
    mkfifo(&discovery);
    let kept = [
        ("state", env.state()),
        ("config", env.config()),
        ("home", env.home()),
    ];
    for (label, root) in &kept {
        log.tree(label, root);
    }
    // Not `log.tree`, which would read the FIFO.
    let before = tree_stat(&env.cache());
    log.json("cache before", &stat_listing(&env.cache()));

    log.step("wrong invocations");
    let blocked =
        "the usage path opened <cache>/discovery.json (a FIFO): it must read no discovery state";
    for (args, fix) in [
        (
            &["retail", "update"][..],
            &["omaddons", "update", "-t", "retail"][..],
        ),
        (&["status", "era"], &["omaddons", "status", "-t", "era"]),
        (
            &["sod", "install", "questie"],
            &["omaddons", "install", "questie", "-t", "sod"],
        ),
    ] {
        let ran = bounded(&log, &env, args, blocked);
        ran.fails(2, "usage").assert_private();
        let ctx = ran.run.context();
        assert_eq!(strings(&ran.envelope()["error"]["fix_argv"]), fix, "{ctx}");
        let elapsed = &ran.envelope()["meta"]["elapsed_ms"];
        log.note(format!("{args:?}: meta.elapsed_ms {elapsed}"));
    }

    log.step("nothing changed");
    for (label, root) in &kept {
        log.assert_unchanged(label, root);
    }
    let after = tree_stat(&env.cache());
    if after != before {
        log.json("cache after", &stat_listing(&env.cache()));
    }
    assert_eq!(after, before, "<cache> changed");
    let kind = fs::symlink_metadata(&discovery).unwrap().file_type();
    assert!(kind.is_fifo(), "the discovery FIFO was replaced");
    assert!(!env.home().join("Games").exists(), "Games/ was created");
}

// ---- A client named among the addons (C2, M4-D12 hook 3) ----

const DBM: &str = "https://github.com/DeadlyBossMods/DeadlyBossMods";

/// (argv after `omaddons`, `fix_argv`, `details.target`, `details.command`, the
/// alternatives after the command's help).
type AsAnAddon<'a> = (
    &'a [&'a str],
    &'a [&'a str],
    &'a str,
    &'a str,
    &'a [&'a [&'a str]],
);

/// `retail` and `era`, with DBM `v1` (loading on both) installed on each client of `on`
/// and `v2` released since.
fn dbm_home(log: &TestLog, on: &[&str]) -> Home {
    let mut home = Home::new(&["wow", "wow_classic_era"]);
    log.env(&home);
    let toc = |version: &str| format!("## Interface: 120105, 11509\n## Version: {version}\n");
    let (v1, v2) = (toc("v1"), toc("v2"));
    let unannotated: &[(&str, u32)] = &[];
    let old: [Asset<'_>; 1] = [(
        "DBM-Core.zip",
        unannotated,
        vec![("DBM-Core/DBM-Core.toc", v1.as_str())],
    )];
    let new: [Asset<'_>; 1] = [(
        "DBM-Core.zip",
        unannotated,
        vec![("DBM-Core/DBM-Core.toc", v2.as_str())],
    )];
    home.installed_behind(DBM, ("v1", &old), ("v2", &new), on);
    let installed: serde_json::Map<String, Value> = on
        .iter()
        .map(|id| ((*id).to_owned(), json!(dbm_on(&home, id))))
        .collect();
    log.json("installed", &installed);
    home
}

/// DBM's recorded version on `id`, if it is installed there.
fn dbm_on(home: &Home, id: &str) -> Option<String> {
    record(home, id)
        .addons
        .values()
        .find(|a| a.name.eq_ignore_ascii_case("DBM"))
        .map(|a| a.version.clone())
}

fn dbm_versions(home: &Home) -> Value {
    json!({ "retail": dbm_on(home, "retail"), "era": dbm_on(home, "era") })
}

/// Writes a zip of `entries` under the home's `dl/` and returns its path.
fn zip_file(home: &Home, name: &str, entries: &[(&str, &str)]) -> String {
    let dl = home.env.root().join("dl");
    let path = dl.join(format!("{name}.zip"));
    let written = fs::create_dir_all(&dl).and_then(|()| fs::write(&path, zip_text(entries)));
    assert!(written.is_ok(), "{}: {written:?}", path.display());
    path.display().to_string()
}

/// Installs a `Foo` folder loading on retail from a local zip, so `retail` is registered
/// and manages an addon of its own.
fn install_foo(log: &TestLog, home: &Home) {
    let zip = zip_file(home, "foo", &[("Foo/Foo.toc", "## Interface: 120105\n")]);
    let argv = json_argv(&["install", "gh:o/foo", "--file", &zip, "-t", "retail"]);
    log.run(home, &argv).ok().assert_private();
}

/// The trees a misplaced client must leave alone: `<state>` (records, files indexes,
/// history, the journal, `net.log`), `<config>` and both clients. `<cache>` is only
/// listed: any command may refresh the derived discovery cache.
fn record_roots(home: &Home) -> [(&'static str, std::path::PathBuf); 4] {
    [
        ("state", home.env.state()),
        ("config", home.env.config()),
        ("retail", home.client("_retail_")),
        ("era", home.client("_classic_era_")),
    ]
}

fn records_baseline(log: &TestLog, home: &Home) {
    for (label, root) in record_roots(home) {
        log.tree(label, &root);
    }
    log.json("cache", &stat_listing(&home.env.cache()));
}

#[track_caller]
fn records_unchanged(log: &TestLog, home: &Home) {
    for (label, root) in record_roots(home) {
        log.assert_unchanged(label, &root);
    }
}

/// Runs a client named among the addons and asserts its usage error: exit 2, the exact
/// fix, `details`, the help alternative then the row's others, the message, one stderr
/// `error` event.
#[track_caller]
fn misplaced(log: &TestLog, home: &Home, (args, fix, target, command, others): AsAnAddon<'_>) {
    let ran = log.run(home, &json_argv(args));
    ran.fails(2, "usage").assert_private();
    let ctx = ran.run.context();
    let error = &ran.envelope()["error"];
    assert_eq!(error["retryable"], false, "{ctx}");
    assert_eq!(strings(&error["fix_argv"]), fix, "{ctx}");
    assert_eq!(
        error["details"],
        json!({ "target": target, "command": command }),
        "{ctx}"
    );
    let help: Vec<&str> = ["omaddons", "help"]
        .into_iter()
        .chain(command.split(' '))
        .collect();
    let alternatives: Vec<Vec<String>> = error["alternatives"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| strings(&a["argv"]))
        .collect();
    let mut want: Vec<Vec<String>> = vec![help.iter().map(|w| (*w).to_owned()).collect()];
    want.extend(
        others
            .iter()
            .map(|o| o.iter().map(|w| (*w).to_owned()).collect()),
    );
    assert_eq!(alternatives, want, "{ctx}");
    let message = match others {
        [] => format!("'{target}' is a client, not an addon; the client goes in -t"),
        _ => {
            let mut words = vec![format!("'{target}'")];
            words.extend(
                others
                    .iter()
                    .filter_map(|o| o.last())
                    .map(|w| format!("'{w}'")),
            );
            let (last, rest) = words.split_last().unwrap_or((&words[0], &[]));
            format!(
                "{} and {last} are clients, not addons; the client goes in -t, one per command",
                rest.join(", ")
            )
        }
    };
    assert_eq!(error["message"], message, "{ctx}");
    one_error_event(&ran);
}

#[test]
fn remove_and_update_with_a_misplaced_client_change_nothing() {
    let log = tlog!();
    log.step("home");
    let home = dbm_home(&log, &["retail", "era"]);
    records_baseline(&log, &home);
    let requests = net_log(&home).len();

    log.step("wrong invocations");
    let cases: &[AsAnAddon<'_>] = &[
        (
            &["remove", "dbm", "retail"],
            &["omaddons", "remove", "dbm", "-t", "retail"],
            "retail",
            "remove",
            &[],
        ),
        (
            &["rm", "dbm", "retail"],
            &["omaddons", "rm", "dbm", "-t", "retail"],
            "retail",
            "remove",
            &[],
        ),
        (
            &["update", "dbm", "retail"],
            &["omaddons", "update", "dbm", "-t", "retail"],
            "retail",
            "update",
            &[],
        ),
        (
            &["upgrade", "dbm", "retail", "--dry-run"],
            &["omaddons", "upgrade", "dbm", "-t", "retail", "--dry-run"],
            "retail",
            "update",
            &[],
        ),
        (
            &["update", "retail"],
            &["omaddons", "update", "-t", "retail"],
            "retail",
            "update",
            &[],
        ),
        (
            &["remove", "dbm", "retail", "--dry-run"],
            &["omaddons", "remove", "dbm", "-t", "retail", "--dry-run"],
            "retail",
            "remove",
            &[],
        ),
    ];
    for case in cases {
        misplaced(&log, &home, *case);
    }

    log.step("nothing changed");
    records_unchanged(&log, &home);
    let after = net_log(&home);
    log.json("net.log", &after.len());
    assert_eq!(
        after.len(),
        requests,
        "a request was sent\n{}",
        after.context()
    );

    log.step("the update fix runs");
    log.run(&home, &json_argv(&["update", "dbm", "-t", "retail"]))
        .ok()
        .assert_private();
    log.json("installed", &dbm_versions(&home));
    assert_eq!(dbm_on(&home, "retail").as_deref(), Some("v2"));
    assert_eq!(dbm_on(&home, "era").as_deref(), Some("v1"));

    log.step("the remove fix runs");
    log.run(&home, &json_argv(&["remove", "dbm", "-t", "retail"]))
        .ok()
        .assert_private();
    log.json("installed", &dbm_versions(&home));
    assert_eq!(dbm_on(&home, "retail"), None);
    assert_eq!(dbm_on(&home, "era").as_deref(), Some("v1"));
    assert!(home.addons("_classic_era_").join("DBM-Core").is_dir());
}

#[test]
fn a_misplaced_client_never_removes_from_another_client() {
    let log = tlog!();
    log.step("home");
    let home = dbm_home(&log, &["era"]);
    install_foo(&log, &home);
    records_baseline(&log, &home);

    log.step("wrong invocation");
    misplaced(
        &log,
        &home,
        (
            &["remove", "dbm", "retail"],
            &["omaddons", "remove", "dbm", "-t", "retail"],
            "retail",
            "remove",
            &[],
        ),
    );

    log.step("nothing changed");
    records_unchanged(&log, &home);
    assert_eq!(dbm_on(&home, "era").as_deref(), Some("v1"));
    log.note(
        "before C2 this removed DBM from era (remove.rs:254-258), then warned that retail is not managed",
    );

    log.step("the fix says where DBM is");
    log.run(&home, &json_argv(&["remove", "dbm", "-t", "retail"]))
        .fails(3, "not_managed")
        .assert_private();
    records_unchanged(&log, &home);
}

#[test]
fn install_with_a_misplaced_client_installs_nothing() {
    let log = tlog!();
    log.step("home");
    let home = Home::new(&["wow", "wow_classic_era"]);
    log.env(&home);
    let unannotated: &[(&str, u32)] = &[];
    let questie: [Asset<'_>; 1] = [(
        "Questie.zip",
        unannotated,
        vec![(
            "Questie/Questie.toc",
            "## Interface: 11509\n## Version: v1\n",
        )],
    )];
    gh_release(
        &home.http,
        "https://github.com/Questie/Questie",
        "v1",
        &questie,
    );
    install_foo(&log, &home);
    records_baseline(&log, &home);
    let requests = net_log(&home).len();

    log.step("wrong invocations");
    let cases: &[AsAnAddon<'_>] = &[
        (
            &["install", "questie", "era"],
            &["omaddons", "install", "questie", "-t", "era"],
            "era",
            "install",
            &[],
        ),
        (
            &["install", "questie", "sod"],
            &["omaddons", "install", "questie", "-t", "sod"],
            "sod",
            "install",
            &[],
        ),
    ];
    for case in cases {
        misplaced(&log, &home, *case);
    }

    log.step("nothing changed");
    records_unchanged(&log, &home);
    let after = net_log(&home);
    assert_eq!(
        after.len(),
        requests,
        "a request was sent\n{}",
        after.context()
    );

    log.step("the fix runs");
    log.run(&home, &json_argv(&["install", "questie", "-t", "era"]))
        .ok()
        .assert_private();
    let named = |id: &str| {
        record(&home, id)
            .addons
            .values()
            .any(|a| a.name.eq_ignore_ascii_case("Questie"))
    };
    assert!(named("era"), "Questie is not on era");
    assert!(!named("retail"), "Questie is on retail");
}

#[test]
fn ordinary_partial_success_is_unchanged() {
    let log = tlog!();
    log.step("home");
    let home = dbm_home(&log, &["retail", "era"]);
    records_baseline(&log, &home);

    log.step("an unknown name fails alone");
    let ran = log.run(
        &home,
        &json_argv(&["remove", "dbm", "nosuch", "-t", "retail"]),
    );
    ran.ok().assert_private();
    let ctx = ran.run.context();
    assert_eq!(ran.envelope()["partial"], true, "{ctx}");
    let unknown = ran.envelope()["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|w| {
            w["code"] == "not_managed"
                && w["message"].as_str().is_some_and(|m| m.contains("nosuch"))
        });
    assert!(unknown, "no not_managed warning naming nosuch\n{ctx}");
    assert_eq!(dbm_on(&home, "retail"), None, "{ctx}");
    assert_eq!(dbm_on(&home, "era").as_deref(), Some("v1"), "{ctx}");
    log.tree("retail after", &home.client("_retail_"));

    log.step("a target given: the client word fails alone");
    let ran = log.run(&home, &json_argv(&["update", "dbm", "retail", "-t", "era"]));
    ran.ok().assert_private();
    let ctx = ran.run.context();
    assert_eq!(ran.envelope()["partial"], true, "{ctx}");
    assert_eq!(dbm_on(&home, "era").as_deref(), Some("v2"), "{ctx}");
    let failed = ran.data()["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|t| t["items"].as_array().into_iter().flatten())
        .any(|item| {
            item["request"] == "retail"
                && item["status"] == "failed"
                && item["error"]["code"] == "not_managed"
        });
    assert!(failed, "no failed not_managed item for retail\n{ctx}");
    log.assert_unchanged("retail after", &home.client("_retail_"));

    log.step("an unknown name with no client word");
    for (label, root) in record_roots(&home) {
        log.tree(&format!("{label} before nosuch"), &root);
    }
    log.run(&home, &json_argv(&["remove", "nosuch"]))
        .fails(3, "not_managed")
        .assert_private();
    for (label, root) in record_roots(&home) {
        log.assert_unchanged(&format!("{label} before nosuch"), &root);
    }
}

#[test]
fn an_addon_named_like_a_client_is_never_moved() {
    let log = tlog!();
    log.step("home");
    let home = Home::new(&["wow", "wow_classic_era"]);
    log.env(&home);
    // `hardcore` is also a game mode of era's client.
    let catalog_d = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&catalog_d).unwrap();
    fs::write(
        catalog_d.join("50-hardcore.toml"),
        "schema_version = \"1.0\"\n[[addon]]\nname = \"Hardcore\"\nref = \"gh:o/hardcore\"\nfamilies = [\"vanilla\"]\nfolders = [\"Hardcore\"]\n",
    )
    .unwrap();
    let unannotated: &[(&str, u32)] = &[];
    let hardcore: [Asset<'_>; 1] = [(
        "Hardcore.zip",
        unannotated,
        vec![(
            "Hardcore/Hardcore.toc",
            "## Interface: 11509\n## Version: v1\n",
        )],
    )];
    gh_release(&home.http, "https://github.com/o/hardcore", "v1", &hardcore);
    // An addon owning a folder named like the mode `sod`.
    let zip = zip_file(&home, "sod", &[("SoD/SoD.toc", "## Interface: 11509\n")]);
    let argv = json_argv(&["install", "gh:o/sod", "--file", &zip, "-t", "era"]);
    log.run(&home, &argv).ok().assert_private();
    log.json(
        "era",
        &record(&home, "era").addons.keys().collect::<Vec<_>>(),
    );

    log.step("a catalog name is read as the addon");
    log.run(&home, &json_argv(&["install", "hardcore", "-t", "era"]))
        .ok()
        .assert_private();
    log.run(&home, &json_argv(&["update", "hardcore"]))
        .ok()
        .assert_private();
    misplaced(
        &log,
        &home,
        (
            &["update", "hardcore", "era"],
            &["omaddons", "update", "hardcore", "-t", "era"],
            "era",
            "update",
            &[],
        ),
    );

    log.step("an owned folder is read as the addon");
    log.run(&home, &json_argv(&["remove", "sod"]))
        .ok()
        .assert_private();
    let owns_sod = record(&home, "era")
        .addons
        .values()
        .any(|a| a.folders.iter().any(|f| f.as_str() == "SoD"));
    assert!(!owns_sod, "the addon owning SoD is still on era");

    log.step("a client word after -- is never moved");
    let argv: Vec<String> = [
        "remove",
        "hardcore",
        "--json",
        "--progress",
        "json",
        "--",
        "era",
    ]
    .map(str::to_owned)
    .to_vec();
    let ran = log.run(&home, &argv);
    ran.ok().assert_private();
    assert_eq!(ran.envelope()["partial"], true, "{}", ran.run.context());
}

#[test]
fn several_misplaced_clients_move_the_first() {
    let log = tlog!();
    log.step("home");
    let home = dbm_home(&log, &["retail", "era"]);
    records_baseline(&log, &home);

    log.step("wrong invocation");
    misplaced(
        &log,
        &home,
        (
            &["update", "dbm", "retail", "era"],
            &["omaddons", "update", "dbm", "-t", "retail"],
            "retail",
            "update",
            &[&["omaddons", "update", "dbm", "-t", "era"]],
        ),
    );

    log.step("a lone client word");
    log.run(&home, &json_argv(&["remove", "retail"]))
        .fails(3, "not_managed")
        .assert_private();

    log.step("nothing changed");
    records_unchanged(&log, &home);
}
