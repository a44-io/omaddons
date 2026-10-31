//! The doctor's view of the machine (M4-D43): each layer (the configuration, the target
//! registry, the records, the discovered clients, each client's `AddOns/`) loaded on first
//! use, at most once, and tolerantly. A layer that cannot be read is unavailable, and its
//! failure becomes one finding that shows where and why, never what the file holds
//! (M4-D42), so the checks that need the layer are skipped rather than run on defaults. A
//! layer built from an unavailable one is unavailable too, with that one as its cause.
//! Nothing here writes, the discovery cache included.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::finding::{
    Confidence, Evidence, Finding, FindingTarget, Observation, Severity, Subject,
};
use super::registry::{self, Layer, ids};
use super::selection::SkippedTarget;
use crate::app::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::discover::cache::DiscoveryMode;
use crate::discover::registry::read_marker;
use crate::discover::running::detect_running;
use crate::discover::shared::siblings;
use crate::engine::journal::{self, Pending};
use crate::engine::retention;
use crate::engine::simulate::{self, Artifact, ArtifactKind, Diagnoses};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::fsx::{LockState, probe_global};
use crate::inventory::{Depth, Inventory, inventory_shared};
use crate::limits::{DISCOVERY_FILE_MAX_BYTES, DOCTOR_SCAN_MAX_ENTRIES};
use crate::model::{Target, TargetKey, TxId};
use crate::omarchy::status::{self as omarchy_status, WidgetInfo};
use crate::state::config::{Config, CredentialRef, LoadedConfig};
use crate::state::history::{self, HistoryFilter, HistoryRead};
use crate::state::paths::Paths;
use crate::state::records::TargetRecord;
use crate::state::registry::Registry;
use crate::state::store::{self, Loaded, VersionStatus, Versioned};

/// How the view finds clients. Neither way writes: loading the view never writes the
/// discovery cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewOptions {
    pub discovery: ViewDiscovery,
    /// The process building the view holds the global lock itself (`doctor fix` under
    /// its lock, M4-D45): the lock probe then sees that hold, which hides no journal.
    pub caller_holds_lock: bool,
}

/// How the Discovery layer finds clients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ViewDiscovery {
    /// Scan for clients, reusing each unchanged root's cached parse (the doctor's
    /// default): [`DiscoveryMode::FullNoWrite`].
    #[default]
    Scan,
    /// No scan: the cached roots looked at again, or without a cache only the registered
    /// roots (`status --cached`): [`DiscoveryMode::NoScan`].
    Cached,
}

impl ViewDiscovery {
    fn mode(self) -> DiscoveryMode {
        match self {
            Self::Scan => DiscoveryMode::FullNoWrite,
            Self::Cached => DiscoveryMode::NoScan,
        }
    }
}

/// One read of the machine, shared by every detector of a run. Each layer is read when a
/// detector first asks for it, and a second ask reads nothing.
///
/// Not `Sync` and not `RefUnwindSafe`: E3 wraps each detector call in `AssertUnwindSafe`.
/// A panic while a layer loads leaves that layer unloaded, so the next ask tries again.
#[derive(Debug)]
pub struct DoctorView {
    paths: Paths,
    options: ViewOptions,
    /// The most entries one listing visits: [`DOCTOR_SCAN_MAX_ENTRIES`] (a test lowers it).
    max_entries: u64,
    config: OnceCell<LayerState<LoadedConfig>>,
    registry: OnceCell<LayerState<Loaded<Registry>>>,
    records: OnceCell<LayerState<Records>>,
    discovered: OnceCell<LayerState<Discovered>>,
    journals: OnceCell<LayerState<Journals>>,
    lock: OnceCell<LayerState<LockState>>,
    history: OnceCell<LayerState<HistoryRead>>,
    caches: OnceCell<LayerState<Caches>>,
    widget: OnceCell<LayerState<WidgetLayer>>,
    skill: OnceCell<LayerState<Vec<SkillFile>>>,
    credentials: OnceCell<LayerState<Credentials>>,
    failures: RefCell<Vec<LayerFailure>>,
    warnings: RefCell<Vec<Warning>>,
    /// What went wrong that no failure mode describes, for a `-v` log.
    diagnostics: RefCell<Vec<String>>,
    loaded: RefCell<BTreeSet<Layer>>,
}

impl DoctorView {
    /// The view of this user's machine.
    pub fn load(_ctx: &Context, options: ViewOptions) -> Result<Self> {
        Ok(Self::load_at(Paths::from_env()?, options))
    }

