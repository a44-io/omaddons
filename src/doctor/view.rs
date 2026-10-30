//! The doctor's view of the machine (M4-D43): each layer (the configuration, the target
//! registry, the records) loaded on first use, at most once, and tolerantly. A layer that
//! cannot be read is unavailable, and its failure becomes one finding that shows where and
//! why, never what the file holds (M4-D42), so the checks that need the layer are skipped
//! rather than run on defaults. Nothing here writes.
//!
//! Until E2's part 4 points `status` at the rules directly and F8 removes them, a view can
//! instead carry the facts `status` computes ([`DoctorView::from_status_facts`]); such a
//! view reads no layer.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use super::facts::StatusFacts;
use super::finding::{
    Confidence, Evidence, Finding, FindingTarget, Observation, Severity, Subject,
};
use super::registry::{self, Layer, ids};
use crate::cli::Context;
use crate::error::{ErrorCode, Result, Warning};
use crate::limits::{DISCOVERY_FILE_MAX_BYTES, DOCTOR_SCAN_MAX_ENTRIES};
use crate::model::TargetKey;
use crate::state::config::{Config, LoadedConfig};
use crate::state::paths::Paths;
use crate::state::records::TargetRecord;
use crate::state::registry::Registry;
use crate::state::store::{self, Loaded, VersionStatus, Versioned};

/// One read of the machine, shared by every detector of a run. Each layer is read when a
/// detector first asks for it, and a second ask reads nothing.
///
/// Not `Sync` and not `RefUnwindSafe`: E3 wraps each detector call in `AssertUnwindSafe`.
/// A panic while a layer loads leaves that layer unloaded, so the next ask tries again.
#[derive(Debug)]
pub struct DoctorView {
    paths: Paths,
    /// Built from `status`'s facts: no layer is read, and every layer is unavailable.
    facts_only: bool,
    /// The most entries one listing visits: [`DOCTOR_SCAN_MAX_ENTRIES`] (a test lowers it).
    max_entries: u64,
    config: OnceCell<LayerState<LoadedConfig>>,
    registry: OnceCell<LayerState<Loaded<Registry>>>,
    records: OnceCell<LayerState<Records>>,
    failures: RefCell<Vec<LayerFailure>>,
    warnings: RefCell<Vec<Warning>>,
    loaded: RefCell<BTreeSet<Layer>>,
    status_facts: StatusFacts,
}

impl DoctorView {
    /// The view of this user's machine.
    pub fn load(_ctx: &Context) -> Result<Self> {
        Ok(Self::load_at(Paths::from_env()?))
    }

    /// The view of the machine under `paths`. Reads nothing yet, so it never fails.
    pub fn load_at(paths: Paths) -> Self {
        Self {
            paths,
            facts_only: false,
            max_entries: DOCTOR_SCAN_MAX_ENTRIES,
            config: OnceCell::new(),
            registry: OnceCell::new(),
            records: OnceCell::new(),
            failures: RefCell::default(),
            warnings: RefCell::default(),
            loaded: RefCell::default(),
            status_facts: StatusFacts::default(),
        }
    }

    /// The facts `status` computes, as the seven rows `status` publishes read them (E1's
    /// bridge). The view reads no file: its layers are all unavailable.
    pub fn from_status_facts(paths: Paths, status_facts: StatusFacts) -> Self {
        Self {
            facts_only: true,
            status_facts,
            ..Self::load_at(paths)
        }
    }

