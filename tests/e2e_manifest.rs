//! e2e suite: manifest
//!
//! The semantic manifest checked against the live binary: every flag and positional `help
//! --json` reports is an input node with the same facts, and every key path of real
//! envelopes, their data, error details and stderr events is located in the outputs. The
//! unit tests prove the normalizer on synthetic schemas; this proves it on what the binary
//! prints.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::env::TestEnv;
use common::home::Home;
use common::tlog::{Ran, TestLog, tlog};
use omaddons::app::{COMMANDS, declared_exits};
use omaddons::contract::Registry;
use omaddons::contract::manifest::{Fact, Manifest, Node, build, fact, section_counts, unmatched};
use serde_json::{Value, json};

/// The most of an instance a failure message prints.
const EXCERPT: usize = 2 << 10;

fn manifest() -> Manifest {
    build(&Registry::builtin())
}

fn get<'m>(m: &'m Manifest, id: &str) -> Option<&'m Node> {
    m.nodes.get(id)
}

fn fact_of<'m>(m: &'m Manifest, id: &str, name: &str) -> Option<&'m Fact> {
    get(m, id).and_then(|node| node.facts.get(name))
}

fn set_of(fact: Option<&Fact>) -> BTreeSet<String> {
    match fact {
        Some(Fact::List(items)) => items.iter().cloned().collect(),
        _ => BTreeSet::new(),
    }
}

/// Whether help's `takes_value` agrees with the node's grammar.
fn takes_value(m: &Manifest, id: &str) -> bool {
    !matches!(fact_of(m, id, fact::GRAMMAR), Some(Fact::Text(g)) if g == "flag" || g == "count")
}

/// `path`, then each of its ancestors, then the root: where a propagated global is declared.
fn ancestors(path: &str) -> Vec<String> {
    let words: Vec<&str> = path.split(' ').collect();
    let mut out: Vec<String> = (1..=words.len())
        .rev()
        .map(|n| words[..n].join(" "))
        .collect();
    out.push("omaddons".to_owned());
    out
}

