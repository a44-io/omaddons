//! Safety limits. Change a value here or nowhere.
//!
//! These are the vision §5.13 `src/limits.rs` table (D22). Changing a default means
//! updating that table in a comment on the vision bead. Only the constants marked
//! "(config)" may be overridden, through the `[limits]` table of `config.toml` keyed by
//! the lowercase constant name; [`EffectiveLimits`] holds the result, and subsystems take
//! it as a parameter instead of reading these constants directly.

use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;

use crate::politeness::DEFAULT_CHECK_INTERVAL;

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Most entries an archive may hold. (config)
pub const ARCHIVE_MAX_ENTRIES: u64 = 20_000;
/// Most bytes an archive may expand to. (config)
pub const ARCHIVE_MAX_UNCOMPRESSED: u64 = GIB;
/// Zip-bomb guard: the N of the largest allowed N:1 compression ratio, checked per entry
/// and in total. (config)
pub const ARCHIVE_MAX_RATIO: u64 = 200;
/// The ratio applies once an entry, or the whole archive, has expanded past this many
/// bytes: repetitive files are not bombs (10 KB of padding deflates over 300:1, and a
/// mostly transparent 512x512 texture about 1000:1). 32 MiB covers a 2048x2048 RGBA
/// texture; `ARCHIVE_MAX_UNCOMPRESSED` still bounds every archive.
pub const ARCHIVE_RATIO_FLOOR: u64 = 32 * MIB;
/// Largest download accepted. (config)
pub const DOWNLOAD_MAX_BYTES: u64 = 512 * MIB;
/// How long to wait for the global lock, then `locked` (exit 5), or `concurrency_lost`
/// in the doctor.
pub const LOCK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// Snapshot retention: always at least 24 h; beyond that, the last 10 transactions
/// within 7 days per target. (config, except the 24 h minimum age)
pub const TX_RETENTION: TxRetention = TxRetention {
    min_age: DAY,
    keep_last: 10,
    window: Duration::from_secs(7 * 24 * 60 * 60),
};
/// Snapshot bytes per target beyond the 24 h protection window. (config)
pub const SNAPSHOT_MAX_BYTES: u64 = 2 * GIB;
/// Blob cache size; LRU with pins. (config)
pub const BLOB_CACHE_MAX_BYTES: u64 = GIB;
/// How long a stored plan stays applicable.
pub const PLAN_TTL: Duration = DAY;
/// How long a second `apply` of one plan waits for the first's lease, then `locked`
/// (M4-D61).
pub const PLAN_LEASE_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest stored plan file (M4-D24): none is written larger, and every read of one,
/// or of a plan store lock's holder line, stops past it.
pub const PLAN_MAX_BYTES: u64 = 256 * 1024;
/// Hard cap on stored plans (M4-D24): at it, a dry run stores nothing
/// (`plan_not_stored`); nothing unexpired is ever evicted to make room.
pub const PLANS_MAX_FILES: usize = 512;
/// Hard cap on the plan store's total size (M4-D24).
pub const PLANS_MAX_BYTES: u64 = 32 * MIB;
/// Expired plans one cleanup batch deletes at most (M4-D24).
pub const PLANS_SWEEP_MAX: usize = 64;
/// History lines `apply`'s `already_applied` check and `plans show` read backwards at
/// most; past it the evidence is incomplete, never "none".
pub const PLAN_EVIDENCE_LINES_MAX: usize = 20_000;
/// Retention sweeps a stage dir or in-flight lock whose transaction is dead (lock free,
/// no journal) only once it is this old: a margin against clocks and races.
pub const ORPHAN_MIN_AGE: Duration = Duration::from_secs(60 * 60);
/// How often snapshot retention also passes over every other registered client
/// discovery found, after a commit or a backup: a client nobody changes is pruned too.
pub const RETENTION_SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Longest `--reason`, in Unicode scalar values.
pub const REASON_MAX_CHARS: usize = 200;
/// `net.log` size before it rotates once, keeping one previous file.
pub const NET_LOG_MAX_BYTES: u64 = 10 * MIB;
/// Lua parser input cap (`errors`).
pub const SAVEDVARIABLES_MAX_BYTES: u64 = 64 * MIB;
/// Client rule: the client reads only the first 1024 characters of each TOC line.
pub const TOC_LINE_MAX: usize = 1024;
/// Time budget per doctor `--quick` / `health` detector.
pub const QUICK_DETECTOR_BUDGET: Duration = Duration::from_millis(5);
/// A default or deep doctor check's run-time deadline without `--budget` (M4-D43); past
/// it the check stops and the run reports it incomplete.
pub const DOCTOR_DETECTOR_DEADLINE: Duration = Duration::from_secs(2);
/// Fresh ULIDs a doctor run draws after its run directory's name is taken (M4-D33):
/// three attempts in all, then the run cannot create its directory.
pub const RUN_ID_RETRY: u32 = 2;
/// Most bytes discovery reads from one client metadata file (`.build.info`,
/// `.flavor.info`, `Config.wtf`, `realmlist.wtf`, …); the rest is ignored. Also the most
/// the doctor's view reads of one state or config file (`config.toml`, `registry.json`,
/// a target's record): a larger one is a finding, never read in part.
pub const DISCOVERY_FILE_MAX_BYTES: u64 = MIB;
/// How long a credential `command` may run before it is killed.
pub const CREDENTIAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// Most bytes `auth github set` reads from standard input for the token's line.
pub const AUTH_TOKEN_MAX_BYTES: u64 = 4_096;
/// Most entries (and most candidate facts) the release memo keeps; past it, the oldest
/// go.
pub const RELEASE_MEMO_MAX_ENTRIES: usize = 4_096;
/// Most other releases a memo entry remembers beside the newest, for telling an
/// installed release that is ahead of the source's newest.
pub const MEMO_KNOWN_RELEASES: usize = 10;
/// A WoWInterface project unchanged for longer is never chosen on its own to satisfy a
/// dependency (`wowi_abandoned`); `search` calls it abandoned.
pub const DEPENDENCY_SOURCE_MAX_AGE: Duration = Duration::from_secs(730 * 24 * 60 * 60);
/// The longest changelog entry shown, in characters (longer ones end with `…`).
pub const CHANGELOG_MAX_CHARS: usize = 4_000;
/// Most changelog entries one answer shows.
pub const CHANGELOG_MAX_ENTRIES: usize = 10;
/// The largest decoded API response (JSON, feeds): a decompression-bomb guard. Archives
/// have `DOWNLOAD_MAX_BYTES`.
pub const API_RESPONSE_MAX_BYTES: u64 = 64 * MIB;
/// Most entries a WowAce directory listing or tag list may name, and most files one
/// WowAce tree may hold: past it, the listing fails `too_many_entries`.
pub const WOWACE_TREE_MAX_FILES: usize = 2_000;
/// Most directory listings one WowAce tree fetches.
pub const WOWACE_TREE_MAX_DIRS: usize = 200;
/// Deepest a WowAce tree's directories nest below its root.
pub const WOWACE_TREE_MAX_DEPTH: usize = 8;
/// Most requests one WowAce tree costs, retries included.
pub const WOWACE_TREE_MAX_REQUESTS: u32 = 2_500;
/// Most entries an imported backup archive may hold (M3-D35).
pub const BACKUP_IMPORT_MAX_ENTRIES: u64 = 200_000;
/// Most bytes an imported backup archive may expand to.
pub const BACKUP_IMPORT_MAX_BYTES: u64 = 8 * 1024 * MIB;
/// Longest path one entry of an imported archive may have, in bytes.
pub const BACKUP_IMPORT_MAX_PATH: u64 = 4096;
/// Largest PAX or GNU long-name record one entry of an imported archive may carry.
pub const BACKUP_IMPORT_MAX_META_BYTES: u64 = 64 * 1024;
/// Largest `manifest.json` an imported archive may carry: [`BACKUP_IMPORT_MAX_ENTRIES`]
/// at 640 bytes each, nearly three times what an entry of a real export takes (one
/// `backup.json` or `addons.json` stays within [`DISCOVERY_FILE_MAX_BYTES`]).
pub const BACKUP_IMPORT_MAX_MANIFEST_BYTES: u64 = BACKUP_IMPORT_MAX_ENTRIES * 640;
/// The zstd decoder window an import allows, as a power of two (16 MiB).
pub const BACKUP_IMPORT_MAX_WINDOW_LOG: u32 = 24;
/// Most dependency rounds one install plans per target (a chain longer than this stops
/// with `missing_dependency`, reason `limit_reached`).
pub const DEPENDENCY_MAX_DEPTH: usize = 4;
/// Most dependencies one transaction installs per target.
pub const DEPENDENCY_MAX_ADDONS: usize = 32;
/// How long after the newest staged file's mtime a commit stamps its folders' index
/// entries (it waits out the rest before taking the lock): covers the kernel's coarse
/// file-timestamp clock, so a file changed in the same tick is re-hashed, never
/// trusted (M2-D16). Nanoseconds, to compare with `mtime_ns`.
pub const RACY_SLACK_NS: i128 = 20_000_000;
/// Most other refs an adoption group lists beside its own (`alternatives`), each an
/// `adopt --ref` command in the report.
pub const ADOPT_MAX_ALTERNATIVES: usize = 3;
/// Most paths a drift check lists per category (`modified`, `deleted`, `added`) per
/// folder; `changed` still counts them all.
pub const DRIFT_LIST_MAX: usize = 50;
/// The racy slack on a filesystem with whole-second mtimes (FAT and the like): a file
/// whose mtime is this close to its folder's stamp is re-hashed. Nanoseconds.
pub const RACY_SLACK_COARSE_NS: i128 = 2_000_000_000;
/// Most results one `search` lists (`--limit`'s bound; its default is 20).
pub const SEARCH_MAX_RESULTS: usize = 50;
/// Characters a search result's `summary` keeps.
pub const SEARCH_SUMMARY_MAX_CHARS: usize = 200;
/// Characters kept from one name, author or version a source sends.
pub const SOURCE_TEXT_MAX_CHARS: usize = 200;
/// Account, realm and character names the privacy redactor learns per client (or
/// archive); any other maps to `account ?`, `realm ?` or `character ?`.
pub const REDACTOR_MAX_NAMES: usize = 4_096;
/// Entries walked in one `WTF/` tree a backup copies; then `archive_too_large`.
pub const BACKUP_TREE_MAX_ENTRIES: usize = 200_000;
/// Directory depth walked in one `WTF/` tree a backup copies; then `archive_too_large`.
pub const BACKUP_TREE_MAX_DEPTH: usize = 16;
/// Captures of one `WTF/` tried before one that changed while it was copied is
/// published `best_effort`.
pub const BACKUP_CAPTURE_ATTEMPTS: u32 = 2;
/// Free space beside a client's backups below which a manual backup warns
/// (`backup_space_low`) and automatic ones reclaim, then skip. (config, as
/// `limits.backup_min_free_mb`; 0 turns the check off)
pub const BACKUP_MIN_FREE: u64 = 2_048 * MIB;
/// The apparent bytes of a target's automatic byte-copy backups (`auto_backup =
/// "daily-copy"` only): beyond it, eligible ones are reclaimed oldest first, or the capture
/// is skipped. (config, as `limits.backup_copy_max_mb`; 0 turns the budget off)
pub const BACKUP_COPY_MAX: u64 = 4_096 * MIB;
/// A client whose newest qualifying backup was captured longer ago than this (or that
/// has none) is offered `backup create`.
pub const BACKUP_STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Local days of automatic backups kept per target: for each of the newest days that
/// hold any, its first pre-change and last post-session backup, and every before-restore
/// one. (config, as `limits.backup_auto_keep_days`, 1 to [`BACKUP_AUTO_KEEP_DAYS_MAX`])
pub const BACKUP_AUTO_KEEP_DAYS: usize = 7;
/// The most `limits.backup_auto_keep_days` accepts.
pub const BACKUP_AUTO_KEEP_DAYS_MAX: usize = 100;
/// No backup younger than this is ever pruned automatically, nor by `--keep` or
/// `--older-than`.
pub const BACKUP_MIN_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Runs with an undoable entry the history projection (`history_tail.json`) keeps whole,
/// newest first; runs with none (backups, settings) never count.
pub const HISTORY_TAIL_RUNS: usize = 16;
/// The most bytes of history lines the history projection keeps.
pub const HISTORY_TAIL_MAX_BYTES: u64 = MIB;
/// Table nesting the SavedVariables parser follows, building or skipping.
pub const LUA_MAX_DEPTH: usize = 64;
/// Values the SavedVariables parser builds per parse (skipped globals build none).
pub const LUA_MAX_NODES: u64 = 2_000_000;
/// Issues the SavedVariables parser records per parse; past it, only a count.
pub const LUA_MAX_ISSUES: usize = 64;
/// BugGrabber entries decoded per database, the newest kept (BugGrabber itself keeps
/// 500, r152 50).
pub const BUGGRABBER_MAX_ENTRIES: usize = 5_000;
/// Account folders `errors` examines per client for BugGrabber's database.
pub const ERRORS_MAX_ACCOUNTS: usize = 32;
/// Candidate BugGrabber files `errors` parses per client.
pub const ERRORS_MAX_FILES: usize = 64;
/// Characters of an error message's first line shown with `--raw-details`.
pub const ERRORS_MESSAGE_MAX_CHARS: usize = 300;
/// Stack frames shown by default.
pub const ERRORS_STACK_LINES: usize = 6;
/// Errors one addon lists in `errors`' output: its new ones first, then the most
/// frequent. Its `entries` counts every one.
pub const ERRORS_SHOWN_PER_ADDON: usize = 10;
/// Stack frames shown with `-v` or `--raw-details`.
pub const ERRORS_STACK_LINES_VERBOSE: usize = 40;
/// Characters of an error's locals shown with `--raw-details`.
pub const ERRORS_LOCALS_MAX_CHARS: usize = 4_096;
/// Characters of a `[string "…"]` chunk's contents shown with `--raw-details`.
pub const ERRORS_CHUNK_MAX_CHARS: usize = 40;
/// Bytes of BugGrabber files one `errors` run reads, every client together.
pub const ERRORS_MAX_TOTAL_BYTES: u64 = 256 * MIB;
/// A BugGrabber file last written longer ago than this has no new errors.
pub const ERRORS_NEW_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Bytes read of an installed widget's `manifest.json` or `Build.js` (a few kilobytes
/// when omaddons wrote them); a larger file is not one it wrote.
pub const WIDGET_READBACK_MAX_BYTES: u64 = 64 * 1024;
/// One run of an Omarchy tool (`omarchy-plugin-validate`, `omarchy-shell`,
/// `omarchy-bar`) is killed after this long.
pub const OMARCHY_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes read of Omarchy's `shell.json` to see where the bar places the widget; past
/// it, the widget counts as not placed (placing it again leaves it where it is).
pub const OMARCHY_SHELL_JSON_MAX_BYTES: u64 = 4 * MIB;
/// `omarchy status` asks the shell `shell ping`; a shell that has not answered `ok` in
/// this long counts as not running.
pub const OMARCHY_PING_TIMEOUT: Duration = Duration::from_secs(2);
/// An Omarchy tool that timed out gets SIGTERM, with its process group, and this long
/// to exit before the group gets SIGKILL.
pub const OMARCHY_KILL_GRACE: Duration = Duration::from_millis(200);
/// Most entries one doctor walk or directory listing visits per run; past it the layer
/// or check is incomplete, never clean. `<state>/targets/` holds two files per client, so
/// a listing over it is damage.
pub const DOCTOR_SCAN_MAX_ENTRIES: u64 = 20_000;
/// Most skill directories a scope table, `history_tail.skill_dirs[]` and the skill-drift
/// check admit (§7.9): the default one and those `robot-docs install-skill` was given.
pub const SKILL_DIRS_MAX: usize = 8;
/// Most primitives one doctor repair may plan (common rule 15); past it the run is
/// refused (`doctor_refused`, `too_many_actions`) before anything moves.
pub const DOCTOR_FIX_MAX_ACTIONS: usize = 1_000;
/// A global-lock holder older than this is `fm-concurrency_primitives-lock-held-long`.
pub const LOCK_HELD_LONG: Duration = Duration::from_secs(10 * 60);
/// `fix` and `undo` run directories past which `doctor-runs-accumulated` reports
/// (diagnose runs do not count).
pub const DOCTOR_RUNS_SOFT_MAX: u64 = 200;
/// The `--before` age remediations and usage fixes suggest (`doctor gc --before 30d
/// --dry-run`).
pub const DOCTOR_GC_SUGGESTED_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Victims one `doctor gc` deletes at most; the report says how many remain.
pub const DOCTOR_GC_MAX_DELETIONS: usize = 1_000;
/// Quarantine bytes past which `doctor-runs-accumulated` reports.
pub const DOCTOR_QUARANTINE_SOFT_MAX: u64 = GIB;
/// Most entries `targets rm --purge` visits to list and size what it deletes (the
/// client's `.omaddons/` and the trees below it, its state halves); past it the purge is
/// refused before anything moves, never planned or sized from part of the walk.
/// Retention keeps a client's snapshots and backups far below it.
pub const PURGE_MAX_ENTRIES: usize = 1_000_000;

