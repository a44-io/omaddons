//! `omaddons backup list` end to end: seeded backups of every kind on Battle.net clients,
//! what each proves, damaged entries, `--limit`, target selection, one backup by id
//! prefix, the human view (`TZ=UTC`), the inventory projection it rebuilds, and that
//! listing 200 backups reads only their `backup.json` files. Every output is checked for
//! the fake account, realm and character names.

use crate::common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use common::backups::{backup_dir, meta, seed_backup, seed_many, seed_meta};
use common::home::Home;
use common::privacy::assert_private_all;
use common::wtf::writable_again;
use jiff::Timestamp;
use omaddons::backup::{BackupKind, BackupSource, Consistency, ImportSource, Integrity};
use omaddons::model::BackupId;
use serde_json::{Value, json};
use support::{at, ids, run, targets};

#[cfg(test)]
mod support {
    use jiff::Timestamp;
    use serde_json::Value;

    use super::common::home::{Home, Run};
    use super::common::privacy::assert_private_all;

    pub fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// `omaddons <args> --json --progress json`, private on both streams, with the free
    /// space pinned (it moves between runs).
    pub fn run(home: &Home, args: &[&str]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let r = home.exec_with(&argv, &[("OMADDONS_FS_FREE_MB", "1024")]);
        assert_private_all(&r.stdout, &r.stderr);
        r
    }

    pub fn targets(envelope: &Value) -> &Vec<Value> {
        envelope["data"]["targets"].as_array().unwrap()
    }

