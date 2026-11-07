//! e2e suite: declared error details agree with hostile archives and ambiguous or failed requests.
#![cfg(test)]

use crate::common;

use std::fs;
use std::path::PathBuf;

use common::archives::{RawEntry, raw_zip};
use common::home::Home;
use common::tlog::{Ran, TestLog, tlog};
use omaddons::error::ErrorCode;
use serde_json::Value;

const FOO_TOC: (&str, &[u8]) = ("Foo/Foo.toc", b"## Interface: 120105\n");

fn archive(home: &Home, name: &str, entries: &[RawEntry]) -> PathBuf {
    let dir = home.env.root().join("dl");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.zip"));
    fs::write(&path, raw_zip(entries)).unwrap();
    path
}

fn install(log: &TestLog, home: &Home, request: &str, folder: &str) {
    log.step(&format!("setup: install {request} owning {folder}"));
    let path = archive(
        home,
        folder,
        &[RawEntry::stored(
            &format!("{folder}/{folder}.toc"),
            FOO_TOC.1,
        )],
    );
    log.run(
        home,
        &[
            "install",
            request,
            "--file",
            path.to_str().unwrap(),
            "-t",
            "retail",
            "--offline",
            "--json",
            "--progress",
            "json",
        ],
    )
    .ok();
}

fn checked_details<'a>(log: &TestLog, ran: &'a Ran) -> &'a Value {
    let envelope = ran.envelope();
    let details = &envelope["error"]["details"];
    log.json("details", details);
    let code = ErrorCode::ALL
        .iter()
        .find(|code| Some(code.wire()) == ran.error_code())
        .unwrap();
    let mismatch = code
        .check_details(details)
        .err()
        .map(|error| error.to_string());
    if let Some(error) = &mismatch {
        log.note(format!("check_details: {error}"));
    }
    log.assert_eq("details match the declared specification", &mismatch, &None);
    details
}

fn keys(log: &TestLog, value: &Value, expected: &[&str]) {
    let actual: Vec<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    log.assert_eq("object keys", &actual, &expected);
}

fn no_network(log: &TestLog, home: &Home) {
    let net = fs::read(home.state().join("net.log")).unwrap_or_default();
    log.assert_eq("offline runs wrote no network log", &net, &Vec::<u8>::new());
}

#[test]
fn install_refusals_of_hostile_archives_name_their_rule() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    install(&log, &home, "gh:o/foo", "Foo");
    log.tree("retail AddOns", &home.addons("_retail_"));
    let zeros = vec![0u8; 40 << 20];
    let cases = [
        (
            "traversal",
            "parent_dir",
            "climbs out of its folder",
            true,
            vec![RawEntry::stored("Foo/../../x", b"x")],
        ),
        (
            "absolute",
            "absolute_path",
            "is an absolute path",
            true,
            vec![RawEntry::stored("/etc/x", b"x")],
        ),
        (
            "drive",
            "drive_letter",
            "starts with a drive letter",
            true,
            vec![RawEntry::stored("C:/x", b"x")],
        ),
        (
            "symlink",
            "symlink",
            "is a symlink",
            true,
            vec![RawEntry::stored("Foo/link", b"/etc/passwd").mode(0o120_777)],
        ),
        (
            "device",
            "special_file",
            "is a device, FIFO or socket",
            true,
            vec![RawEntry::stored("Foo/dev", b"").mode(0o020_644)],
        ),
        (
            "fifo",
            "special_file",
            "is a device, FIFO or socket",
            true,
            vec![RawEntry::stored("Foo/fifo", b"").mode(0o010_644)],
        ),
        (
            "control",
            "control_character",
            "contains a control character",
            true,
            vec![RawEntry::stored("x", b"").raw_name(b"Foo/a\x07.lua")],
        ),
        (
            "case",
            "case_collision",
            "once case is ignored",
            true,
            vec![
                RawEntry::stored("Foo/a.lua", b"1"),
                RawEntry::stored("Foo/A.lua", b"2"),
            ],
        ),
        (
            "file-dir",
            "file_and_directory",
            "both a file and a directory",
            true,
            vec![
                RawEntry::stored("Foo/x", b"1"),
                RawEntry::stored("Foo/X/y", b"2"),
            ],
        ),
        (
            "duplicate",
            "duplicate_entry",
            "distinct names",
            false,
            vec![
                RawEntry::stored("Foo/a.lua", b"1"),
                RawEntry::stored("Foo/a.lua", b"2"),
            ],
        ),
        (
            "ratio",
            "ratio",
            "compression ratio",
            true,
            vec![RawEntry::deflated("Foo/zeros.bin", &zeros)],
        ),
        (
            "root",
            "invalid_folder_name",
            "cannot be an addon folder",
            true,
            vec![RawEntry::stored(".hidden/.hidden.toc", FOO_TOC.1)],
        ),
        (
            "lying-header",
            "size_overrun",
            "header declares",
            true,
            vec![RawEntry::stored("Foo/data.bin", &[7; 4096]).declared_size(100)],
        ),
    ];
    for (label, reason, message, entry, mut entries) in cases {
        let case = log.case(label);
        case.step("install the hostile archive");
        entries.insert(0, RawEntry::stored(FOO_TOC.0, FOO_TOC.1));
        let zip = archive(&home, label, &entries);
        let ran = case.run(
            &home,
            &[
                "install",
                "gh:o/foo",
                "--file",
                zip.to_str().unwrap(),
                "-t",
                "retail",
                "--offline",
                "--json",
                "--progress",
                "json",
            ],
        );
        ran.fails(4, "unsafe_archive");
        let details = checked_details(&case, &ran);
        case.assert_eq("hostile reason", &details["reason"], &Value::from(reason));
        if entry {
            case.assert_eq(
                "entry-based refusal names its entry",
                &details["entry"].is_string(),
                &true,
            );
        }
        case.assert_eq(
            "message retains its existing prose",
            &ran.envelope()["error"]["message"]
                .as_str()
                .unwrap()
                .contains(message),
            &true,
        );
        log.assert_unchanged("retail AddOns", &home.addons("_retail_"));
    }
    no_network(&log, &home);
}

