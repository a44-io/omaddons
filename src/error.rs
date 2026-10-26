//! The error and warning code registries and the `Error` type. Each code carries its
//! exit code and whether it is retryable.
//!
//! Every failure is a typed [`Error`] with a stable snake_case code, a message, a fix
//! that is never empty, and the argv that runs the fix (vision §5.9, D17, D18, D31).
//! Programs execute argv only; display strings are shell-quoted for humans and are
//! never parsed back. The process exit status is only ever derived from an [`Exit`].

use std::borrow::Cow;
use std::fmt;
use std::path::Path;
use std::process::ExitCode;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------------------
// The general exit dictionary.

/// The general exit dictionary, used by every command except the `doctor` tree.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Exit {
    Ok,
    Failed,
    Usage,
    NotFound,
    Refused,
    Locked,
    CheckFailed,
}

impl Exit {
    pub const ALL: &[Exit] = &[
        Self::Ok,
        Self::Failed,
        Self::Usage,
        Self::NotFound,
        Self::Refused,
        Self::Locked,
        Self::CheckFailed,
    ];

    /// The process exit status.
    pub const fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Failed => 1,
            Self::Usage => 2,
            Self::NotFound => 3,
            Self::Refused => 4,
            Self::Locked => 5,
            Self::CheckFailed => 6,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Usage => "usage",
            Self::NotFound => "not_found",
            Self::Refused => "refused",
            Self::Locked => "locked",
            Self::CheckFailed => "check_failed",
        }
    }

    /// Whether retrying can help: `None` for success, and for `failed`, where
    /// `error.retryable` decides per error.
    pub const fn retryable(self) -> Option<bool> {
        match self {
            Self::Ok | Self::Failed => None,
            Self::Locked => Some(true),
            Self::Usage | Self::NotFound | Self::Refused | Self::CheckFailed => Some(false),
        }
    }

    pub const fn meaning(self) -> &'static str {
        match self {
            Self::Ok => {
                "Success, including partial success: per-item failures are listed in data, \
                 mirrored in warnings, and flagged partial: true"
            }
            Self::Failed => {
                "Runtime failure: network, source, archive integrity, I/O, state, internal"
            }
            Self::Usage => "Unknown command or flag, bad argument, bad ref syntax",
            Self::NotFound => "Target, ref, addon, plan or transaction not found, or ambiguous",
            Self::Refused => {
                "Safety refusal: running client, protected folder, ownership or case conflict, \
                 unsafe archive, wrong flavor, undo conflict, blocked host"
            }
            Self::Locked => "Another omaddons process held the lock past the acquire timeout",
            Self::CheckFailed => "An opt-in assertion failed (--check, --strict)",
        }
    }
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        ExitCode::from(exit.code())
    }
}

// ---------------------------------------------------------------------------------------
// Error codes.