/// Snapshot retention: keep everything younger than `min_age`; beyond that, keep the
/// last `keep_last` transactions per target that are younger than `window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TxRetention {
    pub min_age: Duration,
    pub keep_last: usize,
    pub window: Duration,
}

// ---------------------------------------------------------------------------------------
// The registry published by `capabilities.limits` (M4).

/// One named constant, for `capabilities.limits` and config overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LimitInfo {
    /// The constant's name, e.g. `DOWNLOAD_MAX_BYTES`.
    pub name: &'static str,
    pub value: LimitValue,
    pub unit: LimitUnit,
    /// For "(config)" constants, the override key `EffectiveLimits::apply` takes, which
    /// is also the `[limits]` key of `config.toml` except for `tx_retention` and
    /// `default_check_interval` (their notes give the `config.toml` keys); `None` for
    /// every other.
    pub config_key: Option<&'static str>,
    pub notes: &'static str,
}

/// What a constant's `value` measures, which also decides its JSON type: an integer for
/// `bytes`, `ms`, `count`, `ratio` and `chars`, a string for `text`, an array for `list`,
/// an object for `retention` (rule 4: a closed set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LimitUnit {
    Bytes,
    Ms,
    Count,
    Ratio,
    Chars,
    Text,
    List,
    Retention,
}