#[test]
fn help_json_agrees_with_the_manifest_inputs() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("manifest");
    let m = manifest();
    let counts = section_counts(&m);
    log.json(
        "input_counts",
        &json!({"commands": counts.get("commands"), "flags": counts.get("flags"), "positionals": counts.get("positionals")}),
    );

    log.step("bare help");
    let ran = log.run(&env, &["help", "--json"]);
    ran.ok();
    let data = ran.data().clone();
    let mut mismatched = Vec::new();
    for flag in data["global_flags"].as_array().into_iter().flatten() {
        let name = flag["name"].as_str().unwrap_or("");
        let id = format!("input omaddons --{name}");
        if get(&m, &id).is_none() {
            mismatched.push(json!({"arg": id, "fact": "node", "help": true, "manifest": false}));
            continue;
        }
        let global = fact_of(&m, &id, fact::GLOBAL) == Some(&Fact::Bool(true));
        if global != (name != "version") {
            mismatched.push(
                json!({"arg": id, "fact": "global", "help": name != "version", "manifest": global}),
            );
        }
        let short = flag["short"].as_str().map(|s| format!("-{s}"));
        let node_short = match fact_of(&m, &id, fact::SHORT) {
            Some(Fact::Text(s)) => Some(s.clone()),
            _ => None,
        };
        if short != node_short {
            mismatched
                .push(json!({"arg": id, "fact": "short", "help": short, "manifest": node_short}));
        }
        if flag["takes_value"].as_bool() != Some(takes_value(&m, &id)) {
            mismatched.push(json!({"arg": id, "fact": "takes_value", "help": flag["takes_value"], "manifest": takes_value(&m, &id)}));
        }
    }
    for command in data["commands"].as_array().into_iter().flatten() {
        let id = format!("command {}", command["name"].as_str().unwrap_or(""));
        let help: BTreeSet<String> = command["aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        if get(&m, &id).is_none() || set_of(fact_of(&m, &id, fact::ALIASES)) != help {
            mismatched.push(json!({"arg": id, "fact": "aliases", "help": help, "manifest": set_of(fact_of(&m, &id, fact::ALIASES))}));
        }
    }
    log.json(
        "help_vs_manifest",
        &json!({"path": "omaddons", "mismatched": mismatched}),
    );
    assert!(
        mismatched.is_empty(),
        "bare help disagrees with the manifest: {mismatched:#?}"
    );

    let (mut flags_checked, mut positionals_checked) = (0, 0);
    for row in COMMANDS {
        let path = row.name;
        log.step(&format!("help {path}"));
        let mut argv = vec!["help"];
        argv.extend(path.split(' '));
        argv.push("--json");
        let ran = log.run(&env, &argv);
        ran.ok();
        let data = ran.data();
        let mut in_help_only = Vec::new();
        let mut in_manifest_only = Vec::new();
        let mut mismatched = Vec::new();
        let help_flags: BTreeMap<String, &Value> = data["flags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| Some((f["name"].as_str()?.to_owned(), f)))
            .collect();
        for (name, flag) in &help_flags {
            // Declared here, or a global propagated from where it is declared.
            let own = format!("input {path} --{name}");
            let id = if get(&m, &own).is_some() {
                Some(own)
            } else {
                ancestors(path)
                    .into_iter()
                    .skip(1)
                    .map(|at| format!("input {at} --{name}"))
                    .find(|id| fact_of(&m, id, fact::GLOBAL) == Some(&Fact::Bool(true)))
            };
            let Some(id) = id else {
                in_help_only.push(format!("--{name}"));
                continue;
            };
            flags_checked += 1;
            let short = flag["short"].as_str().map(|s| format!("-{s}"));
            let node_short = match fact_of(&m, &id, fact::SHORT) {
                Some(Fact::Text(s)) => Some(s.clone()),
                _ => None,
            };
            if short != node_short {
                mismatched.push(
                    json!({"arg": id, "fact": "short", "help": short, "manifest": node_short}),
                );
            }
            if flag["takes_value"].as_bool() != Some(takes_value(&m, &id)) {
                mismatched.push(json!({"arg": id, "fact": "takes_value", "help": flag["takes_value"], "manifest": takes_value(&m, &id)}));
            }
        }
        let help_args: Vec<&Value> = data["args"].as_array().into_iter().flatten().collect();
        for (i, arg) in help_args.iter().enumerate() {
            let id = format!("input {path} @{}", i + 1);
            if get(&m, &id).is_none() {
                in_help_only.push(format!("@{}", i + 1));
                continue;
            }
            positionals_checked += 1;
            let required = fact_of(&m, &id, fact::REQUIRED) == Some(&Fact::Bool(true));
            if arg["required"].as_bool() != Some(required) {
                mismatched.push(json!({"arg": id, "fact": "required", "help": arg["required"], "manifest": required}));
            }
        }
        let prefix = format!("input {path} ");
        for id in m.nodes.keys().filter(|id| id.starts_with(&prefix)) {
            let identity = &id[prefix.len()..];
            if identity.contains(' ') {
                continue;
            }
            let listed = match identity.strip_prefix("--") {
                Some(long) => help_flags.contains_key(long),
                None => identity
                    .strip_prefix('@')
                    .and_then(|n| n.parse::<usize>().ok())
                    .is_some_and(|n| n >= 1 && n <= help_args.len()),
            };
            if !listed {
                in_manifest_only.push(identity.to_owned());
            }
        }
        log.json(
            "help_vs_manifest",
            &json!({"path": path, "in_help_only": in_help_only, "in_manifest_only": in_manifest_only, "mismatched": mismatched}),
        );
        assert!(
            in_help_only.is_empty() && in_manifest_only.is_empty() && mismatched.is_empty(),
            "help {path}: in help only {in_help_only:?}, in the manifest only {in_manifest_only:?}, mismatched {mismatched:#?}"
        );
    }
    log.json("checked", &json!({"paths": COMMANDS.len(), "flags": flags_checked, "positionals": positionals_checked}));
}