macro_rules! error_codes {
    ($($(#[$doc:meta])* $variant:ident = $wire:literal, $exit:ident, $retryable:literal;)+) => {
        /// A stable error code. Each maps to one [`Exit`] and a default retryability.
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
            JsonSchema,
        )]
        pub enum ErrorCode {
            $($(#[$doc])* #[serde(rename = $wire)] $variant,)+
        }

        impl ErrorCode {
            pub const ALL: &[ErrorCode] = &[$(Self::$variant,)+];

            /// The snake_case wire name.
            pub const fn wire(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }

            pub const fn exit(self) -> Exit {
                match self {
                    $(Self::$variant => Exit::$exit,)+
                }
            }

            /// The retryability an [`Error`] starts with. `http_status`, `plan_stale`
            /// and `hash_mismatch` depend on context; their constructors decide.
            pub const fn default_retryable(self) -> bool {
                match self {
                    $(Self::$variant => $retryable,)+
                }
            }
        }
    };
}

#[rustfmt::skip]
error_codes! {
    Usage               = "usage",                 Usage,       false;
    RefInvalid          = "ref_invalid",           Usage,       false;
    /// `addons.toml` cannot be read as desired state; `details: {path, line, key, reason}`.
    DesiredStateInvalid = "desired_state_invalid", Usage,       false;
    TargetNotFound      = "target_not_found",      NotFound,    false;
    TargetAmbiguous     = "target_ambiguous",      NotFound,    false;
    RefAmbiguous        = "ref_ambiguous",         NotFound,    false;
    NotFound            = "not_found",             NotFound,    false;
    NotManaged          = "not_managed",           NotFound,    false;
    PlanNotFound        = "plan_not_found",        NotFound,    false;
    NoCompatibleRelease = "no_compatible_release", NotFound,    false;
    GameRunning         = "game_running",          Refused,     false;
    ProtectedFolder     = "protected_folder",      Refused,     false;
    OwnershipConflict   = "ownership_conflict",    Refused,     false;
    UnmanagedFolder     = "unmanaged_folder",      Refused,     false;
    CaseCollision       = "case_collision",        Refused,     false;
    WrongFlavor         = "wrong_flavor",          Refused,     false;
    UnsafeArchive       = "unsafe_archive",        Refused,     false;
    ArchiveTooLarge     = "archive_too_large",     Refused,     false;
    UndoConflict        = "undo_conflict",         Refused,     false;
    HostBlocked         = "host_blocked",          Refused,     false;
    /// fsx refused a write outside the write-scope table or onto the deny-list (other
    /// than a protected folder, which stays `protected_folder`).
    ScopeDenied         = "scope_denied",          Refused,     false;
    /// A state file's `schema_version` is newer than this binary; writes are refused.
    StateTooNew         = "state_too_new",         Refused,     false;
    /// A restore needs `WTF/` to be a plain directory on the client's mount, on a
    /// filesystem with atomic exchange (M3-D24); `details.reason`, `details.backup_dir`.
    FilesystemUnsupported = "filesystem_unsupported", Refused,  false;
    /// A file omaddons would create already exists (at the start, or created by another
    /// process meanwhile); it is never replaced (`backup export`, M3-D34).
    FileExists          = "file_exists",           Refused,     false;
    Locked              = "locked",                Locked,      true;
    CheckFailed         = "check_failed",          CheckFailed, false;
    Network             = "network",               Failed,      true;
    /// `details.reset_at` carries the reset time (RFC 3339).
    RateLimited         = "rate_limited",          Failed,      true;
    DownloadFailed      = "download_failed",       Failed,      false;
    /// Retryable for 5xx only; see [`Error::http_status`].
    HttpStatus          = "http_status",           Failed,      false;
    /// Retryable once; see [`Error::hash_mismatch`].
    HashMismatch        = "hash_mismatch",         Failed,      true;
    /// Retryable when raised by re-planning, not by `apply`; see [`Error::plan_stale`].
    PlanStale           = "plan_stale",            Failed,      true;
    Offline             = "offline",               Failed,      false;
    BadArchive          = "bad_archive",           Failed,      false;
    DistributionDisabled = "distribution_disabled", Failed,     false;
    SourceDisabled      = "source_disabled",       Failed,      false;
    AuthRequired        = "auth_required",         Failed,      false;
    AuthRejected        = "auth_rejected",         Failed,      false;
    StateCorrupt        = "state_corrupt",         Failed,      false;
    Io                  = "io",                    Failed,      false;
    Internal            = "internal",              Failed,      false;
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wire())
    }
}

// ---------------------------------------------------------------------------------------
// Warning codes.

