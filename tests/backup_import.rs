//! `omaddons backup import` end to end (M3-D35, M3-D31, M3-D32, M3-D47): export → import →
//! restore gives the original `WTF/` back exactly, a `0500` directory included; the
//! imported backup lists as `imported`, `verified (import)`, with its original capture
//! time, never qualifies as a checkpoint, has its history line, and retention leaves it
//! alone; every hostile archive (a name that leaves `WTF/`, an entry kind an export never
//! holds, a duplicate, a payload that differs from its manifest, a symlink that escapes or
//! loops or stands where a file is written, a bomb, a decoder window past the bound) is
//! refused with no stage left and nothing listed; another family's backup needs
//! `--force`. Every run is checked for the fake account, realm and character names on
//! both streams.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};

use common::archives::{TarEntry, pax, tar_entries, tar_zst, zstd_huge_window};
use common::home::{Home, Run};
use common::privacy::assert_private_all;
use common::trees::tree_hash;
use common::wtf::Wtf;
use serde_json::{Value, json};
use support::{exported_archive, ids, imported, refuse, run, with_manifest, wtf_of};

#[cfg(test)]
mod support {
    use super::*;

    /// `omaddons <args> --json --progress json`, checked for privacy on both streams.
    pub fn run(home: &Home, args: &[&str]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let r = home.exec_with(&argv, &[("OMADDONS_FS_FREE_MB", "4096")]);
        assert_private_all(&r.stdout, &r.stderr);
        r
    }

    pub fn wtf_of(home: &Home) -> PathBuf {
        home.client("_retail_").join("WTF")
    }

