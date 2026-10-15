//! Archive inspection end to end: hostile and oversized zips are refused before
//! extraction, addon roots are found through wrappers, files are measured from their
//! real bytes, nothing is written, and verification applies the target's TOC rules.

mod common;

use std::fs;
use std::io::Cursor;
use std::os::unix::fs::symlink;

use common::addons::toc;
use common::archives::{RawEntry, raw_zip, zip_of, zip_text};
use common::env::TestEnv;
use common::targets::{battlenet, legacy_335};
use common::trees::tree_hash;
use omaddons::engine::inspect::{
    ArchiveLimits, Inspected, compatible, inspect, inspect_dir, inspect_zip, verify,
};
use omaddons::error::{Error, ErrorCode};
use omaddons::model::Target;
use omaddons::rules::RulesTable;
use omaddons::toc::{Verdict, WrongFlavorReason};
use proptest::prelude::*;
use sha2::{Digest, Sha256};
use support::{folders, inspect_bytes, inspect_with, refused, verdicts};

#[cfg(test)]
mod support {
    use super::*;

    pub fn inspect_with(bytes: Vec<u8>, limits: ArchiveLimits) -> Result<Inspected, Error> {
        inspect_zip(Cursor::new(bytes), &limits, &RulesTable::builtin())
    }

    pub fn inspect_bytes(bytes: Vec<u8>) -> Result<Inspected, Error> {
        inspect_with(bytes, ArchiveLimits::default())
    }

    /// The error of a refused archive, logged with the entries when it was accepted.
    pub fn refused(label: &str, result: Result<Inspected, Error>) -> Error {
        assert!(result.is_err(), "{label}: accepted {result:?}");
        result.unwrap_err()
    }

    /// `prefix=name` per folder.
    pub fn folders(inspected: &Inspected) -> Vec<String> {
        inspected
            .folders
            .iter()
            .map(|f| format!("{}={}", f.archive_prefix, f.name.as_str()))
            .collect()
    }

    /// Each folder's verdict on `target`, and whether the archive is compatible.
    pub fn verdicts(inspected: &Inspected, target: &Target) -> (Vec<(String, Verdict)>, bool) {
        let verdicts = verify(inspected, target, &RulesTable::builtin()).unwrap();
        let ok = compatible(&verdicts);
        let list = verdicts
            .into_iter()
            .map(|v| (v.folder.as_str().to_owned(), v.report.verdict))
            .collect();
        (list, ok)
    }
}

const FOO_TOC: (&str, &[u8]) = ("Foo/Foo.toc", b"## Interface: 120105\n");

