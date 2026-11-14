//! `status`' view of Lua errors (M3-D19, M3-D20): one block per client, counted from a
//! whole observation in plain `status`, or recounted from the errors memo and the
//! dismissals under `--cached`, which opens no BugGrabber file. Either way `new` is
//! decided at the time of the call by [`session_is_new`], the rule `errors` uses, so an
//! error stops being new as its file ages or once it is dismissed, without another
//! `errors` run, and the two commands agree. Read-only: the memo and the dismissals are
//! never written here.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::errors::{AfterChangeBrief, CaptureState, CoverageState};
use crate::errors::ack::{TargetAck, db_key};
use crate::errors::attribute::Confidence;
use crate::errors::ledger::Restored;
use crate::errors::locate::{self, Capture, Coverage, DbFile, Declared};
use crate::errors::memo::{AddonMemo, DbMemo, MemoLink, SessionMemo, TargetMemo};
use crate::errors::observe::{
    DbState, Observation, Observed, ReadState, SourceKind, group, session_is_new,
};
use crate::errors::suspect::Relation;
use crate::state::paths::ClientPaths;

/// The addons `top` lists at most: enough for a notification to name them. A display
/// bound, not a safety limit.
const TOP_MAX: usize = 5;

/// One client's Lua errors at a glance, in `errors`' units: `total` counts its
/// `summary.entries`, `top[].count` an addon's `count` (occurrences).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TargetErrors {
    /// `buggrabber` when Lua errors are captured; `none` (then `ok`, every count 0)
    /// without BugGrabber.
    pub capture: CaptureState,
    pub state: ErrorsState,
    /// `truncated` when a bound cut the walk for BugGrabber's files.
    pub coverage: CoverageState,
    /// New local errors, decided now: they age out, and a dismissal covers them.
    pub new: usize,
    /// Local errors.
    pub total: usize,
    /// Addons (and folders) with local errors.
    pub addons: usize,
    /// Errors other players sent: never new, never in the counts above.
    pub received: usize,
    /// The addons with the most new errors, then occurrences, then by name; at most 5.
    pub top_addons: Vec<TopAddon>,
    /// When the client last wrote a BugGrabber file (at logout or `/reload`).
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub last_session_at: Option<Timestamp>,
    /// When the newest dismissal in effect was made; `null` when none.
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub dismissed_at: Option<Timestamp>,
    /// Why the read was incomplete (`partial`, `unreadable`), for the human line.
    #[serde(skip)]
    pub incomplete: Option<ReadState>,
}

/// How far the counts can be believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorsState {
    /// What was read still stands.
    Ok,
    /// `--cached`: a BugGrabber file, or a `SavedVariables/` folder's file names, changed
    /// since `errors` last read them; the counts are that read's.
    Stale,
    /// `--cached`: BugGrabber captures errors here, and no `errors` run has read them.
    Unchecked,
    /// The last read was incomplete (a file being written, over its size bound, or that
    /// could not be read): the counts are what was read, never a clean zero.
    Partial,
    /// No BugGrabber file could be read.
    Unreadable,
}

/// One addon's errors, in short.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TopAddon {
    /// The managed addon, else the folder, else `Blizzard` or `unknown`.
    pub addon: String,
    /// The first of the folders its errors named (sorted ignoring case); `null` for
    /// Blizzard's code or an unknown source.
    pub folder: Option<String>,
    pub new: usize,
    /// Occurrences.
    pub count: u64,
    /// The change its new errors first appeared after, from the errors memo: the first
    /// of its links that still holds (the same database lifetime, no restore since, the
    /// linked error there and new, fresh); only `current` `high` and `medium` links are
    /// kept. `null` when none holds.
    pub after_change: Option<AfterChangeBrief>,
}

/// What a link of the memo is checked against, besides the memo itself (design 7.3.2):
/// the restores committed on the client since the memo's links were projected (`None`:
/// the history cannot say, and no link counts), the dismissals, and the call's time.
#[derive(Debug, Clone, Copy)]
pub struct LinkCheck<'a> {
    pub restores: Option<&'a [Restored]>,
    pub ack: Option<&'a TargetAck>,
    pub now: Timestamp,
}

impl LinkCheck<'_> {
    /// No restore of the client's `WTF/`, or of this database's provider's
    /// SavedVariables, was committed after the memo's links were projected.
    fn unrestored(&self, memo: &TargetMemo, db: &DbMemo) -> bool {
        let (Some(restores), Some(projected)) = (self.restores, memo.projected_at) else {
            return false;
        };
        !restores.iter().any(|r| {
            r.at > projected
                && (r.whole
                    || r.providers
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(&db.provider)))
        })
    }
}

/// A link as `status` shows it.
fn brief(link: &MemoLink) -> AfterChangeBrief {
    AfterChangeBrief {
        tx: link.tx,
        kind: link.kind,
        at: link.at,
        addon: link.addon.clone(),
        relation: link.relation,
        confidence: link.confidence,
    }
}

/// The links the memo holds for an addon (by name ignoring case, and kind), with their
/// database, in database order, each database's best first.
fn links_for<'m>(
    memo: &'m TargetMemo,
    addon: &str,
    kind: SourceKind,
) -> impl Iterator<Item = (&'m DbMemo, &'m AddonMemo, &'m MemoLink)> {
    let addon = addon.to_owned();
    memo.databases.iter().flat_map(move |db| {
        let addon = addon.clone();
        db.addons
            .iter()
            .filter(move |a| a.addon.eq_ignore_ascii_case(&addon) && a.kind == kind)
            .flat_map(move |a| a.links.iter().map(move |l| (db, a, l)))
    })
}

/// `status --cached`'s link for an addon: the first that passes every test of design
/// 7.3.2 over the memo and the dismissals alone. `unchanged` says whether a database's
/// file is as the memo recorded it: `--cached` cannot tell a reset from play without
/// reading the file, so a link shows only while its file is the one judged.
fn cached_link(
    memo: &TargetMemo,
    addon: &str,
    kind: SourceKind,
    check: &LinkCheck<'_>,
    unchanged: &dyn Fn(&DbMemo) -> bool,
) -> Option<AfterChangeBrief> {
    links_for(memo, addon, kind)
        .find(|(db, a, link)| {
            let ack = check
                .ack
                .and_then(|ack| ack.databases.get(&db_key(&db.path)));
            let state = DbState {
                session: db.session,
                baseline: db.baseline,
                written_at: db.written_at,
            };
            let new = a.sessions.contains_key(&link.session)
                && link.session != 0
                && session_is_new(
                    &state,
                    link.session,
                    db.sessions.get(&link.session).map(|s| &s.newest),
                    ack,
                    check.now,
                );
            db.generation.as_ref() == Some(&link.generation)
                && unchanged(db)
                && check.unrestored(memo, db)
                && new
                && check.now < link.fresh_until_at
        })
        .map(|(_, _, link)| brief(link))
}

