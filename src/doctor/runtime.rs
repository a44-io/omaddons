//! The doctor's runtime (E3; M4-D32, M4-D33, M4-D43, §6.12): a run's id and directory,
//! every selected check run isolated from panics and within its budget, and the run's
//! record. A run is `STARTING` (its directory staged and published), `DIAGNOSING`, then
//! `DONE_OK` or `DONE_FINDINGS` (its report written, `latest` moved, its record files
//! hardened).
//!
//! Every write goes through one logged untracked fsx session, so `runtime.jsonl` names
//! each. The runtime writes nothing outside `<state>/doctor/`, takes no lock, makes no
//! request, never prunes and never waits for the clock. Checks take `&DoctorView`, which
//! has no write methods.

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;
use ulid::Ulid;

use super::budget::{self, Budget};
use super::finding::{Confidence, Detected, Evidence, Finding, Incomplete, Observation, Subject};
use super::registry::{self, DetectFn, FailureMode, Tier, ids};
use super::selection::{Selection, Skipped, partition, target_skips, visible_failures};
use super::view::{DoctorView, merge_findings};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{self, FsxError, Op, RenameMode, ScopeTable, WriteSession, Writer};
use crate::limits::{DOCTOR_SCAN_MAX_ENTRIES, RUN_ID_RETRY};
use crate::output::DoctorFailure;
use crate::state::paths::Paths;

// The isolation of a panicking check needs unwinding: a profile that aborts on panic
// would end the whole run at the first one.
const _: () = assert!(cfg!(panic = "unwind"));

/// The runtime's own log, in the run directory: one actions line per write it made.
pub const RUNTIME_LOG: &str = "runtime.jsonl";
/// The run's report, in the run directory: the envelope's record form.
pub const REPORT_FILE: &str = "report.json";

/// A doctor run's id (M4-D33): its UTC start time to the second, `__`, and a fresh ULID,
/// as `2026-10-30T18-03-40Z__01M8S…`. Ordered by start time, then ULID. Not
/// [`crate::model::RunId`], which names one invocation's transactions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct DoctorRunId(String);

/// The time part's shape, `d` for a digit.
const TIME_SHAPE: &str = "dddd-dd-ddTdd-dd-ddZ";

impl DoctorRunId {
    pub fn new(at: Timestamp, ulid: Ulid) -> Self {
        Self(format!("{}__{ulid}", at.strftime("%Y-%m-%dT%H-%M-%SZ")))
    }

    /// Exactly that shape: the time with dashes, `__`, and an uppercase ULID.
    pub fn parse(s: &str) -> Option<Self> {
        let (time, ulid) = s.split_once("__")?;
        let time_ok = time.len() == TIME_SHAPE.len()
            && TIME_SHAPE
                .bytes()
                .zip(time.bytes())
                .all(|(want, have)| match want {
                    b'd' => have.is_ascii_digit(),
                    _ => want == have,
                });
        let ulid_ok = Ulid::from_string(ulid).is_ok_and(|parsed| parsed.to_string() == ulid);
        (time_ok && ulid_ok).then(|| Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a run's checks are budgeted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budgets {
    /// Each check its tier's ([`Budget::for_tier`]).
    TierDefaults,
    /// `--budget`: the run's whole time, shared by the checks' declared costs, each at
    /// most what is left of the run's; the entry cap as by default.
    Run(Duration),
    /// Every check exactly this budget.
    Each {
        deadline: Duration,
        max_entries: u64,
    },
}

/// What a run's checks found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    /// The view's failures, the checks' findings and the runtime's own, merged and
    /// ordered, within the run's `-t` scope.
    pub findings: Vec<Finding>,
    pub skipped: Vec<Skipped>,
    /// The checks a limit cut short.
    pub incomplete: u32,
    /// The checks that started, crashed ones included.
    pub checks_run: u32,
    pub started_at: Timestamp,
    pub finished_at: Timestamp,
    pub duration_ms: u64,
}

/// What became of `latest` when a run finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Latest {
    /// It now names this run.
    Moved,
    /// It names a newer run, which stays the newest.
    Kept,
    /// Something not a symlink is there; it was left alone.
    NotALink,
}

/// A finished run: its directory, and what became of `latest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub dir: PathBuf,
    pub latest: Latest,
}

/// One doctor run, from its published directory to its hardened record.
pub struct Runtime<'a> {
    paths: Paths,
    session: WriteSession<'a>,
    id: DoctorRunId,
}

