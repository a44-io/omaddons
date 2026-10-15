//! Staging end to end: inspected folders land under
//! `Interface/.omaddons/stage/<tx>/`, verified, normalized and indexed, with one
//! `syncfs` and AddOns untouched.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use common::archives::{RawEntry, raw_zip, zip_of, zip_text};
use common::env::TestEnv;
use common::http::paths;
use common::targets::battlenet;
use common::trees::{listing, tree_hash};
use omaddons::engine::inspect::{ArchiveLimits, Inspected, inspect};
use omaddons::engine::stage::{Staged, stage};
use omaddons::error::{ErrorCode, Result};
use omaddons::fsx::{InflightLock, ScopeTable, sync_counts};
use omaddons::model::{Target, TxId};
use omaddons::rules::RulesTable;
use omaddons::state::paths::Paths;
use support::{Fixture, mode, tree};

#[cfg(test)]
mod support {
    use super::*;

    /// A retail target, its paths and a scope table covering its client dir.
    pub struct Fixture {
        pub env: TestEnv,
        pub target: Target,
        pub paths: Paths,
        pub scopes: ScopeTable,
    }

    impl Fixture {
        pub fn new() -> Self {
            let env = TestEnv::new();
            let target = battlenet(&env, "wow");
            let paths = paths(&env);
            let scopes =
                ScopeTable::new(&paths, std::slice::from_ref(&target.client_dir), None).unwrap();
            Self {
                env,
                target,
                paths,
                scopes,
            }
        }

        /// `bytes` written as a blob and inspected.
        pub fn blob(&self, name: &str, bytes: &[u8]) -> (PathBuf, Inspected) {
            let blob = self.env.root().join(name);
            fs::write(&blob, bytes).unwrap();
            let inspected =
                inspect(&blob, &ArchiveLimits::default(), &RulesTable::builtin()).unwrap();
            (blob, inspected)
        }

        /// Stages `inspected` from `blob` under a fresh transaction.
        pub fn stage(&self, blob: &Path, inspected: &Inspected) -> Result<Staged> {
            let inflight = InflightLock::create(&self.paths, &TxId::new()).unwrap();
            stage(&inflight, &self.scopes, &self.target, Some(blob), inspected)
        }

        pub fn addons_hash(&self) -> String {
            tree_hash(&self.target.addons_dir())
        }
    }

    /// Paths of the regular files and directories under `dir`, relative, sorted.
    pub fn tree(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(at) = pending.pop() {
            for entry in fs::read_dir(&at).unwrap() {
                let path = entry.unwrap().path();
                let rel = path.strip_prefix(dir).unwrap().display().to_string();
                if path.is_dir() {
                    out.push(format!("{rel}/"));
                    pending.push(path);
                } else {
                    out.push(rel);
                }
            }
        }
        out.sort();
        out
    }

    /// Permission bits, setuid and friends included.
    pub fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }
}

#[test]
fn a_multi_folder_archive_stages_exactly_its_roots() {
    let fx = Fixture::new();
    let before = fx.addons_hash();
    let zip = raw_zip(&[
        RawEntry::stored("Repo-main/README.md", b"loose"),
        RawEntry::stored("Repo-main/A-main/A.toc", b"## Interface: 120105\n"),
        RawEntry::deflated("Repo-main/A-main/sub/deep/x.lua", &[b'-'; 3000]).mode(0o104_755),
        RawEntry::stored("Repo-main/B/B.toc", b"## Interface: 120105\n").mode(0o100_600),
    ]);
    let (blob, inspected) = fx.blob("multi.zip", &zip);
    let staged = fx.stage(&blob, &inspected).unwrap();

    let client = &fx.target.client_dir;
    assert_eq!(
        staged.stage_dir,
        client.join(format!("Interface/.omaddons/stage/{}", staged.tx))
    );
    assert_eq!(
        tree(&staged.stage_dir),
        [
            "A/",
            "A/A.toc",
            "A/sub/",
            "A/sub/deep/",
            "A/sub/deep/x.lua",
            "B/",
            "B/B.toc"
        ],
        "normalized names, roots only:\n{}",
        listing(&staged.stage_dir)
    );
    assert_eq!(fx.addons_hash(), before, "AddOns untouched");

    for path in ["A/A.toc", "A/sub/deep/x.lua", "B/B.toc"] {
        assert_eq!(mode(&staged.stage_dir.join(path)), 0o644, "{path}");
    }
    for path in ["A", "A/sub", "A/sub/deep", "B"] {
        assert_eq!(mode(&staged.stage_dir.join(path)), 0o755, "{path}");
    }

    for (folder, inspected_folder) in staged.folders.iter().zip(&inspected.folders) {
        assert_eq!(folder.path, staged.stage_dir.join(folder.name.as_str()));
        assert_eq!(folder.index.files.len(), inspected_folder.files.len());
        for file in &folder.index.files {
            let want = inspected_folder
                .files
                .iter()
                .find(|f| f.rel_path == file.path)
                .unwrap();
            assert_eq!(
                (file.size, &file.sha256),
                (want.size, &want.sha256),
                "{}",
                file.path
            );
            let meta = fs::metadata(folder.path.join(&file.path)).unwrap();
            assert_eq!(file.size, meta.len());
            assert!(file.mtime_ns > 0, "{file:?}");
        }
        assert!(
            folder.index.tree.starts_with("tree:sha256:"),
            "{}",
            folder.index.tree
        );
    }
}

