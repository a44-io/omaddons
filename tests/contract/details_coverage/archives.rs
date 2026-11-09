//! Details coverage rows: archives: hostile zips, a source that changed between
//! inspection and extraction, size limits on live folders and backups, and archives
//! whose bytes do not hash to what they should.

use std::fs;

use omaddons::error::ErrorCode;

use super::super::support::{Scene, check};
use super::Row;
use crate::common;
use common::archives::{RawEntry, raw_zip};
use common::home::{Home, wait};
use common::pause::Pause;

pub const ROWS: &[Row] = &[
    HOSTILE_ZIPS,
    CHANGED_BEFORE_EXTRACTION,
    LIVE_FOLDER_OVER_THE_ENTRY_LIMIT,
    BACKUP_PATH_OVER_ITS_BOUND,
    DAMAGED_CACHED_ARCHIVE,
    TUKUI_ETAG_DISAGREES,
    LOCKED_ARCHIVE_DIFFERS,
];

/// gh:o/foo locked by `sync --locked` to an archive other than the one its release
/// serves (omad-t8m3).
const LOCKED_ARCHIVE_DIFFERS: Row = Row {
    id: "a_locked_sync_says_the_archive_is_not_the_locked_one",
    keys: &[(ErrorCode::HashMismatch, "reason")],
    run: || {
        let s = Scene::new();
        let file = s.home.write_desired(&format!(
            "schema_version = \"1.0\"\n\n[[family.mainline.addon]]\nname = \"Foo\"\nref = \"gh:o/foo\"\nversion = \"v1\"\nsha256 = \"{}\"\n",
            "0".repeat(64)
        ));
        let file = file.display().to_string();
        vec![s.json(&["sync", &file, "--locked", "-t", "retail"], 1)]
    },
};

/// A traversal, a name listed twice and a ratio bomb, each refused with what it measured.
const HOSTILE_ZIPS: Row = Row {
    id: "hostile_zips_are_refused_with_the_entry_and_what_was_measured",
    keys: &[
        (ErrorCode::UnsafeArchive, "compressed"),
        (ErrorCode::UnsafeArchive, "distinct"),
        (ErrorCode::UnsafeArchive, "entry"),
        (ErrorCode::UnsafeArchive, "limit"),
        (ErrorCode::UnsafeArchive, "measured"),
        (ErrorCode::UnsafeArchive, "reason"),
    ],
    run: || {
        let s = Scene::new();
        // Past ARCHIVE_RATIO_FLOOR (32 MiB), so the ratio applies.
        let zeros = vec![0u8; 40 << 20];
        let cases = [
            ("traversal", vec![RawEntry::stored("Foo/../../x", b"x")]),
            (
                "duplicate",
                vec![
                    RawEntry::stored("Foo/a.lua", b"1"),
                    RawEntry::stored("Foo/a.lua", b"2"),
                ],
            ),
            (
                "ratio-bomb",
                vec![RawEntry::deflated("Foo/zeros.bin", &zeros)],
            ),
        ];
        cases
            .into_iter()
            .map(|(name, mut entries)| {
                entries.insert(
                    0,
                    RawEntry::stored("Foo/Foo.toc", b"## Interface: 120105\n"),
                );
                let zip = s.home.env.root().join("dl").join(format!("{name}.zip"));
                fs::write(&zip, raw_zip(&entries)).unwrap();
                let zip = zip.display().to_string();
                let args = [
                    "install",
                    "gh:o/foo",
                    "--file",
                    &zip,
                    "-t",
                    "retail",
                    "--offline",
                ];
                s.json(&args, 4)
            })
            .collect()
    },
};

/// A local folder install stopped after inspection, one of its files rewritten, then let
/// go: extraction refuses it with both measurements.
const CHANGED_BEFORE_EXTRACTION: Row = Row {
    id: "a_folder_changed_between_inspection_and_extraction_is_refused",
    keys: &[
        (ErrorCode::BadArchive, "actual"),
        (ErrorCode::BadArchive, "actual.sha256"),
        (ErrorCode::BadArchive, "actual.size"),
        (ErrorCode::BadArchive, "expected"),
        (ErrorCode::BadArchive, "expected.sha256"),
        (ErrorCode::BadArchive, "expected.size"),
        (ErrorCode::BadArchive, "path"),
        (ErrorCode::BadArchive, "reason"),
    ],
    run: || {
        let s = Scene::new();
        let dir = s.home.env.root().join("src/Mine");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Mine.toc"),
            "## Interface: 120105\n## Version: 1\n",
        )
        .unwrap();
        fs::write(dir.join("core.lua"), "-- one\n").unwrap();
        let source = format!("file:{}", dir.display());
        let args = ["install", source.as_str(), "-t", "retail"];
        let pause = Pause::new(s.home.env.root());
        let child = s.home.spawn(
            &args,
            &[
                ("OMADDONS_FAILPOINTS", "engine.stage.begin=pause"),
                pause.env(),
            ],
        );
        pause.reached("engine.stage.begin");
        fs::write(dir.join("core.lua"), "-- two, and longer\n").unwrap();
        pause.resume("engine.stage.begin");
        let argv = [&args[..], &["--json"]].concat();
        vec![check(wait(child, &argv), 1)]
    },
};