/// A constant's typed default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LimitValue {
    Count(u64),
    Bytes(u64),
    Millis(u64),
    Ratio(u64),
    Text(&'static str),
    List(&'static [&'static str]),
    Retention {
        min_age_ms: u64,
        keep_last: u64,
        window_ms: u64,
    },
}

/// Milliseconds of a constant duration. Every duration here is far below `u64::MAX` ms.
pub(crate) const fn ms(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

/// One registry entry; shared with `politeness::REGISTRY`.
pub(crate) const fn limit(
    name: &'static str,
    value: LimitValue,
    unit: LimitUnit,
    config_key: Option<&'static str>,
    notes: &'static str,
) -> LimitInfo {
    LimitInfo {
        name,
        value,
        unit,
        config_key,
        notes,
    }
}

/// Every safety limit, in table order.
pub const REGISTRY: &[LimitInfo] = &[
    limit(
        "ARCHIVE_MAX_ENTRIES",
        LimitValue::Count(ARCHIVE_MAX_ENTRIES),
        LimitUnit::Count,
        Some("archive_max_entries"),
        "Most entries an archive may hold",
    ),
    limit(
        "ARCHIVE_MAX_UNCOMPRESSED",
        LimitValue::Bytes(ARCHIVE_MAX_UNCOMPRESSED),
        LimitUnit::Bytes,
        Some("archive_max_uncompressed"),
        "Most bytes an archive may expand to",
    ),
    limit(
        "ARCHIVE_MAX_RATIO",
        LimitValue::Ratio(ARCHIVE_MAX_RATIO),
        LimitUnit::Ratio,
        Some("archive_max_ratio"),
        "Zip-bomb guard: largest N of an N:1 compression ratio",
    ),
    limit(
        "ARCHIVE_RATIO_FLOOR",
        LimitValue::Bytes(ARCHIVE_RATIO_FLOOR),
        LimitUnit::Bytes,
        None,
        "Expansion below which the compression ratio is not checked",
    ),
    limit(
        "DOWNLOAD_MAX_BYTES",
        LimitValue::Bytes(DOWNLOAD_MAX_BYTES),
        LimitUnit::Bytes,
        Some("download_max_bytes"),
        "Largest download accepted",
    ),
    limit(
        "LOCK_ACQUIRE_TIMEOUT",
        LimitValue::Millis(ms(LOCK_ACQUIRE_TIMEOUT)),
        LimitUnit::Ms,
        None,
        "Then locked (exit 5), or concurrency_lost in the doctor",
    ),
    limit(
        "TX_RETENTION",
        LimitValue::Retention {
            min_age_ms: ms(TX_RETENTION.min_age),
            keep_last: TX_RETENTION.keep_last as u64,
            window_ms: ms(TX_RETENTION.window),
        },
        LimitUnit::Retention,
        Some("tx_retention"),
        "Always at least 24 h; beyond that, 10 transactions within 7 days per target \
         (config.toml: limits.tx_retention_count, limits.tx_retention_days)",
    ),
    limit(
        "SNAPSHOT_MAX_BYTES",
        LimitValue::Bytes(SNAPSHOT_MAX_BYTES),
        LimitUnit::Bytes,
        Some("snapshot_max_bytes"),
        "Per target, beyond the 24 h protection window",
    ),
    limit(
        "BLOB_CACHE_MAX_BYTES",
        LimitValue::Bytes(BLOB_CACHE_MAX_BYTES),
        LimitUnit::Bytes,
        Some("blob_cache_max_bytes"),
        "LRU with pins",
    ),
    limit(
        "PLAN_TTL",
        LimitValue::Millis(ms(PLAN_TTL)),
        LimitUnit::Ms,
        None,
        "Stored plans",
    ),
    limit(
        "PLAN_LEASE_TIMEOUT",
        LimitValue::Millis(ms(PLAN_LEASE_TIMEOUT)),
        LimitUnit::Ms,
        None,
        "How long a second apply of one plan waits for its lease, then locked (exit 5)",
    ),
    limit(
        "PLAN_MAX_BYTES",
        LimitValue::Bytes(PLAN_MAX_BYTES),
        LimitUnit::Bytes,
        None,
        "Largest stored plan file; reads of one stop past it",
    ),
    limit(
        "PLANS_MAX_FILES",
        LimitValue::Count(PLANS_MAX_FILES as u64),
        LimitUnit::Count,
        None,
        "Hard cap on stored plans; at it, a dry run stores nothing (plan_not_stored)",
    ),
    limit(
        "PLANS_MAX_BYTES",
        LimitValue::Bytes(PLANS_MAX_BYTES),
        LimitUnit::Bytes,
        None,
        "Hard cap on the plan store's total size",
    ),
    limit(
        "PLANS_SWEEP_MAX",
        LimitValue::Count(PLANS_SWEEP_MAX as u64),
        LimitUnit::Count,
        None,
        "Expired plans one cleanup batch deletes at most",
    ),
    limit(
        "PLAN_EVIDENCE_LINES_MAX",
        LimitValue::Count(PLAN_EVIDENCE_LINES_MAX as u64),
        LimitUnit::Count,
        None,
        "History lines apply's already_applied check and plans show read backwards at most",
    ),
    limit(
        "ORPHAN_MIN_AGE",
        LimitValue::Millis(ms(ORPHAN_MIN_AGE)),
        LimitUnit::Ms,
        None,
        "Orphan stage dirs and in-flight locks retention sweeps",
    ),
    limit(
        "RETENTION_SWEEP_INTERVAL",
        LimitValue::Millis(ms(RETENTION_SWEEP_INTERVAL)),
        LimitUnit::Ms,
        None,
        "How often snapshot retention also prunes every other client discovery found",
    ),
    limit(
        "REASON_MAX_CHARS",
        LimitValue::Count(REASON_MAX_CHARS as u64),
        LimitUnit::Chars,
        None,
        "--reason, in Unicode scalar values",
    ),
    limit(
        "AUTH_TOKEN_MAX_BYTES",
        LimitValue::Bytes(AUTH_TOKEN_MAX_BYTES),
        LimitUnit::Bytes,
        None,
        "auth github set: the token's line on stdin; past it, refused",
    ),
    limit(
        "NET_LOG_MAX_BYTES",
        LimitValue::Bytes(NET_LOG_MAX_BYTES),
        LimitUnit::Bytes,
        None,
        "Rotate once, keep one previous",
    ),
    limit(
        "SAVEDVARIABLES_MAX_BYTES",
        LimitValue::Bytes(SAVEDVARIABLES_MAX_BYTES),
        LimitUnit::Bytes,
        None,
        "Lua parser input cap (errors)",
    ),
    limit(
        "TOC_LINE_MAX",
        LimitValue::Count(TOC_LINE_MAX as u64),
        LimitUnit::Chars,
        None,
        "Client rule: only the first 1024 characters of each TOC line are read",
    ),
    limit(
        "QUICK_DETECTOR_BUDGET",
        LimitValue::Millis(ms(QUICK_DETECTOR_BUDGET)),
        LimitUnit::Ms,
        None,
        "Doctor --quick / health, per detector",
    ),
    limit(
        "DOCTOR_DETECTOR_DEADLINE",
        LimitValue::Millis(ms(DOCTOR_DETECTOR_DEADLINE)),
        LimitUnit::Ms,
        None,
        "A default or deep doctor check's run-time deadline without --budget; past it, check-incomplete",
    ),
    limit(
        "RUN_ID_RETRY",
        LimitValue::Count(RUN_ID_RETRY as u64),
        LimitUnit::Count,
        None,
        "Fresh ULIDs drawn after a doctor run-directory name collision",
    ),
    limit(
        "DISCOVERY_FILE_MAX_BYTES",
        LimitValue::Bytes(DISCOVERY_FILE_MAX_BYTES),
        LimitUnit::Bytes,
        None,
        "Most bytes read from one client metadata file during discovery, or from one state or config file the doctor's view reads",
    ),
    limit(
        "CREDENTIAL_COMMAND_TIMEOUT",
        LimitValue::Millis(ms(CREDENTIAL_COMMAND_TIMEOUT)),
        LimitUnit::Ms,
        None,
        "A credential command is killed after this long",
    ),
    limit(
        "RELEASE_MEMO_MAX_ENTRIES",
        LimitValue::Count(RELEASE_MEMO_MAX_ENTRIES as u64),
        LimitUnit::Count,
        None,
        "Release memo entries and candidate facts; the oldest go first",
    ),
    limit(
        "MEMO_KNOWN_RELEASES",
        LimitValue::Count(MEMO_KNOWN_RELEASES as u64),
        LimitUnit::Count,
        None,
        "Other releases a memo entry remembers beside the newest",
    ),
    limit(
        "DEPENDENCY_SOURCE_MAX_AGE",
        LimitValue::Millis(ms(DEPENDENCY_SOURCE_MAX_AGE)),
        LimitUnit::Ms,
        None,
        "A WoWInterface project unchanged this long is not chosen on its own for a dependency",
    ),
    limit(
        "CHANGELOG_MAX_CHARS",
        LimitValue::Count(CHANGELOG_MAX_CHARS as u64),
        LimitUnit::Count,
        None,
        "Longest changelog entry shown, in characters; longer ones end with an ellipsis",
    ),
    limit(
        "CHANGELOG_MAX_ENTRIES",
        LimitValue::Count(CHANGELOG_MAX_ENTRIES as u64),
        LimitUnit::Count,
        None,
        "Most changelog entries one answer shows",
    ),
    limit(
        "API_RESPONSE_MAX_BYTES",
        LimitValue::Bytes(API_RESPONSE_MAX_BYTES),
        LimitUnit::Bytes,
        None,
        "Largest decoded API response: a decompression-bomb guard",
    ),
    limit(
        "WOWACE_TREE_MAX_FILES",
        LimitValue::Count(WOWACE_TREE_MAX_FILES as u64),
        LimitUnit::Count,
        None,
        "Most entries a WowAce listing or tag list names, and most files a WowAce tree holds",
    ),
    limit(
        "WOWACE_TREE_MAX_DIRS",
        LimitValue::Count(WOWACE_TREE_MAX_DIRS as u64),
        LimitUnit::Count,
        None,
        "Most directory listings one WowAce tree fetches",
    ),
    limit(
        "WOWACE_TREE_MAX_DEPTH",
        LimitValue::Count(WOWACE_TREE_MAX_DEPTH as u64),
        LimitUnit::Count,
        None,
        "Deepest a WowAce tree's directories nest below its root",
    ),
    limit(
        "WOWACE_TREE_MAX_REQUESTS",
        LimitValue::Count(WOWACE_TREE_MAX_REQUESTS as u64),
        LimitUnit::Count,
        None,
        "Most requests one WowAce tree costs, retries included",
    ),
    limit(
        "BACKUP_IMPORT_MAX_ENTRIES",
        LimitValue::Count(BACKUP_IMPORT_MAX_ENTRIES),
        LimitUnit::Count,
        None,
        "Most entries an imported backup archive holds",
    ),
    limit(
        "BACKUP_IMPORT_MAX_BYTES",
        LimitValue::Bytes(BACKUP_IMPORT_MAX_BYTES),
        LimitUnit::Bytes,
        None,
        "Most bytes an imported backup archive expands to",
    ),
    limit(
        "BACKUP_IMPORT_MAX_PATH",
        LimitValue::Bytes(BACKUP_IMPORT_MAX_PATH),
        LimitUnit::Bytes,
        None,
        "Longest path of one entry of an imported backup archive",
    ),
    limit(
        "BACKUP_IMPORT_MAX_META_BYTES",
        LimitValue::Bytes(BACKUP_IMPORT_MAX_META_BYTES),
        LimitUnit::Bytes,
        None,
        "Largest PAX or long-name record of one entry of an imported backup archive",
    ),
    limit(
        "BACKUP_IMPORT_MAX_MANIFEST_BYTES",
        LimitValue::Bytes(BACKUP_IMPORT_MAX_MANIFEST_BYTES),
        LimitUnit::Bytes,
        None,
        "Largest manifest.json of an imported backup archive",
    ),
    limit(
        "BACKUP_IMPORT_MAX_WINDOW_LOG",
        LimitValue::Count(BACKUP_IMPORT_MAX_WINDOW_LOG as u64),
        LimitUnit::Count,
        None,
        "The zstd decoder window an import allows, as a power of two (24: 16 MiB)",
    ),
    limit(
        "DEPENDENCY_MAX_DEPTH",
        LimitValue::Count(DEPENDENCY_MAX_DEPTH as u64),
        LimitUnit::Count,
        None,
        "Dependency rounds per install and target; a longer chain stops with a warning",
    ),
    limit(
        "DEPENDENCY_MAX_ADDONS",
        LimitValue::Count(DEPENDENCY_MAX_ADDONS as u64),
        LimitUnit::Count,
        None,
        "Dependencies one transaction installs per target",
    ),
    limit(
        "RACY_SLACK_NS",
        LimitValue::Millis((RACY_SLACK_NS / 1_000_000) as u64),
        LimitUnit::Ms,
        None,
        "Covers the kernel's coarse file-timestamp clock: index entries are stamped this long after the newest staged mtime",
    ),
    limit(
        "ADOPT_MAX_ALTERNATIVES",
        LimitValue::Count(ADOPT_MAX_ALTERNATIVES as u64),
        LimitUnit::Count,
        None,
        "Other refs an adoption group lists beside its own",
    ),
    limit(
        "DRIFT_LIST_MAX",
        LimitValue::Count(DRIFT_LIST_MAX as u64),
        LimitUnit::Count,
        None,
        "Paths a drift check lists per category per folder (the count stays exact)",
    ),
    limit(
        "RACY_SLACK_COARSE_NS",
        LimitValue::Millis((RACY_SLACK_COARSE_NS / 1_000_000) as u64),
        LimitUnit::Ms,
        None,
        "The racy slack on filesystems with whole-second mtimes: files this close to a folder's stamp are re-hashed",
    ),
    limit(
        "SEARCH_MAX_RESULTS",
        LimitValue::Count(SEARCH_MAX_RESULTS as u64),
        LimitUnit::Count,
        None,
        "Results one search lists at most (--limit; 20 by default)",
    ),
    limit(
        "SEARCH_SUMMARY_MAX_CHARS",
        LimitValue::Count(SEARCH_SUMMARY_MAX_CHARS as u64),
        LimitUnit::Count,
        None,
        "Characters a search result's summary keeps",
    ),
    limit(
        "SOURCE_TEXT_MAX_CHARS",
        LimitValue::Count(SOURCE_TEXT_MAX_CHARS as u64),
        LimitUnit::Count,
        None,
        "Characters kept from a source's name, author or version",
    ),
    limit(
        "REDACTOR_MAX_NAMES",
        LimitValue::Count(REDACTOR_MAX_NAMES as u64),
        LimitUnit::Count,
        None,
        "Account, realm and character names the privacy redactor learns per client; others map to … ?",
    ),
    limit(
        "BACKUP_TREE_MAX_ENTRIES",
        LimitValue::Count(BACKUP_TREE_MAX_ENTRIES as u64),
        LimitUnit::Count,
        None,
        "Entries walked in one WTF/ a backup copies",
    ),
    limit(
        "BACKUP_TREE_MAX_DEPTH",
        LimitValue::Count(BACKUP_TREE_MAX_DEPTH as u64),
        LimitUnit::Count,
        None,
        "Directory depth walked in one WTF/ a backup copies",
    ),
    limit(
        "BACKUP_CAPTURE_ATTEMPTS",
        LimitValue::Count(BACKUP_CAPTURE_ATTEMPTS as u64),
        LimitUnit::Count,
        None,
        "Captures tried before one is published best_effort",
    ),
    limit(
        "BACKUP_MIN_FREE",
        LimitValue::Bytes(BACKUP_MIN_FREE),
        LimitUnit::Bytes,
        Some("backup_min_free"),
        "Free space below which automatic backups reclaim, then skip, and a manual one warns \
         (config.toml: limits.backup_min_free_mb, in MiB; 0 turns the check off)",
    ),
    limit(
        "BACKUP_COPY_MAX",
        LimitValue::Bytes(BACKUP_COPY_MAX),
        LimitUnit::Bytes,
        Some("backup_copy_max"),
        "Apparent bytes of automatic byte-copy backups per target (auto_backup daily-copy; \
         config.toml: limits.backup_copy_max_mb, in MiB; 0 turns the budget off)",
    ),
    limit(
        "BACKUP_STALE_AFTER",
        LimitValue::Millis(ms(BACKUP_STALE_AFTER)),
        LimitUnit::Ms,
        None,
        "A client with no qualifying backup captured this recently is offered backup create",
    ),
    limit(
        "BACKUP_AUTO_KEEP_DAYS",
        LimitValue::Count(BACKUP_AUTO_KEEP_DAYS as u64),
        LimitUnit::Count,
        Some("backup_auto_keep_days"),
        "Days of automatic backups kept per target \
         (config.toml: limits.backup_auto_keep_days, 1 to 100)",
    ),
    limit(
        "BACKUP_AUTO_KEEP_DAYS_MAX",
        LimitValue::Count(BACKUP_AUTO_KEEP_DAYS_MAX as u64),
        LimitUnit::Count,
        None,
        "The most limits.backup_auto_keep_days accepts",
    ),
    limit(
        "BACKUP_MIN_AGE",
        LimitValue::Millis(ms(BACKUP_MIN_AGE)),
        LimitUnit::Ms,
        None,
        "No backup younger than this is pruned automatically, nor by --keep or --older-than",
    ),
    limit(
        "HISTORY_TAIL_RUNS",
        LimitValue::Count(HISTORY_TAIL_RUNS as u64),
        LimitUnit::Count,
        None,
        "Runs with an undoable entry the history projection keeps whole",
    ),
    limit(
        "HISTORY_TAIL_MAX_BYTES",
        LimitValue::Bytes(HISTORY_TAIL_MAX_BYTES),
        LimitUnit::Bytes,
        None,
        "Size cap of the history projection",
    ),
    limit(
        "LUA_MAX_DEPTH",
        LimitValue::Count(LUA_MAX_DEPTH as u64),
        LimitUnit::Count,
        None,
        "Table nesting the SavedVariables parser follows, building or skipping",
    ),
    limit(
        "LUA_MAX_NODES",
        LimitValue::Count(LUA_MAX_NODES),
        LimitUnit::Count,
        None,
        "Values the SavedVariables parser builds per parse",
    ),
    limit(
        "LUA_MAX_ISSUES",
        LimitValue::Count(LUA_MAX_ISSUES as u64),
        LimitUnit::Count,
        None,
        "Issues the SavedVariables parser records per parse; past it only a count",
    ),
    limit(
        "BUGGRABBER_MAX_ENTRIES",
        LimitValue::Count(BUGGRABBER_MAX_ENTRIES as u64),
        LimitUnit::Count,
        None,
        "Entries decoded per BugGrabber database, the newest kept (BugGrabber itself keeps 500, r152 50)",
    ),
    limit(
        "ERRORS_MAX_ACCOUNTS",
        LimitValue::Count(ERRORS_MAX_ACCOUNTS as u64),
        LimitUnit::Count,
        None,
        "Account dirs examined per target",
    ),
    limit(
        "ERRORS_MAX_FILES",
        LimitValue::Count(ERRORS_MAX_FILES as u64),
        LimitUnit::Count,
        None,
        "Candidate BugGrabber files parsed per target",
    ),
    limit(
        "ERRORS_MESSAGE_MAX_CHARS",
        LimitValue::Count(ERRORS_MESSAGE_MAX_CHARS as u64),
        LimitUnit::Chars,
        None,
        "First line shown with --raw-details",
    ),
    limit(
        "ERRORS_STACK_LINES",
        LimitValue::Count(ERRORS_STACK_LINES as u64),
        LimitUnit::Count,
        None,
        "Frames shown by default",
    ),
    limit(
        "ERRORS_SHOWN_PER_ADDON",
        LimitValue::Count(ERRORS_SHOWN_PER_ADDON as u64),
        LimitUnit::Count,
        None,
        "Errors one addon lists in errors' output, new first, then the most frequent; entries counts them all",
    ),
    limit(
        "ERRORS_STACK_LINES_VERBOSE",
        LimitValue::Count(ERRORS_STACK_LINES_VERBOSE as u64),
        LimitUnit::Count,
        None,
        "Frames with -v or --raw-details",
    ),
    limit(
        "ERRORS_LOCALS_MAX_CHARS",
        LimitValue::Count(ERRORS_LOCALS_MAX_CHARS as u64),
        LimitUnit::Chars,
        None,
        "Locals with --raw-details",
    ),
    limit(
        "ERRORS_CHUNK_MAX_CHARS",
        LimitValue::Count(ERRORS_CHUNK_MAX_CHARS as u64),
        LimitUnit::Chars,
        None,
        "[string \"…\"] contents shown with --raw-details",
    ),
    limit(
        "ERRORS_MAX_TOTAL_BYTES",
        LimitValue::Bytes(ERRORS_MAX_TOTAL_BYTES),
        LimitUnit::Bytes,
        None,
        "BugGrabber bytes read per errors run, every target together",
    ),
    limit(
        "ERRORS_NEW_MAX_AGE",
        LimitValue::Millis(ms(ERRORS_NEW_MAX_AGE)),
        LimitUnit::Ms,
        None,
        "A BugGrabber file older than this has no new errors",
    ),
    limit(
        "WIDGET_READBACK_MAX_BYTES",
        LimitValue::Bytes(WIDGET_READBACK_MAX_BYTES),
        LimitUnit::Bytes,
        None,
        "An installed widget's manifest.json or Build.js read back; a larger one is not omaddons'",
    ),
    limit(
        "OMARCHY_COMMAND_TIMEOUT",
        LimitValue::Millis(ms(OMARCHY_COMMAND_TIMEOUT)),
        LimitUnit::Ms,
        None,
        "An Omarchy tool (plugin validator, shell IPC, bar placement) is killed after this long",
    ),
    limit(
        "OMARCHY_SHELL_JSON_MAX_BYTES",
        LimitValue::Bytes(OMARCHY_SHELL_JSON_MAX_BYTES),
        LimitUnit::Bytes,
        None,
        "Omarchy's shell.json read for the widget's place in the bar; a larger one counts as not placed",
    ),
    limit(
        "OMARCHY_PING_TIMEOUT",
        LimitValue::Millis(ms(OMARCHY_PING_TIMEOUT)),
        LimitUnit::Ms,
        None,
        "omarchy status's shell ping; a shell that has not answered ok by then is not running",
    ),
    limit(
        "OMARCHY_KILL_GRACE",
        LimitValue::Millis(ms(OMARCHY_KILL_GRACE)),
        LimitUnit::Ms,
        None,
        "a timed-out Omarchy tool's process group, between SIGTERM and SIGKILL",
    ),
    limit(
        "DOCTOR_SCAN_MAX_ENTRIES",
        LimitValue::Count(DOCTOR_SCAN_MAX_ENTRIES),
        LimitUnit::Count,
        None,
        "Most entries one doctor walk or listing visits per run",
    ),
    limit(
        "SKILL_DIRS_MAX",
        LimitValue::Count(SKILL_DIRS_MAX as u64),
        LimitUnit::Count,
        None,
        "Skill directories a scope table, history_tail.skill_dirs[] and the skill-drift check admit at most",
    ),
    limit(
        "DOCTOR_FIX_MAX_ACTIONS",
        LimitValue::Count(DOCTOR_FIX_MAX_ACTIONS as u64),
        LimitUnit::Count,
        None,
        "Primitives one doctor repair may plan; past it the run is refused (too_many_actions)",
    ),
    limit(
        "LOCK_HELD_LONG",
        LimitValue::Millis(ms(LOCK_HELD_LONG)),
        LimitUnit::Ms,
        None,
        "A global-lock holder older than this is reported (lock-held-long)",
    ),
    limit(
        "DOCTOR_RUNS_SOFT_MAX",
        LimitValue::Count(DOCTOR_RUNS_SOFT_MAX),
        LimitUnit::Count,
        None,
        "fix and undo run directories past which doctor-runs-accumulated reports (diagnose runs do not count)",
    ),
    limit(
        "DOCTOR_GC_SUGGESTED_AGE",
        LimitValue::Millis(ms(DOCTOR_GC_SUGGESTED_AGE)),
        LimitUnit::Ms,
        None,
        "the --before age remediations suggest (doctor gc --before 30d --dry-run)",
    ),
    limit(
        "DOCTOR_GC_MAX_DELETIONS",
        LimitValue::Count(DOCTOR_GC_MAX_DELETIONS as u64),
        LimitUnit::Count,
        None,
        "victims one doctor gc deletes at most; the report says how many remain",
    ),
    limit(
        "DOCTOR_QUARANTINE_SOFT_MAX",
        LimitValue::Bytes(DOCTOR_QUARANTINE_SOFT_MAX),
        LimitUnit::Bytes,
        None,
        "quarantine bytes past which doctor-runs-accumulated reports",
    ),
    limit(
        "PURGE_MAX_ENTRIES",
        LimitValue::Count(PURGE_MAX_ENTRIES as u64),
        LimitUnit::Count,
        None,
        "entries targets rm --purge visits to list and size what it deletes; past it the purge is refused",
    ),
];

// ---------------------------------------------------------------------------------------
// Effective values after config overrides.

/// The config-overridable values, after `[limits]` overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EffectiveLimits {
    pub default_check_interval: Duration,
    pub archive_max_entries: u64,
    pub archive_max_uncompressed: u64,
    pub archive_max_ratio: u64,
    pub download_max_bytes: u64,
    pub tx_retention: TxRetention,
    pub snapshot_max_bytes: u64,
    pub blob_cache_max_bytes: u64,
    /// Bytes; 0 turns the free-space check off.
    pub backup_min_free: u64,
    /// Bytes; 0 turns the byte-copy budget off.
    pub backup_copy_max: u64,
    pub backup_auto_keep_days: usize,
}