    /// The view of the machine under `paths`. Reads nothing yet, so it never fails.
    pub fn load_at(paths: Paths, options: ViewOptions) -> Self {
        Self {
            paths,
            options,
            max_entries: DOCTOR_SCAN_MAX_ENTRIES,
            config: OnceCell::new(),
            registry: OnceCell::new(),
            records: OnceCell::new(),
            discovered: OnceCell::new(),
            journals: OnceCell::new(),
            lock: OnceCell::new(),
            history: OnceCell::new(),
            caches: OnceCell::new(),
            widget: OnceCell::new(),
            skill: OnceCell::new(),
            credentials: OnceCell::new(),
            failures: RefCell::default(),
            warnings: RefCell::default(),
            diagnostics: RefCell::default(),
            loaded: RefCell::default(),
        }
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// `config.toml`, the default configuration when it is absent; `None` when it cannot
    /// be read.
    pub fn config(&self) -> Option<&LoadedConfig> {
        self.layer(&self.config, Layer::Config, Self::load_config)
    }

    /// `registry.json`, an empty registry when it is absent; `None` when it cannot be read.
    pub fn registry(&self) -> Option<&Loaded<Registry>> {
        self.layer(&self.registry, Layer::Registry, Self::load_registry)
    }

    /// Every target's record that the registry names or `<state>/targets/` holds; `None`
    /// when that directory cannot be listed whole.
    pub fn records(&self) -> Option<&Records> {
        self.layer(&self.records, Layer::Records, Self::load_records)
    }

    /// Every journal that is not committed, or that recovery cannot read, oldest first;
    /// `None` when `<state>/journal/` cannot be listed.
    pub fn journals(&self) -> Option<&Journals> {
        self.layer(&self.journals, Layer::Journals, Self::load_journals)
    }

    /// The global lock, probed without taking it (detectors read `Missing` as `Free`);
    /// `None` when its file cannot be opened.
    pub fn lock(&self) -> Option<&LockState> {
        self.layer(&self.lock, Layer::Lock, Self::load_lock)
    }

    /// Whether the process building the view holds the global lock itself.
    pub fn caller_holds_lock(&self) -> bool {
        self.options.caller_holds_lock
    }

    /// `history.jsonl`, read whole as `history` and `undo` read it; `None` when it cannot
    /// be read. Its `unreadable[].raw` holds damaged lines' text: count them, never put
    /// them in evidence (M4-D42).
    pub fn history(&self) -> Option<&HistoryRead> {
        self.layer(&self.history, Layer::History, Self::load_history)
    }

    /// The listing of `<cache>/blobs/`; `None` when it cannot be listed (derived data: no
    /// failure is recorded).
    pub fn caches(&self) -> Option<&Caches> {
        self.layer(&self.caches, Layer::Caches, Self::load_caches)
    }

    /// The bar widget as `status` reports it; never unavailable.
    pub fn widget(&self) -> Option<&WidgetLayer> {
        self.layer(&self.widget, Layer::Widget, |view| {
            LayerState::Available(WidgetLayer {
                info: omarchy_status::widget(&view.paths),
            })
        })
    }

    /// Each installed agent skill's `SKILL.md`, read bounded; never unavailable: each
    /// file's outcome is its `read`.
    pub fn skill(&self) -> Option<&[SkillFile]> {
        self.layer(&self.skill, Layer::Skill, Self::load_skill)
            .map(Vec::as_slice)
    }

    /// Each stored credential file and each configured `{ file = … }` credential, stat'ed
    /// and never opened; `None` when the configuration cannot be read.
    pub fn credentials(&self) -> Option<&Credentials> {
        self.layer(
            &self.credentials,
            Layer::Credentials,
            Self::load_credentials,
        )
    }

    /// The clients on this machine, reconciled with the registry, found in the view's
    /// discovery mode without writing the discovery cache. `None` when the configuration
    /// or the registry cannot be read: discovery never runs on defaults in place of the
    /// user's broken file. Its `github_token` is none: the view resolves no credential.
    pub fn world(&self) -> Option<&World> {
        Some(&self.discovered()?.world)
    }

    /// `target`'s `AddOns/`, classified at full depth when a check first asks for this
    /// client. `None` for a client not in [`DoctorView::world`], or when its folders
    /// cannot be classified: its record or a registered sibling's cannot be read, or
    /// `AddOns/` cannot be listed.
    pub fn inventory(&self, target: &DiscoveredTarget) -> Option<&Inventory> {
        let discovered = self.discovered()?;
        let index = discovered
            .world
            .resolved
            .targets
            .iter()
            .position(|d| d.target.client_dir == target.target.client_dir)?;
        let cell = discovered.inventories.get(index)?;
        cell.get_or_init(|| {
            let state = self.load_inventory(&discovered.world, index);
            self.loaded.borrow_mut().insert(Layer::Inventory);
            state
        })
        .available()
    }

    /// What went wrong while loading that no failure mode describes, for a `-v` log.
    pub fn diagnostics(&self) -> Vec<String> {
        self.diagnostics.borrow().clone()
    }

    fn discovered(&self) -> Option<&Discovered> {
        self.layer(&self.discovered, Layer::Discovery, Self::load_discovery)
    }

    /// `cell`'s layer, loaded by `load` on the first ask.
    fn layer<'a, T>(
        &'a self,
        cell: &'a OnceCell<LayerState<T>>,
        layer: Layer,
        load: fn(&Self) -> LayerState<T>,
    ) -> Option<&'a T> {
        cell.get_or_init(|| {
            let state = load(self);
            self.loaded.borrow_mut().insert(layer);
            state
        })
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
            .filter_map(|failure| self.failure_finding(failure))
            .collect()
    }

    /// One failure as its failure mode's finding.
    pub(crate) fn failure_finding(&self, failure: &LayerFailure) -> Option<Finding> {
        Some(registry::row(failure.fm)?.finding(self.observation(failure)))
    }

    /// Whether `layer` is available, loading it (and what it is built from) if it was not;
    /// when it is not, the layer whose failure made it so. Inventories are per client:
    /// the layer is available when what they are built from is, and a client whose own
    /// one fails is in [`DoctorView::unavailable_targets`].
    pub fn availability(&self, layer: Layer) -> Availability {
        let available = match layer {
            Layer::Config => self.config().is_some(),
            Layer::Registry => self.registry().is_some(),
            Layer::Discovery => self.world().is_some(),
            Layer::Records => self.records().is_some(),
            Layer::Inventory => {
                return layer
                    .depends_on()
                    .iter()
                    .map(|&from| self.availability(from))
                    .find(|a| *a != Availability::Available)
                    .unwrap_or(Availability::Available);
            }
            Layer::Journals => self.journals().is_some(),
            Layer::Lock => self.lock().is_some(),
            Layer::History => self.history().is_some(),
            Layer::Caches => self.caches().is_some(),
            Layer::Widget => self.widget().is_some(),
            Layer::Skill => self.skill().is_some(),
            Layer::Credentials => self.credentials().is_some(),
        };
        if available {
            return Availability::Available;
        }
        let cause = match layer {
            Layer::Config => cause_of(&self.config, layer),
            Layer::Registry => cause_of(&self.registry, layer),
            Layer::Discovery => cause_of(&self.discovered, layer),
            Layer::Records => cause_of(&self.records, layer),
            Layer::Journals => cause_of(&self.journals, layer),
            Layer::Lock => cause_of(&self.lock, layer),
            Layer::History => cause_of(&self.history, layer),
            Layer::Caches => cause_of(&self.caches, layer),
            Layer::Widget => cause_of(&self.widget, layer),
            Layer::Skill => cause_of(&self.skill, layer),
            Layer::Credentials => cause_of(&self.credentials, layer),
            Layer::Inventory => layer,
        };
        Availability::Unavailable { cause }
    }

    /// The clients whose part of a per-client layer (`Records`, `Inventory`) could not be
    /// read so far, with the cause; none for any other layer. Inventories count once
    /// built: a detector's ask builds them.
    pub fn unavailable_targets(&self, layer: Layer) -> Vec<(SkippedTarget, Layer)> {
        let keyed = |key: TargetKey| SkippedTarget {
            id: self.target_ref(key).id,
            key: Some(key),
        };
        match layer {
            Layer::Records => self
                .records()
                .map(|records| {
                    records
                        .unavailable()
                        .map(|key| (keyed(*key), Layer::Records))
                        .collect()
                })
                .unwrap_or_default(),
            Layer::Inventory => {
                let Some(LayerState::Available(discovered)) = self.discovered.get() else {
                    return Vec::new();
                };
                discovered
                    .world
                    .resolved
                    .targets
                    .iter()
                    .zip(&discovered.inventories)
                    .filter_map(|(d, cell)| match cell.get() {
                        Some(LayerState::Unavailable(unavailable)) => {
                            let target = d.target.key.map_or_else(
                                || SkippedTarget {
                                    id: d.target.id.clone(),
                                    key: None,
                                },
                                keyed,
                            );
                            Some((target, unavailable.cause))
                        }
                        _ => None,
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
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
                    config_key: key,
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
                    ListingError::Unreadable => {
                        self.fail(failed, FailureReason::Unreadable, Evidence::default())
                    }
                    ListingError::TooMany { count } => {
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

    fn load_discovery(&self) -> LayerState<Discovered> {
        let unavailable = |cause| {
            LayerState::Unavailable(Unavailable {
                layer: Layer::Discovery,
                cause,
            })
        };
        // Their own failures stand; discovery adds none.
        let Some(config) = self.config() else {
            return unavailable(cause_of(&self.config, Layer::Config));
        };
        let Some(registry) = self.registry() else {
            return unavailable(cause_of(&self.registry, Layer::Registry));
        };
        let mut found = Vec::new();
        let assembled = World::assemble(
            self.paths.clone(),
            config.clone(),
            registry.clone(),
            self.options.discovery.mode(),
            &mut |batch| found.extend_from_slice(batch),
        );
        // The config's and the registry's own warnings are the view's already.
        let mut warnings = self.warnings.borrow_mut();
        for warning in found {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        drop(warnings);
        match assembled {
            Ok(world) => {
                let inventories = world
                    .resolved
                    .targets
                    .iter()
                    .map(|_| OnceCell::new())
                    .collect();
                LayerState::Available(Discovered { world, inventories })
            }
            Err(err) => self.fail_error(Layer::Discovery, &err, None),
        }
    }

    /// The Journals layer uses Discovery and Registry when they load, and needs neither:
    /// without Discovery every client counts as possibly running, so a settings restore
    /// waits for its game (fail closed, as recovery without discovery does).
    fn load_journals(&self) -> LayerState<Journals> {
        // A home without journals costs only the listing: the clients and the process
        // table are read only when there is a journal to diagnose.
        let any_journal = std::fs::read_dir(self.paths.journal_dir())
            .is_ok_and(|mut listing| listing.next().is_some());
        let discovered: Vec<Target> = any_journal
            .then(|| self.world())
            .flatten()
            .map(|world| {
                world
                    .resolved
                    .targets
                    .iter()
                    .map(|d| d.target.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut client_dirs: Vec<PathBuf> =
            discovered.iter().map(|t| t.client_dir.clone()).collect();
        if let Some(registry) = any_journal.then(|| self.registry()).flatten() {
            client_dirs.extend(registry.value.targets.iter().map(|e| e.client_dir.clone()));
        }
        client_dirs.sort();
        client_dirs.dedup();
        let running = (!discovered.is_empty()).then(|| detect_running(&discovered));
        let sv_writable = |dir: &Path| {
            discovered.iter().any(|t| t.client_dir == dir)
                && running.as_ref().is_some_and(|scan| {
                    scan.unavailable.is_none() && !scan.clients.contains_key(dir)
                })
        };
        match Journals::read(&self.paths, &client_dirs, &sv_writable, self.max_entries) {
            Ok(journals) => LayerState::Available(journals),
            Err(err) => self.fail_error(Layer::Journals, &err, Some("journal/")),
        }
    }

    /// The lock file's path comes only from a failure's error: the view never names it.
    fn load_lock(&self) -> LayerState<LockState> {
        match probe_global(&self.paths) {
            Ok(state) => LayerState::Available(state),
            Err(err) => self.fail_error(Layer::Lock, &err, Some("lock")),
        }
    }

    fn load_history(&self) -> LayerState<HistoryRead> {
        match history::read(&self.paths, &HistoryFilter::default()) {
            Ok(read) => LayerState::Available(read),
            Err(err) => self.fail_error(Layer::History, &err, Some("history.jsonl")),
        }
    }

    fn load_caches(&self) -> LayerState<Caches> {
        match list_dir(&self.paths.blobs_dir(), self.max_entries) {
            Ok(blobs) => LayerState::Available(Caches { blobs }),
            Err(_) => LayerState::Unavailable(Unavailable {
                layer: Layer::Caches,
                cause: Layer::Caches,
            }),
        }
    }

    fn load_skill(&self) -> LayerState<Vec<SkillFile>> {
        let dir = self.paths.skill_dir();
        let file = dir.join("SKILL.md");
        let read = read_bounded(&file, DISCOVERY_FILE_MAX_BYTES);
        LayerState::Available(vec![SkillFile { dir, file, read }])
    }

    fn load_credentials(&self) -> LayerState<Credentials> {
        let Some(config) = self.config() else {
            return LayerState::Unavailable(Unavailable {
                layer: Layer::Credentials,
                cause: cause_of(&self.config, Layer::Config),
            });
        };
        let dir = self.paths.credentials_dir();
        let mut files = Vec::new();
        let truncated = match list_dir(&dir, self.max_entries) {
            Ok(listing) => {
                files.extend(listing.entries.iter().map(|entry| {
                    let path = dir.join(&entry.name);
                    CredentialStat {
                        stat: stat_file(&path),
                        path,
                        origin: CredentialOrigin::Stored,
                    }
                }));
                listing.truncated
            }
            // The directory itself, as one that cannot be read.
            Err(err) => {
                files.push(CredentialStat {
                    path: dir.clone(),
                    origin: CredentialOrigin::Stored,
                    stat: FileStat::Unreadable { kind: err.kind() },
                });
                false
            }
        };
        for (source, settings) in &config.config.sources {
            if let Some(CredentialRef::File { file }) = &settings.credential {
                files.push(CredentialStat {
                    path: file.clone(),
                    origin: CredentialOrigin::Configured {
                        source: source.clone(),
                    },
                    stat: stat_file(file),
                });
            }
        }
        LayerState::Available(Credentials { files, truncated })
    }

    /// The inventory of the `index`th client of `world`.
    fn load_inventory(&self, world: &World, index: usize) -> LayerState<Inventory> {
        let unavailable = |cause| {
            LayerState::Unavailable(Unavailable {
                layer: Layer::Inventory,
                cause,
            })
        };
        let Some(target) = world.resolved.targets.get(index).map(|d| &d.target) else {
            return unavailable(Layer::Inventory);
        };
        // Its record and each registered sibling's say who owns a folder: without one,
        // shared folders would be misclassified.
        let record = match target.key {
            None => None,
            Some(key) => match self.inventory_record(key) {
                Some(record) => Some(record),
                None => return unavailable(Layer::Records),
            },
        };
        let mut shared = Vec::new();
        for sibling in siblings(target, world.resolved.targets.iter().map(|d| &d.target)) {
            let Some(key) = sibling.key.or_else(|| read_marker(&sibling.client_dir)) else {
                continue;
            };
            let Some(record) = self.inventory_record(key) else {
                return unavailable(Layer::Records);
            };
            shared.push((sibling.id.clone(), record));
        }
        match inventory_shared(target, record.as_ref(), &shared, &world.table, Depth::Full) {
            Ok(inventory) => LayerState::Available(inventory),
            // An `AddOns/` that cannot be listed: no failure mode describes it, and the
            // checks that need it are skipped for this client.
            Err(_) => unavailable(Layer::Inventory),
        }
    }

    /// `key`'s record as an inventory reads it: an empty one when it has none on disk,
    /// as `load_record` reads it; `None` when it cannot be read.
    fn inventory_record(&self, key: TargetKey) -> Option<TargetRecord> {
        match self.records()?.by_key.get(&key) {
            Some(LayerState::Available(loaded)) => Some(loaded.value.clone()),
            Some(LayerState::Unavailable(_)) => None,
            None => Some(TargetRecord::empty(key)),
        }
    }

    /// A layer whose load failed with a command's error: a finding when a failure mode
    /// describes it (a state file newer, corrupt or unreadable), else a diagnostic. The
    /// path is the error's (`details.path`), and `file` names it in titles (its file name
    /// when `None`). The error's message never reaches a finding.
    fn fail_error<T>(&self, layer: Layer, err: &Error, file: Option<&str>) -> LayerState<T> {
        let path = err.details["path"].as_str().map(PathBuf::from);
        let file = file.map_or_else(
            || {
                path.as_deref()
                    .and_then(Path::file_name)
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            },
            str::to_owned,
        );
        let failed = |fm, path| Failed {
            layer,
            fm,
            target: None,
            path,
            file: &file,
        };
        match (err.code, path.as_deref()) {
            (ErrorCode::StateTooNew, Some(path)) => {
                let evidence = Evidence {
                    observed: err.details["schema_version"].as_str().map(str::to_owned),
                    ..Evidence::default()
                };
                self.fail(
                    failed(ids::STATE_VERSION_NEWER, path),
                    FailureReason::Newer,
                    evidence,
                )
            }
            (ErrorCode::StateCorrupt, Some(path)) => self.fail(
                failed(ids::STATE_CORRUPT, path),
                FailureReason::Decode,
                Evidence::default(),
            ),
            (ErrorCode::Io, Some(path)) => self.fail(
                failed(ids::STATE_CORRUPT, path),
                FailureReason::Unreadable,
                Evidence::default(),
            ),
            _ => {
                self.diagnostics
                    .borrow_mut()
                    .push(format!("the {layer:?} layer did not load: {}", err.message));
                LayerState::Unavailable(Unavailable {
                    layer,
                    cause: layer,
                })
            }
        }
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

/// Why the layer in `cell` is unavailable: the layer whose failure made it so (`layer`
/// itself unless it records another).
fn cause_of<T>(cell: &OnceCell<LayerState<T>>, layer: Layer) -> Layer {
    match cell.get() {
        Some(LayerState::Unavailable(unavailable)) => unavailable.cause,
        _ => layer,
    }
}

/// The Discovery layer: the world, and one Inventory layer per discovered client, in
/// `world.resolved.targets`' order.
#[derive(Debug)]
struct Discovered {
    world: World,
    inventories: Vec<OnceCell<LayerState<Inventory>>>,
}

/// Whether a layer could be read; when not, the layer whose failure made it so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unavailable { cause: Layer },
}

/// The view's failures and the detectors' findings as one list: one finding per instance
/// (the first, so pass the view's failures first, and a row that reports a view failure
/// again never doubles it), by severity, failure mode, target id (none first) and
/// instance.
pub fn merge_findings(view: Vec<Finding>, detected: Vec<Finding>) -> Vec<Finding> {
    let mut seen = BTreeSet::new();
    let mut merged: Vec<Finding> = view
        .into_iter()
        .chain(detected)
        .filter(|finding| seen.insert(finding.instance.clone()))
        .collect();
    merged.sort_by(|a, b| {
        let order = |f: &Finding| {
            (
                f.severity,
                f.id,
                f.target.as_ref().map(|t| t.id.clone()),
                f.instance.clone(),
            )
        };
        order(a).cmp(&order(b))
    });
    merged
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

    /// Records as a test sets them.
    #[cfg(test)]
    pub(crate) fn from_states(
        by_key: BTreeMap<TargetKey, LayerState<Loaded<TargetRecord>>>,
    ) -> Self {
        Self { by_key }
    }

    /// Whether the layer looked for `key`'s record: the registry names it, or
    /// `<state>/targets/` holds it (read or not).
    pub fn contains(&self, key: &TargetKey) -> bool {
        self.by_key.contains_key(key)
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
            evidence.config_key.as_deref().unwrap_or("a key")
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
            evidence.config_key.as_deref().unwrap_or("the key")
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
enum ListingError {
    Unreadable,
    /// It holds more than the bound; `count` entries were visited.
    TooMany {
        count: u64,
    },
}

/// One directory's entries, as the Caches layer lists them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub dir: PathBuf,
    /// Sorted by name.
    pub entries: Vec<ListedEntry>,
    /// It holds more than [`DOCTOR_SCAN_MAX_ENTRIES`] entries and lists only that many: a
    /// detector reading it must report itself incomplete, never clean (M4-D43).
    pub truncated: bool,
}

/// One entry of a [`Listing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedEntry {
    /// Lossy for a name that is not UTF-8.
    pub name: String,
    pub kind: EntryKind,
    /// Its own size, not followed through a symlink and not summed over a directory.
    pub bytes: u64,
}

/// What a listed entry is, not following a symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    /// Anything else, or an entry that vanished or could not be stat'ed while listed.
    Other,
}

/// `dir`'s entries, at most `max_entries` of them, sorted by name, with one
/// `symlink_metadata` each; a missing `dir` is empty.
fn list_dir(dir: &Path, max_entries: u64) -> std::io::Result<Listing> {
    let mut listing = Listing {
        dir: dir.to_path_buf(),
        ..Listing::default()
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(listing),
        Err(err) => return Err(err),
    };
    for (visited, entry) in (0u64..).zip(entries) {
        let entry = entry?;
        if visited == max_entries {
            listing.truncated = true;
            break;
        }
        let (kind, bytes) = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => {
                let kind = metadata.file_type();
                let kind = if kind.is_symlink() {
                    EntryKind::Symlink
                } else if kind.is_dir() {
                    EntryKind::Dir
                } else if kind.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                };
                (kind, metadata.len())
            }
            Err(_) => (EntryKind::Other, 0),
        };
        listing.entries.push(ListedEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            bytes,
        });
    }
    listing.entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(listing)
}

/// The Journals layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Journals {
    /// Every journal that is not committed, or that recovery cannot read, oldest first.
    pub pending: Vec<Pending>,
    /// What recovery will do with each journal, both families (E11's classifier).
    pub diagnoses: Diagnoses,
    /// Each diagnosed transaction's client-side artifacts, counted.
    pub counts: BTreeMap<TxId, ArtifactCounts>,
}

/// The entries a transaction left on its clients' side, counted one level deep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArtifactCounts {
    /// Entries directly in its client stage directories.
    pub stage_folders: u64,
    /// Entries in its client snapshots' `AddOns/`, plus each `WTF.previous`: retention's
    /// possible-only-copy rule.
    pub snapshot_folders: u64,
    /// A listing passed its bound, so a count is a floor.
    pub truncated: bool,
}

impl Journals {
    /// Reads every journal: `journal::pending` (whose listing error is the layer's
    /// failure), then `journal::diagnose(paths, client_dirs, sv_writable)`, then the
    /// counts of each diagnosed transaction's client-side artifacts, each listing bounded
    /// by `max_entries`. Writes nothing; takes no lock.
    pub fn read(
        paths: &Paths,
        client_dirs: &[PathBuf],
        sv_writable: &dyn Fn(&Path) -> bool,
        max_entries: u64,
    ) -> Result<Self> {
        let pending = journal::pending(paths)?;
        let diagnoses = simulate::diagnose(paths, client_dirs, sv_writable);
        // The directory became unlistable between the two reads: as `pending`'s failure.
        if let Some(failure) = &diagnoses.unlisted {
            return Err(Error::io(
                &failure.dir,
                &std::io::Error::other(failure.error.clone()),
                Suggestion::command(["omaddons", "status"]),
            ));
        }
        let counts = diagnoses
            .journals
            .iter()
            .map(|d| (d.tx, artifact_counts(&d.evidence.artifacts, max_entries)))
            .collect();
        Ok(Self {
            pending,
            diagnoses,
            counts,
        })
    }
}

/// The client stage and client snapshot artifacts of one transaction, counted.
fn artifact_counts(artifacts: &[Artifact], max_entries: u64) -> ArtifactCounts {
    let mut counts = ArtifactCounts::default();
    for artifact in artifacts {
        match artifact.kind {
            ArtifactKind::ClientStage => {
                let (n, cut) = bounded_count(&artifact.path, max_entries);
                counts.stage_folders += n;
                counts.truncated |= cut;
            }
            ArtifactKind::ClientSnapshot => {
                let (n, cut) = bounded_count(&artifact.path.join("AddOns"), max_entries);
                let previous =
                    std::fs::symlink_metadata(artifact.path.join(retention::WTF_PREVIOUS)).is_ok();
                counts.snapshot_folders += n + u64::from(previous);
                counts.truncated |= cut;
            }
            _ => {}
        }
    }
    counts
}

/// The entries directly in `dir`, at most `max_entries` (then `true`): none when it is
/// absent, one when it cannot be read (kept, as retention keeps it, as if it held one).
fn bounded_count(dir: &Path, max_entries: u64) -> (u64, bool) {
    match std::fs::read_dir(dir) {
        Ok(listing) => {
            let mut n = 0;
            for _ in listing {
                if n == max_entries {
                    return (n, true);
                }
                n += 1;
            }
            (n, false)
        }
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (0, false),
        Err(_) => (1, false),
    }
}

/// The Caches layer: the cache directories omaddons keeps, one level deep.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caches {
    /// `<cache>/blobs/`.
    pub blobs: Listing,
}

/// The Widget layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WidgetLayer {
    /// `None` without Omarchy's config directory.
    pub info: Option<WidgetInfo>,
}

/// One installed agent skill's `SKILL.md`, read bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFile {
    pub dir: PathBuf,
    pub file: PathBuf,
    pub read: BoundedRead,
}

/// The Credentials layer: where secrets live, stat'ed. Nothing in it can hold a secret's
/// bytes: no credential file is opened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    /// The stored ones first, by name, then the configured ones, by source.
    pub files: Vec<CredentialStat>,
    /// `<config>/credentials/` holds more than [`DOCTOR_SCAN_MAX_ENTRIES`] entries and
    /// only that many are listed.
    pub truncated: bool,
}

