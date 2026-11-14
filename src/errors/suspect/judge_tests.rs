//! The judge's second half (RG-15): the worked rows of design 7.2.4, every cell of
//! 7.2.5 and its three adjustments, the acceptance rows, the examples of 7.2.7, and two
//! properties over generated inputs: `high` needs every fact known, and taking one fact
//! away never raises a grade (principle 1).

use std::collections::BTreeSet;

use proptest::prelude::*;

use super::*;
use crate::errors::ledger::{PositiveSighting, ReadMark};
use crate::model::TransitionChange;

use Confidence::{High, Low, Medium};

fn t(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + seconds).unwrap()
}

/// A transaction or run id numbered `n`.
fn id<T>(n: usize) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Debug,
{
    format!("01M9Q{n:021}").parse().unwrap()
}

fn unix(at: i64) -> TimeEvidence {
    TimeEvidence {
        at: Some(t(at)),
        meaning: TimeMeaning::Last,
        tz: TzEvidence::Explicit,
    }
}

fn local(at: i64) -> TimeEvidence {
    TimeEvidence {
        at: Some(t(at)),
        meaning: TimeMeaning::FirstOrLast,
        tz: TzEvidence::SystemAssumed,
    }
}

fn untimed() -> TimeEvidence {
    TimeEvidence {
        at: None,
        meaning: TimeMeaning::Unknown,
        tz: TzEvidence::SystemAssumed,
    }
}

fn read(observed: i64, written: Option<i64>, session: Option<u32>, version: &str) -> ReadMark {
    ReadMark {
        stamp: format!("st:{observed}"),
        written_at: written.map(t),
        session,
        observed_at: t(observed),
        client_version: Some(version.to_owned()),
    }
}

const G1: &str = "g:1";

fn g1() -> DbGeneration {
    DbGeneration(G1.to_owned())
}

/// A judge scenario on one database of generation `g:1`. The error is in `Foo` (managed,
/// attributed with high confidence). The file was written without it at 100 (session
/// `p`), and with it at 4990, looked at 5000 (session `e`). Its entry carries a unix time
/// of 4000, so `onset.by` is 4000. The game is 1.0 at both reads.
#[derive(Debug, Clone)]
struct Scene {
    appearance: Appearance,
    changes: Vec<ChangeFact>,
    marks: Vec<ChangeMark>,
    databases: usize,
    generation: DbGeneration,
    folder: Option<String>,
    addon: String,
    managed: bool,
    attribution: Confidence,
    stack: Vec<String>,
    depends: Vec<(String, String)>,
    changed_since: BTreeSet<String>,
}

/// A mark's session, `running`, and whether it is settled.
type MarkSpec = (Option<u32>, Option<bool>, bool);

impl Scene {
    fn new(p: Option<u32>, e: Option<u32>, counter: u32) -> Self {
        Self {
            appearance: Appearance {
                database: "d:1".to_owned(),
                generation: g1(),
                provider: "!BugGrabber".to_owned(),
                before: read(110, Some(100), p, "1.0"),
                found: read(5000, Some(4990), e, "1.0"),
                earliest: None,
                entry_session: e,
                time: unix(4000),
                counter,
            },
            changes: Vec::new(),
            marks: Vec::new(),
            databases: 1,
            generation: g1(),
            folder: Some("Foo".to_owned()),
            addon: "Foo".to_owned(),
            managed: true,
            attribution: High,
            stack: Vec::new(),
            depends: Vec::new(),
            changed_since: BTreeSet::new(),
        }
    }

    /// The usual case: `p` 40, the error last in session `e`, `counter` occurrences.
    fn at(e: u32, counter: u32) -> Self {
        Self::new(Some(40), Some(e), counter)
    }

    fn time(mut self, time: TimeEvidence) -> Self {
        self.appearance.time = time;
        self
    }

    fn versions(mut self, from: Option<&str>, to: Option<&str>) -> Self {
        self.appearance.before.client_version = from.map(str::to_owned);
        self.appearance.found.client_version = to.map(str::to_owned);
        self
    }

    fn databases(mut self, databases: usize) -> Self {
        self.databases = databases;
        self
    }

    /// Adds a change at `at` that replaces `folder` (its own addon), with a settled mark
    /// of session `s` when `mark` is given.
    fn change(self, at: i64, folder: &str, mark: Option<(u32, Option<bool>)>) -> Self {
        let mark = mark.map(|(s, running)| (Some(s), running, true));
        self.change_with(at, &[(FolderOp::Replace, folder)], mark)
    }

    fn change_with(mut self, at: i64, ops: &[(FolderOp, &str)], mark: Option<MarkSpec>) -> Self {
        let n = self.changes.len();
        let run: RunId = id(100 + n);
        let ops: Vec<ChangeOp> = ops
            .iter()
            .map(|(op, folder)| ChangeOp {
                op: *op,
                folder: (*folder).to_owned(),
                addon: (*folder).to_owned(),
            })
            .collect();
        let transitions = ops
            .iter()
            .map(|o| ReleaseTransition {
                addon: o.addon.clone(),
                change: TransitionChange::Updated,
                from: None,
                to: None,
            })
            .collect();
        self.changes.push(ChangeFact {
            tx: id(n),
            run: Some(run),
            kind: TxKind::Update,
            at: t(at),
            actor: Actor::Human,
            ops,
            transitions,
            recorded: TransitionSource::Recorded,
        });
        if let Some((session, running, settled)) = mark {
            self.marks.push(ChangeMark {
                run,
                at: t(at),
                generation: g1(),
                stamp: format!("m:{n}"),
                session,
                running,
                settled,
            });
        }
        self
    }

