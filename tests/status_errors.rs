//! Lua errors in `omaddons status` (M3-D19, M3-D20): `--cached` recounts what `errors`
//! last read without opening a BugGrabber file, and says `stale` or `unchecked` when that
//! read no longer stands; plain `status` reads the files live. Both decide `new` at the
//! time of the call (errors age out, dismissals hold with the cache emptied), never count
//! received reports, and say when a read was incomplete instead of reporting zero.

use crate::common;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use common::buggrabber::{NOW, SvFile, write_files};
use common::home::Home;
use common::privacy::assert_private_all;
use serde_json::{Value, json};
use support::{actions, block, db, error, install, plain, read, received, run, sv_dir, write};

#[cfg(test)]
mod support {
    use super::common::addons::{addon_folder, toc};
    use super::common::buggrabber::{self, BugDb, BugEntry, TimeKind, Writer, write_files};
    use super::common::home::{Home, Run};
    use super::common::privacy::SENDER;
    use super::*;

    /// The frozen clock (`buggrabber::NOW`).
    pub const CLOCK: &str = "2026-10-15T18:30:00Z";

    /// BugGrabber, Foo and Bar in retail's AddOns.
    pub fn install(home: &Home) {
        let addons = home.addons("_retail_");
        let bug_toc = toc("120105", &["## SavedVariables: BugGrabberDB"]);
        addon_folder(
            &addons,
            "!BugGrabber",
            &[("!BugGrabber.toc", &bug_toc)],
            &[],
        );
        for name in ["Foo", "Bar"] {
            let file = format!("{name}.toc");
            addon_folder(&addons, name, &[(file.as_str(), &toc("120105", &[]))], &[]);
        }
    }

    /// An error of `folder` in `session`, last seen at `at` (unix).
    pub fn error(folder: &str, session: u32, at: i64) -> BugEntry {
        BugEntry::new(
            &format!(
                "{} attempt to call a nil value",
                buggrabber::stack_era_c(folder, "core.lua", session)
            ),
            session,
            (TimeKind::Unix, at),
        )
    }

    /// The same error, sent by another player.
    pub fn received(folder: &str, session: u32, at: i64) -> BugEntry {
        error(folder, session, at).source(SENDER)
    }

    /// A database at `session` holding `entries`.
    pub fn db(session: u32, entries: Vec<BugEntry>) -> BugDb {
        entries
            .into_iter()
            .fold(BugDb::new(Writer::Modern, session), BugDb::entry)
    }

    pub fn sv_dir(home: &Home) -> PathBuf {
        home.client("_retail_")
            .join("WTF/Account/FAKEACCT/SavedVariables")
    }

    /// Writes `db` as FAKEACCT's `!BugGrabber.lua` (a logout).
    pub fn write(home: &Home, db: &BugDb) {
        write_files(&home.client("_retail_").join("WTF"), &[db.file()]);
    }

