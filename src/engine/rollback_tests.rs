//! RG-34b: an install that is a rollback (`RollbackOf`) goes back only from the release
//! it was made for, and marks that release skipped (`origin: rollback`) in the same
//! commit, at planning and again under the commit lock.
//!
//! Each case installs Foo from two archives (`v1`, then `v2`) through `TxScene`, puts
//! v1's archive in the blob cache, and asks for the locked install of v1 that
//! `skip --back` builds: its version, its archive hash and its release id. The scene has
//! no network, no sources and no release memo, so a rollback that commits made no
//! request. The race under the lock is `tests/engine_concurrency.rs`'s, the crash matrix
//! `tests/engine_crash.rs`'s.

#![cfg(test)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::plan::{AnyFlavorIntent, PinIntent, RequestedChange, RollbackOf};
use super::testing::{TreeEntry, TxScene};
use crate::error::{ErrorBody, ErrorCode};
use crate::model::{
    AddonReason, AddonRecord, Channel, InstalledRelease, ReleaseKey, SkipOrigin, SkippedRelease,
    TxId,
};

const V1: &[(&str, &str)] = &[
    ("Foo/Foo.toc", "## Interface: 120105\n## Version: 1\n"),
    ("Foo/core.lua", "v1"),
];
const V2: &[(&str, &str)] = &[
    ("Foo/Foo.toc", "## Interface: 120105\n## Version: 2\n"),
    ("Foo/core.lua", "v2"),
];

