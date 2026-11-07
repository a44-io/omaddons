//! The declared registries against the real binary: live stderr and schemas,
//! selector forms, source hosts and environment variables that mirror flags.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;

use omaddons::contract::events::{EventName, LogLevel, Phase};
use omaddons::contract::{env, target_id_forms};
use omaddons::net::allowlist::hosts_for;
use omaddons::sources::SourceId;
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

use super::{Scene, common, support};
use common::archives::zip_text;
use common::netlog::net_log;
use common::tlog::{Ran, TestLog, tlog};
use common::tukui::{elvui, tukui_list};
use common::wowi::{details, wowi_details, wowi_file};

fn checked(ran: Ran, exit: i32) -> Ran {
    support::check(ran.to_run(), exit);
    ran
}

fn missing_probe(message: String) -> ! {
    panic!("{message}"); // ubs:ignore — intentional test-only failure for a newly registered case with no probe
}

fn values(events: &[Value], key: &str) -> BTreeSet<String> {
    events
        .iter()
        .filter_map(|event| event[key].as_str().map(str::to_owned))
        .collect()
}

fn event_keys_fit_schema(log: &TestLog, event: &Value) {
    let name = EventName::ALL
        .iter()
        .copied()
        .find(|name| event["event"] == name.wire());
    assert!(name.is_some(), "unregistered event: {event}");
    let name = name.unwrap();
    let mut generator = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();
    let schema = serde_json::to_value(name.schema(&mut generator)).unwrap();
    let keys: BTreeSet<_> = event.as_object().unwrap().keys().cloned().collect();
    let properties: BTreeSet<_> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let required: BTreeSet<_> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key.as_str().unwrap().to_owned())
        .collect();
    log.json(
        "event schema keys",
        &json!({"event":event, "properties":properties, "required":required}),
    );
    let extra: Vec<_> = keys.difference(&properties).collect();
    let missing: Vec<_> = required.difference(&keys).collect();
    assert!(
        extra.is_empty() && missing.is_empty(),
        "{name:?}: event {event}; extra keys {extra:?}, missing keys {missing:?}"
    );
}

#[test]
fn live_stderr_events_are_the_registered_events() {
    let log = tlog!();
    let s = Scene::new();
    log.step("install with progress");
    let mut events = checked(
        log.run(
            &s.home,
            &[
                "install",
                "weakauras",
                "-t",
                "retail",
                "--json",
                "--progress",
                "json",
                "-v",
            ],
        ),
        0,
    )
    .events();

    log.step("back up a played WTF");
    common::wtf::Wtf::at(s.home.client("_retail_"))
        .saved_variables("Foo", 100)
        .build();
    let backup = checked(
        log.run(
            &s.home,
            &[
                "backup",
                "create",
                "-t",
                "retail",
                "--json",
                "--progress",
                "json",
            ],
        ),
        0,
    );
    backup.ok().assert_private();
    events.extend(backup.events());

    log.step("a warning");
    checked(
        log.run(&s.home, &["pin", "WeakAuras", "-t", "retail", "--json"]),
        0,
    )
    .ok();
    let pinned = checked(
        log.run(&s.home, &["install", "weakauras", "-t", "retail", "--json"]),
        0,
    );
    assert!(
        pinned.warnings().iter().any(|code| code == "addon_pinned"),
        "the pinned install emits its warning"
    );
    events.extend(pinned.events());

    log.step("an info log");
    OpenOptions::new()
        .append(true)
        .open(s.home.state().join("history.jsonl"))
        .unwrap()
        .write_all(b"{\"tx\":")
        .unwrap();
    events.extend(checked(log.run(&s.home, &["history", "--json"]), 0).events());

    log.step("a trace log");
    events.extend(checked(log.run(&s.home, &["status", "-vv", "--json"]), 0).events());

    log.step("an error");
    let failed = checked(log.run(&s.home, &["status", "-t", "nosuch", "--json"]), 3);
    failed.fails(3, "target_not_found");
    events.extend(failed.events());

    log.step("compare registry coverage and event schemas");
    let event_names = values(&events, "event");
    let phases = values(&events, "phase");
    let levels = values(&events, "level");
    let expected_names = EventName::ALL
        .iter()
        .map(|name| name.wire().to_owned())
        .collect();
    let expected_phases = Phase::ALL
        .iter()
        .map(|phase| phase.wire().to_owned())
        .collect();
    let expected_levels = LogLevel::ALL
        .iter()
        .map(|level| level.wire().to_owned())
        .collect();
    log.json("observed events", &event_names);
    log.json("registered events", &expected_names);
    log.assert_eq("event coverage", &event_names, &expected_names);
    log.json("observed phases", &phases);
    log.json("registered phases", &expected_phases);
    log.assert_eq("phase coverage", &phases, &expected_phases);
    log.json("observed levels", &levels);
    log.json("registered levels", &expected_levels);
    log.assert_eq("level coverage", &levels, &expected_levels);
    for event in events {
        event_keys_fit_schema(&log, &event);
    }
}

