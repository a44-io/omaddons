//! Stored plans (M4-D21, M4-D23, M4-D24): what a storable `--dry-run` showed, kept in
//! `<cache>/plans/<plan_id>.json` so that `omaddons apply` can run exactly that within
//! `PLAN_TTL`. A plan is cache: derived, reproduced by previewing again, and immutable
//! once written. The reader fails closed (a plan that does not decode or disagrees with
//! itself is a missing one), the listing and the sweep are bounded, and admission, under
//! the store's own lease and never the global lock, reuses an identical plan, deletes
//! only expired ones, and declines what the hard quotas would not hold rather than evict
//! a live plan.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app::approval::Approval;
use crate::app::intent::StoredIntent;
use crate::app::precondition::{Precondition, token_of_parts};
use crate::engine::plan::{Plan, PlannedRelease};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{
    Content, Expect, Lease, LeaseError, LockUse, MutateOpts, Op, PruneReason, ScopeTable,
    WriteSession, Writer, ensure_dir_all, failpoint, mutate_with, prune,
};
use crate::limits::{
    LOCK_ACQUIRE_TIMEOUT, PLAN_MAX_BYTES, PLAN_TTL, PLANS_MAX_BYTES, PLANS_MAX_FILES,
    PLANS_SWEEP_MAX, REASON_MAX_CHARS,
};
use crate::model::{Actor, ExpectedHash, PlanId};
use crate::sources::SourceId;
use crate::state::paths::Paths;
use crate::state::store::{self, SchemaVersion, VersionStatus, Versioned};

/// One stored plan (kind `plan` 1.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoredPlan {
    pub plan_id: PlanId,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub created_at: Timestamp,
    /// `created_at` and `PLAN_TTL`.
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub expires_at: Timestamp,
    pub tool_version: String,
    pub build_id: String,
    pub intent: StoredIntent,
    /// The intent's own `display_argv`, for people and history.
    pub display_argv: Vec<String>,
    /// Who previewed it.
    pub actor: Actor,
    pub reason: Option<String>,
    pub precondition: StoredPrecondition,
    pub releases: Vec<StoredRelease>,
    pub approval: Approval,
    pub summary: Vec<String>,
}

impl Versioned for StoredPlan {
    const KIND: &'static str = "plan";
    /// 1.1: a `sync` intent may hold `stdin`, the text `sync -` read, which its `apply`
    /// syncs instead of reading standard input (1n2p.8). A 1.0 plan holds none: the
    /// default migration reads it as is. 1.2: the `skip` and `unskip` intents (RG-33),
    /// which an older plan never holds.
    const CURRENT: SchemaVersion = SchemaVersion::new(1, 2);
}

impl StoredPlan {
    /// Whether `display_argv` is the intent's own.
    pub fn display_argv_agrees(&self) -> bool {
        self.display_argv == self.intent.display_argv()
    }

    /// Whether the plan is past its expiry at `now`: `apply`'s `expired` refusal and
    /// `plans show`'s `expired` state, so the two agree at the exact instant.
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now > self.expires_at
    }
}

/// Whether an input a plan installs from is gone: `apply`'s `input_missing` and `plans
/// show`'s `inputs[].present` ask the same question.
pub fn input_is_gone(path: &Path) -> bool {
    std::fs::metadata(path).is_err_and(|err| {
        matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        )
    })
}

/// The token a preview returned, and the parts it is the digest of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoredPrecondition {
    pub token: String,
    pub parts: StoredParts,
}

/// A token's four parts, in canonical form: checked by recomputing the token on load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoredParts {
    pub command: Value,
    pub targets: Value,
    pub selection: Value,
    pub observed: Value,
}

impl StoredParts {
    pub fn of(precondition: &Precondition) -> Self {
        let [command, targets, selection, observed] = precondition.part_values();
        Self {
            command,
            targets,
            selection,
            observed,
        }
    }

    pub fn to_array(&self) -> [Value; 4] {
        [
            self.command.clone(),
            self.targets.clone(),
            self.selection.clone(),
            self.observed.clone(),
        ]
    }
}

/// A release the plan installs from, as stored: a direct release by its URL, a deferred
/// one (Tukui's, whose URL is resolved only for each download) by its source and key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoredRelease {
    pub release_id: String,
    pub source: Option<SourceId>,
    pub key: Option<String>,
    pub url: Option<String>,
    pub expected_hash: Option<ExpectedHash>,
    pub blob_sha256: Option<String>,
}

impl StoredRelease {
    pub fn from_planned(release: &PlannedRelease) -> Self {
        let (source, key, url) = match &release.deferred {
            Some(deferred) => (Some(deferred.source), Some(deferred.key.clone()), None),
            None => (None, None, release.url.clone()),
        };
        Self {
            release_id: release.release_id.clone(),
            source,
            key,
            url,
            expected_hash: release.expected_hash.clone(),
            blob_sha256: release.blob_sha256.clone(),
        }
    }

    /// Every target's releases in `plan`, sorted by release id, each once.
    pub fn of_plan(plan: &Plan) -> Vec<Self> {
        let mut releases: Vec<Self> = plan
            .targets
            .iter()
            .flat_map(|tp| tp.releases.iter().map(Self::from_planned))
            .collect();
        releases.sort_by(|a, b| a.release_id.cmp(&b.release_id));
        releases.dedup_by(|a, b| a.release_id == b.release_id);
        releases
    }
}

/// A plan file's bytes: compact JSON with `schema_version` first and a trailing newline,
/// so `PLAN_MAX_BYTES` is spent on content.
pub fn encode(plan: &StoredPlan) -> Result<Vec<u8>> {
    #[derive(Serialize)]
    struct Document<'a> {
        schema_version: SchemaVersion,
        #[serde(flatten)]
        value: &'a StoredPlan,
    }
    let mut json = serde_json::to_vec(&Document {
        schema_version: StoredPlan::CURRENT,
        value: plan,
    })
    .map_err(|err| Error::internal(format!("serializing a plan: {err}")))?;
    json.push(b'\n');
    Ok(json)
}

// ---------------------------------------------------------------------------------------
// Reading.