/// One credential file and what a stat of it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStat {
    /// As stored, or as configured.
    pub path: PathBuf,
    pub origin: CredentialOrigin,
    pub stat: FileStat,
}

/// Why a credential file is listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialOrigin {
    /// An entry of `<config>/credentials/`.
    Stored,
    /// `[sources.<source>] credential = { file = … }`.
    Configured { source: String },
}

/// A file as `symlink_metadata` and `metadata` show it, never opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStat {
    Missing,
    /// A regular file (after following a symlink): its permission bits, owner and size,
    /// and whether the path itself is a symlink.
    Regular {
        mode: u32,
        uid: u32,
        bytes: u64,
        symlink: bool,
    },
    NotRegular {
        symlink: bool,
    },
    /// It exists but cannot be stat'ed; a dangling symlink too.
    Unreadable {
        kind: ErrorKind,
    },
}

/// `path`'s [`FileStat`].
fn stat_file(path: &Path) -> FileStat {
    let symlink = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type().is_symlink(),
        Err(err) if err.kind() == ErrorKind::NotFound => return FileStat::Missing,
        Err(err) => return FileStat::Unreadable { kind: err.kind() },
    };
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => FileStat::Regular {
            mode: metadata.mode() & 0o7777,
            uid: metadata.uid(),
            bytes: metadata.len(),
            symlink,
        },
        Ok(_) => FileStat::NotRegular { symlink },
        Err(err) => FileStat::Unreadable { kind: err.kind() },
    }
}