/// Every unmatched path of `ran`'s envelope, data and stderr events.
fn check(log: &TestLog, m: &Manifest, step: &str, ran: &Ran) {
    let envelope = ran.envelope();
    let mut anchors: Vec<(String, Value)> = vec![("envelope .".to_owned(), envelope.clone())];
    if let (Some(command), Some(data)) = (envelope["command"].as_str(), envelope.get("data"))
        && !data.is_null()
    {
        anchors.push((format!("output {command} data"), data.clone()));
    }
    for event in ran.events() {
        if let Some(name) = event["event"].as_str() {
            anchors.push((format!("event {name} ."), event.clone()));
        }
    }
    for (anchor, instance) in &anchors {
        let paths = unmatched(m, anchor, instance);
        log.json(
            "unmatched",
            &json!({"step": step, "anchor": anchor, "paths": paths}),
        );
        if !paths.is_empty() {
            let excerpt: String = instance.to_string().chars().take(EXCERPT).collect();
            let failure =
                format!("{step}: {anchor} has unmatched paths {paths:#?}\n  instance: {excerpt}");
            log.note(&failure);
            panic!("{failure}"); // ubs:ignore — the scenario's failure, after logging it
        }
    }
    ran.assert_private();
    let command = envelope["command"].as_str().unwrap_or("");
    let exit = u8::try_from(ran.run.code).unwrap_or(u8::MAX);
    let declared = declared_exits(command);
    log.assert_eq(
        "the exit is declared",
        &declared.is_some_and(|exits| exits.contains(&exit)),
        &true,
    );
}

#[test]
fn live_envelopes_are_in_the_manifest() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic"]);
    let m = manifest();
    let run = |argv: &[&str]| {
        let mut full = argv.to_vec();
        full.extend(["--json", "--offline", "--progress", "json"]);
        log.run(&home, &full)
    };
    let mut events = 0;
    let steps: Vec<(&str, Vec<&str>)> = vec![
        ("targets", vec!["targets"]),
        ("status", vec!["status"]),
        ("list", vec!["list"]),
        ("history", vec!["history"]),
        ("config", vec!["config"]),
        ("config path", vec!["config", "path"]),
        ("net-log", vec!["net-log"]),
        ("backup list", vec!["backup", "list"]),
        ("errors", vec!["errors"]),
        ("help", vec!["help"]),
        ("help install", vec!["help", "install"]),
        ("version", vec!["-V"]),
        ("unknown flag", vec!["--frobnicate"]),
        ("targets rm unknown", vec!["targets", "rm", "nosuch"]),
        (
            "install on unknown target",
            vec!["install", "x", "-t", "nosuch"],
        ),
        ("info unknown", vec!["info", "nosuchaddon"]),
        ("undo unknown", vec!["undo", "01ZZZZZZZZZZZZZZZZZZZZZZZZ"]),
    ];
    let step_count = steps.len();
    for (step, argv) in steps {
        log.step(step);
        let ran = run(&argv);
        let envelope = ran.envelope().clone();
        match step {
            "targets" | "list" | "history" | "config" | "config path" | "net-log"
            | "backup list" | "help" | "help install" => {
                ran.ok();
            }
            "version" => {
                ran.ok();
                log.assert_eq("command", &envelope["command"], &json!("version"));
            }
            "unknown flag" => {
                ran.fails(2, "usage");
                log.assert_eq("command", &envelope["command"], &json!("omaddons"));
            }
            "targets rm unknown" => {
                ran.fails(3, "target_not_found");
            }
            "install on unknown target" => {
                // The ref `x` is resolved before the target: no catalog entry is `x`.
                ran.fails(3, "not_found");
            }
            "info unknown" | "undo unknown" => {
                log.assert_eq("ok", &envelope["ok"], &json!(false));
            }
            _ => {
                log.assert_eq(
                    "ok matches the exit",
                    &envelope["ok"],
                    &json!(ran.run.code == 0),
                );
            }
        }
        events += ran.events().len();
        check(&log, &m, step, &ran);
    }
    log.json(
        "located",
        &json!({"steps": step_count, "envelopes": step_count, "events": events}),
    );
}
