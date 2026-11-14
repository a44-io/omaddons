//! The ways back from the change before a new error (regression guard, design 7.4.1,
//! RG-40). A link without a way back is a diagnosis without a treatment, so a qualifying
//! link carries the commands that act on it, and the command stays true to the link it
//! came from: the rollback names the transaction it goes back from (`skip --back --tx`),
//! and refuses if the addon has moved on since.
//!
//! A link qualifies when it is `current`, `high` or `medium`, on an error that is new,
//! and its first changed addon is still at the release that change left it at. A `low`
//! link never gets one (decision RG-D9): a weak link is not a reason to change anything.

use std::cell::OnceCell;
use std::path::Path;

use crate::app::errors::{AfterChange, LinkState};
use crate::app::list::Latest;
use crate::contract::actions::Action;
use crate::engine::blobs::BlobCache;
use crate::engine::plan::{Restorable, restorable};
use crate::error::CommandHint;
use crate::errors::attribute::Confidence;
use crate::model::{TransitionChange, TxId};
use crate::resolve::updates::StatusKind;
use crate::state::history::{HistoryEntry, HistoryFilter};
use crate::state::paths::Paths;
use crate::state::records::TargetRecord;

/// One way back, in the order they are offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A release newer than the one the change installed: the author's fix first.
    Update,
    /// The one addon back to the files installed before the change.
    Rollback,
    /// The whole transaction undone, which its snapshot can do.
    Undo,
}

/// What the ways back from one link are decided from (design 7.4.1's table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// A release newer than the one the change installed is available for the addon.
    pub newer: bool,
    /// What the change did to the addon: `updated`, `installed` (nothing before it), or
    /// `reverted` (the change was itself an undo).
    pub change: TransitionChange,
    /// Whether the files before can be had again exactly (`No` when there were none).
    pub restorable: Restorable,
    /// Whether the transaction could be undone now.
    pub undoable: bool,
}

/// The ways back, best first (design 7.4.1): `update` when a newer release is out; then
/// the rollback of an update whose earlier files can be had; else the transaction's undo
/// while it can run (an update whose files are gone, a first install, or an undo, whose
/// own undo is the redo); else nothing more.
pub fn decide(f: Facts) -> Vec<Step> {
    let mut steps = Vec::new();
    if f.newer {
        steps.push(Step::Update);
    }
    let restorable = matches!(f.restorable, Restorable::Cached | Restorable::Fetchable);
    match f.change {
        TransitionChange::Updated if restorable => steps.push(Step::Rollback),
        TransitionChange::Updated | TransitionChange::Installed | TransitionChange::Reverted
            if f.undoable =>
        {
            steps.push(Step::Undo);
        }
        _ => {}
    }
    steps
}

/// Whether `link`, on an error that is `new` or not, may carry a way back at all.
pub fn qualifies(link: &AfterChange, new: bool) -> bool {
    new && link.state == LinkState::Current
        && matches!(link.confidence, Confidence::High | Confidence::Medium)
        && link.changed.first().is_some_and(|c| c.still_installed)
}

/// What one client's ways back are worked out against. History is read once, and only
/// when a link needs to know whether its transaction can be undone.
pub struct Remedies<'a> {
    paths: &'a Paths,
    client_dir: &'a Path,
    target: &'a str,
    record: Option<&'a TargetRecord>,
    blobs: BlobCache,
    history: OnceCell<Option<Vec<HistoryEntry>>>,
}

impl<'a> Remedies<'a> {
    pub fn new(
        paths: &'a Paths,
        client_dir: &'a Path,
        target: &'a str,
        record: Option<&'a TargetRecord>,
    ) -> Self {
        Self {
            paths,
            client_dir,
            target,
            record,
            blobs: BlobCache::new(paths),
            history: OnceCell::new(),
        }
    }

    /// Every readable line of history; `None` when it cannot be read, so nothing is
    /// offered that needs it.
    fn history(&self) -> Option<&[HistoryEntry]> {
        self.history
            .get_or_init(|| {
                crate::state::history::read(self.paths, &HistoryFilter::default())
                    .ok()
                    .map(|read| read.entries)
            })
            .as_deref()
    }

    /// Whether `tx` could be undone now, and the other addons its undo would move back.
    fn undo_of(&self, tx: TxId, addon: &str) -> (bool, Vec<String>) {
        let Some(entries) = self.history() else {
            return (false, Vec::new());
        };
        let Some(at) = entries.iter().position(|e| e.tx == tx) else {
            return (false, Vec::new());
        };
        let undoable = crate::engine::undo::undoable_now(self.paths, self.client_dir, entries, at);
        let mut others: Vec<String> = entries[at]
            .ops
            .iter()
            .map(|o| o.addon.clone())
            .filter(|a| !a.is_empty() && !a.eq_ignore_ascii_case(addon))
            .collect();
        others.sort_by_key(|a| a.to_ascii_lowercase());
        others.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        (undoable, others)
    }

