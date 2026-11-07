//! Details coverage rows: `WTF/` backups (create, list, restore whole and `--addon`,
//! export, import), the filesystem refusals of a restore, and `errors --check`.
//!
//! Every scene holds a `WTF/` under the fake account, realm and character names, so every
//! run is also checked for privacy on both streams, with `--progress json` so progress
//! lines are checked too (common rule 9).

use std::fs;
use std::path::PathBuf;

use omaddons::backup::BackupKind;
use omaddons::error::ErrorCode;

use super::super::support::{Checked, Scene, check};
use super::Row;
use crate::common;
use crate::common::home::Home;
use crate::common::privacy::assert_private_all;
use crate::common::wtf::Wtf;

pub const ROWS: &[Row] = &[
    ERRORS_CHECK,
    NOTHING_TO_BACK_UP,
    WHOLE_RESTORE_REFUSED,
    NOT_WHOLE_OR_UNREADABLE,
    SYMLINKED_WTF,
    ADDON_RESTORE_REFUSED,
    EXPORT_REFUSED,
    IMPORT_REFUSED,
];

/// `omaddons <args> --json --progress json` with `env` (the free space pinned), through
/// the contract's check, and naming no account, realm or character on either stream.
fn run(home: &Home, args: &[&str], env: &[(&str, &str)], code: i32) -> Checked {
    let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    argv.extend(["--json", "--progress", "json"].map(str::to_owned));
    let mut all = vec![("OMADDONS_FS_FREE_MB", "4096")];
    all.extend_from_slice(env);
    let checked = check(home.exec_with(&argv, &all), code);
    assert_private_all(&checked.run.stdout, &checked.run.stderr);
    checked
}

/// A retail `WTF/` with `Config.wtf` and Foo's and Bar's SavedVariables (account-wide
/// and for one character); returns it.
fn retail_wtf(home: &Home) -> PathBuf {
    Wtf::at(home.client("_retail_"))
        .config_wtf("SET locale \"enUS\"\n")
        .saved_variables("Foo", 300)
        .saved_variables("Bar", 120)
        .build()
}

/// A seeded manual backup of retail holding two files; returns its id.
fn seeded(home: &Home) -> String {
    let at = "2026-10-05T08:00:00Z".parse().unwrap();
    common::backups::seed_backup(&home.client("_retail_"), BackupKind::Manual, at, 2).to_string()
}

