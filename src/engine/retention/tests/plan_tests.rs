//! Plan pins and bounded sweeps through retention's real eviction path.
#![cfg(test)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use crate::fsx::LockState;
use crate::limits::PLANS_SWEEP_MAX;
use crate::state::plans::test_support::{permission_skip, plan_with, plant};
use jiff::SignedDuration;

use super::*;

fn hash(c: char) -> String {
    c.to_string().repeat(64)
}
fn shifted(now: Timestamp, seconds: i64) -> Timestamp {
    now.checked_add(SignedDuration::from_secs(seconds)).unwrap()
}
fn cap() -> RetentionLimits {
    RetentionLimits {
        blob_cache_max_bytes: 0,
        ..RetentionLimits::default()
    }
}
fn blobs(f: &Fixture) {
    fs::create_dir_all(f.paths.blobs_dir()).unwrap();
    for c in ['a', 'b', 'c'] {
        fs::write(f.paths.blob(&hash(c)), [0; 100]).unwrap();
    }
}
fn run_at(
    f: &Fixture,
    limits: &RetentionLimits,
    pins: &BTreeSet<String>,
    clock: RetentionClock,
) -> RetentionReport {
    run(
        &f.paths,
        &f.scopes,
        f.key,
        &f.client,
        limits,
        clock,
        &|| Ok(pins.clone()),
    )
    .unwrap()
}
fn held(path: &Path) -> fs::File {
    let file = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    file.lock().unwrap();
    file
}

#[test]
fn an_unexpired_plans_blobs_survive_eviction() {
    let f = fixture();
    blobs(&f);
    let plan = plan_with(f.clock, &[Some(&hash('b'))]);
    plant(&f.paths, &plan);
    let report = f.run(&cap(), &BTreeSet::new());
    assert_eq!(report.blobs_evicted, 2, "{plan:?}: {report:?}");
    assert!(f.paths.blob(&hash('b')).exists(), "{report:?}");
    assert!(
        !f.paths.blob(&hash('a')).exists() && !f.paths.blob(&hash('c')).exists(),
        "{report:?}"
    );
    assert_eq!(report.plan_pins.unwrap().live, 1);
}

#[test]
fn an_expired_plans_blobs_are_evicted() {
    for busy in [false, true] {
        let f = fixture();
        blobs(&f);
        let plan = plan_with(shifted(f.clock, -25 * HOUR), &[Some(&hash('b'))]);
        let path = plant(&f.paths, &plan);
        let lock = busy.then(|| held(&f.paths.plans_lock()));
        let report = f.run(&cap(), &BTreeSet::new());
        assert_eq!(report.blobs_evicted, 3, "busy {busy}: {plan:?}: {report:?}");
        assert_eq!(
            report.plans_swept,
            usize::from(!busy),
            "busy {busy}: {report:?}"
        );
        assert_eq!(path.exists(), busy, "busy {busy}: {report:?}");
        let pins = report.plan_pins.unwrap();
        assert_eq!(
            (pins.read, pins.expired, pins.live),
            if busy { (1, 1, 0) } else { (0, 0, 0) },
            "busy {busy}: {pins:?}"
        );
        drop(lock);
    }
}

#[test]
fn record_and_plan_pins_are_both_kept() {
    let f = fixture();
    blobs(&f);
    plant(&f.paths, &plan_with(f.clock, &[Some(&hash('b'))]));
    let report = f.run(&cap(), &BTreeSet::from([hash('a')]));
    assert_eq!(report.blobs_evicted, 1, "{report:?}");
    assert!(
        f.paths.blob(&hash('a')).exists() && f.paths.blob(&hash('b')).exists(),
        "{report:?}"
    );
    assert!(!f.paths.blob(&hash('c')).exists(), "{report:?}");
}

