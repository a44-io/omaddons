//! Barriers (M4-D38, M4-D31): a doctor repair that moves other journals' artifacts owns a
//! barrier over them. A set-aside **suspends** a stuck journal set, renaming its journal
//! file out of `<state>/journal/`; the undo of a set-aside **reactivates** it, its last
//! primitive (the activation) renaming the journal from its provisional name back to its
//! own. While such a repair is unsettled, the journals it names must not be recovered
//! ahead of it, and it protects their targets' records too (E14).
//!
//! This module classifies a `prepared` line ([`of`]), holds the activation rules every
//! reactivation follows ([`validate`]), and plans the order recovery takes repairs in
//! ([`Passes`]). It never writes.

use std::collections::BTreeSet;
use std::path::Path;

use super::repair::{JournalName, PreparedRead, RepairPhase, RepairPrepared, journal_file};
use super::repair_action::Primitive;
use super::resource::ResourceSet;
use crate::fsx::{FenceStop, canonical_lenient};
use crate::model::TxId;
use crate::state::paths::{ClientPaths, Paths, tx_named};

/// Which barrier a repair owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierKind {
    /// It moves a journal file out of `<state>/journal/` under its own name.
    Suspension,
    /// It renames a journal from its provisional name back to its own.
    Activation,
}

/// A repair's barrier, and the transactions behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Barrier {
    pub kind: BarrierKind,
    /// Every transaction whose journal or artifact the repair moves, never its own.
    pub names: BTreeSet<TxId>,
}

/// The barrier `prepared` owns, if any. An activation (any primitive with
/// `activation: true`) wins over a suspension (a `rename_noreplace` whose source is a
/// folder, settings or repair journal in `<state>/journal/` under its own name). Guards
/// are not required for a suspension: one of a journal with no readable target may carry
/// none, and it is a barrier all the same.
pub fn of(prepared: &RepairPrepared, paths: &Paths) -> Option<Barrier> {
    let renames: Vec<(&Path, &Path, bool)> = prepared
        .primitives
        .iter()
        .filter_map(|step| match &step.op {
            Primitive::RenameNoReplace {
                from,
                to,
                activation,
            } => Some((from.as_path(), to.as_path(), *activation)),
            _ => None,
        })
        .collect();
    let kind = if renames.iter().any(|(_, _, activation)| *activation) {
        BarrierKind::Activation
    } else if renames.iter().any(|(from, _, _)| {
        matches!(
            journal_file(paths, from),
            Some((
                _,
                JournalName::Folder | JournalName::Wtf | JournalName::Repair
            ))
        )
    }) {
        BarrierKind::Suspension
    } else {
        return None;
    };
    let names = renames
        .iter()
        .flat_map(|(from, to, _)| [*from, *to])
        .filter_map(|path| moved(prepared, paths, path))
        .filter(|tx| *tx != prepared.tx)
        .collect();
    Some(Barrier { kind, names })
}

/// The transaction `path` is an artifact of, when it lies where omaddons keeps one: a
/// journal file under any of its four names; `<state>/inflight/<tx>.lock`;
/// `<state>/snapshots/<tx>` or `<tx>.wtf`; and, for each client the repair names, its
/// `stage/` and `snapshots/` entries named for a transaction.
fn moved(prepared: &RepairPrepared, paths: &Paths, path: &Path) -> Option<TxId> {
    if let Some((tx, _)) = journal_file(paths, path) {
        return Some(tx);
    }
    let name = path.file_name()?.to_str()?;
    let parent = path.parent()?;
    if in_dir(parent, &paths.inflight_dir()) {
        let id = name.strip_suffix(".lock")?;
        return id.parse::<TxId>().ok().filter(|tx| tx.to_string() == id);
    }
    let client_dirs = prepared.client_dirs.iter().flat_map(|client| {
        let meta = ClientPaths::new(&client.dir).meta_dir();
        [meta.join("stage"), meta.join("snapshots")]
    });
    let mut artifact_dirs = std::iter::once(paths.state_snapshots_dir()).chain(client_dirs);
    artifact_dirs
        .any(|dir| in_dir(parent, &dir))
        .then(|| tx_named(name))
        .flatten()
}

/// Whether `parent` is `dir`, as written or canonical (a lowered path is canonical; the
/// state directory may be reached through a link).
fn in_dir(parent: &Path, dir: &Path) -> bool {
    // ubs:ignore — comparing paths, not secrets
    parent == dir || canonical_lenient(dir).as_deref() == Some(parent)
}

// ---------------------------------------------------------------------------------------
// The activation rules (M4-D38, M4-D46).

/// One `rename_noreplace` of a repair: its step, source, destination and flag.
struct Rename<'p> {
    seq: u32,
    from: &'p Path,
    to: &'p Path,
    activation: bool,
}

/// Whether `path` is a journal in `<state>/journal/` under its own name (a folder,
/// settings or repair journal's), and whose.
fn own_name(paths: &Paths, path: &Path) -> Option<TxId> {
    match journal_file(paths, path)? {
        (tx, JournalName::Folder | JournalName::Wtf | JournalName::Repair) => Some(tx),
        (_, JournalName::Reactivating) => None,
    }
}

/// Whether `path` is the provisional name of `tx`'s journal.
fn provisional_of(paths: &Paths, path: &Path, tx: TxId) -> bool {
    journal_file(paths, path) == Some((tx, JournalName::Reactivating))
}

