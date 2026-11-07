//! Verbosity levels (M4-D17): what `-v` prints, characterized.
//!
//! Every row runs at `-v` on one scene that reaches each command's detail fields (an
//! addon from a zip, one from a GitHub release, a dependency, a backup, BugGrabber's
//! errors, a desired-state file, a stored plan), goes through the contract's `check()`
//! (so every stderr line validates against its event schema), and is snapshotted as
//! `verbosity_<row>_v`. The human rows are snapshotted as `verbosity_<row>_v_human`,
//! with the tempdir, ids and clock times replaced.

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