#[test]
fn hostile_entries_are_refused() {
    // Past ARCHIVE_RATIO_FLOOR (32 MiB), so the ratio applies.
    let zeros = vec![0u8; 40 << 20];
    let cases: Vec<(&str, Vec<RawEntry>)> = vec![
        ("traversal", vec![RawEntry::stored("Foo/../../x", b"x")]),
        (
            "backslash traversal",
            vec![RawEntry::stored("Foo\\..\\..\\x", b"x")],
        ),
        ("absolute", vec![RawEntry::stored("/etc/x", b"x")]),
        ("drive letter", vec![RawEntry::stored("C:/x", b"x")]),
        ("lowercase drive", vec![RawEntry::stored("c:\\x", b"x")]),
        (
            "symlink",
            vec![RawEntry::stored("Foo/link", b"/etc/passwd").mode(0o120_777)],
        ),
        (
            "char device",
            vec![RawEntry::stored("Foo/dev", b"").mode(0o020_644)],
        ),
        (
            "block device",
            vec![RawEntry::stored("Foo/dev", b"").mode(0o060_644)],
        ),
        (
            "fifo",
            vec![RawEntry::stored("Foo/fifo", b"").mode(0o010_644)],
        ),
        (
            "socket",
            vec![RawEntry::stored("Foo/sock", b"").mode(0o140_644)],
        ),
        (
            "nul",
            vec![RawEntry::stored("x", b"").raw_name(b"Foo/a\0.lua")],
        ),
        (
            "control",
            vec![RawEntry::stored("x", b"").raw_name(b"Foo/a\x07.lua")],
        ),
        (
            "case collision",
            vec![
                RawEntry::stored("Foo/a.lua", b"1"),
                RawEntry::stored("Foo/A.lua", b"2"),
            ],
        ),
        (
            "dir case collision",
            vec![
                RawEntry::stored("Foo/Lib/x", b"1"),
                RawEntry::stored("Foo/lib/y", b"2"),
            ],
        ),
        (
            "file/dir collision",
            vec![
                RawEntry::stored("Foo/x", b"1"),
                RawEntry::stored("Foo/X/y", b"2"),
            ],
        ),
        (
            "duplicate",
            vec![
                RawEntry::stored("Foo/a.lua", b"1"),
                RawEntry::stored("Foo/a.lua", b"2"),
            ],
        ),
        (
            "ratio bomb",
            vec![RawEntry::deflated("Foo/zeros.bin", &zeros)],
        ),
        (
            "invalid root name",
            vec![RawEntry::stored(
                ".hidden/.hidden.toc",
                b"## Interface: 120105\n",
            )],
        ),
    ];
    for (label, mut entries) in cases {
        entries.insert(0, RawEntry::stored(FOO_TOC.0, FOO_TOC.1));
        let err = refused(label, inspect_bytes(raw_zip(&entries)));
        assert_eq!(
            (err.code, err.exit().code()),
            (ErrorCode::UnsafeArchive, 4),
            "{label}: {}",
            err.message
        );
    }
}

#[test]
fn lying_headers_stop_at_the_real_size() {
    let zeros = vec![0u8; 10 << 20];
    for (label, entry) in [
        (
            "deflated",
            RawEntry::deflated("Foo/zeros.bin", &zeros).declared_size(1_000),
        ),
        (
            "stored",
            RawEntry::stored("Foo/data.bin", &[7u8; 4096]).declared_size(100),
        ),
    ] {
        let zip = raw_zip(&[RawEntry::stored(FOO_TOC.0, FOO_TOC.1), entry]);
        let err = refused(label, inspect_bytes(zip));
        assert_eq!(
            err.code,
            ErrorCode::UnsafeArchive,
            "{label}: {}",
            err.message
        );
        assert!(err.message.contains("declares"), "{label}: {}", err.message);
    }
}

/// Plater v660 ships a mostly transparent 512x512 TGA (1 MiB of pixels and an 18-byte
/// header) that deflates about 1000:1: a texture, not a bomb.
#[test]
fn a_compressible_texture_is_not_a_bomb() {
    let mut texture = vec![0u8; (1 << 20) + 18];
    texture[2] = 2;
    let zip = raw_zip(&[
        RawEntry::stored(FOO_TOC.0, FOO_TOC.1),
        RawEntry::deflated("Foo/images/transparent.tga", &texture),
    ]);
    let inspected = inspect_bytes(zip);
    assert!(inspected.is_ok(), "{inspected:?}");
}

#[test]
fn size_and_count_bombs() {
    let mut entries = vec![RawEntry::stored(FOO_TOC.0, FOO_TOC.1)];
    entries.extend((0..20_000).map(|i| RawEntry::stored(&format!("Foo/f{i}"), b"")));
    let err = refused("entry count", inspect_bytes(raw_zip(&entries)));
    assert_eq!(
        (err.code, err.exit().code()),
        (ErrorCode::ArchiveTooLarge, 4)
    );
    assert_eq!(
        (
            err.details["measured"].as_u64(),
            err.details["limit"].as_u64()
        ),
        (Some(20_001), Some(20_000))
    );

    let noise: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let limits = ArchiveLimits {
        max_uncompressed: 100_000,
        ..ArchiveLimits::default()
    };
    let zip = zip_of(&[FOO_TOC, ("Foo/big.bin", &noise)]);
    let err = refused("total size", inspect_with(zip, limits));
    assert_eq!(err.code, ErrorCode::ArchiveTooLarge);
    assert_eq!(err.details["config_key"], "limits.archive_max_uncompressed");
    assert!(
        err.fix
            .display()
            .contains("omaddons config set limits.archive_max_uncompressed")
    );

    let small = ArchiveLimits {
        max_entries: 2,
        ..ArchiveLimits::default()
    };
    let zip = zip_text(&[
        ("Foo/Foo.toc", "## Interface: 120105\n"),
        ("Foo/a", ""),
        ("Foo/b", ""),
    ]);
    assert_eq!(
        inspect_with(zip, small).unwrap_err().code,
        ErrorCode::ArchiveTooLarge,
        "config can lower the limits"
    );

    let padding = vec![b' '; 10_000];
    let ok = inspect_bytes(zip_of(&[FOO_TOC, ("Foo/padding.txt", &padding)]));
    assert!(
        ok.is_ok(),
        "a small repetitive file is under the ratio floor: {ok:?}"
    );
}