/// The rules a repair that reactivates a journal follows, so that a crash anywhere in it
/// never lets recovery run the restored journal ahead of the undo that exposed it. Each
/// violation is a message naming its rule and the step:
///
/// 1. at most one primitive is an activation, and it is the repair's last;
/// 2. it renames `tx`'s provisional journal (`<tx>.reactivating.jsonl`) to that journal's
///    own name in `<state>/journal/` (`<tx>.jsonl`, `<tx>.wtf.jsonl` or
///    `<tx>.repair.jsonl`);
/// 3. exactly one earlier primitive renames into that provisional name, and it is the
///    first primitive that names `tx` (as [`of`] names a transaction): the journal comes
///    back before any of its artifacts, so the orphan sweep never sees them without it;
/// 4. no other primitive renames out of that provisional name, and no primitive but an
///    activation moves a journal into `<state>/journal/` under its own name.
///
/// A repair with no activation is held to rule 4's second half only. A rollback's and
/// recovery's inverses are no primitives, and are not held to these.
pub fn validate(prepared: &RepairPrepared, paths: &Paths) -> Result<(), String> {
    let renames: Vec<Rename<'_>> = prepared
        .primitives
        .iter()
        .filter_map(|step| match &step.op {
            Primitive::RenameNoReplace {
                from,
                to,
                activation,
            } => Some(Rename {
                seq: step.seq,
                from,
                to,
                activation: *activation,
            }),
            _ => None,
        })
        .collect();
    let activations: Vec<&Rename<'_>> = renames.iter().filter(|r| r.activation).collect();
    if let [first, second, ..] = activations[..] {
        return Err(format!(
            "rule 1: steps {} and {} both activate a journal, and a repair activates at most one",
            first.seq, second.seq
        ));
    }
    if let Some(activation) = activations.first() {
        activated(prepared, paths, &renames, activation)?;
    }
    for rename in renames.iter().filter(|r| !r.activation) {
        if own_name(paths, rename.to).is_some() {
            return Err(format!(
                "rule 4: step {} moves {} into the journal directory under a journal's own name, which only an activation may",
                rename.seq,
                rename.to.display()
            ));
        }
    }
    Ok(())
}

/// Rules 1 to 4 for the one `activation` of `prepared`.
fn activated(
    prepared: &RepairPrepared,
    paths: &Paths,
    renames: &[Rename<'_>],
    activation: &Rename<'_>,
) -> Result<(), String> {
    let seq = activation.seq;
    let last = prepared.primitives.last().map_or(0, |step| step.seq);
    if seq != last {
        return Err(format!(
            "rule 1: step {seq} activates a journal, but the repair's last primitive is step {last}"
        ));
    }
    let Some((tx, JournalName::Reactivating)) = journal_file(paths, activation.from) else {
        return Err(format!(
            "rule 2: step {seq} activates {}, which is not a journal's provisional name",
            activation.from.display()
        ));
    };
    if own_name(paths, activation.to) != Some(tx) {
        return Err(format!(
            "rule 2: step {seq} activates {}, which is not the own name of journal {tx}",
            activation.to.display()
        ));
    }
    let others = || renames.iter().filter(|r| !r.activation);
    let entries: Vec<u32> = others()
        .filter(|r| provisional_of(paths, r.to, tx))
        .map(|r| r.seq)
        .collect();
    let entry = match entries[..] {
        [entry] => entry,
        [] => {
            return Err(format!(
                "rule 3: no step renames journal {tx} into its provisional name before step {seq} activates it"
            ));
        }
        [first, second, ..] => {
            return Err(format!(
                "rule 3: steps {first} and {second} both rename journal {tx} into its provisional name, and exactly one may"
            ));
        }
    };
    let names = |r: &&Rename<'_>| {
        [r.from, r.to]
            .into_iter()
            .any(|path| moved(prepared, paths, path) == Some(tx))
    };
    if let Some(first) = renames.iter().find(names)
        && first.seq != entry
    {
        return Err(format!(
            "rule 3: step {} names journal {tx} before step {entry} moves it to its provisional name, which must come first",
            first.seq
        ));
    }
    if let Some(out) = others().find(|r| provisional_of(paths, r.from, tx)) {
        return Err(format!(
            "rule 4: step {} renames journal {tx} out of its provisional name, which only the activation may",
            out.seq
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// The passes (M4-D31): the order recovery takes journals in.

/// How recovery left one repair, as [`Passes`] records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Rolled back, rolled forward or pruned.
    Settled,
    /// Recovery stopped at it, everything left in place.
    Unsettled(Stuck),
    /// Its writer holds its in-flight lock, so recovery skipped it. `finished`: its
    /// journal's last line is `committed` or `rolled_back`, the only live state a
    /// recovery under the global lock can meet (a writer holds the lock for its whole
    /// commit window); an unfinished one is seen only by a lock-free `diagnose`.
    Live { finished: bool },
    /// Never recovered: it waits behind this unsettled repair, or (in pass 3) this
    /// journal pass 2 left unresolved.
    Behind(TxId),
}

/// A repair recovery could not settle: its stop, and whether its resources cannot be
/// read (its fences close).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stuck {
    pub stop: FenceStop,
    pub closed: bool,
}

/// Whether a live repair is finished, by its journal's phase: its last complete line is
/// `committed` or `rolled_back`.
pub fn finished(phase: RepairPhase) -> bool {
    matches!(phase, RepairPhase::Committed | RepairPhase::RolledBack)
}

/// One listed repair journal, as [`Passes`] plans with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedRepair {
    pub tx: TxId,
    /// Its first line, read through the activation rules
    /// ([`ParsedRepair::checked`](super::repair::ParsedRepair::checked)); a journal that
    /// cannot be read at all is `Undecodable`.
    pub prepared: PreparedRead,
    /// Its writer holds its in-flight lock.
    pub live: bool,
}

/// What a listed repair is to the order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Role {
    /// It owns a barrier over these transactions ([`of`]).
    Owner(BTreeSet<TxId>),
    /// Its `prepared` line cannot be read (it does not decode, declares a newer major or
    /// breaks the activation rules): it might be a reactivation that has already exposed
    /// a journal, so it owns a barrier over everything (M3-D45: fail closed).
    Unknowable,
    /// Neither: pass 3 takes it.
    Rest,
}

