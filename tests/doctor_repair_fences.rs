//! e2e suite: doctor_repair fences (E14): a stuck repair journal refuses only the writes that overlap what it protects (a token, a directory and its child, a skill file, a cache blob, a fenced orphan), one whose resources cannot be read refuses every change while read commands run, and a repair stuck after a command started is caught under the lock
#![cfg(test)]
#![cfg(debug_assertions)]

mod common;
// The contract suite's schema validator, shared as its module doc allows.
#[allow(dead_code)]
#[path = "contract/validate.rs"]
mod validate;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use common::archives::zip_text;
use common::differential::{Caller, Differential};
use common::home::Home;
use common::http::{Asset, gh_latest, gh_release_v};
use common::pause::Pause;
use common::privacy::{ACCOUNT, CHARACTER};
use common::repairs::{
    FixSpec, Root, Stuck, doctor_run, journals, plant_newer_major, plant_torn_first_line,
    plant_undecodable_prepared, stuck, subject_hashes,
};
use common::tlog::{Ran, TestLog, tlog};
use omaddons::engine::journal::{self, JournalVerdict};
use omaddons::engine::repair::{PreparedRead, read_prepared};
use omaddons::fsx::{Content, FsxError, Op, WriteSession, Writer, mutate};

const RETAIL: &str = "_retail_";
const ERA: &str = "_classic_era_";
const FOO: &str = "https://github.com/o/foo";
const BAR: &str = "https://github.com/o/bar";
// ubs:ignore — canaries to find leaks, not credentials
const TOKEN: &str = "ghp_FENCEc4n4ry0123456789";
// ubs:ignore — canaries to find leaks, not credentials
const TOKEN2: &str = "ghp_FENCEs3c0nd0123456789";
/// The failure mode a stuck chmod's fix explains.
const STEP_UNRECOGNIZED: &str = "fm-concurrency_primitives-journal-step-unrecognized";

/// A child process of the repair scenes (`common::repair::child_main`). Run on its own
/// it does nothing.
#[test]
#[ignore = "a child process of the repair scenes"]
fn child_repair() {
    common::repair::child_main();
}

/// `args` with `--json --progress json`.
fn args(args: &[&str]) -> Vec<String> {
    args.iter()
        .copied()
        .chain(["--json", "--progress", "json"])
        .map(str::to_owned)
        .collect()
}

fn set_mode(path: &Path, bits: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(bits)).unwrap();
}

/// A home for the fence scenarios: its clients, the fixture archives, logged.
struct Fence {
    home: Home,
    dl: PathBuf,
}

impl Fence {
    fn new(log: &TestLog, products: &[&str]) -> Self {
        let home = Home::new(products);
        log.env(&home);
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        let toc = |title: &str| format!("## Interface: 120105\n## Title: {title}\n");
        for (name, entries) in [
            ("foo", vec![("Foo/Foo.toc", toc("Foo"))]),
            ("bar", vec![("Bar/Bar.toc", toc("Bar"))]),
            ("qux", vec![("Qux/Qux.toc", toc("Qux"))]),
            (
                "x",
                vec![("X/X.toc", toc("X")), ("X/new.lua", "-- new\n".to_owned())],
            ),
        ] {
            let entries: Vec<(&str, &str)> =
                entries.iter().map(|(p, t)| (*p, t.as_str())).collect();
            fs::write(dl.join(format!("{name}.zip")), zip_text(&entries)).unwrap();
        }
        Self { home, dl }
    }

    /// `install <r> --file <zip> -t <target> --offline`.
    fn install_file(&self, r: &str, zip: &str, target: &str) -> Vec<String> {
        let file = self.dl.join(format!("{zip}.zip")).display().to_string();
        args(&["install", r, "--file", &file, "-t", target, "--offline"])
    }

    fn paths(&self) -> omaddons::state::paths::Paths {
        common::http::paths(&self.home.env)
    }

    fn credential(&self) -> PathBuf {
        self.home.env.config().join("omaddons/credentials/github")
    }