    fn verdict(&self) -> Verdict {
        let depends = |a: &str, b: &str| self.depends.iter().any(|(x, y)| same(x, a) && same(y, b));
        let error = Appeared {
            cluster: "c:1",
            appearance: &self.appearance,
            addon: &self.addon,
            managed: self.managed,
            folder: self.folder.as_deref(),
            attribution: self.attribution,
            stack_folders: self.stack.clone(),
        };
        let cx = JudgeContext {
            now: t(6000),
            generation: &self.generation,
            marks: &self.marks,
            databases: self.databases,
            depends: &depends,
            changed_since: &self.changed_since,
        };
        judge(&error, &self.changes, &cx)
    }

    fn suspect(&self) -> Option<Suspect> {
        self.verdict().suspect
    }

    /// The suspect's grade and unknowns; `None` for no link.
    fn link(&self) -> Option<(Confidence, Vec<Unknown>)> {
        self.suspect().map(|s| (s.confidence, s.evidence.unknowns))
    }
}

fn mark(
    run: usize,
    at: i64,
    generation: &str,
    session: Option<u32>,
    running: Option<bool>,
    settled: bool,
) -> ChangeMark {
    ChangeMark {
        run: id(run),
        at: t(at),
        generation: DbGeneration(generation.to_owned()),
        stamp: "m".to_owned(),
        session,
        running,
        settled,
    }
}

#[test]
fn a_change_s_mark_is_the_newest_of_its_run_in_the_appearance_s_generation() {
    let marks = [
        mark(1, 10, G1, Some(40), Some(false), true),
        mark(1, 20, G1, Some(41), Some(false), false),
        mark(2, 30, G1, Some(42), Some(false), true),
        mark(3, 40, "g:0", Some(43), Some(false), true),
    ];
    let got = mark_of(&marks, Some(id(1)), &g1()).unwrap();
    assert_eq!(
        (got.at, got.settled),
        (t(20), false),
        "an unsettled newest mark is not passed over for an older settled one"
    );
    assert_eq!(mark_of(&marks, Some(id(2)), &g1()).unwrap().at, t(30));
    assert!(
        mark_of(&marks, None, &g1()).is_none(),
        "a change with no run has no mark"
    );
    assert!(mark_of(&marks, Some(id(4)), &g1()).is_none());
    assert!(
        mark_of(&marks, Some(id(3)), &g1()).is_none(),
        "a mark of another generation is never read"
    );
}

#[test]
fn the_load_boundary_follows_whether_the_client_ran() {
    use Boundaries::{Both, One};
    use Unknown::{Accounts, Mark, Running, Sessions};
    let m = |session: Option<u32>, running: Option<bool>, settled: bool| {
        Some(mark(1, 0, G1, session, running, settled))
    };
    let max = u32::MAX;
    let both = |s: u32| Both {
        s,
        stopped: s + 1,
        running: s + 2,
    };
    // (case, the mark, databases, boundaries, what the mark could not tell)
    type Row = (
        &'static str,
        Option<ChangeMark>,
        usize,
        Boundaries,
        Vec<Unknown>,
    );
    let rows: Vec<Row> = vec![
        (
            "not running",
            m(Some(40), Some(false), true),
            1,
            One { s: 40, loaded: 41 },
            vec![],
        ),
        (
            "running: the mixed session loaded the old code",
            m(Some(40), Some(true), true),
            1,
            One { s: 40, loaded: 42 },
            vec![],
        ),
        (
            "unknown: graded under both",
            m(Some(40), None, true),
            1,
            both(40),
            vec![Running],
        ),
        (
            "several accounts, and the client ran",
            m(Some(40), Some(true), true),
            2,
            both(40),
            vec![Accounts],
        ),
        (
            "several accounts, running unknown",
            m(Some(40), None, true),
            3,
            both(40),
            vec![Running, Accounts],
        ),
        (
            "several accounts, not running: counted exactly",
            m(Some(40), Some(false), true),
            2,
            One { s: 40, loaded: 41 },
            vec![],
        ),
        ("no mark", None, 1, Boundaries::Unknown(Mark), vec![]),
        (
            "an unsettled mark",
            m(Some(40), Some(false), false),
            1,
            Boundaries::Unknown(Mark),
            vec![],
        ),
        (
            "a mark without a session",
            m(None, None, true),
            1,
            Boundaries::Unknown(Sessions),
            vec![Running],
        ),
        (
            "u32::MAX overflows",
            m(Some(max), Some(false), true),
            1,
            Boundaries::Unknown(Sessions),
            vec![],
        ),
        (
            "u32::MAX - 1, not running",
            m(Some(max - 1), Some(false), true),
            1,
            One {
                s: max - 1,
                loaded: max,
            },
            vec![],
        ),
        (
            "u32::MAX - 1, running, overflows",
            m(Some(max - 1), Some(true), true),
            1,
            Boundaries::Unknown(Sessions),
            vec![],
        ),
        (
            "u32::MAX - 1, unknown, overflows",
            m(Some(max - 1), None, true),
            1,
            Boundaries::Unknown(Sessions),
            vec![Running],
        ),
    ];
    for (case, mark, databases, want, unknowns) in rows {
        assert_eq!(
            boundaries(mark.as_ref(), databases),
            (want, unknowns),
            "{case}"
        );
    }
}