/// The one decision procedure for the order recovery takes journals in, which recovery
/// drives with the disk and `journal::diagnose` with its simulated tree (a decision
/// written twice drifts):
///
/// 1. **Pass 1**, [`Passes::next_owner`]: the repairs that own a barrier (a suspension or
///    an activation), and every repair whose `prepared` line cannot be read, which owns a
///    barrier over everything. Those come first, in transaction order; then the owners,
///    one named by another after the one naming it, ties in transaction order. A cycle
///    (which M4 code cannot build) leaves each of its owners unsettled.
/// 2. **Pass 2**, the folder journals, listed after pass 1: a journal [`Passes::behind`]
///    an unsettled owner is never recovered.
/// 3. **Pass 3**, [`Passes::rest`]: the other repairs, re-listed after pass 2, in
///    transaction order; one behind an owner, or whose resources overlap a journal pass 2
///    left unresolved ([`Passes::waits_for`]), is never recovered.
#[derive(Debug, Clone)]
pub struct Passes {
    roles: std::collections::BTreeMap<TxId, Role>,
    /// Pass 1's order.
    order: Vec<TxId>,
    /// How many of `order` [`Passes::next_owner`] has handed out.
    next: usize,
    outcomes: std::collections::BTreeMap<TxId, Outcome>,
    /// The owners a cycle leaves unsettled.
    cycle: BTreeSet<TxId>,
}

impl Passes {
    pub fn new(repairs: Vec<ListedRepair>, paths: &Paths) -> Self {
        let roles: std::collections::BTreeMap<TxId, Role> = repairs
            .into_iter()
            .map(|listed| {
                let role = match &listed.prepared {
                    PreparedRead::Decoded(prepared) => {
                        of(prepared, paths).map_or(Role::Rest, |barrier| Role::Owner(barrier.names))
                    }
                    PreparedRead::Undecodable | PreparedRead::TooNew => Role::Unknowable,
                    PreparedRead::Absent => Role::Rest,
                };
                (listed.tx, role)
            })
            .collect();
        let mut order: Vec<TxId> = roles
            .iter()
            .filter(|(_, role)| **role == Role::Unknowable)
            .map(|(tx, _)| *tx)
            .collect();
        // Kahn's order over the owners: an owner is ready once no unplaced owner names it.
        let owners: std::collections::BTreeMap<TxId, &BTreeSet<TxId>> = roles
            .iter()
            .filter_map(|(tx, role)| match role {
                Role::Owner(names) => Some((*tx, names)),
                _ => None,
            })
            .collect();
        let mut unplaced: BTreeSet<TxId> = owners.keys().copied().collect();
        while let Some(ready) = unplaced
            .iter()
            .find(|tx| {
                !unplaced
                    .iter()
                    .any(|namer| namer != *tx && owners[namer].contains(tx))
            })
            .copied()
        {
            unplaced.remove(&ready);
            order.push(ready);
        }
        // What is left names itself round: each is unsettled, and waits behind another.
        let mut outcomes = std::collections::BTreeMap::new();
        for tx in &unplaced {
            order.push(*tx);
            outcomes.insert(
                *tx,
                Outcome::Unsettled(Stuck {
                    stop: super::fence::repair_stop(
                        *tx,
                        crate::error::ErrorCode::StateCorrupt,
                        Some(super::journal::StopReason::JournalUnreadable),
                        false,
                    ),
                    closed: false,
                }),
            );
        }
        Self {
            roles,
            order,
            next: 0,
            outcomes,
            cycle: unplaced,
        }
    }

    /// Pass 1: the next owner to recover, or `None` when pass 1 is over. The caller first
    /// asks [`Passes::behind`] whether it may, then [`Passes::record`]s what happened.
    pub fn next_owner(&mut self) -> Option<TxId> {
        let tx = self.order.get(self.next).copied()?;
        self.next += 1;
        Some(tx)
    }

    /// Records what happened to `tx`. An owner a cycle leaves unsettled stays so, whatever
    /// the caller saw of it (it waits behind another owner of the cycle), so what it names
    /// is held back with a stop.
    pub fn record(&mut self, tx: TxId, outcome: Outcome) {
        if self.cycle.contains(&tx) {
            return;
        }
        self.outcomes.insert(tx, outcome);
    }

