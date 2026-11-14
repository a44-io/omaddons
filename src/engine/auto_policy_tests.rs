//! RG-52c: the unattended policy under the commit lock. Age was decided when the plan
//! was made and can only have grown by the time the lock is taken, so it is not asked
//! again there; a skip mark can have been committed since, so it is. An unattended update
//! paused right before the lock (`engine.stage.synced`, as `tests/engine_concurrency.rs`
//! pauses its runs) while a skip of its release commits, then resumed, is a no-op: the
//! mark holds, and no folder moved.

#![cfg(test)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::edit::RecordEdit;
use super::plan::UpdatePolicy;
use super::testing::TxScene;
use crate::model::TxKind;

const V1: &[(&str, &str)] = &[
    ("Foo/Foo.toc", "## Interface: 120105\n## Version: 1\n"),
    ("Foo/core.lua", "v1"),
];
const V2: &[(&str, &str)] = &[
    ("Foo/Foo.toc", "## Interface: 120105\n## Version: 2\n"),
    ("Foo/core.lua", "v2"),
];

#[cfg(debug_assertions)]
#[test]
fn a_skip_committed_while_an_unattended_update_planned_is_honored_under_the_lock() {
    use super::testing::Pause;
    use crate::fsx::failpoint;
    let scene = TxScene::new();
    let v1 = scene.zip("v1.zip", V1);
    scene.run(vec![scene.install("gh:o/foo", &v1)]).unwrap();
    let v2 = scene.zip("v2.zip", V2);
    let mut req = scene.request(vec![scene.install("gh:o/foo", &v2)], None, TxKind::Update);
    // No wait: the gate takes the release, so only the mark can hold it.
    req.policy = UpdatePolicy::Unattended {
        min_age: std::time::Duration::ZERO,
        now: jiff::Timestamp::now(),
    };
    let pause = Pause::new();
    let before = scene.addons();
    std::thread::scope(|s| {
        let slow = s.spawn(|| {
            failpoint::with_spec_in(pause.dir(), "engine.stage.synced=pause", || {
                scene.with_ctx("update", |ctx| super::tx::run(&req, ctx))
            })
        });
        pause.reached("engine.stage.synced");
        let mark = RecordEdit::Skip {
            release: None,
            version: "2".into(),
        };
        let marked = scene.run(vec![scene.edit("Foo", mark)]).unwrap();
        assert!(marked.failures.is_empty(), "{:?}", marked.failures);
        pause.resume("engine.stage.synced");
        let done = slow.join().unwrap().unwrap();
        assert!(
            done.outcomes.iter().all(|o| o.tx.is_none()),
            "the update committed nothing: {done:?}"
        );
    });
    let foo = scene.record().addons["foo"].clone();
    assert_eq!(foo.version, "1", "the mark held");
    assert_eq!(foo.skipped_releases.len(), 1);
    assert_eq!(scene.addons(), before, "no folder moved");
}
