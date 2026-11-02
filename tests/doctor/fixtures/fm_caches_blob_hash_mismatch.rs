use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use super::super::Fixture;
use super::found_ids;
use crate::common::doctor::plant;
use crate::common::home::Home;

const ID: &str = "fm-caches-blob-hash-mismatch";

/// `fm-caches-blob-hash-mismatch`: one byte of a cached archive changed, so its bytes no
/// longer hash to its name. `gh:o/foo` v1 is installed on retail from an HTTP fixture (so
/// its archive is cached as `<cache>/blobs/<sha256>` and its record names it), then the
/// blob's middle byte is flipped in place. A damaged cache is a disk fault or another
/// program's write: omaddons re-hashes a blob before each use and downloads it again when
/// online, but refuses offline (`hash_mismatch`).
///
/// The finding: P2, tier `deep` (so the diagnosing run adds `--include-deep`), about the
/// machine (`machine:blob=<sha256>`), evidence `{path, expected: "sha256:<name>",
/// observed: "sha256:<actual>"}`. Its fixer quarantines the blob into
/// `<cache>/quarantine/<run id>/blobs/`; a round trip compares the whole `<cache>/blobs/`,
/// `index.json` included.
pub struct BlobHashMismatch;

fn blobs(home: &Home) -> PathBuf {
    home.env.cache().join("omaddons/blobs")
}

fn is_blob_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Fixture for BlobHashMismatch {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        plant::damaged_blob(home, "retail");
    }

    /// Every regular file of `<cache>/blobs/` named by a blob name hashes to its name, and
    /// a deep diagnose finds no instance.
    fn assert_healthy(&self, home: &Home) {
        for entry in fs::read_dir(blobs(home)).into_iter().flatten() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_blob_name(&name) || !entry.file_type().unwrap().is_file() {
                continue;
            }
            let digest: String = Sha256::digest(fs::read(entry.path()).unwrap())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(
                digest,
                name,
                "{} hashes to its name",
                entry.path().display()
            );
        }
        let ids = found_ids(home, &self.run_args());
        assert!(!ids.iter().any(|id| id == ID), "{ids:?}");
    }

    fn subject(&self, home: &Home) -> Vec<PathBuf> {
        vec![blobs(home)]
    }

    fn fixer(&self) -> bool {
        true
    }

    fn run_args(&self) -> Vec<String> {
        vec!["--include-deep".to_owned()]
    }
}