#[test]
fn the_worked_rows_of_the_session_arithmetic() {
    use Sessions::{NotCandidate, SameSession, Unknown};
    let at = |p: Option<u32>, e: Option<u32>, counter: u32| Scene::new(p, e, counter).appearance;
    let clean = |clean, clean_min, clean_max| Sessions::Clean {
        clean,
        clean_min,
        clean_max,
    };
    let max = u32::MAX;
    let mut earlier = at(Some(40), Some(46), 30);
    earlier.earliest = Some(PositiveSighting {
        database: "d:1".to_owned(),
        generation: g1(),
        observed_at: t(3000),
        written_at: Some(t(2990)),
        entry_session: Some(43),
        time: untimed(),
    });
    // (case, appearance, s, loaded, result)
    let rows: Vec<(&str, Appearance, u32, u32, Sessions)> = vec![
        (
            "game closed; the first login throws 20 times",
            at(Some(40), Some(41), 20),
            40,
            41,
            clean(Some(0), 0, 0),
        ),
        (
            "while playing; /reload; throws",
            at(Some(40), Some(42), 20),
            40,
            42,
            clean(None, 0, 0),
        ),
        (
            "the same, with a read between the reload and the error",
            at(Some(41), Some(42), 20),
            40,
            42,
            clean(Some(0), 0, 0),
        ),
        (
            "while playing; throws before any reload",
            at(Some(40), Some(41), 20),
            40,
            42,
            SameSession,
        ),
        (
            "the same, once",
            at(Some(40), Some(41), 1),
            40,
            42,
            SameSession,
        ),
        (
            "game closed; five reloads; once in the sixth session",
            at(Some(40), Some(46), 1),
            40,
            41,
            clean(Some(5), 5, 5),
        ),
        (
            "the same, thirty times",
            at(Some(40), Some(46), 30),
            40,
            41,
            clean(None, 0, 5),
        ),
        (
            "a range wholly after the load",
            at(Some(43), Some(46), 30),
            40,
            41,
            clean(None, 3, 5),
        ),
        (
            "an earlier read's lower session narrows it",
            earlier,
            40,
            41,
            clean(None, 0, 2),
        ),
        (
            "it last occurred in session 40; mark at 40",
            at(Some(39), Some(40), 20),
            40,
            41,
            NotCandidate,
        ),
        (
            "the same, running",
            at(Some(39), Some(40), 20),
            40,
            42,
            NotCandidate,
        ),
        ("e unknown", at(Some(40), None, 20), 40, 41, Unknown),
        ("e at p", at(Some(42), Some(42), 20), 40, 41, Unknown),
        (
            "e below p, once",
            at(Some(44), Some(42), 1),
            40,
            41,
            Unknown,
        ),
        (
            "p unknown: graded as unknown, not as s",
            at(None, Some(42), 20),
            40,
            41,
            Unknown,
        ),
        (
            "p at u32::MAX overflows",
            at(Some(max), Some(max), 1),
            max - 1,
            max,
            Unknown,
        ),
    ];
    for (case, appearance, s, loaded, want) in rows {
        assert_eq!(sessions(&appearance, s, loaded), want, "{case}");
    }
}

#[test]
fn every_cell_of_the_confidence_table() {
    let max = SUSPECT_CLEAN_SESSIONS_MAX;
    let clean = |n| Sessions::Clean {
        clean: Some(n),
        clean_min: n,
        clean_max: n,
    };
    let range = |clean_min, clean_max| Sessions::Clean {
        clean: None,
        clean_min,
        clean_max,
    };
    // (sessions, one occurrence trusted after the change, written after the change)
    let columns = [
        (clean(0), false, false),
        (clean(1), false, false),
        (clean(max), false, false),
        (clean(max + 1), false, false),
        (range(0, 5), false, false),
        (range(3, 5), false, false),
        (range(0, max + 1), false, false),
        (range(3, max + 1), false, false),
        (Sessions::Unknown, false, false),
        (Sessions::Unknown, false, true),
        (Sessions::SameSession, false, false),
        (Sessions::SameSession, true, false),
    ];
    let n = None;
    let (h, m, l) = (Some(High), Some(Medium), Some(Low));
    let related = [m, l, l, n, l, l, n, n, l, l, l, l];
    let rows = [
        (Relation::Own, [h, m, m, l, m, m, l, l, l, l, l, m]),
        (Relation::InStack, related),
        (Relation::Dependency, related),
        (Relation::None, [l, n, n, n, l, n, l, n, l, n, l, l]),
    ];
    for (relation, want) in rows {
        for ((sessions, exact_after, written_after), want) in columns.iter().zip(want) {
            assert_eq!(
                cell(relation, *sessions, *exact_after, *written_after),
                want,
                "{relation:?} {sessions:?} exact_after={exact_after} written_after={written_after}"
            );
        }
    }
}

#[test]
fn same_session_is_low_unless_one_occurrence_has_a_trusted_time_after_the_change() {
    let playing = |e, counter| Scene::at(e, counter).change(1000, "Foo", Some((40, Some(true))));
    let once = playing(41, 1).suspect().unwrap();
    assert_eq!(
        (once.confidence, once.evidence.same_session),
        (Medium, Some(true))
    );
    assert!(once.evidence.exact);
    assert_eq!(once.evidence.gap_s, 3000);
    let cases = [
        ("twenty times", playing(41, 20), (Low, vec![])),
        (
            "once, a local-time string after the change",
            playing(41, 1).time(local(4000)),
            (Low, vec![Unknown::Time]),
        ),
        (
            "once, no time",
            playing(41, 1).time(untimed()),
            (Low, vec![Unknown::Time]),
        ),
        (
            "once, the unix time at the change itself",
            playing(41, 1).time(unix(1000)),
            (Low, vec![]),
        ),
        (
            "once, a unix time that contradicts the reads",
            playing(41, 1).time(unix(5500)),
            (Low, vec![Unknown::Time]),
        ),
    ];
    for (case, scene, want) in cases {
        assert_eq!(scene.link(), Some(want), "{case}");
    }
}

