//! The published file schemas describe what the binary writes: every `.json` document,
//! history line and journal line a session leaves, and the exported `addons.toml`, validate
//! strictly against their kinds' schemas (`contract::schema::file_schemas`). A `.json`
//! document no kind names fails: a new persisted file cannot ship without one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omaddons::contract::schema::file_schemas;
use omaddons::model::TxId;
use omaddons::state::paths::{ClientPaths, Paths};
use serde_json::Value;

use super::common::privacy::assert_private_all;
use super::common::tlog::{TestLog, tlog};
use super::common::wtf::Wtf;
use super::validate::Validator;
use super::{Scene, support};

/// Directories whose files are not validated, and why.
fn skipped(paths: &Paths, clients: &[ClientPaths]) -> Vec<(PathBuf, &'static str)> {
    let parent = |path: PathBuf| path.parent().map(Path::to_path_buf).unwrap_or(path);
    let tx = TxId::new();
    let mut out = vec![
        (
            paths.state_snapshots_dir(),
            "state copies kept for undo, which may be older versions",
        ),
        (paths.state_trash(), "staged deletions"),
        (paths.inflight_dir(), "in-flight locks"),
        (paths.cache_trash(), "staged deletions"),
        (paths.cache_inflight_dir(), "in-flight locks"),
        (paths.blobs_dir(), "downloaded archives"),
        (
            parent(paths.cache_tmp(&tx)),
            "temporary downloads and checkouts",
        ),
        (paths.git_home(), "git's empty home"),
    ];
    for client in clients {
        out.push((parent(client.snapshots(&tx)), "copies kept for undo"));
        out.push((parent(client.stage(&tx)), "staging"));
        out.push((client.trash(), "staged deletions"));
    }
    out
}

/// The kind of a `.json` document, by where the binary writes it.
fn kind_of(file: &Path, paths: &Paths, clients: &[ClientPaths]) -> Option<&'static str> {
    let name = file.file_name()?.to_str()?;
    let parent = file.parent()?;
    let exact = [
        (paths.registry_file(), "registry"),
        (paths.status_file(), "status"),
        (paths.discovery_cache(), "discovery_cache"),
        (paths.release_memo(), "release_memo"),
        (paths.about_memo(), "about_memo"),
        (paths.wowi_index(), "wowi_index"),
        (paths.export_memo(), "export_memo"),
        (paths.backup_inventory(), "backup_inventory"),
        (paths.history_tail(), "history_tail"),
        (paths.errors_memo(), "errors_memo"),
        (paths.errors_ledger(), "errors_ledger"),
        (paths.errors_ack(), "errors_ack"),
        (paths.doorbell_file(), "doorbell"),
    ];
    if let Some((_, kind)) = exact.iter().find(|(path, _)| path == file) {
        return Some(kind);
    }
    if parent == paths.targets_dir() {
        return Some(if name.ends_with(".files.json") {
            "files_index"
        } else {
            "target_record"
        });
    }
    if file.starts_with(paths.http_cache_dir()) {
        return Some("http_validators");
    }
    if parent == paths.plans_dir() {
        return Some("plan");
    }
    for client in clients {
        if file == client.marker() {
            return Some("target_marker");
        }
        if parent.parent() == Some(client.backups().as_path()) {
            match name {
                "backup.json" => return Some("backup"),
                "manifest.json" => return Some("backup_manifest"),
                "addons.json" => return Some("backup_addons"),
                _ => {}
            }
        }
    }
    None
}

/// Every file under `dir`, recursively, outside the skipped directories.
fn files(dir: &Path, skip: &[(PathBuf, &str)], out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if skip.iter().any(|(skipped, _)| path.starts_with(skipped)) {
            continue;
        }
        if path.is_dir() {
            // A backup's `WTF/` copy is the player's files, not omaddons'.
            if path.file_name().is_some_and(|n| n == "WTF") {
                continue;
            }
            files(&path, skip, out);
        } else {
            out.push(path);
        }
    }
}