#[test]
fn every_item_failing_lists_items_in_a_declared_shape() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let case = log.case("install item failures");
    case.step("two uncached upstream refs both fail offline");
    let ran = case.run(
        &home,
        &[
            "install",
            "gh:o/a",
            "gh:o/b",
            "-t",
            "retail",
            "--offline",
            "--json",
            "--progress",
            "json",
        ],
    );
    ran.fails(1, "offline");
    let details = checked_details(&case, &ran);
    let items = details["items"].as_array().unwrap();
    case.assert_eq("both requests failed", &items.len(), &2);
    for item in items {
        let item_log = case.case(item["request"].as_str().unwrap());
        item_log.json("failed item", item);
        keys(&item_log, item, &["code", "message", "request"]);
    }
    let case = log.case("target item failures");
    case.step("two unknown managed-addon names both fail update");
    let ran = case.run(
        &home,
        &[
            "update",
            "Nope1",
            "Nope2",
            "-t",
            "retail",
            "--offline",
            "--json",
            "--progress",
            "json",
        ],
    );
    ran.fails(3, "not_managed");
    let details = checked_details(&case, &ran);
    let items = details["items"].as_array().unwrap();
    case.assert_eq("both requests failed", &items.len(), &2);
    for item in items {
        let item_log = case.case(item["request"].as_str().unwrap());
        item_log.json("failed item", item);
        keys(&item_log, item, &["error", "request", "target"]);
        for key in ["code", "message", "fix", "fix_argv"] {
            item_log.assert_eq(
                &format!("item error carries {key}"),
                &item["error"].get(key).is_some(),
                &true,
            );
        }
    }
    no_network(&log, &home);
}

fn catalog(home: &Home, text: &str) {
    let dir = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("test.toml"), text).unwrap();
}

#[test]
fn ref_ambiguous_candidates_come_in_their_two_declared_shapes() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let case = log.case("object candidates");
    install(&case, &home, "gh:a/foo", "Foo");
    catalog(
        &home,
        "schema_version = \"1.0\"\n[[addon]]\nname = \"foo\"\nref = \"gh:b/foo\"\nfamilies = [\"mainline\"]\n",
    );
    case.run(
        &home,
        &[
            "list",
            "-t",
            "retail",
            "--offline",
            "--json",
            "--progress",
            "json",
        ],
    )
    .ok();
    case.tree("retail AddOns", &home.addons("_retail_"));
    case.step("installed and catalogue refs disagree for foo");
    let ran = case.run(
        &home,
        &[
            "install",
            "foo",
            "-t",
            "retail",
            "--offline",
            "--json",
            "--progress",
            "json",
        ],
    );
    ran.fails(3, "ref_ambiguous");
    let details = checked_details(&case, &ran);
    let candidates = details["candidates"].as_array().unwrap();
    case.assert_eq("both references are candidates", &candidates.len(), &2);
    for candidate in candidates {
        keys(&case, candidate, &["from", "ref"]);
    }
    case.assert_unchanged("retail AddOns", &home.addons("_retail_"));
    no_network(&case, &home);

    let home = Home::new(&["wow"]);
    let case = log.case("string candidates");
    catalog(
        &home,
        "schema_version = \"1.0\"\n[[addon]]\nname = \"Foo\"\nref = \"gh:a/alpha\"\nfamilies = [\"mainline\"]\n[[addon]]\nname = \"Bar\"\nref = \"gh:b/foo\"\nfamilies = [\"mainline\"]\n",
    );
    install(&case, &home, "Foo", "Alpha");
    install(&case, &home, "Bar", "Foo");
    case.run(
        &home,
        &[
            "list",
            "-t",
            "retail",
            "--offline",
            "--json",
            "--progress",
            "json",
        ],
    )
    .ok();
    case.tree("retail AddOns", &home.addons("_retail_"));
    case.step("one record name and another record's folder both match Foo");
    let ran = case.run(
        &home,
        &[
            "remove",
            "Foo",
            "-t",
            "retail",
            "--offline",
            "--json",
            "--progress",
            "json",
        ],
    );
    ran.fails(3, "ref_ambiguous");
    let details = checked_details(&case, &ran);
    let candidates = details["candidates"].as_array().unwrap();
    case.assert_eq("both addon names are candidates", &candidates.len(), &2);
    case.assert_eq(
        "candidates are addon-name strings",
        &candidates.iter().all(Value::is_string),
        &true,
    );
    case.assert_unchanged("retail AddOns", &home.addons("_retail_"));
    no_network(&case, &home);
}
