//! `apply` end to end (D5's suite): what a stored plan refuses and why, how `apply` finds
//! it, and what it runs, through the real binary with every invocation logged.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp};
use omaddons::limits::PLAN_MAX_BYTES;
use serde_json::{Value, json};

use super::commands::{self, strings};
use crate::common::home::Home;
use crate::common::netlog::net_log;
use crate::common::plans::{edit_plan, lease_files, plans_dir, read_plan, redact_ulids};
use crate::common::tlog::{Cmd, Ran, TestLog, tlog};

// ---------------------------------------------------------------------------------------
// Support.

/// A retail and a Mists Classic client, Foo's v1 release served as `gh:o/foo`, and the
/// same archive as a local zip, whose path is returned.
fn seeded(log: &TestLog) -> (Home, String) {
    log.step("seed home");
    let home = Home::new(&["wow", "wow_classic"]);
    log.env(&home);
    commands::serve(&home, "Foo", "1");
    let zip = commands::zip(&home, "Foo", "1").display().to_string();
    (home, zip)
}

/// A dry run that stored its plan: the plan's id, the preview's token and the file.
struct Preview {
    id: String,
    token: Value,
    plan: Value,
}

impl Preview {
    fn display_argv(&self) -> Vec<String> {
        words(&self.plan["display_argv"])
    }

    fn expires_at(&self) -> Timestamp {
        self.plan["expires_at"].as_str().unwrap().parse().unwrap()
    }

    /// A lifecycle refusal's fix: the plan's display argv as a dry run, before any `--`.
    fn dry_run_fix(&self) -> Vec<String> {
        let mut argv = self.display_argv();
        let at = argv.iter().position(|a| a == "--").unwrap_or(argv.len());
        argv.insert(at, "--dry-run".to_owned());
        argv
    }
}

fn words(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// `args` as a dry run that must store a plan, at the suite's clock; logs the plan file.
fn preview(log: &TestLog, home: &Home, args: &[&str]) -> Preview {
    log.step(&format!("preview {}", args.join(" ")));
    let ran = commands::preview(log, home, &strings(args));
    stored(log, home, &ran)
}

/// The plan the dry run `ran` stored.
fn stored(log: &TestLog, home: &Home, ran: &Ran) -> Preview {
    ran.ok().assert_private();
    let data = ran.data();
    let id = data["plan_id"].as_str().unwrap_or_default().to_owned();
    assert!(!id.is_empty(), "no plan stored: {data}");
    let plan = read_plan(&home.env, &id);
    log.json("plan file", &plan);
    Preview {
        id,
        token: data["precondition"].clone(),
        plan,
    }
}

/// `omaddons apply <id> <extra…> --json --progress json`, at the suite's clock.
fn apply<'a>(log: &'a TestLog, home: &Home, id: &str, extra: &[&str]) -> Cmd<'a> {
    let mut args = vec!["apply", id];
    args.extend_from_slice(extra);
    commands::command(log, home, &strings(&args), true)
}

/// `omaddons <args> --json --progress json` on the real clock, for homes whose release
/// memo was written at real times (`Home::installed_behind`): the suite's fixed clock
/// would read it as written in the future, so never due.
fn real<'a>(log: &'a TestLog, home: &Home, args: &[&str]) -> Cmd<'a> {
    let mut argv = strings(args);
    argv.extend(strings(&["--json", "--progress", "json"]));
    log.cmd(home, &argv)
}