    pub fn status_facts(&self) -> &StatusFacts {
        &self.status_facts
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// `config.toml`, the default configuration when it is absent; `None` when it cannot
    /// be read.
    pub fn config(&self) -> Option<&LoadedConfig> {
        if self.facts_only {
            return None;
        }
        self.config
            .get_or_init(|| self.loading(Layer::Config, Self::load_config))
            .available()
    }

    /// `registry.json`, an empty registry when it is absent; `None` when it cannot be read.
    pub fn registry(&self) -> Option<&Loaded<Registry>> {
        if self.facts_only {
            return None;
        }
        self.registry
            .get_or_init(|| self.loading(Layer::Registry, Self::load_registry))
            .available()
    }

    /// Every target's record that the registry names or `<state>/targets/` holds; `None`
    /// when that directory cannot be listed whole.
    pub fn records(&self) -> Option<&Records> {
        if self.facts_only {
            return None;
        }
        self.records
            .get_or_init(|| self.loading(Layer::Records, Self::load_records))
            .available()
    }

    /// A target as a finding names it: its id in the registry, else its key. Never from
    /// discovery, so a label does not depend on which layers a run loaded.
    pub fn target_ref(&self, key: TargetKey) -> FindingTarget {
        let id = self
            .registry()
            .and_then(|registry| registry.value.entry(&key))
            .map_or_else(|| key.to_string(), |entry| entry.id.clone());
        FindingTarget { id, key }
    }

    /// What the loaded layers could not read, by severity, failure mode, target id and
    /// path.
    pub fn failures(&self) -> Vec<LayerFailure> {
        // Naming a target may load the registry (and record its failure), so do it first.
        let keys: BTreeSet<TargetKey> = self
            .failures
            .borrow()
            .iter()
            .filter_map(|failure| failure.target)
            .collect();
        let ids: BTreeMap<TargetKey, String> = keys
            .into_iter()
            .map(|key| (key, self.target_ref(key).id))
            .collect();
        let mut failures = self.failures.borrow().clone();
        failures.sort_by(|a, b| a.order(&ids).cmp(&b.order(&ids)));
        failures
    }

    /// Each failure as its failure mode's finding, in [`DoctorView::failures`]' order.
    pub fn failure_findings(&self) -> Vec<Finding> {
        self.failures()
            .iter()
            .filter_map(|failure| {
                Some(registry::row(failure.fm)?.finding(self.observation(failure)))
            })
            .collect()
    }

    /// The failures of one failure mode, as its detector reports them.
    pub(crate) fn failure_observations(&self, fm: &str) -> Vec<Observation> {
        self.failures()
            .iter()
            .filter(|failure| failure.fm == fm)
            .map(|failure| self.observation(failure))
            .collect()
    }

    /// What the loaded layers warned about (unknown config keys, older or newer files), by
    /// code and message.
    pub fn warnings(&self) -> Vec<Warning> {
        let mut warnings = self.warnings.borrow().clone();
        warnings.sort_by(|a, b| (a.code, &a.message).cmp(&(b.code, &b.message)));
        warnings
    }

    /// The layers loaded so far.
    pub fn loaded_layers(&self) -> BTreeSet<Layer> {
        self.loaded.borrow().clone()
    }

    fn loading<T>(&self, layer: Layer, load: fn(&Self) -> LayerState<T>) -> LayerState<T> {
        let state = load(self);
        self.loaded.borrow_mut().insert(layer);
        state
    }

    fn observation(&self, failure: &LayerFailure) -> Observation {
        let scope = match failure.target {
            Some(key) => Subject::target(&self.target_ref(key).id, key),
            None => Subject::machine(),
        };
        Observation {
            subject: scope.path(&failure.path),
            title: failure.title.clone(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: failure.evidence.clone(),
        }
    }

    /// Records why `layer` could not read `path` (`file` as titles name it) and leaves the
    /// layer unavailable.
    fn fail<T>(
        &self,
        failed: Failed<'_>,
        reason: FailureReason,
        evidence: Evidence,
    ) -> LayerState<T> {
        let evidence = Evidence {
            path: Some(failed.path.to_path_buf()),
            reason: Some(reason.as_str()),
            ..evidence
        };
        self.failures.borrow_mut().push(LayerFailure {
            layer: failed.layer,
            fm: failed.fm,
            target: failed.target,
            path: failed.path.to_path_buf(),
            title: failure_title(failed.file, reason, &evidence),
            evidence,
        });
        LayerState::Unavailable(Unavailable {
            layer: failed.layer,
            cause: failed.layer,
        })
    }

    fn load_config(&self) -> LayerState<LoadedConfig> {
        let path = self.paths.config_file();
        let failed = Failed {
            layer: Layer::Config,
            fm: ids::CONFIG_PARSE_ERROR,
            target: None,
            path: &path,
            file: "config.toml",
        };
        let bytes = match read_bounded(&path, DISCOVERY_FILE_MAX_BYTES) {
            BoundedRead::Missing => {
                return LayerState::Available(LoadedConfig {
                    path,
                    ..LoadedConfig::default()
                });
            }
            BoundedRead::Bytes(bytes) => bytes,
            other => return self.fail_read(failed, &other),
        };
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(err) => {
                let (line, column) = position(err.as_bytes(), err.utf8_error().valid_up_to());
                let evidence = Evidence {
                    line: Some(line),
                    column: Some(column),
                    ..Evidence::default()
                };
                return self.fail(failed, FailureReason::NotUtf8, evidence);
            }
        };
        match Config::parse(&text, &path) {
            Ok((config, warnings)) => {
                self.warnings.borrow_mut().extend(warnings.iter().cloned());
                LayerState::Available(LoadedConfig {
                    config,
                    warnings,
                    path,
                    exists: true,
                    text: Some(text),
                })
            }
            // The message quotes the parser, which can quote the file: only the details
            // `Config::parse` names are kept.
            Err(err) => {
                let number = |name: &str| {
                    err.details[name]
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                };
                let key = err.details["key"].as_str().map(str::to_owned);
                let reason = if key.is_some() {
                    FailureReason::InvalidValue
                } else {
                    FailureReason::Parse
                };
                let evidence = Evidence {
                    line: number("line"),
                    column: number("column"),
                    key,
                    ..Evidence::default()
                };
                self.fail(failed, reason, evidence)
            }
        }
    }

    fn load_registry(&self) -> LayerState<Loaded<Registry>> {
        let path = self.paths.registry_file();
        let failed = Failed {
            layer: Layer::Registry,
            fm: ids::STATE_CORRUPT,
            target: None,
            path: &path,
            file: "registry.json",
        };
        self.load_state(failed, Registry::default)
    }

    fn load_records(&self) -> LayerState<Records> {
        let dir = self.paths.targets_dir();
        let mut keys = match record_keys(&dir, self.max_entries) {
            Ok(keys) => keys,
            Err(listing) => {
                let failed = Failed {
                    layer: Layer::Records,
                    fm: ids::STATE_CORRUPT,
                    target: None,
                    path: &dir,
                    file: "targets/",
                };
                return match listing {
                    Listing::Unreadable => {
                        self.fail(failed, FailureReason::Unreadable, Evidence::default())
                    }
                    Listing::TooMany { count } => {
                        let evidence = Evidence {
                            count: Some(count),
                            limit: Some("DOCTOR_SCAN_MAX_ENTRIES"),
                            ..Evidence::default()
                        };
                        self.fail(failed, FailureReason::TooManyEntries, evidence)
                    }
                };
            }
        };
        if let Some(registry) = self.registry() {
            keys.extend(registry.value.targets.iter().map(|entry| entry.key));
        }
        let by_key = keys
            .into_iter()
            .map(|key| {
                let path = self.paths.target_record(&key);
                let file = format!("targets/{key}.json");
                let failed = Failed {
                    layer: Layer::Records,
                    fm: ids::STATE_CORRUPT,
                    target: Some(key),
                    path: &path,
                    file: &file,
                };
                (key, self.load_state(failed, || TargetRecord::empty(key)))
            })
            .collect();
        LayerState::Available(Records { by_key })
    }

    /// One state file, bounded and decoded as `store::load` would; `missing` is what an
    /// absent file stands for. A newer file that decodes is available, with its warning.
    fn load_state<T: Versioned>(
        &self,
        failed: Failed<'_>,
        missing: impl FnOnce() -> T,
    ) -> LayerState<Loaded<T>> {
        let bytes = match read_bounded(failed.path, DISCOVERY_FILE_MAX_BYTES) {
            BoundedRead::Missing => {
                return LayerState::Available(Loaded {
                    value: missing(),
                    version: T::CURRENT,
                    status: VersionStatus::Current,
                });
            }
            BoundedRead::Bytes(bytes) => bytes,
            other => return self.fail_read(failed, &other),
        };
        match store::load_bytes::<T>(failed.path, &bytes) {
            Ok(loaded) => {
                self.warnings
                    .borrow_mut()
                    .extend(loaded.warning(failed.path));
                LayerState::Available(loaded)
            }
            Err(err) if err.code == ErrorCode::StateTooNew => {
                let evidence = Evidence {
                    observed: err.details["schema_version"].as_str().map(str::to_owned),
                    expected: Some(T::CURRENT.to_string()),
                    ..Evidence::default()
                };
                let failed = Failed {
                    fm: ids::STATE_VERSION_NEWER,
                    ..failed
                };
                self.fail(failed, FailureReason::Newer, evidence)
            }
            Err(_) => self.fail(failed, FailureReason::Decode, Evidence::default()),
        }
    }

    /// The failure of a read that gave no bytes.
    fn fail_read<T>(&self, failed: Failed<'_>, read: &BoundedRead) -> LayerState<T> {
        match read {
            BoundedRead::TooLarge { bytes } => {
                let evidence = Evidence {
                    bytes: Some(*bytes),
                    limit: Some("DISCOVERY_FILE_MAX_BYTES"),
                    ..Evidence::default()
                };
                self.fail(failed, FailureReason::TooLarge, evidence)
            }
            BoundedRead::NotAFile => {
                self.fail(failed, FailureReason::NotAFile, Evidence::default())
            }
            // Missing and Bytes never come here: their callers handle them.
            BoundedRead::Unreadable { .. } | BoundedRead::Missing | BoundedRead::Bytes(_) => {
                self.fail(failed, FailureReason::Unreadable, Evidence::default())
            }
        }
    }
}

/// Where a layer's read failed: the layer, the failure mode it is a finding of, the
/// target whose file it is, and the file.
#[derive(Clone, Copy)]
struct Failed<'a> {
    layer: Layer,
    fm: &'static str,
    target: Option<TargetKey>,
    path: &'a Path,
    /// The file as a title names it: `config.toml`, `targets/<key>.json`.
    file: &'a str,
}