impl<'a> Runtime<'a> {
    /// `STARTING`: the run's id from `at` and a fresh ULID, its directory staged at 0700
    /// and published. A failure is `cannot_create` (exit 73).
    pub fn start(
        paths: &Paths,
        scopes: &'a ScopeTable,
        at: Timestamp,
    ) -> std::result::Result<Self, DoctorFailure> {
        Self::start_with(paths, scopes, at, &mut Ulid::generate)
    }

    /// [`Runtime::start`] drawing ULIDs from `next_ulid`. A name another run took draws
    /// a fresh ULID, at most [`RUN_ID_RETRY`] times; nothing waits for the clock.
    pub fn start_with(
        paths: &Paths,
        scopes: &'a ScopeTable,
        at: Timestamp,
        next_ulid: &mut dyn FnMut() -> Ulid,
    ) -> std::result::Result<Self, DoctorFailure> {
        let mut retries = 0;
        let mut id = DoctorRunId::new(at, next_ulid());
        // A stage name already there (only a repeated ULID makes one) is a collision
        // too, checked before the session would log into it.
        while paths.doctor_run_stage(&id).exists() {
            retries += 1;
            if retries > RUN_ID_RETRY {
                return Err(cannot_create(
                    &paths.doctor_run_stage(&id),
                    "its name is taken",
                ));
            }
            id = DoctorRunId::new(at, next_ulid());
        }
        let stage = paths.doctor_run_stage(&id);
        let mut session = WriteSession::untracked_logged(
            Writer::Doctor,
            scopes,
            "doctor runtime",
            stage.join(RUNTIME_LOG),
        );
        let fail = |path: &Path, err: FsxError| cannot_create(path, &err.to_string());
        let runs = paths.doctor_runs_dir();
        fsx::ensure_dir_all(&mut session, &runs).map_err(|e| fail(&runs, e))?;
        fsx::mutate(&mut session, &stage, Op::CreateDir).map_err(|e| fail(&stage, e))?;
        fsx::mutate(&mut session, &stage, Op::Chmod { mode: 0o700 })
            .map_err(|e| fail(&stage, e))?;
        fsx::failpoint::hit("doctor.run.staged")
            .map_err(|e| cannot_create(&stage, &e.to_string()))?;
        loop {
            let run = paths.doctor_run(&id);
            let published = fsx::mutate(
                &mut session,
                &stage,
                Op::Rename {
                    to: &run,
                    mode: RenameMode::NoReplace,
                },
            );
            match published {
                Ok(_) => break,
                Err(FsxError::Precondition(_)) if run.exists() && retries < RUN_ID_RETRY => {
                    // The stage keeps its name; only the published one changes.
                    retries += 1;
                    id = DoctorRunId::new(at, next_ulid());
                }
                Err(FsxError::Precondition(_)) if run.exists() => {
                    return Err(cannot_create(&run, "its name is taken"));
                }
                Err(err) => return Err(fail(&run, err)),
            }
        }
        let run = paths.doctor_run(&id);
        fsx::failpoint::hit("doctor.run.published")
            .map_err(|e| cannot_create(&run, &e.to_string()))?;
        Ok(Self {
            paths: paths.clone(),
            session,
            id,
        })
    }

    pub fn id(&self) -> &DoctorRunId {
        &self.id
    }

    /// The run's directory, published.
    pub fn dir(&self) -> PathBuf {
        self.paths.doctor_run(&self.id)
    }

