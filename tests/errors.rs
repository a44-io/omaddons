//! `omaddons errors` end to end over the synthetic BugGrabber scenarios
//! (`tests/common/buggrabber.rs`) on test homes: what it reads in both writer formats,
//! what it shows and never shows (strict and raw), the observation the memo and
//! `--check` derive from before any filter, the "new" rule per database, incomplete
//! reads, and its one write, the errors memo.

mod common;

use std::fs;

use common::buggrabber::{self, BugDb, BugEntry, NOW, SvFile, TimeKind, Writer};
use common::privacy::{assert_private, assert_private_all};
use serde_json::{Value, json};
use support::{Retail, addons_of};

#[cfg(test)]
mod support {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::Value;

    use super::common::addons::{addon_folder, toc};
    use super::common::buggrabber::{SvFile, write_files};
    use super::common::home::{Home, Run};

    /// The frozen clock: the scenarios' times are relative to it.
    pub const CLOCK: &str = "2026-10-15T18:30:00Z";

    /// Retail with BugGrabber installed and `folders` in its AddOns (unmanaged).
    pub struct Retail {
        pub home: Home,
    }

    impl Retail {
        pub fn new(folders: &[&str]) -> Self {
            let home = Home::new(&["wow"]);
            let addons = home.addons("_retail_");
            let bug_toc = toc("120105", &["## SavedVariables: BugGrabberDB"]);
            addon_folder(
                &addons,
                "!BugGrabber",
                &[("!BugGrabber.toc", &bug_toc)],
                &[],
            );
            for folder in folders {
                let file = format!("{folder}.toc");
                addon_folder(&addons, folder, &[(&file, &toc("120105", &[]))], &[]);
            }
            Self { home }
        }

        /// `folder`, a pack that embeds BugGrabber: its TOC declares `BugGrabberDB`.
        pub fn embed(&self, folder: &str) {
            let declares = toc("120105", &["## SavedVariables: BDUI_SAVE, BugGrabberDB"]);
            let file = format!("{folder}.toc");
            addon_folder(
                &self.home.addons("_retail_"),
                folder,
                &[(&file, &declares)],
                &[],
            );
        }

        /// Installs `folder` from a local folder, so a record manages it.
        pub fn manage(&self, folder: &str) {
            let dir = self.home.env.root().join("src").join(folder);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("{folder}.toc")), toc("120105", &[])).unwrap();
            let path = dir.display().to_string();
            self.home.run(&["install", &path, "-t", "retail"]).expect(0);
        }

        pub fn wtf(&self) -> PathBuf {
            self.home.client("_retail_").join("WTF")
        }

        pub fn write(&self, files: &[SvFile]) {
            write_files(&self.wtf(), files);
        }

        /// `omaddons errors <args> --json --progress json` with the clock frozen.
        pub fn errors(&self, args: &[&str]) -> Run {
            let mut argv = vec!["errors".to_owned()];
            argv.extend(args.iter().map(|a| (*a).to_owned()));
            argv.extend(["--json", "--progress", "json"].map(str::to_owned));
            let run = self.home.exec_with(&argv, &[("OMADDONS_NOW", CLOCK)]);
            super::assert_private_all(&run.stdout, &run.stderr);
            run
        }

        /// Human output, with the clock frozen.
        pub fn human(&self, args: &[&str]) -> Run {
            let mut argv = vec!["errors".to_owned()];
            argv.extend(args.iter().map(|a| (*a).to_owned()));
            let run = self.home.exec_with(&argv, &[("OMADDONS_NOW", CLOCK)]);
            super::assert_private_all(&run.stdout, &run.stderr);
            run
        }

        pub fn memo_path(&self) -> PathBuf {
            self.home.env.cache().join("omaddons/errors.json")
        }
    }

    /// The retail target of an envelope.
    pub fn retail(envelope: &Value) -> &Value {
        &envelope["data"]["targets"][0]
    }

    /// The target whose id `pick` accepts.
    pub fn target(envelope: &Value, pick: impl Fn(&str) -> bool) -> &Value {
        envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| pick(t["target"]["id"].as_str().unwrap_or_default()))
            .unwrap()
    }

    /// `(addon, kind, count, new)` for each listed addon of the retail target.
    pub fn addons_of(envelope: &Value) -> Vec<(String, String, u64, u64)> {
        retail(envelope)["addons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["addon"].as_str().unwrap().to_owned(),
                    a["kind"].as_str().unwrap().to_owned(),
                    a["count"].as_u64().unwrap(),
                    a["new"].as_u64().unwrap(),
                )
            })
            .collect()
    }
}