#[test]
fn every_target_id_form_selects_its_client() {
    let log = tlog!();
    let s = Scene::new();
    log.step("register");
    let client = s.home.client("_retail_").display().to_string();
    checked(
        log.run(&s.home, &["targets", "add", client.as_str(), "--json"]),
        0,
    )
    .ok();
    checked(log.run(&s.home, &["status", "--json"]), 0).ok();
    let targets = checked(log.run(&s.home, &["targets", "--json"]), 0);
    let key = targets.data()["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "retail")
        .unwrap()["key"]
        .as_str()
        .unwrap()
        .to_owned();
    for form in target_id_forms() {
        log.step(&format!("select by {form}"));
        let (selector, expected) = match form {
            "key" => (key.clone(), vec!["retail".to_owned()]),
            "id" => ("retail".into(), vec!["retail".to_owned()]),
            "product_code" => ("wow".into(), vec!["retail".to_owned()]),
            "flavor_folder" => ("_retail_".into(), vec!["retail".to_owned()]),
            "game_mode" => ("mop".into(), vec!["classic".to_owned()]),
            "family" => ("mainline".into(), vec!["retail".to_owned()]),
            "path" => (client.clone(), vec!["retail".to_owned()]),
            "all" => (
                "all".into(),
                vec!["classic".to_owned(), "retail".to_owned()],
            ),
            other => missing_probe(format!(
                "no probe for the target-id form {other}: add one to every_target_id_form_selects_its_client"
            )),
        };
        let ran = checked(
            log.run(&s.home, &["status", "-t", selector.as_str(), "--json"]),
            0,
        );
        let ids: Vec<_> = ran.data()["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_owned())
            .collect();
        log.json("selector", &selector);
        log.json("selected", &ids);
        log.json("expected", &expected);
        log.assert_eq("selected clients", &ids, &expected);
    }
}

fn source_hosts(log: &TestLog, s: &Scene, source: SourceId, before: usize) {
    let requests = net_log(&s.home);
    let added = &requests.entries[before..];
    assert!(!added.is_empty(), "{source:?} made no fixture request");
    let hosts: BTreeSet<_> = added.iter().map(|entry| entry.host.clone()).collect();
    let allowed: BTreeSet<_> = hosts_for(source)
        .iter()
        .map(|host| (*host).to_owned())
        .collect();
    log.json("requests", added);
    log.json("hosts", &hosts);
    log.json("allowed hosts", &allowed);
    assert!(
        hosts.is_subset(&allowed),
        "{source:?}: request outside its hosts:\n{}",
        requests.context()
    );
}

#[test]
fn each_source_reaches_only_its_hosts() {
    let log = tlog!();
    let s = Scene::new();
    log.step("github");
    let before = net_log(&s.home).len();
    checked(
        log.run(&s.home, &["install", "weakauras", "-t", "retail", "--json"]),
        0,
    )
    .ok();
    source_hosts(&log, &s, SourceId::Github, before);

    log.step("tukui");
    tukui_list(&s.home.http, &[elvui()]);
    let before = net_log(&s.home).len();
    checked(log.run(&s.home, &["info", "tukui:elvui", "--json"]), 0).ok();
    source_hosts(&log, &s, SourceId::Tukui, before);

    log.step("wowinterface");
    let zip = zip_text(&[(
        "BugSack/BugSack.toc",
        "## Interface: 120105\n## Version: 1.0\n",
    )]);
    let md5 = wowi_file(&s.home.http, 5995, &zip);
    wowi_details(
        &s.home.http,
        &[5995],
        &[details(5995, "BugSack", "1.0", &md5)],
    );
    let before = net_log(&s.home).len();
    checked(
        log.run(&s.home, &["install", "wowi:5995", "-t", "retail", "--json"]),
        0,
    )
    .ok();
    source_hosts(&log, &s, SourceId::Wowi, before);
}

#[test]
fn each_mirrored_variable_acts_like_its_flag() {
    let log = tlog!();
    let s = Scene::new();
    for row in env::release().filter(|row| row.mirrors.is_some()) {
        log.step(&format!(
            "probe {} against {}",
            row.name,
            row.mirrors.unwrap()
        ));
        match row.name {
            "OMADDONS_OFFLINE" => {
                let baseline = checked(log.run(&s.home, &["status", "--json"]), 0);
                let variable = checked(
                    log.cmd(&s.home, &["status", "--json"])
                        .env(row.name, "1")
                        .run(),
                    0,
                );
                let flag = checked(log.run(&s.home, &["status", "--json", "--offline"]), 0);
                log.assert_eq(
                    "baseline offline",
                    &baseline.envelope()["meta"]["offline"],
                    &json!(false),
                );
                log.json("variable offline", &variable.envelope()["meta"]["offline"]);
                log.json("flag offline", &flag.envelope()["meta"]["offline"]);
                log.assert_eq(
                    "offline flag",
                    &flag.envelope()["meta"]["offline"],
                    &json!(true),
                );
                log.assert_eq(
                    "offline mirror",
                    &variable.envelope()["meta"]["offline"],
                    &flag.envelope()["meta"]["offline"],
                );
            }
            "OMADDONS_FORMAT" => {
                let baseline = log.run(&s.home, &["status"]);
                baseline.ok();
                let human = serde_json::from_str::<Value>(baseline.stdout()).is_err();
                log.assert_eq("baseline is human", &human, &true);
                let variable =
                    checked(log.cmd(&s.home, &["status"]).env(row.name, "json").run(), 0);
                let flag = checked(log.run(&s.home, &["status", "--json"]), 0);
                log.assert_eq(
                    "format flag command",
                    &flag.envelope()["command"],
                    &json!("status"),
                );
                log.assert_eq(
                    "format mirror command",
                    &variable.envelope()["command"],
                    &flag.envelope()["command"],
                );
            }
            "OMADDONS_ACTOR" => {
                checked(
                    log.run(&s.home, &["install", "weakauras", "-t", "retail", "--json"]),
                    0,
                )
                .ok();
                let baseline = checked(log.run(&s.home, &["history", "--json", "--limit", "1"]), 0);
                log.assert_eq(
                    "baseline actor",
                    &baseline.data()["entries"][0]["actor"],
                    &json!("agent"),
                );
                checked(
                    log.cmd(&s.home, &["pin", "WeakAuras", "-t", "retail", "--json"])
                        .env(row.name, "widget")
                        .run(),
                    0,
                )
                .ok();
                checked(
                    log.run(
                        &s.home,
                        &[
                            "unpin",
                            "WeakAuras",
                            "-t",
                            "retail",
                            "--json",
                            "--actor",
                            "widget",
                        ],
                    ),
                    0,
                )
                .ok();
                let history = checked(log.run(&s.home, &["history", "--json", "--limit", "2"]), 0);
                let actors: Vec<_> = history.data()["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| entry["actor"].as_str().unwrap().to_owned())
                    .collect();
                log.json("recorded actors", &actors);
                log.assert_eq(
                    "actor variable and flag",
                    &actors,
                    &vec!["widget".to_owned(), "widget".to_owned()],
                );
            }
            name => missing_probe(format!(
                "no probe for the mirrored variable {name}: add one to each_mirrored_variable_acts_like_its_flag"
            )),
        }
    }
}