    pub fn ids(target: &Value) -> Vec<String> {
        target["backups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["id"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn backups_list_newest_first_with_what_each_proves() {
    let home = Home::new(&["wow", "wow_classic_era"]);
    let retail = home.client("_retail_");
    let manual = seed_backup(&retail, BackupKind::Manual, at("2026-10-18T21:15:00Z"), 3);
    let auto = seed_backup(&retail, BackupKind::Auto, at("2026-10-20T18:02:00Z"), 2);
    let before = seed_backup(
        &retail,
        BackupKind::BeforeRestore,
        at("2026-10-19T09:00:00Z"),
        1,
    );
    let mut imported = meta(BackupKind::Imported, at("2026-10-17T00:00:00Z"), 1);
    imported.source = Some(BackupSource::Import(ImportSource {
        id: BackupId::new(),
        target_id: "retail".to_owned(),
        family: omaddons::rules::Family::Mainline,
        created_at: at("2026-10-16T00:00:00Z"),
        captured_at: at("2026-10-17T00:00:00Z"),
        tool_version: "0.1.0".to_owned(),
    }));
    let imported = seed_meta(&retail, &imported);
    let mut torn = meta(BackupKind::Manual, at("2026-10-16T00:00:00Z"), 1);
    torn.consistency = Consistency::BestEffort;
    torn.unsettled = 1;
    let torn = seed_meta(&retail, &torn);
    let mut recovered = meta(BackupKind::Recovered, at("2026-10-15T00:00:00Z"), 1);
    recovered.partial = true;
    let recovered = seed_meta(&retail, &recovered);
    let mut verified = meta(BackupKind::Manual, at("2026-10-14T00:00:00Z"), 1);
    verified.integrity = Integrity::Verified;
    verified.verified_at = Some(at("2026-10-14T01:00:00Z"));
    verified.baseline = Some(omaddons::backup::Baseline::Later);
    verified.reason = Some("before the raid".to_owned());
    let verified = seed_meta(&retail, &verified);

    let envelope = run(&home, &["backup", "list"]).expect(0);
    assert_eq!(envelope["command"], "backup list");
    let listed = targets(&envelope);
    assert_eq!(listed.len(), 1, "only the client with backups: {envelope}");
    let t = &listed[0];
    assert_eq!(t["target"]["id"], "retail");
    let order = [auto, before, manual, imported, torn, recovered, verified]
        .map(|id| id.to_string())
        .to_vec();
    assert_eq!(ids(t), order, "newest capture first");
    let b = |id: &BackupId| {
        t["backups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["id"] == id.to_string())
            .unwrap()
            .clone()
    };
    let flags = |id: &BackupId| {
        let b = b(id);
        (
            b["restorable"].clone(),
            b["qualifying"].clone(),
            b["busy"].clone(),
        )
    };
    let yes = || Value::Bool(true);
    let no = || Value::Bool(false);
    assert_eq!(flags(&manual), (yes(), yes(), no()));
    assert_eq!(flags(&auto), (yes(), yes(), no()));
    assert_eq!(flags(&before), (yes(), yes(), no()));
    assert_eq!(
        flags(&imported),
        (yes(), no(), no()),
        "imported never qualifies"
    );
    assert_eq!(
        flags(&torn),
        (yes(), no(), no()),
        "best effort never qualifies"
    );
    assert_eq!(
        flags(&recovered),
        (yes(), no(), no()),
        "partial: --addon only"
    );
    let a = b(&auto);
    assert_eq!(
        (
            &a["kind"],
            &a["trigger"],
            &a["files"],
            &a["bytes"],
            &a["consistency"]
        ),
        (
            &json!("auto"),
            &json!("post_session"),
            &json!(2),
            &json!(200),
            &json!("stable")
        )
    );
    assert_eq!(a["integrity"], "metadata_only");
    assert_eq!(a["captured_at"], "2026-10-20T18:02:00Z");
    assert!(a.get("addons").is_none(), "addons only with -v");
    let v = b(&verified);
    assert_eq!(
        (&v["integrity"], &v["baseline"], &v["reason"]),
        (
            &json!("verified"),
            &json!("later"),
            &json!("before the raid")
        )
    );
    assert_eq!(t["bytes"], 1000, "the apparent total");
    assert_eq!((&t["shown"], &t["total"]), (&json!(7), &json!(7)));
    assert_eq!(t["free_bytes"], 1024 * 1024 * 1024, "OMADDONS_FS_FREE_MB");
    assert_eq!(
        t["auto"],
        json!({"mode": "daily", "capability": "unknown", "last": null}),
        "the default (M3-D30)"
    );

    // Bare `backup` is the same listing.
    let bare = run(&home, &["backup"]).expect(0);
    assert_eq!(bare["data"], envelope["data"]);

    // -v: the addon versions each capture recorded.
    let verbose = run(&home, &["backup", "list", "-v"]).expect(0);
    assert_eq!(
        targets(&verbose)[0]["backups"][0]["addons"],
        json!([{"name": "WeakAuras", "folders": ["WeakAuras", "WeakAurasOptions"], "version": "5.19.2"}])
    );
}

#[test]
fn a_damaged_backup_lists_as_not_restorable() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let fine = seed_backup(&retail, BackupKind::Manual, at("2026-10-20T00:00:00Z"), 1);
    let no_manifest = seed_backup(&retail, BackupKind::Manual, at("2026-10-19T00:00:00Z"), 1);
    fs::remove_file(backup_dir(&retail, &no_manifest).join("manifest.json")).unwrap();
    let unreadable = seed_backup(&retail, BackupKind::Manual, at("2026-10-18T00:00:00Z"), 1);
    fs::write(
        backup_dir(&retail, &unreadable).join("backup.json"),
        "{ torn",
    )
    .unwrap();
    let mut damaged = meta(BackupKind::Manual, at("2026-10-17T00:00:00Z"), 1);
    damaged.integrity = Integrity::Damaged;
    let damaged = seed_meta(&retail, &damaged);

    let envelope = run(&home, &["backup", "list"]).expect(0);
    let t = &targets(&envelope)[0];
    assert_eq!(
        ids(t),
        [fine, no_manifest, damaged, unreadable].map(|id| id.to_string()),
        "unreadable metadata last"
    );
    let restorable: Vec<&Value> = t["backups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| &b["restorable"])
        .collect();
    assert_eq!(
        restorable,
        [&json!(true), &json!(false), &json!(false), &json!(false)]
    );
    let torn = &t["backups"][3];
    assert_eq!(
        (&torn["kind"], &torn["captured_at"]),
        (&Value::Null, &Value::Null)
    );
    assert_eq!(t["total"], 4);

    let human = home.exec(&["backup".to_owned(), "list".to_owned()]);
    assert_eq!(human.code, 0, "{}", human.context());
    assert!(human.stdout.contains("damaged"), "{}", human.stdout);
    assert!(human.stdout.contains("not restorable"), "{}", human.stdout);
    assert!(human.stdout.contains("unreadable"), "{}", human.stdout);
}

#[test]
fn limit_and_target_selection() {
    let home = Home::new(&["wow", "wow_classic_era", "wow_classic"]);
    let (retail, era) = (home.client("_retail_"), home.client("_classic_era_"));
    let newest = seed_many(&retail, 5);
    seed_backup(&era, BackupKind::Auto, at("2026-10-20T00:00:00Z"), 1);

    let envelope = run(&home, &["backup", "list", "--limit", "2"]).expect(0);
    let listed = targets(&envelope);
    let names: Vec<&str> = listed
        .iter()
        .map(|t| t["target"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["era", "retail"], "every client with backups, by id");
    let r = &listed[1];
    assert_eq!((&r["shown"], &r["total"]), (&json!(2), &json!(5)));
    assert_eq!(
        ids(r),
        newest[..2]
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        r["bytes"], 500,
        "the total covers every backup, not the shown"
    );

    let envelope = run(&home, &["backup", "list", "-t", "classic"]).expect(0);
    let listed = targets(&envelope);
    assert_eq!(
        listed.len(),
        1,
        "a selected client without backups is listed"
    );
    assert_eq!(
        (&listed[0]["total"], &listed[0]["shown"]),
        (&json!(0), &json!(0))
    );

    let envelope = run(&home, &["backup", "list", "-t", "era"]).expect(0);
    assert_eq!(targets(&envelope)[0]["total"], 1);
    let r = run(&home, &["backup", "list", "-t", "nosuch"]);
    assert_eq!(r.expect(3)["error"]["code"], "target_not_found");

    let human = home.exec(&["backup".to_owned(), "-t".to_owned(), "classic".to_owned()]);
    assert!(human.stdout.contains("no backups"), "{}", human.stdout);
    let empty = Home::new(&["wow"]);
    let human = empty.exec(&["backup".to_owned()]);
    assert_eq!(human.stdout, "No backups.\n");
}

#[test]
fn one_backup_by_id_prefix() {
    let home = Home::new(&["wow", "wow_classic_era"]);
    let (retail, era) = (home.client("_retail_"), home.client("_classic_era_"));
    let mut a = meta(BackupKind::Manual, at("2026-10-18T00:00:00Z"), 1);
    a.id = "01M6Q2VA000000000000000000".parse().unwrap();
    let mut b = meta(BackupKind::Manual, at("2026-10-19T00:00:00Z"), 1);
    b.id = "01M6Q2VA100000000000000000".parse().unwrap();
    let mut c = meta(BackupKind::Manual, at("2026-10-20T00:00:00Z"), 1);
    c.id = "01M6Q3XC000000000000000000".parse().unwrap();
    seed_meta(&retail, &a);
    seed_meta(&era, &b);
    seed_meta(&retail, &c);

    let one = run(&home, &["backup", "list", "01m6q3xc"]).expect(0);
    let listed = targets(&one);
    assert_eq!(listed.len(), 1);
    assert_eq!(ids(&listed[0]), [c.id.to_string()]);
    assert_eq!(
        (&listed[0]["shown"], &listed[0]["total"]),
        (&json!(1), &json!(2))
    );

    let short = run(&home, &["backup", "list", "01M6Q3X"]).expect(2);
    assert_eq!(short["error"]["code"], "usage");

    let two = run(&home, &["backup", "list", "01M6Q2VA"]).expect(3);
    assert_eq!(two["error"]["code"], "ref_ambiguous");
    // Candidates in the clients' order (by id), each a complete argv.
    assert_eq!(
        two["error"]["fix_argv"],
        json!([
            "omaddons",
            "backup",
            "list",
            "01M6Q2VA100000000000000000",
            "-t",
            "era"
        ])
    );
    assert_eq!(
        two["error"]["alternatives"][0]["argv"],
        json!([
            "omaddons",
            "backup",
            "list",
            "01M6Q2VA000000000000000000",
            "-t",
            "retail"
        ])
    );
    let alternative: Vec<String> = two["error"]["alternatives"][0]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|v| v.as_str().unwrap().to_owned())
        .chain(["--json".to_owned()])
        .collect();
    assert_eq!(home.exec(&alternative).code, 0, "the alternative runs");

    let unknown = run(&home, &["backup", "list", "01ZZZZZZ", "-t", "retail"]).expect(3);
    assert_eq!(unknown["error"]["code"], "not_found");
    assert_eq!(
        unknown["error"]["details"]["newest_ids"],
        json!([c.id.to_string(), a.id.to_string()])
    );
    assert_eq!(
        unknown["error"]["fix_argv"],
        json!(["omaddons", "backup", "list", "-t", "retail"])
    );
}

#[test]
fn the_human_view() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let now = Timestamp::now();
    seed_backup(&retail, BackupKind::Auto, now, 412);
    let mut raid = meta(BackupKind::Manual, at("2026-10-18T21:15:00Z"), 412);
    raid.integrity = Integrity::Verified;
    raid.baseline = Some(omaddons::backup::Baseline::Capture);
    raid.reason = Some("before the raid".to_owned());
    let raid = seed_meta(&retail, &raid);
    let r = home.exec(&["backup".to_owned(), "list".to_owned()]);
    assert_eq!(r.code, 0, "{}", r.context());
    assert_private_all(&r.stdout, &r.stderr);
    let lines: Vec<&str> = r.stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{}", r.stdout);
    assert!(
        lines[0].starts_with("retail (12.1.5.70391, mainline) · 2 backups · 82.4 kB apparent, initially shared copy-on-write · ")
            && lines[0].ends_with(" free"),
        "{}",
        lines[0]
    );
    let today = format!(
        "today {}",
        now.to_zoned(jiff::tz::TimeZone::UTC).strftime("%H:%M")
    );
    assert!(lines[1].contains(&today), "{}", lines[1]);
    assert!(
        lines[1].contains("auto (after play)") && lines[1].contains("412 files"),
        "{}",
        lines[1]
    );
    assert!(
        lines[1].contains("41.2 kB") && lines[1].ends_with("stable"),
        "{}",
        lines[1]
    );
    let short = format!("{}…", &raid.to_string()[..16]);
    assert!(lines[2].trim_start().starts_with(&short), "{}", lines[2]);
    assert!(
        lines[2].contains("2026-10-18 21:15"),
        "TZ=UTC: {}",
        lines[2]
    );
    assert!(
        lines[2].contains("stable · verified (capture)")
            && lines[2].ends_with("\"before the raid\""),
        "{}",
        lines[2]
    );
    assert!(
        !lines[1].contains("verified"),
        "metadata only never reads verified"
    );
}

#[test]
fn listing_rebuilds_the_inventory_projection_when_stale() {
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    home.run(&["targets", "default", "retail"]).expect(0);
    seed_backup(&retail, BackupKind::Manual, at("2026-10-18T00:00:00Z"), 1);
    let projection = home.env.cache().join("omaddons/backups.json");
    assert!(!projection.exists());
    run(&home, &["backup", "list"]).expect(0);
    let read = || -> Value { serde_json::from_slice(&fs::read(&projection).unwrap()).unwrap() };
    let first = read();
    let key = first["targets"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        first["targets"][&key]["covers"]["backups"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(first["schema_version"], "1.0");

    // Unchanged: not rewritten.
    let mtime = || fs::metadata(&projection).unwrap().modified().unwrap();
    let before = mtime();
    run(&home, &["backup", "list"]).expect(0);
    assert_eq!(mtime(), before);
    // A backup added behind its back: stale, so the next listing rebuilds it.
    seed_backup(&retail, BackupKind::Auto, at("2026-10-19T00:00:00Z"), 1);
    run(&home, &["backup", "list"]).expect(0);
    let second = read();
    assert_eq!(
        second["targets"][&key]["covers"]["backups"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(second["targets"][&key]["latest"]["kind"], "auto");
    // `status --cached` never rebuilds it.
    seed_backup(&retail, BackupKind::Manual, at("2026-10-20T00:00:00Z"), 1);
    home.run(&["status", "--cached"]).expect(0);
    assert_eq!(read(), second);
}

#[test]
fn listing_two_hundred_backups_reads_only_their_metadata() {
    if common::procfs::running_as_root() {
        return; // modes hide nothing from root
    }
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    let ids = seed_many(&retail, 200);
    // Every manifest and tree unreadable: a listing that opened one, walked a tree or
    // hashed a file would fail or mark the backup damaged.
    for id in &ids {
        let dir = backup_dir(&retail, id);
        fs::set_permissions(dir.join("manifest.json"), fs::Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(dir.join("addons.json"), fs::Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(dir.join("WTF"), fs::Permissions::from_mode(0o000)).unwrap();
    }
    let r = run(&home, &["backup", "list"]);
    let envelope = r.expect(0);
    let t = &targets(&envelope)[0];
    assert_eq!((&t["total"], &t["shown"]), (&json!(200), &json!(200)));
    let restorable = t["backups"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["restorable"] == true && b["integrity"] == "metadata_only")
        .count();
    assert_eq!(
        restorable, 200,
        "listed from backup.json and the manifests' presence"
    );
    assert_eq!(ids.first().unwrap().to_string(), t["backups"][0]["id"]);
    for id in &ids {
        let dir = backup_dir(&retail, id);
        fs::set_permissions(dir.join("manifest.json"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(dir.join("addons.json"), fs::Permissions::from_mode(0o644)).unwrap();
    }
    writable_again(&retail.join("Interface/.omaddons/backups"));
}