fn sha256_of(path: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(std::fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `f` applied to Foo's entry in the stored record, its envelope kept.
fn edit_foo(scene: &TxScene, f: impl FnOnce(&mut serde_json::Value)) {
    let key = crate::discover::registry::read_marker(scene.client.client_dir()).unwrap();
    let path = scene.paths.target_record(&key);
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    f(&mut record["addons"]["foo"]);
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
}

/// Foo installed from v1, then updated to v2, with v1's archive in the blob cache: the
/// scene, and Foo's record as v1 left it. Both records carry their archive's hash, as an
/// install from a source records it (one from a local file, as here, has none).
fn at_v2() -> (TxScene, AddonRecord) {
    let scene = TxScene::new();
    let v1 = scene.zip("v1.zip", V1);
    scene.run(vec![scene.install("gh:o/foo", &v1)]).unwrap();
    let mut before = scene.record().addons["foo"].clone();
    let sha = sha256_of(&v1);
    before.blob_sha256 = Some(sha.clone());
    std::fs::create_dir_all(scene.paths.blobs_dir()).unwrap();
    std::fs::copy(&v1, scene.paths.blob(&sha)).unwrap();
    let v2 = scene.zip("v2.zip", V2);
    scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
    let v2_sha = sha256_of(&v2);
    edit_foo(&scene, |foo| foo["blob_sha256"] = serde_json::json!(v2_sha));
    (scene, before)
}

/// The locked install of `back` that `skip --back` builds, expecting `expect`.
fn rollback(
    back: &AddonRecord,
    expect: Option<InstalledRelease>,
    release_id: bool,
) -> RequestedChange {
    RequestedChange::Install {
        r#ref: back.r#ref.clone(),
        channel: Channel::Stable,
        channel_explicit: false,
        version: Some(back.version.clone()),
        file: None,
        name: None,
        pin: PinIntent::Keep,
        reason: AddonReason::Explicit,
        reinstall: false,
        blob_sha256: back.blob_sha256.clone(),
        asset: None,
        revision: back.revision,
        any_flavor: AnyFlavorIntent::Keep,
        rollback: Some(Box::new(RollbackOf {
            expect,
            restore_release_id: back.release_id.clone().filter(|_| release_id),
        })),
    }
}

fn foo(scene: &TxScene) -> AddonRecord {
    scene.record().addons["foo"].clone()
}

/// Runs `change`, which must be refused as the run's one item and commit nothing: the
/// refusal.
fn refused(scene: &TxScene, change: RequestedChange) -> ErrorBody {
    let done = scene.run(vec![change]).unwrap();
    assert!(done.outcomes.iter().all(|o| o.tx.is_none()), "{done:?}");
    assert_eq!(done.failures.len(), 1, "{:?}", done.failures);
    done.failures[0].error.clone()
}

/// The record file's bytes, the AddOns tree and the history's length: what a refused
/// change must leave as it was.
fn untouched(scene: &TxScene) -> (Vec<u8>, Vec<TreeEntry>, usize) {
    let key = crate::discover::registry::read_marker(scene.client.client_dir()).unwrap();
    let record = std::fs::read(scene.paths.target_record(&key)).unwrap();
    let history = std::fs::read_to_string(scene.paths.history_file())
        .map(|h| h.lines().count())
        .unwrap_or(0);
    (record, scene.addons(), history)
}

#[test]
fn a_rollback_from_the_expected_release_installs_the_old_one_and_marks_the_one_left() {
    let (scene, v1) = at_v2();
    let v2 = foo(&scene);
    let done = scene
        .run(vec![rollback(&v1, Some(InstalledRelease::of(&v2)), true)])
        .unwrap();
    assert!(done.failures.is_empty(), "{:?}", done.failures);
    let tx = done.outcomes[0].tx.unwrap();
    let after = foo(&scene);
    assert_eq!(
        (&after.version, &after.release_id, &after.blob_sha256),
        (&v1.version, &v1.release_id, &v1.blob_sha256),
        "v1's exact files, from the cache: no source asked"
    );
    assert_eq!(
        std::fs::read_to_string(scene.client.addons().join("Foo/core.lua")).unwrap(),
        "v1"
    );
    let marks = &after.skipped_releases;
    assert_eq!(marks.len(), 1, "{marks:?}");
    let mark = &marks[0];
    assert_eq!(
        (&mark.release, mark.version.as_str(), mark.origin, mark.tx),
        (&ReleaseKey::of_record(&v2), "2", SkipOrigin::Rollback, tx),
        "the release left, stamped by the rollback's own commit"
    );
    assert_eq!(after.pinned, v2.pinned, "the pin stays as it was");
}

#[test]
fn a_rollback_planned_against_another_release_is_plan_stale_and_writes_nothing() {
    let (scene, v1) = at_v2();
    let v2 = foo(&scene);
    let other_revision = {
        let mut r = InstalledRelease::of(&v2);
        r.release.as_mut().unwrap().revision = Some(7);
        r
    };
    let other_bytes = {
        let mut r = InstalledRelease::of(&v2);
        r.witness.blob_sha256 = Some("0".repeat(64));
        r
    };
    for (why, expect) in [
        ("another release", InstalledRelease::of(&v1)),
        ("another revision of the same tag", other_revision),
        ("the same tag with another hash", other_bytes),
    ] {
        let before = untouched(&scene);
        let refused = refused(&scene, rollback(&v1, Some(expect), true));
        assert_eq!(
            (refused.code, refused.retryable),
            (ErrorCode::PlanStale, false),
            "{why}: {refused:?}"
        );
        assert_eq!(
            refused.details["changed"],
            serde_json::json!(["targets"]),
            "{why}"
        );
        assert_eq!(untouched(&scene), before, "{why}: nothing written or moved");
        assert!(foo(&scene).skipped_releases.is_empty(), "{why}: no mark");
    }

    // The addon gone since: refused the same way.
    scene.run(vec![scene.remove("Foo")]).unwrap();
    let before = untouched(&scene);
    let refused = refused(&scene, rollback(&v1, Some(InstalledRelease::of(&v2)), true));
    assert_eq!(refused.code, ErrorCode::PlanStale, "{refused:?}");
    assert_eq!(untouched(&scene), before);
}

#[test]
fn a_rollback_past_the_mark_limit_fails_before_it_commits() {
    let (scene, v1) = at_v2();
    let v2 = foo(&scene);
    let marks: Vec<SkippedRelease> = (0..crate::limits::SKIPS_PER_ADDON_MAX)
        .map(|n| {
            let mut mark = SkippedRelease::pending(None, format!("9.{n}"), SkipOrigin::Manual);
            mark.tx = TxId::new();
            mark
        })
        .collect();
    edit_foo(&scene, |foo| {
        foo["skipped_releases"] = serde_json::to_value(&marks).unwrap();
    });
    assert_eq!(
        foo(&scene).skipped_releases.len(),
        crate::limits::SKIPS_PER_ADDON_MAX
    );
    let before = untouched(&scene);
    let refused = refused(&scene, rollback(&v1, Some(InstalledRelease::of(&v2)), true));
    assert_eq!(refused.code, ErrorCode::Usage, "{refused:?}");
    assert!(
        refused.message.contains("skipped releases"),
        "{}",
        refused.message
    );
    assert_eq!(
        untouched(&scene),
        before,
        "nothing installed, nothing marked"
    );
}

#[test]
fn an_offline_rollback_names_its_release_itself() {
    // No release memo here at all: without the request's release id, nothing names
    // the cached archive's release, and the install would have to ask a source.
    let (scene, v1) = at_v2();
    let v2 = foo(&scene);
    let without = scene
        .run(vec![rollback(&v1, Some(InstalledRelease::of(&v2)), false)])
        .unwrap();
    assert!(
        !without.failures.is_empty() && without.outcomes.iter().all(|o| o.tx.is_none()),
        "it needed a source: {without:?}"
    );
    assert_eq!(foo(&scene).version, "2");
    let with = scene
        .run(vec![rollback(&v1, Some(InstalledRelease::of(&v2)), true)])
        .unwrap();
    assert!(with.failures.is_empty(), "{:?}", with.failures);
    assert_eq!(foo(&scene).version, "1", "from the cache, with no request");
}

#[test]
fn the_label_form_marks_without_an_expectation() {
    let (scene, v1) = at_v2();
    let v2 = foo(&scene);
    let done = scene.run(vec![rollback(&v1, None, true)]).unwrap();
    assert!(done.failures.is_empty(), "{:?}", done.failures);
    let after = foo(&scene);
    assert_eq!(after.version, "1");
    assert_eq!(
        after
            .skipped_releases
            .iter()
            .map(|m| (m.release.clone(), m.origin))
            .collect::<Vec<_>>(),
        [(ReleaseKey::of_record(&v2), SkipOrigin::Rollback)]
    );
}

/// Under the lock: an update that commits after the rollback planned, before it takes
/// the lock (paused at `engine.stage.synced`, as `tests/engine_concurrency.rs`'s runs
/// pause), leaves the rollback `plan_stale` and moves no folder.
#[cfg(debug_assertions)]
#[test]
fn an_update_committed_while_a_rollback_planned_makes_it_plan_stale() {
    use super::testing::Pause;
    use crate::fsx::failpoint;
    const V3: &[(&str, &str)] = &[
        ("Foo/Foo.toc", "## Interface: 120105\n## Version: 3\n"),
        ("Foo/core.lua", "v3"),
    ];
    let (scene, v1) = at_v2();
    let expect = InstalledRelease::of(&foo(&scene));
    let v3 = scene.zip("v3.zip", V3);
    let pause = Pause::new();
    std::thread::scope(|s| {
        let slow = s.spawn(|| {
            failpoint::with_spec_in(pause.dir(), "engine.stage.synced=pause", || {
                scene.run(vec![rollback(&v1, Some(expect.clone()), true)])
            })
        });
        pause.reached("engine.stage.synced");
        let fast = scene.run(vec![scene.install("gh:o/foo", &v3)]).unwrap();
        assert!(fast.failures.is_empty(), "{:?}", fast.failures);
        let moved = untouched(&scene);
        pause.resume("engine.stage.synced");
        let slow = slow.join().unwrap();
        let refused = match slow {
            Err(e) => e.code,
            Ok(done) => {
                assert!(done.outcomes.iter().all(|o| o.tx.is_none()), "{done:?}");
                done.failures[0].error.code
            }
        };
        assert_eq!(refused, ErrorCode::PlanStale);
        assert_eq!(untouched(&scene), moved, "no folder moved, no mark");
    });
    let after = foo(&scene);
    assert_eq!(after.version, "3");
    assert!(after.skipped_releases.is_empty());
}

/// A rollback stopped at each engine failpoint (an error there, then the next command,
/// which recovers): the record is at the newer release without the mark or at the older
/// one with it, never the older without the mark nor the newer with it. The process
/// crash at each failpoint is `tests/engine_crash.rs`'s matrix over the same steps.
#[cfg(debug_assertions)]
#[test]
fn a_rollback_stopped_anywhere_keeps_the_release_and_its_mark_together() {
    use crate::fsx::failpoint;
    let consistent = |scene: &TxScene, when: &str, name: &str| -> String {
        let foo = foo(scene);
        let marked = foo
            .skipped_releases
            .iter()
            .any(|m| m.origin == SkipOrigin::Rollback);
        assert!(
            matches!((foo.version.as_str(), marked), ("2", false) | ("1", true)),
            "{name}, {when}: at {} with a rollback mark: {marked}",
            foo.version
        );
        foo.version
    };
    let mut seen = std::collections::BTreeSet::new();
    for name in super::tx::FAILPOINTS {
        let (scene, v1) = at_v2();
        let expect = InstalledRelease::of(&foo(&scene));
        let _ = failpoint::with_spec(&format!("{name}=error"), || {
            scene.run(vec![rollback(&v1, Some(expect.clone()), true)])
        });
        seen.insert(consistent(&scene, "after the error", name));
        // The next command recovers whatever was left.
        let _ = scene.run(vec![
            scene.edit("Foo", super::edit::RecordEdit::SetPinned(false)),
        ]);
        consistent(&scene, "after recovery", name);
    }
    assert_eq!(
        seen,
        ["1".to_owned(), "2".to_owned()].into(),
        "failpoints both before and after the commit"
    );
}