fn row(addon: &str, kind: &str, count: u64, new: u64) -> (String, String, u64, u64) {
    (addon.to_owned(), kind.to_owned(), count, new)
}

/// The account-wide file of a scenario database.
fn file(db: &BugDb) -> SvFile {
    db.file()
}

#[test]
fn every_scenario_reads_alike_in_both_writer_formats() {
    type Scenario = fn(Writer) -> Vec<SvFile>;
    let scenarios: [(&str, Scenario, usize); 13] = [
        ("modern_int", |w| vec![file(&buggrabber::modern_int(w))], 4),
        (
            "first_seen",
            |w| vec![file(&buggrabber::modern_string_first_seen(w))],
            2,
        ),
        (
            "last_seen",
            |w| vec![file(&buggrabber::modern_string_last_seen(w))],
            2,
        ),
        (
            "r152_chunked",
            |w| vec![file(&buggrabber::r152_chunked(w))],
            2,
        ),
        ("embedded_bdui", buggrabber::embedded_bdui, 1),
        ("warnings", |w| vec![file(&buggrabber::warnings(w))], 2),
        ("forbidden", |w| vec![file(&buggrabber::forbidden(w))], 1),
        ("personal", |w| vec![file(&buggrabber::personal(w))], 1),
        ("received", |w| vec![file(&buggrabber::received(w))], 1),
        (
            "custom_names",
            |w| vec![file(&buggrabber::custom_names(w))],
            3,
        ),
        ("two_accounts", buggrabber::two_accounts, 3),
        (
            "mixed_times",
            |w| vec![file(&buggrabber::mixed_times(w))],
            3,
        ),
        ("flood", |w| vec![file(&buggrabber::flood(w, 30))], 30),
    ];
    let folders = [
        "Foo",
        "Bar",
        "Baz",
        "Questie",
        "AtlasLootClassic",
        "WeakAuras",
        "SomeUIPack",
        "Details",
        "RaidAlerts",
        "Flood",
    ];
    for (name, files, local_entries) in scenarios {
        let mut seen = Vec::new();
        for writer in Writer::ALL {
            let r = Retail::new(&folders);
            r.embed("bdUI");
            r.write(&files(writer));
            let run = r.errors(&[]);
            let envelope = run.expect(0);
            let t = support::retail(&envelope);
            assert_eq!(
                t["summary"]["entries"].as_u64(),
                Some(local_entries as u64),
                "{name} ({writer:?}): {}",
                run.context()
            );
            assert_eq!(t["capture"], "buggrabber", "{name}");
            // Everything but the database order is the same in both formats.
            seen.push(json!([t["addons"], t["summary"]]));
        }
        assert_eq!(seen[0], seen[1], "{name}: the formats read alike");
    }
}