/// An unmanaged Foo of nine files adopted under `limits.archive_max_entries = 4`.
const LIVE_FOLDER_OVER_THE_ENTRY_LIMIT: Row = Row {
    id: "a_live_folder_over_the_entry_limit_names_the_folder_and_its_count",
    keys: &[
        (ErrorCode::ArchiveTooLarge, "config_key"),
        (ErrorCode::ArchiveTooLarge, "folder"),
        (ErrorCode::ArchiveTooLarge, "limit"),
        (ErrorCode::ArchiveTooLarge, "measured"),
        (ErrorCode::ArchiveTooLarge, "reason"),
    ],
    run: || {
        let s = Scene::new();
        let foo = s.home.addons("_retail_").join("Foo");
        fs::create_dir_all(&foo).unwrap();
        fs::write(foo.join("Foo.toc"), "## Interface: 120105\n## Version: 1\n").unwrap();
        for n in 0..8 {
            fs::write(foo.join(format!("extra{n}.lua")), "--\n").unwrap();
        }
        s.json(&["config", "set", "limits.archive_max_entries", "4"], 0);
        vec![s.json(&["adopt", "--ref", "gh:o/foo", "Foo", "-t", "retail"], 4)]
    },
};

/// An exported backup with a path past `BACKUP_IMPORT_MAX_PATH` added, a bound no config
/// raises.
const BACKUP_PATH_OVER_ITS_BOUND: Row = Row {
    id: "a_backup_path_over_a_fixed_bound_names_the_constant",
    keys: &[
        (ErrorCode::ArchiveTooLarge, "constant"),
        (ErrorCode::ArchiveTooLarge, "limit"),
    ],
    run: || {
        use common::archives::{TarEntry, tar_entries, tar_zst};
        use omaddons::backup::BackupKind;
        let s = Scene::new();
        let client = s.home.client("_retail_");
        let at = "2026-10-05T08:00:00Z".parse().unwrap();
        let id = common::backups::seed_backup(&client, BackupKind::Manual, at, 2).to_string();
        let dir = s.home.env.root().join("usb");
        fs::create_dir_all(&dir).unwrap();
        let export = dir.join("retail-wtf.tar.zst");
        s.json(&["backup", "export", &id, &export.display().to_string()], 0);
        let mut entries = tar_entries(&fs::read(&export).unwrap());
        let long = format!(
            "WTF/{}",
            "a".repeat(omaddons::limits::BACKUP_IMPORT_MAX_PATH as usize)
        );
        entries.insert(entries.len() - 1, TarEntry::file(&long, b"x"));
        let file = dir.join("long.tar.zst");
        fs::write(&file, tar_zst(&entries)).unwrap();
        let file = file.display().to_string();
        vec![s.json(&["backup", "import", &file, "-t", "retail"], 4)]
    },
};

/// gh:o/foo installed, its cached archive damaged, then reinstalled offline: it cannot be
/// downloaded again.
const DAMAGED_CACHED_ARCHIVE: Row = Row {
    id: "a_damaged_cached_archive_names_the_blob_and_what_it_hashes_to",
    keys: &[
        (ErrorCode::HashMismatch, "actual"),
        (ErrorCode::HashMismatch, "blob"),
    ],
    run: || {
        let s = Scene::new();
        s.json(&["install", "gh:o/foo", "-t", "retail"], 0);
        let record = common::state::record(&s.home, "retail");
        let sha = record.addons["foo"].blob_sha256.clone().unwrap();
        let blob = s.home.env.cache().join("omaddons/blobs").join(sha);
        let mut bytes = fs::read(&blob).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        fs::write(&blob, &bytes).unwrap();
        let args = [
            "install",
            "gh:o/foo",
            "-t",
            "retail",
            "--reinstall",
            "--offline",
        ];
        vec![s.json(&args, 1)]
    },
};

/// A Tukui download whose `ETag` is not the MD5 of its bytes, before and after the token
/// is resolved again.
const TUKUI_ETAG_DISAGREES: Row = Row {
    id: "a_download_that_disagrees_with_its_etag_names_the_published_hash",
    keys: &[
        (ErrorCode::HashMismatch, "expected"),
        (ErrorCode::HashMismatch, "tried"),
        (ErrorCode::HashMismatch, "tried[].asset"),
        (ErrorCode::HashMismatch, "tried[].code"),
        (ErrorCode::HashMismatch, "tried[].message"),
    ],
    run: || {
        use common::tukui::{elvui, elvui_zip, etag, tukui_addon, tukui_download, tukui_list};
        let home = Home::new(&["wow"]);
        let zip = elvui_zip("16.00");
        let wrong = etag(b"other bytes");
        tukui_list(&home.http, &[elvui()]);
        tukui_addon(&home.http, &elvui(), &["TOKEN-A", "TOKEN-B"]);
        for token in ["TOKEN-A", "TOKEN-B"] {
            tukui_download(&home.http, "elvui", token, &zip, Some(&wrong));
        }
        vec![check(
            home.run(&["install", "tukui:elvui", "-t", "retail"]),
            1,
        )]
    },
};
