//! e2e suite: the blob cache: concurrent stores, the orphan sweep, and damaged archives never staged
//!
//! The blob cache across threads and transactions: concurrent stores of the same
//! content, and the orphan sweep of dead transactions' temp dirs. And F6 part 4
//! (omad-m4-agent-surface-doctor-c6cm.6.6.4), through the binary: a cached archive whose
//! bytes no longer hash to its name is never staged. Online it is downloaded again into
//! the same place (the cache heals, nothing is deleted); offline, or when the source no
//! longer serves it, the command fails `hash_mismatch` naming it and changes nothing.
#![cfg(test)]

use crate::common;

use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use common::archives::zip_text;
use common::doctor::plant;
use common::env::TestEnv;
use common::home::Home;
use common::http::paths;
use common::netlog::net_log;
use common::tlog::{Ran, TestLog, tlog};
use common::trees::tree_hash;
use omaddons::engine::blobs::{BlobCache, Storage};
use omaddons::fsx::{InflightLock, ScopeTable};
use omaddons::model::TxId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[test]
fn concurrent_stores_of_the_same_content_end_as_one_blob() {
    let env = TestEnv::new();
    let paths = paths(&env);
    let scopes = ScopeTable::new(&paths, &[], None).unwrap();
    let cache = BlobCache::new(&paths);
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let refs: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                s.spawn(|| {
                    cache
                        .store_stream(
                            &scopes,
                            &TxId::new(),
                            Cursor::new(bytes.clone()),
                            None,
                            &Storage::Cache,
                        )
                        .unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(refs.windows(2).all(|w| w[0] == w[1]), "{refs:?}");
    let blobs: Vec<String> = fs::read_dir(paths.blobs_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.len() == 64)
        .collect();
    assert_eq!(blobs, [refs[0].sha256.clone()]);
    let temp_files = fs::read_dir(paths.cache_dir().join("tmp"))
        .unwrap()
        .flat_map(|tx| fs::read_dir(tx.unwrap().path()).unwrap())
        .count();
    assert_eq!(temp_files, 0, "no temp file survives a store");
}

#[test]
fn the_orphan_sweep_spares_live_transactions() {
    let env = TestEnv::new();
    let paths = paths(&env);
    let scopes = ScopeTable::new(&paths, &[], None).unwrap();
    let (dead, live) = (TxId::new(), TxId::new());
    for tx in [dead, live] {
        let dir = paths.cache_tmp(&tx);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("blob-0"), "partial").unwrap();
    }
    let _held = InflightLock::create(&paths, &live).unwrap();
    let report = BlobCache::new(&paths)
        .evict(&scopes, &|| Ok(BTreeSet::new()), u64::MAX)
        .unwrap();
    assert_eq!(report.orphans_removed, 1);
    assert!(
        !paths.cache_tmp(&dead).exists(),
        "the dead transaction's temp dir is gone"
    );
    assert!(
        paths.cache_tmp(&live).join("blob-0").is_file(),
        "a live transaction keeps its download"
    );
    assert!(report.evicted.is_empty() && !report.over_cap);
}

// ---------------------------------------------------------------------------------------
// Damaged archives never staged (F6 part 4).

const DEEP: [&str; 3] = ["omaddons", "doctor", "--include-deep"];

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn text_of(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// `omaddons <args> --json`, its output private.
fn run(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv: Vec<&str> = args.to_vec();
    argv.push("--json");
    let ran = log.run(home, &argv);
    ran.assert_private();
    ran
}

fn assets(home: &Home) -> usize {
    net_log(home).count_purpose("asset")
}

/// A home of `products` with `gh:o/foo` v1 installed on retail: the home, the blob's name
/// and path, and the Foo folder's tree hash as installed.
fn installed(log: &TestLog, products: &[&str]) -> (Home, String, PathBuf, String) {
    let home = Home::new(products);
    log.env(&home);
    log.step("home");
    plant::install(&home, "foo", "retail");
    log.json("planted", &plant::take_planted());
    let name = blob_of(&home, "retail");
    let path = plant::blob_path(&home, &name);
    let size = fs::metadata(&path).unwrap().len();
    log.json("blob", &json!({"sha": name, "size": size}));
    let installed = tree_hash(&home.addons("_retail_").join("Foo"));
    (home, name, path, installed)
}

/// The blob the record of foo on `target` names.
fn blob_of(home: &Home, target: &str) -> String {
    common::state::record(home, target).addons["foo"]
        .blob_sha256
        .clone()
        .unwrap()
}

/// Flips the middle byte of the blob at `path` by a plain write (same inode and size).
fn flip(log: &TestLog, path: &PathBuf) -> Vec<u8> {
    let mut bytes = fs::read(path).unwrap();
    let offset = bytes.len() / 2;
    let old = bytes[offset];
    bytes[offset] ^= 0xff;
    fs::write(path, &bytes).unwrap();
    log.note(format!(
        "damage: byte {offset} {old:#04x} → {:#04x}",
        bytes[offset]
    ));
    bytes
}

/// What a "changes nothing" claim compares: each directory's tree, each file's bytes.
struct Snapshot {
    dirs: Vec<(String, PathBuf)>,
    files: Vec<(String, PathBuf, Option<Vec<u8>>)>,
}

/// `<cache>/blobs`, the client dirs, and each top-level entry of `<state>`.
fn snapshot(log: &TestLog, home: &Home, clients: &[&str]) -> Snapshot {
    let mut paths = vec![("blobs".to_owned(), home.env.cache().join("omaddons/blobs"))];
    for client in clients {
        paths.push(((*client).to_owned(), home.client(client)));
    }
    for entry in fs::read_dir(home.state()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        paths.push((format!("state/{name}"), path));
    }
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for (label, path) in paths {
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            log.tree(&label, &path);
            dirs.push((label, path));
        } else {
            let bytes = fs::read(&path).ok();
            files.push((label, path, bytes));
        }
    }
    Snapshot { dirs, files }
}

fn unchanged(log: &TestLog, snapshot: &Snapshot) {
    for (label, dir) in &snapshot.dirs {
        log.assert_unchanged(label, dir);
    }
    for (label, path, bytes) in &snapshot.files {
        log.assert_eq(&format!("{label} unchanged"), &fs::read(path).ok(), bytes);
    }
}

/// The error of a run that must fail `hash_mismatch` naming the blob `name`.
fn damage_error(log: &TestLog, ran: &Ran, name: &str) -> Value {
    ran.fails(1, "hash_mismatch");
    let error = ran.envelope()["error"].clone();
    log.json("error", &error);
    log.assert_eq("details.blob", &error["details"]["blob"], &json!(name));
    error
}

#[test]
fn an_online_reinstall_downloads_a_damaged_blob_again() {
    let log = tlog!();
    for case in ["same_size_flip", "truncated", "another_archive"] {
        let log = log.case(case);
        let (home, name, path, fresh) = installed(&log, &["wow"]);
        let before = common::state::record(&home, "retail").addons["foo"].clone();
        let asked = assets(&home);
        log.step("damage");
        let bytes = fs::read(&path).unwrap();
        match case {
            "same_size_flip" => {
                flip(&log, &path);
            }
            "truncated" => {
                fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
                log.note(format!(
                    "damage: cut to {} of {} bytes",
                    bytes.len() / 2,
                    bytes.len()
                ));
            }
            _ => {
                let toc = "## Interface: 120105, 50504, 11509\n## Version: v1\n";
                let evil = zip_text(&[("Foo/Foo.toc", toc), ("Foo/Foo.lua", "evil\n")]);
                fs::write(&path, &evil).unwrap();
                log.note(format!(
                    "damage: another valid archive, sha256 {}",
                    sha(&evil)
                ));
            }
        }
        log.step("run");
        let ran = run(
            &log,
            &home,
            &[
                "install",
                "gh:o/foo",
                "-t",
                "retail",
                "--reinstall",
                "-v",
                "--progress",
                "json",
            ],
        );
        ran.ok();
        log.step("check");
        log.assert_eq(
            "updated",
            &ran.envelope()["data"]["items"][0]["status"],
            &json!("updated"),
        );
        log.json(
            "asset requests",
            &json!({"before": asked, "after": assets(&home)}),
        );
        log.assert_eq("downloaded once more", &assets(&home), &(asked + 1));
        log.note(format!("reading {}", path.display()));
        log.assert_eq(
            "healed: it hashes to its name",
            &sha(&fs::read(&path).unwrap()),
            &name,
        );
        log.assert_eq(
            "the release's content, never the damaged file's",
            &tree_hash(&home.addons("_retail_").join("Foo")),
            &fresh,
        );
        let after = common::state::record(&home, "retail").addons["foo"].clone();
        log.assert_eq(
            "release_id unchanged",
            &after.release_id,
            &before.release_id,
        );
        log.assert_eq(
            "blob_sha256 unchanged",
            &after.blob_sha256,
            &before.blob_sha256,
        );
        assert!(
            ran.stderr()
                .contains("did not match its name; downloaded it again"),
            "{case}: the -v note: {}",
            ran.stderr()
        );
    }
}

#[test]
fn offline_a_damaged_blob_is_hash_mismatch_and_changes_nothing() {
    let log = tlog!();
    let (home, name, path, _) = installed(&log, &["wow"]);
    log.step("damage");
    let bytes = flip(&log, &path);
    let trees = snapshot(&log, &home, &["_retail_"]);
    log.step("run");
    let ran = run(
        &log,
        &home,
        &[
            "install",
            "gh:o/foo",
            "-t",
            "retail",
            "--reinstall",
            "--offline",
        ],
    );
    let error = damage_error(&log, &ran, &name);
    log.step("check");
    log.assert_eq("retryable", &error["retryable"], &json!(true));
    // The damage's own keys, and `items`, which every install's command-level error
    // carries (the failed item, here this same error).
    let details = &error["details"];
    log.assert_eq("details.actual", &details["actual"], &json!(sha(&bytes)));
    let keys: Vec<&str> = details
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    log.assert_eq("details' keys", &keys, &vec!["actual", "blob", "items"]);
    log.assert_eq(
        "its item",
        &details["items"][0]["error"]["code"],
        &json!("hash_mismatch"),
    );
    log.assert_eq("its fix", &error["fix_argv"], &json!(DEEP));
    unchanged(&log, &trees);
    log.step("the fix");
    let deep = run(&log, &home, &DEEP[1..]);
    deep.exit(1);
    let found: Vec<&Value> = deep.envelope()["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["id"] == "fm-caches-blob-hash-mismatch")
        .collect();
    assert_eq!(found.len(), 1, "the fix argv reports it: {found:?}");
    log.assert_eq(
        "naming the blob",
        &found[0]["evidence"]["expected"],
        &json!(format!("sha256:{name}")),
    );
    log.step("online, then offline");
    run(
        &log,
        &home,
        &["install", "gh:o/foo", "-t", "retail", "--reinstall"],
    )
    .ok();
    let lines = net_log(&home).len();
    run(
        &log,
        &home,
        &[
            "install",
            "gh:o/foo",
            "-t",
            "retail",
            "--reinstall",
            "--offline",
        ],
    )
    .ok();
    log.assert_eq(
        "healed: offline needs no request",
        &net_log(&home).len(),
        &lines,
    );
}

/// `gh:o/foo` at `tag`, served beside v1 and made the latest.
fn serve_v(home: &Home, tag: &str) {
    let toc = format!("## Interface: 120105, 50504, 11509\n## Version: {tag}\n");
    let lua = format!("local version = \"{tag}\"\n");
    let flavors: &[(&str, u32)] = &[
        ("mainline", 120_105),
        ("mists", 50_504),
        ("classic", 11_509),
    ];
    let assets: [common::http::Asset<'_>; 1] = [(
        "Foo.zip",
        flavors,
        vec![("Foo/Foo.toc", toc.as_str()), ("Foo/Foo.lua", lua.as_str())],
    )];
    common::http::gh_release(&home.http, "https://github.com/o/foo", tag, &assets);
}

/// The blob names in the cache.
fn blob_names(home: &Home) -> BTreeSet<String> {
    fs::read_dir(home.env.cache().join("omaddons/blobs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.len() == 64)
        .collect()
}

#[test]
fn an_update_never_installs_a_damaged_cached_archive() {
    let log = tlog!();
    let (home, _, _, v1) = installed(&log, &["wow"]);
    serve_v(&home, "v2");
    log.step("preview");
    let cached = blob_names(&home);
    // `--refresh`: within the source's recheck interval the release memo answers, and it
    // remembers v1 as the newest.
    run(&log, &home, &["update", "--dry-run", "--refresh"]).ok();
    let new: Vec<String> = blob_names(&home).difference(&cached).cloned().collect();
    assert_eq!(new.len(), 1, "the dry run cached v2's archive: {new:?}");
    let v2 = new[0].clone();
    log.step("damage");
    let damaged = flip(&log, &plant::blob_path(&home, &v2));
    let trees = snapshot(&log, &home, &["_retail_"]);
    log.step("offline");
    let ran = run(&log, &home, &["update", "--offline"]);
    assert!(
        // ubs:ignore — comparing error codes, not secrets
        ran.envelope()["error"]["code"] != "offline",
        "update --offline could not answer v2's listing from the release memo: {}",
        ran.envelope()
    );
    // `update` fails with its one failed item's own error, whole (omad-r86s): the damage's
    // `details.blob` and `actual` beside `items`, as `install`'s carries them.
    let error = damage_error(&log, &ran, &v2);
    log.assert_eq(
        "details.actual",
        &error["details"]["actual"],
        &json!(sha(&damaged)),
    );
    log.assert_eq("retryable", &error["retryable"], &json!(true));
    log.assert_eq("its fix", &error["fix_argv"], &json!(DEEP));
    assert!(
        text_of(&error["message"]).contains(&format!("{}…", &v2[..12])),
        "the message names v2's archive: {}",
        error["message"]
    );
    log.assert_eq(
        "its item",
        &error["details"]["items"][0]["error"]["code"],
        &json!("hash_mismatch"),
    );
    unchanged(&log, &trees);
    log.assert_eq(
        "still v1",
        &tree_hash(&home.addons("_retail_").join("Foo")),
        &v1,
    );
    log.step("online");
    let asked = assets(&home);
    run(&log, &home, &["update"]).ok();
    log.assert_eq("downloaded once more", &assets(&home), &(asked + 1));
    log.assert_eq(
        "v2 installed",
        &common::state::record(&home, "retail").addons["foo"].version,
        &"v2".to_owned(),
    );
    log.assert_eq(
        "v2's blob healed",
        &sha(&fs::read(plant::blob_path(&home, &v2)).unwrap()),
        &v2,
    );
}

#[test]
fn a_second_client_installs_a_good_copy() {
    let log = tlog!();
    let (home, _, path, fresh) = installed(&log, &["wow", "wow_classic"]);
    log.step("damage");
    flip(&log, &path);
    log.step("run");
    let asked = assets(&home);
    run(&log, &home, &["install", "gh:o/foo", "-t", "classic"]).ok();
    log.step("check");
    log.assert_eq("downloaded once more", &assets(&home), &(asked + 1));
    log.assert_eq(
        "the second client's Foo is the release's",
        &tree_hash(&home.addons("_classic_").join("Foo")),
        &fresh,
    );
}

/// Replaces every fixture answering `url` with a 404.
fn not_served(home: &Home, url: &str) {
    for entry in walk(&home.http) {
        if entry.extension().is_some_and(|e| e == "json")
            && let Ok(doc) = serde_json::from_str::<Value>(&fs::read_to_string(&entry).unwrap())
            // ubs:ignore — comparing fixture URLs, not secrets
            && doc["request"]["url"] == url
        {
            fs::remove_file(&entry).unwrap();
        }
    }
    common::http::fixture(&home.http, "gone-foo-v1-asset", url, 404, &[], None);
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).into_iter().flatten() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

#[test]
fn a_source_that_no_longer_serves_the_archive_gives_hash_mismatch() {
    let log = tlog!();
    let (home, name, path, _) = installed(&log, &["wow"]);
    not_served(
        &home,
        "https://github.com/o/foo/releases/download/v1/Foo.zip",
    );
    log.step("damage");
    flip(&log, &path);
    let trees = snapshot(&log, &home, &["_retail_"]);
    log.step("run");
    let ran = run(
        &log,
        &home,
        &["install", "gh:o/foo", "-t", "retail", "--reinstall"],
    );
    damage_error(&log, &ran, &name);
    let snapshot_without_netlog = Snapshot {
        dirs: trees.dirs.clone(),
        files: trees
            .files
            .iter()
            .filter(|(label, _, _)| !label.ends_with("net.log"))
            .cloned()
            .collect(),
    };
    unchanged(&log, &snapshot_without_netlog);
}

/// A dry run's planned release ids and blobs, and its token.
fn preview(log: &TestLog, home: &Home) -> (Value, Value) {
    let ran = run(
        log,
        home,
        &["install", "gh:o/foo", "-t", "classic", "--dry-run"],
    );
    ran.ok();
    let items: Vec<Value> = ran.envelope()["data"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| json!({"release_id": i["release_id"], "to_version": i["to_version"], "blob": i["blob_sha256"]}))
        .collect();
    (json!(items), ran.envelope()["data"]["precondition"].clone())
}

#[test]
fn previews_of_an_unchanged_home_agree_across_a_heal() {
    let log = tlog!();
    let (home, _, path, _) = installed(&log, &["wow", "wow_classic"]);
    log.step("preview A");
    let (a, token_a) = preview(&log, &home);
    log.step("damage");
    flip(&log, &path);
    log.step("preview B");
    let asked = assets(&home);
    let (b, token_b) = preview(&log, &home);
    log.assert_eq("downloaded once more", &assets(&home), &(asked + 1));
    log.assert_eq("the same planned release", &b, &a);
    if token_a.is_string() {
        log.assert_eq("the same token", &token_b, &token_a);
    } else {
        log.note("the dry run returns no precondition: the token half was not checked");
    }
}
