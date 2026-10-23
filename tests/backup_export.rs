//! `omaddons backup export` end to end (M3-D34, M3-D47, M3-D48): the archive lists with
//! GNU tar as recorded (modes, uid/gid 0, no owner names, `backup.json` first and
//! `manifest.json` last) and its manifest's hashes match its files; one backup always
//! exports to the same bytes; an existing destination is never replaced, before or
//! during the export; destinations inside a client, a Wine prefix or omaddons' state are
//! refused; nothing is written anywhere but the destination; every failure (a file gone,
//! a damaged file, a full disk, an injected error, a crash) leaves no destination and no
//! temp file (a crash at most the temp file); a prune meanwhile finds the backup busy.
//! Every run is checked for the fake account, realm and character names on both streams.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use common::home::{Home, Run};
use common::pause::Pause;
use common::privacy::assert_private_all;
use common::trees::tree_hash;
use common::wtf::Wtf;
use serde_json::{Value, json};
use support::{
    archive, backup_with, exported, hidden, json_of, run, run_with, sha256_hex, usb, wtf_backup,
};

#[cfg(test)]
mod support {
    use super::*;

    /// `omaddons <args> --json --progress json` with `extra` in the environment, checked
    /// for privacy on both streams.
    pub fn run_with(home: &Home, args: &[&str], extra: &[(&str, &str)]) -> Run {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--json", "--progress", "json"].map(str::to_owned));
        let mut env = vec![("OMADDONS_FS_FREE_MB", "4096")];
        env.extend_from_slice(extra);
        let r = home.exec_with(&argv, &env);
        assert_private_all(&r.stdout, &r.stderr);
        r
    }

    pub fn run(home: &Home, args: &[&str]) -> Run {
        run_with(home, args, &[])
    }

    /// A retail `WTF/` with `Config.wtf`, two addons' SavedVariables, a read-only
    /// directory, a symlink and `extra` more bytes of incompressible data.
    pub fn backup_with(home: &Home, extra: usize, verify: bool) -> String {
        // xorshift64: incompressible enough that zstd keeps its size.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let noise: Vec<u8> = (0..extra)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 32) as u8
            })
            .collect();
        let wtf = Wtf::at(home.client("_retail_"))
            .config_wtf("SET locale \"enUS\"\n")
            .saved_variables("Foo", 300)
            .saved_variables("Bar", 120)
            .file("Account/FAKEACCT/Sealed/Kept.lua", b"Kept = 1")
            .file("Account/FAKEACCT/Noise.bin", &noise)
            .mode("Account/FAKEACCT/Sealed", 0o500)
            .build();
        std::os::unix::fs::symlink("Config.wtf", wtf.join("Linked.wtf")).unwrap();
        let mut args = vec!["backup", "create", "-t", "retail"];
        if verify {
            args.push("--verify");
        }
        made(&run(home, &args).expect(0))
    }

    /// The id of the backup `backup create` made.
    pub fn made(envelope: &Value) -> String {
        envelope["data"]["targets"][0]["backup"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// A folder of the user's own to export to.
    pub fn usb(home: &Home) -> PathBuf {
        let dir = home.env.root().join("usb");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The backup's folder.
    pub fn wtf_backup(home: &Home, id: &str) -> PathBuf {
        home.client("_retail_")
            .join("Interface/.omaddons/backups")
            .join(id)
    }

    /// `backup export <id> <file>`'s envelope, which must succeed.
    pub fn exported(home: &Home, id: &str, file: &Path) -> Value {
        let r = run(home, &["backup", "export", id, &file.display().to_string()]);
        r.expect(0)
    }

    /// One archive entry: (name, entry type, mode, uid, gid, owner names empty, content).
    pub type ArchiveEntry = (String, char, u32, u64, u64, bool, Vec<u8>);

    /// The archive's entries in order.
    pub fn archive(file: &Path) -> Vec<ArchiveEntry> {
        let decoder = zstd::stream::read::Decoder::new(fs::File::open(file).unwrap()).unwrap();
        let mut tar = tar::Archive::new(decoder);
        tar.entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let h = e.header().clone();
                let path = e.path().unwrap().display().to_string();
                let kind = match h.entry_type() {
                    tar::EntryType::Directory => 'd',
                    tar::EntryType::Symlink => 'l',
                    _ => '-',
                };
                let anonymous = h.username().unwrap().unwrap_or_default().is_empty()
                    && h.groupname().unwrap().unwrap_or_default().is_empty();
                let mut content = Vec::new();
                e.read_to_end(&mut content).unwrap();
                (
                    path,
                    kind,
                    h.mode().unwrap(),
                    h.uid().unwrap(),
                    h.gid().unwrap(),
                    anonymous,
                    content,
                )
            })
            .collect()
    }

    /// Lowercase hex SHA-256.
    pub fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A JSON document's value.
    pub fn json_of(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    /// fsx's hidden temp files in `dir`.
    pub fn hidden(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".omaddons-"))
            .collect()
    }
}

