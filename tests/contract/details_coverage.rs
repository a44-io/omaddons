//! Rule 6's second half (M4-D55, M4-D10): every declared `details` key of every error
//! code, the shared `COMMON` keys included, is observed in an envelope the binary emitted
//! through `support::check`, so that every declared key's value was validated against its
//! code's specification at least once. A key no site can emit is removed from its spec,
//! never kept for a test to explain away.
//!
//! The table is [`rows`]: each row runs its own scene (its own home, through
//! `support::check`) and names the declared pairs its envelopes show. One test checks the
//! table covers every declared pair and names no undeclared one, then runs every row, in
//! parallel, and checks each shows its own pairs. No other test's runs count, so the
//! result depends on no test order and needs no second run of the suite.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use omaddons::error::ErrorCode;

use super::support::Checked;
use super::validate::observed_details;
use crate::common::tlog::tlog;

mod archives;
mod backups;
mod doctor;
mod folders;
mod plans;
mod sources;

/// One scene and the declared `details` pairs its envelopes show.
pub struct Row {
    /// What the scene shows, as a snake-case name, unique in the table.
    pub id: &'static str,
    /// The pairs: a code, and a path in `DetailSpec::declared_paths`' syntax (`k`, `k.j`,
    /// `k[].j`). A shared (`COMMON`) path is named under the code that carried it.
    pub keys: &'static [(ErrorCode, &'static str)],
    /// The scene: every run through `support::check`.
    pub run: fn() -> Vec<Checked>,
}

/// Every row of every area.
fn rows() -> Vec<&'static Row> {
    [
        archives::ROWS,
        backups::ROWS,
        doctor::ROWS,
        folders::ROWS,
        plans::ROWS,
        sources::ROWS,
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// A declared pair: a code's own key, or a shared one any code's envelope may show.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Declared {
    Own(ErrorCode, String),
    Shared(String),
}

impl Declared {
    fn describe(&self) -> String {
        match self {
            Declared::Own(code, path) => format!("`{code}` declares `details.{path}`"),
            Declared::Shared(path) => {
                format!("every code shares `details.{path}` (COMMON)")
            }
        }
    }
}

/// Every declared pair: each code's own paths, then the shared ones once.
fn declared() -> BTreeSet<Declared> {
    let mut out = BTreeSet::new();
    for code in ErrorCode::ALL {
        let own = code.detail_spec().declared_paths();
        for path in code.declared_detail_paths() {
            if own.contains(&path) {
                out.insert(Declared::Own(*code, path));
            } else {
                out.insert(Declared::Shared(path));
            }
        }
    }
    out
}

/// What a row's pair claims, as the declared pair it is (`None`: the code declares no
/// such path).
fn claimed(code: ErrorCode, path: &str, declared: &BTreeSet<Declared>) -> Option<Declared> {
    [
        Declared::Own(code, path.to_owned()),
        Declared::Shared(path.to_owned()),
    ]
    .into_iter()
    .find(|pair| declared.contains(pair))
}

/// The table's own faults, in order: every declared pair no row names, every row pair its
/// code does not declare, and every row id used twice.
fn table_faults(declared: &BTreeSet<Declared>, rows: &[&Row]) -> Vec<String> {
    let mut faults = Vec::new();
    let mut named = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for row in rows {
        if !ids.insert(row.id) {
            faults.push(format!("row `{}` is in the table twice", row.id));
        }
        for (code, path) in row.keys {
            match claimed(*code, path, declared) {
                Some(pair) => {
                    named.insert(pair);
                }
                None => faults.push(format!(
                    "row `{}` names `{code}` `details.{path}`, which `{code}` does not declare",
                    row.id
                )),
            }
        }
    }
    for pair in declared.difference(&named) {
        faults.push(format!(
            "{}, but no row shows it: add a row that produces it through support::check, or \
             remove it from the code's spec (src/error/specs.rs) when no site can emit it",
            pair.describe()
        ));
    }
    faults
}

/// The row's pairs its envelopes do not show.
fn unshown(row: &Row, envelopes: &[Checked]) -> Vec<String> {
    let observed: BTreeSet<(String, String)> = envelopes
        .iter()
        .flat_map(|checked| observed_details(&checked.envelope))
        .collect();
    row.keys
        .iter()
        .filter(|(code, path)| !observed.contains(&(code.wire().to_owned(), (*path).to_owned())))
        .map(|(code, path)| format!("row `{}` does not show `{code}` `details.{path}`", row.id))
        .collect()
}

/// The panic message of a row that failed.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "a panic without a message".to_owned())
}

#[test]
fn every_declared_details_key_is_observed() {
    let log = tlog!();
    let declared = declared();
    let rows = rows();
    log.step("the table covers every declared pair");
    let faults = table_faults(&declared, &rows);
    log.json("table faults", &faults);
    assert!(
        faults.is_empty(),
        "the details coverage table:\n{}",
        faults.join("\n")
    );

    log.step("every row shows its own pairs");
    let failures = row_failures(&rows, &log);
    log.json("row failures", &failures);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Runs `rows` in parallel, each in its own home: every pair a row does not show, and
/// every row that failed (its panic message, which names its command), sorted.
fn row_failures(rows: &[&Row], log: &crate::common::tlog::TestLog) -> Vec<String> {
    let next = AtomicUsize::new(0);
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZero::get)
        .clamp(1, 8);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while let Some(row) = rows.get(next.fetch_add(1, Ordering::SeqCst)) {
                    let ran = std::panic::catch_unwind(row.run);
                    let found = match ran {
                        Ok(envelopes) => unshown(row, &envelopes),
                        Err(payload) => vec![format!(
                            "row `{}` failed: {}",
                            row.id,
                            panic_text(payload.as_ref())
                        )],
                    };
                    log.note(format!("{}: {}", row.id, found.len()));
                    failures
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .extend(found);
                }
            });
        }
    });
    let mut failures = failures
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    failures.sort();
    failures
}

