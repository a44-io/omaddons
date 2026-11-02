//! A detector's budget (M4-D43): a deadline and an entry counter, enforced while it runs,
//! and the bounded walker every walk and listing a detector makes goes through. A detector
//! that runs out stops and returns what it found with an [`Incomplete`] record, so a check
//! cut short is never reported as a clean one. The seven failure modes `status` publishes
//! read computed facts and walk nothing, so they never charge it. Reads only.

use std::collections::BinaryHeap;
use std::ffi::OsString;
use std::fs::{self, FileType, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::finding::{Incomplete, IncompleteLimit};
use super::registry::Tier;
use crate::limits::{DOCTOR_DETECTOR_DEADLINE, DOCTOR_SCAN_MAX_ENTRIES, QUICK_DETECTOR_BUDGET};

/// How much longer an implicit tier deadline is in a debug build: a test seam, neither a
/// safety limit nor a politeness constant (as `failpoint::PAUSE_MAX`). A debug build runs
/// 10 to 50 times slower than the release build the tier contracts are measured on, and
/// without it a check on a home of a few thousand files would stop short in every gate
/// run. An explicit budget ([`Budget::new`]) and the entry cap are never scaled.
const DEBUG_DEADLINE_SCALE: u32 = 20;

/// How many names a listing reads between two looks at the deadline.
const DEADLINE_CHECK_EVERY: usize = 256;

/// A limit was reached: the detector stops and returns what it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exhausted(pub IncompleteLimit);

/// What a detector may still spend: a deadline and an entry cap. Once one is reached,
/// every later charge and check refuses with the first limit hit.
#[derive(Debug)]
pub struct Budget {
    /// When the check must stop, and that deadline in milliseconds as given.
    deadline: Option<(Instant, u64)>,
    max_entries: u64,
    used: u64,
    exhausted: Option<IncompleteLimit>,
}

impl Budget {
    /// `deadline` from now, and at most `max_entries` entries.
    pub fn new(deadline: Duration, max_entries: u64) -> Self {
        let deadline_ms = u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX);
        Self {
            // A deadline past what the clock can name is no deadline.
            deadline: Instant::now()
                .checked_add(deadline)
                .map(|at| (at, deadline_ms)),
            max_entries,
            used: 0,
            exhausted: None,
        }
    }

    /// The budget of one detector of `tier` without `--budget`: `QUICK_DETECTOR_BUDGET`
    /// for the fast tiers, `DOCTOR_DETECTOR_DEADLINE` for the others, scaled in debug
    /// builds ([`DEBUG_DEADLINE_SCALE`]), and `DOCTOR_SCAN_MAX_ENTRIES` entries.
    pub fn for_tier(tier: Tier) -> Self {
        let deadline = match tier {
            // A runtime row has no detector; its arm only keeps the match total.
            Tier::Cached | Tier::Quick | Tier::Runtime => QUICK_DETECTOR_BUDGET,
            Tier::Default | Tier::Deep => DOCTOR_DETECTOR_DEADLINE,
        };
        let deadline = if cfg!(debug_assertions) {
            deadline.saturating_mul(DEBUG_DEADLINE_SCALE)
        } else {
            deadline
        };
        Self::new(deadline, DOCTOR_SCAN_MAX_ENTRIES)
    }

    /// No limit at all.
    pub fn unlimited() -> Self {
        Self {
            deadline: None,
            max_entries: u64::MAX,
            used: 0,
            exhausted: None,
        }
    }

    /// Spends `entries`: refused once they would pass the entry cap, then once the
    /// deadline has passed.
    pub fn charge(&mut self, entries: u64) -> Result<(), Exhausted> {
        if let Some(limit) = self.exhausted {
            return Err(Exhausted(limit));
        }
        if self.used.saturating_add(entries) > self.max_entries {
            return Err(self.exhaust(IncompleteLimit::Entries));
        }
        self.check()?;
        self.used += entries;
        Ok(())
    }

    /// Refused once the deadline has passed, or a limit was reached before.
    pub fn check(&mut self) -> Result<(), Exhausted> {
        if let Some(limit) = self.exhausted {
            return Err(Exhausted(limit));
        }
        if self.expired() {
            return Err(self.exhaust(IncompleteLimit::Deadline));
        }
        Ok(())
    }

    /// Whether the deadline has passed.
    pub fn expired(&self) -> bool {
        self.deadline.is_some_and(|(at, _)| Instant::now() >= at)
    }

    /// The time left until the deadline: zero once it has passed, and `Duration::MAX`
    /// without one. An online check bounds its request with it.
    pub fn remaining(&self) -> Duration {
        self.deadline.map_or(Duration::MAX, |(at, _)| {
            at.saturating_duration_since(Instant::now())
        })
    }

    /// The record of the limit that stopped the check, once one did: the limit, and the
    /// value it was set to.
    pub fn incomplete(&self) -> Option<Incomplete> {
        self.exhausted.map(|limit| Incomplete {
            limit,
            deadline_ms: match limit {
                IncompleteLimit::Deadline => self.deadline.map(|(_, ms)| ms),
                _ => None,
            },
            max_entries: (limit == IncompleteLimit::Entries).then_some(self.max_entries),
        })
    }

    /// Entries still to spend.
    fn entries_left(&self) -> u64 {
        self.max_entries.saturating_sub(self.used)
    }

    /// Records `limit` unless one was recorded first.
    fn exhaust(&mut self, limit: IncompleteLimit) -> Exhausted {
        Exhausted(*self.exhausted.get_or_insert(limit))
    }
}