/// A stored plan, only if it decodes and agrees with itself (M4-D23). Every failure is
/// `plan_not_found` with `details: {plan_id, path, reason: "unreadable"}`: a plan is
/// cache, so an unreadable plan is a missing one, and the remedy is previewing again.
/// Expiry against now and the build are not checked here: `apply` refuses those itself,
/// and a listing shows an expired plan.
pub fn load(path: &Path) -> Result<StoredPlan> {
    let id = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let id = id.strip_suffix(".json").map_or(id.clone(), str::to_owned);
    let unreadable = |why: &str| {
        Error::new(
            ErrorCode::PlanNotFound,
            format!("plan {id} cannot be used: {why}"),
            Suggestion::text("preview the change again: run its command with --dry-run"),
        )
        .details(json!({
            "plan_id": id,
            "path": path.display().to_string(),
            "reason": "unreadable",
        }))
    };
    let bytes = read_plan(path).map_err(|why| unreadable(&why))?;
    let loaded = store::load_bytes::<StoredPlan>(path, &bytes)
        .map_err(|err| unreadable(&format!("it does not decode: {}", err.message)))?;
    if loaded.status == VersionStatus::Newer {
        return Err(unreadable(&format!(
            "a newer omaddons wrote it (version {})",
            loaded.version
        )));
    }
    let plan = loaded.value;
    if plan.plan_id.to_string() != id {
        return Err(unreadable(&format!("it holds plan {}", plan.plan_id)));
    }
    let ttl = SignedDuration::try_from(PLAN_TTL).unwrap_or(SignedDuration::MAX);
    let latest = plan.created_at.checked_add(ttl).ok();
    if plan.expires_at <= plan.created_at || latest.is_none_or(|l| plan.expires_at > l) {
        return Err(unreadable(
            "its expiry is not within a day after its creation",
        ));
    }
    plan.intent
        .validate()
        .map_err(|rule| unreadable(&format!("its intent breaks a rule: {rule}")))?;
    if !plan.display_argv_agrees() {
        return Err(unreadable("its display_argv is not its intent's"));
    }
    // ubs:ignore — a public approval digest, not a secret or authentication token
    if plan.precondition.token != token_of_parts(&plan.precondition.parts.to_array())
        || plan.precondition.parts.command != Value::String(plan.intent.command_path().into())
    {
        return Err(unreadable("its token is not the digest of its parts"));
    }
    if plan
        .reason
        .as_deref()
        .is_some_and(|r| r.chars().count() > REASON_MAX_CHARS)
    {
        return Err(unreadable(&format!(
            "its reason is longer than {REASON_MAX_CHARS} characters"
        )));
    }
    Ok(plan)
}

/// The plan `id`, `None` when there is none.
pub fn load_id(paths: &Paths, id: &PlanId) -> Result<Option<StoredPlan>> {
    let path = paths.plan(id);
    match std::fs::symlink_metadata(&path) {
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => load(&path).map(Some),
    }
}

/// What [`find`] found for an id or a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    One {
        id: PlanId,
        path: PathBuf,
    },
    /// No plan; `cut` when the listing stopped at its bound, so a match may be unread.
    None {
        cut: bool,
    },
    /// Several, sorted.
    Many(Vec<PlanId>),
}

/// Where a plan's evidence scan stops (M4-D63): no history line older than this can be
/// one of its commits. The earliest of the plan's `created_at` (when its file read), the
/// creation time its id (or id prefix) encodes, and, only when the file did not read,
/// `now - PLAN_TTL`. The id's time can only move the horizon earlier, so the scan reads
/// at least what the plan's own rule reads; it also keeps the stop right with a plan
/// older than a day whose file is gone, and in tests whose clock is frozen.
pub fn evidence_horizon(created_at: Option<Timestamp>, given: &str, now: Timestamp) -> Timestamp {
    let a_day_ago = SignedDuration::try_from(PLAN_TTL)
        .ok()
        .and_then(|ttl| now.checked_sub(ttl).ok())
        .unwrap_or(Timestamp::MIN);
    [
        created_at,
        encoded_time(given),
        created_at.is_none().then_some(a_day_ago),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(a_day_ago)
}

/// The creation time a ULID's first 10 characters encode, for a full id or a prefix (the
/// rest read as `0`, the earliest it could be); `None` for text that is not Crockford
/// base32.
pub(crate) fn encoded_time(given: &str) -> Option<Timestamp> {
    let head: String = given.to_ascii_uppercase().chars().take(10).collect();
    let ulid = ulid::Ulid::from_string(&format!("{head:0<26}")).ok()?;
    Timestamp::from_millisecond(i64::try_from(ulid.timestamp_ms()).ok()?).ok()
}

/// The stored plan `given` names (`apply <plan_id>`): a full id, read directly with no
/// listing, or a prefix of 8 to 25 characters, matched against the bounded listing's
/// names (no plan is read). Case is ignored. A symlink is never a plan.
pub fn find(paths: &Paths, given: &str) -> Result<Found> {
    let wanted = given.to_ascii_uppercase();
    if let Ok(id) = wanted.parse::<PlanId>()
        && id.to_string() == wanted
    {
        let path = paths.plan(&id);
        return Ok(match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => Found::One { id, path },
            _ => Found::None { cut: false },
        });
    }
    let listing = list(paths)?;
    let matching: Vec<&ListedPlan> = listing
        .plans
        .iter()
        .filter(|p| p.id.to_string().starts_with(&wanted))
        .collect();
    Ok(match matching.as_slice() {
        [] => Found::None {
            cut: !listing.complete,
        },
        [one] => Found::One {
            id: one.id,
            path: one.path.clone(),
        },
        many => Found::Many(many.iter().map(|p| p.id).collect()),
    })
}

/// A plan file's bytes, at most `PLAN_MAX_BYTES`, never through a symlink.
fn read_plan(path: &Path) -> std::result::Result<Vec<u8>, String> {
    use rustix::fs::{Mode, OFlags};

    let too_large = || "it is larger than 256 KiB".to_owned();
    let meta =
        std::fs::symlink_metadata(path).map_err(|err| format!("it cannot be read: {err}"))?;
    if !meta.is_file() {
        return Err("it is not a regular file".to_owned());
    }
    if meta.len() > PLAN_MAX_BYTES {
        return Err(too_large());
    }
    // Refuse a symlink swapped in after lstat, and a FIFO swapped in before open.
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|err| format!("it cannot be read: {err}"))?;
    let file = std::fs::File::from(fd);
    let opened = file
        .metadata()
        .map_err(|err| format!("it cannot be read: {err}"))?;
    if !opened.is_file() {
        return Err("it is not a regular file".to_owned());
    }
    if opened.len() > PLAN_MAX_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    // A file growing while it is read stops one byte past the cap.
    file.take(PLAN_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("it cannot be read: {err}"))?;
    if bytes.len() as u64 > PLAN_MAX_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

/// A plan's `expires_at` alone, from a bounded read; `None` when it does not read.
fn expiry(path: &Path) -> Option<Timestamp> {
    #[derive(Deserialize)]
    struct Expiry {
        expires_at: Timestamp,
    }
    let bytes = read_plan(path).ok()?;
    serde_json::from_slice::<Expiry>(&bytes)
        .ok()
        .map(|e| e.expires_at)
}

