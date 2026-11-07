//! The semantic manifest's report: its section counts, each checked against its registry,
//! and the classifier's verdict against an empty baseline and against the previous run, so
//! a change's semantic diff is one test run away:
//! `cargo test --test integration contract::manifest_report -- --nocapture`.

use std::path::PathBuf;
use std::time::Instant;

use clap::CommandFactory;
use omaddons::app::{COMMANDS, ENVELOPE_ONLY};
use omaddons::cli::Cli;
use omaddons::contract::Registry;
use omaddons::contract::actions::Action;
use omaddons::contract::env;
use omaddons::contract::events::{EventName, LogLevel, Phase};
use omaddons::contract::manifest::{Manifest, build_checked, classify, report, section_counts};
use omaddons::contract::wire;
use omaddons::doctor::exit::Exit as DoctorExit;
use omaddons::doctor::registry::FAILURE_MODES;
use omaddons::error::{ErrorCode, Exit, WarningCode};
use omaddons::state::config::KEYS;
use omaddons::state::history::RepairOp;
use omaddons::state::store::KINDS;
use serde_json::json;

use crate::common::tlog::tlog;

/// Every built clap path, the root included.
fn clap_paths(cmd: &clap::Command, path: &str, out: &mut Vec<String>) {
    out.push(path.to_owned());
    for sub in cmd.get_subcommands() {
        let child = if path == "omaddons" {
            sub.get_name().to_owned()
        } else {
            format!("{path} {}", sub.get_name())
        };
        clap_paths(sub, &child, out);
    }
}

/// The first line where two renders differ.
fn first_difference(a: &str, b: &str) -> String {
    a.lines()
        .zip(b.lines())
        .enumerate()
        .find(|(_, (x, y))| x != y)
        .map_or_else(
            || "the same lines, different lengths".to_owned(),
            |(i, (x, y))| format!("line {}: {x:?} vs {y:?}", i + 1),
        )
}

/// `text` written to `path` through a temporary sibling, so a concurrent run never reads a
/// half-written file.
fn write_atomically(path: &PathBuf, text: &str) {
    let temporary = path.with_extension("tmp");
    let written = std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, path));
    assert!(written.is_ok(), "{}: {written:?}", path.display());
}

#[test]
fn manifest_report() {
    let log = tlog!();

    log.step("build");
    let started = Instant::now();
    let (m, conflicts) = build_checked(&Registry::builtin());
    let elapsed_ms = started.elapsed().as_millis();
    assert!(
        conflicts.is_empty(),
        "ids two collectors produced differently: {conflicts:#?}"
    );
    let renders: Vec<String> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..2)
            .map(|_| scope.spawn(|| build_checked(&Registry::builtin()).0.render(&[])))
            .collect();
        let mut renders = vec![m.render(&[])];
        renders.extend(threads.into_iter().map(|t| t.join().unwrap()));
        renders
    });
    for pair in renders.windows(2) {
        assert!(
            pair[0] == pair[1],
            "renders differ on another thread: {}",
            first_difference(&pair[0], &pair[1])
        );
    }
    log.json(
        "build",
        &json!({"elapsed_ms": elapsed_ms, "nodes": m.nodes.len()}),
    );

    log.step("round trip");
    assert!(
        Manifest::parse(&renders[0]).ok().as_ref() == Some(&m),
        "parse(render(m)) is m"
    );

    log.step("counts");
    let counts = section_counts(&m);
    log.json("section_counts", &counts);
    let mut root = Cli::command();
    root.build();
    let mut paths = Vec::new();
    clap_paths(&root, "omaddons", &mut paths);
    let envelope_only_extra = ENVELOPE_ONLY
        .iter()
        .filter(|only| !paths.contains(&only.name.to_owned()))
        .count();
    let exits = |dictionary: &str| {
        let prefix = format!("exit {dictionary} ");
        m.nodes.keys().filter(|id| id.starts_with(&prefix)).count()
    };
    let expected = [
        (
            "commands",
            paths.len() + envelope_only_extra,
            "the built clap paths and ENVELOPE_ONLY",
        ),
        (
            "commands with data",
            COMMANDS.len() + ENVELOPE_ONLY.len(),
            "COMMANDS and ENVELOPE_ONLY",
        ),
        ("error codes", ErrorCode::ALL.len(), "ErrorCode::ALL"),
        ("warning", WarningCode::ALL.len(), "WarningCode::ALL"),
        ("action", Action::ALL.len(), "Action::ALL"),
        ("events", EventName::ALL.len(), "EventName::ALL"),
        ("phase", Phase::ALL.len(), "Phase::ALL"),
        ("log-level", LogLevel::ALL.len(), "LogLevel::ALL"),
        ("env vars", env::release().count(), "ENV_VARS' release rows"),
        ("config keys", KEYS.len(), "config::KEYS"),
        (
            "file kinds",
            KINDS.len() + 2,
            "store::KINDS and both journal families",
        ),
        ("fm", FAILURE_MODES.len(), "doctor::registry::FAILURE_MODES"),
        ("doctor-ops", RepairOp::ALL.len(), "RepairOp::ALL"),
    ];
    for (section, want, registry) in expected {
        let got = counts.get(section).copied().unwrap_or(0);
        assert_eq!(
            got, want,
            "section {section}: the manifest has {got}, {registry} has {want}"
        );
    }
    for (dictionary, want, registry) in [
        ("general", Exit::ALL.len(), "Exit::ALL"),
        ("doctor", DoctorExit::ALL.len(), "doctor::exit::Exit::ALL"),
    ] {
        let got = exits(dictionary);
        assert_eq!(
            got, want,
            "exit {dictionary}: the manifest has {got}, {registry} has {want}"
        );
    }

    log.step("report");
    let text = report(&m, None);
    println!("{text}");
    log.note(&text);

    log.step("previous run");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("contract-manifest");
    let previous_path = dir.join("manifest.json");
    if let Ok(previous_text) = std::fs::read_to_string(&previous_path) {
        match Manifest::parse(&previous_text) {
            Ok(previous) => {
                let compared = format!(
                    "compared with the previous run ({}):\n{}",
                    previous_path.display(),
                    report(&m, Some(&previous))
                );
                println!("{compared}");
                log.note(&compared);
                let changes = classify(&previous, &m);
                let (additive, breaking, unclassified) = changes.counts();
                log.json(
                    "previous_run",
                    &json!({
                        "path": previous_path.display().to_string(),
                        "nodes": previous.nodes.len(),
                        "additive": additive,
                        "breaking": breaking,
                        "unclassified": unclassified,
                        "wire_bump": format!("{:?}", changes.wire_bump()),
                    }),
                );
            }
            Err(e) => {
                let note =
                    format!("the previous run's manifest is unreadable (format changed?): {e}");
                println!("{note}");
                log.note(&note);
            }
        }
    } else {
        log.note("no previous run to compare with");
    }

    log.step("write");
    std::fs::create_dir_all(&dir).unwrap();
    let mut names = wire::names(&m).join("\n");
    names.push('\n');
    let files = [
        ("manifest.json", renders[0].clone()),
        ("wire_names.txt", names),
        ("report.txt", text),
    ];
    for (name, contents) in &files {
        write_atomically(&dir.join(name), contents);
    }
    let written: Vec<String> = files
        .iter()
        .map(|(name, _)| dir.join(name).display().to_string())
        .collect();
    println!("wrote:\n  {}", written.join("\n  "));
    log.note(format!("wrote {}", written.join(", ")));
}
