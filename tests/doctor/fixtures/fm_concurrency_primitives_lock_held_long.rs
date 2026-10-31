use std::cell::RefCell;
use std::path::PathBuf;

use jiff::{SignedDuration, Timestamp};
use omaddons::fsx::GlobalLockGuard;
use omaddons::limits::LOCK_HELD_LONG;

use super::super::Fixture;
use super::assert_cleared;
use crate::common::doctor::plant;
use crate::common::home::Home;

const ID: &str = "fm-concurrency_primitives-lock-held-long";
/// The command the holder records.
pub const COMMAND: &str = "update";
/// How far past `LOCK_HELD_LONG` the diagnosing runs' clock is.
pub const PAST: SignedDuration = SignedDuration::from_secs(60);

/// `fm-concurrency_primitives-lock-held-long` (P2, cached, about the machine): an
/// omaddons process has held the global lock for more than `LOCK_HELD_LONG` (10
/// minutes). `corrupt` takes the lock in this test process through the library, as
/// `update` (`plant::hold_lock`), and keeps the guard; every diagnosing run's clock is
/// frozen `LOCK_HELD_LONG` plus 60 seconds after it was taken (`OMADDONS_NOW`, `run_env`).
/// The lock file is never named or written by hand.
///
/// The finding: instance `…@machine`; evidence `pid` (this test process), `command:
/// update`, `age_s` about 660. Advice only: omaddons never touches the lock. Remedy: the
/// holder ends (the guard is dropped, here or with the fixture, so a failing case never
/// keeps it). No subject: nothing on disk is to repair.
#[derive(Default)]
pub struct LockHeldLong {
    guard: RefCell<Option<GlobalLockGuard>>,
    taken: RefCell<Option<Timestamp>>,
}

impl Fixture for LockHeldLong {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        let (guard, taken) = plant::hold_lock(home, COMMAND);
        self.guard.replace(Some(guard));
        self.taken.replace(Some(taken));
    }

    fn assert_healthy(&self, home: &Home) {
        assert_cleared(home, ID, None, &self.run_env());
    }

    fn subject(&self, _: &Home) -> Vec<PathBuf> {
        Vec::new()
    }

    fn run_env(&self) -> Vec<(String, String)> {
        let long = SignedDuration::try_from(LOCK_HELD_LONG).unwrap();
        self.taken
            .borrow()
            .iter()
            .map(|taken| {
                let now = taken.checked_add(long + PAST).unwrap();
                ("OMADDONS_NOW".to_owned(), now.to_string())
            })
            .collect()
    }

    fn remedy(&self, _: &Home) {
        drop(self.guard.take());
    }
}