/// The table's checks report a key no row shows with its code and path, a row naming an
/// undeclared key, and a duplicate row id; a row's envelopes that miss one of its own
/// pairs are reported with the row's id.
#[test]
fn the_coverage_check_reports_what_is_missing() {
    let planted: BTreeSet<Declared> = [
        Declared::Own(ErrorCode::PlanStale, "changed".to_owned()),
        // Planted: declared, but no row can produce it.
        Declared::Own(ErrorCode::PlanStale, "planted_unproducible".to_owned()),
        Declared::Shared("items".to_owned()),
    ]
    .into();
    fn none() -> Vec<Checked> {
        Vec::new()
    }
    static SHOWS: Row = Row {
        id: "shows_changed",
        keys: &[
            (ErrorCode::PlanStale, "changed"),
            (ErrorCode::Usage, "items"),
        ],
        run: none,
    };
    static UNDECLARED: Row = Row {
        id: "names_an_undeclared_key",
        keys: &[(ErrorCode::Locked, "changed")],
        run: none,
    };
    let faults = table_faults(&planted, &[&SHOWS, &UNDECLARED, &SHOWS]);
    assert_eq!(
        faults,
        [
            "row `names_an_undeclared_key` names `locked` `details.changed`, which `locked` \
             does not declare"
                .to_owned(),
            "row `shows_changed` is in the table twice".to_owned(),
            "`plan_stale` declares `details.planted_unproducible`, but no row shows it: add a \
             row that produces it through support::check, or remove it from the code's spec \
             (src/error/specs.rs) when no site can emit it"
                .to_owned(),
        ]
    );
    assert_eq!(
        unshown(&SHOWS, &[]),
        [
            "row `shows_changed` does not show `plan_stale` `details.changed`",
            "row `shows_changed` does not show `usage` `details.items`",
        ]
    );
    let all = declared();
    assert!(all.contains(&Declared::Own(ErrorCode::PlanStale, "changed".to_owned())));
    assert!(all.contains(&Declared::Shared("items[].error.code".to_owned())));
    assert!(
        !all.iter()
            .any(|pair| matches!(pair, Declared::Own(_, path) if path.starts_with("items"))),
        "a shared path is declared once, not under every code"
    );
}

/// `observed_details`' paths, which the rows are judged by: every key of every registered
/// error's `details` at any depth, an embedded error's under its own code too.
#[test]
fn observed_details_paths() {
    use serde_json::{Value, json};
    type Pairs = BTreeSet<(String, String)>;
    let set = |pairs: &[(&str, &str)]| -> Pairs {
        pairs
            .iter()
            .map(|(code, path)| ((*code).to_owned(), (*path).to_owned()))
            .collect()
    };
    let failure =
        |details: Value| json!({"ok": false, "error": {"code": "plan_stale", "details": details}});
    let cases: Vec<(&str, Value, Pairs)> = vec![
        (
            "flat_keys",
            failure(json!({"cause": "x", "changed": ["selection"]})),
            set(&[("plan_stale", "cause"), ("plan_stale", "changed")]),
        ),
        (
            "nested_object",
            failure(json!({"recovery_error": {"code": "io", "message": "m"}})),
            set(&[
                ("plan_stale", "recovery_error"),
                ("plan_stale", "recovery_error.code"),
                ("plan_stale", "recovery_error.message"),
            ]),
        ),
        (
            "array_of_objects",
            failure(json!({"tried": [{"asset": "a", "code": "c"}, {"asset": "b"}]})),
            set(&[
                ("plan_stale", "tried"),
                ("plan_stale", "tried[].asset"),
                ("plan_stale", "tried[].code"),
            ]),
        ),
        (
            "array_of_scalars",
            failure(json!({"required_by": ["A", "B"]})),
            set(&[("plan_stale", "required_by")]),
        ),
        (
            "embedded_target_result_error",
            json!({"ok": true, "data": {"targets": [{"error": {
                "code": "game_running", "details": {"reason": "loaded_toc"},
            }}]}}),
            set(&[("game_running", "reason")]),
        ),
        (
            "item_error_kept_inside_details",
            failure(
                json!({"items": [{"request": "x", "target": "retail", "error": {
                    "code": "rate_limited", "details": {"reset_at": "t"},
                }}]}),
            ),
            set(&[
                ("plan_stale", "items"),
                ("plan_stale", "items[].error"),
                ("plan_stale", "items[].error.code"),
                ("plan_stale", "items[].error.details"),
                ("plan_stale", "items[].error.details.reset_at"),
                ("plan_stale", "items[].request"),
                ("plan_stale", "items[].target"),
                ("rate_limited", "reset_at"),
            ]),
        ),
        (
            "an_unregistered_code_is_no_error",
            json!({"ok": true, "data": {"code": "not_a_code", "details": {"x": 1}}}),
            set(&[]),
        ),
        (
            "no_error",
            json!({"ok": true, "data": {"items": []}}),
            set(&[]),
        ),
    ];
    for (name, envelope, expected) in cases {
        assert_eq!(
            observed_details(&envelope),
            expected,
            "case {name}: {envelope}"
        );
    }
}