/// Plain `status`' link for an addon: the first that passes every test of design 7.3.2
/// over the memo and the live read, which it never writes.
fn live_link(
    observation: &Observation,
    memo: &TargetMemo,
    addon: &str,
    kind: SourceKind,
    check: &LinkCheck<'_>,
) -> Option<AfterChangeBrief> {
    links_for(memo, addon, kind)
        .find(|(db, _, link)| {
            let Some((at, live)) = observation
                .databases
                .iter()
                .enumerate()
                .find(|(_, d)| d.path == db.path)
            else {
                return false;
            };
            // The same lifetime: the file as recorded, or one that continues it.
            let continues = live.stamp.fingerprint() == db.stamp
                || matches!(
                    (live.session, db.session, live.written_at, db.written_at),
                    (Some(s), Some(was), Some(w), Some(written)) if s >= was && w >= written
                );
            let there = observation
                .local()
                .any(|e| e.database == at && e.new && link.fingerprints.contains(&e.fingerprint));
            continues && check.unrestored(memo, db) && there && check.now < link.fresh_until_at
        })
        .map(|(_, _, link)| brief(link))
}

impl TargetErrors {
    /// A client that captures no Lua errors (or whose directory is gone).
    pub fn uncaptured(coverage: CoverageState) -> Self {
        Self {
            capture: CaptureState::None,
            state: ErrorsState::Ok,
            coverage,
            new: 0,
            total: 0,
            addons: 0,
            received: 0,
            top_addons: Vec::new(),
            last_session_at: None,
            dismissed_at: None,
            incomplete: None,
        }
    }

    /// Whether `omaddons errors` would tell more: the counts are stale or were never
    /// taken. Never for a read that was incomplete, which a rerun would not complete.
    pub fn worth_checking(&self) -> bool {
        matches!(self.state, ErrorsState::Stale | ErrorsState::Unchecked)
    }
}

/// The block of a whole observation (plain `status`): `errors`' own counts, and each
/// addon's link from `memo` checked against the live read.
pub fn live(
    observation: &Observation,
    memo: Option<&TargetMemo>,
    check: &LinkCheck<'_>,
) -> TargetErrors {
    let coverage = coverage_state(observation.coverage);
    if observation.capture == Capture::None {
        return TargetErrors::uncaptured(coverage);
    }
    let local: Vec<&Observed> = observation.local().collect();
    let tallies: Vec<Tally> = group(&local)
        .into_iter()
        .map(|g| Tally {
            after_change: memo.and_then(|m| live_link(observation, m, &g.name, g.kind, check)),
            addon: g.name,
            kind: g.kind,
            folders: g.folders,
            new: g.new,
            count: g.count,
        })
        .collect();
    let states: Vec<ReadState> = observation.databases.iter().map(|d| d.state).collect();
    let (state, incomplete) = read_state(&states);
    TargetErrors {
        capture: CaptureState::Buggrabber,
        state,
        coverage,
        new: local.iter().filter(|e| e.new).count(),
        total: local.len(),
        addons: tallies.len(),
        received: observation.received().count(),
        top_addons: top(tallies),
        last_session_at: observation
            .databases
            .iter()
            .filter_map(|d| d.written_at)
            .max(),
        dismissed_at: observation
            .databases
            .iter()
            .filter_map(|d| d.acknowledged.map(|a| a.acked_at))
            .max(),
        incomplete,
    }
}

/// What `status --cached` knows of one client without opening a BugGrabber file: the
/// memo's counts with `new` decided now, `stale` when a file the memo recorded, or a
/// `SavedVariables/` folder's file names, changed since (lstats and listings bounded as
/// `errors`' own walk, M3-D20), and `unchecked` when the memo has no entry for it.
/// `installed` says whether a folder is installed in its AddOns (by name, ignoring case):
/// BugGrabber's, and those the memo found embedding its database, which are looked for
/// again by name (an addon newly declaring it is found by the next `errors`).
pub fn cached(
    client: &ClientPaths,
    installed: &dyn Fn(&str) -> bool,
    memo: Option<&TargetMemo>,
    ack: Option<&TargetAck>,
    now: Timestamp,
    restores: Option<&[Restored]>,
) -> TargetErrors {
    let embedding: Vec<&str> = memo
        .into_iter()
        .flat_map(|m| &m.databases)
        .map(|d| d.provider.as_str())
        .filter(|p| !p.eq_ignore_ascii_case(locate::FOLDER))
        .collect();
    let declared: Vec<Declared> = embedding
        .iter()
        .map(|p| Declared {
            folder: (*p).to_owned(),
        })
        .collect();
    let located = locate::locate_in(client, installed(locate::FOLDER), &declared);
    let coverage = coverage_state(located.coverage);
    if located.capture == Capture::None {
        return TargetErrors::uncaptured(coverage);
    }
    let rel = |f: &DbFile| {
        f.path
            .strip_prefix(client.client_dir())
            .unwrap_or(&f.path)
            .to_string_lossy()
            .into_owned()
    };
    let last_session_at = located
        .files
        .iter()
        .filter_map(|f| Timestamp::from_nanosecond(f.stamp.mtime_ns()).ok())
        .max();
    let Some(memo) = memo else {
        // Never read: nothing is known but the dismissals of the files found.
        return TargetErrors {
            state: ErrorsState::Unchecked,
            last_session_at,
            dismissed_at: located
                .files
                .iter()
                .filter_map(|f| ack?.databases.get(&db_key(&rel(f))))
                .map(|a| a.acked_at)
                .max(),
            capture: CaptureState::Buggrabber,
            ..TargetErrors::uncaptured(coverage)
        };
    };
    let mut seen: Vec<(String, String)> = located
        .files
        .iter()
        .map(|f| (rel(f), f.stamp.fingerprint()))
        .collect();
    seen.sort();
    let mut recorded: Vec<(String, String)> = memo
        .databases
        .iter()
        .map(|d| (d.path.clone(), d.stamp.clone()))
        .collect();
    recorded.sort();
    let listed = located
        .listings
        .iter()
        .map(|l| (l.account, l.fingerprint.as_str()));
    let remembered = memo
        .listings
        .iter()
        .map(|l| (l.account, l.fingerprint.as_str()));
    let changed = memo.capture != "buggrabber"
        || seen != recorded
        || !listed.eq(remembered)
        || embedding.iter().any(|p| !installed(p));
    let mut counts = recount(memo, ack, now);
    let check = LinkCheck { restores, ack, now };
    let unchanged = |db: &DbMemo| {
        seen.iter()
            .any(|(path, stamp)| *path == db.path && *stamp == db.stamp)
    };
    for t in &mut counts.tallies {
        t.after_change = cached_link(memo, &t.addon, t.kind, &check, &unchanged);
    }
    let (state, incomplete) = if changed {
        (ErrorsState::Stale, None)
    } else {
        read_state(&counts.states)
    };
    TargetErrors {
        capture: CaptureState::Buggrabber,
        state,
        coverage,
        new: counts.new,
        total: counts.total,
        addons: counts.tallies.len(),
        received: usize::try_from(memo.received).unwrap_or(usize::MAX),
        top_addons: top(counts.tallies),
        last_session_at,
        dismissed_at: counts.dismissed_at,
        incomplete,
    }
}