#[test]
fn a_game_update_lowers_once_and_never_below_low() {
    let updated = |scene: Scene| scene.versions(Some("1.0"), Some("1.1"));
    let own = |e, counter| Scene::at(e, counter).change(1000, "Foo", Some((40, Some(false))));
    let s = updated(own(41, 20)).suspect().unwrap();
    assert_eq!(s.confidence, Medium);
    assert_eq!(
        s.evidence.client_update,
        Some(ClientUpdated {
            from_version: "1.0".to_owned(),
            to_version: "1.1".to_owned(),
        })
    );
    let mut in_stack = Scene::at(42, 1).change(1000, "Lib", Some((40, Some(false))));
    in_stack.stack = vec!["Foo".to_owned(), "Lib".to_owned()];
    let unrelated = |scene: Scene| scene.change(1000, "Bar", Some((40, Some(false))));
    let cases = [
        (
            "own, clean 1: medium, lowered",
            updated(own(42, 1)).link(),
            Some((Low, vec![])),
        ),
        (
            "in_stack, clean 1: low stays low",
            updated(in_stack.clone()).link(),
            Some((Low, vec![])),
        ),
        (
            "in_stack, the same game",
            in_stack.link(),
            Some((Low, vec![])),
        ),
        (
            "own, clean 0, a version unknown: graded as the update it may have been",
            own(41, 20).versions(Some("1.0"), None).link(),
            Some((Medium, vec![Unknown::ClientVersion])),
        ),
        (
            "none, the same game",
            unrelated(Scene::at(41, 20)).link(),
            Some((Low, vec![])),
        ),
        (
            "none, the game updated",
            updated(unrelated(Scene::at(41, 20))).link(),
            None,
        ),
        (
            "none, a version unknown",
            unrelated(Scene::at(41, 20))
                .versions(None, Some("1.0"))
                .link(),
            None,
        ),
    ];
    for (case, got, want) in cases {
        assert_eq!(got, want, "{case}");
    }
    assert_eq!(
        own(41, 20)
            .versions(None, Some("1.0"))
            .suspect()
            .unwrap()
            .evidence
            .client_update,
        None
    );
}

#[test]
fn a_none_link_is_made_only_for_the_newest_close_change() {
    let day = 24 * 60 * 60;
    let bar = |at: i64| Scene::at(41, 20).change(at, "Bar", Some((40, Some(false))));
    assert_eq!(
        bar(4000 - day).link(),
        Some((Low, vec![])),
        "24 hours to the onset"
    );
    assert_eq!(bar(4000 - day - 1).link(), None, "a second more");
    let two = bar(1000).change(2000, "Baz", Some((40, Some(false))));
    let s = two.suspect().unwrap();
    assert_eq!(
        (s.at, s.evidence.other_changes),
        (t(2000), 0),
        "only the newest"
    );
    let range =
        |p, e| Scene::new(Some(p), Some(e), 20).change(1000, "Bar", Some((40, Some(false))));
    assert_eq!(range(40, 42).link(), Some((Low, vec![])), "clean_min 0");
    assert_eq!(
        range(45, 47).link(),
        None,
        "clean None with clean_min above 0"
    );
}

#[test]
fn without_usable_sessions_own_is_low_and_none_needs_no_save_after_the_change() {
    // (case, mark, its generation, unknown)
    let marks: [(&str, Option<MarkSpec>, &str, Unknown); 4] = [
        ("no mark", None, G1, Unknown::Mark),
        (
            "an unsettled mark",
            Some((Some(40), Some(false), false)),
            G1,
            Unknown::Mark,
        ),
        (
            "a mark without a session",
            Some((None, Some(false), true)),
            G1,
            Unknown::Sessions,
        ),
        (
            "a mark of another generation",
            Some((Some(40), Some(false), true)),
            "g:0",
            Unknown::Mark,
        ),
    ];
    for (case, spec, generation, unknown) in marks {
        // The file was written without the error at 100: a change at 50 had a save after
        // it, one at 1000 did not.
        for (at, written_after) in [(1000, false), (50, true)] {
            let scene = |folder: &str| {
                let mut scene =
                    Scene::at(41, 20).change_with(at, &[(FolderOp::Replace, folder)], spec);
                for m in &mut scene.marks {
                    m.generation = DbGeneration(generation.to_owned());
                }
                scene
            };
            let case = format!("{case}, written_after={written_after}");
            assert_eq!(
                scene("Foo").link(),
                Some((Low, vec![unknown])),
                "own: {case}"
            );
            let none = (!written_after).then(|| (Low, vec![unknown]));
            assert_eq!(scene("Bar").link(), none, "none: {case}");
        }
    }
}

#[test]
fn two_boundaries_keep_the_weaker_and_cap_at_medium() {
    let own = |e, counter, running: Option<bool>| {
        Scene::at(e, counter).change(1000, "Foo", Some((40, running)))
    };
    let link = |scene: Scene| {
        let s = scene.suspect().unwrap();
        (
            s.confidence,
            s.evidence.clean_sessions,
            s.evidence.same_session,
            s.evidence.unknowns,
        )
    };
    use Unknown::{Accounts, Running};
    let cases = [
        (
            "once in 42, running unknown",
            link(own(42, 1, None)),
            (Medium, None, Some(false), vec![Running]),
        ),
        (
            "once in 42, running",
            link(own(42, 1, Some(true))),
            (High, Some(0), Some(false), vec![]),
        ),
        (
            "once in 42, not running",
            link(own(42, 1, Some(false))),
            (Medium, Some(1), Some(false), vec![]),
        ),
        (
            "twenty times in 41, running unknown",
            link(own(41, 20, None)),
            (Low, None, None, vec![Running]),
        ),
        (
            "a mark taken away never helps: with it",
            link(own(41, 20, Some(true))),
            (Low, None, Some(true), vec![]),
        ),
        (
            "two accounts, the game ran, once in 42",
            link(own(42, 1, Some(true)).databases(2)),
            (Medium, None, Some(false), vec![Accounts]),
        ),
        (
            "two accounts, the game ran, in 41",
            link(own(41, 20, Some(true)).databases(2)),
            (Low, None, None, vec![Accounts]),
        ),
        (
            "two accounts, the game closed: exact",
            link(own(42, 1, Some(false)).databases(2)),
            (Medium, Some(1), Some(false), vec![]),
        ),
        (
            "three accounts, running unknown",
            link(own(42, 1, None).databases(3)),
            (Medium, None, Some(false), vec![Running, Accounts]),
        ),
    ];
    for (case, got, want) in cases {
        assert_eq!(got, want, "{case}");
    }
    let without = Scene::at(41, 20).change(1000, "Foo", None);
    assert_eq!(
        without.link(),
        Some((Low, vec![Unknown::Mark])),
        "a mark taken away never helps: without it, still low"
    );
}

