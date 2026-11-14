//! The errors ledger is never more generous than a naive reading of its claim: a
//! property test over its real sessions, not an end-to-end suite (no binary runs).
//!
//! RG-13b. The claim (design 7.1): an error *appeared* in a database when a complete read
//! of it without the error was followed by a complete read with it, in one lifetime of
//! that database, no read of any database held it before, and every database was read
//! completely when it was found. `Model` writes that down a second time, deliberately
//! naively and sharing no code with `ledger::sight`: it keeps every read the scenario
//! made (what each held, whether it was complete, and the database's lifetime, which
//! the generator knows), and answers "may this be an appearance?" by scanning them.
//!
//! Generated histories drive the real ledger sessions (`ledger::open`, `sight`,
//! `add_mark`, `commit`) in a temporary cache: the game writes, an entry recurs or moves
//! a line, `errors` reads, a change reads and marks, a read is degraded in each of the
//! six ways, a database is wiped, a restore puts back an older state with a lower, equal
//! or higher counter, an account appears, a writer that looked earlier records late,
//! and a database is read past `LEDGER_READS_MAX`. Every cluster the ledger creates must
//! be an appearance in the model; no cluster's `before` or `found` is a degraded read;
//! no cluster's reads span two lifetimes or carry another state's session; and making
//! any one complete read degraded never adds a cluster. The model may be more generous
//! than the ledger, never the reverse.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fmt::Write as _;

use jiff::Timestamp;
use omaddons::errors::buggrabber::{TimeEvidence, TimeMeaning, TzEvidence};
use omaddons::errors::ledger::{
    self, ChangeMark, ClientNow, Cluster, DbRead, DbSighting, EntrySighting, Restored, SightMode,
    TargetLedger,
};
use omaddons::errors::observe::{Coverage, Gap};
use omaddons::fsx::ScopeTable;
use omaddons::limits::LEDGER_READS_MAX;
use omaddons::model::RunId;
use omaddons::state::paths::Paths;
use proptest::prelude::*;
use proptest::test_runner::RngSeed;

/// The client's filing in the ledger.
const CLIENT: &str = "c";
/// Error families the game may write.
const FAMILIES: usize = 6;
/// Each database's provider: a provider restore replaces that database alone.
const PROVIDERS: [&str; 3] = ["!BugGrabber", "bdUI", "ElvUI"];

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + seconds).unwrap()
}

// ---------------------------------------------------------------------------------------
// The steps.

#[derive(Debug, Clone, Copy)]
enum Op {
    /// A new entry of `family`, at `line`.
    Add(usize, u8),
    /// An entry happens again.
    Recur(usize),
    /// An entry's line moves: another fingerprint, the same family.
    Move(usize),
    /// An entry is cleared from the file.
    Clear(usize),
}

#[derive(Debug, Clone, Copy)]
enum Counter {
    Lower,
    Equal,
    Higher,
}

#[derive(Debug, Clone, Copy)]
enum Between {
    /// The game writes.
    Write { db: usize, op: Op },
    /// Another `errors` reads and records first.
    Full,
    /// A restore commits.
    Restore {
        whole: bool,
        db: usize,
        back: usize,
        counter: Counter,
    },
}

#[derive(Debug, Clone)]
enum Step {
    /// The game saves database `db` in its next session, with `op` done.
    Write { db: usize, op: Op },
    /// `errors`: every database read, `gap` degrading one; `hidden` one left unfound.
    Full {
        gap: Option<(usize, Gap)>,
        hidden: Option<usize>,
    },
    /// A change: its light read of every database, then a mark on each.
    Change { gap: Option<(usize, Gap)> },
    /// The database wiped: session 1, no entries.
    Reset { db: usize },
    /// A restore puts back an earlier state of every database (`whole`) or of `db`,
    /// its counter lower than, equal to or higher than the current one.
    Restore {
        whole: bool,
        db: usize,
        back: usize,
        counter: Counter,
    },
    /// Another account's database appears, holding `families`.
    Account { families: Vec<usize> },
    /// `errors` looks, `between` happens, then it records what it saw.
    Late { between: Between },
    /// Database `db` written and read whole more often than the ledger keeps reads.
    Burst { db: usize },
}