/// Each `outcomes[]` entry's target and status, in order.
fn outcomes(ran: &Ran) -> Vec<(String, String)> {
    ran.data()["outcomes"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|o| {
                    let field = |key: &str| o[key].as_str().unwrap_or_default().to_owned();
                    (field("target"), field("status"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What a run leaves, as a direct run on an identical home must leave it too: each
/// client's AddOns and `WTF/` content, its records as `list` shows them (keys dropped,
/// the home's root written `<root>`), and its backup count.
fn left(log: &TestLog, home: &Home) -> Value {
    let hash = |dir: PathBuf| dir.exists().then(|| crate::common::trees::tree_hash(&dir));
    let mut clients = BTreeMap::new();
    for (id, folder) in [("classic", "_classic_"), ("retail", "_retail_")] {
        clients.insert(
            id,
            json!({
                "addons": hash(home.addons(folder)),
                "wtf": hash(home.client(folder).join("WTF")),
            }),
        );
    }
    let root = home.env.root().display().to_string();
    let listed = commands::run(log, home, &strings(&["list"]));
    let mut records: Value =
        serde_json::from_str(&listed.data().to_string().replace(&root, "<root>")).unwrap();
    // What differs between two runs of one change: the client's key, and when each
    // addon was installed.
    for target in records["targets"].as_array_mut().into_iter().flatten() {
        target.as_object_mut().map(|t| t.remove("key"));
        for addon in target["addons"].as_array_mut().into_iter().flatten() {
            addon.as_object_mut().map(|a| a.remove("installed_at"));
        }
    }
    let backups = commands::run(log, home, &strings(&["backup", "list"]));
    let totals: BTreeMap<String, Value> = backups.data()["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            (
                t["target"]["id"].as_str().unwrap_or_default().to_owned(),
                t["total"].clone(),
            )
        })
        .collect();
    json!({"clients": clients, "records": records, "backups": totals})
}

/// Foo's v1, v2 and v3 TOCs, which retail and Mists Classic load.
const FOO_TOCS: [&str; 3] = [
    "## Interface: 120105,50504\n## Version: 1\n",
    "## Interface: 120105,50504\n## Version: 2\n",
    "## Interface: 120105,50504\n## Version: 3\n",
];
const FLAVORS: &[(&str, u32)] = &[("mainline", 120_105), ("mists", 50_504)];

/// Foo's release `v<n>` (1 to 3) as one GitHub asset.
fn foo_release(n: usize) -> [crate::common::http::Asset<'static>; 1] {
    [("Foo.zip", FLAVORS, vec![("Foo/Foo.toc", FOO_TOCS[n - 1])])]
}

/// A home whose `targets` run Foo v1 while v2 is the latest release: the memo a day old.
fn behind(log: &TestLog, targets: &[&str]) -> Home {
    log.step(&format!(
        "seed home: Foo v1 on {}, v2 published",
        targets.join(", ")
    ));
    let mut home = Home::new(&["wow", "wow_classic"]);
    log.env(&home);
    home.installed_behind(
        "https://github.com/o/foo",
        ("v1", &foo_release(1)),
        ("v2", &foo_release(2)),
        targets,
    );
    home
}

/// What a refused run or a dry run leaves as it was: every client dir, `<state>` (its
/// `net.log` too: nothing is requested) and every plan file. A real `apply` takes its
/// plan's lease before its checks (M4-D61), so a `.lease` file may appear beside the
/// plan; the plans themselves never change, and no plan appears.
struct Trees {
    roots: Vec<(&'static str, PathBuf)>,
    plans: BTreeMap<String, String>,
}

impl Trees {
    fn record(log: &TestLog, home: &Home) -> Self {
        let roots = vec![
            ("retail", home.client("_retail_")),
            ("classic", home.client("_classic_")),
            ("state", home.state()),
        ];
        for (label, root) in &roots {
            log.tree(label, root);
        }
        let plans = super::hashes(home);
        log.json("plan files", &plans);
        Self { roots, plans }
    }

    fn unchanged(&self, log: &TestLog, home: &Home) {
        for (label, root) in &self.roots {
            log.assert_unchanged(label, root);
        }
        log.assert_eq("plan files", &super::hashes(home), &self.plans);
    }
}

/// A lifecycle refusal: `plan_stale` with `reason`, never retryable, nothing changed,
/// and the plan's dry run as the fix; returns its details.
fn refused<'r>(log: &TestLog, ran: &'r Ran, p: &Preview, reason: &str) -> &'r Value {
    ran.fails(1, "plan_stale").assert_private();
    let error = &ran.envelope()["error"];
    log.assert_eq("reason", &error["details"]["reason"], &json!(reason));
    log.assert_eq("changed", &error["details"]["changed"], &json!([]));
    log.assert_eq("retryable", &error["retryable"], &json!(false));
    log.assert_eq("fix", &words(&error["fix_argv"]), &p.dry_run_fix());
    &error["details"]
}

/// The argv of each `error.alternatives` entry.
fn alternatives(ran: &Ran) -> Vec<Vec<String>> {
    ran.envelope()["error"]["alternatives"]
        .as_array()
        .map(|items| items.iter().map(|a| words(&a["argv"])).collect())
        .unwrap_or_default()
}

fn install_zip(zip: &str) -> [&str; 6] {
    ["install", "gh:o/foo", "--file", zip, "-t", "retail"]
}

// ---------------------------------------------------------------------------------------
// Lifecycle refusals.

#[test]
fn apply_refuses_an_expired_plan() {
    let log = tlog!();
    let (home, zip) = seeded(&log);
    let p = preview(&log, &home, &install_zip(&zip));
    let trees = Trees::record(&log, &home);

    log.step("apply a minute after the plan expired");
    let late = (p.expires_at() + SignedDuration::from_mins(1)).to_string();
    let ran = apply(&log, &home, &p.id, &[])
        .env("OMADDONS_NOW", &late)
        .run();
    refused(&log, &ran, &p, "expired");
    trees.unchanged(&log, &home);
}

#[test]
fn apply_refuses_a_plan_from_another_build() {
    let log = tlog!();
    let (home, zip) = seeded(&log);
    let p = preview(&log, &home, &install_zip(&zip));

    log.step("edit plan: another build's id");
    edit_plan(&home.env, &p.id, |plan| {
        plan["build_id"] = json!("0000000000000000");
    });
    let trees = Trees::record(&log, &home);

    log.step("apply");
    let ran = apply(&log, &home, &p.id, &[]).run();
    refused(&log, &ran, &p, "binary_changed");
    trees.unchanged(&log, &home);
}

#[test]
fn apply_refuses_a_plan_whose_input_is_gone() {
    let log = tlog!();
    {
        let log = log.case("zip");
        let (home, zip) = seeded(&log);
        let p = preview(&log, &home, &install_zip(&zip));
        log.step("delete the zip");
        fs::remove_file(&zip).unwrap();
        let trees = Trees::record(&log, &home);
        log.step("apply");
        let ran = apply(&log, &home, &p.id, &[]).run();
        let details = refused(&log, &ran, &p, "input_missing");
        log.assert_eq("path", &details["path"], &json!(zip));
        trees.unchanged(&log, &home);
    }
    {
        let log = log.case("sync file");
        let (home, _) = seeded(&log);
        let file = home
            .write_desired("schema_version = \"1.0\"\n[target.retail]\naddons = [\"gh:o/foo\"]\n")
            .display()
            .to_string();
        let p = preview(&log, &home, &["sync", &file, "-t", "retail"]);
        log.step("delete addons.toml");
        fs::remove_file(&file).unwrap();
        let trees = Trees::record(&log, &home);
        log.step("apply");
        let ran = apply(&log, &home, &p.id, &[]).run();
        let details = refused(&log, &ran, &p, "input_missing");
        log.assert_eq("path", &details["path"], &json!(file));
        trees.unchanged(&log, &home);
    }
}

#[test]
fn apply_twice_is_already_applied_with_the_first_transactions() {
    let log = tlog!();
    let (home, zip) = seeded(&log);
    let p = preview(&log, &home, &install_zip(&zip));

    log.step("apply");
    let first = apply(&log, &home, &p.id, &[]).run();
    first.ok().assert_private();
    let tx = first.data()["result"]["tx"].clone();
    let trees = Trees::record(&log, &home);

    log.step("second apply");
    let ran = apply(&log, &home, &p.id, &[]).run();
    let details = refused(&log, &ran, &p, "already_applied");
    log.assert_eq("txs", &details["txs"], &json!([tx]));

    log.step("second apply after the plan expired");
    let late = (p.expires_at() + SignedDuration::from_mins(1)).to_string();
    let ran = apply(&log, &home, &p.id, &[])
        .env("OMADDONS_NOW", &late)
        .run();
    let details = refused(&log, &ran, &p, "already_applied");
    log.assert_eq("txs after expiry", &details["txs"], &json!([tx]));
    trees.unchanged(&log, &home);

    log.step("a plan over two clients");
    commands::run(
        &log,
        &home,
        &strings(&["install", "gh:o/foo", "--file", &zip, "-t", "classic"]),
    );
    let p = preview(&log, &home, &["pin", "Foo"]);
    apply(&log, &home, &p.id, &[]).run().ok().assert_private();
    let history = commands::run(&log, &home, &strings(&["history"]));
    let mut txs: Vec<Value> = history.data()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["plan"] == json!(p.id))
        .map(|entry| entry["tx"].clone())
        .collect();
    txs.reverse();
    log.json("the plan's transactions, oldest first", &txs);
    log.assert_eq("one per client", &txs.len(), &2);
    let trees = Trees::record(&log, &home);

    log.step("second apply of the two-client plan");
    let ran = apply(&log, &home, &p.id, &[]).run();
    let details = refused(&log, &ran, &p, "already_applied");
    log.assert_eq("both txs, oldest first", &details["txs"], &json!(txs));
    trees.unchanged(&log, &home);
}

// ---------------------------------------------------------------------------------------
// Unreadable plans and resolution.

#[test]
fn apply_fails_closed_on_an_unreadable_plan() {
    let log = tlog!();
    let (home, zip) = seeded(&log);
    let p = preview(&log, &home, &install_zip(&zip));
    let path = plans_dir(&home.env).join(format!("{}.json", p.id));
    let original = fs::read(&path).unwrap();
    let edited = |edit: &dyn Fn(&mut Value)| {
        let mut plan = p.plan.clone();
        edit(&mut plan);
        serde_json::to_vec(&plan).unwrap()
    };
    let padding = usize::try_from(PLAN_MAX_BYTES).unwrap();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("truncated", original[..original.len() / 2].to_vec()),
        (
            "oversized",
            edited(&|plan| {
                plan["summary"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("x".repeat(padding)));
            }),
        ),
        (
            "unknown_variant",
            edited(&|plan| plan["intent"]["command"] = json!("frobnicate")),
        ),
        (
            "unknown_option",
            edited(&|plan| plan["intent"]["bogus"] = json!(true)),
        ),
        (
            "relative_path",
            edited(&|plan| plan["intent"]["file"] = json!("foo.zip")),
        ),
        (
            "display_argv_disagrees",
            edited(&|plan| {
                let argv = plan["display_argv"].as_array_mut().unwrap();
                let last = argv.len() - 1;
                argv[last] = json!("classic");
            }),
        ),
        (
            "token_disagrees",
            edited(&|plan| {
                let token = plan["precondition"]["token"].as_str().unwrap().to_owned();
                let (head, digit) = token.split_at(token.len() - 1);
                let other = if digit == "a" { "b" } else { "a" };
                plan["precondition"]["token"] = json!(format!("{head}{other}"));
            }),
        ),
    ];
    for (case, bytes) in cases {
        let log = log.case(case);
        log.step("edit plan");
        fs::write(&path, &bytes).unwrap();
        let trees = Trees::record(&log, &home);
        log.step("apply");
        let ran = apply(&log, &home, &p.id, &[]).run();
        ran.fails(3, "plan_not_found");
        let details = &ran.envelope()["error"]["details"];
        log.assert_eq("reason", &details["reason"], &json!("unreadable"));
        log.assert_eq("path", &details["path"], &json!(path.display().to_string()));
        trees.unchanged(&log, &home);
        log.assert_eq("no lease", &lease_files(&home.env), &Vec::<PathBuf>::new());
    }

    log.step("the original plan still applies");
    fs::write(&path, &original).unwrap();
    apply(&log, &home, &p.id, &[]).run().ok().assert_private();
}