/// The id of the backup a `backup create` envelope made.
fn made(created: &Checked) -> String {
    created.envelope["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// `<root>/usb`, a folder of the user's own to export to.
fn usb(home: &Home) -> PathBuf {
    let dir = home.env.root().join("usb");
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A BugGrabber database holding a Lua error from Foo a minute old: `errors --check`
/// fails on it as new.
const ERRORS_CHECK: Row = Row {
    id: "errors_check_fails_on_a_new_lua_error",
    keys: &[
        (ErrorCode::CheckFailed, "reason"),
        (ErrorCode::CheckFailed, "target"),
    ],
    run: || {
        use common::addons::{addon_folder, toc};
        use common::buggrabber::{self, BugDb, BugEntry, NOW, TimeKind, Writer, write_files};
        let s = Scene::new();
        s.install_foo();
        let bug_toc = toc("120105", &["## SavedVariables: BugGrabberDB"]);
        addon_folder(
            &s.home.addons("_retail_"),
            "!BugGrabber",
            &[("!BugGrabber.toc", &bug_toc)],
            &[],
        );
        let at = buggrabber::stack_era_c("Foo", "core.lua", 12);
        let db = BugDb::new(Writer::Modern, 10).entry(
            BugEntry::new(
                &format!("{at} attempt to index a nil value (field 'x')"),
                10,
                (TimeKind::Unix, NOW - 60),
            )
            .stack(&[
                format!("{at} in function 'Refresh'"),
                "[C]: in function 'xpcall'".to_owned(),
            ]),
        );
        let wtf = s.home.client("_retail_").join("WTF");
        write_files(&wtf, &[db.file()]);
        let now = [("OMADDONS_NOW", "2026-10-15T18:30:00Z")];
        vec![run(
            &s.home,
            &["errors", "-t", "retail", "--check"],
            &now,
            6,
        )]
    },
};

/// Only a test client has a `WTF/`: a bare create names it, and `-t` naming a client
/// never launched says which.
const NOTHING_TO_BACK_UP: Row = Row {
    id: "a_create_with_no_wtf_to_back_up_says_why",
    keys: &[
        (ErrorCode::TargetNotFound, "candidates"),
        (ErrorCode::NotFound, "target"),
    ],
    run: || {
        let home = Home::new(&["wow", "wowt"]);
        Wtf::at(home.client("_ptr_"))
            .saved_variables("Foo", 64)
            .build();
        fs::remove_dir_all(home.client("_retail_").join("WTF")).unwrap();
        vec![
            run(&home, &["backup", "create", "--verify"], &[], 3),
            run(&home, &["backup", "create", "-t", "retail"], &[], 3),
        ]
    },
};

/// A whole restore refused before it copies: a backup of another client, an unknown id
/// (and its listing), a partial backup, and a filesystem that cannot exchange.
const WHOLE_RESTORE_REFUSED: Row = Row {
    id: "a_whole_restore_is_refused_before_it_copies",
    keys: &[
        (ErrorCode::NotFound, "id"),
        (ErrorCode::NotFound, "owner"),
        (ErrorCode::NotFound, "target"),
        (ErrorCode::NotFound, "newest_ids"),
        (ErrorCode::NotFound, "prefix"),
        (ErrorCode::Usage, "id"),
        (ErrorCode::Usage, "partial"),
        (ErrorCode::FilesystemUnsupported, "backup_dir"),
        (ErrorCode::FilesystemUnsupported, "reason"),
    ],
    run: || {
        let s = Scene::new();
        let home = &s.home;
        retail_wtf(home);
        let id = seeded(home);
        // The files a crashed partial restore displaced: some addons' settings only.
        let at = "2026-10-06T08:00:00Z".parse().unwrap();
        let mut meta = common::backups::meta(BackupKind::Manual, at, 1);
        meta.partial = true;
        let partial = common::backups::seed_meta(&home.client("_retail_"), &meta).to_string();
        let no_exchange = [("OMADDONS_FAILPOINTS", "fsx.renameat2.einval")];
        vec![
            run(home, &["backup", "restore", &id, "-t", "classic"], &[], 3),
            run(home, &["backup", "restore", "01ZZZZZZ"], &[], 3),
            run(home, &["backup", "list", "01ZZZZZZ"], &[], 3),
            run(
                home,
                &["backup", "restore", &partial, "-t", "retail"],
                &[],
                2,
            ),
            run(
                home,
                &["backup", "restore", &id, "-t", "retail"],
                &no_exchange,
                4,
            ),
        ]
    },
};

/// A backup whose `WTF/` lacks a file its manifest lists is not whole; one whose folder
/// cannot be opened cannot be reserved, so it is gone.
const NOT_WHOLE_OR_UNREADABLE: Row = Row {
    id: "a_backup_not_whole_or_not_reservable_is_named",
    keys: &[
        (ErrorCode::StateCorrupt, "backup"),
        (ErrorCode::NotFound, "backup"),
    ],
    run: || {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new(&["wow"]);
        retail_wtf(&home);
        let client = home.client("_retail_");
        let torn = seeded(&home);
        let dir = |id: &str| common::backups::backup_dir(&client, &id.parse().unwrap());
        let sv = "WTF/Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables";
        fs::remove_file(dir(&torn).join(sv).join("Addon1.lua")).unwrap();
        let not_whole = run(
            &home,
            &["backup", "restore", &torn, "-t", "retail", "--dry-run"],
            &[],
            1,
        );
        // Listed (its name is a backup id), but its reader reservation cannot open it.
        let sealed = seeded(&home);
        let mode = |m| fs::set_permissions(dir(&sealed), fs::Permissions::from_mode(m));
        mode(0o000).unwrap();
        let gone = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(
                &home,
                &["backup", "restore", &sealed, "-t", "retail"],
                &[],
                3,
            )
        }));
        // The tempdir is deleted only once the folder is the owner's again.
        mode(0o755).unwrap();
        let gone = gone.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        vec![not_whole, gone]
    },
};

/// The client's `WTF/` is a symlink to a sync folder: a restore would replace the link,
/// so it is refused, naming the backup folder to copy from by hand.
const SYMLINKED_WTF: Row = Row {
    id: "a_symlinked_wtf_is_refused_naming_the_backup_folder",
    keys: &[
        (ErrorCode::ScopeDenied, "backup_dir"),
        (ErrorCode::ScopeDenied, "reason"),
    ],
    run: || {
        let home = Home::new(&["wow"]);
        let wtf = retail_wtf(&home);
        let id = seeded(&home);
        let away = home.env.root().join("sync/WTF");
        fs::create_dir_all(away.parent().unwrap()).unwrap();
        fs::rename(&wtf, &away).unwrap();
        std::os::unix::fs::symlink(&away, &wtf).unwrap();
        vec![run(
            &home,
            &["backup", "restore", &id, "-t", "retail", "--dry-run"],
            &[],
            4,
        )]
    },
};