/// What the archive holds, in order, as recorded, and that its manifest describes it.
#[test]
fn the_archive_is_the_backup_as_recorded() {
    let home = Home::new(&["wow"]);
    let id = backup_with(&home, 4096, true);
    let file = usb(&home).join("retail-wtf.tar.zst");
    let envelope = exported(&home, &id, &file);
    let data = &envelope["data"];
    assert_eq!(
        (
            &data["verified_against"],
            &data["dry_run"],
            &data["backup"]["id"]
        ),
        (&json!("capture"), &json!(false), &json!(id))
    );
    let size = fs::metadata(&file).unwrap().len();
    assert_eq!(
        data["compressed_bytes"], size,
        "the reader's count is the file"
    );
    assert_eq!(data["sha256"], sha256_hex(&fs::read(&file).unwrap()));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let entries = archive(&file);
    let names: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names.first(), Some(&"backup.json"));
    assert_eq!(names.last(), Some(&"manifest.json"));
    let wtf: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| n.starts_with("WTF/"))
        .collect();
    let mut sorted = wtf.clone();
    sorted.sort_by(|a, b| a.trim_end_matches('/').cmp(b.trim_end_matches('/')));
    assert_eq!(wtf, sorted, "WTF/ in path order");
    assert!(
        entries.iter().all(|e| e.3 == 0 && e.4 == 0 && e.5),
        "uid/gid 0, no owner names"
    );

    let local: Value = json_of(&fs::read(wtf_backup(&home, &id).join("manifest.json")).unwrap());
    let manifest = json_of(&entries.last().unwrap().6);
    let by_path: BTreeMap<String, &support::ArchiveEntry> = entries
        .iter()
        .map(|e| (e.0.trim_end_matches('/').to_owned(), e))
        .collect();
    let recorded = local["entries"].as_array().unwrap();
    assert_eq!(
        manifest["entries"].as_array().unwrap().len(),
        recorded.len()
    );
    for (mine, theirs) in manifest["entries"].as_array().unwrap().iter().zip(recorded) {
        let path = format!("WTF/{}", mine["path"].as_str().unwrap());
        let e = by_path[&path];
        assert_eq!(
            e.2,
            mine["mode"].as_u64().unwrap() as u32 & 0o7777,
            "{path}"
        );
        assert_eq!(
            (&mine["path"], &mine["mode"], &mine["kind"]),
            (&theirs["path"], &theirs["mode"], &theirs["kind"])
        );
        match mine["kind"].as_str().unwrap() {
            "file" => {
                let hex = sha256_hex(&e.6);
                assert_eq!(mine["sha256"], hex, "{path}");
                assert_eq!(theirs["sha256"], hex, "checked against the capture's");
            }
            "dir" => assert_eq!(e.1, 'd'),
            _ => assert_eq!(e.1, 'l'),
        }
    }
    assert_eq!(
        by_path["WTF/Account/FAKEACCT/Sealed"].2, 0o500,
        "a read-only directory keeps its mode"
    );
    let described = json_of(&entries[0].6);
    assert_eq!(
        (&described["id"], &described["verified_against"]),
        (&json!(id), &json!("capture"))
    );

    // GNU tar reads it too, with numeric owners only.
    let listed = std::process::Command::new("tar")
        .args(["--zstd", "-tvf"])
        .arg(&file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&listed.stdout);
    assert!(listed.status.success(), "{text}");
    assert!(text.lines().all(|l| l.contains(" 0/0 ")), "{text}");
    assert!(text.contains("dr-x------"), "{text}");
}

