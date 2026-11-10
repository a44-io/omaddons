//! The retention fixer, of `snapshots-past-retention`: a client's undo snapshots that
//! snapshot retention would prune now are pruned by engine retention itself, which
//! `doctor fix` runs under its lock for exactly the transactions its preview listed
//! (`doctor::delegate`, `retention::prune_selected`). The doctor never prunes on its own
//! (M4-D36): this plan only says which, by the rule the check applies.

use jiff::Timestamp;

use crate::doctor::finding::Finding;
use crate::doctor::registry::Refusal;
use crate::doctor::view::DoctorView;
use crate::engine::repair_action::{Fix, RetentionPrune};
use crate::engine::retention::{self, RetentionLimits, Sizes};
use crate::state::paths::ClientPaths;

/// The fix of one finding: its client's due transactions, judged again on `view` as the
/// check judged them (without sizes, at the system clock's now); nothing when the client
/// is not discovered, the history cannot be read, or nothing is due any more.
pub fn plan(view: &DoctorView, finding: &Finding) -> Result<Vec<Fix>, Refusal> {
    let Some(key) = finding.target.as_ref().map(|t| t.key) else {
        return Ok(Vec::new());
    };
    let (Some(world), Some(history)) = (view.world(), view.history()) else {
        return Ok(Vec::new());
    };
    let Some(target) = world
        .resolved
        .targets
        .iter()
        .map(|d| &d.target)
        .find(|t| t.key == Some(key))
    else {
        return Ok(Vec::new());
    };
    let limits = view
        .config()
        .and_then(|c| c.config.effective_limits().ok())
        .map_or_else(RetentionLimits::default, |l| RetentionLimits::from(&l));
    let judged = retention::judge(
        view.paths(),
        &ClientPaths::new(&target.client_dir),
        key,
        &history.entries,
        &retention::pinned_by(&history.entries),
        &limits,
        Timestamp::now(),
        Sizes::Unmeasured,
    );
    let txs: Vec<_> = judged.due().iter().map(|t| t.tx).collect();
    if txs.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![Fix::Retain {
        what: RetentionPrune {
            target: key,
            client_dir: target.client_dir.clone(),
            txs,
            limits,
        },
    }])
}
