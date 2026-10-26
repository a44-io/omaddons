//! E11's differential test (common rule 14): `journal::diagnose`, run in process on the
//! very home a crash left (never a copy: a copy has new inodes, and every recorded step
//! would read as neither state), predicts what the recovering command then does to each
//! journal. [`predict`] proves the prediction wrote nothing and is the same twice;
//! [`observe_envelope`] (a command of the binary) and [`observe_result`] (`recover_all` in
//! process) read what recovery did; [`assert_agrees`] holds each diagnosis's `expected()`
//! to it, under the caller's rules: a committing command fails on a stop of its own
//! target or of a journal no target owns, and warns of any other; a command that
//! recovers under the lock without committing (`config set`) only warns. The first
//! journal that fails the caller ends the run: every later one is left untouched.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use omaddons::engine::journal::{self, Diagnoses, Expected, JournalDiagnosis};
use omaddons::error::{Result, Warning};
use omaddons::model::{TargetKey, TxId};
use omaddons::state::paths::Paths;
use omaddons::state::registry::load_registry;
use serde::Serialize;
use serde_json::{Value, json};

use super::home::Home;
use super::privacy::assert_private;
use super::tlog::TestLog;

/// What `diagnose` said of a home before the recovering command.
#[derive(Debug, Clone)]
pub struct Prediction {
    pub label: String,
    pub diagnoses: Diagnoses,
}

impl Prediction {
    /// The transactions it names.
    pub fn txs(&self) -> Vec<TxId> {
        self.diagnoses.journals.iter().map(|d| d.tx).collect()
    }
}

/// What the recovering command knows of the games: the clients it discovered and those
/// whose game runs (a committing command), or nothing (a command without discovery,
/// whose recovery leaves every settings restore waiting).
pub enum Games<'a> {
    Known { running: &'a [PathBuf] },
    Unknown,
}

/// Runs `journal::diagnose` twice on `paths` and `clients`, with the `sv_writable` the
/// recovering command will have, and asserts that the two agree and that nothing under
/// `<state>`, `<cache>` or a client dir changed (every entry's kind, mode, size,
/// modification time and link target, through directories it cannot list as well). The
/// diagnoses are logged, and name no planted account, realm or character.
#[track_caller]
pub fn predict(
    log: &TestLog,
    label: &str,
    paths: &Paths,
    clients: &[PathBuf],
    games: &Games<'_>,
) -> Prediction {
    let roots: Vec<PathBuf> = [paths.state_dir(), paths.cache_dir()]
        .into_iter()
        .map(Path::to_path_buf)
        .chain(clients.iter().cloned())
        .collect();
    let before = fingerprint(&roots);
    let canonical = |dirs: &[PathBuf]| -> Vec<PathBuf> {
        dirs.iter()
            .map(|dir| fs::canonicalize(dir).unwrap_or_else(|_| dir.clone()))
            .collect()
    };
    let known = canonical(clients);
    let running = match games {
        Games::Known { running } => Some(canonical(running)),
        Games::Unknown => None,
    };
    let writable = |dir: &Path| {
        let dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        running
            .as_ref()
            .is_some_and(|running| known.contains(&dir) && !running.contains(&dir))
    };
    let diagnoses = journal::diagnose(paths, clients, &writable);
    let again = journal::diagnose(paths, clients, &writable);
    let after = fingerprint(&roots);
    log.json("diagnoses", &diagnoses);
    let transcript = log.transcript_path();
    assert_eq!(
        again,
        diagnoses,
        "{label}: two diagnoses of one state differ\ntranscript: {}",
        transcript.display()
    );
    let changed: Vec<&PathBuf> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .collect();
    assert!(
        changed.is_empty(),
        "{label}: diagnose wrote: {changed:?}\ntranscript: {}",
        transcript.display()
    );
    assert_private(&serde_json::to_string(&diagnoses).unwrap_or_default());
    Prediction {
        label: label.to_owned(),
        diagnoses,
    }
}

/// Every entry under `roots`: its kind, mode, size, modification time and link target.
fn fingerprint(roots: &[PathBuf]) -> BTreeMap<PathBuf, String> {
    let mut found = BTreeMap::new();
    let mut pending: Vec<PathBuf> = roots.to_vec();
    while let Some(path) = pending.pop() {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let link = fs::read_link(&path).ok();
        found.insert(
            path.clone(),
            format!(
                "{:?} {:o} {} {}.{} {link:?}",
                meta.file_type(),
                meta.mode(),
                meta.len(),
                meta.mtime(),
                meta.mtime_nsec()
            ),
        );
        if meta.is_dir()
            && let Ok(children) = fs::read_dir(&path)
        {
            pending.extend(children.filter_map(|child| child.ok().map(|c| c.path())));
        }
    }
    found
}