// ---------------------------------------------------------------------------------------
// Listing.

/// What the store holds, from one bounded directory listing that opens no file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Sorted by id, which is creation order.
    pub plans: Vec<ListedPlan>,
    /// Sorted.
    pub leases: Vec<PlanId>,
    /// `.lock`, temporary files, symlinks and foreign names.
    pub others: u64,
    /// Every entry was read: no bound was reached.
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedPlan {
    pub id: PlanId,
    pub path: PathBuf,
    pub bytes: u64,
}

/// The plans and leases in `<cache>/plans/`. At most `PLANS_MAX_FILES +
/// PLANS_SWEEP_MAX` plans are recorded, and at most twice that and one entries read in
/// all (a lease may stand beside every applied plan); either bound leaves the listing
/// incomplete.
pub fn list(paths: &Paths) -> Result<Listing> {
    let dir = paths.plans_dir();
    let io =
        |err: &std::io::Error| Error::io(&dir, err, Suggestion::command(["omaddons", "status"]));
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Listing {
                complete: true,
                ..Listing::default()
            });
        }
        Err(err) => return Err(io(&err)),
    };
    let plan_cap = PLANS_MAX_FILES + PLANS_SWEEP_MAX;
    let entry_cap = 2 * plan_cap + 1;
    let mut listing = Listing {
        complete: true,
        ..Listing::default()
    };
    for (read, entry) in entries.take(entry_cap).enumerate() {
        let entry = entry.map_err(|err| io(&err))?;
        let name = entry.file_name();
        let name = name.to_str().unwrap_or_default();
        let id_of = |suffix: &str| name.strip_suffix(suffix)?.parse::<PlanId>().ok();
        // The entry itself: a symlink is never a plan, and is never followed.
        let meta = entry.metadata().map_err(|err| io(&err))?;
        if let Some(id) = id_of(".json").filter(|_| meta.is_file()) {
            listing.plans.push(ListedPlan {
                id,
                path: entry.path(),
                bytes: meta.len(),
            });
        } else if let Some(id) = id_of(".lease").filter(|_| meta.is_file()) {
            listing.leases.push(id);
        } else {
            listing.others += 1;
        }
        // Stop at the bounds, without fetching an extra directory entry to test them.
        if listing.plans.len() == plan_cap || read + 1 == entry_cap {
            listing.complete = false;
            break;
        }
    }
    listing.plans.sort_by_key(|p| p.id);
    listing.leases.sort_unstable();
    Ok(listing)
}

// ---------------------------------------------------------------------------------------
// The sweep.

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub removed: usize,
    pub leases_removed: usize,
    /// Plans whose expiry did not read: never deleted, still counted in the quotas.
    pub skipped_unreadable: usize,
    /// Prune calls that refused or failed (a lease taken meanwhile).
    pub refused: usize,
    /// Plan files read.
    pub read: usize,
}

/// A sweep for retention after a commit (D6): under the store's lease when it is free,
/// else nothing.
pub fn sweep(paths: &Paths, now: Timestamp) -> Result<SweepReport> {
    let scopes = ScopeTable::new(paths, &[], None)?;
    let lease = Lease::try_acquire(&scopes, &paths.plans_lock(), "plan sweep")
        .map_err(|err| Error::internal(format!("the plan store's lock: {err}")))?;
    if lease.is_none() {
        return Ok(SweepReport::default());
    }
    let listing = list(paths)?;
    Ok(sweep_locked(&scopes, &listing, now).0)
}

/// Deletes the oldest expired plans in `listing` (at most `PLANS_SWEEP_MAX`) with their
/// leases, then leases whose plan is gone, under the store's lease; returns the report
/// and the plans deleted. Ids are ULIDs minted at creation, so id order is expiry order:
/// it stops at the first live plan. It never fails: refusals are counted.
fn sweep_locked(
    scopes: &ScopeTable,
    listing: &Listing,
    now: Timestamp,
) -> (SweepReport, BTreeSet<PlanId>) {
    let mut report = SweepReport::default();
    let mut victims: Vec<&ListedPlan> = Vec::new();
    for plan in &listing.plans {
        if victims.len() >= PLANS_SWEEP_MAX {
            break;
        }
        report.read += 1;
        match expiry(&plan.path) {
            None => report.skipped_unreadable += 1,
            Some(at) if at <= now => victims.push(plan),
            Some(_) => break,
        }
    }
    let paths = scopes.paths();
    let mut removed = BTreeSet::new();
    let counted = |pruned: &[PathBuf], extension: &str| {
        pruned
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == extension))
            .count()
    };
    for plan in victims {
        // One call per plan, so a lease taken meanwhile refuses only its own pair.
        let mut batch = vec![plan.path.clone()];
        if listing.leases.binary_search(&plan.id).is_ok() {
            batch.push(paths.plan_lease(&plan.id));
        }
        match prune(
            scopes,
            PruneReason::PlanExpiry,
            &batch,
            LockUse::Expired { now },
        ) {
            Ok(pruned) => {
                report.removed += counted(&pruned.removed, "json");
                report.leases_removed += counted(&pruned.removed, "lease");
                removed.insert(plan.id);
            }
            Err(_) => report.refused += 1,
        }
    }
    let planned: BTreeSet<PlanId> = listing.plans.iter().map(|p| p.id).collect();
    let orphans = listing
        .leases
        .iter()
        .filter(|id| !planned.contains(id))
        .take(PLANS_SWEEP_MAX);
    for id in orphans {
        let batch = [paths.plan_lease(id)];
        match prune(
            scopes,
            PruneReason::PlanExpiry,
            &batch,
            LockUse::Expired { now },
        ) {
            Ok(pruned) => report.leases_removed += pruned.removed.len(),
            Err(_) => report.refused += 1,
        }
    }
    (report, removed)
}

// ---------------------------------------------------------------------------------------
// Blob pins.

/// What bounded pin discovery found. A pin only saves a download, so an unreadable
/// store or plan never fails its caller (M4-D26).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanPins {
    pub blobs: BTreeSet<String>,
    /// Files opened and their listed sizes.
    pub read: usize,
    pub bytes: u64,
    pub live: usize,
    pub expired: usize,
    /// Readable plans dated beyond the next `PLAN_TTL`.
    pub future: usize,
    /// Refused by `load`, or too large to open.
    pub unreadable: usize,
    /// False when listing failed or a listing/read bound stopped discovery.
    pub complete: bool,
}

/// Every well-formed blob digest named by a usable plan expiring in the next day.
/// Read-only, lock-free and bounded by the store's own quotas.
pub fn unexpired_blob_pins(paths: &Paths, now: Timestamp) -> PlanPins {
    pins_within(paths, now, PLANS_MAX_FILES, PLANS_MAX_BYTES)
}