#[test]
fn malformed_archives() {
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("not a zip", b"hello".to_vec(), "not_a_zip"),
        (
            "no addon root",
            zip_text(&[
                ("README.md", "x"),
                ("Foo.toc", "## Interface: 1\n"),
                ("Foo/x.lua", ""),
            ]),
            "no_addon_root",
        ),
        ("empty", zip_of(&[]), "no_addon_root"),
        (
            "two roots, one name",
            zip_text(&[
                ("Foo-main/Foo.toc", "## Interface: 1\n"),
                ("Foo/Foo.toc", "## Interface: 1\n"),
            ]),
            "duplicate_folder",
        ),
        (
            "unsupported compression",
            raw_zip(&[RawEntry {
                method: 12,
                ..RawEntry::stored("Foo/Foo.toc", b"x")
            }]),
            "unsupported_compression",
        ),
        (
            "corrupt payload",
            raw_zip(&[RawEntry {
                crc: 1,
                ..RawEntry::stored("Foo/Foo.toc", b"## Interface: 1\n")
            }]),
            "corrupt",
        ),
    ];
    for (label, bytes, reason) in cases {
        let err = refused(label, inspect_bytes(bytes));
        assert_eq!(
            (err.code, err.exit().code(), err.retryable),
            (ErrorCode::BadArchive, 1, false),
            "{label}: {}",
            err.message
        );
        assert_eq!(err.details["reason"], reason, "{label}");
    }
}

#[test]
fn layouts() {
    let toc = "## Interface: 120105\n";
    /// (label, entries, `prefix=name` per folder)
    type Layout<'a> = (&'a str, Vec<(&'a str, &'a str)>, Vec<&'a str>);
    let cases: Vec<Layout<'_>> = vec![
        (
            "branch suffix",
            vec![("Foo-main/Foo.toc", toc), ("Foo-main/x.lua", "")],
            vec!["Foo-main=Foo"],
        ),
        (
            "wrapper",
            vec![("Foo-1.2.3/Foo/Foo.toc", toc), ("Foo-1.2.3/README.md", "")],
            vec!["Foo-1.2.3/Foo=Foo"],
        ),
        (
            "multi-addon",
            vec![
                ("Repo-main/A/A.toc", toc),
                ("Repo-main/B/B_Mainline.toc", toc),
                ("Repo-main/LICENSE", ""),
            ],
            vec!["Repo-main/A=A", "Repo-main/B=B"],
        ),
        (
            "loose files",
            vec![("README.md", ""), ("LICENSE", ""), ("Foo/Foo.toc", toc)],
            vec!["Foo=Foo"],
        ),
        (
            "release zip",
            vec![
                ("WeakAuras/WeakAuras.toc", toc),
                ("WeakAurasOptions/WeakAurasOptions.toc", toc),
            ],
            vec!["WeakAuras=WeakAuras", "WeakAurasOptions=WeakAurasOptions"],
        ),
    ];
    for (label, entries, want) in cases {
        let inspected = inspect_bytes(zip_text(&entries)).unwrap();
        assert_eq!(folders(&inspected), want, "{label}");
    }

    let nested = inspect_bytes(zip_text(&[
        ("Foo/Foo.toc", toc),
        ("Foo/Libs/LibStub/LibStub.toc", "## Interface: 11200\n"),
        ("Foo/Libs/LibStub/LibStub.lua", "--"),
    ]))
    .unwrap();
    assert_eq!(
        folders(&nested),
        ["Foo=Foo"],
        "a library's TOC is not a root"
    );
    let mut files: Vec<&str> = nested.folders[0]
        .files
        .iter()
        .map(|f| f.rel_path.as_str())
        .collect();
    files.sort_unstable();
    assert_eq!(
        files,
        [
            "Foo.toc",
            "Libs/LibStub/LibStub.lua",
            "Libs/LibStub/LibStub.toc"
        ]
    );
}

