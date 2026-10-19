//! `prune()` across processes: a prune killed after renaming leaves only trash, which
//! the next prune sweeps, and deletion runs with the global lock already released.

use std::fs;
use std::path::PathBuf;

use omaddons::error::ErrorCode;
use omaddons::fsx::{
    GlobalLock, InflightLock, LockState, LockUse, PruneReason, probe_global, prune,
};

const TX: &str = "01JB8Q2VA00000000000000000";
const TX2: &str = "01JB8Q2VB00000000000000000";

#[cfg(test)]
mod support {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use omaddons::fsx::ScopeTable;
    use omaddons::state::paths::Paths;

    pub struct Layout {
        pub root: PathBuf,
        pub paths: Paths,
        pub client: PathBuf,
    }

    pub fn layout(root: &Path) -> Layout {
        let root = fs::canonicalize(root).unwrap();
        let paths = Paths::with_roots(
            &root,
            root.join(".config"),
            root.join(".local/state"),
            root.join(".cache"),
        );
        let client = root.join("wow/_retail_");
        fs::create_dir_all(client.join("Interface/AddOns")).unwrap();
        Layout {
            root,
            paths,
            client,
        }
    }

    pub fn snapshot(layout: &Layout, tx: &str) -> PathBuf {
        let dir = layout.client.join("Interface/.omaddons/snapshots").join(tx);
        fs::create_dir_all(dir.join("AddOns/Foo")).unwrap();
        fs::write(dir.join("AddOns/Foo/Foo.toc"), b"## Interface: 120105").unwrap();
        dir
    }

    /// Runs `child_prune` on `victim` with `failpoints` armed.
    pub fn spawn_prune(root: &Path, victim: &Path, failpoints: &str) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "child_prune",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("OMADDONS_PRUNE_TEST_ROOT", root)
            .env("OMADDONS_PRUNE_TEST_VICTIM", victim)
            .env("OMADDONS_FAILPOINTS", failpoints)
            .env("OMADDONS_FAILPOINT_DIR", root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Waits (20 s at most) until the child stops at the `pause` failpoint `name`, its
    /// marker in `root`.
    pub fn reached(root: &Path, name: &str) {
        let marker = root.join(format!("{name}.reached"));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "the child never reached {name}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn entries(dir: &Path) -> usize {
        fs::read_dir(dir).map_or(0, |d| d.count())
    }

    pub fn scopes(paths: &Paths, client: &Path) -> ScopeTable {
        ScopeTable::new(paths, &[client.to_path_buf()], None).unwrap()
    }
}

use support::{entries, layout, reached, scopes, snapshot, spawn_prune};

#[test]
#[ignore = "child process for the prune tests"]
fn child_prune() {
    // A child process of the prune tests, not a check: run on its own it does nothing.
    let Some(root) = std::env::var_os("OMADDONS_PRUNE_TEST_ROOT").map(PathBuf::from) else {
        return;
    };
    let victim = PathBuf::from(std::env::var_os("OMADDONS_PRUNE_TEST_VICTIM").unwrap());
    let l = layout(&root);
    let s = scopes(&l.paths, &l.client);
    prune(&s, PruneReason::Retention, &[victim], LockUse::Acquire).unwrap();
}

#[test]
fn a_killed_prune_leaves_trash_that_the_next_prune_sweeps() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let victim = snapshot(&l, TX);
    let status = spawn_prune(&l.root, &victim, "fsx.prune.renamed=abort")
        .wait()
        .unwrap();
    assert!(!status.success(), "the child should have aborted");
    let trash = l.client.join("Interface/.omaddons/trash");
    assert!(!victim.exists(), "renamed away under the lock");
    assert_eq!(entries(&trash), 1, "left in trash by the crash");

    let next = snapshot(&l, TX2);
    let s = scopes(&l.paths, &l.client);
    let report = prune(
        &s,
        PruneReason::Retention,
        std::slice::from_ref(&next),
        LockUse::Acquire,
    )
    .unwrap();
    assert_eq!(report.removed, std::slice::from_ref(&next));
    assert_eq!(
        entries(&trash),
        0,
        "the crashed prune's leftovers are swept"
    );
    assert_eq!(probe_global(&l.paths).unwrap(), LockState::Free);
}

#[test]
fn deletion_runs_without_the_global_lock() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let victim = snapshot(&l, TX);
    let trash = l.client.join("Interface/.omaddons/trash");
    let mut child = spawn_prune(&l.root, &victim, "fsx.prune.deleting=pause");
    // The child stops at fsx.prune.deleting with its entry still in trash: the lock is
    // already free then.
    reached(&l.root, "fsx.prune.deleting");
    assert!(!victim.exists(), "renamed into trash");
    assert_eq!(entries(&trash), 1);
    assert_eq!(
        probe_global(&l.paths).unwrap(),
        LockState::Free,
        "the lock was held while deleting"
    );
    fs::write(l.root.join("fsx.prune.deleting.resume"), b"").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(entries(&trash), 0);
}

#[test]
fn successful_prunes_empty_every_trash_side() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let client_snapshot = snapshot(&l, TX);
    let state_snapshot = l.paths.state_dir().join("snapshots").join(TX);
    fs::create_dir_all(&state_snapshot).unwrap();
    fs::write(state_snapshot.join("record.json"), b"{}").unwrap();
    let stage = l.client.join("Interface/.omaddons/stage").join(TX2);
    fs::create_dir_all(&stage).unwrap();
    let s = scopes(&l.paths, &l.client);
    prune(
        &s,
        PruneReason::Retention,
        &[client_snapshot.clone(), state_snapshot.clone()],
        LockUse::Acquire,
    )
    .unwrap();
    prune(
        &s,
        PruneReason::PostCommitCleanup,
        std::slice::from_ref(&stage),
        LockUse::Acquire,
    )
    .unwrap();
    for gone in [&client_snapshot, &state_snapshot, &stage] {
        assert!(!gone.exists(), "{}", gone.display());
    }
    assert_eq!(entries(&l.client.join("Interface/.omaddons/trash")), 0);
    assert_eq!(entries(&l.paths.state_dir().join("trash")), 0);
}

/// A transaction that never committed loses its snapshots under the global lock a
/// recovery holds, or under its own in-flight lock at cleanup, which reaches no other
/// transaction's.
#[test]
fn snapshots_prune_under_a_held_global_lock_or_their_own_transactions_lock() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let s = scopes(&l.paths, &l.client);
    let both = |tx: &str| {
        let state = l.paths.state_snapshot(&tx.parse().unwrap());
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("actions.jsonl"), b"").unwrap();
        [snapshot(&l, tx), state]
    };

    let held = both(TX);
    let guard = GlobalLock::acquire(&l.paths, "test").unwrap();
    prune(&s, PruneReason::Retention, &held, LockUse::Held(&guard)).unwrap();
    drop(guard);

    let own = both(TX2);
    let inflight = InflightLock::create(&l.paths, &TX2.parse().unwrap()).unwrap();
    prune(&s, PruneReason::Retention, &own, LockUse::Own(&inflight)).unwrap();
    for gone in held.iter().chain(&own) {
        assert!(!gone.exists(), "{}", gone.display());
    }
    let other = snapshot(&l, TX);
    let refused = prune(
        &s,
        PruneReason::Retention,
        std::slice::from_ref(&other),
        LockUse::Own(&inflight),
    )
    .unwrap_err();
    assert_eq!(refused.code, ErrorCode::ScopeDenied, "{}", refused.message);
    drop(inflight);
    assert!(other.exists(), "another transaction's snapshot stays");
}