/// One addon's errors while counting, grouped as `errors` groups them: by name ignoring
/// case, and kind.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tally {
    addon: String,
    kind: SourceKind,
    folders: Vec<String>,
    new: usize,
    count: u64,
    after_change: Option<AfterChangeBrief>,
}

/// What the memo counts, `new` decided now.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Recount {
    new: usize,
    total: usize,
    /// In database order, then the order their addons are recorded in.
    tallies: Vec<Tally>,
    states: Vec<ReadState>,
    dismissed_at: Option<Timestamp>,
}

/// The memo's counts, each `(database, session)` judged new now by [`session_is_new`]
/// with the database's own newest time for that session, exactly as `errors` judges its
/// entries. BugGrabber numbers sessions from 1, so the memo's session 0 holds only the
/// errors without one, which are never new.
fn recount(memo: &TargetMemo, ack: Option<&TargetAck>, now: Timestamp) -> Recount {
    let mut out = Recount {
        new: 0,
        total: 0,
        tallies: Vec::new(),
        states: Vec::new(),
        dismissed_at: None,
    };
    for db in &memo.databases {
        out.states.push(db.state);
        out.total += usize::try_from(db.entries).unwrap_or(usize::MAX);
        let ack = ack.and_then(|a| a.databases.get(&db_key(&db.path)));
        if let Some(a) = ack.filter(|a| db.session.is_none_or(|s| a.acked_session <= s)) {
            out.dismissed_at = out.dismissed_at.max(Some(a.acked_at));
        }
        let state = DbState {
            session: db.session,
            baseline: db.baseline,
            written_at: db.written_at,
        };
        let fresh: BTreeSet<u32> = db
            .sessions
            .iter()
            .filter(|(s, m)| **s != 0 && session_is_new(&state, **s, Some(&m.newest), ack, now))
            .map(|(s, _)| *s)
            .collect();
        out.new += entries_in(&db.sessions, &fresh);
        for a in &db.addons {
            let at = match out
                .tallies
                .iter()
                .position(|t| t.addon.eq_ignore_ascii_case(&a.addon) && t.kind == a.kind)
            {
                Some(at) => at,
                None => {
                    out.tallies.push(Tally {
                        addon: a.addon.clone(),
                        kind: a.kind,
                        folders: Vec::new(),
                        new: 0,
                        count: 0,
                        after_change: None,
                    });
                    out.tallies.len() - 1
                }
            };
            let t = &mut out.tallies[at];
            t.new += entries_in(&a.sessions, &fresh);
            t.count += a.sessions.values().map(|m| m.occurrences).sum::<u64>();
            for folder in &a.folders {
                if !t.folders.iter().any(|f| f.eq_ignore_ascii_case(folder)) {
                    t.folders.push(folder.clone());
                }
            }
        }
    }
    for t in &mut out.tallies {
        t.folders.sort_by_key(|f| f.to_ascii_lowercase());
    }
    out
}

/// The entries of `sessions` that are in a session of `fresh`.
fn entries_in(sessions: &BTreeMap<u32, SessionMemo>, fresh: &BTreeSet<u32>) -> usize {
    sessions
        .iter()
        .filter(|(s, _)| fresh.contains(s))
        .map(|(_, m)| usize::try_from(m.entries).unwrap_or(usize::MAX))
        .sum()
}

/// The first [`TOP_MAX`] of `tallies` in `errors`' order: new errors, then occurrences,
/// then name ignoring case (then kind, so the order never depends on how they came).
fn top(mut tallies: Vec<Tally>) -> Vec<TopAddon> {
    tallies.sort_by(|a, b| {
        (b.new, b.count)
            .cmp(&(a.new, a.count))
            .then_with(|| {
                a.addon
                    .to_ascii_lowercase()
                    .cmp(&b.addon.to_ascii_lowercase())
            })
            .then(a.kind.cmp(&b.kind))
    });
    tallies
        .into_iter()
        .take(TOP_MAX)
        .map(|t| TopAddon {
            folder: t.folders.into_iter().next(),
            addon: t.addon,
            new: t.new,
            count: t.count,
            after_change: t.after_change,
        })
        .collect()
}

/// `ok`, `partial` or `unreadable` from how each database was read, and why a read was
/// incomplete: a file being written first, then one over its bound, then one unreadable.
fn read_state(states: &[ReadState]) -> (ErrorsState, Option<ReadState>) {
    if !states.is_empty() && states.iter().all(|s| *s == ReadState::Unreadable) {
        return (ErrorsState::Unreadable, Some(ReadState::Unreadable));
    }
    let why = [
        ReadState::Partial,
        ReadState::OverBudget,
        ReadState::Unreadable,
    ]
    .into_iter()
    .find(|w| states.contains(w));
    match why {
        Some(why) => (ErrorsState::Partial, Some(why)),
        None => (ErrorsState::Ok, None),
    }
}