#[test]
fn apply_resolves_ids_and_prefixes() {
    let log = tlog!();
    type Form = fn(&str) -> String;
    let forms: [(&str, Form); 3] = [
        ("full id", |id| id.to_owned()),
        ("8-character prefix", |id| id[..8].to_owned()),
        ("lowercase prefix", |id| id[..8].to_ascii_lowercase()),
    ];
    for (case, form) in forms {
        let log = log.case(case);
        let (home, zip) = seeded(&log);
        let p = preview(&log, &home, &install_zip(&zip));
        log.step("apply");
        let ran = apply(&log, &home, &form(&p.id), &[]).run();
        ran.ok().assert_private();
        log.assert_eq("the plan applied", &ran.data()["plan_id"], &json!(p.id));
        log.assert_eq(
            "committed",
            &ran.data()["outcomes"][0]["status"],
            &json!("committed"),
        );
    }

    let log = log.case("unknown, short and ambiguous");
    let (home, zip) = seeded(&log);
    let p = preview(&log, &home, &install_zip(&zip));
    log.step("plant a second plan sharing a 20-character prefix");
    let twin = format!("{}ZZZZZZ", &p.id[..20]);
    assert_ne!(twin, p.id);
    let dir = plans_dir(&home.env);
    fs::copy(
        dir.join(format!("{}.json", p.id)),
        dir.join(format!("{twin}.json")),
    )
    .unwrap();
    let trees = Trees::record(&log, &home);

    log.step("an unknown id");
    let unknown = "01ZZZZZZZZZZZZZZZZZZZZZZZZ";
    let ran = apply(&log, &home, unknown, &[]).run();
    ran.fails(3, "plan_not_found");
    let error = &ran.envelope()["error"];
    log.assert_eq("prefix", &error["details"]["prefix"], &json!(unknown));
    log.assert_eq(
        "fix",
        &words(&error["fix_argv"]),
        &strings(&["omaddons", "help", "apply"]),
    );
    log.assert_eq(
        "alternatives",
        &alternatives(&ran),
        &vec![strings(&["omaddons", "adopt", "--apply"])],
    );

    log.step("a prefix under 8 characters");
    apply(&log, &home, "01M8", &[]).run().fails(2, "usage");

    log.step("a prefix two plans share");
    let ran = apply(&log, &home, &p.id[..10], &[]).run();
    ran.fails(3, "ref_ambiguous");
    let mut ids = vec![p.id.clone(), twin];
    ids.sort();
    let error = &ran.envelope()["error"];
    log.assert_eq("candidates", &error["details"]["candidates"], &json!(ids));
    log.assert_eq(
        "fix",
        &words(&error["fix_argv"]),
        &strings(&["omaddons", "apply", &ids[0], "--dry-run"]),
    );
    log.assert_eq(
        "alternatives",
        &alternatives(&ran),
        &ids.iter()
            .map(|id| strings(&["omaddons", "apply", id]))
            .collect::<Vec<_>>(),
    );
    trees.unchanged(&log, &home);
}