#[test]
fn tree_hashes_repeat_across_transactions_and_one_syncfs_covers_a_stage() {
    let fx = Fixture::new();
    let files: Vec<(String, Vec<u8>)> = (0..60)
        .map(|i| {
            (
                format!("Foo-main/lib/f{i}.lua"),
                format!("-- {i}\n").into_bytes(),
            )
        })
        .chain([(
            "Foo-main/Foo.toc".to_owned(),
            b"## Interface: 120105\n".to_vec(),
        )])
        .collect();
    let entries: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, c)| (n.as_str(), c.as_slice()))
        .collect();
    let (blob, inspected) = fx.blob("foo.zip", &zip_of(&entries));

    let first = fx.stage(&blob, &inspected).unwrap();
    let counts = sync_counts();
    let second = fx.stage(&blob, &inspected).unwrap();
    let delta = sync_counts();
    assert_ne!(
        first.stage_dir, second.stage_dir,
        "one stage dir per transaction"
    );
    assert_eq!(first.folders[0].index.tree, second.folders[0].index.tree);
    assert_eq!(first.folders[0].name.as_str(), "Foo");
    assert_eq!(
        (delta.syncfs - counts.syncfs, delta.fsync - counts.fsync),
        (1, 0),
        "one syncfs per stage call and no fsync for 61 files"
    );
}

#[test]
fn a_blob_changed_after_inspection_is_refused() {
    let fx = Fixture::new();
    let (blob, inspected) = fx.blob(
        "foo.zip",
        &zip_text(&[
            ("Foo/Foo.toc", "## Interface: 120105\n"),
            ("Foo/a.lua", "one"),
        ]),
    );
    fs::write(
        &blob,
        zip_text(&[
            ("Foo/Foo.toc", "## Interface: 120105\n"),
            ("Foo/a.lua", "two"),
        ]),
    )
    .unwrap();
    let before = fx.addons_hash();
    let err = fx.stage(&blob, &inspected).unwrap_err();
    assert_eq!(
        (err.code, err.exit().code()),
        (ErrorCode::BadArchive, 1),
        "{}",
        err.message
    );
    assert_eq!(err.details["path"], "Foo/a.lua");
    assert_eq!(fx.addons_hash(), before);
}

#[test]
fn concurrent_transactions_stage_side_by_side() {
    let fx = Fixture::new();
    let (blob_a, inspected_a) = fx.blob(
        "a.zip",
        &zip_text(&[("A/A.toc", "## Interface: 120105\n"), ("A/a.lua", "a")]),
    );
    let (blob_b, inspected_b) = fx.blob(
        "b.zip",
        &zip_text(&[("B/B.toc", "## Interface: 120105\n"), ("B/b.lua", "b")]),
    );
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| fx.stage(&blob_a, &inspected_a).unwrap());
        let b = scope.spawn(|| fx.stage(&blob_b, &inspected_b).unwrap());
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(tree(&a.stage_dir), ["A/", "A/A.toc", "A/a.lua"]);
    assert_eq!(tree(&b.stage_dir), ["B/", "B/B.toc", "B/b.lua"]);
}

#[test]
fn five_thousand_small_files_within_budget() {
    let fx = Fixture::new();
    let files: Vec<(String, Vec<u8>)> = (0..5_000)
        .map(|i| {
            (
                format!("Big/d{}/f{i}.lua", i % 50),
                format!("return {i}\n").into_bytes(),
            )
        })
        .chain([("Big/Big.toc".to_owned(), b"## Interface: 120105\n".to_vec())])
        .collect();
    let entries: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, c)| (n.as_str(), c.as_slice()))
        .collect();
    let (blob, inspected) = fx.blob("big.zip", &zip_of(&entries));
    let started = Instant::now();
    let staged = fx.stage(&blob, &inspected).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(staged.folders[0].index.files.len(), 5_001);
    assert!(
        elapsed.as_secs_f64() < 5.0,
        "staging 5 001 files took {elapsed:?} (about 0.3 s on tmpfs in a debug build; the margin absorbs parallel test load)"
    );
}