#[test]
fn attribution_names_the_likely_source_with_its_evidence() {
    let r = Retail::new(&["Questie", "AtlasLootClassic", "WeakAuras", "SomeUIPack"]);
    r.manage("Foo");
    r.write(&[file(&buggrabber::modern_int(Writer::Modern))]);
    let envelope = r.errors(&[]).expect(0);
    let addons = addons_of(&envelope);
    // Session 412 is the latest: the first read marks its errors new.
    assert_eq!(
        addons,
        [
            row("WeakAuras", "addon", 57, 1),
            row("SomeUIPack", "addon", 1, 1),
            row("Foo", "addon", 3, 0),
            row("Questie", "addon", 1, 0),
        ]
    );
    let evidence = |addon: &str| {
        let a = support::retail(&envelope)["addons"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["addon"] == addon)
            .unwrap()
            .clone();
        (a["managed"].clone(), a["errors"][0]["attribution"].clone())
    };
    assert_eq!(
        evidence("Foo"),
        (
            json!(true),
            json!({"method": "first_addon_frame", "confidence": "high"})
        )
    );
    assert_eq!(
        evidence("WeakAuras"),
        (
            json!(false),
            json!({"method": "first_addon_frame", "confidence": "medium"})
        ),
        "custom code"
    );
    assert_eq!(
        evidence("Questie").1["confidence"],
        "high",
        "not the library"
    );
    let forbidden = Retail::new(&["Foo"]);
    forbidden.write(&[file(&buggrabber::forbidden(Writer::Legacy))]);
    let envelope = forbidden.errors(&[]).expect(0);
    let error = &support::retail(&envelope)["addons"][0]["errors"][0];
    assert_eq!(
        (
            &error["attribution"]["method"],
            &error["message_kind"],
            &error["type"]
        ),
        (
            &json!("protected_call"),
            &json!("blizzard"),
            &json!("error")
        )
    );
}