// ---------------------------------------------------------------------------------------
// Refused flags, hints, the working directory and offline runs.

#[test]
fn apply_refuses_if_match_target_and_while_running() {
    let log = tlog!();
    let (home, zip) = seeded(&log);
    let p = preview(&log, &home, &install_zip(&zip));
    let trees = Trees::record(&log, &home);
    let token = p.token.as_str().unwrap();
    let bare = strings(&["omaddons", "apply", &p.id]);
    for (case, extra, flag, fix) in [
        (
            "--if-match",
            vec!["--if-match", token],
            Value::Null,
            strings(&["omaddons", "help", "apply"]),
        ),
        ("-t", vec!["-t", "retail"], json!("--target"), bare.clone()),
        (
            "--while-running",
            vec!["--while-running"],
            json!("--while-running"),
            bare.clone(),
        ),
    ] {
        let log = log.case(case);
        log.step("apply");
        let ran = apply(&log, &home, &p.id, &extra).run();
        ran.fails(2, "usage");
        let error = &ran.envelope()["error"];
        log.assert_eq("fix", &words(&error["fix_argv"]), &fix);
        log.assert_eq("flag", &error["details"]["flag"], &flag);
    }
    log.step("nothing changed and no lease was opened");
    trees.unchanged(&log, &home);
    log.assert_eq("no lease", &lease_files(&home.env), &Vec::<PathBuf>::new());

    log.step("apply");
    apply(&log, &home, &p.id, &[]).run().ok().assert_private();
}

#[test]
fn apply_hints_end_to_end() {
    let log = tlog!();
    // Each home holds Foo unpacked by hand into retail.
    let build = |log: &TestLog| {
        let (home, _) = seeded(log);
        commands::unmanaged(&home, "Foo");
        home
    };
    let home = build(&log);
    let trees = Trees::record(&log, &home);
    let help = strings(&["omaddons", "help", "apply"]);

    {
        let log = log.case("bare apply");
        log.step("wrong");
        let ran = commands::command(&log, &home, &strings(&["apply"]), true).run();
        ran.fails(2, "usage");
        log.assert_eq("fix", &words(&ran.envelope()["error"]["fix_argv"]), &help);
        log.assert_eq(
            "alternatives",
            &alternatives(&ran),
            &vec![strings(&["omaddons", "adopt", "--apply"])],
        );
        log.step("fix");
        commands::run(&log, &home, &help[1..]);
    }
    {
        let log = log.case("apply <an unmanaged folder>");
        log.step("wrong");
        let ran = commands::command(&log, &home, &strings(&["apply", "Foo"]), true).run();
        ran.fails(3, "not_found");
        log.assert_eq("fix", &words(&ran.envelope()["error"]["fix_argv"]), &help);
        let adopt = strings(&["omaddons", "adopt", "Foo", "-t", "retail"]);
        log.assert_eq("alternatives", &alternatives(&ran), &vec![adopt.clone()]);
        log.step("the alternative adopts Foo on an identical home");
        let twin = build(&log);
        let adopted = commands::run(&log, &twin, &adopt[1..]);
        log.assert_eq(
            "adopted",
            &adopted.data()["targets"][0]["items"][0]["status"],
            &json!("adopted"),
        );
    }
    {
        let log = log.case("adopt apply");
        log.step("wrong");
        let wrong = strings(&["adopt", "apply", "-t", "retail"]);
        let ran = commands::command(&log, &home, &wrong, true).run();
        ran.fails(2, "usage");
        let fix = strings(&["omaddons", "adopt", "--apply", "-t", "retail"]);
        log.assert_eq("fix", &words(&ran.envelope()["error"]["fix_argv"]), &fix);
        log.step("the fix adopts Foo on an identical home");
        let twin = build(&log);
        let adopted = commands::run(&log, &twin, &fix[1..]);
        log.assert_eq(
            "adopted",
            &adopted.data()["targets"][0]["items"][0]["status"],
            &json!("adopted"),
        );
    }
    {
        let log = log.case("aply");
        log.step("wrong");
        let ran = commands::command(&log, &home, &strings(&["aply", "01M8Q2VA"]), true).run();
        ran.fails(2, "usage");
        log.assert_eq("fix", &words(&ran.envelope()["error"]["fix_argv"]), &help);
        log.step("fix");
        commands::run(&log, &home, &help[1..]);
    }
    log.step("no wrong invocation changed the home");
    trees.unchanged(&log, &home);
}