macro_rules! warning_codes {
    ($($(#[$doc:meta])* $variant:ident = $wire:literal;)+) => {
        /// A stable warning code.
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
            JsonSchema,
        )]
        pub enum WarningCode {
            $($(#[$doc])* #[serde(rename = $wire)] $variant,)+
            /// A per-item failure mirrored into `warnings` during partial success. Its
            /// wire name is the error's.
            #[serde(untagged)]
            Item(ErrorCode),
        }

        impl WarningCode {
            /// The plain warning codes (every variant except [`WarningCode::Item`]).
            pub const ALL: &[WarningCode] = &[$(Self::$variant,)+];

            /// The snake_case wire name.
            pub const fn wire(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::Item(code) => code.wire(),
                }
            }
        }
    };
}

#[rustfmt::skip]
warning_codes! {
    JournalRecovered            = "journal_recovered";
    JournalPending              = "journal_pending";
    TxInProgress                = "tx_in_progress";
    AtomicSwapUnavailable       = "atomic_swap_unavailable";
    ReloadRequired              = "reload_required";
    SavedVariablesDeferred      = "saved_variables_deferred";
    /// SavedVariables in a `WTF/` reached through a symlink outside the client
    /// directory: never snapshotted or written.
    SavedVariablesOutOfScope    = "saved_variables_out_of_scope";
    /// The process table could not be read, so every client is treated as running.
    RunningUnknown              = "running_unknown";
    SnapshotCapExceeded         = "snapshot_cap_exceeded";
    /// Retention after a commit failed; the transaction itself stands.
    RetentionFailed             = "retention_failed";
    OutOfDate                   = "out_of_date";
    /// An archive's folder built for other clients only was left out of an install
    /// whose addon is built for this client.
    FolderLeftOut               = "folder_left_out";
    /// An addon installed under `--any-flavor` although its publisher labelled the file
    /// for other clients, or its folders target other clients; the message names what.
    AnyFlavor                   = "any_flavor";
    /// A GitHub repository's newest release had no build for this client, so an older
    /// one, the newest that declares it, was installed.
    OlderReleaseForClient       = "older_release_for_client";
    MissingDependency           = "missing_dependency";
    TocCommaOnLegacy            = "toc_comma_on_legacy";
    ClientNotLaunchedSincePatch = "client_not_launched_since_patch";
    WidgetOutdated              = "widget_outdated";
    /// State is migrated forward on the next write.
    StateVersionOlder           = "state_version_older";
    /// Writes are refused until omaddons is upgraded.
    StateVersionNewer           = "state_version_newer";
    ConfigUnknownKey            = "config_unknown_key";
    UnknownFlavor               = "unknown_flavor";
    DiscoveryTruncated          = "discovery_truncated";
    TargetKeyDuplicate          = "target_key_duplicate";
    CatalogOverrideInvalid      = "catalog_override_invalid";
    OwnershipTransferred        = "ownership_transferred";
    /// A credential file other users can read.
    CredentialFileMode          = "credential_file_mode";
    /// A pinned addon was left at its pinned release; nothing was installed.
    AddonPinned                 = "addon_pinned";
    /// An answer came from remembered release metadata, not from the source now.
    StaleCheck                  = "stale_check";
    /// `addons.toml` has a key omaddons does not know; it is ignored.
    DesiredUnknownKey           = "desired_unknown_key";
    /// An `addons.toml` `[target.<id>]` section names no known client; it is skipped.
    DesiredUnknownTarget        = "desired_unknown_target";
    /// An adopted addon's name was taken on its target: it is recorded as
    /// `<name> (<primary folder>)`.
    AdoptNameChanged            = "adopt_name_changed";
    /// `export`: a registered client whose dir is gone was exported from its records.
    ExportMissingTarget         = "export_missing_target";
    /// `export`: an addon from a local file or directory was not exported.
    ExportLocalSource           = "export_local_source";
    /// Files of a replaced folder had changed on disk; undo brings them back.
    LocalChangesReplaced        = "local_changes_replaced";
    /// A transaction moved folders that exist nowhere else into its snapshot: undo can
    /// restore them only until retention prunes it.
    SnapshotOnlyCopy            = "snapshot_only_copy";
    /// `sync --locked`: entries without a version installed their newest release.
    NotLocked                   = "not_locked";
    /// A removed addon is still required by addons that stay; undo brings it back.
    StillRequired               = "still_required";
    /// Addons installed as dependencies that nothing requires any more.
    OrphanedDependency          = "orphaned_dependency";
    /// A newly installed addon's other folder (a bridge or plugin) works with an addon
    /// that is not installed; nothing is installed for it.
    BridgeDependency            = "bridge_dependency";
    /// Two or more targets share one AddOns directory (a symlink): each keeps its own
    /// records, so each sees the other's addons as unmanaged.
    TargetAddonsShared          = "target_addons_shared";
    /// Lines of `history.jsonl` that are not cut-short writes did not read: their
    /// transactions are lost to `history` and `undo`.
    HistoryLinesSkipped         = "history_lines_skipped";
    /// A download installed without the hash its source normally publishes; TLS
    /// protected the transfer, and the TOCs were verified.
    DownloadUnverified          = "download_unverified";
    /// A settings change was made, but its history line could not be written.
    HistoryNotRecorded          = "history_not_recorded";
    /// A backup's files were byte-copied, some or all: the filesystem could not clone
    /// them, so they take their full size on disk.
    BackupCopied                = "backup_copied";
    /// `WTF/` changed while it was copied, again on a second capture: the backup is
    /// `best_effort`.
    BackupUnsettled             = "backup_unsettled";
    /// The free space beside a client's backups is below `limits.backup_min_free_mb`.
    BackupSpaceLow              = "backup_space_low";
    /// A BugGrabber file could not be read completely (being written, over a cap or
    /// bound, or damaged): counts are from what was read.
    ErrorsPartial               = "errors_partial";
    /// An interrupted restore was rolled back; what it had displaced, which no other copy
    /// held, is kept as a `recovered` backup.
    RestoreRolledBack           = "restore_rolled_back";
    /// A restore kept no `before_restore` backup (the filesystem cannot clone, or there
    /// was no `WTF/`): the transaction's snapshot keeps the old `WTF/` until retention.
    BeforeRestoreSkipped        = "before_restore_skipped";
    /// An automatic backup failed; the change went ahead without it.
    AutoBackupFailed            = "auto_backup_failed";
    /// The widget is installed, but Omarchy's shell could not be told to put it on the
    /// bar (no desktop session, or the shell is not running).
    OmarchyShellUnreachable     = "omarchy_shell_unreachable";
}

impl fmt::Display for WarningCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wire())
    }
}

// ---------------------------------------------------------------------------------------
// Suggestions and command hints.

/// The fix for internal errors (DEC-5). M4 switches it to `omaddons doctor`.
pub fn internal_fix() -> Suggestion {
    Suggestion::command(["omaddons", "status"])
}