    pub fn outcome(&self, tx: &TxId) -> Option<&Outcome> {
        self.outcomes.get(tx)
    }

    /// The repair `tx` waits behind, if any: the first in pass 1's order that names it
    /// (one whose `prepared` line cannot be read names everything) and that holds it
    /// back, because recovery could not settle it, it waits itself, or its writer is
    /// alive and unfinished. A repair whose `prepared` line cannot be read waits behind
    /// nothing: recovering it only stops, and writes nothing.
    pub fn behind(&self, tx: &TxId) -> Option<TxId> {
        if self.roles.get(tx) == Some(&Role::Unknowable) {
            return None;
        }
        self.order
            .iter()
            .filter(|owner| *owner != tx)
            .find(|owner| {
                let names = match &self.roles[*owner] {
                    Role::Unknowable => true,
                    Role::Owner(names) => names.contains(tx),
                    Role::Rest => false,
                };
                names && self.holds(owner)
            })
            .copied()
    }

    /// Whether what is behind `tx` waits for it.
    fn holds(&self, tx: &TxId) -> bool {
        matches!(
            self.outcomes.get(tx),
            Some(Outcome::Unsettled(_) | Outcome::Behind(_) | Outcome::Live { finished: false })
        )
    }

    /// Pass 3: the repairs of `relisted` (listed again after pass 2) that pass 1 did not
    /// take, in transaction order.
    pub fn rest(&self, relisted: &[TxId]) -> Vec<TxId> {
        let mut rest: Vec<TxId> = relisted
            .iter()
            .filter(|tx| !self.order.contains(tx))
            .copied()
            .collect();
        rest.sort_unstable();
        rest.dedup();
        rest
    }

    /// Pass 3's overlap rule: the first journal of `unresolved` (one pass 2 stopped at or
    /// left behind an owner) whose protected set conflicts with `resources`, a repair's
    /// own. A repair waiting behind it is never recovered ahead of it.
    pub fn waits_for(
        &self,
        resources: &ResourceSet,
        unresolved: &[(TxId, ResourceSet)],
    ) -> Option<TxId> {
        unresolved
            .iter()
            .find(|(_, protected)| protected.conflicts_with(resources))
            .map(|(tx, _)| *tx)
    }