impl Default for EffectiveLimits {
    fn default() -> Self {
        Self {
            default_check_interval: DEFAULT_CHECK_INTERVAL,
            archive_max_entries: ARCHIVE_MAX_ENTRIES,
            archive_max_uncompressed: ARCHIVE_MAX_UNCOMPRESSED,
            archive_max_ratio: ARCHIVE_MAX_RATIO,
            download_max_bytes: DOWNLOAD_MAX_BYTES,
            tx_retention: TX_RETENTION,
            snapshot_max_bytes: SNAPSHOT_MAX_BYTES,
            blob_cache_max_bytes: BLOB_CACHE_MAX_BYTES,
            backup_min_free: BACKUP_MIN_FREE,
            backup_copy_max: BACKUP_COPY_MAX,
            backup_auto_keep_days: BACKUP_AUTO_KEEP_DAYS,
        }
    }
}

/// Why a `[limits]` entry was not applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LimitOverrideError {
    #[error("limits.{key} is not a known limit")]
    Unknown { key: String },
    #[error("limits.{key} is not overridable; only the (config) limits are")]
    NotOverridable { key: String },
    #[error("limits.{key} must be {expected}")]
    WrongType { key: String, expected: &'static str },
    #[error("limits.{key} must be greater than zero")]
    NotPositive { key: String },
}