/// Every persisted document of the session, validated by kind; returns the count per kind
/// validated. A failure names the file (relative to the root), its kind and every failure.
fn validate_session(
    log: &TestLog,
    s: &Scene,
    validators: &BTreeMap<&'static str, Validator>,
    counts: &mut BTreeMap<String, usize>,
) {
    let env = &s.home.env;
    let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
    let clients: Vec<ClientPaths> = ["_retail_", "_classic_"]
        .iter()
        .map(|folder| ClientPaths::new(s.home.client(folder)))
        .collect();
    let skip = skipped(&paths, &clients);
    let mut all = Vec::new();
    for dir in [
        paths.state_dir().to_path_buf(),
        paths.cache_dir().to_path_buf(),
    ]
    .into_iter()
    .chain(clients.iter().map(ClientPaths::meta_dir))
    {
        files(&dir, &skip, &mut all);
    }
    all.sort();
    let root = env.root();
    let shown = |file: &Path| {
        file.strip_prefix(root)
            .unwrap_or(file)
            .display()
            .to_string()
    };
    let mut problems = Vec::new();
    let mut step_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut check = |kind: &str, file: &Path, document: &Value, problems: &mut Vec<String>| {
        let failures = validators[kind].validate(document);
        if failures.is_empty() {
            *step_counts.entry(kind.to_owned()).or_default() += 1;
        } else {
            let listed: Vec<String> = failures.iter().map(ToString::to_string).collect();
            problems.push(format!(
                "{} ({kind}):\n  {}",
                shown(file),
                listed.join("\n  ")
            ));
        }
    };
    for file in &all {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let history = file == &paths.history_file();
        let journal =
            file.parent() == Some(paths.journal_dir().as_path()) && name.ends_with(".jsonl");
        if history || journal {
            let kind = if history { "history" } else { "journal" };
            for line in std::fs::read_to_string(file)
                .unwrap()
                .lines()
                .filter(|l| !l.trim().is_empty())
            {
                match serde_json::from_str::<Value>(line) {
                    Ok(document) => check(kind, file, &document, &mut problems),
                    Err(error) => problems.push(format!(
                        "{} ({kind}): a line that is not JSON: {error}",
                        shown(file)
                    )),
                }
            }
            continue;
        }
        if !name.ends_with(".json") {
            continue;
        }
        let Some(kind) = kind_of(file, &paths, &clients) else {
            problems.push(format!("{}: a .json document no kind names", shown(file)));
            continue;
        };
        match serde_json::from_slice::<Value>(&std::fs::read(file).unwrap()) {
            Ok(document) => check(kind, file, &document, &mut problems),
            Err(error) => problems.push(format!("{} ({kind}): not JSON: {error}", shown(file))),
        }
    }
    let toml_file = paths.addons_toml();
    if toml_file.is_file() {
        let text = std::fs::read_to_string(&toml_file).unwrap();
        let document = serde_json::to_value(text.parse::<toml::Table>().unwrap()).unwrap();
        check("desired_state", &toml_file, &document, &mut problems);
    }
    log.json("validated", &step_counts);
    if !problems.is_empty() {
        log.note(problems.join("\n"));
    }
    assert!(
        problems.is_empty(),
        "persisted documents that do not validate against their kind:\n{}",
        problems.join("\n")
    );
    for (kind, count) in step_counts {
        *counts.entry(kind).or_default() += count;
    }
}

#[test]
fn every_file_a_session_writes_validates_against_its_kind() {
    let log = tlog!();
    let s = Scene::new();
    Wtf::at(s.home.client("_retail_"))
        .saved_variables("Foo", 100)
        .build();
    let validators: BTreeMap<&'static str, Validator> = file_schemas()
        .into_iter()
        .map(|(kind, schema)| (kind, Validator::new(&schema.to_value(), true).unwrap()))
        .collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let foo2 = s.zip("foo2");
    let steps: [(&str, Vec<&str>); 4] = [
        ("install", vec!["install", "gh:o/foo", "-t", "retail"]),
        ("export --write", vec!["export", "--write"]),
        ("backup create", vec!["backup", "create", "-t", "retail"]),
        ("status", vec!["status"]),
    ];
    for (index, (name, args)) in steps.iter().enumerate() {
        // Before `status`, a crash leaves a journal behind.
        if index == 3 {
            log.step("install, crashed after prepared");
            let mut argv: Vec<&str> = vec!["install", "gh:o/foo", "--file", &foo2, "-t", "retail"];
            argv.extend(["--json", "--progress", "json"]);
            let crashed = log
                .cmd(&s.home, &argv)
                .failpoints("engine.journal.prepared=abort")
                .run();
            assert_ne!(crashed.run.code, 0, "the failpoint aborted the install");
            assert_private_all(&crashed.run.stdout, &crashed.run.stderr);
            log.step("files after install, crashed after prepared");
            log.tree("state", &s.home.env.state());
            log.tree("cache", &s.home.env.cache());
            validate_session(&log, &s, &validators, &mut counts);
        }
        log.step(name);
        let mut argv = args.clone();
        argv.extend(["--json", "--progress", "json"]);
        let checked = support::check(log.run(&s.home, &argv).to_run(), 0);
        assert_private_all(&checked.run.stdout, &checked.run.stderr);
        log.step(&format!("files after {name}"));
        log.tree("state", &s.home.env.state());
        log.tree("cache", &s.home.env.cache());
        validate_session(&log, &s, &validators, &mut counts);
    }

    log.step("coverage");
    log.json("validated", &counts);
    let schemas = file_schemas();
    let unwritten: Vec<&str> = schemas
        .keys()
        .copied()
        .filter(|kind| !counts.contains_key(*kind))
        .collect();
    log.note(format!("kinds no step wrote: {unwritten:?}"));
    for kind in [
        "registry",
        "target_record",
        "files_index",
        "target_marker",
        "history",
        "history_tail",
        "journal",
        "backup",
        "backup_manifest",
        "export_memo",
        "desired_state",
    ] {
        assert!(
            counts.contains_key(kind),
            "{kind} was validated: {counts:?}"
        );
    }
}