#[test]
fn an_overflowing_session_is_unknown_never_wrapped() {
    let max = u32::MAX;
    let mark_at_max = Scene::at(41, 20).change(1000, "Foo", Some((max, Some(false))));
    assert_eq!(mark_at_max.link(), Some((Low, vec![Unknown::Sessions])));
    let running =
        Scene::new(Some(max - 1), Some(max), 1).change(1000, "Foo", Some((max - 1, Some(true))));
    assert_eq!(running.link(), Some((Low, vec![Unknown::Sessions])));
    let fits =
        Scene::new(Some(max - 1), Some(max), 1).change(1000, "Foo", Some((max - 1, Some(false))));
    assert_eq!(fits.link(), Some((High, vec![])));
}

#[test]
fn the_worked_examples_of_7_2_7() {
    let closed = Some((40, Some(false)));
    // 5.1: DBM updated with the game closed; a DBM error in the first session.
    let s = Scene::at(41, 20)
        .change(1000, "Foo", closed)
        .suspect()
        .unwrap();
    assert_eq!(
        (s.relation, s.confidence, s.evidence.unknowns.clone()),
        (Relation::Own, High, vec![])
    );
    assert_eq!(s.evidence.clean_sessions, Some(0));

    // Section 3.2, had the error newly appeared: OneForAll (newest, none), Guidelime a day
    // before (own), DBM and Questie older (none).
    let s = section_3_2().suspect().unwrap();
    assert_eq!(
        (s.relation, s.confidence, s.evidence.other_changes),
        (Relation::Own, Medium, 1)
    );
    assert_eq!(s.changed[0].addon, "Foo");

    // 5.6: updated in the morning, the game patched at noon, the error at night.
    let s = Scene::at(41, 20)
        .versions(Some("11.0.2"), Some("11.0.5"))
        .change(1000, "Foo", closed)
        .suspect()
        .unwrap();
    assert_eq!(s.confidence, Medium);
    assert!(s.evidence.client_update.is_some());

    // 5.7: update while playing, the error before any reload, seen 23 times.
    let playing = Some((40, Some(true)));
    let s = Scene::at(41, 23)
        .change(1000, "Foo", playing)
        .suspect()
        .unwrap();
    assert_eq!(
        (s.confidence, s.evidence.same_session, s.evidence.exact),
        (Low, Some(true), false)
    );
    // The same, seen once, with a local-time string after the update.
    let s = Scene::at(41, 1)
        .time(local(4000))
        .change(1000, "Foo", playing)
        .suspect()
        .unwrap();
    assert_eq!(
        (s.confidence, s.evidence.unknowns),
        (Low, vec![Unknown::Time])
    );
    // The same, once with a unix time; whether the game ran could not be told.
    let s = Scene::at(41, 1)
        .change(1000, "Foo", Some((40, None)))
        .suspect()
        .unwrap();
    assert_eq!(
        (s.confidence, s.evidence.unknowns),
        (Medium, vec![Unknown::Running])
    );

    // 5.9: four reloads, then one error.
    let s = Scene::at(45, 1)
        .change(1000, "Foo", closed)
        .suspect()
        .unwrap();
    assert_eq!((s.confidence, s.evidence.clean_sessions), (Medium, Some(4)));

    // The addon's own update ran three clean sessions; an hour-old update of a library it
    // depends on ran none: the library.
    let library = library_and_own();
    let s = library.suspect().unwrap();
    assert_eq!(
        (s.relation, s.confidence, s.evidence.clean_sessions),
        (Relation::Dependency, Medium, Some(0))
    );
    assert_eq!(s.changed[0].addon, "Lib");
    // The same, the library's folder replaced again since: the edge is not asserted.
    let mut since = library;
    since.changed_since.insert("lib".to_owned());
    let s = since.suspect().unwrap();
    assert_eq!(
        (s.relation, s.confidence, s.evidence.clean_sessions),
        (Relation::Own, Medium, Some(3))
    );
    assert_eq!(s.evidence.unknowns, vec![Unknown::Dependencies]);

    // An unrelated addon updated two days ago, nothing else.
    let two_days = 2 * 24 * 60 * 60;
    assert_eq!(
        Scene::at(41, 20)
            .change(4000 - two_days, "Bar", closed)
            .link(),
        None
    );

    // BugGrabber itself updated between the two reads.
    let v = Scene::at(41, 20)
        .change(1000, "Foo", closed)
        .change(2000, "!BugGrabber", closed)
        .verdict();
    assert_eq!(
        (v.state, v.gap, v.suspect),
        (
            JudgmentState::Unsupported,
            Some(JudgmentGap::ProviderChanged),
            None
        )
    );

    // A light read found it, the snapshot was pruned: the line carries its transitions.
    let s = Scene::at(41, 20)
        .change(1000, "Foo", closed)
        .suspect()
        .unwrap();
    assert_eq!(s.changed.len(), 1);
    assert!(!s.evidence.unknowns.contains(&Unknown::Transition));
    // A line from before transitions, its snapshot pruned, and no mark either.
    let mut old = Scene::at(41, 20).change(1000, "Foo", None);
    old.changes[0].transitions.clear();
    old.changes[0].recorded = TransitionSource::Unavailable;
    let s = old.suspect().unwrap();
    assert_eq!(
        (s.relation, s.confidence, s.changed.len()),
        (Relation::Own, Low, 0)
    );
    assert_eq!(
        s.evidence.unknowns,
        vec![Unknown::Mark, Unknown::Transition]
    );
}

/// Section 3.2's history, had the Guidelime error newly appeared (session 41): Questie,
/// DBM, Guidelime a day before, then OneForAll, the newest.
fn section_3_2() -> Scene {
    let day = 24 * 60 * 60;
    let closed = |s| Some((s, Some(false)));
    Scene::at(41, 20)
        .change(1000 - 3 * day, "Questie", closed(37))
        .change(1000 - 2 * day, "DBM", closed(38))
        .change(3000 - day, "Foo", closed(39))
        .change(3000, "OneForAll", closed(40))
}

