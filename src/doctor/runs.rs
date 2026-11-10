//! The doctor's run directories as data (M4-D33): which runs `<state>/doctor/runs/`
//! holds, which of them a crash left under their stage name, and what each one's
//! `report.json` says about itself. `doctor gc`'s retention plan reads them.
//!
//! Reads only, and every read is bounded: a listing by its [`Budget`], a report by
//! [`API_RESPONSE_MAX_BYTES`]. A name that is neither a run id nor a run's stage name is
//! ignored, never reported as a run, and so never deleted.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::budget::{self, Budget, EntryKind};
use super::runtime::DoctorRunId;
use crate::limits::API_RESPONSE_MAX_BYTES;
use crate::model::TxId;
use crate::output::RunKind;
use crate::state::paths::Paths;
use crate::state::store::SchemaVersion;

/// The run's actions log, in a run that repaired something.
pub const ACTIONS_LOG: &str = "actions.jsonl";

/// One run directory of `<state>/doctor/runs/`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RunEntry {
    pub id: DoctorRunId,
    pub dir: PathBuf,
    /// Under its stage name, `.<id>.tmp`: a run a crash left before it was published.
    pub staged: bool,
}

/// The runs, sorted by id (a published run before its stage, were both there).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunListing {
    pub runs: Vec<RunEntry>,
    /// False when the listing stopped at its budget: more runs may exist.
    pub complete: bool,
}

/// Every run directory, published or staged, within `budget`. A missing directory is an
/// empty, complete listing; one that cannot be read is the error.
pub fn list_runs(paths: &Paths, budget: &mut Budget) -> io::Result<RunListing> {
    let listing = match budget::list_dir(&paths.doctor_runs_dir(), budget) {
        Ok(listing) => listing,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(RunListing {
                runs: Vec::new(),
                complete: true,
            });
        }
        Err(e) => return Err(e),
    };
    let mut runs: Vec<RunEntry> = listing
        .entries
        .into_iter()
        .filter(|(_, kind)| *kind == EntryKind::Dir)
        .filter_map(|(name, _)| {
            let name = name.to_str()?;
            let (id, staged) = match DoctorRunId::parse(name) {
                Some(id) => (id, false),
                None => (staged_id(name)?, true),
            };
            Some(RunEntry {
                dir: paths.doctor_runs_dir().join(name),
                id,
                staged,
            })
        })
        .collect();
    runs.sort();
    Ok(RunListing {
        runs,
        complete: listing.complete,
    })
}

/// The run a stage name `.<id>.tmp` is for ([`Paths::doctor_run_stage`]).
fn staged_id(name: &str) -> Option<DoctorRunId> {
    DoctorRunId::parse(name.strip_prefix('.')?.strip_suffix(".tmp")?)
}

/// When a run started, from its id: the UTC second its runtime stamped. `None` for an
/// id whose time part is shaped right but names no instant (a month 13).
pub fn started_at(id: &DoctorRunId) -> Option<Timestamp> {
    let (time, _) = id.as_str().split_once("__")?;
    let (date, clock) = time.split_once('T')?;
    format!("{date}T{}", clock.replace('-', ":")).parse().ok()
}

/// What a run's `report.json` says about the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    /// `None` for a kind this build does not know.
    pub kind: Option<RunKind>,
    /// Its `state` (`DONE_OK`, `DONE_FINDINGS`, …), as written; empty when absent.
    pub state: String,
    /// A fix run's repair transaction, or the repair an undo run reverted.
    pub tx: Option<TxId>,
    /// An undo run's own transaction.
    pub undo_tx: Option<TxId>,
}

/// The run's report, read tolerantly: unknown keys and values are ignored. `None` for a
/// report that is absent, larger than [`API_RESPONSE_MAX_BYTES`], not a JSON object,
/// without a `schema_version`, or of a newer major version.
pub fn read_summary(dir: &Path) -> Option<RunSummary> {
    read_summary_bounded(dir, API_RESPONSE_MAX_BYTES)
}