impl EffectiveLimits {
    /// Applies one `[limits]` entry. Values must be positive, except `backup_min_free` and
    /// `backup_copy_max`, whose 0 turns their check off. Byte, count and ratio limits take
    /// integers;
    /// `default_check_interval` takes a duration string such as `"6h"` or `"PT6H"`;
    /// `tx_retention` takes `{ keep_last = N, window_days = N }` (either key may be left
    /// out). Its 24 h minimum age is not overridable.
    pub fn apply(&mut self, key: &str, value: &toml::Value) -> Result<(), LimitOverrideError> {
        match key {
            "default_check_interval" => {
                self.default_check_interval = positive_duration(key, value)?;
            }
            "archive_max_entries" => self.archive_max_entries = positive_integer(key, value)?,
            "archive_max_uncompressed" => {
                self.archive_max_uncompressed = positive_integer(key, value)?;
            }
            "archive_max_ratio" => self.archive_max_ratio = positive_integer(key, value)?,
            "download_max_bytes" => self.download_max_bytes = positive_integer(key, value)?,
            "snapshot_max_bytes" => self.snapshot_max_bytes = positive_integer(key, value)?,
            "blob_cache_max_bytes" => self.blob_cache_max_bytes = positive_integer(key, value)?,
            "tx_retention" => self.tx_retention = retention(key, value, self.tx_retention)?,
            "backup_min_free" => self.backup_min_free = non_negative_integer(key, value)?,
            "backup_copy_max" => self.backup_copy_max = non_negative_integer(key, value)?,
            "backup_auto_keep_days" => {
                let days = positive_integer(key, value)?;
                self.backup_auto_keep_days = usize::try_from(days)
                    .ok()
                    .filter(|days| *days <= BACKUP_AUTO_KEEP_DAYS_MAX)
                    .ok_or_else(|| LimitOverrideError::WrongType {
                        key: key.to_owned(),
                        expected: "an integer from 1 to 100",
                    })?;
            }
            _ => {
                let known = REGISTRY
                    .iter()
                    .chain(crate::politeness::REGISTRY)
                    .any(|info| info.name.eq_ignore_ascii_case(key));
                return Err(if known {
                    LimitOverrideError::NotOverridable {
                        key: key.to_owned(),
                    }
                } else {
                    LimitOverrideError::Unknown {
                        key: key.to_owned(),
                    }
                });
            }
        }
        Ok(())
    }
}