// ---------------------------------------------------------------------------------------
// The bounded walker.

/// What an entry is, its final component never followed. Ordered only so names can be
/// kept sorted with their kinds (one directory never lists a name twice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

impl EntryKind {
    fn of(file_type: FileType) -> Self {
        if file_type.is_symlink() {
            Self::Symlink
        } else if file_type.is_dir() {
            Self::Dir
        } else if file_type.is_file() {
            Self::File
        } else {
            Self::Other
        }
    }
}

/// One entry a walk visits; the root's children are at depth 1.
#[derive(Debug)]
pub struct WalkEntry<'a> {
    pub path: &'a Path,
    pub depth: usize,
    pub kind: EntryKind,
    pub meta: &'a Metadata,
}

/// What the walk does after a visit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkStep {
    Continue,
    /// Visit no entry below this directory.
    SkipChildren,
    /// End the walk.
    Stop,
}

/// How a walk ended. `complete` is false when the budget ran out (the budget says which
/// limit) or the visitor stopped it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkResult {
    pub complete: bool,
    /// Directories that could not be read, and entries that could not be examined, in
    /// visit order: each an entry the walk charged, or the root itself.
    pub unreadable: Vec<PathBuf>,
}

/// Walks the tree below `root`, at most `max_depth` levels deep, charging `budget` one
/// entry per entry before `visit` sees it. Each directory's names are visited in byte
/// order, so two walks of one tree visit the same entries in the same order. A symlink
/// is reported, never entered. A directory that cannot be read is recorded and skipped.
pub fn walk(
    root: &Path,
    max_depth: usize,
    budget: &mut Budget,
    mut visit: impl FnMut(&WalkEntry<'_>) -> WalkStep,
) -> WalkResult {
    let mut unreadable = Vec::new();
    let complete = walk_dir(root, 1, max_depth, budget, &mut visit, &mut unreadable);
    WalkResult {
        complete,
        unreadable,
    }
}

/// Visits `dir`'s entries at `depth`; false once the walk must end.
fn walk_dir(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    budget: &mut Budget,
    visit: &mut impl FnMut(&WalkEntry<'_>) -> WalkStep,
    unreadable: &mut Vec<PathBuf>,
) -> bool {
    let Ok((names, read_all)) = read_names(dir, budget) else {
        unreadable.push(dir.to_path_buf());
        return true;
    };
    for (name, _) in names {
        if budget.charge(1).is_err() {
            return false;
        }
        let path = dir.join(&name);
        let Ok(meta) = fs::symlink_metadata(&path) else {
            unreadable.push(path);
            continue;
        };
        let kind = EntryKind::of(meta.file_type());
        let entry = WalkEntry {
            path: &path,
            depth,
            kind,
            meta: &meta,
        };
        match visit(&entry) {
            WalkStep::Stop => return false,
            WalkStep::SkipChildren => {}
            WalkStep::Continue => {
                if kind == EntryKind::Dir
                    && depth < max_depth
                    && !walk_dir(&path, depth + 1, max_depth, budget, visit, unreadable)
                {
                    return false;
                }
            }
        }
    }
    // Names left unread: the deadline passed while reading, or there were more than the
    // budget could visit (the loop above then ran out first).
    read_all
}

/// One directory's entries, in byte order, as [`list_dir`] gives them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<(OsString, EntryKind)>,
    /// False when the budget ran out before every entry was listed.
    pub complete: bool,
}

/// [`walk`]'s one-level form: `dir`'s entries in byte order, one entry charged each. An
/// error only when `dir` itself cannot be read.
pub fn list_dir(dir: &Path, budget: &mut Budget) -> io::Result<Listing> {
    let (names, read_all) = read_names(dir, budget)?;
    let mut entries = Vec::with_capacity(names.len());
    for (name, kind) in names {
        if budget.charge(1).is_err() {
            return Ok(Listing {
                entries,
                complete: false,
            });
        }
        entries.push((name, kind));
    }
    Ok(Listing {
        entries,
        complete: read_all,
    })
}

/// The first of `dir`'s names in byte order, with their kinds, and whether they are all
/// of them. Every name is read, so the first ones are the same whatever order the
/// filesystem lists them in, but only one more than the budget can still visit is kept,
/// so memory stays within the budget; reading stops at the deadline.
fn read_names(dir: &Path, budget: &mut Budget) -> io::Result<(Vec<(OsString, EntryKind)>, bool)> {
    let keep = usize::try_from(budget.entries_left().saturating_add(1)).unwrap_or(usize::MAX);
    // A max-heap of the smallest names so far: the largest leaves first.
    let mut first = BinaryHeap::new();
    let mut all = true;
    for (read, entry) in fs::read_dir(dir)?.enumerate() {
        if read % DEADLINE_CHECK_EVERY == 0 && budget.check().is_err() {
            return Ok((first.into_sorted_vec(), false));
        }
        let entry = entry?;
        let kind = entry.file_type().map_or(EntryKind::Other, EntryKind::of);
        first.push((entry.file_name(), kind));
        if first.len() > keep {
            first.pop();
            all = false;
        }
    }
    Ok((first.into_sorted_vec(), all))
}

// ---------------------------------------------------------------------------------------
// `--budget`.

const BUDGET_FORMS: &str = "--budget takes a duration in ms or s, such as 500ms or 2s";

/// `--budget`'s value: a positive whole number of milliseconds (`500ms`) or seconds
/// (`2s`).
pub fn parse_budget(s: &str) -> Result<Duration, String> {
    let (digits, millis) = match s.strip_suffix("ms") {
        Some(digits) => (digits, true),
        None => match s.strip_suffix('s') {
            Some(digits) => (digits, false),
            None => return Err(BUDGET_FORMS.to_owned()),
        },
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(BUDGET_FORMS.to_owned());
    }
    match digits.parse::<u64>() {
        Ok(0) | Err(_) => Err(BUDGET_FORMS.to_owned()),
        Ok(n) if millis => Ok(Duration::from_millis(n)),
        Ok(n) => Ok(Duration::from_secs(n)),
    }
}

/// Each detector's share of `budget`, by its declared cost: `budget × cost / Σcost`, at
/// least 1 ms; the whole budget for each when every cost is 0.
pub fn shares(budget: Duration, costs: &[u32]) -> Vec<Duration> {
    let total: u128 = costs.iter().map(|&cost| u128::from(cost)).sum();
    if total == 0 {
        return vec![budget; costs.len()];
    }
    costs
        .iter()
        .map(|&cost| {
            let nanos = budget.as_nanos() * u128::from(cost) / total;
            Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
                .max(Duration::from_millis(1))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::TempDir;

    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    /// A tempdir holding `entries`: a name ending in `/` is a directory, `name -> target`
    /// a symlink, anything else an empty file.
    fn tree(entries: &[&str]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for entry in entries {
            let path = |name: &str| dir.path().join(name);
            if let Some((link, target)) = entry.split_once(" -> ") {
                symlink(target, path(link)).unwrap();
            } else if let Some(name) = entry.strip_suffix('/') {
                fs::create_dir_all(path(name)).unwrap();
            } else {
                fs::write(path(entry), b"").unwrap();
            }
        }
        dir
    }

    /// Every entry a walk visits, relative to `root`, with its kind.
    fn visited(
        root: &Path,
        max_depth: usize,
        budget: &mut Budget,
        step: impl Fn(&str) -> WalkStep,
    ) -> (Vec<(String, EntryKind)>, WalkResult) {
        let mut seen = Vec::new();
        let result = walk(root, max_depth, budget, |entry| {
            let name = entry.path.strip_prefix(root).unwrap().display().to_string();
            let next = step(&name);
            seen.push((name, entry.kind));
            next
        });
        (seen, result)
    }

    #[test]
    fn charge_stops_at_max_entries() {
        let cases: [(&str, u64, &[u64], usize); 3] = [
            ("max 1: the second charge", 1, &[1, 1], 1),
            ("max 3: the fourth charge of 1", 3, &[1, 1, 1, 1], 3),
            ("max 4: a charge of 5 at once", 4, &[5], 0),
        ];
        for (case, max, charges, refused_at) in cases {
            let mut budget = Budget::new(HOUR, max);
            for (at, &entries) in charges.iter().enumerate() {
                let charged = budget.charge(entries);
                let expected = if at < refused_at {
                    Ok(())
                } else {
                    Err(Exhausted(IncompleteLimit::Entries))
                };
                assert_eq!(charged, expected, "{case}: charge {at} of {entries}");
            }
            // Refused for good after the first refusal.
            assert_eq!(
                (budget.charge(0), budget.check()),
                (
                    Err(Exhausted(IncompleteLimit::Entries)),
                    Err(Exhausted(IncompleteLimit::Entries))
                ),
                "{case}: still refused"
            );
        }
    }

    #[test]
    fn check_stops_at_the_deadline() {
        let mut budget = Budget::new(Duration::from_millis(1), 10);
        std::thread::sleep(Duration::from_millis(5));
        assert!(budget.expired());
        assert_eq!(budget.check(), Err(Exhausted(IncompleteLimit::Deadline)));
        assert_eq!(
            budget.charge(1),
            Err(Exhausted(IncompleteLimit::Deadline)),
            "the first limit hit stays the one reported"
        );
        let mut unlimited = Budget::unlimited();
        assert_eq!(
            (unlimited.charge(1_000_000), unlimited.check()),
            (Ok(()), Ok(()))
        );
        assert!(!unlimited.expired());
    }

    #[test]
    fn remaining_counts_down_to_zero() {
        let budget = Budget::new(Duration::from_millis(200), 10);
        let first = budget.remaining();
        assert!(
            first > Duration::ZERO && first <= Duration::from_millis(200),
            "{first:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
        let later = budget.remaining();
        assert!(later < first, "{first:?} then {later:?}");
        let spent = Budget::new(Duration::from_millis(1), 10);
        std::thread::sleep(Duration::from_millis(5));
        assert!(spent.expired());
        assert_eq!(spent.remaining(), Duration::ZERO, "zero once expired");
        assert_eq!(
            Budget::unlimited().remaining(),
            Duration::MAX,
            "no deadline"
        );
        let mut entries = Budget::new(HOUR, 1);
        let _ = entries.charge(2);
        assert!(
            entries.remaining() > Duration::from_secs(3_500),
            "the entry cap leaves the time as it is"
        );
    }

    /// An online check's unusable answer has no limit value to report: neither a
    /// deadline nor an entry cap stopped it.
    #[test]
    fn unanswered_detected_has_no_limit_values() {
        let detected = crate::doctor::finding::Detected::unanswered(Vec::new());
        assert!(detected.found.is_empty());
        assert_eq!(
            detected.incomplete,
            Some(Incomplete {
                limit: IncompleteLimit::Unanswered,
                deadline_ms: None,
                max_entries: None,
            })
        );
        assert_eq!(
            serde_json::to_value(detected.incomplete).unwrap(),
            serde_json::json!({"limit": "unanswered", "deadline_ms": null, "max_entries": null})
        );
    }

    #[test]
    fn incomplete_reports_only_fixed_values() {
        let mut deadline = Budget::new(Duration::from_millis(1), 3);
        std::thread::sleep(Duration::from_millis(5));
        let _ = deadline.check();
        let mut entries = Budget::new(HOUR, 3);
        let _ = entries.charge(4);
        let mut fine = Budget::new(HOUR, 3);
        fine.charge(3).unwrap();
        let cases = [
            (
                "deadline",
                deadline.incomplete(),
                Some(Incomplete {
                    limit: IncompleteLimit::Deadline,
                    deadline_ms: Some(1),
                    max_entries: None,
                }),
            ),
            (
                "entries",
                entries.incomplete(),
                Some(Incomplete {
                    limit: IncompleteLimit::Entries,
                    deadline_ms: None,
                    max_entries: Some(3),
                }),
            ),
            ("never exhausted", fine.incomplete(), None),
        ];
        for (case, got, expected) in cases {
            assert_eq!(got, expected, "{case}");
        }
        assert_eq!(
            serde_json::to_value(entries.incomplete()).unwrap(),
            serde_json::json!({"limit": "entries", "deadline_ms": null, "max_entries": 3})
        );
    }

    #[test]
    fn walk_is_sorted_and_never_follows_symlinks() {
        let dir = tree(&["b/", "a/", "a/z", "a/y", "link -> a"]);
        let (seen, result) = visited(dir.path(), 8, &mut Budget::unlimited(), |_| {
            WalkStep::Continue
        });
        assert_eq!(
            seen,
            [
                ("a".to_owned(), EntryKind::Dir),
                ("a/y".to_owned(), EntryKind::File),
                ("a/z".to_owned(), EntryKind::File),
                ("b".to_owned(), EntryKind::Dir),
                ("link".to_owned(), EntryKind::Symlink),
            ],
            "byte order, and the link not entered"
        );
        assert!(
            result.complete && result.unreadable.is_empty(),
            "{result:?}"
        );
    }

    #[test]
    fn walk_stops_at_the_cap() {
        let files: Vec<String> = (0..10).map(|n| format!("f{n}")).collect();
        let names: Vec<&str> = files.iter().map(String::as_str).collect();
        let dir = tree(&names);
        let mut budget = Budget::new(HOUR, 4);
        let (seen, result) = visited(dir.path(), 8, &mut budget, |_| WalkStep::Continue);
        let names: Vec<&str> = seen.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["f0", "f1", "f2", "f3"]);
        assert!(!result.complete, "{result:?}");
        assert_eq!(
            budget.incomplete().map(|i| i.limit),
            Some(IncompleteLimit::Entries)
        );
    }

    #[test]
    fn walk_respects_max_depth() {
        let dir = tree(&["a/", "a/b/", "a/b/c/", "a/b/c/deep", "top"]);
        for (max_depth, expected) in [
            (1, &["a", "top"][..]),
            (2, &["a", "a/b", "top"][..]),
            (3, &["a", "a/b", "a/b/c", "top"][..]),
            (4, &["a", "a/b", "a/b/c", "a/b/c/deep", "top"][..]),
        ] {
            let (seen, result) = visited(dir.path(), max_depth, &mut Budget::unlimited(), |_| {
                WalkStep::Continue
            });
            let names: Vec<&str> = seen.iter().map(|(name, _)| name.as_str()).collect();
            assert_eq!(names, expected, "max_depth {max_depth}");
            assert!(result.complete, "max_depth {max_depth}");
        }
    }

    #[test]
    fn walk_skip_children_and_stop() {
        let dir = tree(&["a/", "a/inner", "b/", "b/inner", "c"]);
        let (seen, result) = visited(dir.path(), 8, &mut Budget::unlimited(), |name| {
            if name == "a" {
                WalkStep::SkipChildren
            } else {
                WalkStep::Continue
            }
        });
        let names: Vec<&str> = seen.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["a", "b", "b/inner", "c"], "skip: a's children");
        assert!(result.complete);

        let mut budget = Budget::unlimited();
        let (seen, result) = visited(dir.path(), 8, &mut budget, |name| {
            if name == "b" {
                WalkStep::Stop
            } else {
                WalkStep::Continue
            }
        });
        let names: Vec<&str> = seen.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["a", "a/inner", "b"], "stop: at b");
        assert!(!result.complete, "the visitor stopped it");
        assert_eq!(budget.incomplete(), None, "not the budget's doing");
    }

    #[test]
    fn walk_records_unreadable_directories() {
        if rustix::process::geteuid().is_root() {
            return; // root reads a mode-000 directory, so nothing is unreadable
        }
        let dir = tree(&["a/", "a/x", "locked/", "locked/hidden", "z"]);
        let locked = dir.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let (seen, result) = visited(dir.path(), 8, &mut Budget::unlimited(), |_| {
            WalkStep::Continue
        });
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let names: Vec<&str> = seen.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["a", "a/x", "locked", "z"], "its siblings visited");
        assert_eq!(result.unreadable, [locked]);
        assert!(
            result.complete,
            "an unreadable directory does not stop the walk"
        );
    }

    #[test]
    fn list_dir_is_one_level() {
        let dir = tree(&["b/", "b/inner", "a", "link -> b"]);
        let listing = list_dir(dir.path(), &mut Budget::unlimited()).unwrap();
        assert_eq!(
            listing,
            Listing {
                entries: vec![
                    ("a".into(), EntryKind::File),
                    ("b".into(), EntryKind::Dir),
                    ("link".into(), EntryKind::Symlink),
                ],
                complete: true,
            }
        );
        let missing = list_dir(&dir.path().join("nowhere"), &mut Budget::unlimited());
        assert!(missing.is_err(), "the directory itself cannot be read");
    }

    #[test]
    fn list_dir_charges_each_entry() {
        let dir = tree(&["a", "b", "c", "d"]);
        let mut budget = Budget::new(HOUR, 2);
        let listing = list_dir(dir.path(), &mut budget).unwrap();
        let names: Vec<&OsString> = listing.entries.iter().map(|(name, _)| name).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(!listing.complete);
        assert_eq!(
            budget.incomplete().map(|i| i.limit),
            Some(IncompleteLimit::Entries)
        );
        let mut roomy = Budget::new(HOUR, 4);
        let all = list_dir(dir.path(), &mut roomy).unwrap();
        assert!(all.complete && all.entries.len() == 4, "{all:?}");
        assert_eq!(roomy.incomplete(), None, "exactly the cap is complete");
    }

    #[test]
    fn parse_budget_cases() {
        let accepted = [
            ("500ms", Duration::from_millis(500)),
            ("2s", Duration::from_secs(2)),
            ("1ms", Duration::from_millis(1)),
        ];
        for (input, expected) in accepted {
            assert_eq!(parse_budget(input), Ok(expected), "{input}");
        }
        for input in [
            "0ms", "0s", "500", "1.5s", "-1s", "ms", "s", "1h", "500 ms", "", " 2s",
        ] {
            let refused = parse_budget(input);
            assert!(
                refused
                    .as_ref()
                    .is_err_and(|message| message.contains("500ms or 2s")),
                "{input:?}: {refused:?}"
            );
        }
    }

    #[test]
    fn tier_defaults() {
        let scale = if cfg!(debug_assertions) { 20 } else { 1 };
        let deadline_ms = |tier| {
            let budget = Budget::for_tier(tier);
            (budget.deadline.map(|(_, ms)| ms), budget.max_entries)
        };
        for (tier, ms) in [
            (Tier::Cached, 5),
            (Tier::Quick, 5),
            (Tier::Default, 2_000),
            (Tier::Deep, 2_000),
        ] {
            assert_eq!(
                deadline_ms(tier),
                (Some(ms * scale), DOCTOR_SCAN_MAX_ENTRIES),
                "{tier:?}"
            );
        }
        // An explicit budget is exact.
        let explicit = Budget::new(Duration::from_millis(5), 7);
        assert_eq!(
            (explicit.deadline.map(|(_, ms)| ms), explicit.max_entries),
            (Some(5), 7)
        );
    }

    #[test]
    fn shares_cases() {
        let ms = Duration::from_millis;
        let cases = [
            ("by cost", ms(400), vec![1, 3], vec![ms(100), ms(300)]),
            ("all free", ms(400), vec![0, 0], vec![ms(400), ms(400)]),
            ("none", ms(400), vec![], vec![]),
        ];
        for (case, budget, costs, expected) in cases {
            assert_eq!(shares(budget, &costs), expected, "{case}: {costs:?}");
        }
        let floor = shares(ms(10), &[1, 1000]);
        assert_eq!(floor[0], ms(1), "at least 1 ms");
        assert!(
            floor[1] > Duration::from_micros(9_900) && floor[1] < ms(10),
            "{floor:?}"
        );
    }
}