fn pins_within(paths: &Paths, now: Timestamp, max_files: usize, max_bytes: u64) -> PlanPins {
    let Ok(listing) = list(paths) else {
        return PlanPins::default();
    };
    let mut found = PlanPins {
        complete: listing.complete,
        ..PlanPins::default()
    };
    let ttl = SignedDuration::try_from(PLAN_TTL).unwrap_or(SignedDuration::MAX);
    let latest = now.checked_add(ttl).unwrap_or(Timestamp::MAX);
    for entry in listing.plans.iter().rev() {
        if found.read == max_files {
            found.complete = false;
            break;
        }
        if entry.bytes > PLAN_MAX_BYTES {
            found.unreadable += 1;
            continue;
        }
        let Some(bytes) = found
            .bytes
            .checked_add(entry.bytes)
            .filter(|bytes| *bytes <= max_bytes)
        else {
            found.complete = false;
            break;
        };
        found.read += 1;
        found.bytes = bytes;
        let Ok(plan) = load(&entry.path) else {
            found.unreadable += 1;
            continue;
        };
        if plan.expires_at <= now {
            found.expired += 1;
        } else if plan.expires_at > latest {
            found.future += 1;
        } else {
            found.live += 1;
            found.blobs.extend(
                plan.releases
                    .into_iter()
                    .filter_map(|release| release.blob_sha256)
                    .filter(|hash| {
                        hash.len() == 64
                            && hash
                                .bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    }),
            );
        }
    }
    found
}

// ---------------------------------------------------------------------------------------
// Admission.

/// Whether a preview's plan was kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Written now, or `reused`: an unexpired plan of the same intent and token, with its
    /// own id and expiry.
    Stored {
        plan_id: PlanId,
        expires_at: Timestamp,
        reused: bool,
    },
    NotStored(NotStored),
}

/// Why a preview's plan was not kept (the warning `plan_not_stored`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotStored {
    /// The store holds as many plans, or bytes, as its quotas allow: nothing unexpired
    /// is evicted for it.
    Full {
        plans: usize,
        bytes: u64,
    },
    TooLarge {
        bytes: u64,
    },
    /// Another admission held the store's lease past the timeout.
    Busy,
    Io(String),
}

/// What admission read and swept, for `-v` and the bounds' tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdmissionReport {
    pub listed: usize,
    pub complete: bool,
    pub sweep: SweepReport,
    /// Plan files opened, by the sweep and by the dedupe.
    pub read: usize,
}

/// Keeps `candidate`, a preview's plan: under the store's own lease (never the global
/// lock), it deletes a batch of expired plans, reuses an unexpired plan of the same
/// intent and token, and writes the candidate only when the store then stays within
/// both quotas. Nothing unexpired is ever evicted.
pub fn store(paths: &Paths, candidate: &StoredPlan) -> (Admission, AdmissionReport) {
    admit(paths, candidate, crate::clock::now(), LOCK_ACQUIRE_TIMEOUT)
}

/// [`store`] at `now`, waiting at most `timeout` for the store's lease.
fn admit(
    paths: &Paths,
    candidate: &StoredPlan,
    now: Timestamp,
    timeout: Duration,
) -> (Admission, AdmissionReport) {
    let mut report = AdmissionReport::default();
    let not_stored = |why: NotStored, report: AdmissionReport| (Admission::NotStored(why), report);
    let bytes = match encode(candidate) {
        Ok(bytes) => bytes,
        Err(err) => return not_stored(NotStored::Io(err.message.clone()), report),
    };
    let size = bytes.len() as u64;
    if size > PLAN_MAX_BYTES {
        return not_stored(NotStored::TooLarge { bytes: size }, report);
    }
    let scopes = match ScopeTable::new(paths, &[], None) {
        Ok(scopes) => scopes,
        Err(err) => return not_stored(NotStored::Io(err.message.clone()), report),
    };
    let _lease = match Lease::acquire(&scopes, &paths.plans_lock(), "plan store", timeout) {
        Ok(lease) => lease,
        Err(LeaseError::Busy { .. }) => return not_stored(NotStored::Busy, report),
        Err(err) => return not_stored(NotStored::Io(err.to_string()), report),
    };
    if let Err(err) = failpoint::hit("plans.admit.locked") {
        return not_stored(NotStored::Io(err.to_string()), report);
    }
    let listing = match list(paths) {
        Ok(listing) => listing,
        Err(err) => return not_stored(NotStored::Io(err.message.clone()), report),
    };
    report.listed = listing.plans.len();
    report.complete = listing.complete;
    let (sweep, removed) = sweep_locked(&scopes, &listing, now);
    report.sweep = sweep;
    report.read = sweep.read;
    let live: Vec<&ListedPlan> = listing
        .plans
        .iter()
        .filter(|p| !removed.contains(&p.id))
        .collect();
    let plans = live.len();
    // Metadata is untrusted too: several enormous sparse files must refuse admission
    // rather than overflow the total and appear to fit.
    let held = live
        .iter()
        .fold(0_u64, |total, p| total.saturating_add(p.bytes));
    // Newest first: a preview repeated reuses the plan it stored a moment ago.
    for listed in live.iter().rev() {
        report.read += 1;
        let Ok(plan) = load(&listed.path) else {
            continue;
        };
        if plan.expires_at > now
            && plan.intent == candidate.intent
            // ubs:ignore — public approval digests, not credentials
            && plan.precondition.token == candidate.precondition.token
        {
            let stored = Admission::Stored {
                plan_id: plan.plan_id,
                expires_at: plan.expires_at,
                reused: true,
            };
            return (stored, report);
        }
    }
    if !listing.complete
        || plans + 1 > PLANS_MAX_FILES
        || held.saturating_add(size) > PLANS_MAX_BYTES
    {
        return not_stored(NotStored::Full { plans, bytes: held }, report);
    }
    if let Err(err) = write(&scopes, paths, &candidate.plan_id, &bytes) {
        return not_stored(NotStored::Io(err.message.clone()), report);
    }
    if let Err(err) = failpoint::hit("plans.admit.written") {
        return not_stored(NotStored::Io(err.to_string()), report);
    }
    let stored = Admission::Stored {
        plan_id: candidate.plan_id,
        expires_at: candidate.expires_at,
        reused: false,
    };
    (stored, report)
}

