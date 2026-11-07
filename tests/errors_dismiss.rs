//! `omaddons errors dismiss`: the durable acknowledgement (M3-D19). A dismissal stops
//! the errors read being new, survives a cache wipe, never covers a session the user was
//! not shown (`--if-match`), and never hides an error that happened after it, even after
//! a reset; it is a settings line in history, never undone.

use crate::common;

use std::fs;

use common::buggrabber::{self, NOW};
use common::home::{Home, Run, wait};
use common::pause::Pause;
use common::privacy::{ACCOUNT2_NAME, assert_private, assert_private_all};
use serde_json::{Value, json};
use support::{Client, cut, db, dismiss_while, warning_codes};

#[cfg(test)]
mod support {
    use serde_json::Value;

    use super::common::addons::{addon_folder, toc};
    use super::common::buggrabber::{self, BugDb, BugEntry, SvFile, TimeKind, Writer, write_files};
    use super::common::home::{Home, Run, wait};
    use super::common::pause::Pause;

    /// The frozen clock (`buggrabber::NOW`): a dismissal is made then.
    pub const CLOCK: &str = "2026-10-15T18:30:00Z";

    /// A database at `session`, with one error per `(session, unix time)`.
    pub fn db(session: u32, errors: &[(u32, i64)]) -> BugDb {
        errors
            .iter()
            .fold(BugDb::new(Writer::Modern, session), |db, (s, at)| {
                db.entry(BugEntry::new(
                    &format!(
                        "{} attempt to call a nil value",
                        buggrabber::stack_era_c("Foo", "a.lua", *s)
                    ),
                    *s,
                    (TimeKind::Unix, *at),
                ))
            })
    }

    /// One client of a home, with BugGrabber installed and Foo in its AddOns.
    pub struct Client<'h> {
        pub home: &'h Home,
        pub folder: &'static str,
    }

    impl<'h> Client<'h> {
        pub fn new(home: &'h Home, folder: &'static str) -> Self {
            let addons = home.addons(folder);
            let bug_toc = toc("120105, 50504", &["## SavedVariables: BugGrabberDB"]);
            addon_folder(
                &addons,
                "!BugGrabber",
                &[("!BugGrabber.toc", &bug_toc)],
                &[],
            );
            addon_folder(
                &addons,
                "Foo",
                &[("Foo.toc", &toc("120105, 50504", &[]))],
                &[],
            );
            Self { home, folder }
        }

        pub fn write(&self, db: &BugDb) {
            write_files(&self.home.client(self.folder).join("WTF"), &[db.file()]);
        }
    }

    /// `omaddons <args> --json --progress json` with the clock frozen; private on both
    /// streams.
    pub fn run(home: &Home, args: &[&str]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let run = home.exec_with(&argv, &[("OMADDONS_NOW", CLOCK)]);
        super::assert_private_all(&run.stdout, &run.stderr);
        run
    }

    /// The new local errors `errors -t <id>` counts.
    pub fn new_errors(home: &Home, id: &str) -> u64 {
        let envelope = run(home, &["errors", "-t", id]).expect(0);
        envelope["data"]["targets"][0]["summary"]["new"]
            .as_u64()
            .unwrap()
    }

    /// The first target of an envelope.
    pub fn first(envelope: &Value) -> &Value {
        &envelope["data"]["targets"][0]
    }

    /// The `dismiss.precondition` of an `errors -t <id>` read.
    pub fn token(home: &Home, id: &str) -> String {
        let read = run(home, &["errors", "-t", id]).expect(0);
        first(&read)["dismiss"]["precondition"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// `db`'s file under `account`, its end cut off, as a file the game is still writing
    /// reads.
    pub fn cut(account: &str, db: &BugDb) -> SvFile {
        let mut file = SvFile::account(account, buggrabber::FILE, db.render());
        let at = file.bytes.len() - 20;
        file.bytes.truncate(at);
        file
    }

    /// The codes of an envelope's warnings, in order.
    pub fn warning_codes(envelope: &Value) -> Vec<&str> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w["code"].as_str())
            .collect()
    }

    /// `errors dismiss <args>` with the clock frozen, stopped after its first read (and
    /// its `--if-match` check) until `meanwhile` has run, before it takes the lock;
    /// private on both streams.
    pub fn dismiss_while(home: &Home, args: &[&str], meanwhile: impl FnOnce()) -> Run {
        const STEP: &str = "errors.dismiss.observed";
        let pause = Pause::new(home.env.root());
        let argv: Vec<&str> = ["errors", "dismiss"].iter().chain(args).copied().collect();
        let failpoints = format!("{STEP}=pause");
        let child = home.spawn(
            &argv,
            &[
                ("OMADDONS_FAILPOINTS", failpoints.as_str()),
                pause.env(),
                ("OMADDONS_NOW", CLOCK),
            ],
        );
        pause.reached(STEP);
        meanwhile();
        pause.resume(STEP);
        let run = wait(child, &argv);
        super::assert_private_all(&run.stdout, &run.stderr);
        run
    }
}