/// The fix for an internal error raised while `command` ran with `argv`, never the
/// failing command itself: `status` points to `status --cached` (no discovery, no
/// network), which failing in turn leaves only a report; any other command gets
/// [`internal_fix`].
pub fn internal_fix_for(command: &str, argv: &[String]) -> Suggestion {
    match command {
        "status" if !argv.iter().any(|a| a == "--cached") => {
            Suggestion::command(["omaddons", "status", "--cached"])
        }
        "status" => Suggestion::text(
            "this is a bug in omaddons: report the message above with the output of omaddons --version",
        ),
        _ => internal_fix(),
    }
}

/// An executable suggestion: a shell-quoted display string plus the argv that runs it,
/// or a non-CLI instruction (`/reload in game`) with no argv. The display is never
/// empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Suggestion {
    display: String,
    argv: Option<Vec<String>>,
}

impl Suggestion {
    /// An `omaddons …` command. The display is `shell_words::join(argv)`.
    pub fn command<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let argv: Vec<String> = argv.into_iter().map(Into::into).collect();
        debug_assert!(
            argv.first().is_some_and(|a| a == "omaddons"),
            "suggested argv must start with omaddons: {argv:?}"
        );
        if argv.is_empty() {
            return internal_fix();
        }
        Self {
            display: shell_words::join(&argv),
            argv: Some(argv),
        }
    }

    /// A non-CLI fix such as `/reload in game`. An empty display falls back to
    /// [`internal_fix`].
    pub fn text(display: impl Into<String>) -> Self {
        let display = display.into();
        if display.trim().is_empty() {
            return internal_fix();
        }
        Self {
            display,
            argv: None,
        }
    }

    pub fn display(&self) -> &str {
        &self.display
    }

    /// `None` for non-CLI fixes.
    pub fn argv(&self) -> Option<&[String]> {
        self.argv.as_deref()
    }
}

impl fmt::Display for Suggestion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display)
    }
}

/// A recommended next command in an envelope's `commands` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CommandHint {
    action: String,
    command: String,
    argv: Vec<String>,
    destructive: bool,
}

impl CommandHint {
    /// `argv` must start with `omaddons`; `command` is its shell-quoted display.
    pub fn new<I, S>(action: impl Into<String>, argv: I, destructive: bool) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let argv: Vec<String> = argv.into_iter().map(Into::into).collect();
        debug_assert!(
            argv.first().is_some_and(|a| a == "omaddons"),
            "command argv must start with omaddons: {argv:?}"
        );
        Self {
            action: action.into(),
            command: shell_words::join(&argv),
            argv,
            destructive,
        }
    }

    pub fn action(&self) -> &str {
        &self.action
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    pub fn destructive(&self) -> bool {
        self.destructive
    }
}

// ---------------------------------------------------------------------------------------
// Errors.

pub type Result<T> = std::result::Result<T, Error>;

/// A typed failure. Serialized through [`ErrorBody`].
///
/// The data is boxed so `Result<T>` stays one pointer wide (clippy's
/// `result_large_err`); its fields are reached through `Deref`, as in `err.code`.
///
/// There is deliberately no `From<std::io::Error>`: every I/O failure goes through
/// [`Error::io`] with its path and a fix.
#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize)]
#[serde(into = "ErrorBody")]
#[error("error[{}]: {}", .0.code, .0.message)]
pub struct Error(Box<ErrorData>);

/// The fields of an [`Error`].
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorData {
    pub code: ErrorCode,
    pub message: String,
    pub fix: Suggestion,
    pub alternatives: Vec<Suggestion>,
    pub retryable: bool,
    /// A JSON object; `{}` when there is nothing to add.
    pub details: serde_json::Value,
    /// How a usage fix relates to the typed command. Not serialized.
    pub usage_fix: UsageFix,
}

impl std::ops::Deref for Error {
    type Target = ErrorData;

    fn deref(&self) -> &ErrorData {
        &self.0
    }
}

impl std::ops::DerefMut for Error {
    fn deref_mut(&mut self) -> &mut ErrorData {
        &mut self.0
    }
}

/// Who raised `plan_stale`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStaleSource {
    /// Engine step 6 re-planning: retrying replans against the new state.
    Replan,
    /// `apply <plan_id>`: the stored plan can never apply; the fix is its dry-run argv.
    Apply,
}

