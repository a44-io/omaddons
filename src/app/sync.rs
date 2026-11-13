//! `omaddons sync [<file>|-] [-t T] [--strict] [--no-deps]`: makes each client match the
//! desired-state file (`<config>/omaddons/addons.toml` unless named; `-` reads it from
//! standard input, and a plan of that keeps the text), idempotently (vision D32).
//!
//! Each selected target takes its family's section (live targets only) and its own
//! `[target."<id>"]` section, each entry resolved as `install` reads its arguments. A
//! missing addon is installed; a pinned entry at another version installs that version
//! pinned; an entry on another channel switches it; a pin that differs is a records-only
//! edit. An installed addon is never moved to a newer release (M2-D20: `update` does
//! that), so a second run changes nothing and asks no source anything. Each target with
//! changes commits one transaction of kind `sync`, all under one run, so `omaddons undo`
//! reverts the whole sync.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};

use super::approval::{Approval, ApprovalFlags};
use super::engine::{Engine, recover_waiting};
use super::intent::{self, AbsPath, Preview, Storable, StorableArgs, TargetScope};
use super::outcome::{
    ItemDeps, ItemError, ItemRequest, ItemResult, ItemStatus, TargetRef, TargetResult, TargetsData,
    TargetsOutcome, sort_keeping_dependencies,
};
use super::precondition::{self, Idle, PlanBinding, PlanExtras, Precondition};
use super::resolve_args::{catalog_name, resolve_entry};
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::engine::edit::RecordEdit;
use crate::engine::plan::{AnyFlavorIntent, PinIntent, RequestedChange};
use crate::engine::{Binding, TxOutcomes, TxRequest, TxTarget};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::limits::SYNC_STDIN_MAX_BYTES;
use crate::model::{AddonReason, AddonRecord, Ref, RunId, Target, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::politeness::SOURCE_RECHECK_MIN_INTERVAL;
use crate::resolve::cache::Freshness;
use crate::resolve::catalog::Catalog;
use crate::rules::TargetKind;
use crate::sources::changelog::same_version;
use crate::sources::refs::local_path_ref;
use crate::state::desired::{self, DesiredState, Entry};
use crate::state::records::TargetRecord;

/// The `FILE` that reads the desired state from standard input.
pub const STDIN: &str = "-";
/// How messages name standard input where they would name the file.
const STDIN_NAME: &str = "<stdin>";

/// `sync`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SyncArgs {
    /// The desired-state file, or - for stdin; default: addons.toml in config
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,
    /// The text a stored plan's `sync -` read, which `apply` syncs instead of reading
    /// standard input again; never a command-line argument.
    #[arg(skip)]
    pub input: Option<String>,
    /// Exit 6 when only some of the addons sync
    #[arg(long)]
    pub strict: bool,
    /// Install only the listed addons, not the ones their TOCs require
    #[arg(long)]
    pub no_deps: bool,
    /// Install the versions the file records, from the blob cache when cached
    #[arg(long)]
    pub locked: bool,
    /// Also remove managed addons the file does not list
    ///
    /// Never one a listed addon requires or a lock names. Refused while a target section
    /// names no known client.
    #[arg(long)]
    pub prune: bool,
}

/// `sync`'s outcome: the shared multi-target outcome, with `--strict`.
#[derive(Debug)]
pub struct SyncOutcome {
    inner: TargetsOutcome,
    strict: bool,
    /// Each committed transaction's global lock hold time, for `-v` log lines.
    pub locks: Vec<(TxId, u64)>,
    /// For `-v` log lines.
    pub notes: Vec<String>,
}

impl Storable for SyncOutcome {
    fn take_preview(&mut self) -> Option<Preview> {
        self.inner.take_preview()
    }

    fn record_plan(
        &mut self,
        plan_id: crate::model::PlanId,
        expires_at: jiff::Timestamp,
        hint: crate::error::CommandHint,
    ) {
        self.inner.record_plan(plan_id, expires_at, hint);
    }
}

impl Outcome for SyncOutcome {
    type Data = TargetsData;

    fn command(&self) -> &'static str {
        "sync"
    }

    fn data(&self) -> &TargetsData {
        &self.inner.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.inner.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.inner.commands
    }

    fn partial(&self) -> bool {
        self.inner.data.partial()
    }

    fn assertion_failure(&self) -> Option<Error> {
        let failed = self.inner.data.summary.failed;
        (self.strict && self.partial()).then(|| {
            let noun = if failed == 1 { "addon" } else { "addons" };
            Error::new(
                ErrorCode::CheckFailed,
                format!("{failed} {noun} did not sync (--strict)"),
                Suggestion::text("see data.targets[].items[].error for each failed addon"),
            )
        })
    }

    fn render_human(&self, w: &mut dyn std::io::Write, style: &Style) -> std::io::Result<()> {
        self.inner.render_human(w, style)
    }
}

/// One resolved entry a target requests (locks are not requests).
#[derive(Debug, Clone)]
pub struct Want<'s> {
    pub entry: &'s Entry,
    pub r#ref: Ref,
    /// The name a new install takes: the catalog's for a catalog ref, else the entry's
    /// `name` when it also gives a `ref`; else the archive decides.
    pub name: Option<String>,
}

/// What `sync` does about one entry on one target.
#[derive(Debug, Clone)]
pub struct DiffItem<'s, 'r> {
    pub entry: &'s Entry,
    /// The installed addon with the entry's ref, if any.
    pub installed: Option<&'r AddonRecord>,
    /// `None`: it matches already.
    pub change: Option<RequestedChange>,
}