    /// `omaddons <args> --json --progress json`, the clock frozen at `clock`; private
    /// on both streams.
    pub fn run(home: &Home, args: &[&str], clock: &str) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let run = home.exec_with(&argv, &[("OMADDONS_NOW", clock)]);
        assert_private_all(&run.stdout, &run.stderr);
        run
    }

    /// `errors -t retail` at the frozen clock: its target.
    pub fn read(home: &Home) -> Value {
        run(home, &["errors", "-t", "retail"], CLOCK).expect(0)["data"]["targets"][0].clone()
    }

    /// `status` (plain) at the frozen clock.
    pub fn plain(home: &Home) -> Value {
        run(home, &["status"], CLOCK).expect(0)
    }

    /// Retail's errors block.
    pub fn block(status: &Value) -> &Value {
        let retail = status["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == "retail")
            .unwrap();
        &retail["errors"]
    }

    pub fn actions(status: &Value) -> Vec<String> {
        status["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["action"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// `status --cached` at `clock`.
fn cached_at(home: &Home, clock: &str) -> Value {
    run(home, &["status", "--cached"], clock).expect(0)
}

fn cached(home: &Home) -> Value {
    cached_at(home, support::CLOCK)
}

/// `(state, new, total, addons, received)` of retail's block.
fn counts(status: &Value) -> (Value, Value, Value, Value, Value) {
    let e = block(status);
    (
        e["state"].clone(),
        e["new"].clone(),
        e["total"].clone(),
        e["addons"].clone(),
        e["received"].clone(),
    )
}

/// Session 10's database: Foo threw in sessions 9 and 10, Bar in 10, and another player
/// sent one of Bar's.
fn session_10() -> common::buggrabber::BugDb {
    db(
        10,
        vec![
            error("Foo", 9, NOW - 86_400),
            error("Foo", 10, NOW - 120).counter(3),
            error("Bar", 10, NOW - 60),
            received("Bar", 10, NOW - 30),
        ],
    )
}

#[test]
fn cached_status_counts_what_errors_read_and_plain_status_reads_it_live() {
    let home = Home::new(&["wow"]);
    install(&home);
    write(&home, &session_10());

    // Never read: a live read counts (and warms the discovery cache `--cached` needs);
    // the cached view knows nothing, so `errors` is recommended.
    let live_before = plain(&home);
    let before = cached(&home);
    assert_eq!(
        counts(&before),
        (json!("unchecked"), json!(0), json!(0), json!(0), json!(0)),
        "{before:#}"
    );
    assert_eq!(block(&before)["capture"], "buggrabber");
    assert!(actions(&before).contains(&"check_errors".to_owned()));
    let hint = before["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["action"] == "check_errors")
        .unwrap();
    assert_eq!(hint["argv"], json!(["omaddons", "errors"]));
    assert_eq!(before["data"]["summary"]["state"], "idle");

    let target = read(&home);
    let summary = &target["summary"];
    let expected = (
        json!("ok"),
        summary["new"].clone(),
        summary["entries"].clone(),
        summary["addons"].clone(),
        summary["received"].clone(),
    );
    assert_eq!(
        expected,
        (json!("ok"), json!(2), json!(3), json!(2), json!(1))
    );
    for status in [cached(&home), plain(&home), live_before] {
        assert_eq!(counts(&status), expected, "{status:#}");
        let e = block(&status);
        assert_eq!(e["coverage"], "complete");
        assert!(e["last_session_at"].is_string(), "{e}");
        assert_eq!(e["dismissed_at"], Value::Null);
        // Per addon, `errors`' own new counts and occurrences, in its order.
        let addons: Vec<Value> = target["addons"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["origin"] == "local")
            .map(|a| json!([a["addon"], a["folders"][0], a["new"], a["count"]]))
            .collect();
        let top: Vec<Value> = e["top"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| json!([a["addon"], a["folder"], a["new"], a["count"]]))
            .collect();
        assert_eq!(top, addons, "{status:#}");
        assert_eq!(
            top,
            [json!(["Foo", "Foo", 1, 4]), json!(["Bar", "Bar", 1, 1])]
        );
        assert_eq!(
            status["data"]["summary"]["errors"],
            json!({"new": 2, "targets": ["retail"]})
        );
        assert_eq!(status["data"]["summary"]["state"], "errors");
        assert!(!actions(&status).contains(&"check_errors".to_owned()));
    }
}

#[test]
fn a_logout_makes_cached_stale_until_errors_reads_again() {
    let home = Home::new(&["wow"]);
    install(&home);
    write(&home, &session_10());
    read(&home);
    // The game logs out after session 11, in which Foo threw again.
    let mut next = session_10();
    next.session = 11;
    let next = next.entry(error("Foo", 11, NOW - 10));
    write(&home, &next);

    let stale = cached(&home);
    assert_eq!(
        counts(&stale),
        (json!("stale"), json!(2), json!(3), json!(2), json!(1)),
        "the last read's counts, said to be stale: {stale:#}"
    );
    assert!(actions(&stale).contains(&"check_errors".to_owned()));
    let live = plain(&home);
    assert_eq!(
        counts(&live),
        (json!("ok"), json!(3), json!(4), json!(2), json!(1)),
        "{live:#}"
    );
    assert!(!actions(&live).contains(&"check_errors".to_owned()));

    let target = read(&home);
    assert_eq!(target["summary"]["new"], 3);
    let again = cached(&home);
    assert_eq!(counts(&again), counts(&live), "{again:#}");
}

#[test]
fn a_new_file_in_saved_variables_is_stale_once() {
    let home = Home::new(&["wow"]);
    install(&home);
    write(&home, &session_10());
    read(&home);
    assert_eq!(block(&cached(&home))["state"], "ok");
    // A new account file (one that holds no BugGrabberDB) changes the folder's names.
    fs::write(sv_dir(&home).join("Details.lua"), "Details = {}\n").unwrap();
    assert_eq!(block(&cached(&home))["state"], "stale");
    read(&home);
    assert_eq!(block(&cached(&home))["state"], "ok", "converged");
    // So does a new account.
    let other = SvFile::account(
        "FAKEACCT2",
        common::buggrabber::FILE,
        db(3, vec![error("Bar", 3, NOW - 5)]).render(),
    );
    write_files(&home.client("_retail_").join("WTF"), &[other]);
    let stale = cached(&home);
    assert_eq!(block(&stale)["state"], "stale", "{stale:#}");
    read(&home);
    let ok = cached(&home);
    assert_eq!(
        counts(&ok),
        (json!("ok"), json!(3), json!(4), json!(2), json!(1)),
        "{ok:#}"
    );
}

#[test]
fn new_errors_age_out_without_a_read() {
    let home = Home::new(&["wow"]);
    install(&home);
    write(&home, &session_10());
    // The client last wrote the file six days before the clock.
    let six_days = SystemTime::UNIX_EPOCH + Duration::from_secs((NOW - 6 * 86_400) as u64);
    fs::File::options()
        .write(true)
        .open(sv_dir(&home).join("!BugGrabber.lua"))
        .unwrap()
        .set_modified(six_days)
        .unwrap();
    assert_eq!(read(&home)["summary"]["new"], 2);
    assert_eq!(block(&cached(&home))["new"], 2);
    // Two days later, nothing on disk changed and nothing ran: no error is new.
    let later = "2026-10-17T18:30:00Z";
    for status in [
        cached_at(&home, later),
        run(&home, &["status"], later).expect(0),
    ] {
        assert_eq!(
            counts(&status),
            (json!("ok"), json!(0), json!(3), json!(2), json!(1)),
            "{status:#}"
        );
        assert_eq!(status["data"]["summary"]["state"], "idle");
        assert_eq!(
            status["data"]["summary"]["errors"],
            json!({"new": 0, "targets": []})
        );
    }
}

#[test]
fn a_dismissal_holds_with_the_cache_emptied() {
    let home = Home::new(&["wow"]);
    install(&home);
    write(&home, &session_10());
    read(&home);
    run(
        &home,
        &["errors", "dismiss", "-t", "retail"],
        support::CLOCK,
    )
    .expect(0);
    let dismissed = cached(&home);
    assert_eq!(
        counts(&dismissed),
        (json!("ok"), json!(0), json!(3), json!(2), json!(1)),
        "{dismissed:#}"
    );
    assert!(block(&dismissed)["dismissed_at"].is_string());
    assert_eq!(block(&plain(&home))["new"], 0);

    // The memo gone: unknown, and still nothing new.
    fs::remove_file(home.env.cache().join("omaddons/errors.json")).unwrap();
    let unread = cached(&home);
    assert_eq!(
        counts(&unread),
        (json!("unchecked"), json!(0), json!(0), json!(0), json!(0)),
        "{unread:#}"
    );
    assert_eq!(
        block(&unread)["dismissed_at"],
        block(&dismissed)["dismissed_at"]
    );
    assert_eq!(block(&plain(&home))["new"], 0, "a live read honours it too");
    read(&home);
    assert_eq!(counts(&cached(&home)), counts(&dismissed));
}

#[test]
fn an_incomplete_read_is_partial_and_not_read_again() {
    // A file the game is still writing: it ends early.
    let home = Home::new(&["wow"]);
    install(&home);
    let mut file = session_10().file();
    let cut = file.bytes.len() - 20;
    file.bytes.truncate(cut);
    write_files(&home.client("_retail_").join("WTF"), &[file]);
    let target = read(&home);
    for status in [cached(&home), plain(&home)] {
        let e = block(&status);
        assert_eq!(e["state"], "partial", "{status:#}");
        assert_eq!(e["new"], target["summary"]["new"]);
        assert_eq!(e["total"], target["summary"]["entries"]);
        assert!(!actions(&status).contains(&"check_errors".to_owned()));
    }
    let human = home.exec_with(&["status".to_owned()], &[("OMADDONS_NOW", support::CLOCK)]);
    assert_private_all(&human.stdout, &human.stderr);
    assert!(
        human
            .stdout
            .contains("  errors: incomplete read (file was being written) · "),
        "{}",
        human.context()
    );

    // A file over the size a run may read: never read, so never read again.
    let home = Home::new(&["wow"]);
    install(&home);
    fs::create_dir_all(sv_dir(&home)).unwrap();
    fs::File::create(sv_dir(&home).join("!BugGrabber.lua"))
        .unwrap()
        .set_len(65 << 20)
        .unwrap();
    read(&home);
    for status in [cached(&home), cached(&home), plain(&home)] {
        let e = block(&status);
        assert_eq!(
            (&e["state"], &e["new"], &e["total"]),
            (&json!("partial"), &json!(0), &json!(0)),
            "{status:#}"
        );
        assert!(!actions(&status).contains(&"check_errors".to_owned()));
    }
}

#[test]
fn a_file_that_cannot_be_read_is_unreadable_never_zero() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new(&["wow"]);
    install(&home);
    write(&home, &session_10());
    let file = sv_dir(&home).join("!BugGrabber.lua");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        fs::read(&file).is_err(),
        "a mode 000 file is unreadable here"
    );
    read(&home);
    for status in [cached(&home), plain(&home)] {
        let e = block(&status);
        assert_eq!(e["state"], "unreadable", "{status:#}");
        assert!(!actions(&status).contains(&"check_errors".to_owned()));
    }
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn more_accounts_than_the_bound_is_truncated() {
    let home = Home::new(&["wow"]);
    install(&home);
    let files: Vec<SvFile> = (0..33)
        .map(|i| {
            SvFile::account(
                &format!("FAKEACCT{i:02}"),
                common::buggrabber::FILE,
                db(2, vec![error("Foo", 2, NOW - 60)]).render(),
            )
        })
        .collect();
    write_files(&home.client("_retail_").join("WTF"), &files);
    read(&home);
    for status in [cached(&home), plain(&home)] {
        let e = block(&status);
        assert_eq!(
            (&e["coverage"], &e["state"], &e["total"]),
            (&json!("truncated"), &json!("ok"), &json!(32)),
            "{status:#}"
        );
    }
}

#[test]
fn received_reports_alone_never_make_the_errors_state() {
    let home = Home::new(&["wow"]);
    install(&home);
    write(&home, &db(4, vec![received("Foo", 4, NOW - 60)]));
    read(&home);
    for status in [cached(&home), plain(&home)] {
        assert_eq!(
            counts(&status),
            (json!("ok"), json!(0), json!(0), json!(0), json!(1)),
            "{status:#}"
        );
        assert_eq!(status["data"]["summary"]["state"], "idle");
        assert_eq!(block(&status)["top"], json!([]));
    }
}

#[test]
fn the_human_view_names_addons_and_counts() {
    let home = Home::new(&["wow"]);
    install(&home);
    write(&home, &session_10());
    let show = |args: &[&str]| {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let run = home.exec_with(&argv, &[("OMADDONS_NOW", support::CLOCK)]);
        assert_eq!(run.code, 0, "{}", run.context());
        assert_private_all(&run.stdout, &run.stderr);
        run.stdout
    };
    let text = show(&["status"]);
    let unchecked = show(&["status", "--cached"]);
    assert!(
        unchecked.contains("  errors: not read yet · omaddons errors -t retail"),
        "{unchecked}"
    );
    assert!(unchecked.contains("Next: omaddons errors\n"), "{unchecked}");
    assert!(
        text.starts_with("2 new Lua errors on 1 client"),
        "the summary line: {text}"
    );
    assert!(
        text.contains("  errors: 2 new (Foo 1, Bar 1) · omaddons errors -t retail"),
        "{text}"
    );
    assert!(!text.contains("attempt to"), "no error text: {text}");
}