#[test]
fn bad_plans_pin_nothing_and_fail_nothing() {
    for case in [
        "truncated",
        "oversized",
        "token_edited",
        "dated_ahead",
        "not_json",
    ] {
        let f = fixture();
        blobs(&f);
        plant(
            &f.paths,
            &plan_with(shifted(f.clock, -1), &[Some(&hash('c'))]),
        );
        let created = if case == "dated_ahead" {
            shifted(f.clock, 30 * DAY)
        } else {
            f.clock
        };
        let path = plant(&f.paths, &plan_with(created, &[Some(&hash('b'))]));
        let mut bytes = fs::read(&path).unwrap();
        match case {
            "truncated" => bytes.truncate(bytes.len() / 2),
            "oversized" => {
                bytes = vec![b' '; usize::try_from(crate::limits::PLAN_MAX_BYTES + 1).unwrap()]
            }
            "token_edited" => {
                let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                value["precondition"]["token"] = serde_json::Value::from("pc1:bad");
                bytes = serde_json::to_vec(&value).unwrap();
            }
            "not_json" => bytes = b"bad json".to_vec(),
            _ => {}
        }
        fs::write(&path, &bytes).unwrap();
        let report = f.run(&cap(), &BTreeSet::new());
        assert!(report.warnings.is_empty(), "{case}: {report:?}");
        assert!(!f.paths.blob(&hash('b')).exists(), "{case}: {report:?}");
        assert!(f.paths.blob(&hash('c')).exists(), "{case}: {report:?}");
        assert_eq!(fs::read(&path).unwrap(), bytes, "{case}: {report:?}");
    }
}

#[test]
fn an_unlistable_plan_store_pins_nothing_and_retention_completes() {
    let f = fixture();
    blobs(&f);
    let old = f.commit(10 * DAY, 1);
    f.commit(2 * HOUR, 1);
    f.commit(HOUR, 1);
    fs::create_dir_all(f.paths.plans_dir()).unwrap();
    fs::set_permissions(f.paths.plans_dir(), fs::Permissions::from_mode(0o000)).unwrap();
    let bypassed = fs::read_dir(f.paths.plans_dir()).is_ok();
    let result = run(
        &f.paths,
        &f.scopes,
        f.key,
        &f.client,
        &cap(),
        f.clock,
        &|| Ok(BTreeSet::new()),
    );
    fs::set_permissions(f.paths.plans_dir(), fs::Permissions::from_mode(0o700)).unwrap();
    if bypassed {
        permission_skip("an_unlistable_plan_store_pins_nothing_and_retention_completes");
        return;
    }
    let report = result.unwrap();
    assert_eq!(
        (report.pruned_tx, report.plans_swept, report.blobs_evicted),
        (1, 0, 3),
        "{report:?}"
    );
    assert_eq!(f.has_snapshot(&old), (false, false), "{report:?}");
    assert!(report.plan_sweep_failed, "{report:?}");
}

#[test]
fn plan_pins_are_read_inside_eviction_under_the_global_lock() {
    let f = fixture();
    blobs(&f);
    plant(&f.paths, &plan_with(f.clock, &[Some(&hash('b'))]));
    let calls = std::cell::Cell::new(0);
    let report = run(
        &f.paths,
        &f.scopes,
        f.key,
        &f.client,
        &cap(),
        f.clock,
        &|| {
            assert!(matches!(
                fsx::probe_global(&f.paths).unwrap(),
                LockState::Held { .. }
            ));
            calls.set(calls.get() + 1);
            Ok(BTreeSet::new())
        },
    )
    .unwrap();
    assert_eq!(calls.get(), 1, "{report:?}");
    assert_eq!(report.plan_pins.unwrap().live, 1);
    assert!(f.paths.blob(&hash('b')).exists());
}

#[test]
fn no_eviction_reads_no_plan() {
    let f = fixture();
    let path = plant(&f.paths, &plan_with(f.clock, &[Some(&hash('b'))]));
    fs::write(path, b"bad json").unwrap();
    let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
    assert!(report.plan_pins.is_none(), "{report:?}");
}

#[test]
fn plans_follow_the_plan_clock_and_snapshots_the_system_clock() {
    let mut counts = Vec::new();
    for advanced in [false, true] {
        let f = fixture();
        blobs(&f);
        f.commit(10 * DAY, 1);
        f.commit(2 * HOUR, 1);
        f.commit(HOUR, 1);
        let path = plant(&f.paths, &plan_with(f.clock, &[Some(&hash('b'))]));
        let clock = RetentionClock {
            system: f.clock,
            plans: shifted(f.clock, if advanced { 25 * HOUR } else { 0 }),
        };
        let report = run_at(&f, &cap(), &BTreeSet::new(), clock);
        counts.push((report.pruned_tx, report.kept_tx));
        assert_eq!(path.exists(), !advanced, "{clock:?}: {report:?}");
        assert_eq!(
            f.paths.blob(&hash('b')).exists(),
            !advanced,
            "{clock:?}: {report:?}"
        );
    }
    assert_eq!(
        counts[0], counts[1],
        "system snapshot policy unchanged: {counts:?}"
    );
}

