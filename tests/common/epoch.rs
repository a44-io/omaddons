//! One explicit epoch for a scenario (regression guard, common rule 13 and design
//! Appendix D's "Time"): the binary keeps its own clock (no `OMADDONS_NOW`), and a
//! scenario places what it writes relative to one epoch in the past instead: the game's
//! writes of a BugGrabber file (`written`), and, after the fact, the errors ledger's
//! reads and the records' installs (`age_ledger`, `age_records`). `assert_consistent`
//! then checks the scenario's timeline: nothing the ledger or a record holds is later
//! than now, and no read saw a file before the game wrote it.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use jiff::{SignedDuration, Timestamp};
use omaddons::errors::ledger::ErrorsLedger;
use omaddons::state::records::TargetRecord;
use omaddons::state::store;
use serde_json::Value;

use super::home::Home;
use super::http::paths;

/// How long after its write a file is left before it is read: the ledger's stable read
/// takes a file changed just now for one being written.
const SETTLE: Duration = Duration::from_millis(60);

/// The epoch of one scenario: a moment in the past every placed time is relative to.
pub struct Epoch {
    base: Timestamp,
}

impl Epoch {
    /// An epoch `ago` before now, on a whole second (BugGrabber keeps unix times in
    /// seconds), so everything placed up to `ago` after it is past.
    pub fn ago(ago: SignedDuration) -> Self {
        let now = Timestamp::from_second(Timestamp::now().as_second()).unwrap();
        Self { base: now - ago }
    }

    /// The epoch plus `offset`.
    pub fn at(&self, offset: SignedDuration) -> Timestamp {
        self.base + offset
    }

    /// Sets `path`'s modification time to the epoch plus `offset` (when the game wrote
    /// it), then waits until a read takes the file as settled. The offset must leave the
    /// time in the past.
    pub fn written(&self, path: &Path, offset: SignedDuration) {
        let when = self.at(offset);
        assert!(when < Timestamp::now(), "{when} is not in the past");
        let system = SystemTime::UNIX_EPOCH + Duration::from_nanos(when.as_nanosecond() as u64);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(system)
            .unwrap();
        std::thread::sleep(SETTLE);
    }

    /// Moves every time the errors ledger holds back by `by`, as if each read had been
    /// made that much earlier.
    pub fn age_ledger(&self, home: &Home, by: SignedDuration) {
        let path = paths(&home.env).errors_ledger();
        let ledger: ErrorsLedger = load(&path);
        let mut value = serde_json::to_value(&ledger).unwrap();
        shift(&mut value, by, "");
        let aged: ErrorsLedger = serde_json::from_value(value).unwrap();
        fs::write(&path, store::encode(&aged).unwrap()).unwrap();
    }

    /// Moves every addon's `installed_at` in every client's record back by `by`.
    pub fn age_records(&self, home: &Home, by: SignedDuration) {
        let dir = paths(&home.env).targets_dir();
        for path in records(&dir) {
            let mut record: TargetRecord = load(&path);
            for addon in record.addons.values_mut() {
                addon.installed_at -= by;
            }
            fs::write(&path, store::encode(&record).unwrap()).unwrap();
        }
    }

    /// Asserts the scenario's timeline: no time the ledger or a record holds is later than
    /// now, and in the ledger every read and appearance saw its file after the game wrote
    /// it.
    pub fn assert_consistent(&self, home: &Home) {
        let now = Timestamp::now();
        let ledger_path = paths(&home.env).errors_ledger();
        if ledger_path.exists() {
            let ledger: ErrorsLedger = load(&ledger_path);
            let value = serde_json::to_value(&ledger).unwrap();
            for (key, at) in times(&value, "") {
                assert!(
                    at <= now,
                    "the ledger's {key} ({at}) is later than now ({now})"
                );
            }
            for (id, target) in &ledger.targets {
                for (key, db) in &target.databases {
                    for read in &db.reads {
                        if let Some(written) = read.written_at {
                            assert!(
                                written <= read.first_observed_at,
                                "{id}/{key}: a read at {} saw a file written at {written}",
                                read.first_observed_at
                            );
                        }
                    }
                }
                for (cluster, c) in &target.clusters {
                    let (before, found) = (&c.appearance.before, &c.appearance.found);
                    assert!(
                        before.observed_at <= found.observed_at,
                        "{id}/{cluster}: found before the read without it"
                    );
                    if let Some(written) = found.written_at {
                        assert!(
                            written <= found.observed_at,
                            "{id}/{cluster}: found at {} in a file written at {written}",
                            found.observed_at
                        );
                    }
                }
            }
        }
        for path in records(&paths(&home.env).targets_dir()) {
            let record: TargetRecord = load(&path);
            for addon in record.addons.values() {
                assert!(
                    addon.installed_at <= now,
                    "{} installed at {}, later than now",
                    addon.name,
                    addon.installed_at
                );
            }
        }
    }
}

fn load<T: store::Versioned>(path: &Path) -> T {
    store::load::<T>(path)
        .unwrap()
        .unwrap_or_else(|| panic!("{} is missing", path.display())) // ubs:ignore — the test's own failure
        .value
}

/// The clients' record files (never their files indexes).
fn records(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(listing) = fs::read_dir(dir) else {
        return Vec::new();
    };
    listing
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.ends_with(".json") && !name.ends_with(".files.json")
        })
        .collect()
}

/// Whether `key` names an instant (naming rule 1: `at`, or ending in `_at`).
fn is_time(key: &str) -> bool {
    key == "at" || key.ends_with("_at")
}

/// Moves every instant under `value` back by `by`.
fn shift(value: &mut Value, by: SignedDuration, key: &str) {
    match value {
        Value::String(text) if is_time(key) => {
            if let Ok(at) = text.parse::<Timestamp>() {
                *text = (at - by).to_string();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| shift(v, by, key)),
        Value::Object(map) => map.iter_mut().for_each(|(k, v)| shift(v, by, k)),
        _ => {}
    }
}

/// Every instant under `value`, with the key that names it.
fn times(value: &Value, key: &str) -> Vec<(String, Timestamp)> {
    match value {
        Value::String(text) if is_time(key) => text
            .parse::<Timestamp>()
            .map(|at| vec![(key.to_owned(), at)])
            .unwrap_or_default(),
        Value::Array(items) => items.iter().flat_map(|v| times(v, key)).collect(),
        Value::Object(map) => map.iter().flat_map(|(k, v)| times(v, k)).collect(),
        _ => Vec::new(),
    }
}