fn positive_integer(key: &str, value: &toml::Value) -> Result<u64, LimitOverrideError> {
    let n = non_negative_integer(key, value)?;
    if n == 0 {
        return Err(LimitOverrideError::NotPositive {
            key: key.to_owned(),
        });
    }
    Ok(n)
}

/// An integer of at least 0; a negative one is [`LimitOverrideError::NotPositive`].
fn non_negative_integer(key: &str, value: &toml::Value) -> Result<u64, LimitOverrideError> {
    let n = value
        .as_integer()
        .ok_or_else(|| LimitOverrideError::WrongType {
            key: key.to_owned(),
            expected: "an integer",
        })?;
    u64::try_from(n).map_err(|_| LimitOverrideError::NotPositive {
        key: key.to_owned(),
    })
}

fn positive_duration(key: &str, value: &toml::Value) -> Result<Duration, LimitOverrideError> {
    let wrong_type = || LimitOverrideError::WrongType {
        key: key.to_owned(),
        expected: "a duration string such as \"6h\"",
    };
    let parsed: jiff::SignedDuration = value
        .as_str()
        .ok_or_else(wrong_type)?
        .parse()
        .map_err(|_| wrong_type())?;
    Duration::try_from(parsed)
        .ok()
        .filter(|d| !d.is_zero())
        .ok_or_else(|| LimitOverrideError::NotPositive {
            key: key.to_owned(),
        })
}