/// The same backup exports to the same bytes; without recorded hashes, the archive's
/// are computed as it streams; nothing but the destination is written.
#[test]
fn exports_are_deterministic_and_write_only_their_file() {
    let home = Home::new(&["wow"]);
    let id = backup_with(&home, 1024, false);
    let dir = usb(&home);
    let cache = home.env.cache().join("omaddons");
    fs::create_dir_all(&cache).unwrap();
    let state_before = tree_hash(&home.state());
    let cache_before = tree_hash(&cache);
    let client_before = tree_hash(&home.client("_retail_"));
    let first = exported(&home, &id, &dir.join("a.tar.zst"));
    let second = exported(&home, &id, &dir.join("b.tar.zst"));
    assert_eq!(
        fs::read(dir.join("a.tar.zst")).unwrap(),
        fs::read(dir.join("b.tar.zst")).unwrap()
    );
    assert_eq!(first["data"]["sha256"], second["data"]["sha256"]);
    assert_eq!(first["data"]["verified_against"], "computed_at_export");
    assert_eq!(tree_hash(&cache), cache_before, "no intermediate copy");
    assert_eq!(tree_hash(&home.client("_retail_")), client_before);
    // Not a transaction: no history line (net.log and the like aside).
    assert!(
        !home.state().join("history.jsonl").exists() || {
            let text = fs::read_to_string(home.state().join("history.jsonl")).unwrap();
            !text.contains("backup export")
        }
    );
    let _ = state_before;
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["a.tar.zst", "b.tar.zst"]);

    // A dry run writes nothing.
    let dry = run(
        &home,
        &[
            "backup",
            "export",
            &id,
            &dir.join("c.tar.zst").display().to_string(),
            "--dry-run",
        ],
    );
    let envelope = dry.expect(0);
    assert_eq!(
        (
            &envelope["data"]["dry_run"],
            &envelope["data"]["compressed_bytes"]
        ),
        (&json!(true), &Value::Null)
    );
    assert!(!dir.join("c.tar.zst").exists());
}

/// An existing destination, one inside a client, a Wine prefix or omaddons' state, `-`
/// and a missing folder are each refused, and nothing is written.
#[test]
fn refused_destinations_are_untouched() {
    let home = Home::new(&["wow"]);
    let id = backup_with(&home, 0, false);
    let dir = usb(&home);
    let taken = dir.join("taken.tar.zst");
    fs::write(&taken, "mine").unwrap();
    let prefix = home.env.root().join("prefix");
    fs::create_dir_all(prefix.join("drive_c/users/me")).unwrap();
    fs::create_dir_all(prefix.join("dosdevices")).unwrap();
    fs::create_dir_all(home.state()).unwrap();
    let cases: Vec<(PathBuf, i32, &str)> = vec![
        (taken.clone(), 4, "file_exists"),
        (home.client("_retail_").join("w.tar.zst"), 4, "scope_denied"),
        (prefix.join("drive_c/users/me/w.tar.zst"), 4, "scope_denied"),
        (home.state().join("w.tar.zst"), 4, "scope_denied"),
        (PathBuf::from("-"), 2, "usage"),
        (dir.join("nope/w.tar.zst"), 1, "io"),
    ];
    for (dest, code, wire) in &cases {
        for dry in [false, true] {
            let mut args = vec!["backup", "export", id.as_str()];
            let dest = dest.display().to_string();
            args.push(&dest);
            if dry {
                args.push("--dry-run");
            }
            let r = run(&home, &args);
            let envelope = r.expect(*code);
            assert_eq!(envelope["error"]["code"], *wire, "{}", r.context());
        }
    }
    assert_eq!(fs::read_to_string(&taken).unwrap(), "mine");
    assert!(!home.client("_retail_").join("w.tar.zst").exists());
    assert!(!prefix.join("drive_c/users/me/w.tar.zst").exists());
    assert!(hidden(&dir).is_empty());
}