/// A plan previewed with a path relative to a directory since deleted applies from
/// another: the stored path is absolute, and the token was taken over that same path.
#[test]
fn apply_works_from_another_directory_after_the_preview_directory_is_gone() {
    let log = tlog!();
    for (case, args, stored, prefix) in [
        (
            "--file",
            &[
                "install",
                "gh:o/foo",
                "--file",
                "../zips/foo.zip",
                "-t",
                "retail",
            ][..],
            "/intent/file",
            "",
        ),
        (
            "a local ref",
            &["install", "../zips/foo.zip", "-t", "retail"],
            "/intent/refs/0",
            "file:",
        ),
    ] {
        let log = log.case(case);
        let (home, zip) = seeded(&log);
        let root = home.env.root().to_path_buf();
        let (a, b, zips) = (root.join("a"), root.join("b"), root.join("zips"));
        for dir in [&a, &b, &zips] {
            fs::create_dir_all(dir).unwrap();
        }
        let moved = zips.join("foo.zip");
        fs::copy(&zip, &moved).unwrap();

        log.step("preview from a, naming the zip relative to it");
        let mut argv = strings(args);
        argv.push("--dry-run".to_owned());
        let ran = commands::command(&log, &home, &argv, true).cwd(&a).run();
        ran.ok().assert_private();
        let id = ran.data()["plan_id"].as_str().unwrap().to_owned();
        let plan = read_plan(&home.env, &id);
        log.json("plan file", &plan);
        log.assert_eq(
            "the stored path is absolute",
            &plan.pointer(stored).cloned().unwrap_or_default(),
            &json!(format!("{prefix}{}", moved.display())),
        );

        log.step("the same preview typed with the absolute path binds alike");
        let typed = moved.display().to_string();
        let absolute: Vec<String> = argv
            .iter()
            .map(|word| {
                if word == "../zips/foo.zip" {
                    typed.clone()
                } else {
                    word.clone()
                }
            })
            .collect();
        let again = commands::command(&log, &home, &absolute, true)
            .cwd(&b)
            .run();
        again.ok().assert_private();
        log.assert_eq(
            "token",
            &again.data()["precondition"],
            &ran.data()["precondition"],
        );

        log.step("delete a, then apply from b");
        fs::remove_dir_all(&a).unwrap();
        let addons = home.addons("_retail_");
        log.tree("addons", &addons);
        let ran = apply(&log, &home, &id, &[]).cwd(&b).run();
        ran.ok().assert_private();
        log.assert_eq(
            "committed",
            &ran.data()["outcomes"][0]["status"],
            &json!("committed"),
        );
        let diff = log.diff("addons", &addons);
        assert!(
            !diff.added.is_empty() && diff.added.iter().all(|path| path.starts_with("Foo")),
            "only Foo installed: {diff:?}"
        );

        log.step("a missing path with .. still fails naming it");
        let missing: Vec<String> = argv
            .iter()
            .map(|word| {
                if word == "../zips/foo.zip" {
                    "../nowhere/foo.zip".to_owned()
                } else {
                    word.clone()
                }
            })
            .collect();
        let ran = commands::command(&log, &home, &missing, true).cwd(&b).run();
        assert_ne!(ran.run.code, 0, "{}", ran.stdout());
        let message = ran.envelope()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(message.contains("nowhere/foo.zip"), "{message}");
    }
}

#[test]
fn apply_offline_refuses_network_releases() {
    let log = tlog!();
    let (home, zip) = seeded(&log);
    let p = preview(&log, &home, &["install", "gh:o/foo", "-t", "retail"]);
    let trees = Trees::record(&log, &home);

    log.step("apply --offline");
    let ran = apply(&log, &home, &p.id, &["--offline"]).run();
    ran.fails(1, "offline");
    log.assert_eq(
        "fix: the same apply online",
        &words(&ran.envelope()["error"]["fix_argv"]),
        &strings(&["omaddons", "apply", &p.id]),
    );

    log.step("apply under OMADDONS_OFFLINE=1");
    let ran = apply(&log, &home, &p.id, &[])
        .env("OMADDONS_OFFLINE", "1")
        .run();
    ran.fails(1, "offline");
    log.assert_eq(
        "a prose fix",
        &ran.envelope()["error"]["fix_argv"],
        &Value::Null,
    );
    log.step("nothing requested, nothing changed");
    trees.unchanged(&log, &home);

    log.step("a plan of a local zip applies offline");
    let local = preview(&log, &home, &install_zip(&zip));
    apply(&log, &home, &local.id, &["--offline"])
        .run()
        .ok()
        .assert_private();

    log.step("a remove plan applies offline");
    let remove = preview(&log, &home, &["remove", "Foo", "-t", "retail"]);
    let ran = apply(&log, &home, &remove.id, &["--offline"]).run();
    ran.ok().assert_private();
    log.assert_eq(
        "removed",
        &ran.data()["outcomes"][0]["status"],
        &json!("committed"),
    );
}

// ---------------------------------------------------------------------------------------
// Execution.

