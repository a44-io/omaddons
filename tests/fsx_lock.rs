//! The global and in-flight locks across processes. Each lock is held by a child
//! process (this test binary re-run with an `#[ignore]`d helper selected by name), so
//! contention happens between separate open file descriptions, as in real use.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use omaddons::error::ErrorCode;
use omaddons::fsx::{GlobalLock, InflightLock, LockState, probe_global};
use omaddons::model::TxId;

use support::{inode, now_ns, paths, spawn_holder, wait_for};

#[cfg(test)]
mod support {
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use omaddons::state::paths::Paths;

    pub fn paths(home: &Path) -> Paths {
        Paths::with_roots(
            home,
            home.join(".config"),
            home.join(".local/state"),
            home.join(".cache"),
        )
    }

    /// Spawns the `child_holder` helper: it takes the lock named by `mode` (`global` or
    /// an in-flight tx id), writes `held` into `home`, holds for `hold_ms`, writes
    /// `released` with the release time in nanoseconds, and exits. A `pause` failpoint
    /// keeps its markers in `home`.
    pub fn spawn_holder(home: &Path, mode: &str, hold_ms: u64, failpoints: &str) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "fsx_lock::child_holder",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("OMADDONS_LOCK_TEST_HOME", home)
            .env("OMADDONS_LOCK_TEST_MODE", mode)
            .env("OMADDONS_LOCK_TEST_HOLD_MS", hold_ms.to_string())
            .env("OMADDONS_FAILPOINTS", failpoints)
            .env("OMADDONS_FAILPOINT_DIR", home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    pub fn wait_for(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn now_ns() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    pub fn inode(path: &Path) -> (u64, u64) {
        let meta = fs::metadata(path).unwrap();
        (meta.dev(), meta.ino())
    }
}

#[test]
#[ignore = "child process for the lock tests"]
fn child_holder() {
    // A child process of the lock tests, not a check: run on its own it does nothing.
    let Some(home) = std::env::var_os("OMADDONS_LOCK_TEST_HOME").map(PathBuf::from) else {
        return;
    };
    let mode = std::env::var("OMADDONS_LOCK_TEST_MODE").unwrap();
    let hold = Duration::from_millis(
        std::env::var("OMADDONS_LOCK_TEST_HOLD_MS")
            .unwrap()
            .parse()
            .unwrap(),
    );
    let p = paths(&home);
    let guard: Box<dyn std::any::Any> = if mode == "global" {
        Box::new(GlobalLock::acquire(&p, "install").unwrap())
    } else {
        Box::new(InflightLock::create(&p, &mode.parse::<TxId>().unwrap()).unwrap())
    };
    fs::write(home.join("held"), b"").unwrap();
    // The hold the tests measure a waiter against; `held` above is what orders them.
    std::thread::sleep(hold);
    fs::write(home.join("released"), now_ns().to_string()).unwrap();
    drop(guard);
}

#[test]
fn two_processes_serialize() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut child = spawn_holder(dir.path(), "global", 300, "");
    wait_for(&dir.path().join("held"));
    let guard = GlobalLock::acquire(&p, "update").unwrap();
    let acquired_ns = now_ns();
    let released_ns: u128 = fs::read_to_string(dir.path().join("released"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        acquired_ns >= released_ns,
        "acquired before the holder released"
    );
    drop(guard);
    assert!(child.wait().unwrap().success());
}

#[test]
fn a_timeout_reports_locked_with_the_holder() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut child = spawn_holder(dir.path(), "global", 3_000, "");
    wait_for(&dir.path().join("held"));
    let started = Instant::now();
    let err =
        GlobalLock::acquire_with_timeout(&p, "update", Duration::from_millis(200)).unwrap_err();
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(190) && waited < Duration::from_secs(2),
        "{waited:?}"
    );
    assert_eq!(
        (err.code, err.exit().code(), err.retryable),
        (ErrorCode::Locked, 5, true)
    );
    assert_eq!(err.details["holder"]["pid"], child.id());
    assert_eq!(err.details["holder"]["command"], "install");
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn sigkill_frees_the_lock_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut child = spawn_holder(dir.path(), "global", 60_000, "");
    wait_for(&dir.path().join("held"));
    assert!(matches!(probe_global(&p).unwrap(), LockState::Held { .. }));
    child.kill().unwrap();
    child.wait().unwrap();
    // The kernel freed the flock when the holder died: free at once, and a zero timeout
    // (one try) takes it.
    assert_eq!(probe_global(&p).unwrap(), LockState::Free);
    GlobalLock::acquire_with_timeout(&p, "update", Duration::ZERO).unwrap();
}

#[test]
fn the_lock_file_keeps_its_inode() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    drop(GlobalLock::acquire(&p, "install").unwrap());
    let original = inode(&p.lock_file());
    for i in 0..100 {
        let command = if i % 2 == 0 {
            "install"
        } else {
            "a-much-longer-command-name"
        };
        drop(GlobalLock::acquire(&p, command).unwrap());
    }
    let mut child = spawn_holder(dir.path(), "global", 60_000, "");
    wait_for(&dir.path().join("held"));
    child.kill().unwrap();
    child.wait().unwrap();
    drop(GlobalLock::acquire(&p, "install").unwrap());
    assert_eq!(inode(&p.lock_file()), original);
}

#[test]
fn probe_reports_missing_held_and_free() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    assert_eq!(probe_global(&p).unwrap(), LockState::Missing);
    assert!(!p.lock_file().exists(), "probing creates nothing");

    // The holder pauses inside the failpoint between the flock and the holder write, so
    // the probe sees the lock held while the holder info is still unwritten.
    let mut child = spawn_holder(dir.path(), "global", 0, "fsx.lock.acquired=pause");
    wait_for(&dir.path().join("fsx.lock.acquired.reached"));
    assert_eq!(probe_global(&p).unwrap(), LockState::Held { holder: None });
    fs::write(dir.path().join("fsx.lock.acquired.resume"), b"").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(probe_global(&p).unwrap(), LockState::Free);
}

#[test]
fn inflight_liveness_follows_the_holder() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let tx = TxId::new();
    assert!(
        !InflightLock::is_live(&p, &tx).unwrap(),
        "missing is not live"
    );
    let mut child = spawn_holder(dir.path(), &tx.to_string(), 60_000, "");
    wait_for(&dir.path().join("held"));
    assert!(InflightLock::is_live(&p, &tx).unwrap());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!InflightLock::is_live(&p, &tx).unwrap());
}
