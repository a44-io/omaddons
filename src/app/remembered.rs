//! The release memo as read-only commands use it (`status`, `list`, `errors`): read
//! once, never written, never fetched.

use std::collections::BTreeSet;

use jiff::Timestamp;

use super::world::World;
use crate::model::{AddonRecord, Channel, Ref, Release, Target};
use crate::resolve::updates::{StatusRow, installed_primary_interface, same_release, statuses};
use crate::rules::RulesTable;
use crate::sources::{SourceId, SourceRegistry};
use crate::state::memo::{self, FactIndex, ReleaseMemo};
use crate::state::records::TargetRecord;

/// The release memo as read-only commands use it: read once, never written, never
/// fetched; a memo that cannot be read is empty (`note` says why, for `-v`).
pub(crate) struct Remembered {
    memo: ReleaseMemo,
    facts: FactIndex,
    enabled: BTreeSet<SourceId>,
    pub note: Option<String>,
}

impl Remembered {
    pub(crate) fn load(world: &World) -> Self {
        let (memo, note) = memo::load(&world.paths);
        let facts = FactIndex::new(&memo.candidates);
        Self {
            memo,
            facts,
            enabled: SourceRegistry::builtin(&world.sources()).enabled(),
            note,
        }
    }

    /// What the memo says about each of `record`'s addons on `target`, from the shared
    /// evaluator (the statuses `outdated` shows).
    pub(crate) fn statuses(
        &self,
        target: &Target,
        record: &TargetRecord,
        table: &RulesTable,
    ) -> Vec<StatusRow> {
        let entry = |r: &Ref, c: Channel| self.memo.entries.get(&memo::key(r, c)).cloned();
        statuses(
            &[(target, record)],
            &entry,
            &self.facts,
            &self.enabled,
            &|t, a| installed_primary_interface(t, a, table),
            &|r, c, family, newest| self.memo.look_back(r, c, family, newest),
        )
    }

    /// The release `release_id` (compared as [`same_release`] does) among the newest the
    /// memo holds for `r` on channel `c`, with its version and revision; `None` when the
    /// memo does not hold it.
    pub(crate) fn release(&self, r: &Ref, c: Channel, release_id: &str) -> Option<&Release> {
        self.memo
            .entries
            .get(&memo::key(r, c))?
            .set
            .as_ref()?
            .releases
            .iter()
            .find(|release| same_release(&release.release_id, release_id))
    }

    pub(crate) fn fetched_at(&self, a: &AddonRecord) -> Option<Timestamp> {
        self.memo
            .entries
            .get(&memo::key(&a.r#ref, a.channel))
            .map(|e| e.fetched_at)
    }
}