    /// Fills `link.commands` (and `link.undo_also`) with its ways back, best first;
    /// nothing when it does not qualify (`new`: the error is new). `latest` is what the
    /// release memo says is newest for an addon.
    pub fn fill(
        &self,
        link: &mut AfterChange,
        new: bool,
        latest: &dyn Fn(&str) -> Option<(String, Latest)>,
    ) {
        link.commands.clear();
        if !qualifies(link, new) {
            return;
        }
        let Some(changed) = link.changed.first() else {
            return;
        };
        let Some(a) = self
            .record
            .and_then(|r| r.addons.get(&TargetRecord::addon_key(&changed.addon)))
        else {
            return;
        };
        let newer = latest(&a.name).is_some_and(|(_, l)| l.status == StatusKind::UpdateAvailable);
        let restorable = changed
            .from_artifact
            .as_ref()
            .filter(|_| changed.change == TransitionChange::Updated)
            .map_or(Restorable::No, |from| {
                restorable(&a.r#ref, from, &self.blobs)
            });
        let rollback = matches!(restorable, Restorable::Cached | Restorable::Fetchable);
        // History is read only when the undo is what would be offered.
        let (undoable, others) = if rollback {
            (false, Vec::new())
        } else {
            self.undo_of(link.tx, &a.name)
        };
        let steps = decide(Facts {
            newer,
            change: changed.change,
            restorable,
            undoable,
        });
        let tx = link.tx.to_string();
        let target = self.target.to_owned();
        let name = a.name.clone();
        link.commands = steps
            .iter()
            .map(|step| match step {
                Step::Update => CommandHint::new(
                    Action::Update,
                    ["omaddons", "update", &name, "-t", &target].map(str::to_owned),
                ),
                Step::Rollback => CommandHint::new(
                    Action::Rollback,
                    [
                        "omaddons", "skip", &name, "--back", "--tx", &tx, "-t", &target,
                    ]
                    .map(str::to_owned),
                ),
                Step::Undo => {
                    CommandHint::new(Action::Undo, ["omaddons", "undo", &tx].map(str::to_owned))
                }
            })
            .collect();
        link.undo_also = if steps.contains(&Step::Undo) {
            others
        } else {
            Vec::new()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::errors::ChangedNow;
    use crate::errors::suspect::{Evidence, Relation};
    use crate::model::{Actor, TxKind};

    /// A link of `confidence` in `state`, its first addon still installed or not.
    fn link(confidence: Confidence, state: LinkState, still_installed: bool) -> AfterChange {
        AfterChange {
            tx: TxId::new(),
            run: None,
            kind: TxKind::Update,
            at: jiff::Timestamp::UNIX_EPOCH,
            actor: Actor::Human,
            relation: Relation::Own,
            confidence,
            state,
            algorithm: 1,
            judged_at: jiff::Timestamp::UNIX_EPOCH,
            changed: vec![ChangedNow {
                addon: "Foo".to_owned(),
                change: TransitionChange::Updated,
                from_version: Some("1".to_owned()),
                to_version: Some("2".to_owned()),
                from_release: None,
                to_release: None,
                still_installed,
                from_artifact: None,
            }],
            evidence: Evidence {
                gap_s: 0,
                exact: false,
                same_session: Some(false),
                clean_sessions: Some(0),
                client_update: None,
                other_changes: 0,
                unknowns: Vec::new(),
            },
            commands: Vec::new(),
            undo_also: Vec::new(),
        }
    }

    /// Only a `current` `high` or `medium` link on a new error, its addon still at the
    /// release the change installed, qualifies: every combination of the four.
    #[test]
    fn a_link_qualifies_only_when_strong_current_new_and_still_installed() {
        use Confidence::{High, Low, Medium};
        use LinkState::{Current, DatabaseReplaced, Expired};
        for confidence in [High, Medium, Low] {
            for state in [Current, Expired, DatabaseReplaced] {
                for new in [false, true] {
                    for still in [false, true] {
                        let want = new && state == Current && confidence != Low && still;
                        assert_eq!(
                            qualifies(&link(confidence, state, still), new),
                            want,
                            "{confidence:?} {state:?} new={new} still={still}"
                        );
                    }
                }
            }
        }
        let mut none = link(High, Current, true);
        none.changed.clear();
        assert!(!qualifies(&none, true), "a link that names no addon");
    }

    /// Design 7.4.1's table over every combination of its inputs.
    #[test]
    fn the_ways_back_follow_the_table() {
        use Restorable::{Cached, Fetchable, No};
        use Step::{Rollback, Undo, Update};
        use TransitionChange::{Installed, Removed, Reverted, Updated};
        for newer in [false, true] {
            for change in [Updated, Installed, Reverted, Removed] {
                for restorable in [Cached, Fetchable, No] {
                    for undoable in [false, true] {
                        let got = decide(Facts {
                            newer,
                            change,
                            restorable,
                            undoable,
                        });
                        let mut want = Vec::new();
                        if newer {
                            want.push(Update);
                        }
                        let can_restore = restorable != No;
                        match change {
                            Updated if can_restore => want.push(Rollback),
                            Updated | Installed | Reverted if undoable => want.push(Undo),
                            _ => {}
                        }
                        assert_eq!(got, want, "{newer} {change:?} {restorable:?} {undoable}");
                        // Update first, then at most one of the others.
                        assert!(got.iter().skip(1).all(|s| *s != Update));
                        assert!(got.iter().filter(|s| **s != Update).count() <= 1);
                    }
                }
            }
        }
        assert_eq!(
            decide(Facts {
                newer: false,
                change: Installed,
                restorable: Cached,
                undoable: false,
            }),
            [],
            "a first install has no files before it, however its archive is cached"
        );
    }
}