    /// `DIAGNOSING`: every row of `registry` the selection includes, of `tiers`, whose
    /// layers are available, in registry order, each isolated from panics and held to its
    /// budget. A check that panics is a `detector-crashed` finding and one a limit cut
    /// short a `check-incomplete` finding; the run goes on either way. Fails only when
    /// `-t` cannot be resolved.
    pub fn diagnose(
        &self,
        registry: &[FailureMode],
        view: &DoctorView,
        selection: &Selection,
        tiers: &[Tier],
        budgets: Budgets,
    ) -> Result<Diagnosis> {
        let started_at = crate::clock::now();
        let clock = Instant::now();
        // Resolving the scope and the layers each row requires loads them, so a lazy
        // layer load never counts against a check's deadline.
        let part = partition(view, selection, registry, tiers)?;
        let shares = match budgets {
            Budgets::Run(total) => {
                let costs: Vec<u32> = part.run.iter().map(|row| row.cost_ms).collect();
                budget::shares(total, &costs)
            }
            Budgets::TierDefaults | Budgets::Each { .. } => Vec::new(),
        };
        let mut detected = Vec::new();
        let mut incomplete = 0;
        let mut checks_run = 0;
        for (at, row) in part.run.iter().enumerate() {
            let Some(detect) = row.detect else {
                continue;
            };
            checks_run += 1;
            let mut budget = match budgets {
                Budgets::TierDefaults => Budget::for_tier(row.tier),
                Budgets::Run(total) => {
                    let left = total.saturating_sub(clock.elapsed());
                    let share = shares.get(at).copied().unwrap_or(left);
                    Budget::new(share.min(left), DOCTOR_SCAN_MAX_ENTRIES)
                }
                Budgets::Each {
                    deadline,
                    max_entries,
                } => Budget::new(deadline, max_entries),
            };
            match run_check(row, detect, view, selection, &mut budget) {
                Checked::Ran { findings, cut } => {
                    detected.extend(findings);
                    if let Some(cut) = cut {
                        incomplete += 1;
                        detected.extend(runtime_finding(ids::CHECK_INCOMPLETE, |subject| {
                            incomplete_observation(subject, row, cut)
                        }));
                    }
                }
                Checked::Crashed(location) => detected.extend(crashed(row, location)),
            }
        }
        let mut skipped = part.skipped.clone();
        skipped.extend(target_skips(view, &part));
        let failures = visible_failures(view, selection, &part, &skipped);
        let findings = merge_findings(failures, detected)
            .into_iter()
            .filter(|f| part.scope.includes(f.target.as_ref()))
            .collect();
        Ok(Diagnosis {
            findings,
            skipped,
            incomplete,
            checks_run,
            started_at,
            finished_at: crate::clock::now(),
            duration_ms: u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
    }

    /// The terminal writes, in order: `report.json` (0600) holding `record`, `latest`
    /// moved forward to this run, `report.json` then `runtime.jsonl` made 0400 (the log's
    /// own line goes out through its open descriptor). The run directory stays 0700.
    /// A failure is `io` (exit 74), naming the path; the caller keeps its report.
    pub fn finish(mut self, record: &[u8]) -> std::result::Result<Finished, DoctorFailure> {
        let dir = self.dir();
        let report = dir.join(REPORT_FILE);
        let io = |path: &Path, err: FsxError| {
            DoctorFailure::from(
                Error::new(
                    ErrorCode::Io,
                    format!("could not record the doctor's run: {err}"),
                    Suggestion::text(
                        "check that the state directory is writable and has space, then run the doctor again",
                    ),
                )
                .details(serde_json::json!({ "path": path.display().to_string() })),
            )
        };
        let mut record_line = record.to_vec();
        if !record_line.ends_with(b"\n") {
            record_line.push(b'\n');
        }
        fsx::mutate(
            &mut self.session,
            &report,
            Op::WriteFile {
                content: fsx::Content::Bytes(&record_line),
                mode: Some(0o600),
            },
        )
        .map_err(|e| io(&report, e))?;
        let latest = self
            .move_latest()
            .map_err(|e| io(&self.paths.doctor_latest(), e))?;
        fsx::mutate(&mut self.session, &report, Op::Chmod { mode: 0o400 })
            .map_err(|e| io(&report, e))?;
        let log = dir.join(RUNTIME_LOG);
        fsx::mutate(&mut self.session, &log, Op::Chmod { mode: 0o400 }).map_err(|e| io(&log, e))?;
        Ok(Finished { dir, latest })
    }

    /// Points `latest` at this run when it is missing, dangling, points outside `runs/`
    /// or names an older run, as a relative link (`runs/<id>`), so a moved state
    /// directory keeps it and it names no home path. A newer run keeps it: a slow run
    /// finishing late never moves it back.
    fn move_latest(&mut self) -> std::result::Result<Latest, FsxError> {
        let latest = self.paths.doctor_latest();
        let current = match fs::symlink_metadata(&latest) {
            Err(_) => None,
            Ok(meta) if meta.file_type().is_symlink() => fs::read_link(&latest).ok(),
            Ok(_) => return Ok(Latest::NotALink),
        };
        let newer = current.as_deref().is_some_and(|link| {
            let names_newer = link
                .strip_prefix("runs")
                .ok()
                .and_then(Path::to_str)
                .and_then(DoctorRunId::parse)
                .is_some_and(|other| other > self.id);
            // `exists` follows the link: false when it dangles.
            names_newer && latest.exists()
        });
        if newer {
            return Ok(Latest::Kept);
        }
        let target = Path::new("runs").join(self.id.as_str());
        fsx::mutate(
            &mut self.session,
            &latest,
            Op::SymlinkAtomic { target: &target },
        )?;
        Ok(Latest::Moved)
    }
}

/// What one check gave.
#[derive(Debug)]
pub enum Checked {
    /// It ran: its findings, and the limit that stopped it short, if one did.
    Ran {
        findings: Vec<Finding>,
        cut: Option<Incomplete>,
    },
    /// It panicked, at the place in omaddons' source when the panic hook saw it, or a
    /// failpoint failed it: it did not run.
    Crashed(Option<String>),
}

/// `row`'s check, `detect`, isolated from panics and held to `budget`. A budget spent
/// before it starts (a slow failpoint, or a run's `--budget` used up) leaves it never
/// started: incomplete, never a pass.
pub fn run_check(
    row: &FailureMode,
    detect: DetectFn,
    view: &DoctorView,
    selection: &Selection,
    budget: &mut Budget,
) -> Checked {
    let ran = catch_unwind(AssertUnwindSafe(|| -> std::io::Result<Detected> {
        fsx::failpoint::hit("doctor.detector.panic")?;
        if budget.check().is_err() {
            return Ok(Detected::cut_short(Vec::new(), budget));
        }
        Ok(detect(view, selection, budget))
    }));
    match ran {
        Ok(Ok(found)) => Checked::Ran {
            findings: found
                .found
                .into_iter()
                .map(|obs| row.finding(obs))
                .collect(),
            cut: found.incomplete,
        },
        Ok(Err(_)) => Checked::Crashed(None),
        Err(_) => Checked::Crashed(crate::cli::take_caught_panic()),
    }
}

/// The run's directory could not be made: exit 73, naming `path`.
fn cannot_create(path: &Path, why: &str) -> DoctorFailure {
    DoctorFailure::cannot_create(
        Error::new(
            ErrorCode::Io,
            format!("could not create the doctor's run directory: {why}"),
            Suggestion::text(
                "check that the state directory is writable, then run the doctor again",
            ),
        )
        .details(serde_json::json!({ "path": path.display().to_string() })),
    )
}

/// The runtime's own finding of the row `id`, about the check `subject` names. Its rows
/// are in the registry (an invariant test holds it), so this is never empty.
fn runtime_finding(
    id: &'static str,
    observe: impl FnOnce(Subject) -> Observation,
) -> Option<Finding> {
    registry::row(id).map(|row| row.finding(observe(Subject::machine())))
}

/// `detector-crashed` for `row`, at `location` when the panic hook saw it.
fn crashed(row: &FailureMode, location: Option<String>) -> Option<Finding> {
    runtime_finding(ids::DETECTOR_CRASHED, |subject| Observation {
        subject: subject.detector(row.id),
        title: format!("The check {} stopped with an internal error", row.id),
        confidence: Confidence::High,
        addon: None,
        folder: None,
        evidence: Evidence {
            detector: Some(row.id),
            location,
            ..Evidence::default()
        },
    })
}

/// `check-incomplete` for `row`, cut short by `cut`: the limit and the value it was set
/// to, nothing that varies between two runs of one state.
fn incomplete_observation(subject: Subject, row: &FailureMode, cut: Incomplete) -> Observation {
    let limit = cut.limit.as_str();
    Observation {
        subject: subject.detector(row.id),
        title: format!("The check {} stopped at its {limit} limit", row.id),
        confidence: Confidence::High,
        addon: None,
        folder: None,
        evidence: Evidence {
            detector: Some(row.id),
            reason: Some(limit),
            deadline_ms: cut.deadline_ms,
            max_entries: cut.max_entries,
            ..Evidence::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;
    use crate::doctor::finding::{Remediation, Severity, Subsystem};
    use crate::doctor::registry::{Layer, Manual};
    use crate::doctor::view::ViewOptions;
    use crate::fsx::ActionRecord;

    const AT: &str = "2026-10-30T18:03:40Z";
    const A: &str = "fm-state_files-test-a";
    const B: &str = "fm-state_files-test-b";
    const C: &str = "fm-state_files-test-c";

    /// A home under a tempdir with its state directory, as a doctor run finds it.
    struct Home {
        _dir: TempDir,
        paths: Paths,
        scopes: ScopeTable,
    }

    fn home() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let home = root.join("home");
        let paths = Paths::with_roots(
            &home,
            home.join(".config"),
            home.join(".local/state"),
            home.join(".cache"),
        );
        fs::create_dir_all(paths.state_dir()).unwrap();
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        Home {
            _dir: dir,
            paths,
            scopes,
        }
    }

    fn at() -> Timestamp {
        AT.parse().unwrap()
    }

    fn ulid(n: u128) -> Ulid {
        Ulid::from(n)
    }

    /// Yields `ulids` in turn.
    fn sequence(ulids: Vec<Ulid>) -> impl FnMut() -> Ulid {
        let mut ulids = ulids.into_iter();
        move || ulids.next().unwrap()
    }

    fn start<'a>(h: &'a Home, ulids: Vec<Ulid>) -> Runtime<'a> {
        Runtime::start_with(&h.paths, &h.scopes, at(), &mut sequence(ulids)).unwrap()
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// `runtime.jsonl`'s lines.
    fn log_of(dir: &Path) -> Vec<ActionRecord> {
        fs::read_to_string(dir.join(RUNTIME_LOG))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn manual(_: &Finding) -> Remediation {
        Remediation {
            command: "look by hand".to_owned(),
            ..Remediation::default()
        }
    }

    fn row(id: &'static str, detect: super::super::registry::DetectFn) -> FailureMode {
        FailureMode {
            id,
            subsystem: Subsystem::StateFiles,
            severity: Severity::P2,
            title: "a test check",
            description: "A test check.",
            why: "It tests the runtime.",
            tier: Tier::Default,
            online: false,
            cost_ms: 1,
            requires: &[],
            detect: Some(detect),
            fix: None,
            manual: Some(Manual {
                instruction: "look by hand",
                why: "a test",
                remediation: manual,
            }),
        }
    }

    fn observation() -> Observation {
        Observation {
            subject: Subject::machine(),
            title: "one".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence::default(),
        }
    }

    fn finds_nothing(_: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
        Detected::complete(Vec::new())
    }

    fn finds_one(_: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
        Detected::complete(vec![observation()])
    }

    fn panics(_: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
        panic!("a check that fails"); // ubs:ignore — the panicking check the runtime isolates
    }

    thread_local! {
        /// What [`walks`] walks, and whether it was called.
        static WALK_ROOT: RefCell<PathBuf> = const { RefCell::new(PathBuf::new()) };
        static CALLED: Cell<bool> = const { Cell::new(false) };
    }

    /// Walks [`WALK_ROOT`], finding one instance, cut short when its budget runs out.
    fn walks(_: &DoctorView, _: &Selection, budget: &mut Budget) -> Detected {
        CALLED.with(|called| called.set(true));
        let root = WALK_ROOT.with(|root| root.borrow().clone());
        let walked = budget::walk(&root, 1, budget, |_| budget::WalkStep::Continue);
        if walked.complete {
            Detected::complete(vec![observation()])
        } else {
            Detected::cut_short(vec![observation()], budget)
        }
    }

    /// A view over `h` that has loaded nothing.
    fn view_of(h: &Home) -> DoctorView {
        DoctorView::load_at(h.paths.clone(), ViewOptions::default())
    }

    fn diagnose(h: &Home, rows: &[FailureMode], budgets: Budgets) -> Diagnosis {
        let runtime = start(h, vec![ulid(1)]);
        runtime
            .diagnose(
                rows,
                &view_of(h),
                &Selection::all(),
                &[Tier::Default],
                budgets,
            )
            .unwrap()
    }

    fn ids_of(diagnosis: &Diagnosis) -> Vec<&'static str> {
        diagnosis.findings.iter().map(|f| f.id).collect()
    }

    #[test]
    fn run_ids_have_the_documented_shape() {
        let id = DoctorRunId::new(at(), ulid(1));
        let expected = format!("2026-10-30T18-03-40Z__{}", ulid(1));
        assert_eq!(id.as_str(), expected);
        assert_eq!(DoctorRunId::parse(&expected), Some(id.clone()));
        assert_eq!(
            serde_json::to_value(&id).unwrap(),
            serde_json::Value::from(expected.clone())
        );
        // Letters, so that lowercasing it changes it.
        let ulid = "01M8S7XQKZ0000000000000ABC";
        assert!(DoctorRunId::parse(&format!("2026-10-30T18-03-40Z__{ulid}")).is_some());
        for (case, bad) in [
            ("no __", format!("2026-10-30T18-03-40Z{ulid}")),
            (
                "a lowercase ulid",
                format!("2026-10-30T18-03-40Z__{}", ulid.to_lowercase()),
            ),
            (
                "colons in the time",
                format!("2026-10-30T18:03:40Z__{ulid}"),
            ),
            (
                "a 25-character ulid",
                format!("2026-10-30T18-03-40Z__{}", &ulid[1..]),
            ),
            ("latest", "latest".to_owned()),
            ("a stage", format!(".2026-10-30T18-03-40Z__{ulid}.tmp")),
        ] {
            assert_eq!(DoctorRunId::parse(&bad), None, "{case}: {bad}");
        }
    }

    #[test]
    fn run_ids_sort_by_time_then_ulid() {
        let early: Timestamp = "2026-10-30T18:03:39Z".parse().unwrap();
        let mut ids = vec![
            DoctorRunId::new(at(), ulid(2)),
            DoctorRunId::new(early, ulid(9)),
            DoctorRunId::new(at(), ulid(1)),
        ];
        ids.sort();
        assert_eq!(
            ids,
            [
                DoctorRunId::new(early, ulid(9)),
                DoctorRunId::new(at(), ulid(1)),
                DoctorRunId::new(at(), ulid(2)),
            ]
        );
    }

    #[test]
    fn starting_stages_then_publishes() {
        let h = home();
        let runtime = start(&h, vec![ulid(1)]);
        let dir = runtime.dir();
        assert_eq!(dir, h.paths.doctor_run(&DoctorRunId::new(at(), ulid(1))));
        assert_eq!(mode(&dir), 0o700, "the run directory");
        let left: Vec<String> = fs::read_dir(h.paths.doctor_runs_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, [runtime.id().as_str()], "no stage left");
    }

    #[test]
    fn a_collision_draws_a_fresh_ulid() {
        let h = home();
        let taken = h.paths.doctor_run(&DoctorRunId::new(at(), ulid(1)));
        fs::create_dir_all(&taken).unwrap();
        let runtime = start(&h, vec![ulid(1), ulid(2)]);
        assert_eq!(runtime.id(), &DoctorRunId::new(at(), ulid(2)));
        assert!(
            !h.paths
                .doctor_run_stage(&DoctorRunId::new(at(), ulid(1)))
                .exists()
        );
        let renames: Vec<ActionRecord> = log_of(&runtime.dir())
            .into_iter()
            .filter(|line| line.op == "rename_noreplace")
            .collect();
        assert_eq!(
            renames.len(),
            1,
            "the refused attempt logs nothing: {renames:?}"
        );
        assert_eq!(
            renames[0].to.as_deref(),
            Some(runtime.dir().to_string_lossy().as_ref())
        );
    }

    #[test]
    fn three_collisions_are_cannot_create() {
        let h = home();
        for n in 1..=3 {
            fs::create_dir_all(h.paths.doctor_run(&DoctorRunId::new(at(), ulid(n)))).unwrap();
        }
        let failed = Runtime::start_with(
            &h.paths,
            &h.scopes,
            at(),
            &mut sequence(vec![ulid(1), ulid(2), ulid(3)]),
        );
        let failure = failed.err().unwrap();
        assert_eq!(
            (failure.exit, failure.error.code),
            (crate::doctor::exit::Exit::CannotCreate, ErrorCode::Io)
        );
        let path = failure.error.details["path"].as_str().unwrap_or_default();
        assert!(
            path.ends_with(DoctorRunId::new(at(), ulid(3)).as_str()),
            "{path}"
        );
    }

    #[test]
    fn an_unwritable_runs_directory_is_cannot_create() {
        if rustix::process::geteuid().is_root() {
            return; // root writes a 0500 directory anyway
        }
        let h = home();
        let runs = h.paths.doctor_runs_dir();
        fs::create_dir_all(&runs).unwrap();
        fs::set_permissions(&runs, fs::Permissions::from_mode(0o500)).unwrap();
        let failed = Runtime::start_with(&h.paths, &h.scopes, at(), &mut sequence(vec![ulid(1)]));
        fs::set_permissions(&runs, fs::Permissions::from_mode(0o700)).unwrap();
        let failure = failed.err().unwrap();
        assert_eq!(failure.exit, crate::doctor::exit::Exit::CannotCreate);
        assert!(
            failure.error.details["path"].is_string(),
            "{:?}",
            failure.error
        );
    }

    #[test]
    fn a_panicking_detector_is_isolated() {
        crate::cli::record_panic_locations();
        let h = home();
        let rows = [row(A, finds_nothing), row(B, panics), row(C, finds_one)];
        let diagnosis = diagnose(&h, &rows, Budgets::TierDefaults);
        assert_eq!(
            ids_of(&diagnosis),
            [ids::DETECTOR_CRASHED, C],
            "{diagnosis:#?}"
        );
        let crashed = &diagnosis.findings[0];
        assert_eq!(crashed.evidence.detector, Some(B));
        let location = crashed.evidence.location.as_deref().unwrap_or_default();
        assert!(location.starts_with("src/doctor/runtime.rs:"), "{location}");
        assert_eq!(
            crashed.instance.as_str(),
            format!("{}@machine:detector={B}", ids::DETECTOR_CRASHED)
        );
        assert_eq!(crate::cli::take_caught_panic(), None, "the slot was taken");
        assert_eq!(diagnosis.checks_run, 3);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn the_failpoint_panics_the_nth_detector() {
        crate::cli::record_panic_locations();
        let h = home();
        let rows = [row(A, finds_one), row(B, finds_one)];
        let diagnosis = fsx::with_spec("doctor.detector.panic=panic@2", || {
            diagnose(&h, &rows, Budgets::TierDefaults)
        });
        let found: Vec<(&str, Option<&str>)> = diagnosis
            .findings
            .iter()
            .map(|f| (f.id, f.evidence.detector))
            .collect();
        assert_eq!(found, [(ids::DETECTOR_CRASHED, Some(B)), (A, None)]);
    }

    #[test]
    fn a_one_entry_budget_is_incomplete() {
        let h = home();
        let tree = tempfile::tempdir().unwrap();
        for n in 0..5 {
            fs::write(tree.path().join(format!("f{n}")), b"").unwrap();
        }
        WALK_ROOT.with(|root| *root.borrow_mut() = tree.path().to_path_buf());
        let rows = [row(A, walks)];
        let budgets = Budgets::Each {
            deadline: Duration::from_secs(60),
            max_entries: 1,
        };
        let diagnosis = diagnose(&h, &rows, budgets);
        assert_eq!(
            ids_of(&diagnosis),
            [A, ids::CHECK_INCOMPLETE],
            "{diagnosis:#?}"
        );
        let evidence = &diagnosis.findings[1].evidence;
        assert_eq!(
            (
                evidence.detector,
                evidence.reason,
                evidence.deadline_ms,
                evidence.max_entries
            ),
            (Some(A), Some("entries"), None, Some(1))
        );
        assert_eq!(diagnosis.incomplete, 1);
        assert_eq!(
            crate::doctor::report::RunState::of(&diagnosis.findings),
            crate::doctor::report::RunState::DoneFindings
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_one_ms_deadline_is_incomplete() {
        let h = home();
        WALK_ROOT.with(|root| *root.borrow_mut() = h.paths.state_dir().to_path_buf());
        let rows = [row(A, walks)];
        let budgets = Budgets::Each {
            deadline: Duration::from_millis(1),
            max_entries: 20_000,
        };
        let diagnosis = fsx::with_spec("doctor.detector.panic=sleep:20", || {
            diagnose(&h, &rows, budgets)
        });
        assert_eq!(
            ids_of(&diagnosis),
            [ids::CHECK_INCOMPLETE],
            "{diagnosis:#?}"
        );
        let evidence = &diagnosis.findings[0].evidence;
        assert_eq!(
            (evidence.reason, evidence.deadline_ms, evidence.max_entries),
            (Some("deadline"), Some(1), None)
        );
        let remediation = &diagnosis.findings[0].remediation;
        assert_eq!(
            remediation.argv.as_deref(),
            Some(
                ["omaddons", "doctor", "--only", A, "--budget", "10ms"]
                    .map(str::to_owned)
                    .as_slice()
            ),
            "a deadline's remediation runs the check alone with ten times as long"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn an_expired_budget_skips_the_call() {
        let h = home();
        CALLED.with(|called| called.set(false));
        WALK_ROOT.with(|root| *root.borrow_mut() = h.paths.state_dir().to_path_buf());
        let rows = [row(A, walks)];
        let budgets = Budgets::Each {
            deadline: Duration::from_millis(1),
            max_entries: 20_000,
        };
        let diagnosis = fsx::with_spec("doctor.detector.panic=sleep:20", || {
            diagnose(&h, &rows, budgets)
        });
        assert!(!CALLED.with(Cell::get), "the check never started");
        assert_eq!(diagnosis.incomplete, 1, "{diagnosis:#?}");
    }

    #[test]
    fn a_row_needing_an_unavailable_layer_is_skipped_not_run() {
        let h = home();
        fs::create_dir_all(h.paths.config_dir()).unwrap();
        fs::write(h.paths.config_dir().join("config.toml"), "not = [valid").unwrap();
        let mut needs_config = row(A, finds_one);
        needs_config.requires = &[Layer::Config];
        let diagnosis = diagnose(&h, &[needs_config], Budgets::TierDefaults);
        assert_eq!(diagnosis.checks_run, 0);
        assert_eq!(diagnosis.skipped.len(), 1, "{diagnosis:#?}");
        assert_eq!(
            ids_of(&diagnosis),
            [ids::CONFIG_PARSE_ERROR],
            "the layer's own finding"
        );
    }

    /// Finishes a run of `ulid` with a small record; returns its directory.
    fn finished(h: &Home, n: u128) -> Finished {
        start(h, vec![ulid(n)]).finish(br#"{"ok":true}"#).unwrap()
    }

    #[test]
    fn hardening_modes() {
        let h = home();
        let done = finished(&h, 1);
        assert_eq!(
            (
                mode(&done.dir),
                mode(&done.dir.join(REPORT_FILE)),
                mode(&done.dir.join(RUNTIME_LOG))
            ),
            (0o700, 0o400, 0o400)
        );
        assert_eq!(
            fs::read(done.dir.join(REPORT_FILE)).unwrap(),
            b"{\"ok\":true}\n",
            "the record, one line"
        );
    }

    #[test]
    fn runtime_jsonl_names_every_write() {
        let h = home();
        let done = finished(&h, 1);
        let lines = log_of(&done.dir);
        let seqs: Vec<u64> = lines.iter().map(|line| line.seq).collect();
        let expected: Vec<u64> = (1..=u64::try_from(lines.len()).unwrap()).collect();
        assert_eq!(seqs, expected, "consecutive from 1");
        let ops: Vec<&str> = lines.iter().map(|line| line.op.as_str()).collect();
        let dirs = ops.iter().take_while(|op| **op == "create_dir").count();
        assert!(
            dirs >= 2,
            "doctor/, runs/ and the stage, as needed: {ops:?}"
        );
        assert_eq!(
            &ops[dirs..],
            [
                "chmod",
                "rename_noreplace",
                "write_file",
                "symlink_atomic",
                "chmod",
                "chmod"
            ]
        );
        let doctor = h.paths.doctor_dir();
        for line in &lines {
            assert!(
                Path::new(&line.path).starts_with(&doctor),
                "outside <state>/doctor/: {line:?}"
            );
        }
    }

    #[test]
    fn latest_moves_only_forward() {
        let link = |h: &Home| fs::read_link(h.paths.doctor_latest()).ok();
        let to = |n: u128| Path::new("runs").join(DoctorRunId::new(at(), ulid(n)).as_str());

        let h = home();
        assert_eq!(finished(&h, 2).latest, Latest::Moved, "missing: set");
        assert_eq!(link(&h), Some(to(2)));
        assert_eq!(finished(&h, 1).latest, Latest::Kept, "a newer one stays");
        assert_eq!(link(&h), Some(to(2)));
        assert_eq!(finished(&h, 3).latest, Latest::Moved, "an older one moves");
        assert_eq!(link(&h), Some(to(3)));

        let dangling = home();
        fs::create_dir_all(dangling.paths.doctor_dir()).unwrap();
        std::os::unix::fs::symlink(to(9), dangling.paths.doctor_latest()).unwrap();
        assert_eq!(
            finished(&dangling, 1).latest,
            Latest::Moved,
            "dangling: set"
        );
        assert_eq!(link(&dangling), Some(to(1)));

        let file = home();
        fs::create_dir_all(file.paths.doctor_dir()).unwrap();
        fs::write(file.paths.doctor_latest(), b"mine").unwrap();
        assert_eq!(
            finished(&file, 1).latest,
            Latest::NotALink,
            "a file: left alone"
        );
        assert_eq!(fs::read(file.paths.doctor_latest()).unwrap(), b"mine");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_hardening_failure_is_io_and_keeps_the_report() {
        let h = home();
        let runtime = start(&h, vec![ulid(1)]);
        let dir = runtime.dir();
        let failed = fsx::with_spec("fsx.write_atomic.before=error", || {
            runtime.finish(br#"{"ok":true}"#)
        });
        let failure = failed.err().unwrap();
        assert_eq!(
            (failure.exit, failure.error.code),
            (crate::doctor::exit::Exit::IoError, ErrorCode::Io)
        );
        let path = failure.error.details["path"].as_str().unwrap_or_default();
        assert!(path.ends_with(REPORT_FILE), "{path}");
        assert!(
            dir.is_dir(),
            "the run directory stays for the caller's report"
        );
    }
}