#[test]
fn strict_output_never_shows_names_and_raw_output_replaces_them() {
    for writer in Writer::ALL {
        for files in [
            vec![file(&buggrabber::personal(writer))],
            vec![file(&buggrabber::custom_names(writer))],
        ] {
            let r = Retail::new(&["Foo", "Bar", "RaidAlerts"]);
            buggrabber::write_files(
                &r.wtf(),
                &[SvFile::account(
                    "FAKEACCT",
                    "Foo.lua",
                    b"\r\nFooDB = {\r\n}\r\n".to_vec(),
                )],
            );
            fs::create_dir_all(r.wtf().join("Account/FAKEACCT/FakeRealm/FakeChar")).unwrap();
            r.write(&files);
            for args in [
                &[][..],
                &["-v"],
                &["--raw-details"],
                &["--raw-details", "-v"],
            ] {
                let run = r.errors(args);
                run.expect(0);
                let human = r.human(args);
                assert_eq!(human.code, 0, "{}", human.context());
                assert!(!run.stdout.contains("Player-1234"), "{}", run.stdout);
                assert!(!human.stdout.contains("Player-1234"), "{}", human.stdout);
                // Code chunks show only with --raw-details (names replaced there).
                if !args.contains(&"--raw-details") {
                    assert!(!run.stdout.contains("print("), "no chunk text");
                    assert!(!human.stdout.contains("print("), "no chunk text");
                }
            }
        }
    }
    let r = Retail::new(&["RaidAlerts", "Foo"]);
    fs::create_dir_all(r.wtf().join("Account/FAKEACCT/FakeRealm/FakeChar")).unwrap();
    r.write(&[file(&buggrabber::custom_names(Writer::Modern))]);
    let plain = r.errors(&[]).expect(0);
    let raid = &support::retail(&plain)["addons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["addon"] == "RaidAlerts")
        .unwrap()["errors"][0];
    assert_eq!(
        (
            &raid["message_kind"],
            &raid["message"],
            &raid["message_chars"]
        ),
        (&json!("custom"), &Value::Null, &json!(28))
    );
    let runtime = plain.to_string();
    assert!(runtime.contains("attempt to index a nil value (field 'x')"));
    let raw = r.errors(&["--raw-details"]).expect(0);
    assert_eq!(raw["data"]["raw_details"], true);
    assert!(
        raw.to_string().contains("<character> on <realm> pulled"),
        "{raw}"
    );
    let human = r.human(&["--raw-details"]);
    assert!(human.stdout.starts_with(
        "raw details may contain other players' names and your own code; share with care\n"
    ));
}

#[test]
fn filters_shape_the_view_never_the_observation() {
    let r = Retail::new(&[]);
    let mut db = BugDb::new(Writer::Modern, 20);
    let folders: Vec<String> = (0..12).map(|i| format!("Addon{i:02}")).collect();
    for (i, folder) in folders.iter().enumerate() {
        let file = format!("{folder}.toc");
        common::addons::addon_folder(
            &r.home.addons("_retail_"),
            folder,
            &[(&file, &common::addons::toc("120105", &[]))],
            &[],
        );
        db = db.entry(
            BugEntry::new(
                &format!(
                    "{} attempt to call a nil value",
                    buggrabber::stack_era_c(folder, "a.lua", 3)
                ),
                // The eleventh (by name) is the only new one.
                if i == 10 { 20 } else { 15 },
                (TimeKind::Unix, NOW - 60),
            )
            .counter(u32::try_from(20 - i).unwrap()),
        );
    }
    r.write(&[file(&db)]);
    let plain = r.errors(&[]).expect(0);
    let memo = fs::read(r.memo_path()).unwrap();
    let limited = r.errors(&["--limit", "10"]).expect(0);
    assert_eq!(addons_of(&limited).len(), 10);
    let summary = &support::retail(&limited)["summary"];
    assert_eq!(
        (&summary["entries"], &summary["matched"], &summary["new"]),
        (&json!(12), &json!(12), &json!(1))
    );
    assert_eq!(summary["shown"], 10);
    let narrow = r.errors(&["--addon", "Addon00", "--limit", "1"]).expect(0);
    assert_eq!(addons_of(&narrow).len(), 1);
    assert_eq!(
        fs::read(r.memo_path()).unwrap(),
        memo,
        "the memo is the whole observation's"
    );
    // The default limit is 10; the totals never depend on it or on --addon.
    assert_eq!(support::retail(&plain)["summary"], *summary);
    let narrowed = &support::retail(&narrow)["summary"];
    assert_eq!(
        (
            &narrowed["entries"],
            &narrowed["new"],
            &narrowed["matched"],
            &narrowed["shown"]
        ),
        (&json!(12), &json!(1), &json!(1), &json!(1))
    );
    // The check sees the new error past `--limit`, not the view.
    let r#check = r.errors(&["--check", "--limit", "1"]);
    let failed = r#check.expect(6);
    assert_eq!(failed["error"]["details"]["reason"], "new_errors");
    assert_eq!(
        failed["error"]["fix_argv"],
        json!(["omaddons", "errors", "-t", "retail"])
    );
    assert!(failed["data"]["targets"].is_array(), "data kept");
}

#[test]
fn received_reports_are_counted_apart_and_never_new() {
    let r = Retail::new(&["Foo", "Details"]);
    r.write(&[file(&buggrabber::received(Writer::Modern))]);
    let envelope = r.errors(&[]).expect(0);
    let t = support::retail(&envelope);
    assert_eq!(addons_of(&envelope), [row("Foo", "addon", 1, 0)]);
    assert_eq!(
        (&t["summary"]["received"], &t["summary"]["new"]),
        (&json!(2), &json!(0))
    );
    assert!(envelope["commands"].as_array().unwrap().is_empty());
    let with = r.errors(&["--received"]).expect(0);
    let groups: Vec<(String, String, u64)> = support::retail(&with)["addons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["addon"].as_str().unwrap().to_owned(),
                a["origin"].as_str().unwrap().to_owned(),
                a["new"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        groups,
        [
            ("Foo".to_owned(), "local".to_owned(), 0),
            ("Details".to_owned(), "received".to_owned(), 0)
        ]
    );
    assert_eq!(support::retail(&with)["summary"]["new"], 0);
}

#[test]
fn since_reports_what_string_times_cannot_place() {
    let r = Retail::new(&["Foo"]);
    r.write(&[file(&buggrabber::mixed_times(Writer::Modern))]);
    let envelope = r.errors(&["--since", "1d"]).expect(0);
    let summary = &support::retail(&envelope)["summary"];
    assert_eq!(
        (
            &summary["matched"],
            &summary["uncertain"],
            &summary["no_time"]
        ),
        (&json!(2), &json!(1), &json!(0))
    );
    let human = r.human(&["--since", "1d"]);
    assert!(
        human.stdout.contains(
            "1 older first-seen error may have recurred since then (string times cannot tell)"
        ),
        "{}",
        human.stdout
    );
    // An uncertain entry that is new fails the check: it may be recent.
    let old_and_new = BugDb::new(Writer::Modern, 50).entry(BugEntry::new(
        &format!(
            "{} attempt to call a nil value",
            buggrabber::stack_era_c("Foo", "a.lua", 1)
        ),
        50,
        (TimeKind::LocalString, NOW - 3 * 86_400),
    ));
    let r = Retail::new(&["Foo"]);
    r.write(&[file(&old_and_new)]);
    let failed = r.errors(&["--check", "--since", "1d"]).expect(6);
    assert_eq!(failed["error"]["details"]["reason"], "new_errors");
    assert_eq!(support::retail(&failed)["summary"]["uncertain"], 1);
    let bad = r.errors(&["--since", "yesterday"]).expect(2);
    assert_eq!(
        bad["error"]["fix_argv"],
        json!(["omaddons", "errors", "--since", "1d"])
    );
    assert_eq!(
        r.errors(&["--limit", "0"]).expect(2)["error"]["code"],
        "usage"
    );
}

#[test]
fn each_database_keeps_its_own_sessions_and_baseline() {
    let r = Retail::new(&["Foo", "Bar", "Baz"]);
    r.write(&buggrabber::two_accounts(Writer::Legacy));
    let envelope = r.errors(&[]).expect(0);
    let dbs: Vec<(String, u64, u64)> = support::retail(&envelope)["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["account"].as_str().unwrap().to_owned(),
                d["session"].as_u64().unwrap(),
                d["baseline"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        dbs,
        [
            ("account 1".to_owned(), 120, 119),
            ("account 2".to_owned(), 7, 6)
        ]
    );
    assert_eq!(support::retail(&envelope)["summary"]["new"], 2);
    assert_private(&envelope.to_string());
}

/// A database of `session` with one error per session in `sessions`.
fn sessions_db(session: u32, sessions: &[u32]) -> BugDb {
    sessions
        .iter()
        .fold(BugDb::new(Writer::Modern, session), |db, s| {
            db.entry(BugEntry::new(
                &format!(
                    "{} attempt to call a nil value",
                    buggrabber::stack_era_c("Foo", "a.lua", *s)
                ),
                *s,
                (TimeKind::Unix, NOW - 60),
            ))
        })
}

/// The first addon's errors' `new` flags, by location.
fn new_flags(envelope: &Value) -> Vec<bool> {
    let mut flags: Vec<(String, bool)> = support::retail(envelope)["addons"][0]["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            (
                e["location"].as_str().unwrap_or_default().to_owned(),
                e["new"].as_bool().unwrap_or_default(),
            )
        })
        .collect();
    flags.sort();
    flags.into_iter().map(|(_, new)| new).collect()
}

#[test]
fn new_errors_stay_new_across_reads_and_a_reset() {
    let r = Retail::new(&["Foo"]);
    r.write(&[file(&sessions_db(10, &[8, 9, 10]))]);
    let first = r.errors(&[]).expect(0);
    assert_eq!(
        new_flags(&first),
        [true, false, false],
        "a.lua:10 then a.lua:8, a.lua:9"
    );
    assert_eq!(
        new_flags(&r.errors(&[]).expect(0)),
        [true, false, false],
        "a second read"
    );
    // Two later sessions (a /reload between them): both new.
    r.write(&[file(&sessions_db(12, &[8, 9, 10, 11, 12]))]);
    let later = r.errors(&[]).expect(0);
    assert_eq!(support::retail(&later)["summary"]["new"], 3);
    // `/bugsack reset`: the session restarts at 1, so everything since is new.
    r.write(&[file(&sessions_db(1, &[1]))]);
    let reset = r.errors(&[]).expect(0);
    assert_eq!(support::retail(&reset)["databases"][0]["baseline"], 0);
    assert_eq!(support::retail(&reset)["summary"]["new"], 1);
    // Past the old baseline again, with no dismissal: still new.
    r.write(&[file(&sessions_db(15, &[1, 15]))]);
    let past = r.errors(&[]).expect(0);
    assert_eq!(support::retail(&past)["summary"]["new"], 2);
}

#[test]
fn a_file_older_than_a_week_has_no_new_errors() {
    let r = Retail::new(&["Foo"]);
    r.write(&[file(&sessions_db(10, &[10]))]);
    let path = r
        .wtf()
        .join("Account/FAKEACCT/SavedVariables/!BugGrabber.lua");
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_792_089_000 - 8 * 86_400);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let envelope = r.errors(&[]).expect(0);
    assert_eq!(support::retail(&envelope)["summary"]["new"], 0);
    assert_eq!(support::retail(&envelope)["summary"]["entries"], 1);
}