/// The changes that make `record` match `wants`, one per entry at most (first matching
/// row wins), never moving an installed addon to a newer release by itself:
///
/// | situation | change | under `locked` |
/// |---|---|---|
/// | not installed | install (its `version` only when pinned) | its `version` whenever it has one |
/// | pinned at another `version` | install that version, pinned | the same |
/// | at another `version` | nothing | install that version, its pin as the entry says |
/// | another `channel` | install from that channel (a records-only switch when its newest release is installed) | at its `version` |
/// | another `channel`, pinned here and in the file, no `version` | a records-only switch that keeps the release and the pin | the same |
/// | only the pin differs | records-only `SetPinned` | the same |
/// | otherwise | nothing | nothing |
///
/// A lock never pins: an unpinned entry installs its version unpinned. A locked install
/// carries the entry's `sha256`, so its archive may come from the blob cache and no other
/// archive installs, and its `revision` (WowAce), so it is that tree; the same version at
/// another revision is "another `version`". An entry with no version (a `url:` release is
/// its URL) is locked by its `sha256` alone.
pub fn diff<'s, 'r>(
    wants: &[Want<'s>],
    record: Option<&'r TargetRecord>,
    locked: bool,
) -> Vec<DiffItem<'s, 'r>> {
    wants
        .iter()
        .map(|w| {
            let e = w.entry;
            let installed = record.and_then(|r| {
                r.addons
                    .values()
                    .find(|a| a.r#ref.identity() == w.r#ref.identity())
            });
            // The entry's `any_flavor` against the record: set or cleared to match.
            let any_flavor = match installed {
                Some(a) if a.any_flavor == e.any_flavor => AnyFlavorIntent::Keep,
                _ if e.any_flavor => AnyFlavorIntent::Set,
                Some(_) => AnyFlavorIntent::Clear,
                None => AnyFlavorIntent::Keep,
            };
            let install = |r#ref: Ref, name: Option<String>, version: Option<String>, pin| {
                // The lock: the recorded archive's hash, with the recorded version; for an
                // entry with no version to record (a `url:` release is its URL), the hash
                // alone is the lock, so the URL's content cannot change silently.
                let blob_sha256 = e
                    .sha256
                    .clone()
                    .filter(|_| locked && (version.is_some() || e.version.is_none()));
                // The recorded revision goes with the recorded version.
                let revision = e.revision.filter(|_| version.is_some());
                Some(RequestedChange::Install {
                    r#ref,
                    channel: e.channel,
                    channel_explicit: true,
                    version,
                    file: None,
                    name,
                    pin,
                    reason: AddonReason::Explicit,
                    reinstall: false,
                    blob_sha256,
                    asset: None,
                    revision,
                    any_flavor,
                })
            };
            // The version an install takes: pinned ones always, every one when locked.
            let pinned_version = || e.version.clone().filter(|_| e.pinned || locked);
            let change = match installed {
                None => {
                    let pin = if e.pinned {
                        PinIntent::Pin
                    } else {
                        PinIntent::Keep
                    };
                    install(w.r#ref.clone(), w.name.clone(), pinned_version(), pin)
                }
                Some(a) => {
                    let again = || (a.r#ref.clone(), Some(a.name.clone()));
                    let pin = if e.pinned == a.pinned {
                        PinIntent::Keep
                    } else if e.pinned {
                        PinIntent::Pin
                    } else {
                        PinIntent::Unpin
                    };
                    // At another version, or (WowAce) the same one at another revision.
                    // A version names the release as `install --version` reads it: a
                    // leading `v` aside (`1.2.3` finds `v1.2.3`), a sha by its prefix.
                    let moved = e.revision.is_some_and(|r| a.revision != Some(r));
                    let elsewhere = e
                        .version
                        .as_ref()
                        .is_some_and(|v| !same_version(v, &a.version) || moved);
                    if e.pinned && elsewhere {
                        let (r, name) = again();
                        install(r, name, e.version.clone(), PinIntent::Pin)
                    } else if locked && elsewhere {
                        let (r, name) = again();
                        install(r, name, e.version.clone(), pin)
                    } else if e.channel != a.channel {
                        let (r, name) = again();
                        let version = pinned_version();
                        // Pinned both here and in the file, with no version to move it:
                        // the switch keeps the release and the pin and records the
                        // channel. `Keep` would be the engine's pinned no-op instead.
                        let pin = if a.pinned && e.pinned && version.is_none() {
                            PinIntent::Pin
                        } else {
                            pin
                        };
                        install(r, name, version, pin)
                    } else if e.pinned != a.pinned && any_flavor != AnyFlavorIntent::Keep {
                        // Both settings at once: an install that keeps its release.
                        let (r, name) = again();
                        install(r, name, None, pin)
                    } else if e.pinned != a.pinned {
                        Some(RequestedChange::Edit {
                            addon: a.name.clone(),
                            edit: RecordEdit::SetPinned(e.pinned),
                        })
                    } else if any_flavor != AnyFlavorIntent::Keep {
                        Some(RequestedChange::Edit {
                            addon: a.name.clone(),
                            edit: RecordEdit::SetAnyFlavor(e.any_flavor),
                        })
                    } else {
                        None
                    }
                }
            };
            DiffItem {
                entry: e,
                installed,
                change,
            }
        })
        .collect()
}

/// The records `--prune` keeps on a target (by record key): those whose ref one of
/// `refs` names (desired entries, locks, and every ref a failed entry may mean), then,
/// to a fixpoint, every record a kept one requires, whatever its `reason`.
pub fn keep_set(record: &TargetRecord, refs: &[Ref]) -> BTreeSet<String> {
    let named: BTreeSet<String> = refs.iter().map(Ref::identity).collect();
    let mut keep: BTreeSet<String> = record
        .addons
        .iter()
        .filter(|(_, a)| named.contains(&a.r#ref.identity()))
        .map(|(key, _)| key.clone())
        .collect();
    loop {
        let required: Vec<String> = record
            .addons
            .iter()
            .filter(|(key, _)| !keep.contains(*key))
            .filter(|(_, a)| {
                a.required_by
                    .iter()
                    .any(|by| keep.contains(&TargetRecord::addon_key(by)))
            })
            .map(|(key, _)| key.clone())
            .collect();
        if required.is_empty() {
            return keep;
        }
        keep.extend(required);
    }
}

/// `--prune` refuses a file with a `[target."<id>"]` section naming no known client: a
/// typo there would leave that client's addons out of its desired set.
fn unknown_section(state: &DesiredState, known: &[&str], path: &Path) -> Option<Error> {
    let (id, section) = state
        .targets
        .iter()
        .find(|(id, _)| !known.iter().any(|k| k.eq_ignore_ascii_case(id)))?;
    Some(
        Error::new(
            ErrorCode::DesiredStateInvalid,
            format!(
                "{} line {}: [target.\"{id}\"] names no known client, so --prune is refused (it could remove what that section meant to keep); sync without --prune is allowed",
                path.display(),
                section.line
            ),
            Suggestion::command(["omaddons", "targets"]),
        )
        .details(json!({
            "path": path.display().to_string(),
            "line": section.line,
            "toml_key": format!("target.{id}"),
            "reason": "unknown_target_section",
        })),
    )
}

/// Every ref a `ref_ambiguous` error lists.
fn ambiguous_refs(error: &Error) -> Vec<Ref> {
    if error.code != ErrorCode::RefAmbiguous {
        return Vec::new();
    }
    error.details["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.as_str()?.parse().ok())
        .collect()
}

/// The positions of entries that resolve to a ref another entry resolves to: they may
/// disagree, so neither is synced.
fn resolved_duplicates(wants: &[Want<'_>]) -> BTreeMap<usize, usize> {
    let mut out = BTreeMap::new();
    for (i, a) in wants.iter().enumerate() {
        if let Some((j, _)) = wants
            .iter()
            .enumerate()
            .find(|(j, b)| *j != i && b.r#ref.identity() == a.r#ref.identity())
        {
            out.insert(i, j);
        }
    }
    out
}

/// What an item is called: the entry's name, else its ref.
fn label(entry: &Entry) -> String {
    entry
        .name
        .clone()
        .or_else(|| entry.r#ref.clone())
        .unwrap_or_default()
}

/// Whether `target` has a section in `state`: its family's when it is live, or its own.
fn has_section(state: &DesiredState, target: &Target) -> bool {
    (target.kind == TargetKind::Live && state.families.contains_key(&target.family))
        || state
            .targets
            .keys()
            .any(|id| id.eq_ignore_ascii_case(&target.id))
}

fn target_ref(d: &DiscoveredTarget) -> TargetRef {
    TargetRef {
        id: d.target.id.clone(),
        key: d.target.key,
        mode: None,
    }
}

/// An item that failed before the engine.
fn failed_item(request: String, r: Option<Ref>, error: &Error) -> ItemResult {
    ItemResult {
        request,
        name: None,
        r#ref: r,
        status: ItemStatus::Failed,
        skip_reason: None,
        duplicate_of: None,
        reason: None,
        from_version: None,
        to_version: None,
        folders: None,
        required_by: None,
        deps: ItemDeps::default(),
        error: Some(ItemError::from(error)),
    }
}

/// The file to read: `<file>` (relative to the working directory), else the default.
fn file_path(world: &World, args: &SyncArgs) -> PathBuf {
    match &args.file {
        Some(file) if file.is_relative() => std::env::current_dir()
            .map(|cwd| cwd.join(file))
            .unwrap_or_else(|_| file.clone()),
        Some(file) => file.clone(),
        None => world.paths.addons_toml(),
    }
}

/// The file's text; a missing file is `not_found`, and one that was read but is not
/// UTF-8 text `desired_state_invalid` at its first such line, as a file that does not
/// parse (TOML is UTF-8).
fn read(path: &Path, given: bool) -> Result<String> {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|err| {
            let valid_up_to = err.utf8_error().valid_up_to();
            crate::state::desired::not_utf8(path, err.as_bytes(), valid_up_to)
        }),
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let fix = if given {
                Suggestion::text("pass the file: `omaddons sync <path>`")
            } else {
                Suggestion::text(format!(
                    "pass the file: `omaddons sync <path>`, or write {} (see `omaddons help sync`)",
                    path.display()
                ))
            };
            Err(Error::new(
                ErrorCode::NotFound,
                format!("{} does not exist", path.display()),
                fix,
            )
            // What the file holds, read-only.
            .alternative(Suggestion::command(["omaddons", "help", "sync"]))
            .details(json!({ "path": path.display().to_string() })))
        }
        Err(err) => Err(Error::io(
            path,
            &err,
            Suggestion::text(format!("check that {} can be read", path.display())),
        )),
    }
}

/// `sync -`: the desired state comes from standard input.
fn reads_stdin(args: &SyncArgs) -> bool {
    args.file.as_deref() == Some(Path::new(STDIN))
}

/// The desired state's text from standard input (`sync -`), refused before any read when
/// it is a terminal, and past [`SYNC_STDIN_MAX_BYTES`] (its plan holds the text).
fn read_stdin(is_tty: bool, input: &mut dyn Read) -> Result<String> {
    let fix = || {
        Suggestion::text(
            "pipe the list in (`omaddons sync - < addons.toml`), or pass its file: `omaddons sync <path>`",
        )
    };
    if is_tty {
        return Err(Error::new(
            ErrorCode::Usage,
            "sync - reads the desired state from standard input, not a terminal",
            fix(),
        ));
    }
    let mut bytes = Vec::new();
    input
        .take(SYNC_STDIN_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| {
            Error::new(
                ErrorCode::Usage,
                format!("could not read standard input: {e}"),
                fix(),
            )
        })?;
    if bytes.len() as u64 > SYNC_STDIN_MAX_BYTES {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "standard input holds more than {} KiB of desired state, the most sync - reads",
                SYNC_STDIN_MAX_BYTES / 1024
            ),
            Suggestion::text("write the list to a file and pass its path: `omaddons sync <path>`"),
        )
        .details(json!({ "limit": "SYNC_STDIN_MAX_BYTES", "max_bytes": SYNC_STDIN_MAX_BYTES })));
    }
    String::from_utf8(bytes).map_err(|_| {
        Error::new(
            ErrorCode::Usage,
            "standard input is not UTF-8 text, so it is not desired state",
            fix(),
        )
    })
}

/// The lowercase hex SHA-256 of the desired-state file's text, as read.
fn desired_digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the command decided for one target before the engine.
#[derive(Default)]
struct TargetWork {
    changes: Vec<RequestedChange>,
    requested: Vec<ItemRequest>,
    items: Vec<ItemResult>,
    /// Item requests in file order.
    order: Vec<String>,
    /// `--locked`: the dependency locks (`dependency = true` entries with a version), by
    /// resolved ref.
    locks: BTreeMap<Ref, String>,
    /// `--locked`: entries installed without a version or an archive hash (the newest
    /// release, or what a URL serves now).
    unlocked: Vec<String>,
    /// `--locked`: the requests that install a recorded version, or a recorded archive
    /// (a `url:` entry's `sha256`).
    locked: Vec<String>,
}

/// Resolves, checks and diffs `target`'s entries; with `--prune`, removes the records
/// nothing keeps ([`keep_set`]).
fn target_work(
    state: &DesiredState,
    d: &DiscoveredTarget,
    records: &BTreeMap<String, TargetRecord>,
    catalog: &Catalog,
    world: &World,
    path: &Path,
    args: &SyncArgs,
) -> TargetWork {
    let locked = args.locked;
    // What the file names, or may name, on this target: never pruned.
    let mut named: Vec<Ref> = Vec::new();
    let t = &d.target;
    // A file's relative paths are its directory's; standard input has none, so its are
    // the working directory's, as `install` reads them.
    let base = if reads_stdin(args) {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
    } else {
        path.parent()
            .unwrap_or_else(|| Path::new("/"))
            .to_path_buf()
    };
    let git_hosts = &world.config.git_hosts;
    let wanted = desired::desired_for(state, t, |e| {
        resolve_entry(e, (&t.id, t.family), records, catalog, git_hosts, &base)
    });
    let at = |entry: &Entry| format!("{} line {}", path.display(), entry.line);
    let mut work = TargetWork::default();
    let mut wants: Vec<Want<'_>> = Vec::new();
    for d in wanted {
        // Locks are not requests: `--locked` fixes the version of a dependency it
        // installs; one that resolves to nothing is ignored.
        if d.entry.dependency {
            if let Ok(r) = &d.resolved {
                named.push(r.clone());
            }
            if let (true, Ok(r), Some(version)) = (locked, &d.resolved, &d.entry.version) {
                work.locks.insert(r.clone(), version.clone());
            }
            continue;
        }
        work.order.push(label(d.entry));
        match &d.resolved {
            Ok(r) => named.push(r.clone()),
            Err(error) => {
                named.extend(ambiguous_refs(error));
                // A path that is no longer there still names the addon installed from it:
                // the entry fails, and that addon stays.
                named.extend(local_path_ref(d.entry.request(), &base));
            }
        }
        match d.resolved {
            Ok(r) => wants.push(Want {
                entry: d.entry,
                name: catalog_name(catalog, &r)
                    .or_else(|| d.entry.r#ref.as_ref().and_then(|_| d.entry.name.clone())),
                r#ref: r,
            }),
            Err(error) => {
                let mut error = error;
                error.message = format!("{}: {}", at(d.entry), error.message);
                work.items.push(failed_item(label(d.entry), None, &error));
            }
        }
    }
    let duplicates = resolved_duplicates(&wants);
    for (&i, &j) in &duplicates {
        let (a, b) = (&wants[i], &wants[j]);
        let (first, later) = (
            a.entry.line.min(b.entry.line),
            a.entry.line.max(b.entry.line),
        );
        let error = Error::new(
            ErrorCode::DesiredStateInvalid,
            format!(
                "{}: {} and {} (line {}) are both {}; neither is synced",
                at(a.entry),
                label(a.entry),
                label(b.entry),
                b.entry.line,
                a.r#ref
            ),
            Suggestion::text(format!(
                "keep one of the entries on lines {first} and {later} of {}",
                path.display()
            )),
        )
        .details(json!({
            "path": path.display().to_string(),
            "line": a.entry.line,
            "toml_key": "",
            "reason": "duplicate",
            "other_line": b.entry.line,
        }));
        work.items
            .push(failed_item(label(a.entry), Some(a.r#ref.clone()), &error));
    }
    let wants: Vec<Want<'_>> = wants
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !duplicates.contains_key(i))
        .map(|(_, w)| w)
        .collect();
    let record = records.get(&t.id);
    for (item, want) in diff(&wants, record, locked).into_iter().zip(&wants) {
        let request = label(item.entry);
        if let Some(RequestedChange::Install {
            version,
            blob_sha256,
            ..
        }) = &item.change
            && locked
        {
            // Locked by its version, or by its archive's hash alone (a `url:` entry).
            if version.is_some() || blob_sha256.is_some() {
                work.locked.push(request.clone());
            } else if item.installed.is_none() {
                work.unlocked.push(request.clone());
            }
        }
        match item.change {
            Some(change) => {
                work.changes.push(change);
                work.requested.push(ItemRequest {
                    request,
                    target: t.id.clone(),
                    addon: item
                        .installed
                        .map(|a| a.name.clone())
                        .or_else(|| want.name.clone()),
                    r#ref: Some(
                        item.installed
                            .map_or_else(|| want.r#ref.clone(), |a| a.r#ref.clone()),
                    ),
                    version: item.installed.map(|a| a.version.clone()),
                });
            }
            None => {
                let a = item.installed;
                work.items.push(ItemResult {
                    request,
                    name: a.map(|a| a.name.clone()),
                    r#ref: Some(a.map_or_else(|| want.r#ref.clone(), |a| a.r#ref.clone())),
                    status: ItemStatus::Unchanged,
                    skip_reason: None,
                    duplicate_of: None,
                    reason: None,
                    from_version: a.map(|a| a.version.clone()),
                    to_version: a.map(|a| a.version.clone()),
                    folders: None,
                    required_by: None,
                    deps: ItemDeps::default(),
                    error: None,
                });
            }
        }
    }
    // `--prune`: every record nothing keeps goes, by name, after the entries' changes.
    if let (true, Some(record)) = (args.prune, record) {
        let keep = keep_set(record, &named);
        let mut gone: Vec<&AddonRecord> = record
            .addons
            .iter()
            .filter(|(key, _)| !keep.contains(*key))
            .map(|(_, a)| a)
            .collect();
        gone.sort_by_key(|a| a.name.to_ascii_lowercase());
        for a in gone {
            // A prune, never a plain removal: the engine keeps it when an addon this run
            // installs requires it, which no record says yet.
            work.changes.push(RequestedChange::Prune {
                addon: a.name.clone(),
            });
            work.requested.push(ItemRequest {
                request: a.name.clone(),
                target: t.id.clone(),
                addon: Some(a.name.clone()),
                r#ref: Some(a.r#ref.clone()),
                version: Some(a.version.clone()),
            });
            work.order.push(a.name.clone());
        }
    }
    work
}

/// Runs `sync`.
pub fn run(ctx: &Context, args: &SyncArgs) -> Result<SyncOutcome> {
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.detail();
    let mut warnings = world.warnings.clone();
    let mut notes = Vec::new();
    // `sync -`: standard input's text (a stored plan's, on `apply`), named `<stdin>` where
    // a message would name the file.
    let stdin = reads_stdin(args);
    let (path, text) = if stdin {
        let text = match &args.input {
            Some(text) => text.clone(),
            None => read_stdin(ctx.stdin_is_tty, &mut std::io::stdin().lock())?,
        };
        (PathBuf::from(STDIN_NAME), text)
    } else {
        let path = file_path(&world, args);
        let text = read(&path, args.file.is_some())?;
        (path, text)
    };
    // The exact bytes parsed, which the token covers (the engine never reads the file).
    let extras = PlanExtras {
        desired_sha256: Some(desired_digest(&text)),
        ..PlanExtras::default()
    };
    // A file that cannot be read as desired state changes nothing.
    let (state, parsed) = desired::parse(&text, &path)?;
    warnings.extend(parsed);
    let known = world.resolved.known_ids();
    // Before anything is resolved: a section for no known client may be a typo whose
    // client --prune would empty.
    if args.prune
        && let Some(error) = unknown_section(&state, &known, &path)
    {
        return Err(error);
    }
    warnings.extend(desired::validate(&state, &known));
    let (catalog, catalog_warnings) = world.catalog(ctx);
    warnings.extend(catalog_warnings);
    let mut targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    let records = world.records()?;

    let mut changes: BTreeMap<String, Vec<RequestedChange>> = BTreeMap::new();
    let mut requested: Vec<ItemRequest> = Vec::new();
    let mut pushed: Vec<(TargetRef, ItemResult)> = Vec::new();
    let mut order: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut without: Vec<TargetResult> = Vec::new();
    let mut locks: BTreeMap<String, BTreeMap<Ref, String>> = BTreeMap::new();
    let mut locked: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut matching = 0usize;
    // The clients the file has a section for: what a run with nothing to do is about.
    let mut idle: Vec<Idle<'_>> = Vec::new();
    for d in &targets {
        if !has_section(&state, &d.target) {
            without.push(TargetResult {
                no_section: true,
                ..TargetResult::empty(target_ref(d))
            });
            continue;
        }
        matching += 1;
        idle.push(Idle {
            target: &d.target,
            generation: records.get(&d.target.id).map_or(0, |r| r.generation),
        });
        let work = target_work(&state, d, &records, &catalog, &world, &path, args);
        if !work.unlocked.is_empty() {
            warnings.push(not_locked(&d.target.id, &work.unlocked, &path));
        }
        locks.insert(d.target.id.clone(), work.locks);
        locked.insert(d.target.id.clone(), work.locked);
        if !work.changes.is_empty() {
            changes.insert(d.target.id.clone(), work.changes);
        }
        requested.extend(work.requested);
        pushed.extend(work.items.into_iter().map(|i| (target_ref(d), i)));
        order.insert(d.target.id.clone(), work.order);
    }

    let run = RunId::new();
    // Nothing to change anywhere: no engine, no cache, no request (unless a crash left a
    // journal to recover). A stale bound run changes nothing, not even a recovery.
    let (done, token, plan_precondition): (
        Option<TxOutcomes>,
        Option<String>,
        Option<Precondition>,
    ) = if changes.is_empty() {
        let token = precondition::check_noop(ctx, "sync", &idle, &extras)?;
        recover_waiting(ctx, &world, "sync");
        (None, token, None)
    } else {
        let engine = Engine::new(ctx, &world, "sync")?;
        let freshness = if ctx.offline {
            Freshness::OfflineOnly
        } else {
            Freshness::Window(SOURCE_RECHECK_MIN_INTERVAL)
        }
        // `apply` revalidates every source it touches (M4-D20).
        .forced(ctx.force_revalidate);
        let cache = engine.cache(freshness);
        let req = TxRequest {
            targets: targets
                .iter()
                .filter_map(|d| {
                    Some(TxTarget {
                        discovered: (*d).clone(),
                        changes: changes.remove(&d.target.id)?,
                        default_kind: TxKind::Sync,
                        dependency_locks: locks.remove(&d.target.id).unwrap_or_default(),
                    })
                })
                .collect(),
            force: false,
            while_running: ctx.while_running,
            dry_run: ctx.dry_run,
            reason: ctx.reason.clone(),
            kind: Some(TxKind::Sync),
            run,
            no_deps: args.no_deps,
            // The run the token describes: a dry run's, or a bound run's to compare.
            witness: precondition::witnessing(ctx),
        };
        let binding = PlanBinding::of_run(ctx, "sync", extras.clone());
        let done = engine.run_bound(
            &req,
            Some(&cache),
            binding.as_ref().map(|b| b as &dyn Binding),
        );
        let remembered = cache.remember(done.as_ref().map_or(&[][..], |d| &d.plan.facts));
        warnings.extend(remembered.warnings);
        notes.extend(remembered.notes);
        let done = done?;
        let precondition = ctx
            .dry_run
            .then(|| precondition::plan_precondition("sync", &done.plan, &extras));
        let token = precondition.as_ref().map(Precondition::token);
        (Some(done), token, precondition)
    };

    let mut data = match &done {
        Some(done) => {
            let mut data = TargetsData::build(done, run, &requested);
            settle_failures(&mut data, &ctx.argv);
            unlock_fixes(&mut data, &locked, &ctx.argv);
            for o in &done.outcomes {
                warnings.extend(o.warnings.iter().cloned());
                notes.extend(o.notes.iter().cloned());
            }
            warnings.extend(done.warnings.iter().cloned());
            data
        }
        None => TargetsData::assemble(&[], &[], &[], ctx.dry_run, run, &[]),
    };
    for (target, item) in pushed {
        data.push_item(target, item);
    }
    for target in without {
        data.push_target(target);
    }
    // Every selected target is listed; items in file order, each with its dependencies
    // right after it.
    for d in &targets {
        // ubs:ignore — comparing target ids, not secrets
        if !data.targets.iter().any(|t| t.target.id == d.target.id) {
            data.push_target(TargetResult::empty(target_ref(d)));
        }
    }
    for t in &mut data.targets {
        let Some(order) = order.get(&t.target.id) else {
            continue;
        };
        sort_keeping_dependencies(&mut t.items, |i| {
            order
                .iter()
                .position(|r| *r == i.request)
                .unwrap_or(order.len())
        });
    }
    for t in &data.targets {
        for i in t.items.iter().filter(|i| i.status == ItemStatus::Failed) {
            if let Some(e) = &i.error {
                let mut warning = Warning::item(&e.error());
                let name = i.name.as_deref().unwrap_or(&i.request);
                warning.message = format!("{}: {name}: {}", t.target.id, e.message);
                warnings.push(warning);
            }
        }
    }
    if let Some(error) = data.all_failed() {
        return Err(error);
    }
    let summary = data.summary;
    let mut footer = Vec::new();
    if summary.changed == 0 && summary.failed == 0 {
        let clients = if matching == 1 {
            "client matches"
        } else {
            "clients match"
        };
        footer.push(format!(
            "nothing to do ({matching} {clients} {})",
            path.display()
        ));
    }
    if summary.unchanged > 0 {
        footer.push("sync does not update; run `omaddons update`".to_owned());
    }
    let locks: Vec<(TxId, u64)> = done
        .iter()
        .flat_map(|d| &d.outcomes)
        .filter_map(|o| Some((o.tx?, o.lock_held_ms?)))
        .collect();
    let selected: Vec<&Target> = targets.iter().map(|d| &d.target).collect();
    let preview = match done
        .as_ref()
        .filter(|done| done.plan.has_ops())
        .zip(plan_precondition)
    {
        // Standard input is named as such, its text kept in the plan.
        Some((done, p)) => match if stdin {
            Ok(AbsPath::stdin())
        } else {
            std::env::current_dir()
                .map_err(|err| err.to_string())
                .and_then(|cwd| AbsPath::resolve(&cwd, &path))
        } {
            Ok(file) => intent::preview_plan(
                ctx,
                &StorableArgs::Sync {
                    args,
                    scope: TargetScope::of(ctx.target.as_deref(), &selected),
                    file,
                    default_file: args.file.is_none(),
                    stdin: stdin.then(|| text.clone()),
                },
                p,
                &done.plan,
                &records,
            )?,
            Err(why) => {
                ctx.notes.warn(intent::plan_not_stored(
                    &crate::state::plans::NotStored::Io(why),
                ));
                None
            }
        },
        None => None,
    };
    let flags = ApprovalFlags {
        while_running: ctx.while_running,
        ..ApprovalFlags::default()
    };
    let approval = if ctx.dry_run {
        Some(match &done {
            Some(done) => Approval::of_plan(&done.plan, &records, &flags)?,
            None => Approval::nothing(&flags),
        })
    } else {
        None
    };
    let data = data
        .with_precondition(token)
        .with_approval(approval)
        .listing(verbose);
    let commands = data.undo_commands();
    let mut unique: Vec<Warning> = Vec::new();
    for w in warnings {
        if !unique.contains(&w) {
            unique.push(w);
        }
    }
    Ok(SyncOutcome {
        inner: TargetsOutcome {
            command: "sync",
            data,
            warnings: unique,
            commands,
            notes: footer,
            preview,
        },
        strict: args.strict,
        locks,
        notes,
    })
}

/// `not_locked`: `entries` of `target` have no version, so `--locked` installs their
/// newest release.
fn not_locked(target: &str, entries: &[String], path: &Path) -> Warning {
    Warning::new(
        WarningCode::NotLocked,
        format!(
            "{target}: {} {} no version in {}, so the newest release installs",
            entries.join(", "),
            if entries.len() == 1 { "has" } else { "have" },
            path.display()
        ),
    )
    .with_fix(Suggestion::text(
        "add `version = \"…\"` to those entries, or regenerate the file with `omaddons export`",
    ))
}

/// A locked version the source no longer serves (`not_found`), or no longer serves as the
/// archive the lock records (`hash_mismatch`, `details.reason: "lock_mismatch"`): the fix
/// syncs without the lock, which is this invocation without `--locked` (and `export` then
/// records what it installed).
fn unlock_fixes(data: &mut TargetsData, locked: &BTreeMap<String, Vec<String>>, argv: &[String]) {
    let fix: Vec<String> = crate::cli::fix_argv(argv)
        .into_iter()
        .filter(|a| !matches!(a.as_str(), "--locked"))
        .collect();
    let fix = Suggestion::command(fix);
    for t in &mut data.targets {
        let Some(requests) = locked.get(&t.target.id) else {
            continue;
        };
        for i in &mut t.items {
            if let Some(error) = &mut i.error
                && (error.code == ErrorCode::NotFound
                    || (error.code == ErrorCode::HashMismatch
                        && error
                            .details
                            .as_ref()
                            .is_some_and(|d| d["reason"] == "lock_mismatch")))
                && requests.contains(&i.request)
            {
                error.set_fix(fix.clone());
            }
        }
    }
}

/// A fix that reruns this `sync` with `--force` (which `sync` does not have) says how
/// with `install`. (A dependency that failed is a failed item of its own already.)
fn settle_failures(data: &mut TargetsData, argv: &[String]) {
    // The engine's retry is this run's argv without its presentation flags.
    let argv = crate::cli::fix_argv(argv);
    for t in &mut data.targets {
        let id = t.target.id.clone();
        for i in &mut t.items {
            let Some(error) = &mut i.error else {
                continue;
            };
            let forced = error.fix_argv.as_ref().is_some_and(|fix| {
                fix.split_last()
                    .is_some_and(|(last, rest)| last == "--force" && rest == argv)
            });
            if !forced {
                continue;
            }
            let what = i.r#ref.as_ref().map_or_else(
                || i.name.clone().unwrap_or_else(|| i.request.clone()),
                ToString::to_string,
            );
            error.set_fix(Suggestion::command([
                "omaddons".to_owned(),
                "install".to_owned(),
                what,
                "--force".to_owned(),
                "-t".to_owned(),
                id.clone(),
            ]));
        }
    }
    data.recount();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AddonState, Channel, TargetKey, VersionConfidence};

    #[test]
    fn the_desired_digest_is_the_files_bytes() {
        let text = "schema_version = \"1.0\"\n";
        // `printf 'schema_version = "1.0"\n' | sha256sum`
        assert_eq!(
            desired_digest(text),
            "c78775ad9c87559888ed09f8d82522c0f210fd3faae238de83a8118502569180"
        );
        assert_eq!(desired_digest(text), desired_digest(text), "the same text");
        assert_ne!(
            desired_digest(&format!("{text}# note\n")),
            desired_digest(text),
            "a comment changes it"
        );
    }

    /// A file that was read but is not UTF-8 text (a Latin-1 comment, say) is not TOML:
    /// `desired_state_invalid` naming the line of its first such byte, as a file that
    /// does not parse, never `io` ("check that it can be read"). A missing file stays
    /// `not_found`, and a readable one is its text.
    #[test]
    fn a_file_that_is_not_utf8_is_invalid_desired_state_at_its_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("addons.toml");
        std::fs::write(
            &path,
            b"schema_version = \"1.0\"\n[family.mainline]\n# caf\xe9\naddons = []\n",
        )
        .unwrap();
        let refused = read(&path, true).unwrap_err();
        assert_eq!(refused.code, ErrorCode::DesiredStateInvalid, "{refused:?}");
        assert_eq!(
            (
                &refused.details["line"],
                &refused.details["reason"],
                &refused.details["path"],
            ),
            (
                &json!(3),
                &json!("bad_value"),
                &json!(path.display().to_string())
            )
        );
        assert!(
            refused.message.contains("line 3") && refused.message.contains("UTF-8"),
            "{}",
            refused.message
        );

        let text = "schema_version = \"1.0\"\n# caf\u{e9}\n";
        std::fs::write(&path, text).unwrap();
        assert_eq!(read(&path, true).unwrap(), text);
        let missing = read(&dir.path().join("none.toml"), true).unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
    }

    // 1n2p.8: `sync -` reads standard input whole up to its cap, never a terminal.
    #[test]
    fn stdin_is_read_whole_up_to_its_cap_and_never_from_a_terminal() {
        let text = "[family.mainline]\naddons = [\"gh:o/foo\"]\n";
        assert_eq!(read_stdin(false, &mut text.as_bytes()).unwrap(), text);
        let tty = read_stdin(true, &mut text.as_bytes()).unwrap_err();
        assert_eq!(tty.code, ErrorCode::Usage);
        assert!(tty.message.contains("not a terminal"), "{}", tty.message);
        let cap = SYNC_STDIN_MAX_BYTES as usize;
        let at = "#".repeat(cap);
        assert_eq!(
            read_stdin(false, &mut at.as_bytes()).unwrap().len(),
            cap,
            "at the cap: read"
        );
        let over = read_stdin(false, &mut format!("{at}#").as_bytes()).unwrap_err();
        assert_eq!(over.code, ErrorCode::Usage);
        assert!(over.message.contains("64 KiB"), "{}", over.message);
        let binary = read_stdin(false, &mut &[0xff_u8, 0xfe][..]).unwrap_err();
        assert_eq!(binary.code, ErrorCode::Usage);
        assert!(binary.message.contains("UTF-8"), "{}", binary.message);
    }

    #[test]
    fn only_a_lone_dash_reads_stdin() {
        let args = |file: Option<&str>| SyncArgs {
            file: file.map(PathBuf::from),
            ..SyncArgs::default()
        };
        assert!(reads_stdin(&args(Some("-"))));
        for other in [None, Some("addons.toml"), Some("./-"), Some("--")] {
            assert!(!reads_stdin(&args(other)), "{other:?}");
        }
    }

    fn record(addons: &[(&str, &str, &str, Channel, bool)]) -> TargetRecord {
        let mut record = TargetRecord::empty(TargetKey::new());
        for (name, r, version, channel, pinned) in addons {
            let a = AddonRecord {
                r#ref: r.parse().unwrap(),
                name: (*name).to_owned(),
                version: (*version).to_owned(),
                version_confidence: VersionConfidence::Release,
                release_id: None,
                channel: *channel,
                pinned: *pinned,
                reason: AddonReason::Explicit,
                required_by: Vec::new(),
                folders: Vec::new(),
                state: AddonState::Ok,
                installed_at: jiff::Timestamp::now(),
                tx_id: TxId::new(),
                blob_sha256: None,
                revision: None,
                left_out: Vec::new(),
                any_flavor: false,
                skipped_releases: Vec::new(),
            };
            record.addons.insert(TargetRecord::addon_key(name), a);
        }
        record
    }

    fn entry(name: &str, channel: Channel, version: Option<&str>, pinned: bool) -> Entry {
        Entry {
            channel,
            version: version.map(str::to_owned),
            pinned,
            ..Entry::named(name)
        }
    }

    fn want<'s>(e: &'s Entry, r: &str) -> Want<'s> {
        Want {
            entry: e,
            r#ref: r.parse().unwrap(),
            name: Some(e.request().to_owned()),
        }
    }

    /// What a change is: `(kind, version, pin or edit, channel)`.
    type Shape<'a> = (&'a str, Option<&'a str>, &'a str, &'a str);

    /// `(kind, version, pin, channel)` of an install, or `("edit", …)`, or `("none", …)`.
    fn shape(change: Option<&RequestedChange>) -> (&'static str, Option<String>, String, String) {
        match change {
            Some(RequestedChange::Install {
                version,
                pin,
                channel,
                channel_explicit,
                reason,
                ..
            }) => {
                assert!(*channel_explicit);
                assert_eq!(*reason, AddonReason::Explicit);
                (
                    "install",
                    version.clone(),
                    format!("{pin:?}"),
                    format!("{channel:?}"),
                )
            }
            Some(RequestedChange::Edit { edit, .. }) => {
                ("edit", None, format!("{edit:?}"), String::new())
            }
            Some(_) => ("other", None, String::new(), String::new()),
            None => ("none", None, String::new(), String::new()),
        }
    }

    #[test]
    fn prune_keeps_what_the_file_names_and_everything_that_requires() {
        use Channel::Stable;
        let mut rec = record(&[
            ("Little", "gh:o/little", "1", Stable, false),
            ("Big", "gh:o/big", "1", Stable, false),
            ("Lib", "gh:o/lib", "1", Stable, false),
            ("Orphan", "gh:o/orphan", "1", Stable, false),
            ("Other", "gh:o/other", "1", Stable, false),
            ("Locked", "gh:o/locked", "1", Stable, false),
            ("Dup", "gh:o/dup", "1", Stable, false),
            ("Amb", "gh:o/amb", "1", Stable, false),
        ]);
        let requires = |rec: &mut TargetRecord, addon: &str, by: &str| {
            rec.addons
                .get_mut(&TargetRecord::addon_key(addon))
                .unwrap()
                .required_by = vec![by.to_owned()];
        };
        // A chain Little → Big → Lib; a stale requester; one required only by a pruned one.
        requires(&mut rec, "Big", "little");
        requires(&mut rec, "Lib", "Big");
        requires(&mut rec, "Orphan", "Gone");
        requires(&mut rec, "Other", "Orphan");
        let refs: Vec<Ref> = ["gh:O/Little", "gh:o/locked", "gh:o/dup", "gh:o/amb"]
            .iter()
            .map(|r| r.parse().unwrap())
            .collect();
        let keep = keep_set(&rec, &refs);
        assert_eq!(
            keep.into_iter().collect::<Vec<_>>(),
            ["amb", "big", "dup", "lib", "little", "locked"]
        );
    }

    #[test]
    fn locked_installs_the_recorded_versions_without_pinning() {
        use Channel::{Beta, Stable};
        let installed = record(&[
            ("Foo", "gh:o/foo", "v1", Stable, false),
            ("Pinned", "gh:o/pinned", "v1", Stable, true),
        ]);
        let with_sha = |mut e: Entry| {
            e.sha256 = Some("ab".repeat(32));
            e
        };
        let cases: &[(Entry, &str, Shape<'_>, Shape<'_>)] = &[
            // Missing: its version whenever it has one, unpinned unless the entry pins.
            (
                entry("New", Stable, Some("v3"), false),
                "gh:o/new",
                ("install", Some("v3"), "Keep", "Stable"),
                ("install", None, "Keep", "Stable"),
            ),
            (
                entry("New", Stable, None, false),
                "gh:o/new",
                ("install", None, "Keep", "Stable"),
                ("install", None, "Keep", "Stable"),
            ),
            // Installed at another version: moved there, still unpinned.
            (
                entry("Foo", Stable, Some("v9"), false),
                "gh:o/foo",
                ("install", Some("v9"), "Keep", "Stable"),
                ("none", None, "", ""),
            ),
            // A pinned record whose entry is unpinned at another version: moved there and
            // unpinned (plain sync only unpins).
            (
                entry("Pinned", Stable, Some("v2"), false),
                "gh:o/pinned",
                ("install", Some("v2"), "Unpin", "Stable"),
                ("edit", None, "SetPinned(false)", ""),
            ),
            // Another channel: at its version.
            (
                entry("Foo", Beta, Some("v1"), false),
                "gh:o/foo",
                ("install", Some("v1"), "Keep", "Beta"),
                ("install", None, "Keep", "Beta"),
            ),
            // At its version already: nothing either way.
            (
                entry("Foo", Stable, Some("v1"), false),
                "gh:o/foo",
                ("none", None, "", ""),
                ("none", None, "", ""),
            ),
        ];
        for (e, r, locked, plain) in cases {
            for (on, expected) in [(true, locked), (false, plain)] {
                let items = diff(&[want(e, r)], Some(&installed), on);
                let got = shape(items[0].change.as_ref());
                assert_eq!(
                    (got.0, got.1.as_deref(), got.2.as_str(), got.3.as_str()),
                    *expected,
                    "locked {on}: {e:?} {r}"
                );
            }
        }
        // Only a locked install carries the recorded archive: with its version, or alone
        // when the entry has no version to record (a `url:` release is its URL).
        let sha = |e: Entry, r: &str, on: bool| {
            let items = diff(&[want(&e, r)], None, on);
            match &items[0].change {
                Some(RequestedChange::Install { blob_sha256, .. }) => blob_sha256.clone(),
                _ => None,
            }
        };
        let url = "url:https://example.com/new.zip";
        let sha_cases = [
            ("versioned, locked", Some("v3"), "gh:o/new", true, true),
            ("versioned, plain", Some("v3"), "gh:o/new", false, false),
            ("versionless url, locked", None, url, true, true),
            ("versionless url, plain", None, url, false, false),
            ("versionless gh, locked", None, "gh:o/new", true, true),
        ];
        for (case, version, r, on, carries) in sha_cases {
            let e = with_sha(entry("New", Stable, version, false));
            assert_eq!(sha(e, r, on).is_some(), carries, "case {case}");
        }
        assert!(
            sha(entry("New", Stable, None, true), url, true).is_none(),
            "no sha256, nothing to lock"
        );
    }

    #[test]
    fn the_diff_table() {
        use Channel::{Beta, Stable};
        let installed = record(&[
            ("Foo", "gh:o/foo", "v1", Stable, false),
            ("Pinned", "gh:o/pinned", "v1", Stable, true),
            ("Beta", "gh:o/beta", "v1", Beta, false),
        ]);
        let cases: &[(Entry, &str, Shape<'_>)] = &[
            // Missing: installed, its version only when pinned.
            (
                entry("New", Stable, Some("v3"), false),
                "gh:o/new",
                ("install", None, "Keep", "Stable"),
            ),
            (
                entry("New", Beta, Some("v3"), true),
                "gh:o/new",
                ("install", Some("v3"), "Pin", "Beta"),
            ),
            // Installed, current entry: nothing, whatever its unpinned version says.
            (
                entry("Foo", Stable, None, false),
                "gh:o/foo",
                ("none", None, "", ""),
            ),
            (
                entry("Foo", Stable, Some("v9"), false),
                "gh:o/foo",
                ("none", None, "", ""),
            ),
            // Pinned at another version: that version, pinned.
            (
                entry("Foo", Stable, Some("v2"), true),
                "gh:o/foo",
                ("install", Some("v2"), "Pin", "Stable"),
            ),
            // Pinned at its version, or pinned without one: a records-only pin.
            (
                entry("Foo", Stable, Some("v1"), true),
                "gh:o/foo",
                ("edit", None, "SetPinned(true)", ""),
            ),
            (
                entry("Foo", Stable, None, true),
                "gh:o/foo",
                ("edit", None, "SetPinned(true)", ""),
            ),
            // Unpinned entry against a pinned addon: unpin, no install.
            (
                entry("Pinned", Stable, None, false),
                "gh:o/pinned",
                ("edit", None, "SetPinned(false)", ""),
            ),
            // Another channel: switched, the pin carried as an intent.
            (
                entry("Foo", Beta, None, false),
                "gh:o/foo",
                ("install", None, "Keep", "Beta"),
            ),
            (
                entry("Beta", Stable, None, true),
                "gh:o/beta",
                ("install", None, "Pin", "Stable"),
            ),
            (
                entry("Pinned", Beta, None, false),
                "gh:o/pinned",
                ("install", None, "Unpin", "Beta"),
            ),
            // Pinned here and in the file, no version: a switch that keeps its release
            // and its pin (omad-4h0d.17), never the engine's pinned no-op (`Keep`).
            (
                entry("Pinned", Beta, None, true),
                "gh:o/pinned",
                ("install", None, "Pin", "Beta"),
            ),
            // Pinned at its own version on another channel: that version, pinned.
            (
                entry("Pinned", Beta, Some("v1"), true),
                "gh:o/pinned",
                ("install", Some("v1"), "Keep", "Beta"),
            ),
            // A pinned version on another channel: the version row wins.
            (
                entry("Beta", Stable, Some("v2"), true),
                "gh:o/beta",
                ("install", Some("v2"), "Pin", "Stable"),
            ),
            // A ref in another case is the same addon.
            (
                entry("Foo", Stable, None, false),
                "gh:O/Foo",
                ("none", None, "", ""),
            ),
        ];
        for (e, r, (kind, version, pin, channel)) in cases {
            let items = diff(&[want(e, r)], Some(&installed), false);
            let got = shape(items[0].change.as_ref());
            assert_eq!(
                (got.0, got.1.as_deref(), got.2.as_str(), got.3.as_str()),
                (*kind, *version, *pin, *channel),
                "{e:?} {r}"
            );
        }
    }

    #[test]
    fn an_existing_addon_is_named_and_reffed_as_recorded() {
        let installed = record(&[("Foo", "gh:o/foo", "v1", Channel::Stable, false)]);
        let e = entry("whatever", Channel::Beta, None, false);
        let items = diff(&[want(&e, "gh:O/FOO")], Some(&installed), false);
        assert!(
            matches!(
                items[0].change.as_ref(),
                Some(RequestedChange::Install { r#ref, name, .. })
                    if r#ref.to_string() == "gh:o/foo" && name.as_deref() == Some("Foo")
            ),
            "{:?}",
            items[0].change
        );
        // Nothing installed, no record at all: installs, under the given name.
        let items = diff(&[want(&e, "gh:o/new")], None, false);
        assert!(items[0].installed.is_none());
        assert!(matches!(
            items[0].change,
            Some(RequestedChange::Install { ref name, .. }) if name.as_deref() == Some("whatever")
        ));
    }

    #[test]
    fn entries_resolving_to_one_ref_are_all_duplicates() {
        let (a, b, c) = (
            Entry::named("BigWigs"),
            Entry::named("gh:BigWigsMods/BigWigs"),
            Entry::named("DBM"),
        );
        let wants = [
            want(&a, "gh:BigWigsMods/BigWigs"),
            want(&b, "gh:bigwigsmods/bigwigs"),
            want(&c, "gh:o/dbm"),
        ];
        let dups = resolved_duplicates(&wants);
        assert_eq!(dups, BTreeMap::from([(0, 1), (1, 0)]));
    }
}