    /// A retail `WTF/` (two addons' SavedVariables, a read-only directory, a symlink),
    /// its verified backup, and that backup exported; returns the archive's path.
    pub fn exported_archive(home: &Home) -> PathBuf {
        let wtf = Wtf::at(home.client("_retail_"))
            .config_wtf("SET locale \"enUS\"\n")
            .saved_variables("Foo", 300)
            .saved_variables("Bar", 120)
            .file("Account/FAKEACCT/Sealed/Kept.lua", b"Kept = 1")
            .mode("Account/FAKEACCT/Sealed", 0o500)
            .build();
        std::os::unix::fs::symlink(
            "SavedVariables/Foo.lua",
            wtf.join("Account/FAKEACCT/Linked.lua"),
        )
        .unwrap();
        let created = run(home, &["backup", "create", "-t", "retail", "--verify"]).expect(0);
        let id = created["data"]["targets"][0]["backup"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let dir = home.env.root().join("usb");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("retail-wtf.tar.zst");
        run(
            home,
            &["backup", "export", &id, &file.display().to_string()],
        )
        .expect(0);
        file
    }

    /// The ids of retail's backups.
    pub fn ids(home: &Home) -> Vec<String> {
        let envelope = run(home, &["backup", "list", "-t", "retail"]).expect(0);
        envelope["data"]["targets"][0]["backups"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|b| b["id"].as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `backup import <file> -t retail`, which must succeed; the imported backup's list
    /// entry.
    pub fn imported(home: &Home, file: &Path) -> Value {
        let r = run(
            home,
            &[
                "backup",
                "import",
                &file.display().to_string(),
                "-t",
                "retail",
            ],
        );
        r.expect(0)["data"]["backup"].clone()
    }

    /// The archive `bytes` imported into retail: refused with `code` and `reason`, no
    /// stage left and no backup more. Returns the error.
    pub fn refuse(
        home: &Home,
        name: &str,
        bytes: &[u8],
        code: &str,
        reason: Option<&str>,
    ) -> Value {
        let before = ids(home);
        let file = home.env.root().join("usb").join(name);
        fs::write(&file, bytes).unwrap();
        let r = run(
            home,
            &[
                "backup",
                "import",
                &file.display().to_string(),
                "-t",
                "retail",
            ],
        );
        let envelope = r.envelope();
        let error = &envelope["error"];
        assert_eq!(error["code"], code, "{name}: {}", r.context());
        if let Some(reason) = reason {
            assert_eq!(
                error["details"]["reason"],
                reason,
                "{name}: {}",
                r.context()
            );
        }
        let stage = home.client("_retail_").join("Interface/.omaddons/stage");
        let left: Vec<_> = fs::read_dir(&stage)
            .map(|l| l.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{name}: a stage is left: {left:?}");
        assert_eq!(ids(home), before, "{name}: nothing listed");
        error.clone()
    }

    /// `entries` with their `manifest.json` changed by `edit`.
    pub fn with_manifest(entries: &[TarEntry], edit: impl Fn(&mut Value)) -> Vec<TarEntry> {
        entries
            .iter()
            .map(|e| {
                if e.path() != "manifest.json" {
                    return e.clone();
                }
                let mut manifest: Value = serde_json::from_slice(&e.body).unwrap();
                edit(&mut manifest);
                TarEntry {
                    body: serde_json::to_vec(&manifest).unwrap(),
                    ..e.clone()
                }
            })
            .collect()
    }
}

/// Export, import, restore: the original `WTF/` back exactly; the imported backup is
/// what M3-D35 says, and nothing treats it as today's checkpoint.
#[test]
fn an_export_imports_and_restores_exactly() {
    let home = Home::new(&["wow"]);
    let file = exported_archive(&home);
    let original = tree_hash(&wtf_of(&home));
    let source_id = ids(&home)[0].clone();

    let dry = run(
        &home,
        &[
            "backup",
            "import",
            &file.display().to_string(),
            "-t",
            "retail",
            "--dry-run",
        ],
    );
    let envelope = dry.expect(0);
    assert_eq!(
        (
            &envelope["data"]["backup"],
            &envelope["data"]["source"]["id"]
        ),
        (&Value::Null, &json!(source_id))
    );
    assert_eq!(ids(&home).len(), 1, "a dry run imports nothing");

    let bell = home.env.cache().join("omaddons/doorbell.json");
    fs::remove_file(&bell).unwrap();
    let backup = imported(&home, &file);
    let id = backup["id"].as_str().unwrap().to_owned();
    assert_ne!(id, source_id, "a new id");
    assert_eq!(
        (
            &backup["kind"],
            &backup["integrity"],
            &backup["baseline"],
            &backup["qualifying"]
        ),
        (
            &json!("imported"),
            &json!("verified"),
            &json!("import"),
            &json!(false)
        )
    );
    let list = run(&home, &["backup", "list", "-t", "retail"]).expect(0);
    let listed = list["data"]["targets"][0]["backups"]
        .as_array()
        .unwrap()
        .clone();
    let source = listed.iter().find(|b| b["id"] == source_id).unwrap();
    assert_eq!(
        backup["captured_at"], source["captured_at"],
        "the original capture"
    );
    let stored = home
        .client("_retail_")
        .join("Interface/.omaddons/backups")
        .join(&id);
    let manifest: Value =
        serde_json::from_slice(&fs::read(stored.join("manifest.json")).unwrap()).unwrap();
    let files: Vec<&Value> = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "file")
        .collect();
    assert!(files.len() >= 4, "{manifest}");
    assert!(
        files
            .iter()
            .all(|e| e["sha256"].as_str().is_some_and(|h| h.len() == 64)),
        "hashes recorded at import"
    );

    // History, the doorbell and the projection.
    let history = fs::read_to_string(home.state().join("history.jsonl")).unwrap();
    assert!(
        history.lines().any(|l| {
            let line: Value = serde_json::from_str(l).unwrap();
            line["kind"] == "backup"
                && line["backup"]["id"] == json!(id)
                && line["backup"]["kind"] == "imported"
        }),
        "{history}"
    );
    let rung: Value = serde_json::from_slice(&fs::read(&bell).unwrap()).unwrap();
    assert_eq!(rung["command"], "backup import");
    let projection: Value =
        serde_json::from_slice(&fs::read(home.env.cache().join("omaddons/backups.json")).unwrap())
            .unwrap();
    let key = list["data"]["targets"][0]["target"]["key"]
        .as_str()
        .unwrap();
    let covered = projection["targets"][key]["covers"]["backups"].to_string();
    assert!(
        covered.contains(&id),
        "the projection includes it: {covered}"
    );
    assert_eq!(
        projection["targets"][key]["latest_qualifying"]["id"], source_id,
        "the checkpoint is still the original"
    );

    // Retention leaves it alone (the policy's own tests reach past the day it protects).
    let prune = run(&home, &["backup", "prune", "-t", "retail", "--dry-run"]).expect(0);
    let pruned = prune["data"]["targets"][0]["pruned"].to_string();
    assert!(!pruned.contains(&id), "{pruned}");

    // A play session since, then the import restored: the original exactly.
    fs::write(
        wtf_of(&home).join("Account/FAKEACCT/SavedVariables/Foo.lua"),
        "Foo = { changed = true }",
    )
    .unwrap();
    fs::write(wtf_of(&home).join("Config.wtf"), "SET locale \"deDE\"\n").unwrap();
    let r = run(&home, &["backup", "restore", &id, "-t", "retail"]);
    r.expect(0);
    assert_eq!(tree_hash(&wtf_of(&home)), original, "{}", r.context());
    use std::os::unix::fs::PermissionsExt;
    let sealed = fs::metadata(wtf_of(&home).join("Account/FAKEACCT/Sealed")).unwrap();
    assert_eq!(sealed.permissions().mode() & 0o777, 0o500);
    assert!(
        fs::symlink_metadata(wtf_of(&home).join("Account/FAKEACCT/Linked.lua"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// A symlink's target travels in an export exactly as it reads on disk: one a path
/// rebuilt from its components would spell differently (a doubled `/`, a `.` inside it)
/// and one too long for a tar header alike. The archive's own manifest records the target
/// as read, so an export that respelled it would be refused by import (`unsafe_archive`,
/// `manifest_mismatch`): a backup that cannot be brought back. Restored, each link reads
/// as it did.
#[test]
fn a_symlink_target_round_trips_exactly_as_it_reads() {
    let home = Home::new(&["wow"]);
    let wtf = Wtf::at(home.client("_retail_"))
        .config_wtf("SET locale \"enUS\"\n")
        .saved_variables("Foo", 300)
        .build();
    let long = format!("{}SavedVariables/Foo.lua", "./".repeat(60));
    let links = [
        ("Doubled.lua", "SavedVariables//Foo.lua"),
        ("Dotted.lua", "./SavedVariables/./Foo.lua"),
        ("Plain.lua", "SavedVariables/Foo.lua"),
        ("Long.lua", long.as_str()),
    ];
    for (name, target) in links {
        std::os::unix::fs::symlink(target, wtf.join("Account/FAKEACCT").join(name)).unwrap();
    }
    let original = tree_hash(&wtf);
    let created = run(&home, &["backup", "create", "-t", "retail", "--verify"]).expect(0);
    let source = created["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let dir = home.env.root().join("usb");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("links.tar.zst");
    run(
        &home,
        &["backup", "export", &source, &file.display().to_string()],
    )
    .expect(0);

    let backup = imported(&home, &file);
    let id = backup["id"].as_str().unwrap().to_owned();
    for (name, _) in links {
        fs::remove_file(wtf.join("Account/FAKEACCT").join(name)).unwrap();
    }
    let r = run(&home, &["backup", "restore", &id, "-t", "retail"]);
    r.expect(0);
    for (name, target) in links {
        let read = fs::read_link(wtf.join("Account/FAKEACCT").join(name)).unwrap();
        assert_eq!(read.to_str(), Some(target), "{name}: {}", r.context());
    }
    assert_eq!(tree_hash(&wtf), original, "{}", r.context());
}

/// On another machine whose only backup is the imported one, the day's automatic
/// checkpoint is still due: an import says nothing about today's settings.
#[test]
fn an_import_is_never_todays_checkpoint() {
    let source = Home::new(&["wow"]);
    let file = exported_archive(&source);
    let home = Home::new(&["wow"]);
    Wtf::at(home.client("_retail_"))
        .config_wtf("SET locale \"enUS\"\n")
        .saved_variables("Foo", 300)
        .build();
    imported(&home, &file);
    // Byte copies allowed, so only "done today" or "unchanged" could skip it.
    run(&home, &["config", "set", "auto_backup", "daily-copy"]).expect(0);
    let r = run(&home, &["backup", "create", "--auto", "-t", "retail"]);
    let envelope = r.expect(0);
    let made = &envelope["data"]["targets"][0]["backup"];
    assert_eq!(made["kind"], "auto", "{}", r.context());
}

/// Every hostile shape is refused, with no stage left and nothing listed.
#[test]
fn hostile_archives_are_refused_before_anything_is_kept() {
    let home = Home::new(&["wow"]);
    let file = exported_archive(&home);
    let good = tar_entries(&fs::read(&file).unwrap());
    let sv = "WTF/Account/FAKEACCT/SavedVariables";
    let add = |extra: TarEntry| -> Vec<u8> {
        let mut entries = good.clone();
        let at = entries.len() - 1;
        entries.insert(at, extra);
        tar_zst(&entries)
    };
    let changed = |path: &str, change: &dyn Fn(&mut TarEntry)| -> Vec<u8> {
        let mut entries = good.clone();
        for e in &mut entries {
            if e.path() == path {
                change(e);
            }
        }
        tar_zst(&entries)
    };
    let unsafe_archive = "unsafe_archive";

    // Names and kinds.
    refuse(
        &home,
        "traversal",
        &add(TarEntry::file("WTF/../FAKEACCT.txt", b"x")),
        unsafe_archive,
        Some("bad_name"),
    );
    refuse(
        &home,
        "absolute",
        &add(TarEntry::file("/tmp/FakeChar.txt", b"x")),
        unsafe_archive,
        Some("bad_name"),
    );
    refuse(
        &home,
        "device",
        &add(TarEntry::typed("WTF/dev", b'3', "")),
        unsafe_archive,
        Some("unsupported_entry"),
    );
    refuse(
        &home,
        "hardlink",
        &add(TarEntry::typed("WTF/hard", b'1', "WTF/Config.wtf")),
        unsafe_archive,
        Some("unsupported_entry"),
    );
    refuse(
        &home,
        "sparse",
        &add(TarEntry::typed("WTF/sparse", b'S', "")),
        unsafe_archive,
        Some("unsupported_entry"),
    );
    refuse(
        &home,
        "duplicate",
        &add(TarEntry::file("WTF/Config.wtf", b"x")),
        unsafe_archive,
        Some("duplicate_name"),
    );
    refuse(
        &home,
        "case",
        &add(TarEntry::file("WTF/config.WTF", b"x")),
        unsafe_archive,
        Some("duplicate_name"),
    );
    let backup_json = good
        .iter()
        .find(|e| e.path() == "backup.json")
        .unwrap()
        .clone();
    refuse(
        &home,
        "second-meta",
        &add(backup_json),
        unsafe_archive,
        Some("duplicate_metadata"),
    );

    // The payload against its manifest.
    refuse(
        &home,
        "unlisted",
        &add(TarEntry::file(&format!("{sv}/Extra.lua"), b"x")),
        unsafe_archive,
        Some("manifest_mismatch"),
    );
    let without: Vec<TarEntry> = good
        .iter()
        .filter(|e| e.path() != format!("{sv}/Bar.lua"))
        .cloned()
        .collect();
    refuse(
        &home,
        "missing",
        &tar_zst(&without),
        unsafe_archive,
        Some("manifest_mismatch"),
    );
    refuse(
        &home,
        "size",
        &changed(&format!("{sv}/Foo.lua"), &|e| e.body.push(b' ')),
        unsafe_archive,
        Some("manifest_mismatch"),
    );
    refuse(
        &home,
        "mode",
        &changed(&format!("{sv}/Foo.lua"), &|e| e.mode = 0o600),
        unsafe_archive,
        Some("manifest_mismatch"),
    );
    refuse(
        &home,
        "hash",
        &changed(&format!("{sv}/Foo.lua"), &|e| e.body[0] ^= 1),
        unsafe_archive,
        Some("manifest_mismatch"),
    );

    // Symlinks, each listed in the manifest so only the graph is at fault.
    let listed = |links: &[(&str, &str)]| -> Vec<u8> {
        let mut entries = good.clone();
        let at = entries.len() - 1;
        for (path, target) in links {
            entries.insert(at, TarEntry::symlink(&format!("WTF/{path}"), target));
        }
        let entries = with_manifest(&entries, |m| {
            let list = m["entries"].as_array_mut().unwrap();
            for (path, target) in links {
                list.push(
                    json!({"path": path, "kind": "symlink", "size": 0, "mode": 0o777,
                    "mtime_ns": 0, "link": target, "sha256": null}),
                );
            }
        });
        tar_zst(&entries)
    };
    refuse(
        &home,
        "escape",
        &listed(&[
            ("Account/FAKEACCT/l1", "l2"),
            ("Account/FAKEACCT/l2", "../../../FakeRealm"),
        ]),
        unsafe_archive,
        Some("symlink_escape"),
    );
    refuse(
        &home,
        "cycle",
        &listed(&[("Account/FAKEACCT/l1", "l2"), ("Account/FAKEACCT/l2", "l1")]),
        unsafe_archive,
        Some("symlink_escape"),
    );
    // A link placed before a file that would be written through it.
    let mut through = good.clone();
    let at = through.len() - 1;
    through.insert(
        at,
        TarEntry::symlink("WTF/Account/FAKEACCT/Escape", "../../.."),
    );
    through.insert(
        at + 1,
        TarEntry::file("WTF/Account/FAKEACCT/Escape/FakeChar.lua", b"x"),
    );
    let through = with_manifest(&through, |m| {
        let list = m["entries"].as_array_mut().unwrap();
        list.push(
            json!({"path": "Account/FAKEACCT/Escape", "kind": "symlink", "size": 0,
            "mode": 0o777, "mtime_ns": 0, "link": "../../..", "sha256": null}),
        );
    });
    refuse(
        &home,
        "through-link",
        &tar_zst(&through),
        unsafe_archive,
        Some("symlink_escape"),
    );
    assert!(
        !home.env.root().join("FakeChar.lua").exists(),
        "never written through"
    );

    // Bounds.
    let zeros = vec![0u8; 40 * 1024 * 1024];
    refuse(
        &home,
        "bomb",
        &add(TarEntry::file(&format!("{sv}/Bomb.lua"), &zeros)),
        "archive_too_large",
        None,
    );
    let long = format!("WTF/{}", "FakeChar/".repeat(600));
    refuse(
        &home,
        "long-path",
        &add(TarEntry::dir(&long)),
        "archive_too_large",
        None,
    );
    let big_pax = vec![b'a'; 70 * 1024];
    let mut padded = good.clone();
    let at = padded.len() - 1;
    padded.insert(at, pax(&[("comment", &big_pax)]));
    padded.insert(at + 1, TarEntry::file(&format!("{sv}/Pax.lua"), b"x"));
    refuse(&home, "pax", &tar_zst(&padded), "archive_too_large", None);
    refuse(
        &home,
        "window",
        &zstd_huge_window(),
        "bad_archive",
        Some("decoder_window"),
    );
    let mut cut = fs::read(&file).unwrap();
    cut.truncate(cut.len() / 2);
    refuse(&home, "truncated", &cut, "bad_archive", None);
}

/// More entries than an import reads.
/// A regular file with the setuid or setgid bit, in its header and its manifest alike, is
/// refused before anything is kept, its path redacted, its fix saying how to clear the
/// bit; a setgid directory imports with its exact mode.
#[test]
fn a_setuid_or_setgid_file_is_refused_and_a_setgid_directory_imports() {
    let home = Home::new(&["wow"]);
    let file = exported_archive(&home);
    let good = tar_entries(&fs::read(&file).unwrap());
    let wtf_before = tree_hash(&wtf_of(&home));
    // `path` (below `WTF/`) at `mode`, in its tar header and its manifest entry.
    let with_mode = |path: &str, mode: u32| -> Vec<u8> {
        let mut entries = good.clone();
        for e in &mut entries {
            if e.path().trim_end_matches('/') == format!("WTF/{path}") {
                e.mode = mode;
            }
        }
        let entries = with_manifest(&entries, |m| {
            for entry in m["entries"].as_array_mut().unwrap() {
                if entry["path"] == path {
                    entry["mode"] = json!(mode);
                }
            }
        });
        tar_zst(&entries)
    };
    let foo = "Account/FAKEACCT/SavedVariables/Foo.lua";
    for (case, mode, bit) in [
        ("setuid", 0o4755, "setuid bit (mode 4755)"),
        ("setgid", 0o2644, "setgid bit (mode 2644)"),
        ("both", 0o6755, "setuid and setgid bits (mode 6755)"),
    ] {
        let error = refuse(
            &home,
            &format!("{case}.tar.zst"),
            &with_mode(foo, mode),
            "unsafe_archive",
            Some("special_mode"),
        );
        let message = error["message"].as_str().unwrap();
        assert!(message.contains(bit), "{case}: {message}");
        let path = error["details"]["path"].as_str().unwrap();
        assert!(path.starts_with("Account/"), "{case}: {path}");
        assert!(path.ends_with("SavedVariables/Foo.lua"), "{case}: {path}");
        assert!(!path.contains("FAKEACCT"), "{case}: redacted: {path}");
        let fix = error["fix"].as_str().unwrap();
        assert!(fix.contains("chmod u-s,g-s"), "{case}: {fix}");
        assert!(error["fix_argv"].is_null(), "{case}: prose");
        assert_eq!(
            tree_hash(&wtf_of(&home)),
            wtf_before,
            "{case}: WTF/ untouched"
        );
    }

    // A setgid directory is a group-shared folder's, and imports exactly.
    let sv = "Account/FAKEACCT/SavedVariables";
    let dir_file = home.env.root().join("usb").join("setgid-dir.tar.zst");
    fs::write(&dir_file, with_mode(sv, 0o2755)).unwrap();
    let backup = imported(&home, &dir_file);
    let id = backup["id"].as_str().unwrap().to_owned();
    // A stored directory stays owner-writable; its exact mode is the manifest's, which a
    // restore puts back.
    let manifest: Value = serde_json::from_slice(
        &fs::read(
            home.client("_retail_")
                .join("Interface/.omaddons/backups")
                .join(&id)
                .join("manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let recorded = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == sv)
        .unwrap()["mode"]
        .clone();
    assert_eq!(recorded, json!(0o2755), "the manifest keeps the exact mode");
    run(&home, &["backup", "restore", &id, "-t", "retail"]).expect(0);
    let live = wtf_of(&home).join(sv);
    let mode = std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&live).unwrap().permissions())
        & 0o7777;
    assert_eq!(mode, 0o2755, "{}: {mode:04o}", live.display());
}

#[test]
fn an_archive_with_too_many_entries_is_refused() {
    let home = Home::new(&["wow"]);
    let file = exported_archive(&home);
    let mut entries = tar_entries(&fs::read(&file).unwrap());
    let at = entries.len() - 1;
    let many: Vec<TarEntry> = (0..=omaddons::limits::BACKUP_IMPORT_MAX_ENTRIES)
        .map(|i| TarEntry::dir(&format!("WTF/d{i}")))
        .collect();
    entries.splice(at..at, many);
    let error = refuse(&home, "many", &tar_zst(&entries), "archive_too_large", None);
    // One shape for every bound: the number, and the constant's name (no config key).
    assert_eq!(
        error["details"],
        json!({
            "limit": omaddons::limits::BACKUP_IMPORT_MAX_ENTRIES,
            "constant": "BACKUP_IMPORT_MAX_ENTRIES",
        })
    );
}

/// Another family's backup is refused unless `--force`; the fix names a client of that
/// family.
#[test]
fn another_familys_backup_needs_force() {
    let home = Home::new(&["wow", "wow_classic"]);
    let file = exported_archive(&home);
    let arg = file.display().to_string();
    let r = run(&home, &["backup", "import", &arg, "-t", "classic"]);
    let error = &r.envelope()["error"];
    assert_eq!(error["code"], "wrong_flavor", "{}", r.context());
    assert_eq!(
        error["fix_argv"],
        json!(["omaddons", "backup", "import", arg, "-t", "retail"])
    );
    assert_eq!(
        error["alternatives"][0]["argv"],
        json!([
            "omaddons", "backup", "import", arg, "-t", "classic", "--force"
        ])
    );
    let r = run(
        &home,
        &["backup", "import", &arg, "-t", "classic", "--force"],
    );
    assert_eq!(r.expect(0)["data"]["backup"]["kind"], "imported");
    let r = run(&home, &["backup", "import", &arg]);
    assert_eq!(r.expect(2)["error"]["code"], "usage", "-t is required");
}

/// A `WTF/` whose manifest is past the 1 MiB that bounds `backup.json` imports: the
/// manifest has a bound of its own, sized by the entries it may list.
#[test]
fn a_large_wtf_imports() {
    let home = Home::new(&["wow"]);
    let mut wtf = Wtf::at(home.client("_retail_")).config_wtf("SET locale \"enUS\"\n");
    for character in 0..60 {
        for addon in 0..90 {
            wtf = wtf.file(
                &format!(
                    "Account/FAKEACCT/FakeRealm/FakeChar{character}/SavedVariables/Addon{addon}.lua"
                ),
                b"x = 1",
            );
        }
    }
    wtf.build();
    let created = run(&home, &["backup", "create", "-t", "retail", "--verify"]).expect(0);
    let id = created["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let dir = home.env.root().join("usb");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("large.tar.zst");
    run(
        &home,
        &["backup", "export", &id, &file.display().to_string()],
    )
    .expect(0);
    let entries = tar_entries(&fs::read(&file).unwrap());
    let manifest = entries
        .iter()
        .find(|e| e.path() == "manifest.json")
        .unwrap();
    assert!(manifest.body.len() > 1024 * 1024, "{}", manifest.body.len());
    let backup = imported(&home, &file);
    assert_eq!(backup["files"], 5_401);
}