    /// Plants a token file readable by others (0644), so a chmod repair has work to do.
    fn loose_token(&self, token: &str) {
        let credential = self.credential();
        fs::create_dir_all(credential.parent().unwrap()).unwrap();
        set_mode(credential.parent().unwrap(), 0o700);
        fs::write(&credential, format!("{token}\n")).unwrap();
        set_mode(&credential, 0o644);
    }

    /// The chmod of the token to 0600, crashed after it ran, then the token replaced by
    /// a new file of the old mode: in neither its before nor its after state.
    fn stuck_chmod(&self, log: &TestLog, run_id: &str) -> Stuck {
        let credential = self.credential();
        stuck(
            &self.home,
            log,
            run_id,
            &[],
            FixSpec::Chmod {
                path: credential.clone(),
                mode: 0o600,
            },
            || {
                fs::remove_file(&credential).unwrap();
                fs::write(&credential, format!("{TOKEN}\n")).unwrap();
                set_mode(&credential, 0o644);
            },
        )
    }

    /// FAKEACCT, FakeRealm and FakeChar SavedVariables on `folder`'s client, so privacy
    /// is checked on every channel.
    fn plant_wtf(&self, folder: &str) {
        for dir in [ACCOUNT, CHARACTER] {
            let dir = self.home.client(folder).join(dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("Foo.lua"), "FooDB = { seen = true }\n").unwrap();
        }
    }
}

/// Runs `argv`, which names no account, realm or character on either channel.
fn run(log: &TestLog, f: &Fence, argv: &[String]) -> Ran {
    let ran = log.run(&f.home, argv);
    ran.assert_private();
    ran
}

/// Runs `argv` between E11's prediction and its comparison: `commits` names the clients
/// a committing command commits, `None` for one that recovers under the lock alone.
fn predicted(
    log: &TestLog,
    f: &Fence,
    folders: &[&str],
    label: &str,
    argv: &[String],
    commits: Option<&[&str]>,
) -> Ran {
    let mut differential = Differential::new(log, &f.home, folders, &[]);
    let caller = match commits {
        Some(ids) => {
            differential.predict(log, label);
            differential.commit(ids)
        }
        None => {
            differential.predict_unknown(log, label);
            Caller::Under
        }
    };
    let ran = run(log, f, argv);
    let compared = differential.compare(log, ran.envelope(), &journals(&f.home), &caller);
    assert!(compared >= 1, "{label}: the repair was compared");
    ran
}

