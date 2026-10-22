//! The release memo as read-only commands use it (`status`, `list`, `errors`): read
//! once, never written, never fetched.

use std::collections::BTreeSet;

use jiff::Timestamp;

use super::world::World;
use crate::model::{AddonRecord, Channel, Ref, Target};
use crate::resolve::updates::{StatusRow, installed_primary_interface, statuses};
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
        )
    }

    pub(crate) fn fetched_at(&self, a: &AddonRecord) -> Option<Timestamp> {
        self.memo
            .entries
            .get(&memo::key(&a.r#ref, a.channel))
            .map(|e| e.fetched_at)
    }
}