/// What recovery did to one journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Observed {
    RolledForward,
    RolledBack,
    /// It deleted the journal without a word: committed, or never prepared.
    Pruned,
    /// The journal is where it was, and nothing was said of it.
    Untouched,
    /// It stopped there: the command failed with `code` (`None` when it only warned),
    /// `reason` and, when the message names one, the step.
    Stopped {
        code: Option<String>,
        reason: Option<String>,
        step: Option<u32>,
    },
}

/// What one recovering command did: per journal, and of a journal directory it could
/// not list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observation {
    pub journals: BTreeMap<TxId, Observed>,
    pub unlisted: Option<Observed>,
}

/// Who recovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Caller {
    /// A command about to commit these targets (`install`, `restore`, …).
    Commit { own: Vec<TargetKey> },
    /// A command that recovers under the lock without committing a target (`config
    /// set`, `targets …`, `auth github set`).
    Under,
}

/// What the recovering command the binary ran did, from its envelope and the journals
/// left after it.
pub fn observe_envelope(
    envelope: &Value,
    prediction: &Prediction,
    journals_after: &[PathBuf],
) -> Observation {
    let error = &envelope["error"];
    let warnings: Vec<(&str, &str)> = envelope["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|w| {
            (
                w["code"].as_str().unwrap_or_default(),
                w["message"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    let warned = |code: &str, about: &str| {
        warnings
            .iter()
            .find(|(c, message)| *c == code && message.contains(about))
            .map(|(_, message)| *message)
    };
    let left: Vec<String> = journals_after
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let mut journals = BTreeMap::new();
    for diagnosis in &prediction.diagnoses.journals {
        let tx = diagnosis.tx.to_string();
        let observed = if error.is_object() && error["details"]["journal_pending"] == tx.as_str() {
            let message = error["message"].as_str().unwrap_or_default();
            Observed::Stopped {
                code: error["code"].as_str().map(str::to_owned),
                reason: error["details"]["reason"].as_str().map(str::to_owned),
                step: step_in(message),
            }
        } else if let Some(message) = warned("journal_recovered", &tx) {
            if message.contains("rolled forward") {
                Observed::RolledForward
            } else {
                Observed::RolledBack
            }
        } else if let Some(message) = warned("journal_pending", &tx) {
            Observed::Stopped {
                code: None,
                reason: reason_in(message),
                step: step_in(message),
            }
        } else if left.iter().any(|name| name.starts_with(&tx)) {
            Observed::Untouched
        } else {
            Observed::Pruned
        };
        journals.insert(diagnosis.tx, observed);
    }
    let unlisted = prediction.diagnoses.unlisted.as_ref().map(|_| {
        if error.is_object() {
            Observed::Stopped {
                code: error["code"].as_str().map(str::to_owned),
                reason: error["details"]["reason"].as_str().map(str::to_owned),
                step: None,
            }
        } else if warned("journal_pending", "could not be recovered").is_some() {
            Observed::Stopped {
                code: None,
                reason: None,
                step: None,
            }
        } else {
            Observed::Untouched
        }
    });
    Observation { journals, unlisted }
}

/// [`observe_envelope`] for recovery run in process (`recover_all`).
pub fn observe_result(
    result: &Result<Vec<Warning>>,
    prediction: &Prediction,
    journals_after: &[PathBuf],
) -> Observation {
    let envelope = match result {
        Ok(warnings) => json!({
            "ok": true,
            "warnings": warnings
                .iter()
                .map(|w| json!({"code": w.code.wire(), "message": w.message}))
                .collect::<Vec<_>>(),
        }),
        Err(error) => json!({
            "ok": false,
            "error": {
                "code": error.code.wire(),
                "message": error.message,
                "details": error.details,
            },
        }),
    };
    observe_envelope(&envelope, prediction, journals_after)
}

/// The `<reason>` of a warning's "could not be recovered (<reason>)" or "cannot recover
/// it (<reason>)".
fn reason_in(message: &str) -> Option<String> {
    ["could not be recovered (", "cannot recover it ("]
        .iter()
        .find_map(|lead| message.split_once(lead))
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(reason, _)| reason.to_owned())
}

/// The `<n>` of "journal step <n>".
fn step_in(message: &str) -> Option<u32> {
    let (_, rest) = message.split_once("journal step ")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// What recovery should have done to `diagnosis`, as an [`Observed`], under `caller`;
/// and whether it fails the caller (which ends the run).
fn wanted(diagnosis: &JournalDiagnosis, caller: &Caller) -> (Observed, bool) {
    match diagnosis.expected() {
        Expected::RolledForward => (Observed::RolledForward, false),
        Expected::RolledBack => (Observed::RolledBack, false),
        Expected::Pruned => (Observed::Pruned, false),
        Expected::Skipped => (Observed::Untouched, false),
        Expected::Stops { code, reason, step } => {
            let unowned = diagnosis.target.is_none();
            let fails = match caller {
                Caller::Commit { own } => {
                    unowned || diagnosis.target.is_some_and(|t| own.contains(&t))
                }
                Caller::Under => unowned,
            };
            let code =
                (fails && matches!(caller, Caller::Commit { .. })).then(|| code.wire().to_owned());
            // Under the lock alone, a journal no target owns ends recovery with one warning
            // that carries its message but not its reason.
            let reason =
                (!(unowned && matches!(caller, Caller::Under))).then(|| reason.as_str().to_owned());
            let step = step.filter(|_| reason.is_some());
            (Observed::Stopped { code, reason, step }, fails)
        }
    }
}

/// Holds every diagnosis's `expected()` to what recovery did, in recovery's order, under
/// `caller`'s rules. A disagreement on an exact diagnosis fails with the label, the
/// diagnosis, the observation and the transcript; one the diagnosis flags as an
/// approximation is noted. Returns how many journals it compared.
#[track_caller]
pub fn assert_agrees(
    log: &TestLog,
    prediction: &Prediction,
    observation: &Observation,
    caller: &Caller,
) -> usize {
    log.json("observed", observation);
    let mut ended = false;
    let mut compared = 0;
    if prediction.diagnoses.unlisted.is_some() {
        let want = Observed::Stopped {
            code: matches!(caller, Caller::Commit { .. }).then(|| "io".to_owned()),
            reason: matches!(caller, Caller::Commit { .. }).then(|| "unattributed".to_owned()),
            step: None,
        };
        assert_eq!(
            observation.unlisted.as_ref(),
            Some(&want),
            "{}: an unlisted journal directory\ntranscript: {}",
            prediction.label,
            log.transcript_path().display()
        );
        ended = true;
    }
    for diagnosis in &prediction.diagnoses.journals {
        let observed = observation
            .journals
            .get(&diagnosis.tx)
            .cloned()
            .unwrap_or(Observed::Untouched);
        let (want, fails) = if ended {
            (Observed::Untouched, false)
        } else {
            wanted(diagnosis, caller)
        };
        ended |= fails;
        compared += 1;
        let agreed = agrees(&want, &observed);
        if !agreed && !diagnosis.exact() {
            log.note(format!(
                "known approximation {:?}: {} predicted {want:?}, recovery did {observed:?}",
                diagnosis.evidence.approximations, diagnosis.tx
            ));
            continue;
        }
        assert!(
            agreed,
            "{}: diagnose predicted {want:?} for {}, recovery did {observed:?} (caller {caller:?})\n{diagnosis:#?}\ntranscript: {}",
            prediction.label,
            diagnosis.tx,
            log.transcript_path().display()
        );
    }
    log.note(format!(
        "{}: {compared} journal(s) compared, all agree",
        prediction.label
    ));
    compared
}

/// Whether `observed` is what `want` predicts: the outcome, the reason and code when
/// predicted, the step when predicted.
fn agrees(want: &Observed, observed: &Observed) -> bool {
    match (want, observed) {
        (
            Observed::Stopped { code, reason, step },
            Observed::Stopped {
                code: seen_code,
                reason: seen_reason,
                step: seen_step,
            },
        ) => {
            code == seen_code
                && (reason.is_none() || reason == seen_reason)
                && (step.is_none() || step == seen_step)
        }
        _ => want == observed,
    }
}

/// The differential on one home, for a scene that predicts, recovers and compares: the
/// paths and clients the recovering command knows, the games running, the prediction.
pub struct Differential {
    pub paths: Paths,
    pub clients: Vec<PathBuf>,
    pub running: Vec<PathBuf>,
    pub prediction: Option<Prediction>,
}

impl Differential {
    /// `home`'s clients in `folders` (`_retail_`, …), the games of `running` among them
    /// running. Registers the home's root with `log`.
    pub fn new(log: &TestLog, home: &Home, folders: &[&str], running: &[&str]) -> Self {
        log.env(home);
        Self {
            paths: super::http::paths(&home.env),
            clients: folders.iter().map(|f| home.client(f)).collect(),
            running: running.iter().map(|f| home.client(f)).collect(),
            prediction: None,
        }
    }

    /// Predicts the home as it is now (the prediction a later [`Self::compare`] uses).
    #[track_caller]
    pub fn predict(&mut self, log: &TestLog, label: &str) -> &Prediction {
        let games = Games::Known {
            running: &self.running,
        };
        self.prediction
            .insert(predict(log, label, &self.paths, &self.clients, &games))
    }

    /// The same for a recovering command without discovery (`config set`): every
    /// settings restore waits.
    #[track_caller]
    pub fn predict_unknown(&mut self, log: &TestLog, label: &str) -> &Prediction {
        self.prediction.insert(predict(
            log,
            label,
            &self.paths,
            &self.clients,
            &Games::Unknown,
        ))
    }

    /// The finishing command's envelope against the last prediction (see [`compare`]).
    #[track_caller]
    pub fn compare(
        &self,
        log: &TestLog,
        envelope: &Value,
        journals_after: &[PathBuf],
        caller: &Caller,
    ) -> usize {
        let prediction = self.prediction.as_ref().expect("compare before predict");
        compare(
            log,
            prediction,
            envelope,
            journals_after,
            caller,
            &self.paths,
            &self.clients,
        )
    }

    /// A committing command on the targets `ids`.
    pub fn commit(&self, ids: &[&str]) -> Caller {
        Caller::Commit {
            own: keys_of(&self.paths, ids),
        }
    }
}

/// The keys of the registered targets among `ids`; one not registered yet is left out.
pub fn keys_of(paths: &Paths, ids: &[&str]) -> Vec<TargetKey> {
    let Ok(loaded) = load_registry(paths) else {
        return Vec::new();
    };
    loaded
        .value
        .targets
        .iter()
        .filter(|target| ids.contains(&target.id.as_str()))
        .map(|target| target.key)
        .collect()
}

/// The finishing command's envelope against `prediction`: observed, compared under
/// `caller`, and, when no journal is left, nothing diagnosed after. Returns how many
/// journals it compared.
#[track_caller]
pub fn compare(
    log: &TestLog,
    prediction: &Prediction,
    envelope: &Value,
    journals_after: &[PathBuf],
    caller: &Caller,
    paths: &Paths,
    clients: &[PathBuf],
) -> usize {
    let observation = observe_envelope(envelope, prediction, journals_after);
    let compared = assert_agrees(log, prediction, &observation, caller);
    if journals_after.is_empty() {
        assert_settled(log, prediction, paths, clients);
    }
    compared
}

/// After the finishing command: `diagnose` names none of `prediction`'s transactions.
#[track_caller]
pub fn assert_settled(log: &TestLog, prediction: &Prediction, paths: &Paths, clients: &[PathBuf]) {
    let left = journal::diagnose(paths, clients, &|_| false);
    let txs = prediction.txs();
    let named: Vec<TxId> = left
        .journals
        .iter()
        .map(|d| d.tx)
        .filter(|tx| txs.contains(tx))
        .collect();
    assert!(
        named.is_empty(),
        "{}: still diagnosed after recovery: {named:?}\ntranscript: {}",
        prediction.label,
        log.transcript_path().display()
    );
}

/// How many states a matrix compared, and how many left a journal.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Tally {
    pub compared: usize,
    pub with_journal: usize,
}

impl Tally {
    /// One state: whether the crash left a journal, and whether it was compared.
    pub fn count(&mut self, with_journal: bool, compared: bool) {
        self.with_journal += usize::from(with_journal);
        self.compared += usize::from(compared && with_journal);
    }

    pub fn add(&mut self, other: Tally) {
        self.compared += other.compared;
        self.with_journal += other.with_journal;
    }

    /// Logs the tally and asserts every state with a journal was compared, and some was.
    #[track_caller]
    pub fn assert_complete(&self, log: &TestLog, what: &str) {
        log.json(&format!("tally {what}"), self);
        assert_eq!(
            self.compared, self.with_journal,
            "{what}: states with a journal left uncompared"
        );
        assert!(self.compared > 0, "{what}: no state was compared");
    }
}