/// `Foo` requires `Lib`: Foo's own update ran three clean sessions; Lib's, an hour
/// before the error, none. One occurrence, at 4000.
fn library_and_own() -> Scene {
    let mut scene = Scene::at(41, 1)
        .change(-50_000, "Foo", Some((37, Some(false))))
        .change(400, "Lib", Some((40, Some(false))));
    scene.depends = vec![("Foo".to_owned(), "Lib".to_owned())];
    scene
}

#[test]
fn no_judgment_across_an_ended_generation_or_a_buggrabber_change() {
    let mut ended = Scene::at(41, 20).change(1000, "Foo", Some((40, Some(false))));
    ended.generation = DbGeneration("g:2".to_owned());
    let v = ended.verdict();
    assert_eq!(
        (v.state, v.gap, v.suspect),
        (
            JudgmentState::Unsupported,
            Some(JudgmentGap::GenerationEnded),
            None
        )
    );
    assert_eq!((v.algorithm, v.judged_at), (SUSPECT_ALGORITHM, t(6000)));
    let none = Scene::at(41, 20).verdict();
    assert_eq!(
        (none.state, none.gap, none.suspect),
        (JudgmentState::Complete, None, None)
    );
    let after = Scene::at(41, 20).change(4001, "Foo", Some((40, Some(false))));
    assert_eq!(after.verdict().suspect, None, "a change after the onset");
}

#[test]
fn the_verdict_does_not_depend_on_the_order_of_the_changes() {
    for scene in [section_3_2(), library_and_own()] {
        let want = scene.verdict();
        for turn in 1..scene.changes.len() {
            let mut turned = scene.clone();
            turned.changes.rotate_left(turn);
            assert_eq!(turned.verdict(), want);
        }
        let mut reversed = scene.clone();
        reversed.changes.reverse();
        assert_eq!(reversed.verdict(), want);
    }
}

// ---------------------------------------------------------------------------------------
// Properties over generated inputs.

/// A generated change: when, which folder and how, whether it has a run and recorded
/// transitions, and its mark (session offset from `p`'s, `running`, settled, of this
/// generation).
type ChangeSpec = (
    i64,
    &'static str,
    FolderOp,
    bool,
    bool,
    Option<(Option<i32>, Option<bool>, bool, bool)>,
);

fn arb_change() -> impl Strategy<Value = ChangeSpec> {
    (
        prop_oneof![1 => -400_000i64..-3000, 3 => -3000i64..7000],
        prop_oneof![
            4 => Just("Foo"),
            2 => Just("Lib"),
            2 => Just("Bar"),
            1 => Just("Plug"),
            1 => Just("!BugGrabber"),
        ],
        prop_oneof![
            3 => Just(FolderOp::Replace),
            1 => Just(FolderOp::Add),
            1 => Just(FolderOp::Remove),
        ],
        prop::bool::weighted(0.9),
        prop::bool::weighted(0.8),
        prop::option::weighted(
            0.85,
            (
                prop::option::weighted(0.9, -2i32..3),
                prop_oneof![Just(Some(true)), Just(Some(false)), Just(None)],
                prop::bool::weighted(0.85),
                prop::bool::weighted(0.9),
            ),
        ),
    )
}

fn arb_time() -> impl Strategy<Value = TimeEvidence> {
    prop_oneof![
        1 => Just(untimed()),
        1 => (-3000i64..9000).prop_map(local),
        3 => (-3000i64..9000).prop_map(unix),
        1 => (100i64..5000).prop_map(unix),
    ]
}

fn arb_version() -> impl Strategy<Value = Option<&'static str>> {
    prop_oneof![4 => Just(Some("1.0")), 1 => Just(Some("1.1")), 1 => Just(None)]
}

fn arb_versions() -> impl Strategy<Value = (Option<&'static str>, Option<&'static str>)> {
    prop_oneof![
        6 => Just((Some("1.0"), Some("1.0"))),
        1 => Just((Some("1.0"), Some("1.1"))),
        2 => (arb_version(), arb_version()),
    ]
}

