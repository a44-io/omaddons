//! `apply` end to end (D5's suite): what a stored plan refuses and why, how `apply` finds
//! it, and what it runs, through the real binary with every invocation logged.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use jiff::{SignedDuration, Timestamp};
use omaddons::limits::PLAN_MAX_BYTES;
use serde_json::{Value, json};

use crate::commands::{self, strings};
use crate::common::home::Home;
use crate::common::plans::{edit_plan, lease_files, plans_dir, read_plan};
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

/// `args` as a dry run that must store a plan; logs the plan file.
fn preview(log: &TestLog, home: &Home, args: &[&str]) -> Preview {
    log.step(&format!("preview {}", args.join(" ")));
    let ran = commands::preview(log, home, &strings(args));
    let data = ran.data();
    let id = data["plan_id"].as_str().unwrap_or_default().to_owned();
    assert!(!id.is_empty(), "{args:?} stored no plan: {data}");
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