/// The keys of the records in `dir`: each name that is exactly `<TargetKey>.json`, the key
/// in its canonical form. A missing `dir` holds none. One listing, of at most
/// `max_entries` entries.
fn record_keys(
    dir: &Path,
    max_entries: u64,
) -> std::result::Result<BTreeSet<TargetKey>, ListingError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(_) => return Err(ListingError::Unreadable),
    };
    let mut keys = BTreeSet::new();
    for (visited, entry) in (0u64..).zip(entries) {
        let entry = entry.map_err(|_| ListingError::Unreadable)?;
        if visited == max_entries {
            return Err(ListingError::TooMany { count: visited });
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
    /// The Discovery layer, each client's inventory still to build.
    pub world: Option<LayerState<World>>,
    pub journals: Option<LayerState<Journals>>,
    pub lock: Option<LayerState<LockState>>,
    pub history: Option<LayerState<HistoryRead>>,
    pub caches: Option<LayerState<Caches>>,
    pub widget: Option<LayerState<WidgetLayer>>,
    pub skill: Option<LayerState<Vec<SkillFile>>>,
    pub credentials: Option<LayerState<Credentials>>,
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
        let world = preloaded.world.map(|state| match state {
            LayerState::Available(world) => {
                let inventories = world
                    .resolved
                    .targets
                    .iter()
                    .map(|_| OnceCell::new())
                    .collect();
                LayerState::Available(Discovered { world, inventories })
            }
            LayerState::Unavailable(unavailable) => LayerState::Unavailable(unavailable),
        });
        let discovered = cell(&mut loaded, Layer::Discovery, world);
        Self {
            config,
            registry,
            records,
            discovered,
            journals: cell(&mut loaded, Layer::Journals, preloaded.journals),
            lock: cell(&mut loaded, Layer::Lock, preloaded.lock),
            history: cell(&mut loaded, Layer::History, preloaded.history),
            caches: cell(&mut loaded, Layer::Caches, preloaded.caches),
            widget: cell(&mut loaded, Layer::Widget, preloaded.widget),
            skill: cell(&mut loaded, Layer::Skill, preloaded.skill),
            credentials: cell(&mut loaded, Layer::Credentials, preloaded.credentials),
            failures: RefCell::new(preloaded.failures),
            loaded: RefCell::new(loaded),
            ..Self::load_at(paths, ViewOptions::default())
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::doctor::finding::Severity;
    use crate::error::WarningCode;
    use crate::state::registry::{Origin, RegistryEntry};

    /// Planted in every broken file: no failure may show it.
    pub(crate) const MARKER: &str = "PLANTED-CONTENT-7f3a";
    pub(crate) const K: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";
    pub(crate) const K2: &str = "01M7A1B2C3D4E5F6G7H8J9K0MN";

    pub(crate) fn key(text: &str) -> TargetKey {
        text.parse().unwrap()
    }

    /// The doctor's default view of `paths`.
    pub(crate) fn view_at(paths: Paths) -> DoctorView {
        DoctorView::load_at(paths, ViewOptions::default())
    }

    pub(crate) fn home() -> (tempfile::TempDir, Paths) {
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

    pub(crate) fn is_root() -> bool {
        rustix::process::geteuid().is_root()
    }

    /// How a test leaves one file.
    pub(crate) enum Plant {
        Absent,
        Bytes(Vec<u8>),
        Dir,
        /// These bytes, at mode 000.
        Mode000(Vec<u8>),
        DanglingSymlink,
        /// `DISCOVERY_FILE_MAX_BYTES + 1` bytes, the marker first.
        TooLarge,
    }

    pub(crate) fn plant(path: &Path, plant: &Plant) {
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

    pub(crate) fn registry_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        store::encode(&Registry {
            targets: entries.iter().map(|(k, id)| entry(k, id)).collect(),
            default: None,
        })
        .unwrap()
    }

    pub(crate) fn record_bytes(key_text: &str, generation: u64) -> Vec<u8> {
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
                key: e.config_key.clone(),
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
            let view = view_at(paths.clone());
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
        let view = view_at(paths);
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
            let view = view_at(paths.clone());
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
        let view = view_at(paths);
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
            let mut view = view_at(paths.clone());
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
        assert_eq!(
            record_keys(&dir, 2),
            Err(ListingError::TooMany { count: 2 })
        );
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
        let view = view_at(paths.clone());
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
        let view = view_at(paths.clone());
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
            let view = view_at(paths);
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
            config_key: Some("check_interval_hours".to_owned()),
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

    /// Why the view's Discovery layer is unavailable, when it is.
    fn discovery_cause(view: &DoctorView) -> Option<Layer> {
        match view.discovered.get() {
            Some(LayerState::Unavailable(unavailable)) => Some(unavailable.cause),
            _ => None,
        }
    }

    #[test]
    fn discovery_layer_requires_config_and_registry() {
        let cases = [
            (
                "config_broken",
                Plant::Bytes(b"a = [\n".to_vec()),
                Plant::Absent,
                ViewDiscovery::Scan,
                Some(Layer::Config),
            ),
            (
                "registry_broken",
                Plant::Absent,
                Plant::Bytes(b"{".to_vec()),
                ViewDiscovery::Scan,
                Some(Layer::Registry),
            ),
            (
                "both_fine",
                Plant::Absent,
                Plant::Absent,
                ViewDiscovery::Scan,
                None,
            ),
            (
                "cached_mode",
                Plant::Absent,
                Plant::Absent,
                ViewDiscovery::Cached,
                None,
            ),
        ];
        for (case, config, registry, discovery, cause) in cases {
            let (_dir, paths) = home();
            plant(&paths.config_file(), &config);
            plant(&paths.registry_file(), &registry);
            let view = DoctorView::load_at(
                paths.clone(),
                ViewOptions {
                    discovery,
                    ..ViewOptions::default()
                },
            );
            let world = view.world();
            assert_eq!(
                (world.is_some(), discovery_cause(&view)),
                (cause.is_none(), cause),
                "case {case}: got available {}, cause {:?}, expected cause {cause:?}",
                world.is_some(),
                discovery_cause(&view)
            );
            let failures = view.failures();
            assert_eq!(
                failures.len(),
                usize::from(cause.is_some()),
                "case {case}: only the broken file's failure: {failures:?}"
            );
            if let Some(world) = world {
                assert!(world.resolved.targets.is_empty(), "case {case}");
                assert_eq!(
                    world.discovery.scanned,
                    discovery == ViewDiscovery::Scan,
                    "case {case}: {:?}",
                    world.discovery
                );
                assert_eq!(
                    world.github_token.source(),
                    crate::sources::github_token::TokenSource::None,
                    "case {case}"
                );
            }
            assert!(
                view.loaded_layers().contains(&Layer::Discovery),
                "case {case}: {:?}",
                view.loaded_layers()
            );
            assert!(
                !paths.cache_dir().exists(),
                "case {case}: loading the view wrote {}",
                paths.cache_dir().display()
            );
        }
    }

    /// A registered retail-like client at `client_dir`, as discovery finds it.
    pub(crate) fn client(id: &str, client_dir: &Path, key_text: Option<&str>) -> DiscoveredTarget {
        let mut target = crate::engine::testing::target(client_dir);
        target.id = id.to_owned();
        target.key = key_text.map(key);
        DiscoveredTarget {
            target,
            default_id: id.to_owned(),
            qualifier: "test".to_owned(),
            source: crate::discover::DiscoverySource::Scan,
        }
    }

    /// A world holding `targets`, as the view's Discovery layer.
    pub(crate) fn world_of(paths: &Paths, targets: Vec<DiscoveredTarget>) -> World {
        let mut world = World::assemble(
            paths.clone(),
            LoadedConfig::default(),
            Loaded {
                value: Registry::default(),
                version: Registry::CURRENT,
                status: VersionStatus::Current,
            },
            DiscoveryMode::NoScan,
            &mut |_| {},
        )
        .unwrap();
        world.resolved.targets = targets;
        world
    }

    pub(crate) fn records_of(states: Vec<(&str, bool)>) -> Records {
        let by_key = states
            .into_iter()
            .map(|(k, readable)| {
                let state = if readable {
                    LayerState::Available(Loaded {
                        value: TargetRecord::empty(key(k)),
                        version: TargetRecord::CURRENT,
                        status: VersionStatus::Current,
                    })
                } else {
                    LayerState::Unavailable(Unavailable {
                        layer: Layer::Records,
                        cause: Layer::Records,
                    })
                };
                (key(k), state)
            })
            .collect();
        Records { by_key }
    }

    /// Why the inventory of the `index`th client is unavailable, when it is.
    fn inventory_cause(view: &DoctorView, index: usize) -> Option<Layer> {
        match view.discovered.get() {
            Some(LayerState::Available(discovered)) => match discovered.inventories[index].get() {
                Some(LayerState::Unavailable(unavailable)) => Some(unavailable.cause),
                _ => None,
            },
            _ => None,
        }
    }

    #[test]
    fn inventory_layer_outcomes() {
        struct Case {
            name: &'static str,
            /// `(id, key)` of each client; the first is the one asked about.
            clients: Vec<(&'static str, Option<&'static str>)>,
            /// `(key, readable)` of each record; `None` leaves Records unavailable.
            records: Option<Vec<(&'static str, bool)>>,
            addons_mode_000: bool,
            /// `None`: available, listing `Foo`.
            cause: Option<Layer>,
        }
        let cases = vec![
            Case {
                name: "available",
                clients: vec![("retail", Some(K))],
                records: Some(vec![(K, true)]),
                addons_mode_000: false,
                cause: None,
            },
            Case {
                name: "registered_without_a_record_file",
                clients: vec![("retail", Some(K))],
                records: Some(Vec::new()),
                addons_mode_000: false,
                cause: None,
            },
            Case {
                name: "own_record_unavailable",
                clients: vec![("retail", Some(K))],
                records: Some(vec![(K, false)]),
                addons_mode_000: false,
                cause: Some(Layer::Records),
            },
            Case {
                name: "records_layer_unavailable",
                clients: vec![("retail", Some(K))],
                records: None,
                addons_mode_000: false,
                cause: Some(Layer::Records),
            },
            Case {
                name: "unregistered_needs_no_record",
                clients: vec![("retail", None)],
                records: None,
                addons_mode_000: false,
                cause: None,
            },
            Case {
                name: "keyed_sibling_record_unavailable",
                clients: vec![("retail", Some(K)), ("retail@wine", Some(K2))],
                records: Some(vec![(K, true), (K2, false)]),
                addons_mode_000: false,
                cause: Some(Layer::Records),
            },
            Case {
                name: "unkeyed_sibling_contributes_nothing",
                clients: vec![("retail", Some(K)), ("retail@wine", None)],
                records: Some(vec![(K, true)]),
                addons_mode_000: false,
                cause: None,
            },
            Case {
                name: "addons_mode_000",
                clients: vec![("retail", Some(K))],
                records: Some(vec![(K, true)]),
                addons_mode_000: true,
                cause: Some(Layer::Inventory),
            },
        ];
        for case in cases {
            let name = case.name;
            if case.addons_mode_000 && is_root() {
                continue;
            }
            let (_dir, paths) = home();
            // The first client's AddOns holds Foo; every other client's AddOns is a
            // symlink to it (a shared directory).
            let first = paths.home().join("Games/wow/_retail_");
            let addons = first.join("Interface/AddOns");
            plant(
                &addons.join("Foo/Foo.toc"),
                &Plant::Bytes(b"## Interface: 120105\n".to_vec()),
            );
            let mut targets = Vec::new();
            for (n, (id, k)) in case.clients.iter().enumerate() {
                let dir = paths.home().join(format!("Games/wow{n}/_retail_"));
                let client_dir = if n == 0 { first.clone() } else { dir };
                if n > 0 {
                    std::fs::create_dir_all(client_dir.join("Interface")).unwrap();
                    std::os::unix::fs::symlink(&addons, client_dir.join("Interface/AddOns"))
                        .unwrap();
                }
                targets.push(client(id, &client_dir, *k));
            }
            let asked = targets[0].clone();
            let records = case
                .records
                .map(|states| LayerState::Available(records_of(states)))
                .unwrap_or(LayerState::Unavailable(Unavailable {
                    layer: Layer::Records,
                    cause: Layer::Records,
                }));
            let view = DoctorView::preloaded(
                paths.clone(),
                Preloaded {
                    records: Some(records),
                    world: Some(LayerState::Available(world_of(&paths, targets))),
                    ..Preloaded::default()
                },
            );
            if case.addons_mode_000 {
                std::fs::set_permissions(&addons, std::fs::Permissions::from_mode(0o000)).unwrap();
            }
            let inventory = view.inventory(&asked);
            if case.addons_mode_000 {
                std::fs::set_permissions(&addons, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let names: Option<Vec<String>> =
                inventory.map(|inv| inv.folders.iter().map(|f| f.name.clone()).collect());
            match case.cause {
                None => assert_eq!(
                    names,
                    Some(vec!["Foo".to_owned()]),
                    "case {name}: got {names:?}, cause {:?}",
                    inventory_cause(&view, 0)
                ),
                Some(cause) => assert_eq!(
                    (names, inventory_cause(&view, 0)),
                    (None, Some(cause)),
                    "case {name}"
                ),
            }
            assert!(
                view.failures().is_empty(),
                "case {name}: no failure of its own"
            );
            assert!(
                view.loaded_layers().contains(&Layer::Inventory),
                "case {name}"
            );
        }
    }

    #[test]
    fn an_inventory_is_built_once_and_only_for_its_client() {
        let (_dir, paths) = home();
        let client_dir = paths.home().join("Games/wow/_retail_");
        let toc = client_dir.join("Interface/AddOns/Foo/Foo.toc");
        plant(&toc, &Plant::Bytes(b"## Interface: 120105\n".to_vec()));
        let retail = client("retail", &client_dir, Some(K));
        let elsewhere = client("era", &paths.home().join("Games/era"), None);
        let view = DoctorView::preloaded(
            paths.clone(),
            Preloaded {
                records: Some(LayerState::Available(records_of(vec![(K, true)]))),
                world: Some(LayerState::Available(world_of(
                    &paths,
                    vec![retail.clone()],
                ))),
                ..Preloaded::default()
            },
        );
        assert!(
            !view.loaded_layers().contains(&Layer::Inventory),
            "nothing built before an ask"
        );
        assert!(
            view.inventory(&elsewhere).is_none(),
            "a client not in the world"
        );
        let first = view.inventory(&retail).unwrap();
        assert_eq!(first.folders.len(), 1);
        std::fs::remove_dir_all(toc.parent().unwrap()).unwrap();
        let again = view.inventory(&retail).unwrap();
        assert!(std::ptr::eq(first, again), "built once");
        assert_eq!(again.folders.len(), 1, "the first build stands");
    }

    /// A committed install's history line for `key`.
    fn history_entry(tx: crate::model::TxId, k: TargetKey) -> history::HistoryEntry {
        use crate::model::{Actor, FolderName, TxKind};
        use crate::state::history::{FolderOp, HistoryEntry, HistoryOp, TxResult};
        HistoryEntry {
            schema_version: history::CURRENT,
            tx,
            kind: TxKind::Install,
            at: "2026-10-14T12:00:00Z".parse().unwrap(),
            target: Some(k),
            targets: Vec::new(),
            actor: Actor::Agent,
            reason: None,
            argv: vec!["omaddons".into(), "install".into()],
            ops: vec![HistoryOp {
                op: FolderOp::Add,
                folder: FolderName::new("Foo").unwrap(),
                addon: "Foo".into(),
                edit: None,
                backup: None,
                from: None,
                files: None,
            }],
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: None,
            backup: None,
            plan: None,
            doctor_run_id: None,
            repairs: Vec::new(),
        }
    }

    /// A journal that reached `committed`, with the journal module's own lines.
    pub(crate) fn committed_journal(tx: crate::model::TxId, k: TargetKey) -> String {
        use crate::engine::journal::{CURRENT, CommitPayload, IndexChanges, JournalLine, Prepared};
        let prepared = JournalLine::Prepared(Box::new(Prepared {
            schema_version: CURRENT,
            tx,
            target_key: k,
            client_dir: "/fixture/client".into(),
            kind: crate::model::TxKind::Install,
            started_at: "2026-10-14T12:00:00Z".parse().unwrap(),
            creates_dirs: Vec::new(),
            writes_wtf: false,
            staged_stamp: None,
            staged_files: None,
            source_backup: None,
            client: None,
            commit: CommitPayload {
                record: TargetRecord::empty(k),
                index: IndexChanges::default(),
                history: history_entry(tx, k),
                stage_dir: "/fixture/client/Interface/.omaddons/stage/x".into(),
                sv_restore: None,
            },
        }));
        [prepared, JournalLine::Committed]
            .iter()
            .map(|line| format!("{}\n", serde_json::to_string(line).unwrap()))
            .collect()
    }

    /// The view's one failure, as `(fm, reason, path)`.
    fn only_failure(view: &DoctorView) -> Vec<(&'static str, &'static str, PathBuf)> {
        view.failures()
            .into_iter()
            .map(|f| (f.fm, f.evidence.reason.unwrap_or_default(), f.path))
            .collect()
    }

    pub(crate) fn set_mode(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn journals_layer_outcomes() {
        use crate::engine::journal::Unrecoverable;
        let tx: crate::model::TxId = "01M7A1B2C3D4E5F6G7H8J9K0MP".parse().unwrap();

        let (_dir, paths) = home();
        let view = view_at(paths);
        assert_eq!(
            view.journals().map(|j| j.pending.len()),
            Some(0),
            "case no_journal_dir"
        );

        let (_dir, paths) = home();
        let line = format!(r#"{{"type":"prepared","schema_version":"1.1","target_key":"{K}"}}"#);
        plant(
            &paths.journal_dir().join(format!("{tx}.jsonl")),
            &Plant::Bytes(format!("{line}\n").into_bytes()),
        );
        let view = view_at(paths);
        assert_eq!(
            view.journals().map(|j| j.pending.clone()),
            Some(vec![Pending {
                tx,
                target: Some(key(K)),
                unrecoverable: Some(Unrecoverable::Unreadable),
            }]),
            "case unreadable_journal_naming_a_key"
        );

        let (_dir, paths) = home();
        plant(
            &paths.journal_dir().join(format!("{tx}.jsonl")),
            &Plant::Bytes(committed_journal(tx, key(K)).into_bytes()),
        );
        let view = view_at(paths);
        assert_eq!(
            view.journals().map(|j| j.pending.len()),
            Some(0),
            "case committed_journal"
        );

        if is_root() {
            return;
        }
        let (_dir, paths) = home();
        let dir = paths.journal_dir();
        plant(
            &dir.join(format!("{tx}.jsonl")),
            &Plant::Bytes(committed_journal(tx, key(K)).into_bytes()),
        );
        set_mode(&dir, 0o000);
        let view = view_at(paths);
        let journals = view.journals().cloned();
        let failures = only_failure(&view);
        set_mode(&dir, 0o755);
        assert_eq!(journals, None, "case journal_dir_mode_000");
        assert_eq!(
            failures,
            [(ids::STATE_CORRUPT, "unreadable", dir)],
            "case journal_dir_mode_000"
        );
        assert_eq!(view.failures()[0].title, "journal/ cannot be read");
    }

    /// Every path below `root` with its length: what a read must leave as it was.
    fn tree(root: &Path) -> Vec<(PathBuf, u64)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(at) = stack.pop() {
            for entry in std::fs::read_dir(&at).into_iter().flatten().flatten() {
                let meta = entry.metadata().unwrap();
                if meta.is_dir() {
                    stack.push(entry.path());
                }
                out.push((entry.path(), meta.len()));
            }
        }
        out.sort();
        out
    }

    /// A prepared journal on a marked client, a stage of three entries, and a snapshot
    /// with two `AddOns/` entries and a `WTF.previous`.
    fn journal_with_artifacts(paths: &Paths, tx: TxId) -> PathBuf {
        let client = paths.home().join("retail");
        crate::engine::testing::mark(&client, key(K));
        let prepared = committed_journal(tx, key(K));
        let prepared = prepared
            .lines()
            .next()
            .unwrap()
            .replace("/fixture/client", &client.display().to_string());
        plant(
            &paths.journal_dir().join(format!("{tx}.jsonl")),
            &Plant::Bytes(format!("{prepared}\n").into_bytes()),
        );
        let client_paths = crate::state::paths::ClientPaths::new(&client);
        for name in ["A", "B", "C"] {
            plant(&client_paths.stage(&tx).join(name), &Plant::Dir);
        }
        let snapshot = client_paths.snapshots(&tx);
        for name in ["A", "B"] {
            plant(&snapshot.join("AddOns").join(name), &Plant::Dir);
        }
        plant(
            &snapshot.join(retention::WTF_PREVIOUS),
            &Plant::Bytes(b"x".to_vec()),
        );
        client
    }

    #[test]
    fn journals_layer_reads_diagnoses_and_counts() {
        let (_dir, paths) = home();
        let tx: TxId = "01M7A1B2C3D4E5F6G7H8J9K0MP".parse().unwrap();
        let client = journal_with_artifacts(&paths, tx);
        let before = tree(paths.home());
        let read = Journals::read(&paths, &[client], &|_| false, DOCTOR_SCAN_MAX_ENTRIES).unwrap();
        assert_eq!(tree(paths.home()), before, "the read writes nothing");
        assert_eq!(read.diagnoses.journals.len(), 1, "{:?}", read.diagnoses);
        assert_eq!(read.pending.len(), 1);
        assert_eq!(
            read.counts.get(&tx),
            Some(&ArtifactCounts {
                stage_folders: 3,
                snapshot_folders: 3,
                truncated: false,
            })
        );
    }

    #[test]
    fn the_counts_stop_at_the_limit() {
        let (_dir, paths) = home();
        let tx: TxId = "01M7A1B2C3D4E5F6G7H8J9K0MP".parse().unwrap();
        let client = journal_with_artifacts(&paths, tx);
        let read = Journals::read(&paths, &[client], &|_| false, 2).unwrap();
        let counts = read.counts[&tx];
        assert_eq!(
            (counts.stage_folders, counts.truncated),
            (2, true),
            "{counts:?}"
        );
    }

    /// Discovery is used when it loads, never required: without it the layer still reads
    /// every journal (and, knowing no client, finds no client-side artifact).
    #[test]
    fn journals_layer_loads_without_discovery() {
        let (_dir, paths) = home();
        let tx: TxId = "01M7A1B2C3D4E5F6G7H8J9K0MP".parse().unwrap();
        journal_with_artifacts(&paths, tx);
        let view = DoctorView::preloaded(
            paths,
            Preloaded {
                world: Some(LayerState::Unavailable(Unavailable {
                    layer: Layer::Discovery,
                    cause: Layer::Config,
                })),
                ..Preloaded::default()
            },
        );
        let journals = view.journals().unwrap();
        assert_eq!(journals.diagnoses.journals.len(), 1);
        assert_eq!(journals.counts.get(&tx).map(|c| c.stage_folders), Some(0));
    }

    #[test]
    fn the_counts_agree_with_retentions_only_copy_rule() {
        let (_dir, paths) = home();
        let snapshot = |name: &str, addons: &[&str], previous: bool| {
            let dir = paths.home().join(name);
            plant(&dir.join("keep"), &Plant::Dir);
            for entry in addons {
                plant(&dir.join("AddOns").join(entry), &Plant::Dir);
            }
            if previous {
                plant(
                    &dir.join(retention::WTF_PREVIOUS),
                    &Plant::Bytes(b"x".to_vec()),
                );
            }
            dir
        };
        for (case, dir) in [
            ("empty_snapshot", snapshot("empty", &[], false)),
            ("two_addons_entries", snapshot("two", &["A", "B"], false)),
            ("wtf_previous_alone", snapshot("previous", &[], true)),
            ("both", snapshot("both", &["A", "B"], true)),
        ] {
            let ours = artifact_counts(
                &[Artifact {
                    path: dir.clone(),
                    kind: ArtifactKind::ClientSnapshot,
                    client: None,
                    marker_matches: None,
                }],
                DOCTOR_SCAN_MAX_ENTRIES,
            )
            .snapshot_folders;
            let retention = retention::snapshot_folders(&dir) as u64;
            assert_eq!(ours, retention, "{case}");
        }
    }

    #[test]
    fn lock_layer_outcomes() {
        use crate::fsx::GlobalLock;

        let (_dir, paths) = home();
        assert_eq!(
            view_at(paths).lock(),
            Some(&LockState::Missing),
            "case no_lock_file"
        );

        let (_dir, paths) = home();
        drop(GlobalLock::acquire(&paths, "test").unwrap());
        assert_eq!(
            view_at(paths.clone()).lock(),
            Some(&LockState::Free),
            "case free"
        );

        let guard = GlobalLock::acquire(&paths, "test").unwrap();
        let held = view_at(paths.clone()).lock().cloned();
        drop(guard);
        assert!(
            matches!(held, Some(LockState::Held { .. })),
            "case held: got {held:?}"
        );

        if is_root() {
            return;
        }
        set_mode(&paths.lock_file(), 0o000);
        let view = view_at(paths.clone());
        assert_eq!(view.lock(), None, "case unopenable");
        assert_eq!(
            only_failure(&view),
            [(ids::STATE_CORRUPT, "unreadable", paths.lock_file())],
            "case unopenable: the failure's path is the error's"
        );
    }

    #[test]
    fn history_layer_outcomes() {
        let (_dir, paths) = home();
        let view = view_at(paths);
        let read = view.history().unwrap();
        assert_eq!(
            (read.entries.len(), read.skipped_lines),
            (0, 0),
            "case missing"
        );

        let (_dir, paths) = home();
        let line = |n: &str| {
            let tx = n.parse().unwrap();
            serde_json::to_string(&history_entry(tx, key(K))).unwrap()
        };
        let text = format!(
            "{}\n{MARKER} not json\n{}\n",
            line("01M7A1B2C3D4E5F6G7H8J9K0MP"),
            line("01M7A1B2C3D4E5F6G7H8J9K0MQ")
        );
        plant(&paths.history_file(), &Plant::Bytes(text.into_bytes()));
        let view = view_at(paths);
        let read = view.history().unwrap();
        assert_eq!(
            (read.entries.len(), read.skipped_lines),
            (2, 1),
            "case two_good_one_bad"
        );
        assert_content_free("two_good_one_bad", &view);

        if is_root() {
            return;
        }
        let (_dir, paths) = home();
        plant(
            &paths.history_file(),
            &Plant::Mode000(format!("{MARKER}\n").into_bytes()),
        );
        let view = view_at(paths.clone());
        assert!(view.history().is_none(), "case mode_000");
        assert_eq!(
            only_failure(&view),
            [(ids::STATE_CORRUPT, "unreadable", paths.history_file())],
            "case mode_000"
        );
        assert_content_free("mode_000", &view);
    }

    #[test]
    fn caches_layer_outcomes() {
        let (_dir, paths) = home();
        let caches = view_at(paths.clone()).caches().cloned().unwrap();
        assert_eq!(
            (
                caches.blobs.entries.len(),
                caches.blobs.truncated,
                caches.blobs.dir
            ),
            (0, false, paths.blobs_dir()),
            "case no_blobs_dir"
        );

        let (_dir, paths) = home();
        let blobs = paths.blobs_dir();
        for (name, bytes) in [("b", 2), ("a", 1), ("c", 3)] {
            plant(&blobs.join(name), &Plant::Bytes(vec![b'x'; bytes]));
        }
        std::fs::create_dir(blobs.join("d")).unwrap();
        let caches = view_at(paths.clone()).caches().cloned().unwrap();
        let got: Vec<(String, EntryKind, Option<u64>)> = caches
            .blobs
            .entries
            .iter()
            .map(|e| {
                let bytes = (e.kind == EntryKind::File).then_some(e.bytes);
                (e.name.clone(), e.kind, bytes)
            })
            .collect();
        let expected = vec![
            ("a".to_owned(), EntryKind::File, Some(1)),
            ("b".to_owned(), EntryKind::File, Some(2)),
            ("c".to_owned(), EntryKind::File, Some(3)),
            ("d".to_owned(), EntryKind::Dir, None),
        ];
        assert_eq!(got, expected, "case three_blobs_and_a_subdir");
        assert!(!caches.blobs.truncated);

        let mut view = view_at(paths.clone());
        view.max_entries = 2;
        let listing = &view.caches().unwrap().blobs;
        assert_eq!(
            (listing.entries.len(), listing.truncated),
            (2, true),
            "case over_the_bound"
        );

        if is_root() {
            return;
        }
        set_mode(&blobs, 0o000);
        let view = view_at(paths);
        let caches = view.caches().cloned();
        let failures = view.failures();
        set_mode(&blobs, 0o755);
        assert_eq!(caches, None, "case blobs_dir_mode_000");
        assert_eq!(failures, Vec::new(), "case blobs_dir_mode_000: no failure");
        assert!(view.loaded_layers().contains(&Layer::Caches));
    }

    #[test]
    fn widget_layer_without_omarchy() {
        let (_dir, paths) = home();
        let view = view_at(paths);
        assert_eq!(view.widget(), Some(&WidgetLayer { info: None }));
        assert_eq!(view.loaded_layers(), BTreeSet::from([Layer::Widget]));
    }

    #[test]
    fn skill_layer_outcomes() {
        let cases = [
            ("missing", Plant::Absent, "Missing"),
            ("present", Plant::Bytes(b"# omaddons\n".to_vec()), "Bytes"),
            ("oversized", Plant::TooLarge, "TooLarge"),
            ("directory", Plant::Dir, "NotAFile"),
        ];
        for (case, how, expected) in cases {
            let (_dir, paths) = home();
            let file = paths.skill_dir().join("SKILL.md");
            plant(&file, &how);
            let view = view_at(paths.clone());
            let skill = view.skill().unwrap();
            assert_eq!(skill.len(), 1, "case {case}");
            assert_eq!(
                (&skill[0].dir, &skill[0].file),
                (&paths.skill_dir(), &file),
                "case {case}"
            );
            let read = &skill[0].read;
            let got = match read {
                BoundedRead::Missing => "Missing",
                BoundedRead::Bytes(bytes) => {
                    assert_eq!(bytes, b"# omaddons\n", "case {case}");
                    "Bytes"
                }
                BoundedRead::TooLarge { bytes } => {
                    assert_eq!(*bytes, DISCOVERY_FILE_MAX_BYTES + 1, "case {case}");
                    "TooLarge"
                }
                BoundedRead::NotAFile => "NotAFile",
                BoundedRead::Unreadable { .. } => "Unreadable",
            };
            assert_eq!(got, expected, "case {case}: got {read:?}");
        }
    }

    #[test]
    fn credentials_layer_outcomes() {
        // What each credential file holds: never in the layer.
        const HELD: &str = "planted-credential-bytes-7f3a";
        let uid = rustix::process::getuid().as_raw();
        let regular = |mode, symlink| FileStat::Regular {
            mode,
            uid,
            bytes: HELD.len() as u64 + 1,
            symlink,
        };
        type Expect = Vec<(CredentialOrigin, FileStat)>;
        /// `(case, config.toml given the token's directory, stored mode, expected stats)`.
        type Case = (&'static str, fn(&Path) -> String, Option<u32>, Expect);
        let configured = || CredentialOrigin::Configured {
            source: "github".to_owned(),
        };
        let cases: Vec<Case> = vec![
            (
                "stored_0600",
                |_| String::new(),
                Some(0o600),
                vec![(CredentialOrigin::Stored, regular(0o600, false))],
            ),
            (
                "configured_0644",
                |at| {
                    format!(
                        "[sources.github]\ncredential = {{ file = {:?} }}\n",
                        at.join("token")
                    )
                },
                None,
                vec![(configured(), regular(0o644, false))],
            ),
            (
                "configured_symlink",
                |at| {
                    format!(
                        "[sources.github]\ncredential = {{ file = {:?} }}\n",
                        at.join("link")
                    )
                },
                None,
                vec![(configured(), regular(0o600, true))],
            ),
            (
                "configured_missing",
                |at| {
                    format!(
                        "[sources.github]\ncredential = {{ file = {:?} }}\n",
                        at.join("gone")
                    )
                },
                None,
                vec![(configured(), FileStat::Missing)],
            ),
            (
                "env_not_listed",
                |_| "[sources.github]\ncredential = { env = \"GH_TOKEN\" }\n".to_owned(),
                None,
                Vec::new(),
            ),
            (
                "command_not_listed",
                |_| "[sources.github]\ncredential = { command = [\"pass\", \"gh\"] }\n".to_owned(),
                None,
                Vec::new(),
            ),
        ];
        for (case, config, stored, expected) in cases {
            let (_dir, paths) = home();
            let at = paths.home().join("secrets");
            plant(
                &at.join("token"),
                &Plant::Bytes(format!("{HELD}\n").into_bytes()),
            );
            set_mode(&at.join("token"), 0o644);
            plant(
                &at.join("private"),
                &Plant::Bytes(format!("{HELD}\n").into_bytes()),
            );
            set_mode(&at.join("private"), 0o600);
            std::os::unix::fs::symlink(at.join("private"), at.join("link")).unwrap();
            if let Some(mode) = stored {
                let file = paths.credentials_dir().join("github");
                plant(&file, &Plant::Bytes(format!("{HELD}\n").into_bytes()));
                set_mode(&file, mode);
            }
            plant(
                &paths.config_file(),
                &Plant::Bytes(config(&at).into_bytes()),
            );
            let view = view_at(paths.clone());
            let credentials = view.credentials();
            assert!(credentials.is_some(), "case {case}: unavailable");
            let credentials = credentials.unwrap();
            let got: Expect = credentials
                .files
                .iter()
                .map(|c| (c.origin.clone(), c.stat.clone()))
                .collect();
            assert_eq!(got, expected, "case {case}: {credentials:?}");
            assert!(
                !format!("{credentials:?}").contains(HELD),
                "case {case}: a credential's bytes reached the layer"
            );
        }

        let (_dir, paths) = home();
        plant(&paths.config_file(), &Plant::Bytes(b"a = [\n".to_vec()));
        let view = view_at(paths);
        assert!(view.credentials().is_none(), "case config_broken");
        assert_eq!(
            view.credentials.get(),
            Some(&LayerState::Unavailable(Unavailable {
                layer: Layer::Credentials,
                cause: Layer::Config,
            })),
            "case config_broken"
        );
    }

    #[test]
    fn state_corrupt_detector_covers_every_state_layer() {
        use crate::doctor::budget::Budget;
        use crate::doctor::selection::Selection;
        use crate::fsx::GlobalLock;

        if is_root() {
            return;
        }
        let (_dir, paths) = home();
        drop(GlobalLock::acquire(&paths, "test").unwrap());
        set_mode(&paths.lock_file(), 0o000);
        plant(&paths.registry_file(), &Plant::Bytes(b"{".to_vec()));
        plant(&paths.target_record(&key(K)), &Plant::Bytes(b"[".to_vec()));
        plant(&paths.history_file(), &Plant::Mode000(b"x\n".to_vec()));
        let journal = paths.journal_dir();
        std::fs::create_dir_all(&journal).unwrap();
        set_mode(&journal, 0o000);
        let view = view_at(paths.clone());
        let row = registry::row(ids::STATE_CORRUPT).unwrap();
        let found = (row.detect.unwrap())(&view, &Selection::all(), &mut Budget::unlimited());
        set_mode(&journal, 0o755);
        let mut paths_found: Vec<PathBuf> = found
            .found
            .iter()
            .filter_map(|obs| obs.evidence.path.clone())
            .collect();
        paths_found.sort();
        let mut expected = vec![
            paths.registry_file(),
            paths.target_record(&key(K)),
            journal,
            paths.lock_file(),
            paths.history_file(),
        ];
        expected.sort();
        assert_eq!(paths_found, expected);
    }

    #[test]
    fn merge_keeps_one_finding_per_instance() {
        let (_dir, paths) = home();
        plant(&paths.config_file(), &Plant::Bytes(b"a = [\n".to_vec()));
        plant(&paths.registry_file(), &Plant::Bytes(b"{".to_vec()));
        let view = view_at(paths);
        view.config();
        view.registry();
        let from_view = view.failure_findings();
        assert_eq!(from_view.len(), 2);
        // A detector that reports the registry's failure again, under another title.
        let mut again = from_view[0].clone();
        again.title = "reported twice".to_owned();
        let merged = merge_findings(from_view.clone(), vec![again]);
        let got: Vec<(&str, Severity, &str)> = merged
            .iter()
            .map(|f| (f.id, f.severity, f.title.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (ids::STATE_CORRUPT, Severity::P0, "registry.json is corrupt"),
                (
                    ids::CONFIG_PARSE_ERROR,
                    Severity::P1,
                    from_view[1].title.as_str()
                ),
            ],
            "one per instance, the view's first, by severity"
        );
        // Order does not depend on the input's.
        let reversed = merge_findings(Vec::new(), from_view.into_iter().rev().collect());
        assert_eq!(reversed, merged);
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