/// The items' `{name, to_version, status}`, in order.
fn items(data: &Value) -> Vec<Value> {
    data["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| json!({"name": i["name"], "to_version": i["to_version"], "status": i["status"]}))
        .collect()
}

#[test]
fn apply_install_executes_exactly_the_previewed_intent() {
    let log = tlog!();
    let dbm = "## Interface: 120105\n## Version: 12.1.15\n";
    let bigwigs = "## Interface: 120105\n## Version: v400\n";
    let catalog = |home: &Home| {
        crate::common::http::gh_release(
            &home.http,
            "https://github.com/DeadlyBossMods/DeadlyBossMods",
            "12.1.15",
            &[(
                "DBM-Core.zip",
                &[("mainline", 120_105)],
                vec![("DBM-Core/DBM-Core.toc", dbm)],
            )],
        );
        crate::common::http::gh_release(
            &home.http,
            "https://github.com/BigWigsMods/BigWigs",
            "v400",
            &[(
                "BigWigs.zip",
                &[("mainline", 120_105)],
                vec![("BigWigs/BigWigs.toc", bigwigs)],
            )],
        );
    };
    for case in ["a local zip", "two catalog names"] {
        let log = log.case(case);
        let build = |log: &TestLog| {
            let (home, zip) = seeded(log);
            catalog(&home);
            (home, zip)
        };
        let (home, zip) = build(&log);
        let (twin, twin_zip) = build(&log);
        let args = |zip: &str| -> Vec<String> {
            if case == "a local zip" {
                strings(&install_zip(zip))
            } else {
                strings(&["install", "dbm", "bigwigs", "-t", "retail"])
            }
        };
        let mut previewing = args(&zip);
        previewing.extend(strings(&["--actor", "human", "--reason", "first"]));
        let preview = commands::preview(&log, &home, &previewing);
        let p = stored(&log, &home, &preview);

        log.step("apply");
        let ran = apply(&log, &home, &p.id, &["--actor", "agent"]).run();
        ran.ok().assert_private();
        let data = ran.data();
        log.json("outcomes", &data["outcomes"]);
        log.assert_eq(
            "applied command",
            &data["applied"]["command"],
            &json!("install"),
        );
        log.assert_eq(
            "applied display argv",
            &data["applied"]["display_argv"],
            &p.plan["display_argv"],
        );
        log.assert_eq("approved", &data["approved"], &Value::Null);
        log.assert_eq(
            "outcomes",
            &data["outcomes"],
            &json!([{
                "target": "retail",
                "key": data["result"]["target"]["key"],
                "status": "committed",
                "tx": data["result"]["tx"],
                "error_code": null,
            }]),
        );

        log.step("compare with a direct run");
        let direct = commands::run(&log, &twin, &args(&twin_zip));
        log.assert_eq("items", &items(&data["result"]), &items(direct.data()));
        let (applied, directly) = (left(&log, &home), left(&log, &twin));
        log.assert_eq(
            "trees and records",
            &applied.to_string().replace(&zip, "<zip>"),
            &directly.to_string().replace(&twin_zip, "<zip>"),
        );
    }
}

#[test]
fn apply_runs_every_storable_command() {
    let log = tlog!();
    for (path, _) in omaddons::app::intent::StoredIntent::PATHS {
        let path: &str = path;
        let log = log.case(path);
        let applied = commands::home_for(&log, path);
        let twin = commands::home_for(&log, path);

        log.step("preview");
        let previewed = commands::preview(&log, &applied.home, &applied.args);
        let p = stored(&log, &applied.home, &previewed);
        log.step("apply");
        let ran = apply(&log, &applied.home, &p.id, &[]).run();
        ran.ok().assert_private();
        if path == "backup restore" {
            // A WTF/ of FAKEACCT on FakeRealm as FakeChar, with --progress json.
            for run in [&previewed, &ran] {
                crate::common::privacy::assert_private_all(run.stdout(), run.stderr());
            }
        }
        log.assert_eq(
            "applied command",
            &ran.data()["applied"]["command"],
            &json!(path),
        );
        log.json("outcomes", &ran.data()["outcomes"]);
        let client = if path == "copy" { "classic" } else { "retail" };
        log.assert_eq(
            "outcomes",
            &outcomes(&ran),
            &vec![(client.to_owned(), "committed".to_owned())],
        );
        if path == "backup prune" {
            log.assert_eq(
                "a prune commits no tx",
                &ran.data()["outcomes"][0]["tx"],
                &Value::Null,
            );
        }

        log.step("compare with a direct run");
        commands::run(&log, &twin.home, &twin.args);
        log.assert_eq(
            "trees, records and backups",
            &left(&log, &applied.home),
            &left(&log, &twin.home),
        );
    }
}

#[test]
fn apply_multi_target_update_reports_each_target() {
    let log = tlog!();
    {
        let log = log.case("both clients behind");
        let home = behind(&log, &["retail", "classic"]);
        log.step("preview update");
        let p = stored(
            &log,
            &home,
            &real(&log, &home, &["update", "--dry-run"]).run(),
        );
        log.step("apply");
        let ran = real(&log, &home, &["apply", &p.id]).run();
        ran.ok().assert_private();
        let data = ran.data();
        log.json("outcomes", &data["outcomes"]);
        log.assert_eq(
            "one outcome per client, sorted",
            &outcomes(&ran),
            &vec![
                ("classic".to_owned(), "committed".to_owned()),
                ("retail".to_owned(), "committed".to_owned()),
            ],
        );
        let txs: Vec<&Value> = data["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| &o["tx"])
            .collect();
        assert!(
            txs.iter().all(|tx| tx.is_string()) && txs[0] != txs[1],
            "{txs:?}"
        );
        assert!(data["result"]["run"].is_string(), "{data}");

        log.step("history");
        let history = real(&log, &home, &["history"]).run();
        let lines: Vec<&Value> = history.data()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["plan"] == json!(p.id))
            .collect();
        log.json("history lines", &lines);
        log.assert_eq("two lines carry the plan", &lines.len(), &2);
        for line in &lines {
            log.assert_eq("their run", &line["run"], &data["result"]["run"]);
        }
    }
    {
        let log = log.case("one client current");
        let home = behind(&log, &["retail"]);
        log.step("install Foo v2 on classic");
        real(&log, &home, &["install", "gh:o/foo", "-t", "classic"])
            .run()
            .ok();
        log.step("preview update Foo");
        let p = stored(
            &log,
            &home,
            &real(&log, &home, &["update", "Foo", "--dry-run"]).run(),
        );
        log.step("apply");
        let ran = real(&log, &home, &["apply", &p.id]).run();
        ran.ok().assert_private();
        log.json("outcomes", &ran.data()["outcomes"]);
        log.assert_eq(
            "retail updated, classic unchanged",
            &outcomes(&ran),
            &vec![
                ("classic".to_owned(), "unchanged".to_owned()),
                ("retail".to_owned(), "committed".to_owned()),
            ],
        );
        log.assert_eq(
            "unchanged has no tx",
            &ran.data()["outcomes"][0]["tx"],
            &Value::Null,
        );
    }
}

#[test]
fn apply_partial_run_is_reported_per_target() {
    let log = tlog!();
    let home = behind(&log, &["retail", "classic"]);
    log.step("preview update");
    let p = stored(
        &log,
        &home,
        &real(&log, &home, &["update", "--dry-run"]).run(),
    );
    let addons = |id: &str| {
        home.addons(if id == "retail" {
            "_retail_"
        } else {
            "_classic_"
        })
    };
    for id in ["classic", "retail"] {
        log.tree(&format!("addons-{id}"), &addons(id));
    }

    log.step("apply, the second client's commit failing");
    let ran = real(&log, &home, &["apply", &p.id])
        .failpoints("engine.revalidated=error@2")
        .run();
    ran.ok().assert_private();
    log.json("outcomes", &ran.data()["outcomes"]);
    log.assert_eq("partial", &ran.envelope()["partial"], &json!(true));
    // Outcomes are sorted by client id; the commits ran in their own order, and the
    // second one failed.
    let all = ran.data()["outcomes"].as_array().unwrap().clone();
    let with = |status: &str| -> Vec<Value> {
        all.iter()
            .filter(|o| o["status"] == json!(status))
            .cloned()
            .collect()
    };
    let (committed, failed) = (with("committed"), with("failed"));
    log.assert_eq(
        "one client committed, one failed",
        &(committed.len(), failed.len()),
        &(1, 1),
    );
    let first = committed[0]["tx"].clone();
    assert!(first.is_string(), "{committed:?}");
    log.assert_eq("the failure's code", &failed[0]["error_code"], &json!("io"));
    log.assert_eq("the failure's tx", &failed[0]["tx"], &Value::Null);
    let failed_id = failed[0]["target"].as_str().unwrap().to_owned();
    log.assert_unchanged(&format!("addons-{failed_id}"), &addons(&failed_id));

    log.step("second apply: a partial first application is never resumed");
    let ran = real(&log, &home, &["apply", &p.id]).run();
    let details = refused(&log, &ran, &p, "already_applied");
    log.assert_eq("only the committed tx", &details["txs"], &json!([first]));
    log.assert_unchanged(&format!("addons-{failed_id}"), &addons(&failed_id));
}

#[test]
fn apply_history_records_the_plan_and_the_applier() {
    let log = tlog!();
    let (home, zip) = seeded(&log);
    let mut args = install_zip(&zip).to_vec();
    args.extend(["--actor", "human", "--reason", "boss mods"]);
    let p = preview(&log, &home, &args);

    log.step("apply as an agent, without a reason");
    apply(&log, &home, &p.id, &["--actor", "agent"])
        .run()
        .ok()
        .assert_private();
    let doorbell: Value =
        serde_json::from_slice(&fs::read(home.env.cache().join("omaddons/doorbell.json")).unwrap())
            .unwrap();
    log.json("doorbell", &doorbell);
    log.assert_eq(
        "the doorbell names apply",
        &doorbell["command"],
        &json!("apply"),
    );
    let newest = |log: &TestLog| -> Value {
        let history = commands::run(log, &home, &strings(&["history"]));
        history.data()["entries"][0].clone()
    };
    let line = newest(&log);
    log.json("history line", &line);
    log.assert_eq("argv", &line["argv"], &p.plan["display_argv"]);
    log.assert_eq("plan", &line["plan"], &json!(p.id));
    log.assert_eq("actor", &line["actor"], &json!("agent"));
    log.assert_eq("the preview's reason", &line["reason"], &json!("boss mods"));
    log.assert_eq(
        "the plan keeps the previewer",
        &read_plan(&home.env, &p.id)["actor"],
        &json!("human"),
    );

    log.step("a second plan applied with its own reason");
    let removing = preview(&log, &home, &["remove", "Foo", "-t", "retail"]);
    apply(&log, &home, &removing.id, &["--reason", "now"])
        .run()
        .ok()
        .assert_private();
    let line = newest(&log);
    log.json("history line", &line);
    log.assert_eq("apply's reason", &line["reason"], &json!("now"));
    log.assert_eq("plan", &line["plan"], &json!(removing.id));

    log.step("a direct install records no plan");
    commands::run(&log, &home, &strings(&install_zip(&zip)));
    let line = newest(&log);
    log.json("history line", &line);
    log.assert_eq("no plan", &line["plan"], &Value::Null);

    log.step("history -v names the plans");
    let human = commands::command(&log, &home, &strings(&["history", "-v"]), false).run();
    human.ok();
    for id in [&p.id, &removing.id] {
        assert!(
            human.stdout().contains(&format!("plan {}", &id[..16])),
            "history -v names plan {id}:\n{}",
            human.stdout()
        );
    }
}

// ---------------------------------------------------------------------------------------
// The dry run.

#[test]
fn apply_dry_run_writes_nothing_and_takes_no_lock() {
    let log = tlog!();
    let (home, zip) = seeded(&log);
    let p = preview(&log, &home, &install_zip(&zip));

    log.step("hold the global lock and the plan's lease");
    let paths = crate::common::http::paths(&home.env);
    let _global = omaddons::fsx::GlobalLock::acquire(&paths, "test").unwrap();
    let id: omaddons::model::PlanId = p.id.parse().unwrap();
    let scopes = omaddons::fsx::ScopeTable::new(&paths, &[], None).unwrap();
    let lease = omaddons::fsx::Lease::acquire(
        &scopes,
        &paths.plan_lease(&id),
        "test",
        Duration::from_secs(1),
    );
    let _lease = lease.map_err(|_| "the plan's lease").unwrap();
    let trees = Trees::record(&log, &home);

    log.step("apply --dry-run");
    let started = Instant::now();
    let ran = apply(&log, &home, &p.id, &["--dry-run"])
        .timeout(Duration::from_secs(20))
        .run();
    let took = started.elapsed();
    ran.ok().assert_private();
    assert!(
        took < Duration::from_secs(3),
        "took {took:?} under the locks"
    );
    let data = ran.data();
    log.assert_eq("a dry run", &data["result"]["dry_run"], &json!(true));
    log.assert_eq("approved", &data["approved"], &p.plan["approval"]);
    assert!(data["result"]["approval"].is_object(), "{data}");
    log.assert_eq(
        "no plan of its own",
        &data["result"]["plan_id"],
        &Value::Null,
    );
    for outcome in data["outcomes"].as_array().unwrap() {
        log.assert_eq("unchanged", &outcome["status"], &json!("unchanged"));
        log.assert_eq("no tx", &outcome["tx"], &Value::Null);
    }
    let hint = strings(&["omaddons", "apply", &p.id]);
    assert!(
        ran.commands().iter().any(|c| c.argv == hint),
        "no apply hint in {:?}",
        ran.commands()
    );
    trees.unchanged(&log, &home);

    log.step("the human form names the plan");
    let human = commands::command(&log, &home, &strings(&["apply", &p.id, "--dry-run"]), false)
        .timeout(Duration::from_secs(20))
        .run();
    human.ok();
    log.assert_eq(
        "last line",
        &human.stdout().lines().last().unwrap_or_default().to_owned(),
        &format!(
            "Plan {}… still applies: omaddons apply {}",
            &p.id[..16],
            p.id
        ),
    );
    trees.unchanged(&log, &home);
}

#[test]
fn apply_dry_run_reports_staleness() {
    let log = tlog!();
    {
        let log = log.case("engine_command");
        let (home, zip) = seeded(&log);
        let p = preview(&log, &home, &install_zip(&zip));
        log.step("another install moves retail's records");
        let bar = commands::zip(&home, "Bar", "1").display().to_string();
        commands::run(
            &log,
            &home,
            &strings(&["install", "gh:o/bar", "--file", &bar, "-t", "retail"]),
        );
        let trees = Trees::record(&log, &home);
        log.step("apply --dry-run");
        let ran = apply(&log, &home, &p.id, &["--dry-run"]).run();
        ran.fails(1, "plan_stale").assert_private();
        let changed = &ran.envelope()["error"]["details"]["changed"];
        assert!(
            changed
                .as_array()
                .is_some_and(|parts| parts.contains(&json!("targets"))),
            "{changed}"
        );
        trees.unchanged(&log, &home);
    }
    {
        let log = log.case("m3_command");
        let (home, zip) = seeded(&log);
        commands::run(&log, &home, &strings(&install_zip(&zip)));
        let p = preview(&log, &home, &["remove", "Foo", "-t", "retail"]);
        log.step("Foo reinstalled from another archive");
        let v2 = commands::zip(&home, "Foo", "2").display().to_string();
        commands::run(&log, &home, &strings(&install_zip(&v2)));
        let trees = Trees::record(&log, &home);
        log.step("apply --dry-run");
        let ran = apply(&log, &home, &p.id, &["--dry-run"]).run();
        ran.fails(1, "plan_stale").assert_private();
        log.json("changed", &ran.envelope()["error"]["details"]["changed"]);
        trees.unchanged(&log, &home);
        log.step("apply");
        let ran = apply(&log, &home, &p.id, &[]).run();
        ran.fails(1, "plan_stale").assert_private();
        trees.unchanged(&log, &home);
    }
}

// ---------------------------------------------------------------------------------------
// Revalidation and side events.

#[test]
fn apply_force_revalidates_inside_the_memo_window() {
    let log = tlog!();
    let home = behind(&log, &["retail"]);
    log.step("preview update: the memo is fresh from here on");
    let p = stored(
        &log,
        &home,
        &real(&log, &home, &["update", "--dry-run"]).run(),
    );

    log.step("publish v3");
    let base = "https://github.com/o/foo";
    crate::common::http::gh_release_v(&home.http, base, "v3", &foo_release(3));
    crate::common::http::gh_latest(&home.http, base, "v3");
    let memo = || fs::read_to_string(home.memo()).unwrap_or_default();
    assert!(!memo().contains("\"v3\""), "the memo knew v3 already");

    log.step("a direct preview inside the window asks nothing");
    let probes = net_log(&home).count_purpose("release-latest");
    let direct = real(&log, &home, &["update", "--dry-run"]).run();
    direct.ok().assert_private();
    let retail = direct.data()["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["target"]["id"] == json!("retail"))
        .cloned()
        .unwrap_or_default();
    log.assert_eq(
        "the memo's release",
        &retail["items"][0]["to_version"],
        &json!("v2"),
    );
    log.assert_eq(
        "no probe",
        &net_log(&home).count_purpose("release-latest"),
        &probes,
    );

    log.step("apply probes the source and is stale");
    let addons = home.addons("_retail_");
    log.tree("addons", &addons);
    let ran = real(&log, &home, &["apply", &p.id]).run();
    ran.fails(1, "plan_stale").assert_private();
    let changed = &ran.envelope()["error"]["details"]["changed"];
    assert!(
        changed
            .as_array()
            .is_some_and(|parts| parts.contains(&json!("selection"))),
        "{changed}"
    );
    assert!(
        net_log(&home).count_purpose("release-latest") > probes,
        "apply asked no source"
    );
    assert!(memo().contains("\"v3\""), "the memo does not know v3");
    log.assert_unchanged("addons", &addons);
}

/// A stderr `log` event of level `debug`, with ids, numbers and the home's root
/// replaced, so two homes' runs compare.
fn debug_lines(ran: &Ran, root: &str) -> Vec<String> {
    ran.events()
        .iter()
        .filter(|e| e["event"] == json!("log") && e["level"] == json!("debug"))
        .filter_map(|e| e["message"].as_str())
        .map(|message| {
            let text = redact_ulids(&message.replace(root, "<root>"));
            let mut out = String::new();
            let mut digits = false;
            for c in text.chars() {
                if c.is_ascii_digit() {
                    if !digits {
                        out.push('#');
                    }
                    digits = true;
                } else {
                    out.push(c);
                    digits = false;
                }
            }
            out
        })
        .collect()
}

#[test]
fn apply_side_events_match_the_direct_command() {
    let log = tlog!();
    let (direct_home, direct_zip) = seeded(&log);
    let (home, zip) = seeded(&log);

    // `apply` is a bound run, so its twin is the install bound to its own preview: both
    // check the approval again under the lock, and `-v` says so.
    log.step("install directly, bound to its preview, verbose");
    let token = commands::preview(&log, &direct_home, &strings(&install_zip(&direct_zip)))
        .data()["precondition"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let mut args = strings(&install_zip(&direct_zip));
    args.extend(strings(&["--if-match", &token, "-v"]));
    let direct = commands::run(&log, &direct_home, &args);

    log.step("preview, then apply verbose");
    let p = preview(&log, &home, &install_zip(&zip));
    let ran = apply(&log, &home, &p.id, &["-v"]).run();
    ran.ok().assert_private();
    log.assert_eq("command", &ran.envelope()["command"], &json!("apply"));
    let undo = strings(&[
        "omaddons",
        "undo",
        ran.data()["result"]["tx"].as_str().unwrap_or_default(),
    ]);
    assert!(
        ran.commands().iter().any(|c| c.argv == undo),
        "no undo hint in {:?}",
        ran.commands()
    );
    let (applied, directly) = (
        debug_lines(&ran, &home.env.root().display().to_string()),
        debug_lines(&direct, &direct_home.env.root().display().to_string()),
    );
    log.json("debug lines (apply)", &applied);
    log.assert_eq("debug lines, in order", &applied, &directly);
}