/// `backup restore --addon` of a seeded backup (its record: WeakAuras owned WeakAuras and
/// WeakAurasOptions): an unknown name, an addon neither side holds files of, and a
/// capture-time folder another addon owns now.
const ADDON_RESTORE_REFUSED: Row = Row {
    id: "an_addon_restore_names_what_it_cannot_find_or_split",
    keys: &[
        (ErrorCode::NotFound, "addon"),
        (ErrorCode::NotFound, "addons"),
        (ErrorCode::NotFound, "target"),
        (ErrorCode::RefAmbiguous, "addon"),
        (ErrorCode::RefAmbiguous, "candidates"),
        (ErrorCode::RefAmbiguous, "current_owners"),
        (ErrorCode::RefAmbiguous, "current_owners[].folder"),
        (ErrorCode::RefAmbiguous, "current_owners[].owner"),
        (ErrorCode::RefAmbiguous, "target"),
    ],
    run: || {
        use common::addons::{addon_folder, toc};
        let s = Scene::new();
        let home = &s.home;
        retail_wtf(home);
        let id = seeded(home);
        let restore = |addon: &str, more: &[&str], code| {
            let mut args = vec!["backup", "restore", id.as_str(), "-t", "retail"];
            args.extend(["--addon", addon]);
            args.extend_from_slice(more);
            run(home, &args, &[], code)
        };
        let unknown = restore("Nothing", &["--dry-run"], 3);
        // The backup holds Addon0 and Addon1, the client Foo and Bar: none of WeakAuras.
        let nothing = restore("WeakAuras", &["--dry-run"], 3);
        let src = home.env.root().join("src");
        let options = addon_folder(
            &src,
            "WeakAurasOptions",
            &[("WeakAurasOptions.toc", &toc("120105", &[]))],
            &[],
        );
        let options = options.display().to_string();
        run(home, &["install", &options, "-t", "retail"], &[], 0);
        let split = restore("WeakAuras", &[], 3);
        vec![unknown, nothing, split]
    },
};

/// A verified backup exported: again to the same file, then with a file of the backup
/// damaged (its size kept), then with another of its files gone.
const EXPORT_REFUSED: Row = Row {
    id: "an_export_never_replaces_a_file_or_ships_damage",
    keys: &[
        (ErrorCode::FileExists, "file"),
        (ErrorCode::HashMismatch, "backup"),
        (ErrorCode::Io, "backup"),
        (ErrorCode::Io, "path"),
    ],
    run: || {
        let home = Home::new(&["wow"]);
        retail_wtf(&home);
        let created = run(
            &home,
            &["backup", "create", "-t", "retail", "--verify"],
            &[],
            0,
        );
        let id = made(&created);
        let dir = usb(&home);
        let file = |name: &str| dir.join(name).display().to_string();
        let export =
            |name: &str, code| run(&home, &["backup", "export", &id, &file(name)], &[], code);
        export("first.tar.zst", 0);
        let exists = export("first.tar.zst", 4);
        let sv = common::backups::backup_dir(&home.client("_retail_"), &id.parse().unwrap())
            .join("WTF/Account/FAKEACCT/SavedVariables");
        let foo = sv.join("Foo.lua");
        let mut bytes = fs::read(&foo).unwrap();
        bytes[0] ^= 1;
        fs::write(&foo, &bytes).unwrap();
        let damaged = export("damaged.tar.zst", 1);
        // Bar.lua comes before Foo.lua: the export stops at it first.
        fs::remove_file(sv.join("Bar.lua")).unwrap();
        let changed = export("changed.tar.zst", 1);
        vec![exists, damaged, changed]
    },
};

/// A retail backup exported, then imported into Mists Classic (another family) and,
/// with an entry its manifest does not list, into retail.
const IMPORT_REFUSED: Row = Row {
    id: "an_import_refuses_another_family_and_a_hostile_entry",
    keys: &[
        (ErrorCode::WrongFlavor, "family"),
        (ErrorCode::WrongFlavor, "target_family"),
        (ErrorCode::UnsafeArchive, "path"),
        (ErrorCode::UnsafeArchive, "reason"),
    ],
    run: || {
        use common::archives::{TarEntry, tar_entries, tar_zst};
        let home = Home::new(&["wow", "wow_classic"]);
        retail_wtf(&home);
        let created = run(
            &home,
            &["backup", "create", "-t", "retail", "--verify"],
            &[],
            0,
        );
        let id = made(&created);
        let dir = usb(&home);
        let file = dir.join("retail-wtf.tar.zst");
        let arg = file.display().to_string();
        run(&home, &["backup", "export", &id, &arg], &[], 0);
        let family = run(&home, &["backup", "import", &arg, "-t", "classic"], &[], 4);
        let mut entries = tar_entries(&fs::read(&file).unwrap());
        let last = entries.len() - 1;
        let extra = "WTF/Account/FAKEACCT/SavedVariables/Extra.lua";
        entries.insert(last, TarEntry::file(extra, b"x"));
        let hostile = dir.join("hostile.tar.zst");
        fs::write(&hostile, tar_zst(&entries)).unwrap();
        let hostile = hostile.display().to_string();
        let unlisted = run(
            &home,
            &["backup", "import", &hostile, "-t", "retail"],
            &[],
            4,
        );
        vec![family, unlisted]
    },
};