/// [`read_summary`] with its bound given (a test seam).
fn read_summary_bounded(dir: &Path, max: u64) -> Option<RunSummary> {
    let path = dir.join(super::runtime::REPORT_FILE);
    // Never through a link: a run's record is a regular file.
    if !fs::symlink_metadata(&path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .and_then(|file| file.take(max.saturating_add(1)).read_to_end(&mut bytes))
        .ok()?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let report = value.as_object()?;
    let text = |key: &str| report.get(key).and_then(serde_json::Value::as_str);
    let version: SchemaVersion = text("schema_version")?.parse().ok()?;
    if version.major > 1 {
        return None;
    }
    let tx = |key: &str| {
        text(key).and_then(|id| id.parse::<TxId>().ok().filter(|tx| tx.to_string() == id))
    };
    Some(RunSummary {
        kind: text("kind").and_then(|kind| match kind {
            "diagnose" => Some(RunKind::Diagnose),
            "fix" => Some(RunKind::Fix),
            "undo" => Some(RunKind::Undo),
            _ => None,
        }),
        state: text("state").unwrap_or_default().to_owned(),
        tx: tx("tx"),
        undo_tx: tx("undo_tx"),
    })
}

/// Whether the run kept an actions log: it repaired something, or tried to.
pub fn has_actions_log(dir: &Path) -> bool {
    fs::symlink_metadata(dir.join(ACTIONS_LOG)).is_ok_and(|meta| meta.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::DOCTOR_SCAN_MAX_ENTRIES;

    const ID: &str = "2026-10-30T18-03-40Z__01M8S0000000000000000000AA";
    const OTHER: &str = "2026-10-31T07-23-32Z__01M8S0000000000000000000AB";
    const TX: &str = "01JB8Q2VA00000000000000003";
    const UNDO_TX: &str = "01JB8Q2VA00000000000000004";

    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_roots(
            dir.path().join("home"),
            dir.path().join("config"),
            dir.path().join("state"),
            dir.path().join("cache"),
        );
        (dir, paths)
    }

    fn id(s: &str) -> DoctorRunId {
        DoctorRunId::parse(s).unwrap()
    }

    #[test]
    fn lists_runs_stages_and_ignores_unknown_names() {
        let (_dir, paths) = home();
        let runs = paths.doctor_runs_dir();
        for name in [
            ID.to_owned(),
            format!(".{OTHER}.tmp"),
            ".x.tmp".to_owned(),
            "not-a-run".to_owned(),
        ] {
            fs::create_dir_all(runs.join(name)).unwrap();
        }
        fs::write(runs.join("notes.txt"), "hello").unwrap();
        // A file named as a run is no run.
        fs::write(
            runs.join("2026-10-29T00-00-00Z__01M8S0000000000000000000AC"),
            "",
        )
        .unwrap();
        std::os::unix::fs::symlink(format!("runs/{ID}"), paths.doctor_latest()).unwrap();
        let listing =
            list_runs(&paths, &mut Budget::entries_only(DOCTOR_SCAN_MAX_ENTRIES)).unwrap();
        assert!(listing.complete);
        assert_eq!(
            listing.runs,
            vec![
                RunEntry {
                    id: id(ID),
                    dir: runs.join(ID),
                    staged: false,
                },
                RunEntry {
                    id: id(OTHER),
                    dir: runs.join(format!(".{OTHER}.tmp")),
                    staged: true,
                },
            ],
            "{listing:?}"
        );
    }

    #[test]
    fn a_missing_runs_directory_is_an_empty_complete_listing() {
        let (_dir, paths) = home();
        let listing = list_runs(&paths, &mut Budget::entries_only(10)).unwrap();
        assert_eq!(
            listing,
            RunListing {
                runs: Vec::new(),
                complete: true
            }
        );
    }

    #[test]
    fn a_listing_at_its_budget_is_incomplete() {
        let (_dir, paths) = home();
        for name in [ID, OTHER] {
            fs::create_dir_all(paths.doctor_runs_dir().join(name)).unwrap();
        }
        let listing = list_runs(&paths, &mut Budget::entries_only(1)).unwrap();
        assert!(!listing.complete, "{listing:?}");
        assert_eq!(listing.runs.len(), 1, "{listing:?}");
    }

    #[test]
    fn started_at_reads_the_ids_second() {
        assert_eq!(
            started_at(&id(ID)),
            Some("2026-10-30T18:03:40Z".parse().unwrap())
        );
        let impossible = id("2026-13-30T18-03-40Z__01M8S0000000000000000000AA");
        assert_eq!(started_at(&impossible), None);
    }

    #[test]
    fn reads_report_summaries_tolerantly() {
        let fix = RunSummary {
            kind: Some(RunKind::Fix),
            state: "DONE_OK".to_owned(),
            tx: Some(TX.parse().unwrap()),
            undo_tx: None,
        };
        let cases: [(&str, Option<String>, u64, Option<RunSummary>); 10] = [
            (
                "fix_with_tx",
                Some(format!(
                    r#"{{"schema_version":"1.0","kind":"fix","state":"DONE_OK","tx":"{TX}"}}"#
                )),
                API_RESPONSE_MAX_BYTES,
                Some(fix.clone()),
            ),
            (
                "undo_with_tx_and_undo_tx",
                Some(format!(
                    r#"{{"schema_version":"1.0","kind":"undo","state":"DONE_OK","tx":"{TX}","undo_tx":"{UNDO_TX}"}}"#
                )),
                API_RESPONSE_MAX_BYTES,
                Some(RunSummary {
                    kind: Some(RunKind::Undo),
                    undo_tx: Some(UNDO_TX.parse().unwrap()),
                    ..fix.clone()
                }),
            ),
            (
                "unknown_keys_ignored",
                Some(format!(
                    r#"{{"schema_version":"1.3","kind":"fix","state":"DONE_OK","tx":"{TX}","future":{{"x":1}}}}"#
                )),
                API_RESPONSE_MAX_BYTES,
                Some(fix.clone()),
            ),
            (
                "unknown_kind",
                Some(r#"{"schema_version":"1.0","kind":"sweep","state":"DONE_OK"}"#.to_owned()),
                API_RESPONSE_MAX_BYTES,
                Some(RunSummary {
                    kind: None,
                    state: "DONE_OK".to_owned(),
                    tx: None,
                    undo_tx: None,
                }),
            ),
            (
                "a_tx_that_is_no_id_is_none",
                Some(
                    r#"{"schema_version":"1.0","kind":"fix","state":"DONE_FAILED","tx":7}"#
                        .to_owned(),
                ),
                API_RESPONSE_MAX_BYTES,
                Some(RunSummary {
                    kind: Some(RunKind::Fix),
                    state: "DONE_FAILED".to_owned(),
                    tx: None,
                    undo_tx: None,
                }),
            ),
            (
                "newer_major",
                Some(r#"{"schema_version":"2.0","kind":"fix","state":"DONE_OK"}"#.to_owned()),
                API_RESPONSE_MAX_BYTES,
                None,
            ),
            (
                "no_schema_version",
                Some(r#"{"kind":"fix","state":"DONE_OK"}"#.to_owned()),
                API_RESPONSE_MAX_BYTES,
                None,
            ),
            (
                "over_the_bound",
                Some(format!(
                    r#"{{"schema_version":"1.0","kind":"fix","state":"DONE_OK","tx":"{TX}"}}"#
                )),
                16,
                None,
            ),
            (
                "invalid_json",
                Some("{\"schema_version\":".to_owned()),
                API_RESPONSE_MAX_BYTES,
                None,
            ),
            ("missing", None, API_RESPONSE_MAX_BYTES, None),
        ];
        for (case, report, max, want) in cases {
            let dir = tempfile::tempdir().unwrap();
            if let Some(report) = &report {
                fs::write(dir.path().join("report.json"), report).unwrap();
            }
            let got = read_summary_bounded(dir.path(), max);
            assert_eq!(got, want, "{case}: report {report:?}, bound {max}");
        }
    }

    #[test]
    fn a_report_through_a_link_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("elsewhere.json");
        fs::write(
            &real,
            r#"{"schema_version":"1.0","kind":"fix","state":"DONE_OK"}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("report.json")).unwrap();
        assert_eq!(read_summary(dir.path()), None);
    }

    #[test]
    fn the_actions_log_is_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_actions_log(dir.path()));
        fs::create_dir(dir.path().join(ACTIONS_LOG)).unwrap();
        assert!(!has_actions_log(dir.path()), "a directory is no log");
        fs::remove_dir(dir.path().join(ACTIONS_LOG)).unwrap();
        fs::write(dir.path().join(ACTIONS_LOG), "{}\n").unwrap();
        assert!(has_actions_log(dir.path()));
    }
}