fn retention(
    key: &str,
    value: &toml::Value,
    current: TxRetention,
) -> Result<TxRetention, LimitOverrideError> {
    let wrong_type = || LimitOverrideError::WrongType {
        key: key.to_owned(),
        expected: "a table with keep_last and/or window_days",
    };
    let table = value.as_table().ok_or_else(wrong_type)?;
    let mut retention = current;
    for (name, value) in table {
        let field = format!("{key}.{name}");
        let too_large = || LimitOverrideError::WrongType {
            key: field.clone(),
            expected: "a smaller integer",
        };
        match name.as_str() {
            "keep_last" => {
                retention.keep_last =
                    usize::try_from(positive_integer(&field, value)?).map_err(|_| too_large())?;
            }
            "window_days" => {
                let days = positive_integer(&field, value)?;
                retention.window = DAY
                    .checked_mul(u32::try_from(days).map_err(|_| too_large())?)
                    .ok_or_else(too_large)?;
            }
            _ => return Err(wrong_type()),
        }
    }
    Ok(retention)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::politeness;

    /// `limits[].unit` is a closed set (rule 4) of the eight units the docs name, and
    /// every row's unit serializes to one of them.
    #[test]
    fn limit_units_are_a_closed_enum() {
        let schema = serde_json::to_value(schemars::schema_for!(LimitUnit)).unwrap();
        let units = serde_json::json!([
            "bytes",
            "ms",
            "count",
            "ratio",
            "chars",
            "text",
            "list",
            "retention"
        ]);
        assert_eq!(schema["enum"], units, "{schema}");
        for row in REGISTRY.iter().chain(politeness::REGISTRY) {
            let wire = serde_json::to_value(row.unit).unwrap();
            assert!(
                units.as_array().unwrap().contains(&wire),
                "{}: {wire}",
                row.name
            );
        }
    }

    #[rustfmt::skip]
    const POLITENESS_NAMES: &[&str] = &[
        "USER_AGENT", "MAX_CONCURRENT_REQUESTS", "MAX_CONCURRENT_PER_HOST", "CONNECT_TIMEOUT",
        "API_READ_TIMEOUT", "DOWNLOAD_TIMEOUT", "MAX_RETRIES", "BACKOFF_BASE", "BACKOFF_MAX",
        "RETRY_AFTER_CAP", "SOURCE_RECHECK_MIN_INTERVAL", "DEFAULT_CHECK_INTERVAL",
        "WOWI_FILELIST_MIN_AGE", "GITHUB_REST_RESERVE", "CURSEFORGE_BATCH_SIZE",
        "WOWI_DETAILS_BATCH_SIZE", "DISCOVERY_MAX_DEPTH", "DISCOVERY_SKIP_DIRS",
        "DISCOVERY_MAX_DIRS", "MAX_REDIRECTS", "GITHUB_SEARCH_PER_PAGE", "LOOK_BACK_MAX_RELEASES",
        "LOOK_BACK_SAME_TRACK",
    ];

    #[rustfmt::skip]
    const LIMIT_NAMES: &[&str] = &[
        "ARCHIVE_MAX_ENTRIES", "ARCHIVE_MAX_UNCOMPRESSED", "ARCHIVE_MAX_RATIO",
        "ARCHIVE_RATIO_FLOOR", "DOWNLOAD_MAX_BYTES", "LOCK_ACQUIRE_TIMEOUT", "TX_RETENTION", "SNAPSHOT_MAX_BYTES",
        "BLOB_CACHE_MAX_BYTES", "PLAN_TTL", "PLAN_LEASE_TIMEOUT", "PLAN_MAX_BYTES", "PLANS_MAX_FILES", "PLANS_MAX_BYTES",
        "PLANS_SWEEP_MAX", "PLAN_EVIDENCE_LINES_MAX", "ORPHAN_MIN_AGE", "RETENTION_SWEEP_INTERVAL",
        "REASON_MAX_CHARS",
        "AUTH_TOKEN_MAX_BYTES", "NET_LOG_MAX_BYTES",
        "SAVEDVARIABLES_MAX_BYTES", "TOC_LINE_MAX", "QUICK_DETECTOR_BUDGET",
        "DOCTOR_DETECTOR_DEADLINE", "RUN_ID_RETRY",
        "DISCOVERY_FILE_MAX_BYTES", "CREDENTIAL_COMMAND_TIMEOUT", "RELEASE_MEMO_MAX_ENTRIES",
        "MEMO_KNOWN_RELEASES", "DEPENDENCY_SOURCE_MAX_AGE", "CHANGELOG_MAX_CHARS",
        "CHANGELOG_MAX_ENTRIES", "API_RESPONSE_MAX_BYTES",
        "WOWACE_TREE_MAX_FILES",
        "WOWACE_TREE_MAX_DIRS",
        "WOWACE_TREE_MAX_DEPTH",
        "WOWACE_TREE_MAX_REQUESTS",
        "BACKUP_IMPORT_MAX_ENTRIES",
        "BACKUP_IMPORT_MAX_BYTES",
        "BACKUP_IMPORT_MAX_PATH",
        "BACKUP_IMPORT_MAX_META_BYTES",
        "BACKUP_IMPORT_MAX_MANIFEST_BYTES",
        "BACKUP_IMPORT_MAX_WINDOW_LOG",
        "DEPENDENCY_MAX_DEPTH",
        "DEPENDENCY_MAX_ADDONS",
        "RACY_SLACK_NS",
        "ADOPT_MAX_ALTERNATIVES",
        "DRIFT_LIST_MAX",
        "RACY_SLACK_COARSE_NS",
        "SEARCH_MAX_RESULTS",
        "SEARCH_SUMMARY_MAX_CHARS",
        "SOURCE_TEXT_MAX_CHARS",
        "REDACTOR_MAX_NAMES",
        "BACKUP_TREE_MAX_ENTRIES",
        "BACKUP_TREE_MAX_DEPTH",
        "BACKUP_CAPTURE_ATTEMPTS",
        "BACKUP_MIN_FREE",
        "BACKUP_COPY_MAX",
        "BACKUP_STALE_AFTER",
        "BACKUP_AUTO_KEEP_DAYS",
        "BACKUP_AUTO_KEEP_DAYS_MAX",
        "BACKUP_MIN_AGE",
        "HISTORY_TAIL_RUNS",
        "HISTORY_TAIL_MAX_BYTES",
        "LUA_MAX_DEPTH",
        "LUA_MAX_NODES",
        "LUA_MAX_ISSUES",
        "BUGGRABBER_MAX_ENTRIES",
        "ERRORS_MAX_ACCOUNTS",
        "ERRORS_MAX_FILES",
        "ERRORS_MESSAGE_MAX_CHARS",
        "ERRORS_STACK_LINES",
        "ERRORS_SHOWN_PER_ADDON",
        "ERRORS_STACK_LINES_VERBOSE",
        "ERRORS_LOCALS_MAX_CHARS",
        "ERRORS_CHUNK_MAX_CHARS",
        "ERRORS_MAX_TOTAL_BYTES",
        "ERRORS_NEW_MAX_AGE",
        "WIDGET_READBACK_MAX_BYTES",
        "OMARCHY_COMMAND_TIMEOUT",
        "OMARCHY_SHELL_JSON_MAX_BYTES",
        "OMARCHY_PING_TIMEOUT",
        "OMARCHY_KILL_GRACE",
        "DOCTOR_SCAN_MAX_ENTRIES",
        "SKILL_DIRS_MAX",
        "DOCTOR_FIX_MAX_ACTIONS",
        "LOCK_HELD_LONG",
        "DOCTOR_RUNS_SOFT_MAX",
        "DOCTOR_GC_SUGGESTED_AGE",
        "DOCTOR_GC_MAX_DELETIONS",
        "DOCTOR_QUARANTINE_SOFT_MAX",
        "PURGE_MAX_ENTRIES",
    ];

    const CONFIG_NAMES: &[&str] = &[
        "DEFAULT_CHECK_INTERVAL",
        "ARCHIVE_MAX_ENTRIES",
        "ARCHIVE_MAX_UNCOMPRESSED",
        "ARCHIVE_MAX_RATIO",
        "DOWNLOAD_MAX_BYTES",
        "TX_RETENTION",
        "SNAPSHOT_MAX_BYTES",
        "BLOB_CACHE_MAX_BYTES",
        "BACKUP_MIN_FREE",
        "BACKUP_COPY_MAX",
        "BACKUP_AUTO_KEEP_DAYS",
    ];

    fn names(registry: &[LimitInfo]) -> Vec<&'static str> {
        registry.iter().map(|info| info.name).collect()
    }

    #[test]
    fn the_suggested_age_is_thirty_days() {
        assert_eq!(DOCTOR_GC_SUGGESTED_AGE.as_secs(), 30 * 86_400);
    }

    #[test]
    fn registries_list_every_constant_once() {
        assert_eq!(names(politeness::REGISTRY), POLITENESS_NAMES);
        assert_eq!(names(REGISTRY), LIMIT_NAMES);
        let mut all: Vec<&str> = POLITENESS_NAMES
            .iter()
            .chain(LIMIT_NAMES)
            .copied()
            .collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), POLITENESS_NAMES.len() + LIMIT_NAMES.len());
    }

    #[test]
    fn exactly_the_config_constants_have_config_keys() {
        for info in politeness::REGISTRY.iter().chain(REGISTRY) {
            let expected = CONFIG_NAMES.contains(&info.name);
            assert_eq!(info.config_key.is_some(), expected, "{}", info.name);
            if let Some(key) = info.config_key {
                assert_eq!(key, info.name.to_ascii_lowercase());
                // Every advertised key is accepted by EffectiveLimits::apply.
                let mut limits = EffectiveLimits::default();
                let err = limits.apply(key, &toml::Value::Boolean(true)).unwrap_err();
                assert!(
                    matches!(err, LimitOverrideError::WrongType { .. }),
                    "{key}: {err}"
                );
            }
            assert!(!info.notes.is_empty(), "{}", info.name);
        }
    }

    #[test]
    fn spot_check_values() {
        assert_eq!(ARCHIVE_MAX_UNCOMPRESSED, 1 << 30);
        assert_eq!(DOWNLOAD_MAX_BYTES, 512 << 20);
        assert_eq!(NET_LOG_MAX_BYTES, 10 << 20);
        assert_eq!(SAVEDVARIABLES_MAX_BYTES, 64 << 20);
        assert_eq!(SNAPSHOT_MAX_BYTES, 2 << 30);
        assert_eq!(BLOB_CACHE_MAX_BYTES, 1 << 30);
        assert_eq!(TX_RETENTION.min_age, Duration::from_secs(86_400));
        assert_eq!(TX_RETENTION.keep_last, 10);
        assert_eq!(TX_RETENTION.window, Duration::from_secs(7 * 86_400));
        assert_eq!(PLAN_TTL, Duration::from_secs(86_400));
        assert_eq!(QUICK_DETECTOR_BUDGET.as_millis(), 5);
        assert_eq!(TOC_LINE_MAX, 1024);
    }

    #[test]
    fn effective_limits_apply_one_value() {
        let mut limits = EffectiveLimits::default();
        limits
            .apply("download_max_bytes", &toml::Value::Integer(1000))
            .unwrap();
        assert_eq!(
            limits,
            EffectiveLimits {
                download_max_bytes: 1000,
                ..EffectiveLimits::default()
            }
        );

        limits
            .apply("default_check_interval", &toml::Value::String("90m".into()))
            .unwrap();
        assert_eq!(limits.default_check_interval, Duration::from_secs(90 * 60));

        let retention: toml::Value = toml::from_str("keep_last = 20\nwindow_days = 14").unwrap();
        limits.apply("tx_retention", &retention).unwrap();
        assert_eq!(
            limits.tx_retention,
            TxRetention {
                min_age: DAY,
                keep_last: 20,
                window: 14 * DAY,
            }
        );

        // 0 turns the free-space check off; a negative value is refused.
        limits
            .apply("backup_min_free", &toml::Value::Integer(0))
            .unwrap();
        assert_eq!(limits.backup_min_free, 0);
        assert_eq!(
            limits.apply("backup_min_free", &toml::Value::Integer(-1)),
            Err(LimitOverrideError::NotPositive {
                key: "backup_min_free".into()
            })
        );
    }

    #[test]
    fn effective_limits_reject_bad_overrides() {
        let int = |n: i64| toml::Value::Integer(n);
        let cases: Vec<(&str, toml::Value, LimitOverrideError)> = vec![
            (
                "lock_acquire_timeout",
                int(10),
                LimitOverrideError::NotOverridable {
                    key: "lock_acquire_timeout".into(),
                },
            ),
            (
                "max_retries",
                int(10),
                LimitOverrideError::NotOverridable {
                    key: "max_retries".into(),
                },
            ),
            (
                "no_such_limit",
                int(10),
                LimitOverrideError::Unknown {
                    key: "no_such_limit".into(),
                },
            ),
            (
                "archive_max_ratio",
                int(0),
                LimitOverrideError::NotPositive {
                    key: "archive_max_ratio".into(),
                },
            ),
            (
                "archive_max_entries",
                int(-5),
                LimitOverrideError::NotPositive {
                    key: "archive_max_entries".into(),
                },
            ),
            (
                "download_max_bytes",
                toml::Value::String("1GB".into()),
                LimitOverrideError::WrongType {
                    key: "download_max_bytes".into(),
                    expected: "an integer",
                },
            ),
            (
                "default_check_interval",
                toml::Value::String("0s".into()),
                LimitOverrideError::NotPositive {
                    key: "default_check_interval".into(),
                },
            ),
            (
                "default_check_interval",
                toml::Value::String("-1h".into()),
                LimitOverrideError::NotPositive {
                    key: "default_check_interval".into(),
                },
            ),
        ];
        for (key, value, expected) in cases {
            let mut limits = EffectiveLimits::default();
            assert_eq!(limits.apply(key, &value), Err(expected), "{key} = {value}");
            assert_eq!(limits, EffectiveLimits::default(), "{key} changed a value");
        }

        let mut limits = EffectiveLimits::default();
        for bad in ["min_age = 1", "keep_last = 0", "window_days = \"7\""] {
            let value: toml::Value = toml::from_str(bad).unwrap();
            assert!(limits.apply("tx_retention", &value).is_err(), "{bad}");
        }
        assert!(limits.apply("default_check_interval", &int(6)).is_err());
    }

    #[test]
    fn registries_serialize_deterministically() {
        let first = serde_json::to_string(&REGISTRY).unwrap();
        assert_eq!(first, serde_json::to_string(&REGISTRY).unwrap());
        assert!(
            first.contains(r#""config_key":"download_max_bytes""#),
            "{first}"
        );
        // The crate version is masked so a version bump does not churn the snapshot.
        let politeness: Vec<LimitInfo> = politeness::REGISTRY
            .iter()
            .map(|info| match info.value {
                LimitValue::Text(_) if info.name == "USER_AGENT" => LimitInfo {
                    value: LimitValue::Text("omaddons/<version>"),
                    ..*info
                },
                _ => *info,
            })
            .collect();
        insta::assert_json_snapshot!("limit_registries", (politeness, REGISTRY));
    }
}