/// A layer as the view loaded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerState<T> {
    Available(T),
    Unavailable(Unavailable),
}

impl<T> LayerState<T> {
    pub fn available(&self) -> Option<&T> {
        match self {
            Self::Available(value) => Some(value),
            Self::Unavailable(_) => None,
        }
    }
}

/// A layer the view could not load, and the layer whose failure made it so: itself, or
/// one it is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unavailable {
    pub layer: Layer,
    pub cause: Layer,
}

/// The records layer: each target's record, or that it could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Records {
    by_key: BTreeMap<TargetKey, LayerState<Loaded<TargetRecord>>>,
}

impl Records {
    /// `key`'s record, when it was read.
    pub fn get(&self, key: &TargetKey) -> Option<&Loaded<TargetRecord>> {
        self.by_key.get(key)?.available()
    }

    /// The records that were read, by key.
    pub fn available(&self) -> impl Iterator<Item = (&TargetKey, &Loaded<TargetRecord>)> {
        self.by_key
            .iter()
            .filter_map(|(key, state)| Some((key, state.available()?)))
    }

    /// The keys whose record could not be read.
    pub fn unavailable(&self) -> impl Iterator<Item = &TargetKey> {
        self.by_key
            .iter()
            .filter(|(_, state)| state.available().is_none())
            .map(|(key, _)| key)
    }
}

/// A file a layer could not read, as a failure mode's finding will show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerFailure {
    pub layer: Layer,
    /// The failure mode it is a finding of.
    pub fm: &'static str,
    /// The target whose file it is; `None` for a file of no client.
    pub target: Option<TargetKey>,
    pub path: PathBuf,
    pub evidence: Evidence,
    pub title: String,
}

impl LayerFailure {
    /// Its place in a report: severity, failure mode, target id (none first), path.
    fn order<'a>(
        &'a self,
        ids: &'a BTreeMap<TargetKey, String>,
    ) -> (Option<Severity>, &'static str, Option<&'a str>, &'a Path) {
        (
            registry::row(self.fm).map(|row| row.severity),
            self.fm,
            self.target
                .and_then(|key| ids.get(&key))
                .map(String::as_str),
            &self.path,
        )
    }
}

/// Why a layer's file could not be read: a failure's `evidence.reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    /// `config.toml` is not TOML, or not the configuration's shape.
    Parse,
    /// A configuration value is out of range.
    InvalidValue,
    NotUtf8,
    /// Over [`DISCOVERY_FILE_MAX_BYTES`].
    TooLarge,
    /// A directory, a FIFO, a socket or a device.
    NotAFile,
    /// Permission denied, a dangling symlink, or any other error.
    Unreadable,
    /// A state file that does not decode.
    Decode,
    /// A state file a newer omaddons wrote, which does not decode.
    Newer,
    /// `<state>/targets/` over [`DOCTOR_SCAN_MAX_ENTRIES`].
    TooManyEntries,
}

impl FailureReason {
    pub const ALL: [Self; 9] = [
        Self::Parse,
        Self::InvalidValue,
        Self::NotUtf8,
        Self::TooLarge,
        Self::NotAFile,
        Self::Unreadable,
        Self::Decode,
        Self::Newer,
        Self::TooManyEntries,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::InvalidValue => "invalid_value",
            Self::NotUtf8 => "not_utf8",
            Self::TooLarge => "too_large",
            Self::NotAFile => "not_a_file",
            Self::Unreadable => "unreadable",
            Self::Decode => "decode",
            Self::Newer => "newer",
            Self::TooManyEntries => "too_many_entries",
        }
    }

    fn of(evidence: &Evidence) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| evidence.reason == Some(reason.as_str()))
    }
}

/// A failure's title: the file, then what is wrong with it.
fn failure_title(file: &str, reason: FailureReason, evidence: &Evidence) -> String {
    let line = evidence.line.unwrap_or(1);
    let column = evidence.column.unwrap_or(1);
    match reason {
        FailureReason::Parse => format!("{file} does not parse (line {line}, column {column})"),
        FailureReason::InvalidValue => format!(
            "{file} has an invalid value for {} (line {line})",
            evidence.key.as_deref().unwrap_or("a key")
        ),
        FailureReason::NotUtf8 => format!("{file} is not UTF-8 (line {line}, column {column})"),
        FailureReason::TooLarge => format!("{file} is larger than omaddons reads"),
        FailureReason::NotAFile => format!("{file} is not a regular file"),
        FailureReason::Unreadable => format!("{file} cannot be read"),
        FailureReason::Decode => format!("{file} is corrupt"),
        FailureReason::Newer => format!(
            "{file} was written by a newer omaddons (version {})",
            evidence.observed.as_deref().unwrap_or("unknown")
        ),
        FailureReason::TooManyEntries => {
            format!("{file} holds more entries than omaddons ever writes")
        }
    }
}

/// The prose remediation of a failure the view recorded, from its finding's evidence: never
/// a command, since none repairs a file omaddons cannot read.
pub(crate) fn failure_remediation(fm: &str, evidence: &Evidence) -> String {
    let path = evidence
        .path
        .as_deref()
        .map_or_else(String::new, |path| path.display().to_string());
    let line = evidence.line.unwrap_or(1);
    let column = evidence.column.unwrap_or(1);
    let mib = DISCOVERY_FILE_MAX_BYTES >> 20;
    match FailureReason::of(evidence) {
        Some(FailureReason::Parse) => format!(
            "fix line {line}, column {column} of {path}; omaddons never rewrites a config.toml that does not parse"
        ),
        Some(FailureReason::InvalidValue) => format!(
            "fix the value of {} on line {line} of {path}",
            evidence.key.as_deref().unwrap_or("the key")
        ),
        Some(FailureReason::NotUtf8) => format!("save {path} as UTF-8"),
        Some(FailureReason::TooLarge) if fm == ids::CONFIG_PARSE_ERROR => {
            format!("shorten {path}: omaddons reads at most {mib} MiB of it")
        }
        Some(FailureReason::TooLarge) => format!(
            "{path} is over {mib} MiB, which omaddons never writes; restore it from a backup"
        ),
        Some(FailureReason::NotAFile) => format!("replace {path} with a regular file"),
        Some(FailureReason::Unreadable) => format!("make {path} readable by your user"),
        Some(FailureReason::Decode) => format!(
            "omaddons cannot decode {path} and never rewrites it; restore it from a backup of the state directory"
        ),
        Some(FailureReason::Newer) => "upgrade omaddons".to_owned(),
        Some(FailureReason::TooManyEntries) => format!(
            "inspect {path}: it holds over {} entries",
            evidence.count.unwrap_or(DOCTOR_SCAN_MAX_ENTRIES)
        ),
        None => format!("inspect {path}"),
    }
}

/// The 1-based line and column of byte `at` of `bytes`, whose first `at` bytes are UTF-8.
fn position(bytes: &[u8], at: usize) -> (u32, u32) {
    let before = bytes
        .get(..at)
        .and_then(|valid| std::str::from_utf8(valid).ok())
        .unwrap_or_default();
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    let to_u32 = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    (to_u32(line), to_u32(column))
}