#[test]
fn a_plain_timestamp_means_one_clock() {
    let t = Timestamp::now();
    assert_eq!(
        RetentionClock::from(t),
        RetentionClock {
            system: t,
            plans: t
        }
    );
}

#[test]
fn retention_sweeps_one_bounded_batch_of_expired_plans() {
    let f = fixture();
    let mut expired = Vec::new();
    for i in 0..70 {
        let created = shifted(f.clock, -2 * DAY)
            .checked_add(SignedDuration::from_millis(i))
            .unwrap();
        expired.push(plant(&f.paths, &plan_with(created, &[])));
    }
    let live = plant(&f.paths, &plan_with(f.clock, &[]));
    let before = fs::read(&live).unwrap();
    let live2 = plant(&f.paths, &plan_with(shifted(f.clock, -1), &[]));
    for expected in [PLANS_SWEEP_MAX, 6, 0] {
        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!(report.plans_swept, expected, "{report:?}");
        assert_eq!(fs::read(&live).unwrap(), before, "{report:?}");
        assert!(live2.exists(), "{report:?}");
        if expected == PLANS_SWEEP_MAX {
            assert!(
                expired[..PLANS_SWEEP_MAX].iter().all(|p| !p.exists()),
                "oldest first: {report:?}"
            );
            assert!(
                expired[PLANS_SWEEP_MAX..].iter().all(|p| p.exists()),
                "one batch only: {report:?}"
            );
        }
    }
    assert!(expired.iter().all(|p| !p.exists()));
}

#[test]
fn the_sweep_takes_free_leases_and_never_a_held_one() {
    let f = fixture();
    let e1 = plan_with(shifted(f.clock, -2 * DAY), &[]);
    let e2 = plan_with(shifted(f.clock, -2 * DAY + 1), &[]);
    let p1 = plant(&f.paths, &e1);
    let p2 = plant(&f.paths, &e2);
    let l1 = f.paths.plan_lease(&e1.plan_id);
    let l2 = f.paths.plan_lease(&e2.plan_id);
    fs::write(&l1, b"").unwrap();
    let lock = held(&l2);
    let orphan = f.paths.plan_lease(&crate::model::PlanId::new());
    fs::write(&orphan, b"").unwrap();
    let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
    assert!(
        !p1.exists() && !l1.exists() && !orphan.exists(),
        "{report:?}"
    );
    assert!(p2.exists() && l2.exists(), "{report:?}");
    drop(lock);
    f.run(&RetentionLimits::default(), &BTreeSet::new());
    assert!(!p2.exists() && !l2.exists());
}

#[test]
fn the_sweep_skips_while_admission_holds_its_lock() {
    let f = fixture();
    let paths: Vec<_> = (0..3)
        .map(|i| plant(&f.paths, &plan_with(shifted(f.clock, -2 * DAY + i), &[])))
        .collect();
    let lock = held(&f.paths.plans_lock());
    let start = Instant::now();
    let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
    assert!(start.elapsed() < Duration::from_secs(1), "{report:?}");
    assert_eq!(report.plans_swept, 0, "{report:?}");
    assert!(paths.iter().all(|p| p.exists()), "{report:?}");
    drop(lock);
    let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
    assert_eq!(report.plans_swept, 3, "{report:?}");
}

#[test]
fn the_sweep_runs_even_when_history_cannot_be_read() {
    let f = fixture();
    f.commit(HOUR, 1);
    let paths: Vec<_> = (0..3)
        .map(|i| plant(&f.paths, &plan_with(shifted(f.clock, -2 * DAY + i), &[])))
        .collect();
    let history = f.paths.history_file();
    fs::set_permissions(&history, fs::Permissions::from_mode(0o000)).unwrap();
    let bypassed = fs::read(&history).is_ok();
    let result = run(
        &f.paths,
        &f.scopes,
        f.key,
        &f.client,
        &RetentionLimits::default(),
        f.clock,
        &|| Ok(BTreeSet::new()),
    );
    fs::set_permissions(history, fs::Permissions::from_mode(0o600)).unwrap();
    if bypassed {
        permission_skip("the_sweep_runs_even_when_history_cannot_be_read");
        return;
    }
    assert!(result.is_err(), "{result:?}");
    assert!(
        paths.iter().all(|p| !p.exists()),
        "sweep preceded history: {result:?}"
    );
}

#[test]
fn a_home_without_a_plan_store_gets_none() {
    let f = fixture();
    let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
    assert!(!f.paths.plans_dir().exists(), "{report:?}");
    assert!(!report.plan_sweep_failed, "{report:?}");
}