prop_compose! {
    /// A scene whose numbers mostly fit together, and sometimes do not: `e` above `p`
    /// (at or below it now and then), marks near `p`, sessions at `u32::MAX`, unknown
    /// `running`, local and unix times on both sides of the changes, marks of another
    /// generation, and several databases.
    fn arb_scene()(
        base in prop_oneof![4 => Just(40u32), 1 => Just(u32::MAX - 6)],
        p_offset in 0u32..4,
        p_known in prop::bool::weighted(0.9),
        e_delta in prop::option::weighted(
            0.92,
            prop_oneof![1 => Just(0u32), 4 => 1u32..3, 2 => 3u32..7],
        ),
        counter in prop_oneof![Just(1u32), Just(2), Just(20)],
        earliest in prop::option::weighted(0.15, 0u32..3),
        before_written in prop::option::weighted(0.85, 0i64..2000),
        found_written in prop::option::weighted(0.85, 0i64..60),
        found_late in 0i64..3000,
        time in arb_time(),
        versions in arb_versions(),
        databases in prop_oneof![3 => Just(1usize), 1 => 2usize..=3],
        ended in prop::bool::weighted(0.05),
        error in (
            prop::option::weighted(0.9, Just("Foo")),
            any::<bool>(),
            prop_oneof![Just(High), Just(Medium), Just(Low)],
            prop::collection::btree_set(prop_oneof![Just("Foo"), Just("Lib"), Just("Bar")], 0..3),
            (any::<bool>(), any::<bool>()),
            prop::collection::btree_set(prop_oneof![Just("Foo"), Just("Lib")], 0..2),
        ),
        changes in prop::collection::vec(arb_change(), 1..=4),
    ) -> Scene {
        let p_session = base.saturating_add(p_offset);
        let p = p_known.then_some(p_session);
        let e = e_delta.map(|delta| p_session.saturating_add(delta));
        let mut scene = Scene::new(p, e, counter).time(time);
        let found = 5000 + found_late;
        let before = before_written.unwrap_or(100);
        scene.appearance.before = read(before + 10, before_written, p, "1.0");
        scene.appearance.found = read(found, found_written.map(|x| found - x), e, "1.0");
        scene = scene.versions(versions.0, versions.1);
        if let Some(lower) = earliest {
            scene.appearance.earliest = Some(PositiveSighting {
                database: "d:1".to_owned(),
                generation: g1(),
                observed_at: t(found - 500),
                written_at: None,
                entry_session: e.map(|e| e.saturating_sub(lower)),
                time: untimed(),
            });
        }
        scene.databases = databases;
        if ended {
            scene.generation = DbGeneration("g:2".to_owned());
        }
        let (folder, managed, attribution, stack, (lib, plug), since) = error;
        scene.folder = folder.map(str::to_owned);
        scene.addon = folder.unwrap_or("Foo").to_owned();
        scene.managed = managed;
        scene.attribution = attribution;
        scene.stack = stack.into_iter().map(str::to_owned).collect();
        if lib {
            scene.depends.push(("Foo".to_owned(), "Lib".to_owned()));
        }
        if plug {
            scene.depends.push(("Plug".to_owned(), "Foo".to_owned()));
        }
        scene.changed_since = since.into_iter().map(str::to_owned).collect();
        for (at, folder, op, has_run, recorded, mark) in changes {
            let spec = mark.map(|(offset, running, settled, _)| {
                (offset.map(|o| p_session.saturating_add_signed(o)), running, settled)
            });
            scene = scene.change_with(at, &[(op, folder)], spec);
            let change = scene.changes.last_mut().unwrap();
            if !recorded {
                change.transitions.clear();
                change.recorded = TransitionSource::Unavailable;
            }
            if let Some((_, _, _, false)) = mark {
                scene.marks.last_mut().unwrap().generation = DbGeneration("g:0".to_owned());
            }
            if !has_run {
                let change = scene.changes.last_mut().unwrap();
                change.run = None;
            }
        }
        consistent(&mut scene);
        scene
    }
}

/// A change that only a trusted time keeps out of the window (it is after the entry's
/// last occurrence, before the read that found it) came after the error, and where its
/// mark can show that, it does: its session is at least `e`. The game cannot produce
/// otherwise, and without it, taking the time away would let in a change whose made-up
/// sessions put it before the error.
fn consistent(scene: &mut Scene) {
    let Some(e) = entry_session(&scene.appearance) else {
        return;
    };
    let (narrow, _) = onset(&scene.appearance);
    let mut untrusted = scene.appearance.clone();
    untrusted.time = untimed();
    let (wide, _) = onset(&untrusted);
    for change in &scene.changes {
        if change.at > narrow.by && change.at <= wide.by {
            for m in scene.marks.iter_mut().filter(|m| Some(m.run) == change.run) {
                if let Some(s) = m.session.as_mut() {
                    *s = (*s).max(e);
                }
            }
        }
    }
}

/// One supporting fact taken away (design 7.2.5's property).
#[derive(Debug, Clone, Copy)]
enum Removal {
    /// The change's mark.
    DropMark(usize),
    Unsettle(usize),
    BlankSession(usize),
    BlankRunning(usize),
    /// The unix time replaced by a local-time string (`false`) or by none (`true`).
    Untrust(bool),
    /// The game's version at the read without the error (`true`) or with it.
    BlankVersion(bool),
    BlankP,
    BlankE,
    ClearTransitions(usize),
    /// The erroring folder changed since.
    ChangedSince,
}

fn removals(scene: &Scene) -> Vec<Removal> {
    let mut all = vec![
        Removal::Untrust(false),
        Removal::Untrust(true),
        Removal::BlankVersion(true),
        Removal::BlankVersion(false),
        Removal::BlankP,
        Removal::BlankE,
        Removal::ChangedSince,
    ];
    for i in 0..scene.changes.len() {
        all.extend([
            Removal::DropMark(i),
            Removal::Unsettle(i),
            Removal::BlankSession(i),
            Removal::BlankRunning(i),
            Removal::ClearTransitions(i),
        ]);
    }
    all
}

/// The marks of change `i`.
fn marks_of(s: &mut Scene, i: usize) -> impl Iterator<Item = &mut ChangeMark> {
    let run = s.changes[i].run;
    s.marks.iter_mut().filter(move |m| Some(m.run) == run)
}

fn remove(scene: &Scene, removal: Removal) -> Scene {
    let mut s = scene.clone();
    match removal {
        Removal::DropMark(i) => {
            let run = s.changes[i].run;
            s.marks.retain(|m| Some(m.run) != run);
        }
        Removal::Unsettle(i) => marks_of(&mut s, i).for_each(|m| m.settled = false),
        Removal::BlankSession(i) => marks_of(&mut s, i).for_each(|m| m.session = None),
        Removal::BlankRunning(i) => marks_of(&mut s, i).for_each(|m| m.running = None),
        Removal::Untrust(to_none) => {
            if s.appearance.time.tz == TzEvidence::Explicit {
                s.appearance.time = match (to_none, s.appearance.time.at) {
                    (false, Some(at)) => TimeEvidence {
                        at: Some(at),
                        meaning: TimeMeaning::FirstOrLast,
                        tz: TzEvidence::SystemAssumed,
                    },
                    _ => untimed(),
                };
            }
        }
        Removal::BlankVersion(before) => {
            let read = if before {
                &mut s.appearance.before
            } else {
                &mut s.appearance.found
            };
            read.client_version = None;
        }
        Removal::BlankP => s.appearance.before.session = None,
        Removal::BlankE => s.appearance.entry_session = None,
        Removal::ClearTransitions(i) => {
            s.changes[i].transitions.clear();
            s.changes[i].recorded = TransitionSource::Unavailable;
        }
        Removal::ChangedSince => {
            if let Some(folder) = s.folder.clone() {
                s.changed_since.insert(folder);
            }
        }
    }
    s
}

