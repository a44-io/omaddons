//! Fallback paths and refusals end to end (vision §5.12): the plain-rename fallback when
//! `renameat2` flags fail with EINVAL (`atomic_swap: false`, and crashes in that mode
//! still recover to one side), the byte-copy fallback when reflinks are unsupported
//! (SavedVariables still snapshotted and restored), hostile archives refused with
//! nothing written to the client, and `blizzard_auctionui` on a 3.3.5a client.

mod common;

use std::fs;

use common::addons::toc;
use common::archives::{RawEntry, raw_zip};
use common::engine::{Scene, warning_codes};
use common::home::Home;
use common::trees::{CustomRoot, tree_hash};
use serde_json::Value;

const EINVAL: &str = "fsx.renameat2.einval=error";

#[test]
fn without_renameat2_flags_the_swap_uses_two_renames_and_says_so() {
    let s = Scene::new();
    for (zip, kind) in [("v1", "install"), ("v2", "update")] {
        let r = s.run(&s.install_args("gh:o/foo", zip), EINVAL);
        let envelope = r.expect(0);
        assert_eq!(envelope["data"]["kind"], kind, "{}", r.context());
        assert_eq!(envelope["data"]["atomic_swap"], false, "{}", r.context());
        assert!(
            warning_codes(&envelope)
                .iter()
                .any(|c| c == "atomic_swap_unavailable"),
            "{}",
            r.context()
        );
    }
    let core = fs::read_to_string(s.addons().join("Foo/core.lua")).unwrap();
    assert_eq!(core, "-- v2\n");
    assert!(!s.addons().join("FooOld").exists());
}

#[test]
fn crashes_in_fallback_mode_still_recover_to_one_side() {
    let reference = Scene::new();
    reference.install("gh:o/foo", "v1");
    let old = reference.folders();
    reference.install("gh:o/foo", "v2");
    let new = reference.folders();
    for failpoint in [
        "engine.journal.prepared",
        "engine.swap.before_rename",
        "engine.swap.after_exchange",
        "engine.swapped",
        "engine.record.written",
    ] {
        let s = Scene::new();
        s.install("gh:o/foo", "v1");
        let r = s.run(
            &s.install_args("gh:o/foo", "v2"),
            &format!("{EINVAL},{failpoint}=abort"),
        );
        assert_eq!(r.code, -1, "{failpoint}: {}", r.context());
        let bar = s.run(&s.install_args("gh:o/bar", "bar"), "").expect(0);
        assert!(
            warning_codes(&bar).iter().any(|c| c == "journal_recovered"),
            "{failpoint}: {bar}"
        );
        let now = s.folders();
        assert!(now == old || now == new, "{failpoint}: {now:?}");
        assert!(s.journals().is_empty(), "{failpoint}");
    }
}

#[test]
fn without_reflinks_saved_variables_are_byte_copied_and_undo_restores_them() {
    let s = Scene::new();
    let sv = s
        .home
        .client("_retail_")
        .join("WTF/Account/FAKEACCT/SavedVariables");
    fs::create_dir_all(&sv).unwrap();
    fs::write(sv.join("Foo.lua"), "-- before").unwrap();
    let nolink = "fsx.reflink.unsupported=error";
    let r = s.run(&s.install_args("gh:o/foo", "v1"), nolink);
    r.expect(0);
    fs::write(sv.join("Foo.lua"), "-- after").unwrap();
    let undo = ["undo", "--offline", "--json"].map(str::to_owned);
    let r = s.run(&undo, nolink);
    let envelope = r.expect(0);
    assert_eq!(
        envelope["data"]["saved_variables"]["restored"],
        1,
        "{}",
        r.context()
    );
    assert_eq!(fs::read_to_string(sv.join("Foo.lua")).unwrap(), "-- before");
}

#[test]
fn hostile_archives_are_refused_and_nothing_reaches_the_client() {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    let foo_toc = ("Foo/Foo.toc", "## Interface: 120105\n".as_bytes());
    // Past ARCHIVE_RATIO_FLOOR (32 MiB), so the ratio applies.
    let zeros = vec![0u8; 40 << 20];
    let cases: Vec<(&str, Vec<RawEntry>)> = vec![
        ("traversal", vec![RawEntry::stored("Foo/../../x", b"x")]),
        ("absolute", vec![RawEntry::stored("/etc/x", b"x")]),
        (
            "symlink",
            vec![RawEntry::stored("Foo/link", b"/etc/passwd").mode(0o120_777)],
        ),
        (
            "case collision",
            vec![
                RawEntry::stored("Foo/a.lua", b"1"),
                RawEntry::stored("Foo/A.lua", b"2"),
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
    ];
    let before = tree_hash(&s.home.client("_retail_"));
    for (label, mut entries) in cases {
        entries.insert(0, RawEntry::stored(foo_toc.0, foo_toc.1));
        let path = s.dl.join(format!("{}.zip", label.replace(' ', "-")));
        fs::write(&path, raw_zip(&entries)).unwrap();
        let args = [
            "install",
            "gh:o/foo",
            "--file",
            &path.display().to_string(),
            "-t",
            "retail",
            "--offline",
            "--json",
        ]
        .map(|a| a.to_owned());
        let r = s.run(&args, "");
        let envelope = r.expect(4);
        assert_eq!(
            envelope["error"]["code"],
            "unsafe_archive",
            "{label}: {}",
            r.context()
        );
    }
    let garbage = s.dl.join("garbage.zip");
    fs::write(&garbage, b"this is not a zip archive").unwrap();
    let args = [
        "install",
        "gh:o/foo",
        "--file",
        &garbage.display().to_string(),
        "-t",
        "retail",
        "--offline",
        "--json",
    ]
    .map(|a| a.to_owned());
    let r = s.run(&args, "");
    let envelope = r.envelope();
    assert_eq!(envelope["error"]["code"], "bad_archive", "{}", r.context());
    assert!(matches!(r.code, 1 | 4), "{}", r.context());
    assert_eq!(
        tree_hash(&s.home.client("_retail_")),
        before,
        "the client dir is untouched"
    );
}

#[test]
fn a_lowercase_blizzard_folder_on_a_335_client_is_protected() {
    let home = Home::new(&[]);
    let root = CustomRoot::at(home.env.home().join("Games/wow-335"))
        .version(3, 3, 5, 12_340)
        .blizzard_folders(23)
        .build();
    let added = home.run(&[
        "targets",
        "add",
        &root.display().to_string(),
        "--name",
        "wotlk",
    ]);
    added.expect(0);
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("evil.zip");
    let text = toc("30300", &[]);
    fs::write(
        &zip,
        common::archives::zip_text(&[("blizzard_auctionui/blizzard_auctionui.toc", &text)]),
    )
    .unwrap();
    let addons = root.join("Interface/AddOns");
    let before = tree_hash(&addons);
    let path = zip.display().to_string();
    for force in [false, true] {
        let mut args = vec![
            "install",
            "gh:o/evil",
            "--file",
            &path,
            "-t",
            "wotlk",
            "--offline",
        ];
        if force {
            args.push("--force");
        }
        let r = home.run(&args);
        let envelope: Value = r.expect(4);
        assert_eq!(
            envelope["error"]["code"],
            "protected_folder",
            "{}",
            r.context()
        );
    }
    assert_eq!(tree_hash(&addons), before);
}