#[test]
fn sizes_and_hashes_are_the_extracted_bytes() {
    let contents: [(&str, &[u8]); 3] = [
        ("Foo-main/Foo.toc", b"## Interface: 120105\n"),
        ("Foo-main/core.lua", b"local x = 1\n"),
        ("Foo-main/media/blank.tga", &[0u8; 5000]),
    ];
    let inspected = inspect_bytes(zip_of(&contents)).unwrap();
    let folder = &inspected.folders[0];
    for (name, bytes) in contents {
        let rel = name.strip_prefix("Foo-main/").unwrap();
        let file = folder.files.iter().find(|f| f.rel_path == rel);
        assert!(file.is_some(), "{rel} in {folder:?}");
        let file = file.unwrap();
        let sha: String = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            (file.size, file.sha256.as_str()),
            (bytes.len() as u64, sha.as_str()),
            "{rel}"
        );
    }
    let json = serde_json::to_value(&inspected).unwrap();
    assert_eq!(json["folders"][0]["name"], "Foo");
    assert!(
        json["folders"][0]["files"][0].get("source").is_none(),
        "sources stay internal"
    );
}

#[test]
fn inspecting_writes_nothing() {
    let env = TestEnv::new();
    let blob = env.root().join("blob.zip");
    let bytes = zip_text(&[("Foo/Foo.toc", "## Interface: 120105\n"), ("Foo/x.lua", "")]);
    fs::write(&blob, &bytes).unwrap();
    let before = tree_hash(env.root());
    let inspected = inspect(&blob, &ArchiveLimits::default(), &RulesTable::builtin()).unwrap();
    assert_eq!(tree_hash(env.root()), before);
    let sha: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(inspected.blob_sha256.as_deref(), Some(sha.as_str()));
}

#[test]
fn directories() {
    let env = TestEnv::new();
    let addon = env.root().join("MyAddon");
    fs::create_dir_all(addon.join(".git/objects")).unwrap();
    fs::create_dir_all(addon.join("Libs")).unwrap();
    fs::write(addon.join("MyAddon.toc"), "## Interface: 120105\n").unwrap();
    fs::write(addon.join("Libs/lib.lua"), "--").unwrap();
    fs::write(addon.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let before = tree_hash(env.root());
    let inspected = inspect_dir(&addon, &ArchiveLimits::default(), &RulesTable::builtin()).unwrap();
    assert_eq!(tree_hash(env.root()), before);
    assert_eq!(
        folders(&inspected),
        ["MyAddon=MyAddon"],
        "the directory is the addon"
    );
    let mut files: Vec<&str> = inspected.folders[0]
        .files
        .iter()
        .map(|f| f.rel_path.as_str())
        .collect();
    files.sort_unstable();
    assert_eq!(files, ["Libs/lib.lua", "MyAddon.toc"], ".git is skipped");
    assert_eq!(inspected.blob_sha256, None);

    let repo = env.root().join("Repo-main");
    for name in ["A", "B"] {
        fs::create_dir_all(repo.join(name)).unwrap();
        fs::write(
            repo.join(format!("{name}/{name}.toc")),
            "## Interface: 120105\n",
        )
        .unwrap();
    }
    let inspected = inspect_dir(&repo, &ArchiveLimits::default(), &RulesTable::builtin()).unwrap();
    assert_eq!(folders(&inspected), ["Repo-main/A=A", "Repo-main/B=B"]);

    symlink("/etc/passwd", repo.join("A/link")).unwrap();
    let err = inspect_dir(&repo, &ArchiveLimits::default(), &RulesTable::builtin()).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnsafeArchive, "{}", err.message);
    fs::remove_file(repo.join("A/link")).unwrap();

    fs::write(repo.join("A/x.lua"), "1").unwrap();
    fs::write(repo.join("A/X.lua"), "2").unwrap();
    let err = inspect_dir(&repo, &ArchiveLimits::default(), &RulesTable::builtin()).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnsafeArchive, "{}", err.message);
}