/// How a usage fix relates to the command the user typed (M4-D12). Not on the wire:
/// `cli::usage_error` and the tests read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageFix {
    /// May name another command (the closest name, a synonym, `help`); a guessed command
    /// that changes something is offered as its help (`subcommand_fix`).
    #[default]
    Guess,
    /// The user's own command, kept: only a client word moved into -t (M4-D12) or a
    /// selector corrected (M4-D14). Never demoted to help; it must still parse.
    UserVerb,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>, fix: Suggestion) -> Self {
        Self(Box::new(ErrorData {
            code,
            message: message.into(),
            fix,
            alternatives: Vec::new(),
            retryable: code.default_retryable(),
            details: serde_json::Value::Object(serde_json::Map::new()),
            usage_fix: UsageFix::Guess,
        }))
    }

    /// Marks the fix as the user's own command, kept ([`UsageFix::UserVerb`]).
    #[must_use]
    pub fn user_verb(mut self) -> Self {
        self.usage_fix = UsageFix::UserVerb;
        self
    }

    #[must_use]
    pub fn alternative(mut self, suggestion: Suggestion) -> Self {
        self.alternatives.push(suggestion);
        self
    }

    #[must_use]
    pub fn details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }

    #[must_use]
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// A bug or an impossible state. The fix is [`internal_fix`].
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message, internal_fix())
    }

    /// An I/O failure on `path`. `details.path` carries the path.
    pub fn io(path: &Path, err: &std::io::Error, fix: Suggestion) -> Self {
        Self::new(ErrorCode::Io, format!("{}: {err}", path.display()), fix)
            .details(serde_json::json!({ "path": path.display().to_string() }))
    }

    /// An unexpected HTTP status; retryable for 5xx only. `details.status` carries it.
    pub fn http_status(status: u16, message: impl Into<String>, fix: Suggestion) -> Self {
        Self::new(ErrorCode::HttpStatus, message, fix)
            .retryable(status >= 500)
            .details(serde_json::json!({ "status": status }))
    }

    /// Retryable when raised by re-planning, not when raised by `apply`.
    pub fn plan_stale(
        source: PlanStaleSource,
        message: impl Into<String>,
        fix: Suggestion,
    ) -> Self {
        Self::new(ErrorCode::PlanStale, message, fix).retryable(source == PlanStaleSource::Replan)
    }

    /// A downloaded file did not match its expected hash; retryable on the first
    /// attempt (counting from 1) only.
    pub fn hash_mismatch(attempt: u8, message: impl Into<String>, fix: Suggestion) -> Self {
        Self::new(ErrorCode::HashMismatch, message, fix).retryable(attempt <= 1)
    }

    /// A rate limit with a known reset time, carried as `details.reset_at` (RFC 3339).
    pub fn rate_limited(
        reset_at: jiff::Timestamp,
        message: impl Into<String>,
        fix: Suggestion,
    ) -> Self {
        Self::new(ErrorCode::RateLimited, message, fix)
            .details(serde_json::json!({ "reset_at": reset_at.to_string() }))
    }

    pub fn exit(&self) -> Exit {
        self.code.exit()
    }
}

/// The wire form of an [`Error`]: `fix` is the display string and `fix_argv` its argv.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    pub fix: String,
    pub fix_argv: Option<Vec<String>>,
    pub alternatives: Vec<Suggestion>,
    pub retryable: bool,
    pub details: serde_json::Value,
}

impl From<Error> for ErrorBody {
    fn from(error: Error) -> Self {
        let error = *error.0;
        Self {
            code: error.code,
            message: error.message,
            fix: error.fix.display,
            fix_argv: error.fix.argv,
            alternatives: error.alternatives,
            retryable: error.retryable,
            details: error.details,
        }
    }
}

/// An `Error`'s schema is its wire body's. Written by hand because schemars derives the
/// deserialize contract by default, which `#[serde(into)]` does not affect.
impl JsonSchema for Error {
    fn schema_name() -> Cow<'static, str> {
        "Error".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        ErrorBody::json_schema(generator)
    }
}

// ---------------------------------------------------------------------------------------
// Warnings.

/// A non-fatal finding attached to an outcome. Serialized through [`WarningBody`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(into = "WarningBody", from = "WarningBody")]
pub struct Warning {
    pub code: WarningCode,
    pub message: String,
    pub fix: Option<Suggestion>,
}

impl Warning {
    pub fn new(code: WarningCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            fix: None,
        }
    }

    #[must_use]
    pub fn with_fix(mut self, fix: Suggestion) -> Self {
        self.fix = Some(fix);
        self
    }

    /// A per-item failure mirrored into `warnings` during partial success: the error's
    /// code, message and fix.
    pub fn item(error: &Error) -> Self {
        Self {
            code: WarningCode::Item(error.code),
            message: error.message.clone(),
            fix: Some(error.fix.clone()),
        }
    }
}

/// The wire form of a [`Warning`]. `fix` and `fix_argv` are always present, `null`
/// when absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WarningBody {
    pub code: WarningCode,
    pub message: String,
    pub fix: Option<String>,
    pub fix_argv: Option<Vec<String>>,
}