/// Why `<state>/targets/` could not be listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listing {
    Unreadable,
    /// It holds more than the bound; `count` entries were visited.
    TooMany {
        count: u64,
    },
}

/// The keys of the records in `dir`: each name that is exactly `<TargetKey>.json`, the key
/// in its canonical form. A missing `dir` holds none. One listing, of at most
/// `max_entries` entries.
fn record_keys(dir: &Path, max_entries: u64) -> std::result::Result<BTreeSet<TargetKey>, Listing> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(_) => return Err(Listing::Unreadable),
    };
    let mut keys = BTreeSet::new();
    for (visited, entry) in (0u64..).zip(entries) {
        let entry = entry.map_err(|_| Listing::Unreadable)?;
        if visited == max_entries {
            return Err(Listing::TooMany { count: visited });
        }
        let name = entry.file_name();
        let key = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|stem| {
                stem.parse::<TargetKey>()
                    .ok()
                    .filter(|key| key.to_string() == stem)
            });
        keys.extend(key);
    }
    Ok(keys)
}

/// What a bounded read of one file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundedRead {
    Missing,
    /// The whole file, at most the bound.
    Bytes(Vec<u8>),
    /// Over the bound: its size.
    TooLarge {
        bytes: u64,
    },
    /// Not a regular file (after following symlinks); never opened, so a FIFO cannot
    /// block the read.
    NotAFile,
    /// It exists but cannot be read; a dangling symlink too.
    Unreadable {
        kind: ErrorKind,
    },
}

/// Reads `path` whole when it is a regular file of at most `max` bytes.
pub(crate) fn read_bounded(path: &Path, max: u64) -> BoundedRead {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => return BoundedRead::Missing,
        Err(err) => return BoundedRead::Unreadable { kind: err.kind() },
    }
    let regular = |metadata: std::io::Result<std::fs::Metadata>| match metadata {
        Ok(metadata) if metadata.is_file() => Ok(metadata.len()),
        Ok(_) => Err(BoundedRead::NotAFile),
        Err(err) => Err(BoundedRead::Unreadable { kind: err.kind() }),
    };
    if let Err(read) = regular(std::fs::metadata(path)) {
        return read;
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) => return BoundedRead::Unreadable { kind: err.kind() },
    };
    // The file may have been swapped since the first look.
    let len = match regular(file.metadata()) {
        Ok(len) => len,
        Err(read) => return read,
    };
    let mut bytes = Vec::new();
    if let Err(err) = file.take(max.saturating_add(1)).read_to_end(&mut bytes) {
        return BoundedRead::Unreadable { kind: err.kind() };
    }
    let read = bytes.len() as u64;
    if read > max {
        BoundedRead::TooLarge {
            bytes: len.max(read),
        }
    } else {
        BoundedRead::Bytes(bytes)
    }
}

/// Layers a test sets instead of loading: `None` leaves a layer to load from disk.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Preloaded {
    pub config: Option<LayerState<LoadedConfig>>,
    pub registry: Option<LayerState<Loaded<Registry>>>,
    pub records: Option<LayerState<Records>>,
    pub failures: Vec<LayerFailure>,
}

#[cfg(test)]
impl DoctorView {
    /// A view whose preloaded layers count as loaded, with the failures given.
    pub(crate) fn preloaded(paths: Paths, preloaded: Preloaded) -> Self {
        fn cell<T>(loaded: &mut BTreeSet<Layer>, layer: Layer, state: Option<T>) -> OnceCell<T> {
            if state.is_some() {
                loaded.insert(layer);
            }
            state.map_or_else(OnceCell::new, OnceCell::from)
        }
        let mut loaded = BTreeSet::new();
        let config = cell(&mut loaded, Layer::Config, preloaded.config);
        let registry = cell(&mut loaded, Layer::Registry, preloaded.registry);
        let records = cell(&mut loaded, Layer::Records, preloaded.records);
        Self {
            config,
            registry,
            records,
            failures: RefCell::new(preloaded.failures),
            loaded: RefCell::new(loaded),
            ..Self::load_at(paths)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::doctor::finding::Severity;
    use crate::error::WarningCode;
    use crate::state::registry::{Origin, RegistryEntry};

    /// Planted in every broken file: no failure may show it.
    const MARKER: &str = "PLANTED-CONTENT-7f3a";
    const K: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";
    const K2: &str = "01M7A1B2C3D4E5F6G7H8J9K0MN";

    fn key(text: &str) -> TargetKey {
        text.parse().unwrap()
    }

    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(
            &root,
            root.join("config"),
            root.join("state"),
            root.join("cache"),
        );
        (dir, paths)
    }

    fn is_root() -> bool {
        rustix::process::geteuid().is_root()
    }

    /// How a test leaves one file.
    enum Plant {
        Absent,
        Bytes(Vec<u8>),
        Dir,
        /// These bytes, at mode 000.
        Mode000(Vec<u8>),
        DanglingSymlink,
        /// `DISCOVERY_FILE_MAX_BYTES + 1` bytes, the marker first.
        TooLarge,
    }

    fn plant(path: &Path, plant: &Plant) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        match plant {
            Plant::Absent => {}
            Plant::Bytes(bytes) => std::fs::write(path, bytes).unwrap(),
            Plant::Dir => std::fs::create_dir(path).unwrap(),
            Plant::Mode000(bytes) => {
                std::fs::write(path, bytes).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
            }
            Plant::DanglingSymlink => {
                std::os::unix::fs::symlink(path.with_extension("gone"), path).unwrap();
            }
            Plant::TooLarge => {
                let mut bytes = format!("# {MARKER}\n").into_bytes();
                bytes.resize(usize::try_from(DISCOVERY_FILE_MAX_BYTES).unwrap() + 1, b' ');
                std::fs::write(path, bytes).unwrap();
            }
        }
    }

    fn entry(key_text: &str, id: &str) -> RegistryEntry {
        RegistryEntry {
            key: key(key_text),
            id: id.to_owned(),
            install_root: "/games/wow".into(),
            client_dir: "/games/wow/_retail_".into(),
            flavor_dir: Some("_retail_".into()),
            product: Some("wow".into()),
            qualifier: "battlenet".into(),
            origin: Origin::Discovered,
            registered_at: "2026-10-14T12:00:00Z".parse().unwrap(),
        }
    }