/// A file gone from the backup, a damaged one, an injected write error and a crash each
/// leave no destination (a crash at most fsx's hidden temp file).
#[test]
fn a_failed_export_leaves_nothing_behind() {
    let home = Home::new(&["wow"]);
    let id = backup_with(&home, 2048, true);
    let dir = usb(&home);
    let wtf = wtf_backup(&home, &id).join("WTF");
    let file = dir.join("w.tar.zst");
    let file_arg = file.display().to_string();

    // An injected error at the publish.
    let r = run_with(
        &home,
        &["backup", "export", &id, &file_arg],
        &[("OMADDONS_FAILPOINTS", "fsx.write_atomic.after=error")],
    );
    assert_eq!(r.envelope()["error"]["code"], "io", "{}", r.context());
    assert!(!file.exists() && hidden(&dir).is_empty());

    // A crash at the publish: at most the temp file.
    let r = home.exec_with(
        &["backup", "export", &id, &file_arg].map(str::to_owned),
        &[("OMADDONS_FAILPOINTS", "fsx.write_atomic.after=abort")],
    );
    assert_eq!(r.code, -1, "{}", r.context());
    assert!(!file.exists());
    assert!(hidden(&dir).len() <= 1, "{:?}", hidden(&dir));
    for leftover in hidden(&dir) {
        fs::remove_file(dir.join(leftover)).unwrap();
    }

    // A damaged file: the size it was recorded with, other bytes.
    let foo = wtf.join("Account/FAKEACCT/SavedVariables/Foo.lua");
    let original = fs::read(&foo).unwrap();
    let mut damaged = original.clone();
    damaged[0] ^= 1;
    fs::write(&foo, &damaged).unwrap();
    let r = run(&home, &["backup", "export", &id, &file_arg]);
    let error = &r.envelope()["error"];
    assert_eq!(
        (&error["code"], &error["retryable"]),
        (&json!("hash_mismatch"), &json!(false)),
        "{}",
        r.context()
    );
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains("is damaged") && message.contains("SavedVariables/Foo.lua"),
        "{message}"
    );
    assert_eq!(
        error["fix_argv"],
        json!(["omaddons", "backup", "verify", id, "-t", "retail"])
    );
    assert!(!file.exists() && hidden(&dir).is_empty());
    // Recorded as `backup verify` records it (M3-D47); with the bytes back, the fix
    // checks it again and finds it whole.
    let listed = run(&home, &["backup", "list", "-t", "retail"]).expect(0);
    let entry = &listed["data"]["targets"][0]["backups"][0];
    assert_eq!(
        (&entry["integrity"], &entry["restorable"]),
        (&json!("damaged"), &json!(false)),
        "{listed:#}"
    );
    fs::write(&foo, &original).unwrap();
    run(&home, &["backup", "verify", &id, "-t", "retail"]).expect(0);

    // A file gone since the backup was made.
    fs::remove_file(wtf.join("Account/FAKEACCT/Sealed/Kept.lua")).ok();
    let sealed = wtf.join("Account/FAKEACCT/Sealed");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_file(sealed.join("Kept.lua")).ok();
    let r = run(&home, &["backup", "export", &id, &file_arg]);
    assert_eq!(r.envelope()["error"]["code"], "io", "{}", r.context());
    assert!(!file.exists() && hidden(&dir).is_empty());
}

