//! `-v`-only fields are optional in their serialize schemas (M4-D9): each command below
//! validates, data, frame and every stderr event, both with and without `-v`, and the
//! named field is absent without it and present with it.

use serde_json::Value;

use super::common::tlog::tlog;
use super::{Scene, support};

/// One command and a field whose presence `-v` decides.
struct Case {
    name: &'static str,
    argv: Vec<String>,
    /// The objects that hold the field.
    holders: fn(&Value) -> Vec<&Value>,
    key: &'static str,
    /// Present with `-v` (`true`), or only without it (`false`).
    with_verbose: bool,
}

fn each<'a>(data: &'a Value, array: &str) -> Vec<&'a Value> {
    data[array]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn install_line(data: &Value) -> Vec<&Value> {
    each(data, "entries")
        .into_iter()
        .filter(|entry| entry["kind"] == "install")
        .collect()
}

#[test]
fn verbose_only_fields_are_optional_and_validate() {
    let log = tlog!();
    let s = Scene::new();
    let words = |args: &[&str]| -> Vec<String> { args.iter().map(|a| (*a).to_owned()).collect() };

    log.step("install");
    let zip = s.zip("foo");
    support::check(
        log.run(
            &s.home,
            &[
                "install", "gh:o/foo", "--file", &zip, "-t", "retail", "--json",
            ],
        )
        .to_run(),
        0,
    );

    let foo2 = s.zip("foo2");
    let cases = [
        Case {
            name: "status",
            argv: words(&["status"]),
            holders: |data| each(data, "targets"),
            key: "client_dir",
            with_verbose: true,
        },
        Case {
            name: "list",
            argv: words(&["list"]),
            holders: |data| each(data, "targets"),
            key: "client_dir",
            with_verbose: true,
        },
        Case {
            name: "history ops",
            argv: words(&["history"]),
            holders: install_line,
            key: "ops",
            with_verbose: true,
        },
        Case {
            name: "history changes",
            argv: words(&["history"]),
            holders: install_line,
            key: "changes",
            with_verbose: false,
        },
        Case {
            name: "targets",
            argv: words(&["targets"]),
            holders: |data| each(data, "targets"),
            key: "details",
            with_verbose: true,
        },
        Case {
            name: "info",
            argv: words(&["info", "Foo", "-t", "retail"]),
            holders: |data| each(data, "installed"),
            key: "record_path",
            with_verbose: true,
        },
        Case {
            name: "install --dry-run",
            argv: words(&[
                "install",
                "gh:o/foo",
                "--file",
                &foo2,
                "-t",
                "retail",
                "--dry-run",
            ]),
            holders: |data| vec![data],
            key: "client_dir",
            with_verbose: true,
        },
    ];

    let (mut progress, mut debug) = (0usize, 0usize);
    for case in &cases {
        for verbose in [false, true] {
            log.step(&format!(
                "{} {} -v",
                case.name,
                if verbose { "with" } else { "without" }
            ));
            let mut argv = case.argv.clone();
            argv.push("--json".to_owned());
            if verbose {
                argv.extend(["-v", "--progress", "json"].map(str::to_owned));
            }
            let checked = support::check(log.run(&s.home, &argv).to_run(), 0);
            if verbose {
                for line in checked.run.stderr.lines() {
                    let event: Value = serde_json::from_str(line).unwrap();
                    progress += usize::from(event["event"] == "progress");
                    debug += usize::from(event["event"] == "log" && event["level"] == "debug");
                }
            }
            let holders = (case.holders)(&checked.envelope["data"]);
            assert!(
                !holders.is_empty(),
                "{}: nothing holds `{}`",
                case.name,
                case.key
            );
            let values: Vec<Option<&Value>> = holders.iter().map(|h| h.get(case.key)).collect();
            log.json("field", &values);
            let expected = verbose == case.with_verbose;
            assert!(
                values.iter().all(|value| value.is_some() == expected),
                "{}: `{}` {} {}: {values:?}",
                case.name,
                case.key,
                if expected {
                    "must be present"
                } else {
                    "must be absent"
                },
                if verbose { "with -v" } else { "without -v" }
            );
        }
    }

    log.step("events");
    log.note(format!(
        "validated under -v: {progress} progress events, {debug} debug log events"
    ));
    assert!(
        progress > 0 && debug > 0,
        "event validation ran under -v: {progress} progress, {debug} debug log events"
    );
}