fn retail(home: &Home) -> Client<'_> {
    Client::new(home, "_retail_")
}

fn ack_file(home: &Home) -> std::path::PathBuf {
    home.state().join("errors_ack.json")
}

fn history_lines(home: &Home) -> Vec<Value> {
    fs::read_to_string(home.state().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
fn a_dismissal_clears_new_records_itself_and_survives_a_cache_wipe() {
    let home = Home::new(&["wow"]);
    retail(&home).write(&db(10, &[(9, NOW - 120), (10, NOW - 60)]));
    assert_eq!(support::new_errors(&home, "retail"), 1);
    let bell = home.env.cache().join("omaddons/doorbell.json");
    let _ = fs::remove_file(&bell);
    let run = support::run(
        &home,
        &["errors", "dismiss", "-t", "retail", "--reason", "seen them"],
    );
    let envelope = run.expect(0);
    let t = support::first(&envelope);
    assert_eq!(
        (&t["acknowledged"], &t["skipped"], &t["errors"]),
        (
            &json!([{"database": "db-1", "session": 10}]),
            &json!([]),
            &json!(1)
        ),
        "{}",
        run.context()
    );
    assert!(bell.exists(), "the doorbell rang");
    assert_eq!(support::new_errors(&home, "retail"), 0);
    let read = support::run(&home, &["errors", "-t", "retail"]).expect(0);
    assert_eq!(support::first(&read)["databases"][0]["acknowledged"], 10);
    assert!(support::first(&read)["dismissed"].is_string());
    assert!(
        !read["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["action"] == "dismiss_errors"),
        "nothing new, nothing to dismiss"
    );

    // One settings line, counts only.
    let lines = history_lines(&home);
    let dismiss: Vec<&Value> = lines.iter().filter(|l| l["kind"] == "dismiss").collect();
    assert_eq!(dismiss.len(), 1);
    assert_eq!(
        dismiss[0]["dismissed"],
        json!({"databases": 1, "errors": 1})
    );
    assert_eq!(dismiss[0]["reason"], "seen them");
    let text = dismiss[0].to_string();
    assert!(
        !text.contains("attempt to") && !text.contains("WTF"),
        "{text}"
    );
    let ack = fs::read_to_string(ack_file(&home)).unwrap();
    assert_private(&ack);
    assert!(
        !ack.contains("WTF") && !ack.contains("SavedVariables"),
        "{ack}"
    );

    // A cache wipe changes nothing: the dismissal is in the state directory.
    fs::remove_dir_all(home.env.cache().join("omaddons")).unwrap();
    assert_eq!(support::new_errors(&home, "retail"), 0);

    let human = home.exec(&["history".to_owned()]);
    assert!(
        human.stdout.contains("dismiss") && human.stdout.contains("1 error in 1 database"),
        "{}",
        human.stdout
    );
}

#[test]
fn the_token_binds_a_dismissal_to_what_was_read() {
    let home = Home::new(&["wow"]);
    let c = retail(&home);
    c.write(&db(10, &[(10, NOW - 60)]));
    let read = support::run(&home, &["errors", "-t", "retail"]).expect(0);
    let token = support::first(&read)["dismiss"]["precondition"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(support::first(&read)["dismiss"]["databases"], 1);
    let hint = read["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["action"] == "dismiss_errors")
        .cloned()
        .unwrap();
    assert_eq!(
        hint["argv"],
        json!(["omaddons", "errors", "dismiss", "-t", "retail"])
    );
    // A session ended (the game wrote the file) between the read and the dismissal.
    c.write(&db(11, &[(10, NOW - 60), (11, NOW - 30)]));
    let stale = support::run(
        &home,
        &["errors", "dismiss", "-t", "retail", "--if-match", &token],
    );
    let refused = stale.expect(1);
    let error = &refused["error"];
    assert_eq!(error["code"], "plan_stale", "{}", stale.context());
    assert_eq!(error["retryable"], false);
    assert!(
        error["details"]["changed"]
            .as_array()
            .unwrap()
            .contains(&json!("observed"))
    );
    assert_eq!(
        error["fix_argv"],
        json!(["omaddons", "errors", "dismiss", "-t", "retail", "--dry-run"])
    );
    assert!(!ack_file(&home).exists(), "nothing acknowledged");
    // A dry run given the stale token previews afresh: the current token, nothing
    // written, no doorbell.
    let bell = home.env.cache().join("omaddons/doorbell.json");
    let rang = fs::read(&bell).ok();
    let preview = support::run(
        &home,
        &[
            "errors",
            "dismiss",
            "-t",
            "retail",
            "--dry-run",
            "--if-match",
            &token,
        ],
    );
    let previewed = preview.expect(0);
    assert!(!ack_file(&home).exists(), "nothing acknowledged");
    assert_eq!(fs::read(&bell).ok(), rang, "no ring");
    assert_eq!(
        previewed["data"]["precondition"].as_str(),
        Some(support::token(&home, "retail").as_str()),
        "{}",
        preview.context()
    );
    assert_ne!(
        previewed["data"]["precondition"].as_str(),
        Some(token.as_str())
    );
    // Bound to a fresh read, it acknowledges exactly that.
    let token = support::token(&home, "retail");
    let bound = support::run(
        &home,
        &["errors", "dismiss", "-t", "retail", "--if-match", &token],
    )
    .expect(0);
    assert_eq!(
        support::first(&bound)["acknowledged"],
        json!([{"database": "db-1", "session": 11}])
    );
    // The hint the read gave runs.
    let argv: Vec<String> = serde_json::from_value(hint["argv"].clone()).unwrap();
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    support::run(&home, &args).expect(0);
}

#[test]
fn an_install_between_the_read_and_the_dismissal_leaves_the_token_good() {
    let home = Home::new(&["wow"]);
    retail(&home).write(&db(10, &[(10, NOW - 60)]));
    let dir = home.env.root().join("src/Bar");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Bar.toc"), common::addons::toc("120105", &[])).unwrap();
    let path = dir.display().to_string();
    home.run(&["install", &path, "-t", "retail"]).expect(0);
    let token = support::token(&home, "retail");
    // The target's generation moves; what the read showed does not.
    home.run(&["remove", "Bar", "-t", "retail"]).expect(0);
    let bound = support::run(
        &home,
        &["errors", "dismiss", "-t", "retail", "--if-match", &token],
    )
    .expect(0);
    assert_eq!(
        support::first(&bound)["acknowledged"],
        json!([{"database": "db-1", "session": 10}])
    );
}

#[test]
fn a_token_for_one_client_never_dismisses_another_set() {
    let home = Home::new(&["wow", "wow_classic"]);
    retail(&home).write(&db(5, &[(5, NOW - 60)]));
    Client::new(&home, "_classic_").write(&db(7, &[(7, NOW - 60)]));
    let token = support::token(&home, "retail");
    let refused = support::run(&home, &["errors", "dismiss", "--if-match", &token]).expect(1);
    assert_eq!(refused["error"]["code"], "plan_stale");
    assert!(
        !ack_file(&home).exists(),
        "refused before anything is written"
    );
}

#[test]
fn errors_after_a_dismissal_are_new_even_after_a_reset() {
    let home = Home::new(&["wow"]);
    let c = retail(&home);
    c.write(&db(48, &[(47, NOW - 120), (48, NOW - 60)]));
    support::run(&home, &["errors", "dismiss", "-t", "retail"]).expect(0);
    assert_eq!(support::new_errors(&home, "retail"), 0);
    // A recurrence moves its session forward: that later session is new.
    c.write(&db(49, &[(47, NOW - 120), (49, NOW + 60)]));
    assert_eq!(support::new_errors(&home, "retail"), 1);
    // `/bugsack reset`: the session restarts at 1; what happened since is new.
    c.write(&db(1, &[(1, NOW + 120)]));
    assert_eq!(support::new_errors(&home, "retail"), 1);
    // Past 48 again: the stale acknowledgement hides nothing after the dismissal.
    c.write(&db(50, &[(2, NOW + 150), (50, NOW + 180)]));
    assert_eq!(support::new_errors(&home, "retail"), 2);
}

#[test]
fn a_cache_wipe_keeps_exactly_the_errors_since_the_dismissal_new() {
    let home = Home::new(&["wow"]);
    let c = retail(&home);
    c.write(&db(10, &[(9, NOW - 120), (10, NOW - 60)]));
    support::run(&home, &["errors", "dismiss", "-t", "retail"]).expect(0);
    c.write(&db(
        12,
        &[
            (9, NOW - 120),
            (10, NOW - 60),
            (11, NOW + 60),
            (12, NOW + 120),
        ],
    ));
    fs::remove_dir_all(home.env.cache().join("omaddons")).unwrap();
    assert_eq!(support::new_errors(&home, "retail"), 2);
}

#[test]
fn two_dismissers_take_turns_and_both_are_kept() {
    let home = Home::new(&["wow", "wow_classic"]);
    retail(&home).write(&db(5, &[(5, NOW - 60)]));
    Client::new(&home, "_classic_").write(&db(7, &[(7, NOW - 60)]));
    let pause = Pause::new(home.env.root());
    let first_argv = ["errors", "dismiss", "-t", "retail"];
    let first = home.spawn(
        &first_argv,
        &[
            ("OMADDONS_FAILPOINTS", "fsx.lock.acquired=pause"),
            pause.env(),
            ("OMADDONS_NOW", support::CLOCK),
        ],
    );
    pause.reached("fsx.lock.acquired");
    let second_argv = ["errors", "dismiss", "-t", "classic"];
    let second = home.spawn(&second_argv, &[("OMADDONS_NOW", support::CLOCK)]);
    std::thread::sleep(std::time::Duration::from_millis(300));
    pause.resume("fsx.lock.acquired");
    let (a, b): (Run, Run) = (wait(first, &first_argv), wait(second, &second_argv));
    a.expect(0);
    b.expect(0);
    let ack: Value = serde_json::from_slice(&fs::read(ack_file(&home)).unwrap()).unwrap();
    assert_eq!(ack["targets"].as_object().unwrap().len(), 2, "{ack}");
    let dismissals = history_lines(&home)
        .iter()
        .filter(|l| l["kind"] == "dismiss")
        .count();
    assert_eq!(dismissals, 2);
}

#[test]
fn a_database_read_in_part_is_never_acknowledged() {
    let home = Home::new(&["wow"]);
    retail(&home);
    let file = cut("FAKEACCT", &db(10, &[(10, NOW - 60)]));
    buggrabber::write_files(&home.client("_retail_").join("WTF"), &[file]);
    let run = support::run(&home, &["errors", "dismiss", "-t", "retail"]);
    let envelope = run.expect(0);
    assert_eq!(envelope["partial"], true, "{}", run.context());
    assert_eq!(
        support::first(&envelope)["skipped"],
        json!([{"database": "db-1", "reason": "partial"}])
    );
    assert!(!ack_file(&home).exists());
    assert!(history_lines(&home).iter().all(|l| l["kind"] != "dismiss"));
}

/// A session that ends while a dismissal waits for the lock is read again under it: a
/// token from the first read no longer matches, and without one the dismissal binds to
/// the second read, reporting each of its warnings (a second account's database the
/// game is still writing) once.
#[test]
fn a_session_ending_while_a_dismissal_waits_is_read_again() {
    let home = Home::new(&["wow"]);
    let c = retail(&home);
    c.write(&db(10, &[(10, NOW - 60)]));
    let wtf = home.client("_retail_").join("WTF");
    buggrabber::write_files(&wtf, &[cut(ACCOUNT2_NAME, &db(3, &[(3, NOW - 90)]))]);
    let token = support::token(&home, "retail");

    let stale = dismiss_while(&home, &["-t", "retail", "--if-match", &token], || {
        c.write(&db(11, &[(10, NOW - 60), (11, NOW - 30)]));
    });
    let error = &stale.expect(1)["error"];
    assert_eq!(error["code"], "plan_stale", "{}", stale.context());
    assert!(
        error["details"]["changed"]
            .as_array()
            .unwrap()
            .contains(&json!("observed")),
        "{}",
        stale.context()
    );
    assert!(!ack_file(&home).exists(), "nothing acknowledged");

    let run = dismiss_while(&home, &["-t", "retail"], || {
        c.write(&db(12, &[(10, NOW - 60), (11, NOW - 30), (12, NOW - 10)]));
    });
    let envelope = run.expect(0);
    let t = support::first(&envelope);
    // Sessions 10 to 12 are new since the read that took the token: three errors, where
    // the first look (session 11) had two.
    assert_eq!(
        (
            &envelope["partial"],
            &t["acknowledged"],
            &t["skipped"],
            &t["errors"]
        ),
        (
            &json!(true),
            &json!([{"database": "db-1", "session": 12}]),
            &json!([{"database": "db-2", "reason": "partial"}]),
            &json!(3)
        ),
        "{}",
        run.context()
    );
    let codes = warning_codes(&envelope);
    let read = codes.iter().filter(|c| **c == "errors_partial").count();
    assert_eq!(read, 1, "{}", run.context());
}

/// A database the game was still writing at a dismissal's first look, and finished before
/// the lock, is acknowledged whole, and the first look's warning about it is not
/// reported: a dismissal reports the read it binds to.
#[test]
fn a_database_finished_while_a_dismissal_waits_is_acknowledged_without_a_stale_warning() {
    let home = Home::new(&["wow"]);
    let c = retail(&home);
    let whole = db(10, &[(10, NOW - 60)]);
    let wtf = home.client("_retail_").join("WTF");
    buggrabber::write_files(&wtf, &[cut("FAKEACCT", &whole)]);

    let run = dismiss_while(&home, &["-t", "retail"], || c.write(&whole));
    let envelope = run.expect(0);
    let t = support::first(&envelope);
    assert_eq!(
        (&envelope["partial"], &t["acknowledged"], &t["skipped"]),
        (
            &json!(false),
            &json!([{"database": "db-1", "session": 10}]),
            &json!([])
        ),
        "{}",
        run.context()
    );
    assert_eq!(
        warning_codes(&envelope),
        Vec::<&str>::new(),
        "{}",
        run.context()
    );
    assert_eq!(support::new_errors(&home, "retail"), 0);
}

#[test]
fn a_dry_run_writes_nothing_and_returns_its_token() {
    let home = Home::new(&["wow"]);
    retail(&home).write(&db(10, &[(10, NOW - 60)]));
    support::run(&home, &["errors"]).expect(0);
    let hash = |dir: std::path::PathBuf| {
        if dir.exists() {
            common::trees::tree_hash(&dir)
        } else {
            String::new()
        }
    };
    let before = (hash(home.state()), hash(home.env.cache().join("omaddons")));
    let dry = support::run(&home, &["errors", "dismiss", "-t", "retail", "--dry-run"]).expect(0);
    assert!(
        dry["data"]["precondition"]
            .as_str()
            .is_some_and(|t| t.starts_with("pc1:"))
    );
    assert_eq!(dry["data"]["dry_run"], true);
    assert_eq!(
        (hash(home.state()), hash(home.env.cache().join("omaddons"))),
        before
    );
    // The dry run's token binds the run that follows, as the read's does.
    let token = dry["data"]["precondition"].as_str().unwrap().to_owned();
    assert_eq!(token, support::token(&home, "retail"));
    support::run(
        &home,
        &["errors", "dismiss", "-t", "retail", "--if-match", &token],
    )
    .expect(0);
}

#[test]
fn a_corrupt_acknowledgement_file_stops_only_the_dismissal() {
    let home = Home::new(&["wow"]);
    retail(&home).write(&db(10, &[(10, NOW - 60)]));
    fs::create_dir_all(home.state()).unwrap();
    fs::write(ack_file(&home), b"{ not json").unwrap();
    let refused = support::run(&home, &["errors", "dismiss", "-t", "retail"]).expect(1);
    assert_eq!(refused["error"]["code"], "state_corrupt");
    assert_eq!(refused["error"]["fix_argv"], Value::Null);
    assert!(
        !refused["error"]["fix"]
            .as_str()
            .unwrap()
            .contains("omaddons"),
        "prose names no command"
    );
    assert_eq!(fs::read(ack_file(&home)).unwrap(), b"{ not json");
    assert_eq!(
        support::new_errors(&home, "retail"),
        1,
        "errors still reads"
    );
}

#[test]
fn undo_steps_over_a_dismissal() {
    let home = Home::new(&["wow"]);
    retail(&home).write(&db(10, &[(10, NOW - 60)]));
    let dir = home.env.root().join("src/Bar");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Bar.toc"), common::addons::toc("120105", &[])).unwrap();
    let path = dir.display().to_string();
    home.run(&["install", &path, "-t", "retail"]).expect(0);
    support::run(&home, &["errors", "dismiss", "-t", "retail"]).expect(0);
    let undone = home.run(&["undo"]).expect(0);
    assert_eq!(undone["data"]["reverted"]["kind"], "install");
    assert!(!home.addons("_retail_").join("Bar").exists());
}