    /// Where `tx` is stuck: its own stop, or the stop of the repair it waits behind,
    /// followed to the one recovery could not settle. `None` when it settled, or when it
    /// waits behind a live writer or a journal the planner does not hold.
    pub fn stop(&self, tx: &TxId) -> Option<&Stuck> {
        let mut at = *tx;
        for _ in 0..=self.order.len() {
            match self.outcomes.get(&at)? {
                Outcome::Unsettled(stuck) => return Some(stuck),
                Outcome::Behind(owner) => at = *owner,
                Outcome::Settled | Outcome::Live { .. } => return None,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::path::PathBuf;

    use super::*;
    use crate::engine::repair::{PlannedFix, provisional_journal};
    use crate::engine::repair_action::{
        Attribution, Fix, Guard, JournalFamily, JournalSet, Mode, QuarantineRoot, Stamp,
        SuspendedArtifact,
    };
    use crate::engine::testing::{RepairHome, repair_home, repair_prepared};

    const PENDING: &str = "fm-concurrency_primitives-journal-pending";

    fn planned(fix: Fix) -> PlannedFix {
        PlannedFix {
            fm: Cow::Borrowed(PENDING),
            fix,
        }
    }

    /// The set-aside of the stuck journal `repair_home` leaves, with `artifacts` as
    /// `(path, root)` and `guards`.
    fn suspension(
        home: &RepairHome,
        family: JournalFamily,
        artifacts: Vec<(std::path::PathBuf, QuarantineRoot)>,
        guards: Vec<Guard>,
    ) -> RepairPrepared {
        let journal = JournalSet {
            tx: home.stuck(),
            family,
            target: Some(home.key),
            artifacts: artifacts
                .into_iter()
                .map(|(path, root)| SuspendedArtifact { path, root })
                .collect(),
            guards,
            attribution: Attribution::Complete,
            unattributed: Vec::new(),
        };
        repair_prepared(home, &[], &[planned(Fix::Suspend { journal })])
    }

    fn client_root(home: &RepairHome) -> QuarantineRoot {
        QuarantineRoot::Client {
            client_dir: home.client.client_dir().to_path_buf(),
        }
    }

    /// Moves the stuck folder journal aside under the name `name`, so a suspension of
    /// that kind of journal can be lowered.
    fn plant_journal(home: &RepairHome, name: &str) -> std::path::PathBuf {
        let stuck = home.stuck();
        let to = home.paths.journal_dir().join(name);
        std::fs::rename(home.paths.journal(&stuck), &to).unwrap();
        to
    }

    #[test]
    fn of_classifies_suspensions_and_activations() {
        type Case = (
            &'static str,
            fn(&RepairHome) -> RepairPrepared,
            Option<BarrierKind>,
        );
        let cases: Vec<Case> = vec![
            (
                "suspension_of_folder_journal",
                |home| {
                    let stuck = home.stuck();
                    suspension(
                        home,
                        JournalFamily::Folder,
                        vec![
                            (home.paths.journal(&stuck), QuarantineRoot::State),
                            (home.paths.inflight_lock(&stuck), QuarantineRoot::State),
                        ],
                        vec![Guard::Generation {
                            target: home.key,
                            generation: 7,
                        }],
                    )
                },
                Some(BarrierKind::Suspension),
            ),
            (
                "suspension_of_wtf_journal",
                |home| {
                    let stuck = home.stuck();
                    let journal = plant_journal(home, &format!("{stuck}.wtf.jsonl"));
                    suspension(
                        home,
                        JournalFamily::Folder,
                        vec![(journal, QuarantineRoot::State)],
                        Vec::new(),
                    )
                },
                Some(BarrierKind::Suspension),
            ),
            (
                "suspension_of_repair_journal",
                |home| {
                    let stuck = home.stuck();
                    let journal = plant_journal(home, &format!("{stuck}.repair.jsonl"));
                    suspension(
                        home,
                        JournalFamily::Repair,
                        vec![(journal, QuarantineRoot::State)],
                        Vec::new(),
                    )
                },
                Some(BarrierKind::Suspension),
            ),
            (
                "suspension_without_guards",
                |home| {
                    let stuck = home.stuck();
                    suspension(
                        home,
                        JournalFamily::Folder,
                        vec![(home.paths.journal(&stuck), QuarantineRoot::State)],
                        Vec::new(),
                    )
                },
                Some(BarrierKind::Suspension),
            ),
            (
                "activation",
                |home| {
                    let stuck = home.stuck();
                    let mut p = suspension(
                        home,
                        JournalFamily::Folder,
                        vec![(home.paths.journal(&stuck), QuarantineRoot::State)],
                        Vec::new(),
                    );
                    for step in &mut p.primitives {
                        if let Primitive::RenameNoReplace { activation, .. } = &mut step.op {
                            *activation = true;
                        }
                    }
                    p
                },
                Some(BarrierKind::Activation),
            ),
            (
                "no_barrier_for_a_chmod",
                |home| {
                    repair_prepared(
                        home,
                        &[],
                        &[planned(Fix::Chmod {
                            path: home.credential(),
                            mode: Mode::new(0o600).unwrap(),
                        })],
                    )
                },
                None,
            ),
            (
                "no_barrier_for_a_quarantine",
                |home| {
                    repair_prepared(
                        home,
                        &[],
                        &[planned(Fix::Quarantine {
                            path: home.addons("Stray"),
                            root: client_root(home),
                        })],
                    )
                },
                None,
            ),
        ];
        for (case, build, want) in cases {
            let home = repair_home();
            let p = build(&home);
            let got = of(&p, &home.paths);
            assert_eq!(
                got.as_ref().map(|b| b.kind),
                want,
                "{case}: {:#?}\n{got:?}",
                p.primitives
            );
            if want.is_some() {
                assert_eq!(
                    got.map(|b| b.names),
                    Some(BTreeSet::from([home.stuck()])),
                    "{case}: the suspended journal is named"
                );
            }
        }
    }

    #[test]
    fn names_every_artifact_kind() {
        let home = repair_home();
        let stuck = home.stuck();
        for path in [
            home.paths.state_snapshot(&stuck),
            home.client.snapshots(&stuck),
            home.client.snapshots_wtf(&stuck),
            home.client.stage_wtf(&stuck),
        ] {
            std::fs::create_dir_all(&path).unwrap();
        }
        let p = suspension(
            &home,
            JournalFamily::Folder,
            vec![
                (home.paths.journal(&stuck), QuarantineRoot::State),
                (home.paths.inflight_lock(&stuck), QuarantineRoot::State),
                (home.paths.state_snapshot(&stuck), QuarantineRoot::State),
                (home.client.stage(&stuck), client_root(&home)),
                (home.client.stage_wtf(&stuck), client_root(&home)),
                (home.client.snapshots(&stuck), client_root(&home)),
                (home.client.snapshots_wtf(&stuck), client_root(&home)),
            ],
            vec![Guard::Generation {
                target: home.key,
                generation: 7,
            }],
        );
        for step in &p.primitives {
            let Primitive::RenameNoReplace { from, .. } = &step.op else {
                panic!("{step:?}"); // ubs:ignore — fails the test
            };
            assert_eq!(
                moved(&p, &home.paths, from),
                Some(stuck),
                "{}",
                from.display()
            );
        }
        let barrier = of(&p, &home.paths).unwrap();
        assert_eq!(barrier.kind, BarrierKind::Suspension);
        assert_eq!(barrier.names, BTreeSet::from([stuck]));
    }

    #[test]
    fn never_names_its_own_tx() {
        let home = repair_home();
        let stuck = home.stuck();
        let mut p = suspension(
            &home,
            JournalFamily::Folder,
            vec![(home.paths.journal(&stuck), QuarantineRoot::State)],
            vec![Guard::Epoch {
                path: home.client.stage(&stuck),
                stamp: Stamp([1; 32]),
            }],
        );
        // The same repair also moves its own in-flight lock: never a name of its own.
        let mut own = p.primitives[0].clone();
        own.seq = 2;
        own.op = Primitive::RenameNoReplace {
            from: home.paths.inflight_lock(&p.tx),
            to: home.run.dir.join("quarantine/state/inflight.lock"),
            activation: false,
        };
        p.primitives.push(own);
        let barrier = of(&p, &home.paths).unwrap();
        assert!(!barrier.names.contains(&p.tx), "{:?}", barrier.names);
        assert_eq!(barrier.names, BTreeSet::from([stuck]));
    }

    // -----------------------------------------------------------------------------------
    // The activation rules.

    /// The set-aside of the stuck folder journal: its journal, in-flight lock and client
    /// stage moved into the run's quarantine.
    fn set_aside(home: &RepairHome) -> RepairPrepared {
        let stuck = home.stuck();
        suspension(
            home,
            JournalFamily::Folder,
            vec![
                (home.paths.journal(&stuck), QuarantineRoot::State),
                (home.paths.inflight_lock(&stuck), QuarantineRoot::State),
                (home.client.stage(&stuck), client_root(home)),
            ],
            Vec::new(),
        )
    }

    /// The renames of `p`, as `(from, to, activation)`.
    fn renames(p: &RepairPrepared) -> Vec<(PathBuf, PathBuf, bool)> {
        p.primitives
            .iter()
            .map(|step| match &step.op {
                Primitive::RenameNoReplace {
                    from,
                    to,
                    activation,
                } => (from.clone(), to.clone(), *activation),
                op => panic!("{op:?}"), // ubs:ignore — fails the test
            })
            .collect()
    }

    /// `p` with these renames as its primitives, numbered from 1.
    fn with_renames(
        mut p: RepairPrepared,
        renames: Vec<(PathBuf, PathBuf, bool)>,
    ) -> RepairPrepared {
        let template = p.primitives[0].clone();
        p.primitives = renames
            .into_iter()
            .zip(1..)
            .map(|((from, to, activation), seq)| {
                let mut step = template.clone();
                step.seq = seq;
                step.guards = Vec::new();
                step.op = Primitive::RenameNoReplace {
                    from,
                    to,
                    activation,
                };
                step
            })
            .collect();
        p
    }

    /// The undo of [`set_aside`] as M4-D38 orders it: the journal back under its
    /// provisional name first, then every other artifact, then the activation.
    fn reactivation(home: &RepairHome) -> RepairPrepared {
        let stuck = home.stuck();
        let journal = home.paths.journal(&stuck);
        let provisional = provisional_journal(&home.paths, &stuck);
        let set_aside = set_aside(home);
        let moved = renames(&set_aside);
        let quarantined = |orig: &Path| {
            moved
                .iter()
                .find(|(from, _, _)| from == orig)
                .map(|(_, to, _)| to.clone())
                .unwrap()
        };
        let mut undo = vec![(quarantined(&journal), provisional.clone(), false)];
        undo.extend(
            moved
                .iter()
                .filter(|(from, _, _)| *from != journal)
                .map(|(from, to, _)| (to.clone(), from.clone(), false)),
        );
        undo.push((provisional, journal, true));
        with_renames(set_aside, undo)
    }

    #[test]
    fn validate_refuses_each_malformed_activation() {
        type Case = (
            &'static str,
            fn(&RepairHome) -> RepairPrepared,
            Option<&'static str>,
        );
        let cases: Vec<Case> = vec![
            ("well_formed_reactivation", reactivation, None),
            ("well_formed_suspension", set_aside, None),
            (
                "two_activations",
                |home| {
                    let mut r = renames(&reactivation(home));
                    r[1].2 = true;
                    with_renames(reactivation(home), r)
                },
                Some("rule 1: steps 2 and 4"),
            ),
            (
                "activation_not_last",
                |home| {
                    let mut r = renames(&reactivation(home));
                    let n = r.len();
                    r.swap(n - 2, n - 1);
                    with_renames(reactivation(home), r)
                },
                Some("rule 1: step 3 activates"),
            ),
            (
                "activation_not_from_provisional",
                |home| {
                    let mut r = renames(&reactivation(home));
                    let last = r.len() - 1;
                    r[last].0 = home.run.dir.join("quarantine/state/journal.jsonl");
                    with_renames(reactivation(home), r)
                },
                Some("rule 2: step 4 activates"),
            ),
            (
                "activation_to_another_tx",
                |home| {
                    let mut r = renames(&reactivation(home));
                    let last = r.len() - 1;
                    r[last].1 = home.paths.journal(&TxId::new());
                    with_renames(reactivation(home), r)
                },
                Some("rule 2: step 4 activates"),
            ),
            (
                "provisional_not_first",
                |home| {
                    let mut r = renames(&reactivation(home));
                    r.swap(0, 1);
                    with_renames(reactivation(home), r)
                },
                Some("rule 3: step 1 names journal"),
            ),
            (
                "no_provisional_entry",
                |home| {
                    let stuck = home.stuck();
                    let mut r = renames(&reactivation(home));
                    r[0].1 = home.run.dir.join("elsewhere.jsonl");
                    r[0].0 = home.paths.inflight_lock(&stuck);
                    with_renames(reactivation(home), r)
                },
                Some("rule 3: no step renames journal"),
            ),
            (
                "two_provisional_entries",
                |home| {
                    let mut r = renames(&reactivation(home));
                    let again = (home.run.dir.join("copy.jsonl"), r[0].1.clone(), false);
                    r.insert(1, again);
                    with_renames(reactivation(home), r)
                },
                Some("rule 3: steps 1 and 2 both rename"),
            ),
            (
                "renamed_out_of_provisional",
                |home| {
                    let mut r = renames(&reactivation(home));
                    let out = (r[0].1.clone(), home.run.dir.join("away.jsonl"), false);
                    r.insert(1, out);
                    with_renames(reactivation(home), r)
                },
                Some("rule 4: step 2 renames journal"),
            ),
            (
                "journal_moved_in_without_activation",
                |home| {
                    let mut r = renames(&reactivation(home));
                    let last = r.len() - 1;
                    r[last].2 = false;
                    with_renames(reactivation(home), r)
                },
                Some("rule 4: step 4 moves"),
            ),
        ];
        for (case, build, want) in cases {
            let home = repair_home();
            let p = build(&home);
            let got = validate(&p, &home.paths);
            match want {
                None => assert!(got.is_ok(), "{case}: {got:?}\n{:#?}", renames(&p)),
                Some(rule) => assert!(
                    got.as_ref().is_err_and(|message| message.starts_with(rule)),
                    "{case}: want {rule}…, got {got:?}\n{:#?}",
                    renames(&p)
                ),
            }
        }
    }

    // -----------------------------------------------------------------------------------
    // The passes.

    fn id(n: u8) -> TxId {
        format!("01JB8Q2VA000000000000000{n:02}").parse().unwrap()
    }

    /// A journal the planner is asked about: a folder journal or a repair's, by its id.
    #[derive(Clone, Copy)]
    enum Names {
        Folder(u8),
        Repair(u8),
    }

    /// Repair `n`, suspending each journal of `names` (none: a chmod, which owns no
    /// barrier).
    fn repair(home: &RepairHome, n: u8, names: &[Names]) -> ListedRepair {
        let mut p = if names.is_empty() {
            repair_prepared(
                home,
                &[],
                &[planned(Fix::Chmod {
                    path: home.credential(),
                    mode: Mode::new(0o600).unwrap(),
                })],
            )
        } else {
            let moves = names
                .iter()
                .enumerate()
                .map(|(i, named)| {
                    let from = match *named {
                        Names::Folder(m) => home.paths.journal(&id(m)),
                        Names::Repair(m) => home.paths.journal_repair(&id(m)),
                    };
                    (
                        from,
                        home.run.dir.join(format!("quarantine/{n}-{i}")),
                        false,
                    )
                })
                .collect();
            with_renames(set_aside(home), moves)
        };
        p.tx = id(n);
        ListedRepair {
            tx: id(n),
            prepared: PreparedRead::Decoded(Box::new(p)),
            live: false,
        }
    }

    fn unknowable(n: u8) -> ListedRepair {
        ListedRepair {
            tx: id(n),
            prepared: PreparedRead::Undecodable,
            live: false,
        }
    }

    fn stuck(n: u8) -> Outcome {
        Outcome::Unsettled(Stuck {
            stop: crate::engine::fence::repair_stop(
                id(n),
                crate::error::ErrorCode::Internal,
                Some(crate::engine::journal::StopReason::StepUnrecognized),
                false,
            ),
            closed: false,
        })
    }

    /// Hands out every pass-1 owner, recording each as `outcome` gives it.
    fn drain(passes: &mut Passes, outcome: impl Fn(TxId) -> Outcome) -> Vec<TxId> {
        let mut order = Vec::new();
        while let Some(owner) = passes.next_owner() {
            order.push(owner);
            passes.record(owner, outcome(owner));
        }
        order
    }

    #[test]
    fn passes_order_owners_by_naming() {
        type Case = (&'static str, fn(&RepairHome) -> Vec<ListedRepair>, Vec<u8>);
        let cases: Vec<Case> = vec![
            (
                "one_owner",
                |home| vec![repair(home, 1, &[Names::Folder(9)]), repair(home, 2, &[])],
                vec![1],
            ),
            (
                "chain_of_two",
                |home| {
                    vec![
                        repair(home, 1, &[Names::Folder(9)]),
                        repair(home, 2, &[Names::Repair(1)]),
                    ]
                },
                vec![2, 1],
            ),
            (
                "tie_in_tx_order",
                |home| {
                    vec![
                        repair(home, 2, &[Names::Folder(8)]),
                        repair(home, 1, &[Names::Folder(9)]),
                    ]
                },
                vec![1, 2],
            ),
            (
                "unknowable_first",
                |home| vec![repair(home, 1, &[Names::Folder(9)]), unknowable(5)],
                vec![5, 1],
            ),
            (
                "planted_cycle_is_unsettled",
                |home| {
                    vec![
                        repair(home, 1, &[Names::Repair(2)]),
                        repair(home, 2, &[Names::Repair(1)]),
                    ]
                },
                vec![1, 2],
            ),
        ];
        for (case, build, want) in cases {
            let home = repair_home();
            let listed = build(&home);
            let mut passes = Passes::new(listed.clone(), &home.paths);
            let mut order = Vec::new();
            while let Some(owner) = passes.next_owner() {
                order.push(owner);
            }
            let want: Vec<TxId> = want.into_iter().map(id).collect();
            assert_eq!(order, want, "{case}: pass 1's order");
            let rest: Vec<TxId> = listed.iter().map(|l| l.tx).collect();
            assert_eq!(
                passes.rest(&rest),
                rest.iter()
                    .filter(|tx| !want.contains(tx))
                    .copied()
                    .collect::<Vec<_>>(),
                "{case}: pass 3 takes the others"
            );
            if case == "planted_cycle_is_unsettled" {
                for (tx, other) in [(1, 2), (2, 1)] {
                    assert!(
                        matches!(passes.outcome(&id(tx)), Some(Outcome::Unsettled(_))),
                        "{case}: {tx} is unsettled"
                    );
                    assert_eq!(passes.behind(&id(tx)), Some(id(other)), "{case}: {tx}");
                    // Recorded as waiting, it stays unsettled: what it names keeps a stop.
                    passes.record(id(tx), Outcome::Behind(id(other)));
                    assert!(
                        matches!(passes.outcome(&id(tx)), Some(Outcome::Unsettled(_))),
                        "{case}: {tx} stays unsettled"
                    );
                    assert!(passes.stop(&id(tx)).is_some(), "{case}: {tx}");
                }
            }
        }
    }

    #[test]
    fn passes_put_named_journals_behind_an_unsettled_owner() {
        let home = repair_home();
        let mut passes = Passes::new(
            vec![
                repair(&home, 1, &[Names::Folder(9)]),
                repair(&home, 2, &[Names::Repair(1)]),
                repair(&home, 3, &[]),
            ],
            &home.paths,
        );
        assert_eq!(passes.next_owner(), Some(id(2)));
        assert_eq!(passes.behind(&id(2)), None, "nothing names the first owner");
        passes.record(id(2), stuck(2));
        assert_eq!(passes.next_owner(), Some(id(1)));
        assert_eq!(passes.behind(&id(1)), Some(id(2)), "the named owner waits");
        passes.record(id(1), Outcome::Behind(id(2)));
        assert_eq!(passes.next_owner(), None);
        assert_eq!(
            passes.behind(&id(9)),
            Some(id(1)),
            "what the waiting owner names"
        );
        assert_eq!(passes.behind(&id(8)), None, "a journal nobody names");
        assert_eq!(passes.behind(&id(3)), None, "a repair nobody names");
        let root = passes.stop(&id(9)).or_else(|| passes.stop(&id(1)));
        assert_eq!(
            root.map(|stuck| stuck.stop.tx),
            Some(Some(id(2))),
            "the stop is the one recovery could not settle"
        );
    }

    #[test]
    fn a_settled_owner_releases_what_it_names() {
        let home = repair_home();
        let listed = vec![
            repair(&home, 1, &[Names::Folder(9)]),
            repair(&home, 2, &[Names::Repair(1)]),
        ];
        let mut passes = Passes::new(listed, &home.paths);
        assert_eq!(drain(&mut passes, |_| Outcome::Settled), [id(2), id(1)]);
        for tx in [id(1), id(9)] {
            assert_eq!(passes.behind(&tx), None, "{tx}");
            assert_eq!(passes.stop(&tx), None, "{tx}");
        }
    }

    #[test]
    fn an_unknowable_repair_puts_everything_behind_it() {
        let home = repair_home();
        let listed = vec![
            unknowable(4),
            unknowable(5),
            repair(&home, 1, &[Names::Folder(9)]),
            repair(&home, 3, &[]),
        ];
        let mut passes = Passes::new(listed, &home.paths);
        assert_eq!(passes.next_owner(), Some(id(4)));
        assert_eq!(passes.behind(&id(4)), None, "never behind: it only stops");
        passes.record(id(4), stuck(4));
        assert_eq!(passes.next_owner(), Some(id(5)));
        assert_eq!(passes.behind(&id(5)), None, "a second unknowable stops too");
        passes.record(id(5), stuck(5));
        assert_eq!(passes.next_owner(), Some(id(1)));
        for tx in [1, 3, 9, 8] {
            assert_eq!(passes.behind(&id(tx)), Some(id(4)), "{tx} waits");
        }
    }

    #[test]
    fn a_live_owner_is_settled_only_when_finished() {
        let cases = [
            ("live_committed", RepairPhase::Committed, false),
            ("live_rolled_back", RepairPhase::RolledBack, false),
            ("live_unfinished", RepairPhase::Stepped(1), true),
        ];
        for (case, phase, holds) in cases {
            let home = repair_home();
            let mut owner = repair(&home, 1, &[Names::Folder(9)]);
            owner.live = true;
            let mut passes = Passes::new(vec![owner], &home.paths);
            let finished = finished(phase);
            drain(&mut passes, |_| Outcome::Live { finished });
            assert_eq!(passes.behind(&id(9)).is_some(), holds, "{case}: {phase:?}");
            assert_eq!(
                passes.stop(&id(1)),
                None,
                "{case}: a live owner has no stop"
            );
        }
    }

    #[test]
    fn pass_three_waits_on_an_overlapping_unresolved_journal() {
        use crate::engine::resource::{PathKind, Resource};
        let home = repair_home();
        let passes = Passes::new(Vec::new(), &home.paths);
        let write = |path: &str| {
            let mut set = ResourceSet::new();
            set.write(Resource::from_canonical(Path::new(path), PathKind::Subtree).unwrap());
            set
        };
        let unresolved = vec![(id(8), write("/s/other")), (id(9), write("/s/stage"))];
        assert_eq!(
            passes.waits_for(&write("/s/stage/Foo"), &unresolved),
            Some(id(9)),
            "below an unresolved journal's stage"
        );
        assert_eq!(
            passes.waits_for(&write("/s/elsewhere"), &unresolved),
            None,
            "a disjoint repair runs"
        );
    }
}
