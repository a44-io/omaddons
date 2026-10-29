//! Valid stored plans shared by pin-discovery and retention unit tests.
#![cfg(test)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::*;

pub fn plan_with(created_at: Timestamp, blobs: &[Option<&str>]) -> StoredPlan {
    let mut plan = super::tests::plan_at(0, created_at);
    plan.plan_id = crate::model::PlanId::from_ulid(ulid::Ulid::from_parts(
        u64::try_from(created_at.as_millisecond()).unwrap(),
        0,
    ));
    plan.releases = blobs
        .iter()
        .enumerate()
        .map(|(i, hash)| StoredRelease {
            release_id: format!("gh:o/addon{i}@v1"),
            source: None,
            key: None,
            url: None,
            expected_hash: None,
            blob_sha256: hash.map(str::to_owned),
        })
        .collect();
    plan
}

pub fn plant(paths: &Paths, plan: &StoredPlan) -> PathBuf {
    fs::create_dir_all(paths.plans_dir()).unwrap();
    let path = paths.plan(&plan.plan_id);
    fs::write(&path, encode(plan).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(load(&path).is_ok(), "fixture plan must load: {plan:?}");
    path
}

pub fn paths() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let paths = Paths::with_roots(
        &root,
        root.join("config"),
        root.join("state"),
        root.join("cache"),
    );
    (dir, paths)
}

pub fn permission_skip(case: &str) {
    use std::io::Write;
    writeln!(
        std::io::stderr().lock(),
        "SKIP: {case}: running with permission overrides"
    )
    .unwrap();
}