fn arb_gap() -> impl Strategy<Value = Gap> {
    prop_oneof![
        Just(Gap::NotAFile),
        Just(Gap::Unread),
        Just(Gap::Unstable),
        Just(Gap::Truncated),
        Just(Gap::Unrecognized),
        Just(Gap::Dropped),
    ]
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0..FAMILIES, 0u8..3).prop_map(|(f, l)| Op::Add(f, l)),
        2 => (0usize..8).prop_map(Op::Recur),
        1 => (0usize..8).prop_map(Op::Move),
        1 => (0usize..8).prop_map(Op::Clear),
    ]
}

fn arb_counter() -> impl Strategy<Value = Counter> {
    prop_oneof![
        Just(Counter::Lower),
        Just(Counter::Equal),
        Just(Counter::Higher)
    ]
}

fn arb_step() -> impl Strategy<Value = Step> {
    let gap = || prop::option::weighted(0.3, (0usize..3, arb_gap()));
    prop_oneof![
        8 => (0usize..3, arb_op()).prop_map(|(db, op)| Step::Write { db, op }),
        5 => (gap(), prop::option::weighted(0.1, 0usize..3))
            .prop_map(|(gap, hidden)| Step::Full { gap, hidden }),
        3 => gap().prop_map(|gap| Step::Change { gap }),
        1 => (0usize..3).prop_map(|db| Step::Reset { db }),
        2 => (any::<bool>(), 0usize..3, 0usize..6, arb_counter())
            .prop_map(|(whole, db, back, counter)| Step::Restore { whole, db, back, counter }),
        1 => prop::collection::vec(0..FAMILIES, 0..3).prop_map(|families| Step::Account { families }),
        2 => prop_oneof![
            (0usize..3, arb_op()).prop_map(|(db, op)| Between::Write { db, op }),
            Just(Between::Full),
            (any::<bool>(), 0usize..3, 0usize..6, arb_counter())
                .prop_map(|(whole, db, back, counter)| Between::Restore { whole, db, back, counter }),
        ]
        .prop_map(|between| Step::Late { between }),
        1 => (0usize..3).prop_map(|db| Step::Burst { db }),
    ]
}

fn arb_history() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(arb_step(), 4..28)
}

// ---------------------------------------------------------------------------------------
// The world: what the game wrote.

/// One state of a database file.
#[derive(Debug, Clone)]
struct State {
    stamp: String,
    session: u32,
    /// `(family, line)` per entry.
    entries: Vec<(usize, u8)>,
    written: i64,
    /// Which lifetime of the database it belongs to: a wipe or a restore starts another.
    lifetime: u32,
}

#[derive(Debug, Clone)]
struct Db {
    key: String,
    provider: &'static str,
    states: Vec<State>,
    /// The lifetime as a read can tell it (design RG-D22): it ends at omaddons' restore,
    /// or at a wipe whose counter the next complete read finds lower than before. A wipe
    /// whose counter is back where it was by then looks like play (design record 6,
    /// known limits), so it does not end it.
    observable: u32,
    /// The highest session a complete read found in this lifetime.
    read_max: Option<u32>,
    reset_pending: bool,
    restore_pending: bool,
    /// Each state read, with the lifetime it was read in.
    seen: Vec<(String, u32)>,
}

impl Db {
    fn seen_in(&self, stamp: &str) -> Option<u32> {
        self.seen.iter().find(|(s, _)| s == stamp).map(|(_, l)| *l)
    }
}

impl Db {
    fn now(&self) -> &State {
        self.states.last().unwrap()
    }
}

// ---------------------------------------------------------------------------------------
// The model: every read made, and the naive claim over them.