#[test]
fn verify_applies_the_targets_load_rules() {
    let env = TestEnv::new();
    let retail = battlenet(&env, "wow");
    let era = battlenet(&env, "wow_classic_era");
    let mists = battlenet(&env, "wow_classic");
    let forever = battlenet(&env, "wowf");
    let wotlk = legacy_335(&env);

    // WeakAuras-style: classic TOCs only, no Mainline.
    let classic_only = inspect_bytes(zip_text(&[
        ("WeakAuras/WeakAuras_Classic.toc", &toc("11509", &[])),
        ("WeakAuras/WeakAuras_Vanilla.toc", &toc("11509", &[])),
        ("WeakAuras/WeakAuras_Mists.toc", &toc("50504", &[])),
        ("WeakAuras/init.lua", ""),
    ]))
    .unwrap();
    let (list, ok) = verdicts(&classic_only, &retail);
    assert!(!ok, "{list:?}");
    assert_eq!(
        list[0].1,
        Verdict::WrongFlavor {
            reason: WrongFlavorReason::NoLoadableToc
        }
    );
    for target in [&era, &mists] {
        let (list, ok) = verdicts(&classic_only, target);
        assert!(
            ok && matches!(list[0].1, Verdict::Compatible { out_of_date: false }),
            "{}: {list:?}",
            target.id
        );
    }

    // Forever loads _Mainline TOCs, but not retail's interfaces.
    let mainline =
        inspect_bytes(zip_text(&[("Foo/Foo_Mainline.toc", &toc("120105", &[]))])).unwrap();
    let (list, ok) = verdicts(&mainline, &forever);
    assert!(
        !ok && matches!(
            list[0].1,
            Verdict::WrongFlavor {
                reason: WrongFlavorReason::InterfaceNotInFamily { .. }
            }
        ),
        "{list:?}"
    );
    assert!(verdicts(&mainline, &retail).1);

    // 3.3.5a reads the plain TOC.
    let wrath = inspect_bytes(zip_text(&[("Foo/Foo.toc", &toc("30300", &[]))])).unwrap();
    assert!(verdicts(&wrath, &wotlk).1);
    assert!(!verdicts(&wrath, &retail).1);

    // Out of date still installs; one wrong-flavor folder sinks the archive.
    let old = inspect_bytes(zip_text(&[("Foo/Foo.toc", &toc("120000", &[]))])).unwrap();
    let (list, ok) = verdicts(&old, &retail);
    assert!(
        ok && list[0].1 == Verdict::Compatible { out_of_date: true },
        "{list:?}"
    );
    let mixed = inspect_bytes(zip_text(&[
        ("A/A.toc", &toc("120105", &[])),
        ("B/B.toc", &toc("11509", &[])),
    ]))
    .unwrap();
    assert!(!verdicts(&mixed, &retail).1);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Random entry names never panic, and every accepted file stays inside its folder.
    #[test]
    fn random_archives(names in proptest::collection::vec("[a-zA-Z0-9._/\\\\: -]{1,20}(\\.toc)?", 1..12)) {
        let entries: Vec<RawEntry> = names.iter().map(|n| RawEntry::stored(n, b"## Interface: 120105\n")).collect();
        if let Ok(inspected) = inspect_bytes(raw_zip(&entries)) {
            for folder in &inspected.folders {
                for file in &folder.files {
                    prop_assert!(!file.rel_path.starts_with('/'), "{file:?}");
                    prop_assert!(!file.rel_path.split('/').any(|p| p == ".." || p.is_empty()), "{file:?}");
                }
            }
        }
    }
}
