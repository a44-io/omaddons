//! Barriers (M4-D38, M4-D31): a doctor repair that moves other journals' artifacts owns a
//! barrier over them. A set-aside **suspends** a stuck journal set, renaming its journal
//! file out of `<state>/journal/`; the undo of a set-aside **reactivates** it, its last
//! primitive (the activation) renaming the journal from its provisional name back to its
//! own. While such a repair is unsettled, the journals it names must not be recovered
//! ahead of it, and it protects their targets' records too (E14).
//!
//! This module classifies a `prepared` line and never writes.

use std::collections::BTreeSet;
use std::path::Path;

use super::repair::{JournalName, RepairPrepared, journal_file};
use super::repair_action::Primitive;
use crate::fsx::canonical_lenient;
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

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::engine::repair::PlannedFix;
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
}