#[test]
fn incomplete_reads_are_reported_and_fail_the_check() {
    // Cut mid-entry: what came before is kept.
    let r = Retail::new(&["Foo"]);
    let mut cut = file(&sessions_db(10, &[9, 10]));
    // Cut where the second entry begins: the first stays whole.
    let at = cut
        .bytes
        .windows(11)
        .enumerate()
        .filter(|(_, w)| *w == b"[\"message\"]")
        .nth(1)
        .unwrap()
        .0;
    cut.bytes.truncate(at);
    r.write(&[cut]);
    let run = r.errors(&[]);
    let envelope = run.expect(0);
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "errors_partial"),
        "{}",
        run.context()
    );
    assert_eq!(
        support::retail(&envelope)["databases"][0]["state"],
        "partial"
    );
    assert_eq!(support::retail(&envelope)["summary"]["entries"], 1);
    let failed = r.errors(&["--check"]).expect(6);
    assert_eq!(failed["error"]["details"]["reason"], "incomplete");

    // Over the size a run may read: not read, and recorded with its stamp.
    let big = Retail::new(&["Foo"]);
    let sv = big.wtf().join("Account/FAKEACCT/SavedVariables");
    fs::create_dir_all(&sv).unwrap();
    fs::File::create(sv.join("!BugGrabber.lua"))
        .unwrap()
        .set_len(65 << 20)
        .unwrap();
    let envelope = big.errors(&[]).expect(0);
    assert_eq!(
        support::retail(&envelope)["databases"][0]["state"],
        "over_budget"
    );
    let memo: Value = serde_json::from_slice(&fs::read(big.memo_path()).unwrap()).unwrap();
    let db = &memo["targets"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()["databases"][0];
    assert_eq!(db["state"], "over_budget");
    assert!(db["stamp"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(
        big.errors(&["--check"]).expect(6)["error"]["details"]["reason"],
        "incomplete"
    );

    // More account folders than the walk examines.
    let many = Retail::new(&["Foo"]);
    for i in 0..33 {
        let sv = many
            .wtf()
            .join(format!("Account/ACCT{i:02}/SavedVariables"));
        fs::create_dir_all(&sv).unwrap();
    }
    let envelope = many.errors(&[]).expect(0);
    assert_eq!(support::retail(&envelope)["coverage"], "truncated");
    assert_eq!(
        many.errors(&["--check"]).expect(6)["error"]["details"]["reason"],
        "incomplete"
    );
}

/// A client without BugGrabber gets the catalog's build for it as a runnable install
/// (which works, replayed); a 4.3.4 client, for which no build is known (every revision
/// needs the packager), gets no command and says so, never another family's build.
#[test]
fn a_client_without_bug_grabber_is_listed_with_a_hint() {
    let mut home = common::home::Home::new(&["wow"]);
    home.with_custom(
        common::trees::CustomRoot::at(home.env.root().join("wow-434")).version(4, 3, 4, 15595),
    );
    let run = home.exec_with(
        &["errors", "--json"].map(str::to_owned),
        &[("OMADDONS_NOW", support::CLOCK)],
    );
    let envelope = run.expect(0);
    let retail = support::target(&envelope, |id| id == "retail");
    assert_eq!(
        (&retail["capture"], retail.get("capture_note")),
        (&json!("none"), None)
    );
    let vanilla = support::target(&envelope, |id| id != "retail");
    assert_eq!(
        (&vanilla["capture"], &vanilla["capture_note"]),
        (
            &json!("none"),
            &json!("no BugGrabber build for this client is known")
        )
    );
    assert_eq!(
        envelope["commands"],
        json!([{
            "action": "install_buggrabber",
            "command": "omaddons install buggrabber -t retail",
            "argv": ["omaddons", "install", "buggrabber", "-t", "retail"],
            "destructive": false
        }]),
        "one install, for the client a build is known for"
    );
    let human = home.exec(&["errors".to_owned()]);
    assert!(
        human
            .stdout
            .contains("no Lua errors are captured: BugGrabber is not installed"),
        "{}",
        human.context()
    );
    assert!(
        human
            .stdout
            .contains("no Lua errors are captured: no BugGrabber build for this client is known"),
        "{}",
        human.context()
    );
    assert!(
        human
            .stdout
            .contains("Capture Lua errors: omaddons install buggrabber -t retail"),
        "{}",
        human.context()
    );
    let missing = home.run(&["errors", "--addon", "Foo", "-t", "retail"]);
    assert_eq!(missing.expect(3)["error"]["code"], "not_found");

    // The hint as given installs BugGrabber.
    home.serve_recorded("wowace-bug-grabber");
    let argv: Vec<String> = envelope["commands"][0]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|a| a.as_str().unwrap().to_owned())
        .collect();
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let installed = home.run(&args);
    let item = &installed.expect(0)["data"]["items"][0];
    assert_eq!(
        (&item["status"], &item["ref"]),
        (&json!("installed"), &json!("wowace:bug-grabber")),
        "{}",
        installed.context()
    );
}

/// A 1.12.1 client is offered its own build (the 1.12-era tree, `#r43:tmp/trunk/…`),
/// and the hint as given installs it.
#[test]
fn a_1121_client_is_offered_its_own_build() {
    let mut home = common::home::Home::new(&[]);
    home.with_custom(
        common::trees::CustomRoot::at(home.env.root().join("wow-1121")).version(1, 12, 1, 5875),
    );
    let run = home.exec_with(
        &["errors", "--json"].map(str::to_owned),
        &[("OMADDONS_NOW", support::CLOCK)],
    );
    let envelope = run.expect(0);
    let vanilla = support::target(&envelope, |_| true);
    assert_eq!(
        (&vanilla["capture"], vanilla.get("capture_note")),
        (&json!("none"), None),
        "{}",
        run.context()
    );
    let id = vanilla["target"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        envelope["commands"][0]["argv"],
        json!(["omaddons", "install", "buggrabber", "-t", id]),
        "{}",
        run.context()
    );
    home.serve_recorded("wowace-bug-grabber");
    let installed = home.run(&["install", "buggrabber", "-t", &id]);
    let item = &installed.expect(0)["data"]["items"][0];
    assert_eq!(
        (&item["status"], &item["ref"]),
        (
            &json!("installed"),
            &json!("wowace:bug-grabber#r43:tmp/trunk/!BugGrabber")
        ),
        "{}",
        installed.context()
    );
}

#[test]
fn the_memo_is_written_only_when_it_changed_and_rings_then() {
    let r = Retail::new(&["Foo"]);
    r.write(&[file(&sessions_db(10, &[10]))]);
    let bell = r.home.env.cache().join("omaddons/doorbell.json");
    r.errors(&[]).expect(0);
    let memo = fs::read_to_string(r.memo_path()).unwrap();
    assert!(!memo.contains("attempt to"), "no message text in the memo");
    assert!(bell.exists());
    let written = fs::metadata(r.memo_path()).unwrap().modified().unwrap();
    fs::remove_file(&bell).unwrap();
    r.errors(&[]).expect(0);
    assert_eq!(
        fs::metadata(r.memo_path()).unwrap().modified().unwrap(),
        written
    );
    assert!(!bell.exists(), "nothing changed, nothing rung");
    // State is untouched: the memo is in the cache.
    assert!(!r.home.state().join("errors.json").exists());
}

#[test]
fn an_update_that_may_fix_new_errors_is_offered_and_runs() {
    let mut home = common::home::Home::new(&["wow"]);
    let (t1, t2) = (
        common::addons::toc("120105", &["## Version: v1"]),
        common::addons::toc("120105", &["## Version: v2"]),
    );
    let a1: [common::http::Asset<'_>; 1] = [("Foo.zip", &[], vec![("Foo/Foo.toc", t1.as_str())])];
    let a2: [common::http::Asset<'_>; 1] = [("Foo.zip", &[], vec![("Foo/Foo.toc", t2.as_str())])];
    home.installed_behind(
        "https://github.com/o/foo",
        ("v1", &a1),
        ("v2", &a2),
        &["retail"],
    );
    let addons = home.addons("_retail_");
    let bug_toc = common::addons::toc("120105", &["## SavedVariables: BugGrabberDB"]);
    common::addons::addon_folder(
        &addons,
        "!BugGrabber",
        &[("!BugGrabber.toc", &bug_toc)],
        &[],
    );
    buggrabber::write_files(
        &home.client("_retail_").join("WTF"),
        &[file(&sessions_db(10, &[10]))],
    );
    // A check teaches the release memo that v2 is out; `errors` asks nothing.
    home.run(&["outdated"]).expect(0);
    let run = home.exec_with(
        &["errors", "--json"].map(str::to_owned),
        &[("OMADDONS_NOW", support::CLOCK)],
    );
    let envelope = run.expect(0);
    let foo = &support::retail(&envelope)["addons"][0];
    assert_eq!(
        (&foo["addon"], &foo["managed"], &foo["latest"]["status"]),
        (&json!("Foo"), &json!(true), &json!("update_available")),
        "{}",
        run.context()
    );
    // The update first; then dismissing what was read.
    assert_eq!(
        envelope["commands"],
        json!([{
            "action": "update",
            "command": "omaddons update Foo -t retail",
            "argv": ["omaddons", "update", "Foo", "-t", "retail"],
            "destructive": false
        }, {
            "action": "dismiss_errors",
            "command": "omaddons errors dismiss -t retail",
            "argv": ["omaddons", "errors", "dismiss", "-t", "retail"],
            "destructive": false
        }])
    );
    let human = home.exec_with(&["errors".to_owned()], &[("OMADDONS_NOW", support::CLOCK)]);
    assert!(
        human.stdout.contains("v1 → v2 available (may fix it)"),
        "{}",
        human.stdout
    );
    assert!(
        human
            .stdout
            .contains("Update them: omaddons update Foo -t retail")
    );
    home.run(&["update", "Foo", "-t", "retail"]).expect(0);
}

#[test]
fn human_output_says_likely_source_and_folds_c_frames() {
    let r = Retail::new(&["Questie", "AtlasLootClassic", "WeakAuras", "SomeUIPack"]);
    r.manage("Foo");
    r.write(&[file(&buggrabber::modern_int(Writer::Modern))]);
    let human = r.human(&[]);
    for expected in [
        "retail (12.1.5.70391, mainline) · BugGrabber · 4 errors from 4 addons · 2 new\n",
        "likely source (custom code)",
        "    Foo/core.lua:12: attempt to index a nil value (field 'db')\n",
        "      Foo/core.lua:12: in function 'Refresh'\n",
        "      … (1 C frame)\n",
    ] {
        assert!(
            human.stdout.contains(expected),
            "{expected:?} in\n{}",
            human.stdout
        );
    }
    assert!(!human.stdout.contains("caused by"));
}