impl From<Warning> for WarningBody {
    fn from(warning: Warning) -> Self {
        let (fix, fix_argv) = match warning.fix {
            Some(fix) => (Some(fix.display), fix.argv),
            None => (None, None),
        };
        Self {
            code: warning.code,
            message: warning.message,
            fix,
            fix_argv,
        }
    }
}

impl From<WarningBody> for Warning {
    /// A blank `fix` loads as no fix, so a loaded warning never carries an empty display.
    fn from(body: WarningBody) -> Self {
        let fix = body
            .fix
            .filter(|display| !display.trim().is_empty())
            .map(|display| Suggestion {
                display,
                argv: body.fix_argv,
            });
        Self {
            code: body.code,
            message: body.message,
            fix,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet;

    /// A hand-written copy of the bead's table: wire name, exit code, default retryable.
    #[rustfmt::skip]
    const EXPECTED: &[(&str, u8, bool)] = &[
        ("usage", 2, false), ("ref_invalid", 2, false), ("desired_state_invalid", 2, false),
        ("target_not_found", 3, false), ("target_ambiguous", 3, false),
        ("ref_ambiguous", 3, false), ("not_found", 3, false), ("not_managed", 3, false),
        ("plan_not_found", 3, false), ("no_compatible_release", 3, false),
        ("game_running", 4, false), ("protected_folder", 4, false),
        ("ownership_conflict", 4, false), ("unmanaged_folder", 4, false),
        ("case_collision", 4, false), ("wrong_flavor", 4, false), ("unsafe_archive", 4, false),
        ("archive_too_large", 4, false), ("undo_conflict", 4, false), ("host_blocked", 4, false),
        ("scope_denied", 4, false), ("state_too_new", 4, false),
        ("filesystem_unsupported", 4, false), ("file_exists", 4, false),
        ("locked", 5, true),
        ("check_failed", 6, false),
        ("network", 1, true), ("rate_limited", 1, true), ("download_failed", 1, false),
        ("http_status", 1, false), ("hash_mismatch", 1, true), ("plan_stale", 1, true),
        ("offline", 1, false), ("bad_archive", 1, false), ("distribution_disabled", 1, false),
        ("source_disabled", 1, false), ("auth_required", 1, false), ("auth_rejected", 1, false),
        ("state_corrupt", 1, false), ("io", 1, false), ("internal", 1, false),
    ];

    /// The top-level keys of a JSON object in serialized order. `serde_json::Value`
    /// sorts keys (no `preserve_order`), so order checks must read the text.
    fn top_level_keys(text: &str) -> Vec<String> {
        struct Keys(Vec<String>);
        impl<'de> Deserialize<'de> for Keys {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                struct Visitor;
                impl<'de> serde::de::Visitor<'de> for Visitor {
                    type Value = Keys;
                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("a JSON object")
                    }
                    fn visit_map<A: serde::de::MapAccess<'de>>(
                        self,
                        mut map: A,
                    ) -> std::result::Result<Keys, A::Error> {
                        let mut keys = Vec::new();
                        while let Some((key, serde::de::IgnoredAny)) = map.next_entry()? {
                            keys.push(key);
                        }
                        Ok(Keys(keys))
                    }
                }
                deserializer.deserialize_map(Visitor)
            }
        }
        serde_json::from_str::<Keys>(text).unwrap().0
    }

    fn is_wire_name(s: &str) -> bool {
        s.starts_with(|c: char| c.is_ascii_lowercase())
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }

    #[test]
    fn error_registry_matches_the_table() {
        assert_eq!(ErrorCode::ALL.len(), 41);
        assert_eq!(EXPECTED.len(), 41);
        for (code, &(wire, exit, retryable)) in ErrorCode::ALL.iter().zip(EXPECTED) {
            assert_eq!(code.wire(), wire);
            assert_eq!(code.exit().code(), exit, "{wire}");
            assert_eq!(code.default_retryable(), retryable, "{wire}");
            assert_eq!(serde_json::to_value(code).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<ErrorCode>(wire.into()).unwrap(),
                *code
            );
        }
    }

    #[test]
    fn exit_dictionary_is_zero_to_six() {
        let names: Vec<(u8, &str)> = Exit::ALL.iter().map(|e| (e.code(), e.name())).collect();
        assert_eq!(
            names,
            [
                (0, "ok"),
                (1, "failed"),
                (2, "usage"),
                (3, "not_found"),
                (4, "refused"),
                (5, "locked"),
                (6, "check_failed"),
            ]
        );
        for exit in Exit::ALL {
            assert_eq!(serde_json::to_value(exit).unwrap(), exit.name());
            assert!(!exit.meaning().is_empty());
        }
    }

    #[test]
    fn wire_names_are_unique_and_snake_case() {
        let mut seen = HashSet::new();
        let errors = ErrorCode::ALL.iter().map(|c| c.wire());
        let warnings = WarningCode::ALL.iter().map(|c| c.wire());
        for wire in errors.chain(warnings) {
            assert!(is_wire_name(wire), "{wire}");
            assert!(seen.insert(wire), "duplicate wire name {wire}");
        }
        assert_eq!(WarningCode::ALL.len(), 52);
        for code in WarningCode::ALL {
            assert_eq!(serde_json::to_value(code).unwrap(), code.wire());
        }
    }

    #[test]
    fn item_warnings_use_the_error_wire_name() {
        let item = WarningCode::Item(ErrorCode::NoCompatibleRelease);
        assert_eq!(item.wire(), "no_compatible_release");
        assert_eq!(serde_json::to_value(item).unwrap(), "no_compatible_release");
        let back: WarningCode = serde_json::from_value("no_compatible_release".into()).unwrap();
        assert_eq!(back, item);
        let plain: WarningCode = serde_json::from_value("reload_required".into()).unwrap();
        assert_eq!(plain, WarningCode::ReloadRequired);

        let error = Error::new(
            ErrorCode::NoCompatibleRelease,
            "b has no release for retail",
            Suggestion::command(["omaddons", "info", "b"]),
        );
        let warning = Warning::item(&error);
        assert_eq!(warning.code.wire(), "no_compatible_release");
        assert_eq!(
            warning.fix.as_ref().map(Suggestion::display),
            Some("omaddons info b")
        );
    }

    #[test]
    fn context_dependent_retryability() {
        let fix = || Suggestion::command(["omaddons", "status"]);
        assert!(Error::http_status(503, "x", fix()).retryable);
        assert!(Error::http_status(500, "x", fix()).retryable);
        assert!(!Error::http_status(404, "x", fix()).retryable);
        assert!(Error::plan_stale(PlanStaleSource::Replan, "x", fix()).retryable);
        assert!(!Error::plan_stale(PlanStaleSource::Apply, "x", fix()).retryable);
        assert!(Error::hash_mismatch(1, "x", fix()).retryable);
        assert!(!Error::hash_mismatch(2, "x", fix()).retryable);
        assert!(Error::new(ErrorCode::Locked, "x", fix()).retryable);
        assert!(!Error::new(ErrorCode::Usage, "x", fix()).retryable);
        assert_eq!(Error::http_status(503, "x", fix()).details["status"], 503);
    }

    #[test]
    fn user_verb_marks_the_error_and_never_reaches_the_wire() {
        let error = || {
            Error::new(
                ErrorCode::Usage,
                "'retail' is a client, not a command; the client goes in -t",
                Suggestion::command(["omaddons", "update", "-t", "retail"]),
            )
            .details(serde_json::json!({ "target": "retail", "command": "update" }))
        };
        let (guess, kept) = (error(), error().user_verb());
        assert_eq!(guess.usage_fix, UsageFix::Guess);
        assert_eq!(kept.usage_fix, UsageFix::UserVerb);
        assert_eq!(
            serde_json::to_string(&kept).unwrap(),
            serde_json::to_string(&guess).unwrap()
        );
    }

    #[test]
    fn suggestions_are_shell_quoted_and_round_trip() {
        let fix = Suggestion::command(["omaddons", "install", "my addon", "-t", "retail"]);
        assert_eq!(fix.display(), "omaddons install 'my addon' -t retail");
        let hostile = [
            "omaddons",
            "install",
            "a b",
            "$(rm -rf x)",
            "it's",
            "!x",
            "back\\slash",
            "\"dq\"",
            "",
        ];
        let fix = Suggestion::command(hostile);
        assert_eq!(shell_words::split(fix.display()).unwrap(), hostile);
        assert_eq!(fix.argv().unwrap(), hostile);
    }

    #[test]
    fn an_internal_fix_never_reruns_the_failing_command() {
        let argv = |args: &[&str]| -> Vec<String> {
            std::iter::once("omaddons")
                .chain(args.iter().copied())
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(
            internal_fix_for("status", &argv(&["status", "--json"])).argv(),
            Some(&["omaddons", "status", "--cached"].map(str::to_owned)[..])
        );
        let cached = internal_fix_for("status", &argv(&["status", "--cached"]));
        assert_eq!(cached.argv(), None);
        assert!(cached.display().starts_with("this is a bug in omaddons"));
        assert_eq!(internal_fix_for("list", &argv(&["list"])), internal_fix());
    }

    #[test]
    fn suggestions_never_have_an_empty_display() {
        for blank in ["", "   "] {
            let fix = Suggestion::text(blank);
            assert_eq!(fix, internal_fix());
        }
        let reload = Suggestion::text("/reload in game");
        assert_eq!(reload.display(), "/reload in game");
        assert_eq!(reload.argv(), None);

        let internal = Error::internal("boom");
        assert_eq!(internal.fix.display(), "omaddons status");
        assert_eq!(internal.fix.argv().unwrap(), ["omaddons", "status"]);
        assert_eq!(internal.exit(), Exit::Failed);
        assert_eq!(internal.to_string(), "error[internal]: boom");
    }

    proptest! {
        #[test]
        fn any_argv_round_trips_through_its_display(args in proptest::collection::vec(any::<String>(), 0..6)) {
            let mut argv = vec!["omaddons".to_owned()];
            argv.extend(args);
            let fix = Suggestion::command(argv.clone());
            prop_assert_eq!(shell_words::split(fix.display()).unwrap(), argv);
        }
    }

    #[test]
    fn command_hints_carry_argv_and_display() {
        let hint = CommandHint::new(
            "undo",
            ["omaddons", "undo", "01JB8Q2VA00000000000000000"],
            false,
        );
        assert_eq!(
            serde_json::to_string(&hint).unwrap(),
            r#"{"action":"undo","command":"omaddons undo 01JB8Q2VA00000000000000000","argv":["omaddons","undo","01JB8Q2VA00000000000000000"],"destructive":false}"#
        );
    }

    #[test]
    fn error_and_warning_wire_order() {
        let error = Error::new(
            ErrorCode::TargetAmbiguous,
            "weakauras is compatible with 2 targets",
            Suggestion::command(["omaddons", "install", "weakauras", "-t", "retail"]),
        )
        .alternative(Suggestion::command([
            "omaddons",
            "install",
            "weakauras",
            "-t",
            "forever",
        ]))
        .details(serde_json::json!({ "candidates": ["retail", "forever"] }));
        assert_eq!(
            top_level_keys(&serde_json::to_string(&error).unwrap()),
            [
                "code",
                "message",
                "fix",
                "fix_argv",
                "alternatives",
                "retryable",
                "details"
            ]
        );
        insta::assert_json_snapshot!("target_ambiguous_error", error);

        let warning = Warning::new(WarningCode::ReloadRequired, "retail needs a /reload")
            .with_fix(Suggestion::text("/reload in game"));
        assert_eq!(
            top_level_keys(&serde_json::to_string(&warning).unwrap()),
            ["code", "message", "fix", "fix_argv"]
        );
        let json = serde_json::to_value(&warning).unwrap();
        assert_eq!(json["fix_argv"], serde_json::Value::Null);
        insta::assert_json_snapshot!("reload_required_warning", warning);

        let bare = serde_json::to_value(Warning::new(WarningCode::UnknownFlavor, "x")).unwrap();
        assert_eq!(bare["fix"], serde_json::Value::Null);
        assert_eq!(bare["fix_argv"], serde_json::Value::Null);
    }

    #[test]
    fn warnings_round_trip_and_blank_fixes_load_as_none() {
        let warning = Warning::new(WarningCode::JournalPending, "a journal is pending")
            .with_fix(Suggestion::command(["omaddons", "status"]));
        let back: Warning =
            serde_json::from_value(serde_json::to_value(&warning).unwrap()).unwrap();
        assert_eq!(back, warning);
        let blank: Warning = serde_json::from_value(serde_json::json!({
            "code": "unknown_flavor", "message": "x", "fix": " ", "fix_argv": null
        }))
        .unwrap();
        assert_eq!(blank.fix, None);
    }

    #[test]
    fn errors_stay_one_pointer_wide() {
        assert_eq!(std::mem::size_of::<Error>(), std::mem::size_of::<usize>());
        let mut err = Error::internal("x");
        err.message.push('y');
        assert_eq!(err.message, "xy");
    }

    #[test]
    fn helper_constructors_fill_details() {
        let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let io = Error::io(
            Path::new("/x/AddOns"),
            &err,
            Suggestion::text("check permissions"),
        );
        assert_eq!(io.code, ErrorCode::Io);
        assert_eq!(io.details["path"], "/x/AddOns");
        assert!(io.message.starts_with("/x/AddOns: "), "{}", io.message);

        let reset: jiff::Timestamp = "2026-10-14T22:00:00Z".parse().unwrap();
        let limited = Error::rate_limited(reset, "GitHub rate limit", Suggestion::text("wait"));
        assert_eq!(limited.details["reset_at"], "2026-10-14T22:00:00Z");
        assert!(limited.retryable);
        assert_eq!(
            Error::new(ErrorCode::Usage, "x", internal_fix()).details,
            serde_json::json!({})
        );
    }

    #[test]
    fn error_schema_is_the_wire_body() {
        let schema = serde_json::to_value(schemars::schema_for!(Error)).unwrap();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("fix_argv"), "{schema}");
        assert!(!props.contains_key("argv"), "{schema}");
        let warning = serde_json::to_value(schemars::schema_for!(Warning)).unwrap();
        assert!(
            warning["properties"]
                .as_object()
                .unwrap()
                .contains_key("fix_argv")
        );
    }
}