    fn registry_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        store::encode(&Registry {
            targets: entries.iter().map(|(k, id)| entry(k, id)).collect(),
            default: None,
        })
        .unwrap()
    }

    fn record_bytes(key_text: &str, generation: u64) -> Vec<u8> {
        store::encode(&TargetRecord {
            generation,
            ..TargetRecord::empty(key(key_text))
        })
        .unwrap()
    }

    /// What one failure is expected to show.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Shown {
        fm: &'static str,
        reason: &'static str,
        line: Option<u32>,
        column: Option<u32>,
        key: Option<String>,
        observed: Option<String>,
        bytes: Option<u64>,
        count: Option<u64>,
        limit: Option<&'static str>,
    }

    fn shown(fm: &'static str, reason: &'static str) -> Shown {
        Shown {
            fm,
            reason,
            line: None,
            column: None,
            key: None,
            observed: None,
            bytes: None,
            count: None,
            limit: None,
        }
    }

    fn shown_of(failure: &LayerFailure) -> (Shown, Option<&Path>) {
        let e = &failure.evidence;
        (
            Shown {
                fm: failure.fm,
                reason: e.reason.unwrap_or_default(),
                line: e.line,
                column: e.column,
                key: e.key.clone(),
                observed: e.observed.clone(),
                bytes: e.bytes,
                count: e.count,
                limit: e.limit,
            },
            e.path.as_deref(),
        )
    }

    /// The `parse` failure `config.toml` holding `text` shows: the position
    /// `Config::parse` reports.
    fn parse_failure(text: &str) -> Shown {
        let err = Config::parse(text, Path::new("/fixture/config.toml")).unwrap_err();
        assert_eq!(err.details["key"], serde_json::Value::Null, "{text:?}");
        let number = |name: &str| {
            err.details[name]
                .as_u64()
                .map(|n| u32::try_from(n).unwrap())
        };
        Shown {
            line: number("line"),
            column: number("column"),
            ..shown(ids::CONFIG_PARSE_ERROR, "parse")
        }
    }

    fn too_large(fm: &'static str) -> Shown {
        Shown {
            bytes: Some(DISCOVERY_FILE_MAX_BYTES + 1),
            limit: Some("DISCOVERY_FILE_MAX_BYTES"),
            ..shown(fm, "too_large")
        }
    }

    /// The view's findings hold nothing of what the files hold.
    fn assert_content_free(case: &str, view: &DoctorView) {
        let findings = view.failure_findings();
        let text = serde_json::to_string(&findings).unwrap();
        assert!(
            !text.contains(MARKER),
            "case {case}: a finding shows planted content: {text}"
        );
        for finding in &findings {
            assert_eq!(finding.confidence, Confidence::High, "case {case}");
            assert_eq!(finding.remediation.argv, None, "case {case}");
        }
    }

    #[test]
    fn read_bounded_outcomes() {
        let (_dir, paths) = home();
        let base = paths.home().join("files");
        std::fs::create_dir_all(&base).unwrap();
        let at = |name: &str, how: Plant| {
            let path = base.join(name);
            plant(&path, &how);
            path
        };
        let mut cases = vec![
            (
                "missing",
                at("missing", Plant::Absent),
                BoundedRead::Missing,
            ),
            (
                "empty",
                at("empty", Plant::Bytes(Vec::new())),
                BoundedRead::Bytes(Vec::new()),
            ),
            (
                "small",
                at("small", Plant::Bytes(b"abc".to_vec())),
                BoundedRead::Bytes(b"abc".to_vec()),
            ),
            (
                "exactly_max",
                at("exact", Plant::Bytes(b"12345678".to_vec())),
                BoundedRead::Bytes(b"12345678".to_vec()),
            ),
            (
                "one_over_max",
                at("over", Plant::Bytes(b"123456789".to_vec())),
                BoundedRead::TooLarge { bytes: 9 },
            ),
            ("directory", at("dir", Plant::Dir), BoundedRead::NotAFile),
            (
                "dangling_symlink",
                at("dangling", Plant::DanglingSymlink),
                BoundedRead::Unreadable {
                    kind: ErrorKind::NotFound,
                },
            ),
        ];
        let target = at("target", Plant::Bytes(b"abc".to_vec()));
        let link = base.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        cases.push(("symlink_to_file", link, BoundedRead::Bytes(b"abc".to_vec())));
        let fifo = base.join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success());
        if made {
            cases.push(("fifo", fifo, BoundedRead::NotAFile));
        }
        if !is_root() {
            cases.push((
                "mode_000",
                at("locked", Plant::Mode000(b"abc".to_vec())),
                BoundedRead::Unreadable {
                    kind: ErrorKind::PermissionDenied,
                },
            ));
        }
        for (case, path, expected) in cases {
            let got = read_bounded(&path, 8);
            assert_eq!(
                got,
                expected,
                "case {case}: input {} → got {got:?}, expected {expected:?}",
                path.display()
            );
        }
    }

    #[test]
    fn config_layer_outcomes() {
        let toml_error = format!("a = 1\n[limits\n# {MARKER}\n");
        // The parser's message quotes this file, which proves the message is not copied.
        let quoting = format!("running_policy = \"{MARKER}\"\n");
        let err = Config::parse(&quoting, Path::new("/fixture/config.toml")).unwrap_err();
        assert!(err.message.contains(MARKER), "{}", err.message);
        let toml_shown = parse_failure(&toml_error);
        assert_eq!(toml_shown.line, Some(2), "{toml_shown:?}");
        let mut cases: Vec<(&str, Plant, Option<Shown>)> = vec![
            ("missing", Plant::Absent, None),
            (
                "valid",
                Plant::Bytes(b"check_interval_hours = 6\n".to_vec()),
                None,
            ),
            (
                "toml_error",
                Plant::Bytes(toml_error.clone().into_bytes()),
                Some(toml_shown),
            ),
            (
                "type_error_quoting_the_file",
                Plant::Bytes(quoting.clone().into_bytes()),
                Some(parse_failure(&quoting)),
            ),
            (
                "value_out_of_range",
                Plant::Bytes(format!("check_interval_hours = 99\n# {MARKER}\n").into_bytes()),
                Some(Shown {
                    line: Some(1),
                    column: Some(1),
                    key: Some("check_interval_hours".to_owned()),
                    ..shown(ids::CONFIG_PARSE_ERROR, "invalid_value")
                }),
            ),
            (
                // `a = "x\xffy …"`: the first bad byte is line 1, column 7.
                "not_utf8",
                Plant::Bytes([b"a = \"x\xffy ".as_slice(), MARKER.as_bytes(), b"\"\n"].concat()),
                Some(Shown {
                    line: Some(1),
                    column: Some(7),
                    ..shown(ids::CONFIG_PARSE_ERROR, "not_utf8")
                }),
            ),
            (
                "not_utf8_on_line_two",
                // `bé = "`: six characters before the bad byte, seven bytes.
                Plant::Bytes([b"a = 1\nb\xc3\xa9 = \"\xff".as_slice(), MARKER.as_bytes()].concat()),
                Some(Shown {
                    line: Some(2),
                    column: Some(7),
                    ..shown(ids::CONFIG_PARSE_ERROR, "not_utf8")
                }),
            ),
            (
                "too_large",
                Plant::TooLarge,
                Some(too_large(ids::CONFIG_PARSE_ERROR)),
            ),
            (
                "directory",
                Plant::Dir,
                Some(shown(ids::CONFIG_PARSE_ERROR, "not_a_file")),
            ),
            (
                "dangling_symlink",
                Plant::DanglingSymlink,
                Some(shown(ids::CONFIG_PARSE_ERROR, "unreadable")),
            ),
        ];
        if !is_root() {
            cases.push((
                "mode_000",
                Plant::Mode000(format!("# {MARKER}\n").into_bytes()),
                Some(shown(ids::CONFIG_PARSE_ERROR, "unreadable")),
            ));
        }
        for (case, how, expected) in cases {
            let (_dir, paths) = home();
            let path = paths.config_file();
            plant(&path, &how);
            let view = DoctorView::load_at(paths.clone());
            let config = view.config();
            let failures = view.failures();
            match &expected {
                None => {
                    assert!(failures.is_empty(), "case {case}: got {failures:?}");
                    assert!(config.is_some(), "case {case}: unavailable");
                    let config = config.unwrap();
                    assert_eq!(config.path, path, "case {case}");
                    assert_eq!(config.exists, !matches!(how, Plant::Absent), "case {case}");
                }
                Some(expected) => {
                    assert!(config.is_none(), "case {case}: available");
                    assert_eq!(failures.len(), 1, "case {case}: got {failures:?}");
                    let (got, got_path) = shown_of(&failures[0]);
                    assert_eq!(
                        &got,
                        expected,
                        "case {case}: input {} → got {got:?}, expected {expected:?}",
                        path.display()
                    );
                    assert_eq!(got_path, Some(path.as_path()), "case {case}");
                    assert_eq!(failures[0].target, None, "case {case}");
                    assert_content_free(case, &view);
                }
            }
        }
    }

    #[test]
    fn config_warnings_reach_the_view() {
        let (_dir, paths) = home();
        plant(
            &paths.config_file(),
            &Plant::Bytes(b"check_intervl_hours = 6\n".to_vec()),
        );
        let view = DoctorView::load_at(paths);
        let config = view.config().unwrap();
        assert_eq!(config.warnings.len(), 1, "{:?}", config.warnings);
        let codes: Vec<WarningCode> = view.warnings().iter().map(|w| w.code).collect();
        assert_eq!(codes, [WarningCode::ConfigUnknownKey]);
    }

    #[test]
    fn registry_layer_outcomes() {
        let newer_decodes = br#"{"schema_version":"1.9","targets":[],"default":null}"#.to_vec();
        let newer_breaks = br#"{"schema_version":"9.0","targets":"x"}"#.to_vec();
        let corrupt = |reason| Some(shown(ids::STATE_CORRUPT, reason));
        let mut cases: Vec<(&str, Plant, Option<Shown>, usize)> = vec![
            ("missing", Plant::Absent, None, 0),
            (
                "valid",
                Plant::Bytes(registry_bytes(&[(K, "retail")])),
                None,
                1,
            ),
            ("newer_that_decodes", Plant::Bytes(newer_decodes), None, 0),
            (
                "newer_that_does_not_decode",
                Plant::Bytes(newer_breaks),
                Some(Shown {
                    observed: Some("9.0".to_owned()),
                    ..shown(ids::STATE_VERSION_NEWER, "newer")
                }),
                0,
            ),
            (
                "not_json",
                Plant::Bytes(format!("{MARKER} {{").into_bytes()),
                corrupt("decode"),
                0,
            ),
            (
                "no_schema_version",
                Plant::Bytes(format!(r#"{{"targets":[],"x":"{MARKER}"}}"#).into_bytes()),
                corrupt("decode"),
                0,
            ),
            ("empty_file", Plant::Bytes(Vec::new()), corrupt("decode"), 0),
            (
                "too_large",
                Plant::TooLarge,
                Some(too_large(ids::STATE_CORRUPT)),
                0,
            ),
            ("directory", Plant::Dir, corrupt("not_a_file"), 0),
        ];
        if !is_root() {
            cases.push((
                "mode_000",
                Plant::Mode000(registry_bytes(&[(K, "retail")])),
                corrupt("unreadable"),
                0,
            ));
        }
        for (case, how, expected, entries) in cases {
            let (_dir, paths) = home();
            let path = paths.registry_file();
            plant(&path, &how);
            let view = DoctorView::load_at(paths.clone());
            let registry = view.registry();
            let failures = view.failures();
            match &expected {
                None => {
                    assert!(failures.is_empty(), "case {case}: got {failures:?}");
                    assert!(registry.is_some(), "case {case}: unavailable");
                    let registry = registry.unwrap();
                    assert_eq!(registry.value.targets.len(), entries, "case {case}");
                }
                Some(expected) => {
                    assert!(registry.is_none(), "case {case}: available");
                    assert_eq!(failures.len(), 1, "case {case}: got {failures:?}");
                    let (got, got_path) = shown_of(&failures[0]);
                    assert_eq!(
                        &got,
                        expected,
                        "case {case}: input {} → got {got:?}, expected {expected:?}",
                        path.display()
                    );
                    assert_eq!(got_path, Some(path.as_path()), "case {case}");
                    assert_content_free(case, &view);
                }
            }
        }
        // A newer file that decodes is read, and says so.
        let (_dir, paths) = home();
        plant(
            &paths.registry_file(),
            &Plant::Bytes(br#"{"schema_version":"1.9","targets":[],"default":null}"#.to_vec()),
        );
        let view = DoctorView::load_at(paths);
        assert_eq!(
            view.registry().map(|r| r.status),
            Some(VersionStatus::Newer)
        );
        let codes: Vec<WarningCode> = view.warnings().iter().map(|w| w.code).collect();
        assert_eq!(codes, [WarningCode::StateVersionNewer]);
    }

    #[test]
    fn records_layer_outcomes() {
        struct Case {
            name: &'static str,
            registry: Plant,
            files: Vec<(String, Plant)>,
            max_entries: u64,
            /// `(key, generation)` of each available record.
            available: Vec<(&'static str, u64)>,
            unavailable: Vec<&'static str>,
            /// The layer's own failure, when it is unavailable.
            layer_failure: Option<Shown>,
            /// The failures scoped to a target: `(key, reason)`.
            scoped: Vec<(&'static str, &'static str)>,
        }
        let json = json_name;
        let base = |name| Case {
            name,
            registry: Plant::Absent,
            files: Vec::new(),
            max_entries: DOCTOR_SCAN_MAX_ENTRIES,
            available: Vec::new(),
            unavailable: Vec::new(),
            layer_failure: None,
            scoped: Vec::new(),
        };
        let cases = vec![
            Case {
                registry: Plant::Bytes(registry_bytes(&[(K, "retail")])),
                files: vec![(json(K), Plant::Bytes(record_bytes(K, 3)))],
                available: vec![(K, 3)],
                ..base("registry_key_with_file")
            },
            Case {
                registry: Plant::Bytes(registry_bytes(&[(K, "retail")])),
                available: vec![(K, 0)],
                ..base("registry_key_without_file")
            },
            Case {
                files: vec![(json(K2), Plant::Bytes(record_bytes(K2, 2)))],
                available: vec![(K2, 2)],
                ..base("orphan_record_file")
            },
            Case {
                files: vec![
                    (format!("{K}.files.json"), Plant::Bytes(b"{}".to_vec())),
                    (".omaddons-tmp1".to_owned(), Plant::Bytes(b"{}".to_vec())),
                    ("notakey.json".to_owned(), Plant::Bytes(b"{}".to_vec())),
                    (json(&K.to_ascii_lowercase()), Plant::Bytes(b"{}".to_vec())),
                    ("subdir".to_owned(), Plant::Dir),
                ],
                ..base("ignored_names")
            },
            Case {
                files: vec![
                    (json(K), Plant::Bytes(record_bytes(K, 1))),
                    (json(K2), Plant::Bytes(format!("{MARKER} {{").into_bytes())),
                ],
                available: vec![(K, 1)],
                unavailable: vec![K2],
                scoped: vec![(K2, "decode")],
                ..base("one_corrupt_record")
            },
            Case {
                files: vec![(
                    json(K),
                    Plant::Bytes(br#"{"schema_version":"7.0","key":3}"#.to_vec()),
                )],
                unavailable: vec![K],
                scoped: vec![(K, "newer")],
                ..base("newer_record_that_does_not_decode")
            },
            Case {
                registry: Plant::Bytes(format!("{MARKER} {{").into_bytes()),
                files: vec![(json(K), Plant::Bytes(record_bytes(K, 4)))],
                available: vec![(K, 4)],
                ..base("corrupt_registry")
            },
            Case {
                files: vec![
                    (json(K), Plant::Bytes(record_bytes(K, 1))),
                    (json(K2), Plant::Bytes(record_bytes(K2, 1))),
                    ("third".to_owned(), Plant::Bytes(Vec::new())),
                ],
                max_entries: 2,
                layer_failure: Some(Shown {
                    count: Some(2),
                    limit: Some("DOCTOR_SCAN_MAX_ENTRIES"),
                    ..shown(ids::STATE_CORRUPT, "too_many_entries")
                }),
                ..base("listing_over_bound")
            },
            Case {
                layer_failure: Some(shown(ids::STATE_CORRUPT, "unreadable")),
                ..base("targets_not_a_directory")
            },
            base("no_targets_dir"),
        ];
        for case in cases {
            let name = case.name;
            let (_dir, paths) = home();
            plant(&paths.registry_file(), &case.registry);
            for (file, how) in &case.files {
                plant(&paths.targets_dir().join(file), how);
            }
            if name == "targets_not_a_directory" {
                plant(&paths.targets_dir(), &Plant::Bytes(Vec::new()));
            }
            let mut view = DoctorView::load_at(paths.clone());
            view.max_entries = case.max_entries;
            let records = view.records();
            let failures: Vec<LayerFailure> = view
                .failures()
                .into_iter()
                .filter(|f| f.layer == Layer::Records)
                .collect();
            if let Some(expected) = &case.layer_failure {
                assert!(records.is_none(), "case {name}: available");
                assert_eq!(failures.len(), 1, "case {name}: got {failures:?}");
                let (got, got_path) = shown_of(&failures[0]);
                assert_eq!(
                    &got, expected,
                    "case {name}: got {got:?}, expected {expected:?}"
                );
                assert_eq!(got_path, Some(paths.targets_dir().as_path()), "case {name}");
                continue;
            }
            assert!(records.is_some(), "case {name}: unavailable");
            let records = records.unwrap();
            let available: Vec<(String, u64)> = records
                .available()
                .map(|(k, r)| (k.to_string(), r.value.generation))
                .collect();
            let expected: Vec<(String, u64)> = case
                .available
                .iter()
                .map(|(k, g)| ((*k).to_owned(), *g))
                .collect();
            assert_eq!(available, expected, "case {name}: available records");
            let unavailable: Vec<String> = records.unavailable().map(ToString::to_string).collect();
            assert_eq!(
                unavailable, case.unavailable,
                "case {name}: unavailable records"
            );
            let scoped: Vec<(String, &str)> = failures
                .iter()
                .map(|f| {
                    let target = f.target.map(|k| k.to_string()).unwrap_or_default();
                    (target, f.evidence.reason.unwrap_or_default())
                })
                .collect();
            let expected: Vec<(String, &str)> = case
                .scoped
                .iter()
                .map(|(k, reason)| ((*k).to_owned(), *reason))
                .collect();
            assert_eq!(scoped, expected, "case {name}: scoped failures");
            for (k, _) in &case.scoped {
                let path = paths.target_record(&key(k));
                assert!(
                    failures.iter().any(|f| f.path == path),
                    "case {name}: no failure at {}",
                    path.display()
                );
            }
            assert_content_free(name, &view);
        }
    }

    #[test]
    fn record_keys_stop_at_the_bound() {
        let (_dir, paths) = home();
        let dir = paths.targets_dir();
        for name in [json_name(K), json_name(K2), "third".to_owned()] {
            plant(&dir.join(name), &Plant::Bytes(Vec::new()));
        }
        assert_eq!(record_keys(&dir, 2), Err(Listing::TooMany { count: 2 }));
        assert_eq!(record_keys(&dir, 3), Ok(BTreeSet::from([key(K), key(K2)])));
        assert_eq!(
            record_keys(&paths.state_dir().join("nowhere"), 2),
            Ok(BTreeSet::new())
        );
    }

    fn json_name(k: &str) -> String {
        format!("{k}.json")
    }

    #[test]
    fn layers_load_lazily_and_once() {
        let (_dir, paths) = home();
        plant(&paths.config_file(), &Plant::Bytes(b"a = [\n".to_vec()));
        plant(
            &paths.registry_file(),
            &Plant::Bytes(registry_bytes(&[(K, "retail")])),
        );
        let view = DoctorView::load_at(paths.clone());
        assert_eq!(
            view.loaded_layers(),
            BTreeSet::new(),
            "nothing before an ask"
        );
        assert!(view.registry().is_some());
        assert_eq!(view.loaded_layers(), BTreeSet::from([Layer::Registry]));
        assert_eq!(view.failures(), Vec::new());
        assert!(view.config().is_none());
        assert_eq!(view.failures().len(), 1);
        // Mended on disk: the view has read it and does not again.
        std::fs::write(paths.config_file(), b"check_interval_hours = 6\n").unwrap();
        assert!(view.config().is_none());
        assert_eq!(view.failures().len(), 1);
        assert!(view.records().is_some());
        assert_eq!(
            view.loaded_layers(),
            BTreeSet::from([Layer::Config, Layer::Registry, Layer::Records])
        );
    }

    #[test]
    fn a_facts_view_reads_no_layer() {
        let (_dir, paths) = home();
        plant(&paths.config_file(), &Plant::Bytes(b"a = [\n".to_vec()));
        let view = DoctorView::from_status_facts(paths, StatusFacts::default());
        assert!(view.config().is_none());
        assert!(view.registry().is_none());
        assert!(view.records().is_none());
        assert_eq!(view.loaded_layers(), BTreeSet::new());
        assert_eq!(view.failures(), Vec::new());
    }

    #[test]
    fn failure_findings_are_sorted_instanced_and_content_free() {
        let (_dir, paths) = home();
        let broken = format!("a = [ # {MARKER}\n");
        let at = parse_failure(&broken);
        plant(&paths.config_file(), &Plant::Bytes(broken.into_bytes()));
        plant(
            &paths.registry_file(),
            &Plant::Bytes(format!("{MARKER} {{").into_bytes()),
        );
        let record = paths.target_record(&key(K));
        plant(&record, &Plant::Bytes(format!("{MARKER} [").into_bytes()));
        let view = DoctorView::load_at(paths.clone());
        // Asked in the opposite order of the report's.
        assert!(view.config().is_none());
        assert!(view.records().is_some());
        assert!(view.registry().is_none());
        let findings = view.failure_findings();
        let registry = paths.registry_file();
        let config = paths.config_file();
        let got: Vec<(&str, Severity, String, String, Option<String>)> = findings
            .iter()
            .map(|f| {
                (
                    f.id,
                    f.severity,
                    f.instance.to_string(),
                    f.label.clone(),
                    f.target.as_ref().map(|t| t.id.clone()),
                )
            })
            .collect();
        let expected = vec![
            (
                ids::STATE_CORRUPT,
                Severity::P0,
                format!("{}@machine:path={}", ids::STATE_CORRUPT, registry.display()),
                format!("machine · {}", registry.display()),
                None,
            ),
            (
                ids::STATE_CORRUPT,
                Severity::P0,
                format!("{}@{K}:path={}", ids::STATE_CORRUPT, record.display()),
                format!("{K} · {}", record.display()),
                Some(K.to_owned()),
            ),
            (
                ids::CONFIG_PARSE_ERROR,
                Severity::P1,
                format!(
                    "{}@machine:path={}",
                    ids::CONFIG_PARSE_ERROR,
                    config.display()
                ),
                format!("machine · {}", config.display()),
                None,
            ),
        ];
        assert_eq!(got, expected);
        let titles: Vec<String> = findings.iter().map(|f| f.title.clone()).collect();
        assert_eq!(
            titles,
            [
                "registry.json is corrupt".to_owned(),
                format!("targets/{K}.json is corrupt"),
                format!(
                    "config.toml does not parse (line {}, column {})",
                    at.line.unwrap(),
                    at.column.unwrap()
                ),
            ]
        );
        assert_content_free("three_layers", &view);
    }

    #[test]
    fn target_ref_comes_from_the_registry() {
        let cases = [
            (
                "in_the_registry",
                Plant::Bytes(registry_bytes(&[(K, "retail")])),
                K,
                "retail",
            ),
            (
                "not_in_it",
                Plant::Bytes(registry_bytes(&[(K, "retail")])),
                K2,
                K2,
            ),
            ("corrupt_registry", Plant::Bytes(b"{".to_vec()), K, K),
            ("no_registry", Plant::Absent, K, K),
        ];
        for (case, how, k, id) in cases {
            let (_dir, paths) = home();
            plant(&paths.registry_file(), &how);
            let view = DoctorView::load_at(paths);
            let got = view.target_ref(key(k));
            assert_eq!(
                (got.id.as_str(), got.key),
                (id, key(k)),
                "case {case}: key {k} → got {got:?}, expected id {id}"
            );
        }
    }

    #[test]
    fn titles_and_remediations_by_reason() {
        let path = PathBuf::from("/fixture/state/omaddons/registry.json");
        let config = PathBuf::from("/fixture/config/omaddons/config.toml");
        let evidence = |path: &Path, reason: FailureReason| Evidence {
            path: Some(path.to_path_buf()),
            reason: Some(reason.as_str()),
            line: Some(3),
            column: Some(5),
            key: Some("check_interval_hours".to_owned()),
            observed: Some("9.0".to_owned()),
            count: Some(20_000),
            ..Evidence::default()
        };
        let cases = [
            (
                ids::CONFIG_PARSE_ERROR,
                FailureReason::Parse,
                "config.toml does not parse (line 3, column 5)",
                "fix line 3, column 5 of /fixture/config/omaddons/config.toml; omaddons never rewrites a config.toml that does not parse",
            ),
            (
                ids::CONFIG_PARSE_ERROR,
                FailureReason::InvalidValue,
                "config.toml has an invalid value for check_interval_hours (line 3)",
                "fix the value of check_interval_hours on line 3 of /fixture/config/omaddons/config.toml",
            ),
            (
                ids::CONFIG_PARSE_ERROR,
                FailureReason::NotUtf8,
                "config.toml is not UTF-8 (line 3, column 5)",
                "save /fixture/config/omaddons/config.toml as UTF-8",
            ),
            (
                ids::CONFIG_PARSE_ERROR,
                FailureReason::TooLarge,
                "config.toml is larger than omaddons reads",
                "shorten /fixture/config/omaddons/config.toml: omaddons reads at most 1 MiB of it",
            ),
            (
                ids::STATE_CORRUPT,
                FailureReason::TooLarge,
                "registry.json is larger than omaddons reads",
                "/fixture/state/omaddons/registry.json is over 1 MiB, which omaddons never writes; restore it from a backup",
            ),
            (
                ids::STATE_CORRUPT,
                FailureReason::NotAFile,
                "registry.json is not a regular file",
                "replace /fixture/state/omaddons/registry.json with a regular file",
            ),
            (
                ids::STATE_CORRUPT,
                FailureReason::Unreadable,
                "registry.json cannot be read",
                "make /fixture/state/omaddons/registry.json readable by your user",
            ),
            (
                ids::STATE_CORRUPT,
                FailureReason::Decode,
                "registry.json is corrupt",
                "omaddons cannot decode /fixture/state/omaddons/registry.json and never rewrites it; restore it from a backup of the state directory",
            ),
            (
                ids::STATE_VERSION_NEWER,
                FailureReason::Newer,
                "registry.json was written by a newer omaddons (version 9.0)",
                "upgrade omaddons",
            ),
            (
                ids::STATE_CORRUPT,
                FailureReason::TooManyEntries,
                "registry.json holds more entries than omaddons ever writes",
                "inspect /fixture/state/omaddons/registry.json: it holds over 20000 entries",
            ),
        ];
        assert_eq!(cases.len(), FailureReason::ALL.len() + 1, "every reason");
        for (fm, reason, title, remediation) in cases {
            let file = if fm == ids::CONFIG_PARSE_ERROR {
                "config.toml"
            } else {
                "registry.json"
            };
            let at = if fm == ids::CONFIG_PARSE_ERROR {
                &config
            } else {
                &path
            };
            let evidence = evidence(at, reason);
            let case = reason.as_str();
            assert_eq!(failure_title(file, reason, &evidence), title, "case {case}");
            assert_eq!(
                failure_remediation(fm, &evidence),
                remediation,
                "case {case} of {fm}"
            );
            assert_eq!(FailureReason::of(&evidence), Some(reason), "case {case}");
        }
    }

    #[test]
    fn view_source_never_writes() {
        use crate::source_scan::{Literals, production_code};
        let code = production_code(include_str!("view.rs"), Literals::Blank).unwrap();
        for forbidden in [
            "WriteSession",
            "mutate(",
            "prune(",
            "Op::",
            "OpenOptions",
            "fs::write",
            "lock_file",
        ] {
            assert!(
                !code.contains(forbidden),
                "src/doctor/view.rs uses {forbidden}"
            );
        }
    }
}