fn coverage_state(coverage: Coverage) -> CoverageState {
    match coverage {
        Coverage::Complete => CoverageState::Complete,
        Coverage::Truncated { .. } => CoverageState::Truncated,
    }
}

/// The human line under client `id`'s row, when its errors say anything: new errors, a
/// read that is stale, unchecked, incomplete or unreadable, or a walk a bound cut. Names
/// and counts only, never an error's text (M3-D21). The first addon whose link counts
/// says after which change (`DBM 1, likely since its update today 16:02`).
pub fn human_line(id: &str, e: &TargetErrors, now: Timestamp) -> Option<String> {
    if matches!(e.capture, CaptureState::None) {
        return None;
    }
    let look = format!("omaddons errors -t {id}");
    let mut parts: Vec<String> = match e.state {
        ErrorsState::Unchecked => vec!["not read yet".to_owned(), look],
        ErrorsState::Stale if e.new > 0 => {
            vec![format!("{} new when last read, changed since", e.new), look]
        }
        ErrorsState::Stale => vec!["changed since last read".to_owned(), look],
        ErrorsState::Unreadable => vec!["BugGrabber's data could not be read".to_owned()],
        ErrorsState::Partial => {
            let why = match e.incomplete {
                Some(ReadState::OverBudget) => "file over its size limit",
                Some(ReadState::Unreadable) => "a file could not be read",
                _ => "file was being written",
            };
            vec![
                format!("incomplete read ({why})"),
                format!("{} new so far", e.new),
            ]
        }
        ErrorsState::Ok if e.new > 0 => {
            let mut linked = false;
            let named: Vec<String> = e
                .top_addons
                .iter()
                .filter(|a| a.new > 0)
                .map(|a| match &a.after_change {
                    Some(link) if !linked => {
                        linked = true;
                        format!("{} {}, {}", a.addon, a.new, link_phrase(link, now))
                    }
                    _ => format!("{} {}", a.addon, a.new),
                })
                .collect();
            // New errors of addons `top_addons` leaves out.
            let shown: usize = e.top_addons.iter().map(|a| a.new).sum();
            let more = if shown < e.new { ", …" } else { "" };
            vec![format!("{} new ({}{more})", e.new, named.join(", ")), look]
        }
        ErrorsState::Ok => Vec::new(),
    };
    if matches!(e.coverage, CoverageState::Truncated) {
        if parts.is_empty() {
            parts.push("none new".to_owned());
        }
        parts.push("read in part (too many accounts or files)".to_owned());
    }
    (!parts.is_empty()).then(|| format!("  errors: {}", parts.join(" · ")))
}