/// One read of one database.
#[derive(Debug, Clone)]
struct Obs {
    batch: usize,
    db: String,
    stamp: String,
    session: u32,
    lifetime: u32,
    complete: bool,
    families: BTreeSet<usize>,
    at: i64,
}

#[derive(Debug, Default)]
struct Model {
    reads: Vec<Obs>,
    /// Per batch: every database found and read completely.
    covered: Vec<bool>,
}

impl Model {
    /// Whether `c` may be an appearance: `Err` says why not.
    fn allows(&self, c: &Cluster) -> Result<(), String> {
        let a = &c.appearance;
        let family: usize = c.family.trim_start_matches("a:f").parse().unwrap();
        let found = self
            .reads
            .iter()
            .find(|o| {
                o.db == a.database && o.stamp == a.found.stamp && at(o.at) == a.found.observed_at
            })
            .ok_or("found by no read the scenario made")?;
        if !found.complete {
            return Err("found by a degraded read".into());
        }
        let before = self
            .reads
            .iter()
            .filter(|o| o.db == a.database && o.stamp == a.before.stamp)
            .find(|o| o.complete)
            .ok_or("its read without the error was never complete")?;
        if before.lifetime != found.lifetime {
            return Err(format!(
                "its two reads span lifetimes {} and {}",
                before.lifetime, found.lifetime
            ));
        }
        if (a.before.session, a.found.session) != (Some(before.session), Some(found.session)) {
            return Err("a session that is not its states'".into());
        }
        if !self.covered[found.batch] {
            return Err("found while some database was not read completely".into());
        }
        if !found.families.contains(&family) || before.families.contains(&family) {
            return Err("its reads do not show it appear".into());
        }
        if before.at >= found.at {
            return Err("its read without the error is not earlier".into());
        }
        // Nothing held it before its window opened. A read inside the window that held it
        // (after `before`, before `found`) only bounds when it began, as a degraded read's
        // note does (RG-13a's scenario 12): the claim "it appeared between `before` and
        // `found`" stays true. The ledger's `before` is the last look at that state.
        let before_last = a.before.observed_at.as_second() - 1_800_000_000;
        if let Some(o) = self
            .reads
            .iter()
            .find(|o| o.at <= before_last && o.families.contains(&family))
        {
            return Err(format!(
                "{} held it at {}, by its read without it",
                o.db, o.at
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// One run of a history against the real ledger sessions.

struct Run {
    _dir: tempfile::TempDir,
    paths: Paths,
    scopes: ScopeTable,
    world: Vec<Db>,
    clock: i64,
    restores: Vec<Restored>,
    model: Model,
    /// Reads made so far (for `degrade`).
    looked: usize,
    /// This complete read is made degraded instead (assertion 4).
    degrade: Option<usize>,
    /// Every cluster ever created.
    clustered: Vec<Cluster>,
    stamps: u64,
    log: String,
}

impl Run {
    fn new(degrade: Option<usize>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::with_roots(root, root.join("c"), root.join("s"), root.join("k"));
        std::fs::create_dir_all(paths.errors_ledger().parent().unwrap()).unwrap();
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let mut run = Self {
            _dir: dir,
            paths,
            scopes,
            world: Vec::new(),
            clock: 0,
            restores: Vec::new(),
            model: Model::default(),
            looked: 0,
            degrade,
            clustered: Vec::new(),
            stamps: 0,
            log: String::new(),
        };
        run.account(&[0]);
        run
    }

    fn tick(&mut self) -> i64 {
        self.clock += 60;
        self.clock
    }

    fn stamp(&mut self) -> String {
        self.stamps += 1;
        format!("st{}", self.stamps)
    }

    fn account(&mut self, families: &[usize]) {
        let n = self.world.len();
        if n == PROVIDERS.len() {
            return;
        }
        let stamp = self.stamp();
        let written = self.tick();
        self.world.push(Db {
            key: format!("d{n}"),
            provider: PROVIDERS[n],
            states: vec![State {
                stamp,
                session: 3,
                entries: families.iter().map(|f| (*f, 0)).collect(),
                written,
                lifetime: 0,
            }],
            observable: 0,
            read_max: None,
            reset_pending: false,
            restore_pending: false,
            seen: Vec::new(),
        });
    }

    /// The game saves `db` in its next session, with `op` done to its entries.
    fn write(&mut self, db: usize, op: Op) {
        let db = db % self.world.len();
        let mut next = self.world[db].now().clone();
        match op {
            Op::Add(family, line) => next.entries.push((family, line)),
            Op::Recur(i) if !next.entries.is_empty() => {
                let i = i % next.entries.len();
                let entry = next.entries.remove(i);
                next.entries.push(entry);
            }
            Op::Move(i) if !next.entries.is_empty() => {
                let i = i % next.entries.len();
                next.entries[i].1 = (next.entries[i].1 + 1) % 3;
            }
            Op::Clear(i) if !next.entries.is_empty() => {
                let i = i % next.entries.len();
                next.entries.remove(i);
            }
            _ => {}
        }
        next.session += 1;
        next.stamp = self.stamp();
        next.written = self.tick();
        self.world[db].states.push(next);
    }

    fn reset(&mut self, db: usize) {
        let db = db % self.world.len();
        let lifetime = self.world[db].now().lifetime + 1;
        let stamp = self.stamp();
        let written = self.tick();
        self.world[db].reset_pending = true;
        self.world[db].states.push(State {
            stamp,
            session: 1,
            entries: Vec::new(),
            written,
            lifetime,
        });
    }

    fn restore(&mut self, whole: bool, db: usize, back: usize, counter: Counter) {
        let db = db % self.world.len();
        let chosen: Vec<usize> = if whole {
            (0..self.world.len()).collect()
        } else {
            vec![db]
        };
        for d in chosen {
            let now = self.world[d].now().clone();
            let states = &self.world[d].states;
            let old = states[back % states.len()].clone();
            let session = match counter {
                Counter::Lower => now.session.saturating_sub(1).max(1),
                Counter::Equal => now.session,
                Counter::Higher => now.session + 2,
            };
            let stamp = self.stamp();
            let written = self.tick();
            self.world[d].restore_pending = true;
            self.world[d].states.push(State {
                stamp,
                session,
                entries: old.entries,
                written,
                lifetime: now.lifetime + 1,
            });
        }
        let at_ = self.tick();
        self.restores.push(Restored {
            at: at(at_),
            whole,
            providers: if whole {
                Vec::new()
            } else {
                vec![self.world[db].provider.to_owned()]
            },
        });
    }

    /// Looks at every database (but `hidden`), `gap` degrading one; returns the
    /// sightings, recorded in the model as one batch.
    fn look(
        &mut self,
        gap: Option<(usize, Gap)>,
        hidden: Option<usize>,
    ) -> (Vec<DbSighting>, bool) {
        let observed = self.tick();
        let batch = self.model.covered.len();
        let hidden = hidden.map(|h| h % self.world.len());
        let gap = gap.map(|(d, g)| (d % self.world.len(), g));
        let mut sightings = Vec::new();
        let mut complete = true;
        for i in 0..self.world.len() {
            if hidden == Some(i) {
                continue;
            }
            let mut coverage = match gap {
                Some((d, g)) if d == i => Coverage::Degraded(g),
                _ => Coverage::Proved,
            };
            if coverage == Coverage::Proved {
                // Only its completeness goes: it still holds every entry (`Unstable`). A
                // read that also lost entries would take away facts of presence, which
                // no reader can keep.
                if self.degrade == Some(self.looked) {
                    coverage = Coverage::Degraded(Gap::Unstable);
                }
                self.looked += 1;
            }
            // A read that got at the file at all reports its counter; one that could not
            // read it (not a file, unread, no database) reports none.
            let reported = match coverage {
                Coverage::Degraded(Gap::NotAFile | Gap::Unread | Gap::Unrecognized) => None,
                _ => Some(self.world[i].now().session),
            };
            // Only a complete read can tell that a lifetime ended (a degraded one never
            // starts one); any counter read raises the highest the lifetime has shown.
            let db = &mut self.world[i];
            // A degraded read that reports a lower counter ends the lifetime (the next
            // complete read starts another), as a restore does.
            if let (Coverage::Degraded(_), Some(session)) = (coverage, reported)
                && db.read_max.is_some_and(|max| session < max)
            {
                db.restore_pending = true;
            }
            if let (Coverage::Proved, Some(session)) = (coverage, reported) {
                let lower = db.read_max.is_some_and(|max| session < max);
                if db.restore_pending || lower {
                    db.observable += 1;
                    db.read_max = None;
                }
                db.restore_pending = false;
                db.reset_pending = false;
            }
            if let Some(session) = reported {
                db.read_max = Some(db.read_max.map_or(session, |max| max.max(session)));
            }
            let lifetime = db.observable;
            let stamp = db.now().stamp.clone();
            // A state's lifetime is the one its complete read found it in.
            if coverage == Coverage::Proved {
                db.seen.retain(|(s, _)| *s != stamp);
                db.seen.push((stamp, lifetime));
            }
            let db = &self.world[i];
            let state = db.now();
            let held: Vec<(usize, u8)> = match coverage {
                Coverage::Proved | Coverage::Degraded(Gap::Unstable) => state.entries.clone(),
                Coverage::Degraded(Gap::Dropped | Gap::Truncated) => {
                    state.entries[..state.entries.len() / 2].to_vec()
                }
                Coverage::Degraded(_) => Vec::new(),
            };
            complete &= coverage == Coverage::Proved;
            self.model.reads.push(Obs {
                batch,
                db: db.key.clone(),
                stamp: state.stamp.clone(),
                session: state.session,
                lifetime,
                complete: coverage == Coverage::Proved,
                families: held.iter().map(|(f, _)| *f).collect(),
                at: observed,
            });
            sightings.push(DbSighting {
                key: db.key.clone(),
                provider: db.provider.to_owned(),
                stamp: state.stamp.clone(),
                written_at: Some(at(state.written)),
                observed_at: at(observed),
                session: reported,
                read: Some(DbRead {
                    coverage,
                    entries: held
                        .iter()
                        .map(|(family, line)| EntrySighting {
                            family: format!("a:f{family}"),
                            fingerprint: format!("e:f{family}l{line}"),
                            session: Some(state.session),
                            time: TimeEvidence {
                                at: Some(at(state.written - 1)),
                                meaning: TimeMeaning::Last,
                                tz: TzEvidence::Explicit,
                            },
                            counter: 1,
                        })
                        .collect(),
                }),
            });
        }
        let discovered = hidden.is_none();
        self.model.covered.push(discovered && complete);
        (sightings, discovered)
    }

    /// Records `sightings` in one ledger session, with a mark per database for a change;
    /// checks every cluster it creates against the model.
    fn record(
        &mut self,
        sightings: &[DbSighting],
        discovered: bool,
        mode: SightMode,
        mark: bool,
    ) -> Result<(), String> {
        let now = at(self.tick());
        let mut session = ledger::open(&self.paths, &self.scopes, "test").unwrap();
        let client = ClientNow {
            version: Some("1.0".to_owned()),
            now,
        };
        let sighted = session.sight(CLIENT, sightings, &self.restores, &client, discovered, mode);
        let target = session
            .ledger
            .targets
            .get(CLIENT)
            .cloned()
            .unwrap_or_default();
        for id in &sighted.appeared {
            let cluster = &target.clusters[id];
            self.model
                .allows(cluster)
                .map_err(|why| format!("cluster {id} of {}: {why}", cluster.family))?;
            self.clustered.push(cluster.clone());
        }
        if mark {
            // As the light read marks: the database's newest proved read names the state
            // and its session (`add_mark` takes it only when that is the state looked at).
            for s in sightings {
                if let Some(db) = target.databases.get(&s.key) {
                    let newest = db.reads.last().and_then(|r| r.session);
                    session.add_mark(
                        CLIENT,
                        &s.key,
                        ChangeMark {
                            run: RunId::new(),
                            at: s.observed_at,
                            generation: db.generation.clone(),
                            stamp: s.stamp.clone(),
                            session: newest,
                            running: Some(false),
                            settled: true,
                        },
                    );
                }
            }
        }
        let stored = session.commit().unwrap();
        if let Some(target) = stored.ledger.targets.get(CLIENT) {
            marks_hold(target, &self.world)?;
        }
        Ok(())
    }

    fn step(&mut self, step: &Step) -> Result<(), String> {
        let _ = writeln!(self.log, "{step:?}");
        match step {
            Step::Write { db, op } => self.write(*db, *op),
            Step::Full { gap, hidden } => {
                let (sightings, discovered) = self.look(*gap, *hidden);
                self.record(&sightings, discovered, SightMode::Full, false)?;
            }
            Step::Change { gap } => {
                let (sightings, discovered) = self.look(*gap, None);
                self.record(&sightings, discovered, SightMode::Light, true)?;
            }
            Step::Reset { db } => self.reset(*db),
            Step::Restore {
                whole,
                db,
                back,
                counter,
            } => self.restore(*whole, *db, *back, *counter),
            Step::Account { families } => self.account(families),
            Step::Late { between } => {
                let (late, discovered) = self.look(None, None);
                match *between {
                    Between::Write { db, op } => self.write(db, op),
                    Between::Full => {
                        let (first, discovered) = self.look(None, None);
                        self.record(&first, discovered, SightMode::Full, false)?;
                    }
                    Between::Restore {
                        whole,
                        db,
                        back,
                        counter,
                    } => self.restore(whole, db, back, counter),
                }
                self.record(&late, discovered, SightMode::Full, false)?;
            }
            Step::Burst { db } => {
                for _ in 0..=LEDGER_READS_MAX {
                    self.write(*db, Op::Recur(0));
                    let (sightings, discovered) = self.look(None, None);
                    self.record(&sightings, discovered, SightMode::Full, false)?;
                }
            }
        }
        Ok(())
    }
}

/// A mark of a database's current generation names a state of the lifetime that
/// generation's newest read belongs to, with that state's session.
fn marks_hold(target: &TargetLedger, world: &[Db]) -> Result<(), String> {
    for (key, db) in &target.databases {
        let Some(d) = world.iter().find(|d| &d.key == key) else {
            continue;
        };
        let state = |stamp: &str| {
            let s = d.states.iter().find(|s| s.stamp == stamp)?;
            Some((s.session, d.seen_in(stamp)?))
        };
        let Some((_, newest)) = db.reads.last().and_then(|r| state(&r.stamp)) else {
            continue;
        };
        for mark in db.marks.iter().filter(|m| m.generation == db.generation) {
            let Some((session, lifetime)) = state(&mark.stamp) else {
                return Err(format!("a mark on {key} names no state read"));
            };
            if lifetime != newest || mark.session != Some(session) {
                return Err(format!(
                    "a mark on {key} of lifetime {lifetime} in a generation of lifetime {newest}"
                ));
            }
        }
    }
    Ok(())
}

/// What one run of a history left: every cluster created, the model of its reads, and
/// how many complete reads it made.
struct Played {
    clustered: Vec<Cluster>,
    model: Model,
    looked: usize,
}

/// Runs `history`, the read numbered `degrade` made degraded; or why a cluster is not an
/// appearance.
fn play(history: &[Step], degrade: Option<usize>) -> Result<Played, String> {
    let mut run = Run::new(degrade);
    for step in history {
        run.step(step)
            .map_err(|why| format!("{why}\nsteps:\n{}", run.log))?;
    }
    Ok(Played {
        clustered: run.clustered,
        model: run.model,
        looked: run.looked,
    })
}

/// The four assertions over one history: the first three in every run, each cluster as
/// it is created; the fourth by replaying it with one complete read made degraded.
///
/// The fourth is principle 1, "uncertainty never creates a link": a cluster the
/// degraded run adds must still be an appearance under the history's full facts (the
/// undegraded run's model). It may exist where the undegraded run has none: a complete
/// read made while another account's file could not be read decides `baseline`
/// (`coverage_incomplete`) at once (RG-13a's scenario 15), while a degraded read leaves
/// the family undecided until a covered read; that loses a link in the undegraded run,
/// which principle 1 allows, and never makes a false one.
fn check(history: &[Step], degrade_some: &[usize]) -> Result<(), TestCaseError> {
    let full = play(history, None).map_err(TestCaseError::fail)?;
    for pick in degrade_some {
        if full.looked == 0 {
            break;
        }
        let k = pick % full.looked;
        let degraded = play(history, Some(k)).map_err(TestCaseError::fail)?;
        for c in &degraded.clustered {
            if full.clustered.iter().any(|o| o.family == c.family) {
                continue;
            }
            if let Err(why) = full.model.allows(c) {
                return Err(TestCaseError::fail(format!(
                    "making read {k} degraded added a cluster of {} the full facts do not allow: {why}\n{history:#?}",
                    c.family
                )));
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        rng_seed: RngSeed::Fixed(0x13b),
        ..ProptestConfig::default()
    })]

    #[test]
    fn the_ledger_is_never_more_generous_than_the_model(
        history in arb_history(),
        degrade in prop::collection::vec(any::<usize>(), 2),
    ) {
        check(&history, &degrade)?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    #[ignore = "checkpoint: the model against many more generated histories"]
    fn checkpoint_the_ledger_is_never_more_generous_than_the_model(
        history in arb_history(),
        degrade in prop::collection::vec(any::<usize>(), 4),
    ) {
        check(&history, &degrade)?;
    }
}

/// The generator reaches every step kind the bead names, each gap among them.
#[test]
fn the_generator_reaches_every_step_kind() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let mut seen = BTreeSet::new();
    for _ in 0..400 {
        for step in arb_history().new_tree(&mut runner).unwrap().current() {
            let kind = match &step {
                Step::Write { op, .. } => {
                    format!("write {}", format!("{op:?}").split('(').next().unwrap())
                }
                Step::Full {
                    gap: Some((_, g)), ..
                }
                | Step::Change { gap: Some((_, g)) } => format!("gap {g:?}"),
                Step::Full {
                    hidden: Some(_), ..
                } => "hidden".to_owned(),
                Step::Full { .. } => "full".to_owned(),
                Step::Change { .. } => "change".to_owned(),
                Step::Reset { .. } => "reset".to_owned(),
                Step::Restore { counter, .. } => format!("restore {counter:?}"),
                Step::Account { .. } => "account".to_owned(),
                Step::Late { between } => {
                    format!("late {}", format!("{between:?}").split(' ').next().unwrap())
                }
                Step::Burst { .. } => "burst".to_owned(),
            };
            seen.insert(kind);
        }
    }
    for want in [
        "write Add",
        "write Recur",
        "write Move",
        "write Clear",
        "full",
        "hidden",
        "change",
        "reset",
        "restore Lower",
        "restore Equal",
        "restore Higher",
        "account",
        "late Write",
        "late Full",
        "late Restore",
        "burst",
        "gap NotAFile",
        "gap Unread",
        "gap Unstable",
        "gap Truncated",
        "gap Unrecognized",
        "gap Dropped",
    ] {
        assert!(
            seen.contains(want),
            "never generated: {want} (saw {seen:?})"
        );
    }
}