/// Writes a new plan file: mode 0600 (the intent and reason are the user's), and only
/// where nothing is, since a plan file is never replaced.
fn write(scopes: &ScopeTable, paths: &Paths, id: &PlanId, bytes: &[u8]) -> Result<()> {
    let mut s = WriteSession::untracked(Writer::Derived, scopes, "plan");
    ensure_dir_all(&mut s, &paths.plans_dir())?;
    mutate_with(
        &mut s,
        &paths.plan(id),
        Op::WriteFile {
            content: Content::Bytes(bytes),
            mode: Some(0o600),
        },
        MutateOpts {
            expect: Some(Expect::Absent),
            ..MutateOpts::default()
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod pins_tests;
#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use crate::app::approval::ApprovalScope;
    use crate::app::intent::RemoveIntent;

    /// The earliest of the plan's creation, the time its id (or prefix) encodes and,
    /// without a readable plan, a day before now (M4-D63).
    #[test]
    fn the_evidence_horizon_is_the_earliest_bound() {
        let at = |text: &str| text.parse::<Timestamp>().unwrap();
        let id_at = |text: &str| {
            let ms = u64::try_from(at(text).as_millisecond()).unwrap();
            ulid::Ulid::from_parts(ms, 7).to_string()
        };
        let now = at("2026-10-29T12:00:00Z");
        let created = at("2026-10-28T12:00:00Z");
        let (early, late) = (id_at("2026-10-28T11:00:00Z"), id_at("2026-10-28T13:00:00Z"));
        let older = id_at("2026-10-20T00:00:00Z");
        for (case, created_at, given, want) in [
            ("the plan's creation", Some(created), late.as_str(), created),
            (
                "an id older than its creation",
                Some(created),
                early.as_str(),
                at("2026-10-28T11:00:00Z"),
            ),
            (
                "no file: a day before now",
                None,
                late.as_str(),
                at("2026-10-28T12:00:00Z"),
            ),
            (
                "no file, an older id",
                None,
                older.as_str(),
                at("2026-10-20T00:00:00Z"),
            ),
            ("not an id", None, "not-a-plan", at("2026-10-28T12:00:00Z")),
            ("not an id, a file", Some(created), "not-a-plan", created),
        ] {
            assert_eq!(
                evidence_horizon(created_at, given, now),
                want,
                "{case}: created {created_at:?}, given {given}"
            );
        }
        // An 8-character prefix encodes the earliest time it could be (the rest read as
        // 0: at most 1,023 ms before the full id's), in any case.
        let full = at("2026-10-28T11:00:00Z");
        for prefix in [early[..8].to_owned(), early[..8].to_ascii_lowercase()] {
            let got = evidence_horizon(None, &prefix, now);
            assert!(
                got <= full && full.duration_since(got) < SignedDuration::from_millis(1_024),
                "{prefix}: {got}"
            );
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        scopes: ScopeTable,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(
            &root,
            root.join("config"),
            root.join("state"),
            root.join("cache"),
        );
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        Fixture {
            _dir: dir,
            paths,
            scopes,
        }
    }

    #[test]
    fn find_cases() {
        const A: &str = "01M9P00000000000000000000A";
        const B: &str = "01M9P00000000000000000000B";
        const C: &str = "01M9Q00000000000000000000C";
        let ids =
            |list: &[&str]| -> Vec<PlanId> { list.iter().map(|s| s.parse().unwrap()).collect() };
        // Each case: its name, the files in `<cache>/plans/` (`None`: no directory), what
        // `find` is given, and what it should find.
        let one = |id: &str| Some(id.parse::<PlanId>().unwrap());
        let lowercase = C.to_ascii_lowercase();
        type Want = std::result::Result<Option<PlanId>, Vec<PlanId>>;
        let cases: Vec<(&str, Option<Vec<String>>, &str, Want)> = vec![
            ("full_id", Some(vec![format!("{A}.json")]), A, Ok(one(A))),
            (
                "prefix",
                Some(vec![format!("{A}.json"), format!("{C}.json")]),
                "01M9P000",
                Ok(one(A)),
            ),
            (
                "lowercase_prefix",
                Some(vec![format!("{C}.json")]),
                "01m9q000",
                Ok(one(C)),
            ),
            (
                "lowercase_full_id",
                Some(vec![format!("{C}.json")]),
                &lowercase,
                Ok(one(C)),
            ),
            (
                "none",
                Some(vec![format!("{A}.json")]),
                "01M9Z000",
                Ok(None),
            ),
            (
                "many_sorted",
                Some(vec![format!("{B}.json"), format!("{A}.json")]),
                "01M9P000",
                Err(ids(&[A, B])),
            ),
            ("directory_missing", None, "01M9P000", Ok(None)),
            ("full_id_missing", Some(Vec::new()), A, Ok(None)),
            (
                "non_plan_names_ignored",
                Some(vec![
                    ".lock".into(),
                    format!("{A}.lease"),
                    "notes.txt".into(),
                ]),
                "01M9P000",
                Ok(None),
            ),
        ];
        for (name, files, given, want) in cases {
            let f = fixture();
            if let Some(files) = &files {
                fs::create_dir_all(f.paths.plans_dir()).unwrap();
                for file in files {
                    fs::write(f.paths.plans_dir().join(file), b"{}").unwrap();
                }
            }
            let got = find(&f.paths, given).unwrap();
            let got = match got {
                Found::One { id, path } => {
                    assert_eq!(path, f.paths.plan(&id), "{name}");
                    Ok(Some(id))
                }
                Found::None { cut } => {
                    assert!(!cut, "{name}: a short listing is never cut");
                    Ok(None)
                }
                Found::Many(ids) => Err(ids),
            };
            assert_eq!(got, want, "{name}: files {files:?}, given {given:?}");
        }
    }

    #[test]
    fn find_reports_a_listing_cut_by_its_bound() {
        let f = fixture();
        fs::create_dir_all(f.paths.plans_dir()).unwrap();
        // More plan names than one listing reads, and none matching what is asked.
        for _ in 0..=PLANS_MAX_FILES + PLANS_SWEEP_MAX {
            let id = PlanId::new();
            fs::write(f.paths.plan(&id), b"{}").unwrap();
        }
        assert_eq!(
            find(&f.paths, "7ZZZZZZZ").unwrap(),
            Found::None { cut: true },
            "a match may be among the names not read"
        );
    }

    #[test]
    fn a_symlinked_plan_is_not_one() {
        const A: &str = "01M9P00000000000000000000A";
        let f = fixture();
        fs::create_dir_all(f.paths.plans_dir()).unwrap();
        let elsewhere = f.paths.plans_dir().join("elsewhere");
        fs::write(&elsewhere, b"{}").unwrap();
        symlink(&elsewhere, f.paths.plans_dir().join(format!("{A}.json"))).unwrap();
        for given in [A, "01M9P000"] {
            assert_eq!(
                find(&f.paths, given).unwrap(),
                Found::None { cut: false },
                "{given}"
            );
        }
    }

    fn now() -> Timestamp {
        "2026-10-26T12:00:00Z".parse().unwrap()
    }

    fn id(n: usize) -> PlanId {
        format!("{:026}", n + 1).parse().unwrap()
    }

    pub(super) fn plan_at(n: usize, created_at: Timestamp) -> StoredPlan {
        let intent = StoredIntent::Remove(RemoveIntent {
            addons: vec!["Foo".into()],
            target: crate::model::TargetRef::parse("retail"),
            target_id: "retail".into(),
            while_running: false,
        });
        let precondition = Precondition::new(
            "remove",
            vec![(crate::model::TargetRef::parse("retail"), Some(7))],
            json!({"retail": ["Foo"]}),
            json!({"generation": 7}),
        );
        StoredPlan {
            plan_id: id(n),
            created_at,
            expires_at: created_at
                .checked_add(SignedDuration::from_hours(24))
                .unwrap(),
            tool_version: env!("CARGO_PKG_VERSION").into(),
            build_id: crate::build_info::BUILD_ID.into(),
            display_argv: intent.display_argv(),
            intent,
            actor: Actor::Agent,
            reason: None,
            precondition: StoredPrecondition {
                token: precondition.token(),
                parts: StoredParts::of(&precondition),
            },
            releases: Vec::new(),
            approval: Approval {
                routine: true,
                reasons: Vec::new(),
                selection: Vec::new(),
                scope: ApprovalScope::default(),
            },
            summary: vec!["Remove Foo".into()],
        }
    }

    fn live(n: usize) -> StoredPlan {
        plan_at(n, now())
    }

    fn expired(n: usize) -> StoredPlan {
        plan_at(
            n,
            now().checked_sub(SignedDuration::from_hours(25)).unwrap(),
        )
    }

    fn plant(f: &Fixture, plan: &StoredPlan) -> PathBuf {
        fs::create_dir_all(f.paths.plans_dir()).unwrap();
        let path = f.paths.plan(&plan.plan_id);
        fs::write(&path, encode(plan).unwrap()).unwrap();
        path
    }

    fn admit_now(f: &Fixture, plan: &StoredPlan) -> (Admission, AdmissionReport) {
        admit(&f.paths, plan, now(), Duration::from_millis(50))
    }

    fn assert_new(result: &(Admission, AdmissionReport), plan: &StoredPlan) {
        assert_eq!(
            result.0,
            Admission::Stored {
                plan_id: plan.plan_id,
                expires_at: plan.expires_at,
                reused: false,
            },
            "{result:?}"
        );
    }

    #[test]
    fn a_plan_round_trips_compact_with_schema_version_first() {
        let f = fixture();
        let plan = live(0);
        let bytes = encode(&plan).unwrap();
        assert!(bytes.starts_with(b"{\"schema_version\":\"1.2\",\"plan_id\":"));
        assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 1);
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(load(&plant(&f, &plan)).unwrap(), plan);
        assert_eq!(load_id(&f.paths, &plan.plan_id).unwrap(), Some(plan));
        assert_eq!(load_id(&f.paths, &id(1)).unwrap(), None);
        assert!(
            store::KINDS
                .iter()
                .any(|row| row.kind == "plan" && row.current == SchemaVersion::new(1, 2))
        );
    }

    #[test]
    fn load_fails_closed() {
        let f = fixture();
        let original = live(0);
        let path = plant(&f, &original);
        let good: Value = serde_json::from_slice(&encode(&original).unwrap()).unwrap();
        let cases = [
            ("truncated", "does not decode"),
            ("oversized", "larger than"),
            ("unknown variant", "does not decode"),
            ("unknown option", "does not decode"),
            ("relative path", "does not decode"),
            ("non-normalized path", "does not decode"),
            ("newer minor", "newer"),
            ("newer major", "newer"),
            ("id mismatch", "holds plan"),
            ("extended expiry", "expiry"),
            ("reversed expiry", "expiry"),
            ("display argv", "display_argv"),
            ("token", "token"),
            ("parts", "token"),
            ("command part", "token"),
            ("long reason", "reason"),
            ("invalid intent", "intent breaks"),
            ("symlink", "regular file"),
            ("directory", "regular file"),
            ("untouched", ""),
        ];
        for (case, cause) in cases {
            let mut value = good.clone();
            match case {
                "unknown variant" => value["intent"]["command"] = json!("auth"),
                "unknown option" => value["intent"]["yes"] = json!(true),
                "relative path" | "non-normalized path" => {
                    value["intent"] = json!({
                        "command": "install", "refs": ["gh:o/foo"],
                        "target": "retail", "target_id": "retail",
                        "channel": null, "version": null,
                        "file": if case == "relative path" { "foo.zip" } else { "/foo/../bar.zip" },
                        "reinstall": false, "no_deps": false, "force": false,
                        "strict": false, "refresh": false, "any_flavor": false,
                        "no_any_flavor": false, "while_running": false
                    });
                }
                "newer minor" => value["schema_version"] = json!("1.3"),
                "newer major" => value["schema_version"] = json!("2.0"),
                "id mismatch" => value["plan_id"] = json!(id(1)),
                "extended expiry" => value["expires_at"] = json!("2026-10-28T12:00:00Z"),
                "reversed expiry" => value["expires_at"] = value["created_at"].clone(),
                "display argv" => value["display_argv"] = json!(["omaddons", "auth"]),
                "token" => value["precondition"]["token"] = json!("pc1:edited"),
                "parts" => value["precondition"]["parts"]["observed"] = json!({"generation": 8}),
                "command part" => {
                    let mut parts = original.precondition.parts.clone();
                    parts.command = json!("update");
                    value["precondition"]["parts"] = serde_json::to_value(&parts).unwrap();
                    value["precondition"]["token"] = json!(token_of_parts(&parts.to_array()));
                }
                "long reason" => value["reason"] = json!("é".repeat(REASON_MAX_CHARS + 1)),
                "invalid intent" => value["intent"]["addons"] = json!([]),
                _ => {}
            }
            let bytes = match case {
                "truncated" => b"{\"schema_version\":".to_vec(),
                "oversized" => vec![b' '; PLAN_MAX_BYTES as usize + 1],
                _ => serde_json::to_vec(&value).unwrap(),
            };
            fs::write(&path, bytes).unwrap();
            if case == "symlink" {
                fs::rename(&path, path.with_extension("original")).unwrap();
                symlink(path.with_extension("original"), &path).unwrap();
            } else if case == "directory" {
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
            let result = load(&path);
            if case == "untouched" {
                assert_eq!(result.unwrap(), original, "{case}");
            } else {
                let err = result.expect_err(case);
                assert_eq!(err.code, ErrorCode::PlanNotFound, "{case}: {err:?}");
                assert_eq!(
                    err.details,
                    json!({
                        "plan_id": original.plan_id, "path": path, "reason": "unreadable"
                    }),
                    "{case}: {err:?}"
                );
                assert!(err.message.contains(cause), "{case}: {err:?}");
            }
            if case == "directory" {
                fs::remove_dir(&path).unwrap();
            } else {
                fs::remove_file(&path).unwrap();
            }
        }
    }

    #[test]
    fn list_classifies_and_bounds() {
        let f = fixture();
        assert_eq!(
            list(&f.paths).unwrap(),
            Listing {
                complete: true,
                ..Listing::default()
            }
        );
        let planted: Vec<_> = (0..3).map(|n| plant(&f, &live(n))).collect();
        for n in 0..2 {
            fs::write(f.paths.plan_lease(&id(n)), b"").unwrap();
        }
        for name in [".lock", ".omaddons-x", "notes.txt"] {
            fs::write(f.paths.plans_dir().join(name), b"").unwrap();
        }
        symlink(&planted[0], f.paths.plan(&id(3))).unwrap();
        let listing = list(&f.paths).unwrap();
        assert!(listing.complete, "{listing:?}");
        assert_eq!(listing.plans.len(), 3, "{listing:?}");
        assert_eq!(listing.leases, vec![id(0), id(1)]);
        assert_eq!(listing.others, 4);
        for (plan, path) in listing.plans.iter().zip(planted) {
            assert_eq!(plan.path, path);
            assert_eq!(plan.bytes, fs::metadata(path).unwrap().len());
        }
        let f = fixture();
        for n in 0..PLANS_MAX_FILES + PLANS_SWEEP_MAX + 1 {
            plant(&f, &live(n));
        }
        let listing = list(&f.paths).unwrap();
        assert!(!listing.complete, "{listing:?}");
        assert_eq!(listing.plans.len(), PLANS_MAX_FILES + PLANS_SWEEP_MAX);
        let f = fixture();
        fs::create_dir_all(f.paths.plans_dir()).unwrap();
        for n in 0..2 * (PLANS_MAX_FILES + PLANS_SWEEP_MAX) + 2 {
            fs::write(f.paths.plans_dir().join(format!("foreign-{n}")), b"").unwrap();
        }
        let listing = list(&f.paths).unwrap();
        assert!(!listing.complete, "{listing:?}");
        assert_eq!(
            listing.others,
            (2 * (PLANS_MAX_FILES + PLANS_SWEEP_MAX) + 1) as u64
        );
    }

    #[test]
    fn sweep_takes_the_oldest_expired_first_and_stops_at_the_first_live_plan() {
        let f = fixture();
        for n in 0..70 {
            plant(&f, &expired(n));
        }
        for n in 70..72 {
            plant(&f, &live(n));
        }
        let report = sweep(&f.paths, now()).unwrap();
        assert_eq!(report.removed, PLANS_SWEEP_MAX, "{report:?}");
        assert_eq!(report.read, PLANS_SWEEP_MAX, "{report:?}");
        assert_eq!(
            list(&f.paths)
                .unwrap()
                .plans
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            (64..72).map(id).collect::<Vec<_>>(),
            "{report:?}"
        );
        // An expired plan after the first live one waits for a later sweep.
        plant(&f, &expired(72));
        let report = sweep(&f.paths, now()).unwrap();
        assert_eq!(report.removed, 6, "{report:?}");
        assert_eq!(report.read, 7, "{report:?}");
        assert!(f.paths.plan(&id(72)).exists());
    }

    #[test]
    fn sweep_skips_unreadable_plans_and_counts_them() {
        let f = fixture();
        let bad = plant(&f, &expired(0));
        fs::write(&bad, b"{").unwrap();
        plant(&f, &expired(1));
        plant(&f, &live(2));
        let report = sweep(&f.paths, now()).unwrap();
        assert_eq!(
            report,
            SweepReport {
                removed: 1,
                skipped_unreadable: 1,
                read: 3,
                ..SweepReport::default()
            }
        );
        assert!(bad.exists());
    }

    #[test]
    fn sweep_removes_leases_with_their_plans_and_orphans() {
        let f = fixture();
        for n in 0..3 {
            plant(&f, &expired(n));
        }
        for n in [0, 1, 3] {
            drop(
                Lease::try_acquire(&f.scopes, &f.paths.plan_lease(&id(n)), "test")
                    .unwrap()
                    .unwrap(),
            );
        }
        let held = Lease::try_acquire(&f.scopes, &f.paths.plan_lease(&id(1)), "held")
            .unwrap()
            .unwrap();
        let report = sweep(&f.paths, now()).unwrap();
        assert_eq!(report.removed, 2, "{report:?}");
        assert_eq!(report.leases_removed, 2, "{report:?}");
        assert_eq!(report.refused, 1, "{report:?}");
        // Guarding plan 2 created a lease absent from the original listing; it is an
        // orphan now, collected by the next bounded sweep.
        assert_eq!(list(&f.paths).unwrap().leases, vec![id(1), id(2)]);
        assert!(f.paths.plan(&id(1)).exists());
        drop(held);
        let report = sweep(&f.paths, now()).unwrap();
        assert_eq!(report.removed, 1, "{report:?}");
        assert_eq!(report.leases_removed, 2, "{report:?}");
        assert!(list(&f.paths).unwrap().leases.is_empty());
    }

    #[test]
    fn sweep_is_idle_while_admission_is_locked() {
        let f = fixture();
        plant(&f, &expired(0));
        let _held = Lease::try_acquire(&f.scopes, &f.paths.plans_lock(), "test")
            .unwrap()
            .unwrap();
        assert_eq!(sweep(&f.paths, now()).unwrap(), SweepReport::default());
        assert!(f.paths.plan(&id(0)).exists());
    }

    #[test]
    fn a_lease_taken_after_listing_protects_its_plan() {
        let f = fixture();
        plant(&f, &expired(0));
        let listing = list(&f.paths).unwrap();
        assert!(listing.leases.is_empty());
        let held = Lease::try_acquire(&f.scopes, &f.paths.plan_lease(&id(0)), "apply")
            .unwrap()
            .unwrap();
        let (report, removed) = sweep_locked(&f.scopes, &listing, now());
        assert_eq!(report.refused, 1, "{report:?}");
        assert!(removed.is_empty(), "{removed:?}");
        assert!(f.paths.plan(&id(0)).exists());
        drop(held);
        let (report, _) = sweep_locked(&f.scopes, &listing, now());
        assert_eq!(report.removed, 1, "{report:?}");
    }

    #[test]
    fn admission_dedupes_on_intent_and_token() {
        let f = fixture();
        let first = live(0);
        assert_new(&admit_now(&f, &first), &first);
        let bytes = fs::read(f.paths.plan(&first.plan_id)).unwrap();
        let mut repeated = live(1);
        repeated.actor = Actor::Widget;
        repeated.reason = Some("a second preview".into());
        repeated.summary = vec!["another summary".into()];
        let result = admit_now(&f, &repeated);
        assert_eq!(
            result.0,
            Admission::Stored {
                plan_id: first.plan_id,
                expires_at: first.expires_at,
                reused: true
            },
            "{result:?}"
        );
        assert_eq!(list(&f.paths).unwrap().plans.len(), 1);
        assert_eq!(fs::read(f.paths.plan(&first.plan_id)).unwrap(), bytes);
        repeated.precondition.parts.observed = json!({"generation": 8});
        repeated.precondition.token = token_of_parts(&repeated.precondition.parts.to_array());
        assert_new(&admit_now(&f, &repeated), &repeated);
        let mut different = live(2);
        if let StoredIntent::Remove(ref mut intent) = different.intent {
            intent.addons = vec!["Bar".into()];
        }
        different.display_argv = different.intent.display_argv();
        assert_new(&admit_now(&f, &different), &different);
        assert_eq!(list(&f.paths).unwrap().plans.len(), 3);
    }

    fn distinct(n: usize) -> StoredPlan {
        let mut plan = live(n);
        plan.precondition.parts.observed = json!(n);
        plan.precondition.token = token_of_parts(&plan.precondition.parts.to_array());
        plan
    }

    #[test]
    fn admission_declines_when_full_by_count() {
        let f = fixture();
        let originals: Vec<_> = (0..PLANS_MAX_FILES)
            .map(|n| {
                let plan = distinct(n);
                let path = plant(&f, &plan);
                (path, encode(&plan).unwrap())
            })
            .collect();
        let candidate = distinct(PLANS_MAX_FILES);
        let result = admit_now(&f, &candidate);
        assert!(
            matches!(
                result.0,
                Admission::NotStored(NotStored::Full {
                    plans: PLANS_MAX_FILES,
                    ..
                })
            ),
            "{result:?}"
        );
        assert!(result.1.read <= PLANS_MAX_FILES + 1, "{result:?}");
        assert!(!f.paths.plan(&candidate.plan_id).exists());
        for (path, bytes) in originals {
            assert_eq!(fs::read(&path).unwrap(), bytes, "{path:?}");
        }
        for n in 0..3 {
            plant(&f, &expired(n));
        }
        let result = admit_now(&f, &candidate);
        assert_new(&result, &candidate);
        assert_eq!(result.1.sweep.removed, 3, "{result:?}");
        assert_eq!(list(&f.paths).unwrap().plans.len(), PLANS_MAX_FILES - 2);
    }

    #[test]
    fn admission_declines_when_full_by_bytes() {
        let f = fixture();
        let mut held = 0;
        for n in 0..(PLANS_MAX_BYTES / PLAN_MAX_BYTES) {
            let mut plan = distinct(n as usize);
            let size = encode(&plan).unwrap().len();
            plan.summary
                .push("x".repeat(PLAN_MAX_BYTES as usize - size - 3));
            let path = plant(&f, &plan);
            held += fs::metadata(&path).unwrap().len();
        }
        assert_eq!(held, PLANS_MAX_BYTES);
        let candidate = distinct(PLANS_MAX_FILES);
        let result = admit_now(&f, &candidate);
        assert!(
            matches!(
                result.0,
                Admission::NotStored(NotStored::Full {
                    bytes: PLANS_MAX_BYTES,
                    ..
                })
            ),
            "{result:?}"
        );
        assert!(!f.paths.plan(&candidate.plan_id).exists());
        assert_eq!(
            list(&f.paths)
                .unwrap()
                .plans
                .iter()
                .map(|p| p.bytes)
                .sum::<u64>(),
            held
        );
    }

    #[test]
    fn admission_never_exceeds_the_quota_after_a_bounded_sweep() {
        let f = fixture();
        for n in 0..600 {
            plant(&f, &expired(n));
        }
        let candidate = distinct(600);
        let result = admit_now(&f, &candidate);
        assert!(
            matches!(result.0, Admission::NotStored(NotStored::Full { .. })),
            "{result:?}"
        );
        assert!(!result.1.complete, "{result:?}");
        assert_eq!(result.1.sweep.removed, PLANS_SWEEP_MAX, "{result:?}");
        assert!(!f.paths.plan(&candidate.plan_id).exists());
        assert_eq!(
            fs::read_dir(f.paths.plans_dir())
                .unwrap()
                .filter_map(|e| {
                    let path = e.unwrap().path();
                    (path.extension().is_some_and(|e| e == "json")).then_some(path)
                })
                .count(),
            600 - PLANS_SWEEP_MAX
        );
    }

    #[test]
    fn admission_declines_an_oversized_plan() {
        let f = fixture();
        let mut plan = live(0);
        let size = encode(&plan).unwrap().len();
        plan.summary
            .push("x".repeat(PLAN_MAX_BYTES as usize - size - 2));
        let bytes = encode(&plan).unwrap().len() as u64;
        assert_eq!(bytes, PLAN_MAX_BYTES + 1);
        let result = admit_now(&f, &plan);
        assert_eq!(
            result,
            (
                Admission::NotStored(NotStored::TooLarge { bytes }),
                AdmissionReport::default()
            )
        );
        assert!(!f.paths.plans_lock().exists());
        assert!(!f.paths.plan(&plan.plan_id).exists());
    }

    #[test]
    fn admission_reports_busy() {
        let f = fixture();
        let _held = Lease::try_acquire(&f.scopes, &f.paths.plans_lock(), "test")
            .unwrap()
            .unwrap();
        assert_eq!(
            admit_now(&f, &live(0)),
            (
                Admission::NotStored(NotStored::Busy),
                AdmissionReport::default()
            )
        );
        assert!(!f.paths.plan(&id(0)).exists());
    }

    #[test]
    fn the_store_files_are_0600_and_never_replaced() {
        let f = fixture();
        let plan = live(0);
        assert_new(&admit_now(&f, &plan), &plan);
        let path = f.paths.plan(&plan.plan_id);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let original = fs::read(&path).unwrap();
        let mut edited = plan.clone();
        edited.precondition.parts.observed = json!("edited");
        edited.precondition.token = token_of_parts(&edited.precondition.parts.to_array());
        assert!(matches!(
            admit_now(&f, &edited).0,
            Admission::NotStored(NotStored::Io(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}