/// `likely since its update today 16:02` for the addon's own change, `possible after
/// update today 16:02` for any other (design 7.3.2): evidence of order, never of cause.
fn link_phrase(link: &AfterChangeBrief, now: Timestamp) -> String {
    let word = match link.confidence {
        Confidence::High => "likely",
        Confidence::Medium => "possible",
        Confidence::Low => "weak",
    };
    let when = super::backup::when(link.at, now);
    if link.relation == Relation::Own {
        format!("{word} since its update {when}")
    } else {
        format!(
            "{word} after {} {when}",
            super::history::kind_name(link.kind)
        )
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use jiff::SignedDuration;
    use proptest::prelude::*;

    use super::*;
    use crate::errors::ack::Ack;
    use crate::errors::attribute::{Attributed, Attribution, Confidence, Method};
    use crate::errors::buggrabber::{
        BugEntry, DbShape, EntryType, Origin, TimeEvidence, TimeMeaning, TzEvidence,
    };
    use crate::errors::locate::Skipped;
    use crate::errors::memo::{AddonMemo, DbMemo, MemoCoverage, MemoListing, target_memo};
    use crate::errors::observe::{Database, mark_new};
    use crate::fsx::FileStamp;

    const NOW: &str = "2026-10-15T18:30:00Z";
    const SV: &str = "WTF/Account/FAKEACCT/SavedVariables";

    fn now() -> Timestamp {
        NOW.parse().unwrap()
    }

    fn hours(n: i64) -> SignedDuration {
        SignedDuration::from_hours(n)
    }

    // -----------------------------------------------------------------------------------
    // `errors`' counts and the memo's recount agree (shared with B8's rule).

    /// Who an entry is attributed to: `(name, kind, folder)`. Two spell one addon two
    /// ways, two share a name with different kinds.
    const WHO: [(&str, SourceKind, Option<&str>); 7] = [
        ("Foo", SourceKind::Addon, Some("Foo")),
        ("foo", SourceKind::Addon, Some("FOO")),
        ("Foo", SourceKind::Library, Some("Foo_Lib")),
        ("Bar", SourceKind::Addon, Some("Bar_Core")),
        ("Bar", SourceKind::Addon, Some("Bar_Ui")),
        ("Blizzard", SourceKind::Blizzard, None),
        ("unknown", SourceKind::Unknown, None),
    ];

    #[derive(Debug, Clone)]
    struct GenEntry {
        session: Option<u32>,
        received: bool,
        who: usize,
        counter: u32,
        /// Minutes before `NOW`, when it has a time.
        ago: Option<i64>,
        meaning: TimeMeaning,
    }

    #[derive(Debug, Clone)]
    struct GenDb {
        session: Option<u32>,
        baseline: u32,
        /// Hours before `NOW` the client wrote the file.
        written_ago: i64,
        /// `(acked_session, minutes before NOW)`.
        ack: Option<(u32, i64)>,
        entries: Vec<GenEntry>,
    }

    fn gen_entry() -> impl Strategy<Value = GenEntry> {
        (
            prop::option::weighted(0.9, 1u32..=7),
            prop::bool::weighted(0.15),
            0..WHO.len(),
            1u32..=5,
            prop::option::of(-120i64..=20_000),
            prop_oneof![
                Just(TimeMeaning::Last),
                Just(TimeMeaning::FirstOrLast),
                Just(TimeMeaning::Unknown)
            ],
        )
            .prop_map(|(session, received, who, counter, ago, meaning)| GenEntry {
                session,
                received,
                who,
                counter,
                ago,
                meaning,
            })
    }

    fn gen_db() -> impl Strategy<Value = GenDb> {
        (
            prop::option::weighted(0.9, 1u32..=6),
            0u32..=7,
            0i64..=10 * 24,
            prop::option::of((0u32..=8, -60i64..=15_000)),
            prop::collection::vec(gen_entry(), 0..12),
        )
            .prop_map(|(session, baseline, written_ago, ack, entries)| GenDb {
                session,
                baseline,
                written_ago,
                ack,
                entries,
            })
    }

    fn stamp() -> FileStamp {
        FileStamp::read(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap()
            .unwrap()
    }

    fn observed(database: usize, g: &GenEntry) -> Observed {
        let (name, kind, folder) = WHO[g.who];
        let target = match (kind, folder) {
            (SourceKind::Addon, Some(f)) => Attributed::Addon(f.to_owned()),
            (SourceKind::Library, Some(f)) => Attributed::Library(f.to_owned()),
            (SourceKind::Blizzard, _) => Attributed::Blizzard,
            _ => Attributed::Unknown,
        };
        Observed {
            database,
            entry: BugEntry {
                message: format!("{name}: stack overflow"),
                stack: None,
                locals: None,
                session: g.session,
                time: None,
                counter: g.counter,
                entry_type: EntryType::Error,
                origin: if g.received {
                    Origin::Received
                } else {
                    Origin::Local
                },
            },
            attribution: Attribution {
                target,
                method: Method::FirstAddonFrame,
                confidence: Confidence::High,
            },
            name: name.to_owned(),
            managed: kind == SourceKind::Addon,
            kind,
            folder: folder.map(str::to_owned),
            new: false,
            evidence: TimeEvidence {
                at: g.ago.map(|m| now() - SignedDuration::from_mins(m)),
                meaning: g.meaning,
                tz: TzEvidence::Explicit,
            },
            fingerprint: format!("e:{:016x}", g.who),
        }
    }

    /// An observation of `dbs` as `read_target` builds one (entries in database order,
    /// each database's marked by its rule), and the dismissals.
    fn observation(dbs: &[GenDb]) -> (Observation, TargetAck) {
        let mut acks = TargetAck::default();
        let mut obs = Observation {
            capture: Capture::BugGrabber,
            coverage: Coverage::Complete,
            skipped: Skipped::default(),
            listings: Vec::new(),
            accounts: 1,
            databases: Vec::new(),
            entries: Vec::new(),
            warnings: Vec::new(),
        };
        for (at, g) in dbs.iter().enumerate() {
            let path = format!("{SV}/Pack{at}.lua");
            let ack = g.ack.map(|(session, ago)| Ack {
                acked_session: session,
                acked_at: now() - SignedDuration::from_mins(ago),
            });
            if let Some(a) = ack {
                acks.databases.insert(db_key(&path), a);
            }
            let written_at = Some(now() - hours(g.written_ago));
            let first = obs.entries.len();
            obs.entries
                .extend(g.entries.iter().map(|e| observed(at, e)));
            let state = DbState {
                session: g.session,
                baseline: g.baseline,
                written_at,
            };
            mark_new(&mut obs.entries[first..], &state, ack.as_ref(), now());
            obs.databases.push(Database {
                id: format!("db-{}", at + 1),
                account: 1,
                provider: format!("Pack{at}"),
                shape: DbShape::Modern,
                session: g.session,
                state: ReadState::Complete,
                coverage: crate::errors::observe::Coverage::Proved,
                baseline: g.baseline,
                acknowledged: ack.filter(|a| g.session.is_none_or(|s| a.acked_session <= s)),
                path,
                stamp: stamp(),
                written_at,
            });
        }
        (obs, acks)
    }

    proptest! {
        /// `status --cached` recounts from the memo exactly what `errors` counts from the
        /// files, per client and per addon, whatever the sessions, times and dismissals.
        #[test]
        fn the_memo_recounts_what_errors_counts(dbs in prop::collection::vec(gen_db(), 1..4)) {
            let (obs, acks) = observation(&dbs);
            let live = live(&obs, None, &no_links());
            let memo = target_memo(&obs, None, None);
            let again = recount(&memo, Some(&acks), now());
            prop_assert_eq!(live.new, again.new);
            prop_assert_eq!(live.total, again.total);
            prop_assert_eq!(live.addons, again.tallies.len());
            prop_assert_eq!(live.received as u64, memo.received);
            prop_assert_eq!(live.dismissed_at, again.dismissed_at);
            prop_assert_eq!(&live.top_addons, &top(again.tallies));
            // And per addon, every one of them, as `errors` lists them.
            let local: Vec<&Observed> = obs.local().collect();
            let groups = group(&local);
            prop_assert_eq!(live.new, groups.iter().map(|g| g.new).sum::<usize>());
            for (g, t) in groups.iter().zip(&live.top_addons) {
                prop_assert_eq!((&g.name, g.new, g.count), (&t.addon, t.new, t.count));
            }
        }
    }

    // -----------------------------------------------------------------------------------
    // `--cached` on a client: lstats and listings decide whether the memo still stands.

    /// A client with `accounts` account folders, each with BugGrabber's file.
    fn client(accounts: usize) -> (tempfile::TempDir, ClientPaths) {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..accounts {
            let name = if i == 0 {
                "FAKEACCT".to_owned()
            } else {
                format!("FAKEACCT{i:02}")
            };
            let sv = dir
                .path()
                .join("WTF/Account")
                .join(name)
                .join("SavedVariables");
            fs::create_dir_all(&sv).unwrap();
            fs::write(sv.join("!BugGrabber.lua"), b"BugGrabberDB = {}\n").unwrap();
        }
        let paths = ClientPaths::new(fs::canonicalize(dir.path()).unwrap());
        (dir, paths)
    }

    fn session(entries: u64, newest: Timestamp) -> SessionMemo {
        SessionMemo {
            entries,
            occurrences: entries * 2,
            newest: TimeEvidence {
                at: Some(newest),
                meaning: TimeMeaning::Last,
                tz: TzEvidence::Explicit,
            },
        }
    }

    /// What `errors` would have recorded for FAKEACCT's file as it is now: session 5,
    /// baseline 4, written an hour ago; Foo threw 2 errors in session 4 and 3 in 5.
    fn memo_of(client: &ClientPaths) -> TargetMemo {
        let located = locate::locate_in(client, true, &[]);
        let at = now() - hours(1);
        let databases = located
            .files
            .iter()
            .map(|f| DbMemo {
                path: f
                    .path
                    .strip_prefix(client.client_dir())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                account: f.account,
                provider: f.folder.clone(),
                stamp: f.stamp.fingerprint(),
                written_at: Some(at),
                state: ReadState::Complete,
                session: Some(5),
                baseline: 4,
                sessions: BTreeMap::from([(4, session(2, at)), (5, session(3, at))]),
                entries: 5,
                occurrences: 10,
                addons: vec![AddonMemo {
                    addon: "Foo".to_owned(),
                    managed: true,
                    kind: SourceKind::Addon,
                    sessions: BTreeMap::from([(4, session(2, at)), (5, session(3, at))]),
                    folders: vec!["Foo".to_owned()],
                    links: Vec::new(),
                }],
                generation: None,
            })
            .collect();
        TargetMemo {
            capture: "buggrabber".to_owned(),
            coverage: MemoCoverage {
                truncated: false,
                accounts: located.listings.len(),
                files: located.files.len(),
            },
            listings: located
                .listings
                .iter()
                .map(|l| MemoListing {
                    account: l.account,
                    fingerprint: l.fingerprint.clone(),
                })
                .collect(),
            received: 1,
            databases,
            ledger_revision: None,
            projected_at: None,
        }
    }

    /// No link can count: no restores known (as without a projection).
    fn no_links() -> LinkCheck<'static> {
        LinkCheck {
            restores: None,
            ack: None,
            now: now(),
        }
    }

    fn bug_grabber(folder: &str) -> bool {
        folder.eq_ignore_ascii_case(locate::FOLDER)
    }

    #[test]
    fn an_unchanged_client_keeps_the_memo_and_new_is_decided_now() {
        let (_dir, c) = client(1);
        let memo = memo_of(&c);
        let e = cached(&c, &bug_grabber, Some(&memo), None, now(), None);
        assert_eq!(
            (e.capture, e.state, e.coverage),
            (
                CaptureState::Buggrabber,
                ErrorsState::Ok,
                CoverageState::Complete
            )
        );
        assert_eq!((e.new, e.total, e.addons, e.received), (3, 5, 1, 1));
        assert_eq!(
            e.top_addons,
            [TopAddon {
                addon: "Foo".to_owned(),
                folder: Some("Foo".to_owned()),
                new: 3,
                count: 10,
                after_change: None,
            }]
        );
        assert!(e.last_session_at.is_some());
        assert!(!e.worth_checking());
        // A week later, with no file changed, nothing is new any more.
        let later = cached(
            &c,
            &bug_grabber,
            Some(&memo),
            None,
            now() + hours(7 * 24),
            None,
        );
        assert_eq!(
            (later.state, later.new, later.total),
            (ErrorsState::Ok, 0, 5)
        );
    }

    #[test]
    fn a_dismissal_covers_what_the_memo_counted() {
        let (_dir, c) = client(1);
        let memo = memo_of(&c);
        let dismissed = now() - hours(1) + SignedDuration::from_mins(5);
        let ack = TargetAck {
            databases: BTreeMap::from([(
                db_key(&memo.databases[0].path),
                Ack {
                    acked_session: 5,
                    acked_at: dismissed,
                },
            )]),
        };
        let e = cached(&c, &bug_grabber, Some(&memo), Some(&ack), now(), None);
        assert_eq!((e.state, e.new, e.total), (ErrorsState::Ok, 0, 5));
        assert_eq!(e.top_addons[0].new, 0);
        assert_eq!(e.dismissed_at, Some(dismissed));
        // Without a memo (the cache emptied), it is unchecked, never new, and the
        // dismissal is still known.
        let unread = cached(&c, &bug_grabber, None, Some(&ack), now(), None);
        assert_eq!(
            (unread.state, unread.new, unread.total, unread.dismissed_at),
            (ErrorsState::Unchecked, 0, 0, Some(dismissed))
        );
        assert!(unread.worth_checking());
    }

    #[test]
    fn a_rewritten_file_or_a_new_one_makes_it_stale() {
        let (dir, c) = client(1);
        let memo = memo_of(&c);
        let sv = dir.path().join(SV);
        fs::write(
            sv.join("!BugGrabber.lua"),
            b"BugGrabberDB = { session = 6 }\n",
        )
        .unwrap();
        let e = cached(&c, &bug_grabber, Some(&memo), None, now(), None);
        assert_eq!((e.state, e.new, e.total), (ErrorsState::Stale, 3, 5));
        assert!(e.worth_checking());

        let (dir, c) = client(1);
        let memo = memo_of(&c);
        fs::write(dir.path().join(SV).join("Details.lua"), b"x").unwrap();
        let e = cached(&c, &bug_grabber, Some(&memo), None, now(), None);
        assert_eq!(e.state, ErrorsState::Stale, "a new file name in the folder");

        let (dir, c) = client(1);
        let memo = memo_of(&c);
        fs::remove_file(dir.path().join(SV).join("!BugGrabber.lua")).unwrap();
        let e = cached(&c, &bug_grabber, Some(&memo), None, now(), None);
        assert_eq!(
            e.state,
            ErrorsState::Stale,
            "BugGrabber is installed, its file gone"
        );
    }

    #[test]
    fn an_embedding_addon_is_looked_for_again_and_its_removal_is_stale() {
        let (dir, c) = client(1);
        fs::write(dir.path().join(SV).join("bdUI.lua"), b"BugGrabberDB = {}\n").unwrap();
        let mut memo = memo_of(&c);
        let located = locate::locate_in(
            &c,
            true,
            &[Declared {
                folder: "bdUI".to_owned(),
            }],
        );
        let embedded = located.files.iter().find(|f| f.folder == "bdUI").unwrap();
        let mut db = memo.databases[0].clone();
        db.path = format!("{SV}/bdUI.lua");
        db.provider = "bdUI".to_owned();
        db.stamp = embedded.stamp.fingerprint();
        memo.databases.push(db);
        let both = |f: &str| bug_grabber(f) || f.eq_ignore_ascii_case("bdui");
        let e = cached(&c, &both, Some(&memo), None, now(), None);
        assert_eq!((e.state, e.total, e.new), (ErrorsState::Ok, 10, 6));
        let e = cached(&c, &bug_grabber, Some(&memo), None, now(), None);
        assert_eq!(e.state, ErrorsState::Stale, "bdUI was removed");
    }

    #[test]
    fn incomplete_reads_are_partial_or_unreadable_never_clean() {
        let (_dir, c) = client(2);
        let mut memo = memo_of(&c);
        memo.databases[1].state = ReadState::OverBudget;
        let e = cached(&c, &bug_grabber, Some(&memo), None, now(), None);
        assert_eq!(
            (e.state, e.incomplete, e.new, e.total),
            (ErrorsState::Partial, Some(ReadState::OverBudget), 6, 10)
        );
        assert!(!e.worth_checking(), "reading it again would not help");
        for db in &mut memo.databases {
            db.state = ReadState::Unreadable;
        }
        let e = cached(&c, &bug_grabber, Some(&memo), None, now(), None);
        assert_eq!(e.state, ErrorsState::Unreadable);
        assert!(!e.worth_checking());
    }

    #[test]
    fn without_bug_grabber_nothing_is_captured() {
        let (_dir, c) = client(0);
        let none = |_: &str| false;
        let e = cached(&c, &none, None, None, now(), None);
        assert_eq!(e, TargetErrors::uncaptured(CoverageState::Complete));
        // Installed but never run: nothing read yet.
        let e = cached(&c, &bug_grabber, None, None, now(), None);
        assert_eq!(
            (e.capture, e.state),
            (CaptureState::Buggrabber, ErrorsState::Unchecked)
        );
    }

    #[test]
    fn more_accounts_than_the_bound_is_truncated() {
        let (_dir, c) = client(crate::limits::ERRORS_MAX_ACCOUNTS + 1);
        let memo = memo_of(&c);
        let e = cached(&c, &bug_grabber, Some(&memo), None, now(), None);
        assert_eq!(
            (e.coverage, e.state),
            (CoverageState::Truncated, ErrorsState::Ok)
        );
    }

    // -----------------------------------------------------------------------------------
    // The human line.

    fn errors(state: ErrorsState, new: usize, top: &[(&str, usize)]) -> TargetErrors {
        TargetErrors {
            capture: CaptureState::Buggrabber,
            state,
            new,
            total: new + 2,
            addons: top.len(),
            top_addons: top
                .iter()
                .map(|(addon, new)| TopAddon {
                    addon: (*addon).to_owned(),
                    folder: Some((*addon).to_owned()),
                    new: *new,
                    count: 9,
                    after_change: None,
                })
                .collect(),
            incomplete: matches!(state, ErrorsState::Partial).then_some(ReadState::Partial),
            ..TargetErrors::uncaptured(CoverageState::Complete)
        }
    }

    #[test]
    fn the_human_line_names_addons_and_counts_only() {
        let line = |e: &TargetErrors| human_line("retail", e, now());
        assert_eq!(
            line(&errors(
                ErrorsState::Ok,
                4,
                &[("DBM", 3), ("WeakAuras", 1), ("Bar", 0)]
            ))
            .as_deref(),
            Some("  errors: 4 new (DBM 3, WeakAuras 1) · omaddons errors -t retail")
        );
        assert_eq!(
            line(&errors(ErrorsState::Ok, 9, &[("DBM", 3), ("WeakAuras", 1)])).as_deref(),
            Some("  errors: 9 new (DBM 3, WeakAuras 1, …) · omaddons errors -t retail"),
            "more new errors than top names"
        );
        assert_eq!(
            line(&errors(ErrorsState::Partial, 2, &[("DBM", 2)])).as_deref(),
            Some("  errors: incomplete read (file was being written) · 2 new so far")
        );
        assert_eq!(
            line(&errors(ErrorsState::Unchecked, 0, &[])).as_deref(),
            Some("  errors: not read yet · omaddons errors -t retail")
        );
        assert_eq!(
            line(&errors(ErrorsState::Stale, 0, &[])).as_deref(),
            Some("  errors: changed since last read · omaddons errors -t retail")
        );
        assert_eq!(line(&errors(ErrorsState::Ok, 0, &[("DBM", 0)])), None);
        let mut cut = errors(ErrorsState::Ok, 0, &[]);
        cut.coverage = CoverageState::Truncated;
        assert_eq!(
            line(&cut).as_deref(),
            Some("  errors: none new · read in part (too many accounts or files)")
        );
        assert_eq!(
            line(&TargetErrors::uncaptured(CoverageState::Complete)),
            None
        );
    }

    // -----------------------------------------------------------------------------------
    // Links (RG-17, design 7.3.2).

    use crate::errors::ledger::DbGeneration;
    use crate::errors::memo::MemoLink;
    use crate::errors::suspect::Relation;
    use crate::model::{ReleaseKey, TxKind};

    /// A link of Foo's cluster in session `session`, of generation `g:1`, graded
    /// `confidence`, fresh for a day.
    fn link(session: u32, confidence: Confidence, tx: &str) -> MemoLink {
        MemoLink {
            cluster: format!("c:{session}"),
            fingerprints: vec![format!("e:{session}")],
            generation: DbGeneration("g:1".to_owned()),
            session,
            tx: tx.parse().unwrap(),
            kind: TxKind::Update,
            at: now() - hours(3),
            addon: "Foo".to_owned(),
            release: ReleaseKey::parse("gh:o/foo@v2", None),
            relation: Relation::Own,
            confidence,
            algorithm: 1,
            fresh_until_at: now() + hours(24),
        }
    }

    const TX1: &str = "01M9Q00000000000000000000A";
    const TX2: &str = "01M9Q00000000000000000000B";

    /// The memo of FAKEACCT's file, projected an hour ago in generation `g:1`, with
    /// Foo's `links` (sessions 4 and 5; 5 is new, baseline 3).
    fn linked(c: &ClientPaths, links: Vec<MemoLink>) -> TargetMemo {
        let mut memo = memo_of(c);
        memo.projected_at = Some(now() - hours(1));
        memo.ledger_revision = Some(7);
        let db = &mut memo.databases[0];
        db.baseline = 3;
        db.generation = Some(DbGeneration("g:1".to_owned()));
        db.addons[0].links = links;
        memo
    }

    fn restore(hours_ago: i64, whole: bool, providers: &[&str]) -> Restored {
        Restored {
            at: now() - hours(hours_ago),
            whole,
            providers: providers.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    #[test]
    fn a_cached_link_shows_only_while_every_test_holds() {
        let (_dir, c) = client(1);
        let none: &[Restored] = &[];
        let earlier = [restore(2, true, &[])];
        let later = [restore(0, true, &[])];
        let elsewhere = [restore(0, false, &["bdUI"])];
        let provider = [restore(0, false, &["!bugGrabber"])];
        let dismissed = |memo: &TargetMemo, session| TargetAck {
            databases: BTreeMap::from([(
                db_key(&memo.databases[0].path),
                Ack {
                    acked_session: session,
                    acked_at: now() - hours(1) + SignedDuration::from_mins(5),
                },
            )]),
        };
        let shown = |memo: &TargetMemo,
                     restores: Option<&[Restored]>,
                     ack: Option<&TargetAck>,
                     at,
                     unchanged: bool| {
            let check = LinkCheck {
                restores,
                ack,
                now: at,
            };
            cached_link(memo, "foo", SourceKind::Addon, &check, &|_| unchanged)
                .map(|b| b.tx.to_string())
        };
        let one = linked(&c, vec![link(5, Confidence::High, TX1)]);
        let mut other_generation = one.clone();
        other_generation.databases[0].generation = Some(DbGeneration("g:2".to_owned()));
        let mut not_in_addon = one.clone();
        not_in_addon.databases[0].addons[0].links = vec![link(6, Confidence::High, TX1)];
        let ack5 = dismissed(&one, 5);
        // (case, memo, restores, dismissal, now, file unchanged, shown)
        type Case<'a> = (
            &'a str,
            &'a TargetMemo,
            Option<&'a [Restored]>,
            Option<&'a TargetAck>,
            Timestamp,
            bool,
            bool,
        );
        let cases: Vec<Case<'_>> = vec![
            (
                "every test holds",
                &one,
                Some(none),
                None,
                now(),
                true,
                true,
            ),
            (
                "another generation",
                &other_generation,
                Some(none),
                None,
                now(),
                true,
                false,
            ),
            (
                "the file changed since",
                &one,
                Some(none),
                None,
                now(),
                false,
                false,
            ),
            (
                "the history cannot say",
                &one,
                None,
                None,
                now(),
                true,
                false,
            ),
            (
                "a restore before the projection",
                &one,
                Some(&earlier),
                None,
                now(),
                true,
                true,
            ),
            (
                "a whole restore since",
                &one,
                Some(&later),
                None,
                now(),
                true,
                false,
            ),
            (
                "its provider's settings restored since",
                &one,
                Some(&provider),
                None,
                now(),
                true,
                false,
            ),
            (
                "another provider's settings restored",
                &one,
                Some(&elsewhere),
                None,
                now(),
                true,
                true,
            ),
            (
                "its session dismissed",
                &one,
                Some(none),
                Some(&ack5),
                now(),
                true,
                false,
            ),
            (
                "a session the addon does not hold",
                &not_in_addon,
                Some(none),
                None,
                now(),
                true,
                false,
            ),
            (
                "past fresh_until_at",
                &one,
                Some(none),
                None,
                now() + hours(25),
                true,
                false,
            ),
        ];
        for (case, memo, restores, ack, at, unchanged, want) in cases {
            assert_eq!(
                shown(memo, restores, ack, at, unchanged).is_some(),
                want,
                "{case}"
            );
        }
        // Dismissing the strongest falls back to the next that holds.
        let mut two = linked(
            &c,
            vec![
                link(4, Confidence::High, TX1),
                link(5, Confidence::Medium, TX2),
            ],
        );
        two.databases[0].baseline = 3;
        let ack4 = dismissed(&two, 4);
        assert_eq!(
            shown(&two, Some(none), None, now(), true).as_deref(),
            Some(TX1),
            "the strongest first"
        );
        assert_eq!(
            shown(&two, Some(none), Some(&ack4), now(), true).as_deref(),
            Some(TX2),
            "its session dismissed, the next that holds"
        );
        assert_eq!(
            shown(&two, Some(none), Some(&ack5), now(), true),
            None,
            "both dismissed: none"
        );
    }

    /// Without the ledger's projection (a dismissal, a run without the evidence lock), a
    /// database keeps its links only while its file is unchanged, read or unread; a
    /// projection replaces them.
    #[test]
    fn the_memo_keeps_a_database_s_links_only_while_its_file_is_unchanged() {
        let foo = GenEntry {
            session: Some(5),
            received: false,
            who: 0,
            counter: 1,
            ago: Some(30),
            meaning: TimeMeaning::Last,
        };
        let db = GenDb {
            session: Some(5),
            baseline: 3,
            written_ago: 1,
            ack: None,
            entries: vec![foo],
        };
        let (obs, _) = observation(&[db]);
        let g1 = Some(DbGeneration("g:1".to_owned()));
        let mut previous = target_memo(&obs, None, None);
        previous.databases[0].generation = g1.clone();
        previous.databases[0].addons[0].links = vec![link(5, Confidence::High, TX1)];
        let links = |memo: &TargetMemo| memo.databases[0].addons[0].links.len();

        let kept = target_memo(&obs, Some(&previous), None);
        assert_eq!(
            (kept.databases[0].generation.clone(), links(&kept)),
            (g1.clone(), 1)
        );

        let mut unread = obs.clone();
        unread.databases[0].state = ReadState::Unreadable;
        let kept_unread = target_memo(&unread, Some(&previous), None);
        assert_eq!(
            links(&kept_unread),
            1,
            "an unreadable file with the same stamp"
        );

        let mut changed = previous.clone();
        changed.databases[0].stamp = "another".to_owned();
        let dropped = target_memo(&obs, Some(&changed), None);
        assert_eq!(
            (dropped.databases[0].generation.clone(), links(&dropped)),
            (None, 0)
        );
        let dropped_unread = target_memo(&unread, Some(&changed), None);
        assert_eq!(
            links(&dropped_unread),
            0,
            "an unreadable file with another stamp"
        );

        let projection = crate::errors::memo::Projection {
            databases: vec![crate::errors::memo::DbProjection {
                generation: Some(DbGeneration("g:9".to_owned())),
                links: BTreeMap::new(),
            }],
        };
        let projected = target_memo(&obs, Some(&previous), Some(&projection));
        assert_eq!(
            (projected.databases[0].generation.clone(), links(&projected)),
            (Some(DbGeneration("g:9".to_owned())), 0)
        );
    }
}
