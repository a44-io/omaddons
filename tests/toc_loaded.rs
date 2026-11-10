//! `DiskFolder` over real files: symlinks are never followed, odd entries are skipped,
//! and a huge single-line TOC is read with bounded memory.

use crate::common;

use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;

use common::env::TestEnv;
use omaddons::limits::TOC_LINE_MAX;
use omaddons::rules::{ClientVersion, InstallKind, RulesTable};
use omaddons::toc::{DiskFolder, FolderView, Verdict, loaded_toc, verdict};

fn retail() -> (omaddons::rules::ClientRules, RulesTable) {
    let table = RulesTable::builtin();
    let v = ClientVersion::new(12, 1, 5);
    (table.rules_for(&v, InstallKind::Battlenet).clone(), table)
}

#[test]
fn symlinked_tocs_are_ignored_and_refused() {
    let env = TestEnv::new();
    let outside = env.root().join("outside.toc");
    fs::write(&outside, "## Interface: 120105\n").unwrap();
    let foo = env.root().join("AddOns/Foo");
    fs::create_dir_all(&foo).unwrap();
    symlink(&outside, foo.join("Foo.toc")).unwrap();

    let view = DiskFolder::new(&foo).unwrap();
    assert!(
        view.root_files().unwrap().is_empty(),
        "symlinks are not root files"
    );
    assert!(view.toc("Foo.toc").is_err(), "and are never read");
    let (rules, table) = retail();
    assert_eq!(
        verdict(&view, &rules, rules.family, 120_105, &table)
            .unwrap()
            .verdict,
        Verdict::NoToc
    );

    fs::remove_file(foo.join("Foo.toc")).unwrap();
    fs::write(foo.join("Foo.toc"), "## Interface: 120105\n").unwrap();
    assert_eq!(
        verdict(&view, &rules, rules.family, 120_105, &table)
            .unwrap()
            .verdict,
        Verdict::Compatible { out_of_date: false }
    );
    assert!(view.toc("../outside.toc").is_err(), "no paths");
}

#[test]
fn fifos_and_directories_are_not_tocs() {
    let env = TestEnv::new();
    let foo = env.root().join("Foo");
    fs::create_dir_all(foo.join("Foo_Mainline.toc")).unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(foo.join("Foo.toc"))
        .status()
        .is_ok_and(|s| s.success());
    let view = DiskFolder::new(&foo).unwrap();
    assert!(view.root_files().unwrap().is_empty());
    if made {
        assert!(
            view.toc("Foo.toc").is_err(),
            "a FIFO is refused without blocking"
        );
    }
    assert!(loaded_toc(&view, &retail().0).unwrap().is_none());
}

#[test]
fn case_variants_on_disk() {
    let env = TestEnv::new();
    let foo = env.root().join("Foo");
    fs::create_dir_all(&foo).unwrap();
    fs::write(foo.join("foo_standard.TOC"), "## Interface: 120105\n").unwrap();
    fs::write(foo.join("Foo_Standard.toc"), "## Interface: 120105\n").unwrap();
    let loaded = loaded_toc(&DiskFolder::new(&foo).unwrap(), &retail().0)
        .unwrap()
        .unwrap();
    assert_eq!(
        (loaded.file.as_str(), loaded.case_ambiguous),
        ("Foo_Standard.toc", true)
    );
}

#[test]
fn a_huge_single_line_toc_is_read_bounded() {
    let env = TestEnv::new();
    let foo = env.root().join("Foo");
    fs::create_dir_all(&foo).unwrap();
    let mut file = fs::File::create(foo.join("Foo.toc")).unwrap();
    file.write_all(b"## Interface: 120105\n## Notes: ").unwrap();
    let chunk = vec![b'n'; 1 << 20];
    for _ in 0..50 {
        file.write_all(&chunk).unwrap();
    }
    file.write_all(b"\n## Version: 2\n").unwrap();
    drop(file);

    let view = DiskFolder::new(&foo).unwrap();
    let toc = view.toc("Foo.toc").unwrap();
    let kept: usize = toc.lines.iter().map(|d| d.raw_value.len()).sum();
    assert!(kept < 3 * (TOC_LINE_MAX + 2), "{kept} bytes kept");
    let (rules, table) = retail();
    let report = verdict(&view, &rules, rules.family, 120_105, &table).unwrap();
    assert_eq!(report.verdict, Verdict::Compatible { out_of_date: false });
    let loaded = loaded_toc(&view, &rules).unwrap().unwrap();
    assert_eq!(loaded.toc.version.as_deref(), Some("2"));
    assert_eq!(
        loaded.toc.notes.map(|n| n.len()),
        Some(TOC_LINE_MAX - "## Notes: ".len())
    );
}

#[test]
fn folder_names_must_be_valid() {
    assert!(DiskFolder::new("/").is_err());
    assert!(DiskFolder::new("/tmp/.hidden").is_err());
}
