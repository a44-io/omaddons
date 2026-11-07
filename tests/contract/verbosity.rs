//! Verbosity levels (M4-D17): what `-v` prints, characterized, and what `-vv` adds.
//!
//! Every row runs at `-v` on one scene that reaches each command's detail fields (an
//! addon from a zip, one from a GitHub release, a dependency, a backup, BugGrabber's
//! errors, a desired-state file, a stored plan), goes through the contract's `check()`
//! (so every stderr line validates against its event schema), and is snapshotted as
//! `verbosity_<row>_v`. The human rows are snapshotted as `verbosity_<row>_v_human`,
//! with the tempdir, ids and clock times replaced. The same rows at `-vv` on a twin
//! scene print the same stdout and add only `trace` events on stderr; three scenarios
//! show those events: discovery's routes, the release memo's decisions with their
//! times, and a wait for the global lock while it waits.

use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

use super::common::home::Home;
use super::common::privacy::assert_private_all;
use super::{Scene, support};

/// One characterized command: its name in the snapshot, its argv (without `--json` or a
/// verbosity flag), the exit it ends with, and extra environment variables.
struct Row {
    name: &'static str,
    argv: Vec<String>,
    code: i32,
    env: Vec<(&'static str, String)>,
}

fn row(name: &'static str, argv: &[&str], code: i32) -> Row {
    Row {
        name,
        argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        code,
        env: Vec::new(),
    }
}

fn with_env(mut row: Row, env: &[(&'static str, &str)]) -> Row {
    row.env = env.iter().map(|(k, v)| (*k, (*v).to_owned())).collect();
    row
}

/// The clock `errors` reads, so which errors are new does not move with the day.
const NOW: (&str, &str) = ("OMADDONS_NOW", "2026-10-15T18:30:00Z");
/// The free space backups see, so `backup list` is the same on every disk.
const FREE: (&str, &str) = ("OMADDONS_FS_FREE_MB", "1024");

/// The scene every row runs on: retail with Foo (a zip, recorded as `gh:o/foo`),
/// WeakAuras (a GitHub release), LittleWigs and its dependency BigWigs, an unmanaged
/// `!BugGrabber` with one database of errors, a manual backup, and a desired-state file;
/// Mists Classic empty.
fn scene() -> Scene {
    use super::common::addons::{addon_folder, toc};
    use super::common::buggrabber::{self, BugDb, BugEntry, NOW as BUG_NOW, TimeKind, Writer};
    use omaddons::backup::BackupKind;

    let s = Scene::new();
    s.install_foo();
    s.json(&["install", "weakauras", "-t", "retail"], 0);
    s.dependencies();
    s.json(&["install", "littlewigs", "-t", "retail"], 0);

    let bug_toc = toc("120105", &["## SavedVariables: BugGrabberDB"]);
    addon_folder(
        &s.home.addons("_retail_"),
        "!BugGrabber",
        &[("!BugGrabber.toc", &bug_toc)],
        &[],
    );
    let at = |file: &str, line| buggrabber::stack_era_c("Foo", file, line);
    let db = BugDb::new(Writer::Modern, 10).entry(
        BugEntry::new(
            &format!(
                "{} attempt to index a nil value (field 'x')",
                at("core.lua", 12)
            ),
            10,
            (TimeKind::Unix, BUG_NOW - 60),
        )
        .stack(&[
            format!("{} in function 'Refresh'", at("core.lua", 12)),
            "[C]: in function 'xpcall'".to_owned(),
        ])
        .counter(4),
    );
    let wtf = s.home.client("_retail_").join("WTF");
    buggrabber::write_files(&wtf, &[db.file()]);

    let captured = "2026-10-14T21:15:00Z".parse().unwrap();
    super::common::backups::seed_backup(
        &s.home.client("_retail_"),
        BackupKind::Manual,
        captured,
        2,
    );
    s.home
        .write_desired("[family.mainline]\naddons = [\"weakauras\", \"gh:o/foo\"]\n");
    s
}

/// The `-v`-gated JSON rows, in the order they run: reads and dry runs first, then the
/// one real run (`adopt --apply`), which changes what the others would see.
fn json_rows(s: &Scene) -> Vec<Row> {
    let foo2 = s.zip("foo2");
    vec![
        row("status", &["status"], 0),
        row("list", &["list"], 0),
        row("targets", &["targets"], 0),
        row("history", &["history", "-t", "retail"], 0),
        row("info", &["info", "weakauras", "-t", "retail"], 0),
        row("outdated", &["outdated"], 0),
        with_env(row("backup_list", &["backup", "list"], 0), &[FREE]),
        with_env(row("errors", &["errors", "-t", "retail"], 0), &[NOW]),
        row(
            "install_dry_run",
            &[
                "install",
                "gh:o/foo",
                "--file",
                &foo2,
                "-t",
                "retail",
                "--dry-run",
            ],
            0,
        ),
        row(
            "pin_dry_run",
            &["pin", "weakauras", "-t", "retail", "--dry-run"],
            0,
        ),
        row(
            "update_dry_run",
            &["update", "-t", "retail", "--dry-run"],
            0,
        ),
        row(
            "copy_dry_run",
            &["copy", "--from", "retail", "--to", "classic", "--dry-run"],
            0,
        ),
        row("sync_dry_run", &["sync", "--dry-run"], 0),
        row("undo_dry_run", &["undo", "--dry-run"], 0),
        row("doctor", &["doctor"], 0),
        row(
            "doctor_gc_dry_run",
            &["doctor", "gc", "--before", "30d", "--dry-run"],
            0,
        ),
        row(
            "adopt_apply",
            &["adopt", "--apply", "-t", "retail", "--no-verify"],
            0,
        ),
    ]
}

/// Runs `row` at `verbosity` (`-v` or `-vv`) with `--json`: the checked envelope.
fn run_json(s: &Scene, row: &Row, verbosity: &str) -> support::Checked {
    let mut argv = row.argv.clone();
    argv.push(verbosity.to_owned());
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let env: Vec<(&str, &str)> = row.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    s.json_env(&argv, &env, row.code)
}

/// The human rows: the commands whose human output `-v` changes.
fn human_rows(s: &Scene) -> Vec<Row> {
    let foo2 = s.zip("foo2");
    vec![
        row("list", &["list"], 0),
        row("info", &["info", "weakauras", "-t", "retail"], 0),
        row("outdated", &["outdated"], 0),
        row("history", &["history", "-t", "retail"], 0),
        row(
            "install_dry_run",
            &[
                "install",
                "gh:o/foo",
                "--file",
                &foo2,
                "-t",
                "retail",
                "--dry-run",
            ],
            0,
        ),
    ]
}

/// Runs `row` at `verbosity` in human mode: its stdout, made stable.
fn run_human(s: &Scene, row: &Row, verbosity: &str) -> String {
    let mut argv = row.argv.clone();
    argv.extend([verbosity.to_owned(), "--no-color".to_owned()]);
    let env: Vec<(&str, &str)> = row.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let run = s.home.exec_with(&argv, &env);
    assert_eq!(run.code, row.code, "{}", run.context());
    human_stable(&run.stdout, &s.home.env.root().display().to_string())
}

/// Human text with the tempdir, ids, tokens and clock times replaced.
fn human_stable(text: &str, root: &str) -> String {
    let text = text.replace(root, "<root>");
    text.lines()
        .map(|line| {
            line.split(' ')
                .map(human_word)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One space-separated word, replaced when it is an id, a token, a date or a time.
fn human_word(word: &str) -> String {
    let crockford = |c: char| c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c));
    let trimmed = word.trim_end_matches(['…', ',', ')', ':', '.']);
    let tail = &word[trimmed.len()..];
    let id = trimmed.chars().count() >= 16 && trimmed.chars().all(crockford);
    let token = trimmed.starts_with("pc1:");
    let date = trimmed.len() == 10
        && trimmed.chars().enumerate().all(|(i, c)| match i {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        });
    let clock = trimmed.len() == 5
        && trimmed.chars().enumerate().all(|(i, c)| match i {
            2 => c == ':',
            _ => c.is_ascii_digit(),
        });
    match () {
        () if id => format!("<id>{tail}"),
        () if token => format!("<precondition>{tail}"),
        () if date => format!("<date>{tail}"),
        () if clock => format!("<clock>{tail}"),
        () => word.to_owned(),
    }
}

/// `-v` output, JSON and human, as it was before the verbosity levels: the snapshots
/// the levels must leave byte for byte as they are.
#[test]
fn level_one_output_is_characterized() {
    let s = scene();
    for row in human_rows(&s) {
        let text = run_human(&s, &row, "-v");
        insta::assert_snapshot!(format!("verbosity_{}_v_human", row.name), text);
    }
    for row in json_rows(&s) {
        let checked = run_json(&s, &row, "-v");
        insta::assert_json_snapshot!(
            format!("verbosity_{}_v", row.name),
            s.stable(&checked.envelope)
        );
    }
}

/// The replacements the human snapshots rely on.
#[test]
fn human_text_is_made_stable() {
    let cases = [
        ("root", "<r>/a b", "<root>/a b"),
        ("ulid", "tx 01M51EX645RCVFN6Z4ZV5GRWVS", "tx <id>"),
        (
            "ulid_prefix",
            "tx 01M51EX645RCVFN6… · undo",
            "tx <id>… · undo",
        ),
        ("token", "--if-match pc1:9f3c", "--if-match <precondition>"),
        ("date_time", "(2026-10-21 18:04)", "(2026-10-21 <clock>)"),
        ("date", "2026-10-21 x", "<date> x"),
        ("version", "v12.1.5 3.3.5", "v12.1.5 3.3.5"),
        ("word", "WeakAuras", "WeakAuras"),
    ];
    for (case, input, want) in cases {
        let got = human_stable(input, "<r>");
        assert_eq!(got, want, "{case}: {input:?}");
    }
}

/// The `log` events on a run's NDJSON stderr, as `(level, message)`; each has exactly
/// the keys `event`, `level` and `message`.
fn log_events(stderr: &str) -> Vec<(String, String)> {
    stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["event"] == "log")
        .map(|event| {
            let keys: Vec<&String> = event.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["event", "level", "message"], "a log event: {event}");
            (
                event["level"].as_str().unwrap().to_owned(),
                event["message"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// The messages of the `log` events of `level`.
fn at_level(stderr: &str, level: &str) -> Vec<String> {
    log_events(stderr)
        .into_iter()
        .filter(|(l, _)| l == level)
        .map(|(_, message)| message)
        .collect()
}

/// `-vv` prints on stdout exactly what `-v` prints, in JSON and in human form, row for
/// row on twin scenes, and adds only `trace` events on stderr (which `check()` validates
/// against the `log` event's schema).
#[test]
fn level_two_prints_what_level_one_prints() {
    let (one, two) = (scene(), scene());
    for (a, b) in human_rows(&one).iter().zip(human_rows(&two)) {
        assert_eq!(
            run_human(&one, a, "-v"),
            run_human(&two, &b, "-vv"),
            "{}: human stdout at -v and -vv",
            a.name
        );
    }
    let mut traced = 0;
    for (a, b) in json_rows(&one).iter().zip(json_rows(&two)) {
        let v = run_json(&one, a, "-v");
        let vv = run_json(&two, &b, "-vv");
        // The scene's WTF/ holds BugGrabber's database under a fake account.
        assert_private_all(&vv.run.stdout, &vv.run.stderr);
        assert_eq!(
            one.stable(&v.envelope),
            two.stable(&vv.envelope),
            "{}: --json stdout at -v and -vv",
            a.name
        );
        assert!(
            at_level(&v.run.stderr, "trace").is_empty(),
            "{}: no trace at -v\n{}",
            a.name,
            v.run.stderr
        );
        for (level, _) in log_events(&vv.run.stderr) {
            assert!(
                ["info", "debug", "trace"].contains(&level.as_str()),
                "{}: {level}",
                a.name
            );
        }
        traced += at_level(&vv.run.stderr, "trace").len();
        let other = |stderr: &str| {
            stderr
                .lines()
                .filter(|line| !line.contains("\"level\":\"trace\""))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            other(&v.run.stderr).len(),
            other(&vv.run.stderr).len(),
            "{}: -vv adds trace lines and nothing else\n-v: {}\n-vv: {}",
            a.name,
            v.run.stderr,
            vv.run.stderr
        );
    }
    assert!(traced > 0, "the -vv rows traced something");
}

/// A retail home whose `WTF/` holds SavedVariables under a fake account, realm and
/// character, so [`run`]'s privacy check has names to look for.
fn planted() -> Home {
    let home = Home::new(&["wow"]);
    super::common::wtf::Wtf::at(home.client("_retail_"))
        .saved_variables("Foo", 100)
        .build();
    home
}

/// `omaddons <args> --json --progress json` on `home`, its exit asserted, and checked
/// for account, realm and character names on both streams.
fn run(home: &Home, args: &[&str], code: i32) -> support::Checked {
    let mut argv: Vec<&str> = args.to_vec();
    argv.extend(["--progress", "json"]);
    let checked = support::check(home.run(&argv), code);
    assert_private_all(&checked.run.stdout, &checked.run.stderr);
    checked
}

#[test]
fn status_traces_discovery_at_vv_only() {
    use super::common::trees::tree_hash;

    let home = planted();

    // The first run scans and parses.
    let first = run(&home, &["status", "-vv"], 0);
    let traces = at_level(&first.run.stderr, "trace");
    assert!(
        traces.iter().any(|t| t.starts_with("discovery route ")
            && t.contains("World of Warcraft")
            && t.ends_with("first route to reach it")),
        "a route reached the Battle.net root: {traces:?}"
    );
    assert!(
        traces
            .iter()
            .any(|t| t.starts_with("discovery root ") && t.ends_with(": parsed")),
        "{traces:?}"
    );
    for t in &traces {
        assert!(!t.contains("WTF/"), "a trace names a path below WTF/: {t}");
    }

    // Warm runs write nothing, whatever the verbosity.
    let (state, cache) = (home.env.state(), home.env.cache());
    let before = (tree_hash(&state), tree_hash(&cache));
    let warm = run(&home, &["status", "-vv"], 0);
    assert!(
        at_level(&warm.run.stderr, "trace")
            .iter()
            .any(|t| t.ends_with(": reused from the discovery cache")),
        "{}",
        warm.run.stderr
    );
    let v = run(&home, &["status", "-v"], 0);
    assert!(at_level(&v.run.stderr, "trace").is_empty());
    assert!(
        !at_level(&v.run.stderr, "debug").is_empty(),
        "-v keeps its debug events"
    );
    assert_eq!(
        support::without_meta(&v.run.stdout),
        support::without_meta(&warm.run.stdout),
        "status --json at -v and -vv on one home"
    );
    assert!(
        v.envelope["data"]["targets"][0].get("client_dir").is_some(),
        "-v's detail field"
    );
    let plain = run(&home, &["status"], 0);
    assert!(
        log_events(&plain.run.stderr)
            .iter()
            .all(|(level, _)| level == "info")
    );
    assert!(
        plain.envelope["data"]["targets"][0]
            .get("client_dir")
            .is_none()
    );
    let quiet = run(&home, &["status", "--quiet", "-vv"], 0);
    assert!(
        log_events(&quiet.run.stderr).is_empty(),
        "--quiet drops every level"
    );
    assert_eq!(
        support::without_meta(&quiet.run.stdout),
        support::without_meta(&warm.run.stdout)
    );

    // Two v's together or side by side are level 2; one each side of the command is 1.
    for (case, args, level_two) in [
        ("v_v", &["-v", "-v", "status"][..], true),
        ("long_long", &["--verbose", "--verbose", "status"][..], true),
        ("after", &["status", "-vv"][..], true),
        ("either_side", &["-v", "status", "-v"][..], false),
    ] {
        let r = run(&home, args, 0);
        assert_eq!(
            !at_level(&r.run.stderr, "trace").is_empty(),
            level_two,
            "{case}: {args:?}\n{}",
            r.run.stderr
        );
        assert!(!at_level(&r.run.stderr, "debug").is_empty(), "{case}");
    }

    // No scan under --cached: the cached roots only.
    let cached = run(&home, &["status", "--cached", "-vv"], 0);
    let traces = at_level(&cached.run.stderr, "trace");
    assert!(
        traces
            .iter()
            .any(|t| t.ends_with("reused from the discovery cache"))
            && !traces.iter().any(|t| t.starts_with("discovery route ")),
        "{traces:?}"
    );
    assert_eq!(
        (tree_hash(&state), tree_hash(&cache)),
        before,
        "a warm status writes nothing at any verbosity"
    );
}

/// The age in `fetched <n> s ago`.
fn age(line: &str) -> Option<u64> {
    let (_, after) = line.split_once("fetched ")?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[test]
fn info_traces_each_memo_decision_and_its_timing() {
    use super::common::http::{Asset, gh_release};
    use super::common::memo;
    use super::common::netlog::net_log;

    let home = planted();
    let assets: [Asset<'_>; 1] = [(
        "Foo.zip",
        &[("mainline", 120_105)],
        vec![("Foo/Foo.toc", "## Interface: 120105\n")],
    )];
    gh_release(&home.http, "https://github.com/o/foo", "v1", &assets);
    let memo = "release memo gh:o/foo|stable: ";
    let info = |extra: &[&str]| {
        let mut args = vec!["info", "gh:o/foo"];
        args.extend(extra);
        let r = run(&home, &args, 0);
        // Discovery's lines come first; the memo's are the release lines.
        let traces: Vec<String> = at_level(&r.run.stderr, "trace")
            .into_iter()
            .filter(|t| t.starts_with("release "))
            .collect();
        (r, traces)
    };

    let (_, t) = info(&["-vv"]);
    assert_eq!(t[0], format!("{memo}not remembered, fetching"), "{t:?}");
    assert!(
        t.iter()
            .any(|l| l.starts_with("release fetch: 1 ref from 1 source in ") && l.ends_with(" ms")),
        "{t:?}"
    );
    assert!(
        t.contains(&"release fetch gh:o/foo|stable: newest v1 (1 release), remembered".to_owned()),
        "{t:?}"
    );
    assert!(!net_log(&home).is_empty(), "the fetch reached the network");

    let requests = net_log(&home).len();
    let (fresh, t) = info(&["-vv"]);
    assert!(
        t[0].starts_with(&format!("{memo}fresh, answered from the memo (fetched "))
            && t[0].ends_with(" s ago, window 900 s); no request"),
        "{t:?}"
    );
    assert_eq!(
        net_log(&home).len(),
        requests,
        "a fresh answer asks nothing"
    );
    let (v, t) = info(&["-v"]);
    assert!(t.is_empty(), "no trace at -v: {t:?}");
    assert_eq!(
        support::without_meta(&v.run.stdout),
        support::without_meta(&fresh.run.stdout)
    );

    let (_, t) = info(&["-vv", "--offline"]);
    assert!(
        t[0].starts_with(&format!(
            "{memo}offline, answered from the memo whatever its age (fetched "
        )),
        "{t:?}"
    );
    let (_, t) = info(&["-vv", "--refresh"]);
    assert_eq!(t[0], format!("{memo}--refresh, fetching"), "{t:?}");
    assert!(t.iter().any(|l| l.starts_with("release fetch: ")), "{t:?}");

    // Twenty minutes old, the same tag: one probe confirms it.
    home.seed_memo(&memo::of(vec![memo::entry(
        "gh:o/foo",
        "v1",
        memo::ago(20),
    )]));
    let (_, t) = info(&["-vv"]);
    assert!(
        t[0].starts_with(&format!("{memo}stale (fetched "))
            && age(&t[0]).is_some_and(|seconds| seconds >= 1200),
        "{t:?}"
    );
    assert!(
        t[1].starts_with(
            "release probe gh:o/foo|stable: token unchanged, kept the remembered set ("
        ) && t[1].ends_with(" ms)"),
        "{t:?}"
    );

    // A moved tag: the probe sees it, and the release is fetched.
    home.seed_memo(&memo::of(vec![memo::entry(
        "gh:o/foo",
        "v0",
        memo::ago(20),
    )]));
    let (_, t) = info(&["-vv"]);
    assert!(
        t[1].starts_with("release probe gh:o/foo|stable: token changed, fetching ("),
        "{t:?}"
    );
    assert!(
        t.contains(&"release fetch gh:o/foo|stable: newest v1 (1 release), remembered".to_owned()),
        "{t:?}"
    );
}

#[test]
fn a_lock_wait_is_traced_while_it_waits() {
    use super::common::pause::Pause;

    let home = planted();
    let r = run(
        &home,
        &["config", "set", "check_interval_hours", "6", "-vv"],
        0,
    );
    assert_eq!(
        at_level(&r.run.stderr, "trace"),
        ["global lock: acquired at once"]
    );
    let r = run(
        &home,
        &["config", "set", "check_interval_hours", "6", "-v"],
        0,
    );
    assert!(at_level(&r.run.stderr, "trace").is_empty());

    // The holder stops with the lock held; the waiter must say so while it waits.
    let pause = Pause::new(home.env.root());
    let point = "fsx.lock.holder_written";
    let failpoints = format!("{point}=pause");
    let holder = home.spawn(
        &["config", "set", "check_interval_hours", "12"],
        &[("OMADDONS_FAILPOINTS", &failpoints), pause.env()],
    );
    pause.reached(point);
    let mut waiter = home.spawn(&["config", "set", "check_interval_hours", "24", "-vv"], &[]);
    let stderr = waiter.stderr.take().unwrap();
    let (lines, arrived) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let waiting = format!(
        "global lock: held by omaddons config set (pid {}, since ",
        holder.id()
    );
    let mut seen = Vec::new();
    while !seen.iter().any(|t: &String| t.starts_with(&waiting)) {
        let line = arrived
            .recv_timeout(Duration::from_secs(4))
            .unwrap_or_else(|_| panic!("no wait traced within 4 s: {seen:?}"));
        seen.extend(at_level(&line, "trace"));
    }
    assert!(
        seen.last().unwrap().ends_with("; waiting up to 5000 ms"),
        "{seen:?}"
    );
    pause.resume(point);
    let held = super::common::home::wait(holder, &["config", "set"]);
    assert_eq!(held.code, 0, "{}", held.context());
    let status = waiter.wait().unwrap();
    reader.join().unwrap();
    seen.extend(arrived.try_iter().flat_map(|line| at_level(&line, "trace")));
    assert_eq!(status.code(), Some(0), "{seen:?}");
    let acquired = seen
        .iter()
        .position(|t| t.starts_with("global lock: acquired after "))
        .unwrap_or_else(|| panic!("no acquired-after line: {seen:?}"));
    let waited = seen.iter().position(|t| t.starts_with(&waiting)).unwrap();
    assert!(acquired > waited, "the wait, then its end: {seen:?}");
    let got = run(&home, &["config", "get", "check_interval_hours"], 0);
    assert_eq!(got.envelope["data"]["value"], 24, "the waiter's value");
}