/// The `journal_pending` warnings of `ran` that name `tx`.
fn pending_for(ran: &Ran, tx: &str) -> Vec<String> {
    ran.envelope()["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == "journal_pending")
        .filter_map(|w| w["message"].as_str())
        .filter(|m| m.contains(tx))
        .map(str::to_owned)
        .collect()
}

/// `ran` warned once of the stuck repair `tx`, with `reason`.
fn warned_once(ran: &Ran, tx: &str, reason: &str) {
    let pending = pending_for(ran, tx);
    assert_eq!(
        pending.len(),
        1,
        "one journal_pending for {tx}: {pending:?}"
    );
    assert!(
        pending[0].contains(&format!("({reason})")),
        "{}",
        pending[0]
    );
}

/// `ran` was refused by the fence of `tx`: exit and code, `details.reason`, `tx`,
/// `journal_pending`, a null `target` and `resource`.
fn fenced(ran: &Ran, exit: i32, code: &str, reason: &str, tx: &str, resource: &str) {
    ran.fails(exit, code);
    let details = &ran.envelope()["error"]["details"];
    assert_eq!(details["reason"], reason, "{details}");
    assert_eq!(details["tx"], tx, "{details}");
    assert_eq!(details["journal_pending"], tx, "{details}");
    assert_eq!(details["target"], Value::Null, "{details}");
    assert_eq!(
        details["resource"]
            .as_str()
            .map(|r| r.trim_end_matches('/')),
        Some(resource.trim_end_matches('/')),
        "{details}"
    );
}

/// The envelope with what changes between runs replaced: the home's root, the
/// transaction `tx` and the elapsed time.
fn stable(f: &Fence, envelope: &Value, tx: &str) -> Value {
    let root = f.home.env.root().display().to_string();
    let text = envelope
        .to_string()
        .replace(&root, "<root>")
        .replace(tx, "<tx>");
    let mut value: Value = serde_json::from_str(&text).unwrap();
    value["meta"]["elapsed_ms"] = 0.into();
    value
}

/// A release of `gh:o/foo` whose one TOC loads on retail and Classic Era alike.
fn foo_release(version: &str) -> String {
    format!("## Interface: 120105, 11508\n## Version: {version}\n")
}

// ---------------------------------------------------------------------------------------
// The builders, checked.

#[test]
fn fence_builders_stuck_produces_a_journal_recovery_cannot_settle() {
    let log = tlog!();
    log.secret(TOKEN);
    let f = Fence::new(&log, &["wow"]);
    run(&log, &f, &f.install_file("gh:o/foo", "foo", "retail")).ok();
    f.loose_token(TOKEN);
    let run_id = doctor_run(&f.home);
    log.step("a chmod repair crashed after its chmod, then the token replaced");
    let s = f.stuck_chmod(&log, &run_id);
    let diagnoses = journal::diagnose(&f.paths(), &[], &journal::no_saved_variables);
    let repair = diagnoses
        .journals
        .iter()
        .find(|d| d.tx.to_string() == s.tx)
        .unwrap();
    assert!(
        matches!(repair.verdict, JournalVerdict::StepUnrecognized { step: 1 }),
        "{:?}",
        repair.verdict
    );
    log.step("recovery leaves it");
    run(
        &log,
        &f,
        &args(&["config", "set", "check_interval_hours", "6"]),
    )
    .ok();
    assert!(s.journal.exists(), "kept");
}

#[test]
fn fence_builders_plants_decode_as_intended() {
    let log = tlog!();
    let f = Fence::new(&log, &["wow"]);
    let journal =
        |tx: &str| common::repairs::journal_dir(&f.home).join(format!("{tx}.repair.jsonl"));
    let undecodable = plant_undecodable_prepared(&f.home);
    let newer = plant_newer_major(&f.home);
    let torn = plant_torn_first_line(&f.home);
    let paths = f.paths();
    assert_eq!(
        read_prepared(&paths, &journal(&undecodable)).unwrap(),
        PreparedRead::Undecodable
    );
    assert_eq!(
        read_prepared(&paths, &journal(&newer)).unwrap(),
        PreparedRead::TooNew
    );
    assert_eq!(
        read_prepared(&paths, &journal(&torn)).unwrap(),
        PreparedRead::Absent
    );
}

// ---------------------------------------------------------------------------------------
// Scenarios.

#[test]
fn fence_a_stuck_chmod_guards_the_token_only() {
    let log = tlog!();
    log.secret(TOKEN);
    log.secret(TOKEN2);
    let mut f = Fence::new(&log, &["wow", "wow_classic_era"]);
    log.step("store a token; foo behind on era; bar on retail");
    log.cmd(&f.home, &args(&["auth", "github", "set"]))
        .stdin(format!("{TOKEN}\n"))
        .run()
        .ok();
    set_mode(&f.credential(), 0o644);
    let unannotated: &[(&str, u32)] = &[];
    let (v1, v2) = (foo_release("1"), foo_release("2"));
    let old: [Asset<'_>; 1] = [("Foo.zip", unannotated, vec![("Foo/Foo.toc", v1.as_str())])];
    let new: [Asset<'_>; 1] = [("Foo.zip", unannotated, vec![("Foo/Foo.toc", v2.as_str())])];
    f.home
        .installed_behind(FOO, ("v1", &old), ("v2", &new), &["era"]);
    run(&log, &f, &f.install_file("gh:o/bar", "bar", "retail")).ok();
    let run_id = doctor_run(&f.home);

    log.step("a chmod repair crashed after its chmod, then the token replaced");
    let s = f.stuck_chmod(&log, &run_id);
    let fences = omaddons::engine::fence::load(&f.paths(), &[]);
    let shown: Vec<String> = fences.iter().map(|fence| fence.resource.clone()).collect();
    log.json("fences", &shown);
    assert_eq!(shown, [f.credential().display().to_string()]);

    log.step("status reports it once");
    let status = run(&log, &f, &args(&["status"]));
    status.ok();
    warned_once(&status, &s.tx, "step_unrecognized");
    let config = f.home.env.config().join("omaddons");
    log.tree("config", &config);

    log.step("auth github set is refused");
    let refused = log
        .cmd(&f.home, &args(&["auth", "github", "set"]))
        .stdin(format!("{TOKEN2}\n"))
        .run();
    refused.assert_private();
    let credential = f.credential().display().to_string();
    fenced(
        &refused,
        1,
        "internal",
        "step_unrecognized",
        &s.tx,
        &credential,
    );
    assert_eq!(
        refused.envelope()["error"]["fix_argv"],
        serde_json::json!(["omaddons", "doctor", "explain", STEP_UNRECOGNIZED])
    );
    log.assert_unchanged("config", &config);
    validate::validate_envelope(refused.envelope(), refused.stderr()).unwrap();
    insta::assert_json_snapshot!(
        "fence_auth_github_set_refused_by_a_stuck_repair",
        stable(&f, refused.envelope(), &s.tx)
    );

    let folders = [RETAIL, ERA];
    log.step("an unrelated install on retail proceeds");
    let installed = predicted(
        &log,
        &f,
        &folders,
        "install qux on retail",
        &f.install_file("gh:o/qux", "qux", "retail"),
        Some(&["retail"]),
    );
    installed.ok();
    warned_once(&installed, &s.tx, "step_unrecognized");

    log.step("the other client's update proceeds");
    let updated = predicted(
        &log,
        &f,
        &folders,
        "update on era",
        &args(&["update", "-t", "era"]),
        Some(&["era"]),
    );
    updated.ok();
    warned_once(&updated, &s.tx, "step_unrecognized");
    assert_eq!(
        common::state::record(&f.home, "era").addons["foo"].version,
        "v2",
        "era updated"
    );

    log.step("a settings change beside the credentials proceeds");
    predicted(
        &log,
        &f,
        &folders,
        "config set",
        &args(&["config", "set", "check_interval_hours", "6"]),
        None,
    )
    .ok();

    for ran in [&status, &refused, &installed, &updated] {
        for canary in [TOKEN, TOKEN2] {
            assert!(
                !ran.stdout().contains(canary) && !ran.stderr().contains(canary),
                "a token leaked"
            );
        }
    }
}

#[test]
fn fence_a_stuck_quarantine_guards_a_directory_and_its_child() {
    let log = tlog!();
    let f = Fence::new(&log, &["wow"]);
    log.step("retail registered, an unmanaged AddOns/X beside it");
    run(&log, &f, &f.install_file("gh:o/foo", "foo", "retail")).ok();
    let addons = f.home.addons(RETAIL);
    let x = addons.join("X");
    fs::create_dir_all(&x).unwrap();
    fs::write(x.join("X.toc"), "## Interface: 120105\n").unwrap();
    fs::write(x.join("child.lua"), "-- by hand\n").unwrap();
    let run_id = doctor_run(&f.home);
    let retail = f.home.client(RETAIL);

    log.step("a quarantine of AddOns/X crashed after its rename, then X made again");
    let s = stuck(
        &f.home,
        &log,
        &run_id,
        std::slice::from_ref(&retail),
        FixSpec::Quarantine {
            path: x.clone(),
            root: Root::Client(retail.clone()),
        },
        || fs::create_dir(&x).unwrap(),
    );
    log.tree("retail addons", &addons);

    log.step("installing an addon whose folder is X is refused before its journal");
    let refused = run(
        &log,
        &f,
        &[
            f.install_file("gh:o/x", "x", "retail"),
            vec!["--force".to_owned()],
        ]
        .concat(),
    );
    fenced(
        &refused,
        1,
        "internal",
        "step_unrecognized",
        &s.tx,
        &x.display().to_string(),
    );
    assert_eq!(
        journals(&f.home),
        std::slice::from_ref(&s.journal),
        "no journal of its own"
    );
    log.assert_unchanged("retail addons", &addons);

    log.step("another addon installs");
    let installed = run(&log, &f, &f.install_file("gh:o/bar", "bar", "retail"));
    installed.ok();
    warned_once(&installed, &s.tx, "step_unrecognized");

    log.step("in process: a write into X is refused, one into Y is not");
    let paths = f.paths();
    let scopes =
        omaddons::engine::fence::table(&paths, std::slice::from_ref(&retail), None).unwrap();
    let mut session = WriteSession::untracked(Writer::Mutating, &scopes, "test");
    let write = |s: &mut WriteSession<'_>, path: &Path| {
        let op = Op::WriteFile {
            content: Content::Bytes(b"-- written\n"),
            mode: Some(0o644),
        };
        mutate(s, path, op)
    };
    assert!(
        matches!(
            write(&mut session, &x.join("child.lua")),
            Err(FsxError::Fenced(_))
        ),
        "a child of the fenced folder"
    );
    fs::create_dir_all(addons.join("Y")).unwrap();
    write(&mut session, &addons.join("Y/a.lua")).unwrap();
}

#[test]
fn fence_a_stuck_skill_rewrite_guards_only_that_file() {
    let log = tlog!();
    let f = Fence::new(&log, &["wow"]);
    run(&log, &f, &f.install_file("gh:o/foo", "foo", "retail")).ok();
    let paths = f.paths();
    let skills = paths.skill_dir();
    let skill = skills.join("SKILL.md");
    fs::create_dir_all(&skills).unwrap();
    fs::write(&skill, "---\nname: omaddons\n---\nguide v1\n").unwrap();
    set_mode(&skill, 0o644);
    let run_id = doctor_run(&f.home);

    log.step("a rewrite of SKILL.md crashed after its write, then the file edited");
    let s = stuck(
        &f.home,
        &log,
        &run_id,
        &[],
        FixSpec::Write {
            path: skill.clone(),
            content: "---\nname: omaddons\n---\nguide v2\n".to_owned(),
            mode: 0o644,
        },
        || fs::write(&skill, "edited by hand\n").unwrap(),
    );

    log.step("in process: SKILL.md is refused, another file of the skill is not");
    let scopes = omaddons::engine::fence::table(&paths, &[], Some(&skills)).unwrap();
    let mut session = WriteSession::untracked(Writer::Skill, &scopes, "test");
    let write = |s: &mut WriteSession<'_>, path: &Path| {
        let op = Op::WriteFile {
            content: Content::Bytes(b"notes\n"),
            mode: Some(0o644),
        };
        mutate(s, path, op)
    };
    match write(&mut session, &skill) {
        Err(FsxError::Fenced(refusal)) => {
            assert_eq!(refusal.resource, skill.display().to_string());
        }
        other => panic!("SKILL.md was not fenced: {other:?}"), // ubs:ignore — fails the test
    }
    write(&mut session, &skills.join("notes.md")).unwrap();
    // `robot-docs install-skill` (B3) is not a command yet: its table will be fenced
    // through `fence::table`, which the scan test holds every mutating table to.
    assert!(
        !omaddons::app::COMMANDS
            .iter()
            .any(|c| c.name == "robot-docs install-skill"),
        "B3 landed: add its refusal here"
    );

    log.step("everything else proceeds");
    let set = run(
        &log,
        &f,
        &args(&["config", "set", "check_interval_hours", "6"]),
    );
    set.ok();
    warned_once(&set, &s.tx, "step_unrecognized");
    run(&log, &f, &f.install_file("gh:o/bar", "bar", "retail")).ok();
}

#[test]
fn fence_a_stuck_blob_quarantine_guards_that_blob_only() {
    let log = tlog!();
    let mut f = Fence::new(&log, &["wow", "wow_classic_era"]);
    log.step("two releases of foo in the blob cache, through era");
    let unannotated: &[(&str, u32)] = &[];
    let (v1, v2) = (foo_release("1"), foo_release("2"));
    let old: [Asset<'_>; 1] = [("Foo.zip", unannotated, vec![("Foo/Foo.toc", v1.as_str())])];
    let new: [Asset<'_>; 1] = [("Foo.zip", unannotated, vec![("Foo/Foo.toc", v2.as_str())])];
    f.home
        .installed_behind(FOO, ("v1", &old), ("v2", &new), &["era"]);
    let blobs_dir = f.home.env.cache().join("omaddons/blobs");
    let blobs = || -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = fs::read_dir(&blobs_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.file_name().is_some_and(|n| n != "index.json"))
            .collect();
        found.sort();
        found
    };
    let first = blobs();
    assert_eq!(first.len(), 1, "{first:?}");
    let blob_a = first[0].clone();
    run(&log, &f, &args(&["update", "-t", "era"])).ok();
    assert_eq!(blobs().len(), 2, "v2's blob too");
    run(&log, &f, &f.install_file("gh:o/bar", "bar", "retail")).ok();
    let run_id = doctor_run(&f.home);

    // The quarantined copy is taken away, so the blob is at neither end of the rename: the
    // step is in neither state, and the cache no longer holds the blob, so installing its
    // release downloads and writes it again. (Other bytes at the blob's own path would
    // be read as the cached archive: verifying blobs is the blob-hash failure mode's.)
    log.step("a quarantine of v1's blob crashed after its rename, then the parked copy lost");
    let journal_dir = common::repairs::journal_dir(&f.home);
    let s = stuck(
        &f.home,
        &log,
        &run_id,
        &[],
        FixSpec::Quarantine {
            path: blob_a.clone(),
            root: Root::Cache,
        },
        || {
            let journal = fs::read_dir(&journal_dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.to_string_lossy().ends_with(".repair.jsonl"))
                .unwrap();
            let text = fs::read_to_string(journal).unwrap();
            let prepared: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
            let parked = PathBuf::from(prepared["primitives"][0]["to"].as_str().unwrap());
            fs::remove_file(parked).unwrap();
        },
    );
    assert!(!blob_a.exists(), "the blob left the cache");
    let addons = f.home.addons(RETAIL);
    log.tree("retail addons", &addons);

    log.step("the release with the other blob installs on retail");
    let installed = run(
        &log,
        &f,
        &args(&["install", "gh:o/foo", "--version", "v2", "-t", "retail"]),
    );
    installed.ok();
    warned_once(&installed, &s.tx, "step_unrecognized");
    log.tree("retail addons", &addons);

    log.step("the release whose blob is fenced is refused on retail");
    let refused = run(
        &log,
        &f,
        &args(&["install", "gh:o/foo", "--version", "v1", "-t", "retail"]),
    );
    fenced(
        &refused,
        1,
        "internal",
        "step_unrecognized",
        &s.tx,
        &blob_a.display().to_string(),
    );
    log.assert_unchanged("retail addons", &addons);
}

#[test]
fn fence_an_unreadable_prepared_fails_every_committing_command_closed() {
    type Plant = fn(&Home) -> String;
    let cases: [(&str, Plant, i32, &str, &str); 2] = [
        (
            "undecodable",
            plant_undecodable_prepared,
            1,
            "state_corrupt",
            "journal_unreadable",
        ),
        (
            "newer_major",
            plant_newer_major,
            4,
            "state_too_new",
            "journal_too_new",
        ),
    ];
    let outer = tlog!();
    for (case, plant, exit, code, reason) in cases {
        let log = outer.case(case);
        log.secret(TOKEN);
        let mut f = Fence::new(&log, &["wow"]);
        log.step("foo behind on retail, bar published, a played WTF/");
        let unannotated: &[(&str, u32)] = &[];
        let (v1, v2) = (foo_release("1"), foo_release("2"));
        let old: [Asset<'_>; 1] = [("Foo.zip", unannotated, vec![("Foo/Foo.toc", v1.as_str())])];
        let new: [Asset<'_>; 1] = [("Foo.zip", unannotated, vec![("Foo/Foo.toc", v2.as_str())])];
        f.home
            .installed_behind(FOO, ("v1", &old), ("v2", &new), &["retail"]);
        let bar_toc = "## Interface: 120105\n";
        let bar: [Asset<'_>; 1] = [("Bar.zip", unannotated, vec![("Bar/Bar.toc", bar_toc)])];
        gh_release_v(&f.home.http, BAR, "v1", &bar);
        gh_latest(&f.home.http, BAR, "v1");
        f.plant_wtf(RETAIL);

        log.step("a repair journal whose prepared line cannot be read");
        let tx = plant(&f.home);
        let resource = format!("journal:{tx}");
        let before = subject_hashes(&f.home, &[RETAIL]);
        let refused: Vec<(&str, Vec<String>, Option<String>)> = vec![
            (
                "install",
                args(&["install", "gh:o/bar", "-t", "retail"]),
                None,
            ),
            (
                "install --dry-run",
                args(&["install", "gh:o/bar", "-t", "retail", "--dry-run"]),
                None,
            ),
            ("update", args(&["update", "-t", "retail"]), None),
            ("remove", args(&["remove", "foo", "-t", "retail"]), None),
            (
                "config set",
                args(&["config", "set", "check_interval_hours", "6"]),
                None,
            ),
            (
                "auth github set",
                args(&["auth", "github", "set"]),
                Some(format!("{TOKEN}\n")),
            ),
            (
                "targets rename",
                args(&["targets", "rename", "retail", "main"]),
                None,
            ),
            (
                "backup create",
                args(&["backup", "create", "-t", "retail"]),
                None,
            ),
        ];
        for (label, argv, stdin) in refused {
            log.step(&format!("{label} is refused"));
            let mut cmd = log.cmd(&f.home, &argv);
            if let Some(stdin) = stdin {
                cmd = cmd.stdin(stdin);
            }
            let ran = cmd.run();
            ran.assert_private();
            ran.fails(exit, code);
            let details = &ran.envelope()["error"]["details"];
            assert_eq!(details["reason"], reason, "{label}: {details}");
            assert_eq!(details["tx"], tx.as_str(), "{label}: {details}");
            assert_eq!(details["resource"], resource.as_str(), "{label}: {details}");
            assert!(
                ran.envelope()["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.ends_with("every change is refused until it is set aside")),
                "{label}: {}",
                ran.envelope()["error"]["message"]
            );
            assert_eq!(
                subject_hashes(&f.home, &[RETAIL]),
                before,
                "{label} changed nothing"
            );
            if label == "install" {
                validate::validate_envelope(ran.envelope(), ran.stderr()).unwrap();
                insta::assert_json_snapshot!(
                    format!("fence_install_refused_by_a_closed_set_{case}"),
                    stable(&f, ran.envelope(), &tx)
                );
            }
        }
        for read in [&["status"][..], &["list"], &["history"]] {
            log.step(&format!("{} runs", read[0]));
            let ran = run(&log, &f, &args(read));
            ran.ok();
            warned_once(&ran, &tx, reason);
            assert!(
                pending_for(&ran, &tx)[0]
                    .ends_with("every change is refused until it is set aside")
            );
        }
        log.step("doctor runs");
        let doctor = run(&log, &f, &args(&["doctor"]));
        doctor.exit(1);
        assert_eq!(
            subject_hashes(&f.home, &[RETAIL]),
            before,
            "reads changed nothing"
        );
    }
}

#[test]
fn fence_a_repair_with_no_complete_line_protects_nothing() {
    let log = tlog!();
    let f = Fence::new(&log, &["wow"]);
    log.step("a repair journal with a torn first line");
    let tx = plant_torn_first_line(&f.home);
    log.step("a settings change proceeds, and prunes it as never started");
    let set = run(
        &log,
        &f,
        &args(&["config", "set", "check_interval_hours", "6"]),
    );
    set.ok();
    assert!(journals(&f.home).is_empty(), "{:?}", journals(&f.home));
    log.step("status no longer reports it");
    let status = run(&log, &f, &args(&["status"]));
    status.ok();
    assert!(pending_for(&status, &tx).is_empty());
}

#[test]
fn fence_a_repair_stuck_after_the_table_was_built_is_caught_under_the_lock() {
    let log = tlog!();
    log.secret(TOKEN);
    let f = Fence::new(&log, &["wow"]);
    run(&log, &f, &f.install_file("gh:o/foo", "foo", "retail")).ok();
    f.loose_token(TOKEN);
    let run_id = doctor_run(&f.home);
    let addons = f.home.addons(RETAIL);
    log.tree("retail addons", &addons);

    log.step("an install paused with its table built, before its lock");
    let pause = Pause::new(f.home.env.root());
    let paused = log
        .cmd(&f.home, &f.install_file("gh:o/bar", "bar", "retail"))
        .failpoints("engine.snapshot.prestaged=pause")
        .pause(&pause)
        .spawn();
    log.reached(&pause, "engine.snapshot.prestaged");

    log.step("meanwhile a chmod repair crashed, then the token replaced");
    let s = f.stuck_chmod(&log, &run_id);

    log.step("the install resumes and is refused under the lock");
    log.resume(&pause, "engine.snapshot.prestaged");
    let refused = paused.wait();
    refused.assert_private();
    fenced(
        &refused,
        1,
        "internal",
        "step_unrecognized",
        &s.tx,
        &format!("journal:{}", s.tx),
    );
    assert!(
        refused.envelope()["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("it could not be settled after this command started")),
        "{}",
        refused.envelope()["error"]["message"]
    );
    log.assert_unchanged("retail addons", &addons);
    assert_eq!(
        journals(&f.home),
        std::slice::from_ref(&s.journal),
        "no folder journal left"
    );

    log.step("the same install again, with the fence loaded, proceeds");
    let installed = run(&log, &f, &f.install_file("gh:o/bar", "bar", "retail"));
    installed.ok();
    warned_once(&installed, &s.tx, "step_unrecognized");
}

#[test]
fn fence_retention_keeps_a_fenced_orphan() {
    let log = tlog!();
    let f = Fence::new(&log, &["wow"]);
    run(&log, &f, &f.install_file("gh:o/foo", "foo", "retail")).ok();
    let retail = f.home.client(RETAIL);
    let stage = retail.join("Interface/.omaddons/stage");
    let (t1, t2) = (
        omaddons::model::TxId::new().to_string(),
        omaddons::model::TxId::new().to_string(),
    );
    let age = |dir: &Path| {
        let past = SystemTime::now() - Duration::from_secs(2 * 3600);
        fs::File::open(dir).unwrap().set_modified(past).unwrap();
    };
    let orphan = |tx: &str| {
        let dir = stage.join(tx);
        fs::create_dir_all(dir.join("Foo")).unwrap();
        fs::write(dir.join("Foo/Foo.toc"), "## Interface: 120105\n").unwrap();
        age(&dir.join("Foo"));
        age(&dir);
        dir
    };
    log.step("two orphan stages an hour past the sweep's age");
    let (first, second) = (orphan(&t1), orphan(&t2));
    let run_id = doctor_run(&f.home);

    log.step("a quarantine of the first crashed after its rename, then it was made again");
    let s = stuck(
        &f.home,
        &log,
        &run_id,
        std::slice::from_ref(&retail),
        FixSpec::Quarantine {
            path: first.clone(),
            root: Root::Client(retail.clone()),
        },
        || {
            orphan(&t1);
        },
    );

    log.step("a commit on retail runs retention");
    let installed = run(&log, &f, &f.install_file("gh:o/qux", "qux", "retail"));
    installed.ok();
    warned_once(&installed, &s.tx, "step_unrecognized");
    assert!(first.exists(), "the fenced orphan is kept");
    assert!(!second.exists(), "the other orphan is swept");
}