/// `after` still says everything `before` did not know. A missing mark leaves every fact
/// the mark would have told unknown: its sessions, whether the client ran, and so which
/// account played; the verdict says `mark` for all of them.
fn covers(after: &[Unknown], before: &[Unknown]) -> bool {
    let mut known: BTreeSet<Unknown> = after.iter().copied().collect();
    if known.contains(&Unknown::Mark) {
        known.extend([Unknown::Sessions, Unknown::Running, Unknown::Accounts]);
    }
    before.iter().all(|u| known.contains(u))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn taking_a_fact_away_never_raises_a_grade(scene in arb_scene()) {
        let before = scene.verdict();
        for removal in removals(&scene) {
            let after = remove(&scene, removal).verdict();
            match (&before.suspect, &after.suspect) {
                (Some(b), Some(a)) => {
                    prop_assert!(
                        grade(a.confidence) <= grade(b.confidence),
                        "{removal:?} raised {:?} to {:?}", b.confidence, a.confidence
                    );
                    prop_assert!(
                        covers(&a.evidence.unknowns, &b.evidence.unknowns),
                        "{removal:?}: {:?} does not cover {:?}", a.evidence.unknowns, b.evidence.unknowns
                    );
                }
                (None, Some(a)) => prop_assert_eq!(a.confidence, Low, "{:?} made a link", removal),
                _ => {}
            }
        }
    }

    #[test]
    fn high_needs_every_fact_known(scene in arb_scene()) {
        let verdict = scene.verdict();
        if let Some(s) = verdict.suspect.filter(|s| s.confidence == High) {
            let mark = mark_of(&scene.marks, s.run, &scene.appearance.generation);
            prop_assert!(mark.is_some_and(|m| m.settled && m.session.is_some() && m.running.is_some()));
            prop_assert_eq!(s.relation, Relation::Own);
            prop_assert_ne!(scene.attribution, Low);
            prop_assert_eq!(s.evidence.clean_sessions, Some(0));
            let (from, to) = (&scene.appearance.before.client_version, &scene.appearance.found.client_version);
            prop_assert!(from.is_some() && from == to);
        }
    }

    #[test]
    fn the_verdict_is_deterministic_and_never_names_a_change_after_the_onset(scene in arb_scene()) {
        let verdict = scene.verdict();
        let mut reversed = scene.clone();
        reversed.changes.reverse();
        prop_assert_eq!(&reversed.verdict(), &verdict);
        let (bounds, _) = onset(&scene.appearance);
        if let Some(s) = &verdict.suspect {
            prop_assert!(s.at <= bounds.by);
            prop_assert_eq!(verdict.state, JudgmentState::Complete);
            prop_assert!(s.evidence.unknowns.windows(2).all(|w| w[0] < w[1]));
        }
        if verdict.state == JudgmentState::Unsupported {
            prop_assert!(verdict.suspect.is_none());
        }
        let mut empty = scene.clone();
        empty.changes.clear();
        prop_assert!(empty.verdict().suspect.is_none());
    }
}

/// The properties are only as strong as what the generator reaches: every grade, the
/// removals lowering one and making a link, and each input the acceptance names.
#[test]
fn the_generator_reaches_every_case_the_properties_need() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let mut seen = BTreeSet::new();
    let strength_of = |v: &Verdict| v.suspect.as_ref().map_or(0, |s| grade(s.confidence));
    for _ in 0..2000 {
        let scene = arb_scene().new_tree(&mut runner).unwrap().current();
        let verdict = scene.verdict();
        seen.insert(match &verdict.suspect {
            Some(s) => format!("{:?}", s.confidence),
            None => format!("{:?}", verdict.state),
        });
        let a = &scene.appearance;
        let time = &a.time;
        for change in &scene.changes {
            let side = if time.at.is_some_and(|at| at > change.at) {
                "after"
            } else {
                "before"
            };
            match time.tz {
                TzEvidence::Explicit => seen.insert(format!("unix {side} a change")),
                TzEvidence::SystemAssumed if time.at.is_some() => {
                    seen.insert(format!("local {side} a change"))
                }
                TzEvidence::SystemAssumed => false,
            };
        }
        let sessions = [a.before.session, a.entry_session]
            .into_iter()
            .chain(scene.marks.iter().map(|m| m.session));
        if sessions.flatten().any(|s| s == u32::MAX) {
            seen.insert("a session at u32::MAX".to_owned());
        }
        if let (Some(p), Some(e)) = (a.before.session, a.entry_session)
            && e <= p
        {
            seen.insert("e at or below p".to_owned());
        }
        if scene.marks.iter().any(|m| m.running.is_none()) {
            seen.insert("running unknown".to_owned());
        }
        if scene.marks.iter().any(|m| m.generation != a.generation) {
            seen.insert("a mark of another generation".to_owned());
        }
        if scene.databases > 1 {
            seen.insert("several databases".to_owned());
        }
        for removal in removals(&scene) {
            let after = strength_of(&remove(&scene, removal).verdict());
            if after < strength_of(&verdict) {
                seen.insert("a removal lowered a grade".to_owned());
            }
            if after > 0 && strength_of(&verdict) == 0 {
                seen.insert("a removal made a link".to_owned());
            }
        }
    }
    let want = [
        "High",
        "Medium",
        "Low",
        "Complete",
        "Unsupported",
        "unix before a change",
        "unix after a change",
        "local before a change",
        "local after a change",
        "a session at u32::MAX",
        "e at or below p",
        "running unknown",
        "a mark of another generation",
        "several databases",
        "a removal lowered a grade",
        "a removal made a link",
    ];
    for case in want {
        assert!(seen.contains(case), "the generator never produced: {case}");
    }
}