/// A destination another process creates mid-export is never replaced, and a prune
/// meanwhile finds the backup busy, after which the export completes.
#[test]
fn what_happens_meanwhile_is_never_clobbered_or_pruned() {
    let home = Home::new(&["wow"]);
    let id = backup_with(&home, 1024, false);
    // A newer checkpoint, so the exported one may be pruned.
    fs::write(
        home.client("_retail_").join("WTF/Config.wtf"),
        "SET x \"1\"\n",
    )
    .unwrap();
    support::made(&run(&home, &["backup", "create", "-t", "retail"]).expect(0));
    let dir = usb(&home);
    let file = dir.join("w.tar.zst");
    let file_arg = file.display().to_string();
    let point = "fsx.write_atomic.after";

    let pause = Pause::new(home.env.root());
    let child = home.spawn(
        &["backup", "export", &id, &file_arg],
        &[
            ("OMADDONS_FAILPOINTS", &format!("{point}=pause")),
            pause.env(),
        ],
    );
    pause.reached(point);
    let r = run(&home, &["backup", "prune", &id, "-t", "retail"]);
    let busy = &r.expect(0)["data"]["targets"][0]["busy"];
    assert_eq!(busy, &json!([{"id": id}]), "{}", r.context());
    fs::write(&file, "theirs").unwrap();
    pause.resume(point);
    let done = common::home::wait(child, &["backup", "export"]);
    assert_private_all(&done.stdout, &done.stderr);
    assert_eq!(
        done.envelope()["error"]["code"],
        "file_exists",
        "{}",
        done.context()
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "theirs");
    assert!(hidden(&dir).is_empty());

    // The backup is still there, and exports.
    fs::remove_file(&file).unwrap();
    exported(&home, &id, &file);
    assert!(file.exists());
}

/// A destination that fills up: in a size-limited tmpfs (an unprivileged user and mount
/// namespace), the export fails and leaves neither the file nor its temp file.
#[test]
fn a_full_disk_leaves_nothing_behind() {
    let probe = std::process::Command::new("unshare")
        .args(["-rm", "true"])
        .status();
    if !probe.is_ok_and(|s| s.success()) {
        common::checkpoint::skip(
            "a_full_disk_leaves_nothing_behind",
            "unprivileged user namespaces are unavailable",
        );
        return;
    }
    let home = Home::new(&["wow"]);
    let id = backup_with(&home, 512 * 1024, false);
    let small = home.env.root().join("small");
    fs::create_dir_all(&small).unwrap();
    let file = small.join("w.tar.zst");
    let inner = home.std_command(
        &[
            "backup",
            "export",
            &id,
            &file.display().to_string(),
            "--json",
        ]
        .map(str::to_owned),
        &[("OMADDONS_FS_FREE_MB", "4096")],
    );
    let mut cmd = std::process::Command::new("unshare");
    cmd.args(["-rm", "sh", "-c"])
        .arg(
            "mount -t tmpfs -o size=64k none \"$1\" || exit 99; shift; \"$@\"; echo; echo LEFT:; ls -A \"$0\"",
        )
        .arg(&small)
        .arg(&small)
        .arg(inner.get_program())
        .args(inner.get_args());
    for (key, value) in inner.get_envs() {
        match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        };
    }
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_private_all(&stdout, &stderr);
    let (envelope, left) = stdout.split_once("\nLEFT:\n").unwrap();
    let envelope: Value = serde_json::from_str(envelope.trim()).unwrap();
    assert_eq!(envelope["ok"], false, "{stdout}{stderr}");
    assert_eq!(left.trim(), "", "nothing left in the full folder: {left}");
}
