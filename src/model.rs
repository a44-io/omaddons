//! Domain types: targets, folder names, addon records, refs, release sets, candidates,
//! plans, transactions and findings.
//!
//! Several safety invariants live in these types rather than in scattered checks
//! (vision §3, D14, D27). A [`FolderName`] cannot escape `Interface/AddOns/`, folder
//! identity is ASCII case-insensitive like Wine's lookup, and only a
//! [`MutableFolderName`] (never a `blizzard_*` key) reaches the swap that changes a
//! folder.
//! `Family`, `InstallKind`, `TargetKind`, `ClientVersion` and `ClientRules` live in
//! [`crate::rules`] (DEC-13).

#[cfg(feature = "schemas")]
use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::str::FromStr;

use jiff::Timestamp;
use schemars::JsonSchema;
#[cfg(feature = "schemas")]
use schemars::{Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::rules::{ClientRules, ClientVersion, Family, InstallKind, RulesTable, TargetKind};

// ---------------------------------------------------------------------------------------
// Folder names.

/// Linux `NAME_MAX`: the longest directory entry name, in bytes.
const NAME_MAX: usize = 255;

/// A validated `Interface/AddOns/<Name>` entry name that keeps its original spelling.
///
/// Equality, hashing and ordering use [`FolderName::key`], the ASCII case-folded name,
/// so case variants are one folder, as they are to Wine.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct FolderName {
    name: String,
    key: String,
}

impl FolderName {
    /// Rejects empty names, names starting with `.` (including `.` and `..`), path
    /// separators, ASCII control characters (NUL included) and names over 255 bytes.
    pub fn new(name: &str) -> Result<Self, FolderNameError> {
        if name.is_empty() {
            return Err(FolderNameError::Empty);
        }
        if name.starts_with('.') {
            return Err(FolderNameError::LeadingDot(name.to_owned()));
        }
        if name.contains(['/', '\\']) {
            return Err(FolderNameError::Separator(name.to_owned()));
        }
        if name.bytes().any(|b| b.is_ascii_control()) {
            return Err(FolderNameError::ControlCharacter(name.to_owned()));
        }
        if name.len() > NAME_MAX {
            return Err(FolderNameError::TooLong {
                len: name.len(),
                max: NAME_MAX,
            });
        }
        Ok(Self {
            name: name.to_owned(),
            key: name.to_ascii_lowercase(),
        })
    }

    /// The original spelling.
    pub fn as_str(&self) -> &str {
        &self.name
    }

    /// The ASCII case-folded identity used for protection, ownership and collisions.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Whether the folder belongs to the client (`blizzard_*`) and is never mutable.
    pub fn is_protected(&self) -> bool {
        self.key.starts_with("blizzard_")
    }

    /// The mutable form of this name, or the name back when it is protected.
    pub fn into_mutable(self) -> Result<MutableFolderName, FolderName> {
        if self.is_protected() {
            Err(self)
        } else {
            Ok(MutableFolderName(self))
        }
    }
}

impl PartialEq for FolderName {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for FolderName {}

impl Hash for FolderName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

impl PartialOrd for FolderName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FolderName {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

impl fmt::Display for FolderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

impl TryFrom<String> for FolderName {
    type Error = FolderNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value)
    }
}

impl From<FolderName> for String {
    fn from(folder: FolderName) -> Self {
        folder.name
    }
}

/// A folder name that is not protected. The swap, the only code that adds, replaces or
/// removes an AddOns folder, takes it for every folder it changes (`engine::swap::SwapOp`),
/// so a `blizzard_*` folder cannot reach it (D14). The ownership check makes it, refusing
/// a client folder as `protected_folder`, and undo makes it from history. Only
/// [`FolderName::into_mutable`] constructs it.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct MutableFolderName(FolderName);

impl MutableFolderName {
    pub fn as_folder(&self) -> &FolderName {
        &self.0
    }

    pub fn into_folder(self) -> FolderName {
        self.0
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn key(&self) -> &str {
        self.0.key()
    }
}

impl fmt::Display for MutableFolderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl TryFrom<String> for MutableFolderName {
    type Error = FolderNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        FolderName::new(&value)?
            .into_mutable()
            .map_err(|folder| FolderNameError::Protected(folder.name))
    }
}

impl From<MutableFolderName> for String {
    fn from(folder: MutableFolderName) -> Self {
        folder.0.name
    }
}

/// Why a string is not a usable AddOns folder name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FolderNameError {
    #[error("folder name is empty")]
    Empty,
    #[error("folder name {0:?} starts with a dot")]
    LeadingDot(String),
    #[error("folder name {0:?} contains a path separator")]
    Separator(String),
    #[error("folder name {0:?} contains a control character")]
    ControlCharacter(String),
    #[error("folder name is {len} bytes long; the limit is {max}")]
    TooLong { len: usize, max: usize },
    /// Only from parsing a [`MutableFolderName`].
    #[error("folder {0:?} belongs to the client and is never modified")]
    Protected(String),
}

// ---------------------------------------------------------------------------------------
// Ids.

/// A string that is not a ULID.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind} {input:?}: expected a 26-character ULID")]
pub struct IdParseError {
    pub kind: &'static str,
    pub input: String,
}

/// The pattern every ULID key and id publishes (Crockford base32, 26 characters): what
/// rule 3 holds a property named `key` to.
pub const ULID_PATTERN: &str = "^[0-9A-HJKMNP-TV-Z]{26}$";

macro_rules! ulid_id {
    ($(#[$doc:meta])* $name:ident, $kind:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Ulid);

        impl $name {
            /// A fresh id.
            #[expect(
                clippy::new_without_default,
                reason = "a default id would be either random or nil; both would surprise"
            )]
            pub fn new() -> Self {
                Self(Ulid::generate())
            }

            pub const fn from_ulid(ulid: Ulid) -> Self {
                Self(ulid)
            }

            pub const fn ulid(self) -> Ulid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_string(s).map(Self).map_err(|_| IdParseError {
                    kind: $kind,
                    input: s.to_owned(),
                })
            }
        }

        #[cfg(feature = "schemas")]
        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "pattern": $crate::model::ULID_PATTERN
                })
            }
        }
    };
}

ulid_id!(
    /// A target's immutable identity, assigned under the lock at registration.
    TargetKey,
    "target key"
);
ulid_id!(
    /// A transaction id.
    TxId,
    "transaction id"
);
ulid_id!(
    /// A stored plan id.
    PlanId,
    "plan id"
);
ulid_id!(
    /// One command invocation's transactions (one per target it changed): what bare
    /// `undo` reverts together (M2-D38).
    RunId,
    "run id"
);
ulid_id!(
    /// A `WTF/` backup, named by its directory `<meta>/backups/<id>/` (M3-D23).
    BackupId,
    "backup id"
);

/// A target as a token or a stored plan names it: its key once registered, else its
/// provisional id (deterministic, `discover::registry::reconcile`). Every key sorts before
/// every provisional id, keys as [`TargetKey`] sorts them. On the wire it is the plain
/// string, and a string reads as a key only when it is a key's canonical (uppercase)
/// display, so a provisional id (always lowercase) never does.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TargetRef {
    Key(TargetKey),
    Provisional(String),
}

impl TargetRef {
    /// The key when there is one, else the id.
    pub fn of(key: Option<TargetKey>, id: &str) -> Self {
        key.map_or_else(|| Self::Provisional(id.to_owned()), Self::Key)
    }

    /// `Key` for a key's canonical display, `Provisional` for anything else.
    pub fn parse(s: &str) -> Self {
        match s.parse::<TargetKey>() {
            Ok(key) if key.to_string() == s => Self::Key(key),
            _ => Self::Provisional(s.to_owned()),
        }
    }
}

impl fmt::Display for TargetRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(key) => key.fmt(f),
            Self::Provisional(id) => f.write_str(id),
        }
    }
}

impl Serialize for TargetRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TargetRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|s| Self::parse(&s))
    }
}

#[cfg(feature = "schemas")]
impl JsonSchema for TargetRef {
    fn schema_name() -> Cow<'static, str> {
        "TargetRef".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string" })
    }
}

// ---------------------------------------------------------------------------------------
// Small enums.

/// Who made a change: recorded on every transaction (D31).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    Human,
    Agent,
    Widget,
}

/// The value set of the actor flags (`--actor`, `history --by`, `undo --last-by`).
pub const ACTORS: crate::contract::values::ValueSet =
    crate::contract::values::ValueSet::Words(&Actor::WORDS);

impl Actor {
    pub const ALL: [Self; 3] = [Self::Human, Self::Agent, Self::Widget];

    /// The wire names, in [`Actor::ALL`] order: the one list `--actor`, `history --by` and
    /// `undo --last-by` accept.
    pub const WORDS: [&'static str; 3] = [
        Self::Human.as_str(),
        Self::Agent.as_str(),
        Self::Widget.as_str(),
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Widget => "widget",
        }
    }

    /// The `--actor` flag, then `OMADDONS_ACTOR` (empty counts as unset), then `human`
    /// when stdin is a terminal and `agent` otherwise. stdin decides because a human
    /// piping `omaddons list | jq` still types into a terminal, while agents run with no
    /// terminal on any stream.
    pub fn resolve(
        flag: Option<&str>,
        env: Option<&str>,
        stdin_is_tty: bool,
    ) -> Result<Self, ActorError> {
        if let Some(value) = flag {
            return value.parse().map_err(|()| ActorError {
                value: value.to_owned(),
                origin: "--actor",
            });
        }
        if let Some(value) = env.filter(|v| !v.is_empty()) {
            return value.parse().map_err(|()| ActorError {
                value: value.to_owned(),
                origin: "OMADDONS_ACTOR",
            });
        }
        Ok(if stdin_is_tty {
            Self::Human
        } else {
            Self::Agent
        })
    }
}

impl FromStr for Actor {
    type Err = ();

    /// Accepts `human`, `agent` or `widget`, ignoring ASCII case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|a| a.as_str().eq_ignore_ascii_case(s))
            .ok_or(())
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An actor value that is not `human`, `agent` or `widget`. The CLI maps it to `usage`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid actor {value:?} from {origin}: expected human, agent or widget")]
pub struct ActorError {
    pub value: String,
    /// `--actor` or `OMADDONS_ACTOR`.
    pub origin: &'static str,
}

/// A release channel, ordered from most to least stable.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
    Alpha,
}

impl Channel {
    /// Whether an addon on this channel may take a release from `release`: beta takes
    /// stable and beta, alpha takes everything.
    pub fn includes(self, release: Channel) -> bool {
        release <= self
    }
}

/// What a transaction did.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TxKind {
    Install,
    Update,
    Remove,
    Undo,
    Adopt,
    Sync,
    Restore,
    Repair,
    /// WTF backup creation (M3): recorded in history, never undone.
    Backup,
    /// Pins or unpins addons: a records-only change, undoable like any other.
    Pin,
    /// `config set`: a settings change, recorded in history, never undone.
    Config,
    /// `targets add`, `rm`, `rename` and `default`: recorded, never undone.
    Target,
    /// `auth github set` and `clear`: recorded, never undone.
    Auth,
    /// `export --write`: recorded, never undone.
    Export,
    /// `errors dismiss`: Lua errors acknowledged; recorded, never undone.
    Dismiss,
    /// `omarchy install` and `remove`: the bar widget put on or taken off the desktop;
    /// recorded, never undone.
    Widget,
    /// `robot-docs install-skill`: the agent guide installed as a skill; recorded, never
    /// undone.
    Skill,
}

impl TxKind {
    pub fn is_undoable(self) -> bool {
        !matches!(self, Self::Backup) && !self.is_setting()
    }

    /// A settings change (`config`, `target`, `auth`, `export`, `dismiss`, `widget`,
    /// `skill`): a history line that changes no addon folder, so undo never reverts it
    /// (vision §5.9 Attribution).
    pub fn is_setting(self) -> bool {
        matches!(
            self,
            Self::Config
                | Self::Target
                | Self::Auth
                | Self::Export
                | Self::Dismiss
                | Self::Widget
                | Self::Skill
        )
    }
}

/// What the running client needs to pick up a change, ordered so `max()` merges
/// per-operation requirements into one per target.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ReloadRequired {
    #[default]
    None,
    Reload,
    Restart,
}

/// Where an installed version string came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum VersionConfidence {
    /// The source release that was installed.
    Release,
    /// The TOC's `## Version` (adopted folders).
    Toc,
}

/// Why an addon is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AddonReason {
    Explicit,
    Dependency,
}

/// Whether an addon record is whole. `degraded` follows an ownership transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AddonState {
    Ok,
    Degraded,
}

// ---------------------------------------------------------------------------------------
// Targets and records.

/// One client dir plus its identity (vision §5.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Target {
    /// `None` until registered.
    pub key: Option<TargetKey>,
    /// The human alias, e.g. `retail` or `retail@steam`.
    pub id: String,
    pub install_root: PathBuf,
    /// The directory that holds `Interface/` and `WTF/`.
    pub client_dir: PathBuf,
    /// E.g. `_retail_`; `None` for custom roots.
    pub flavor_dir: Option<String>,
    /// E.g. `wow_classic_era`; `None` for custom roots and unknown products.
    pub product: Option<String>,
    pub install_kind: InstallKind,
    pub family: Family,
    pub kind: TargetKind,
    /// `0.0.0` when the version could not be read (family `unknown`, interface 0).
    pub version: ClientVersion,
    pub interface: u32,
    /// The executable's on-disk name; `None` when the flavor folder has none.
    pub exe_name: Option<String>,
    /// E.g. `localhost` from a custom client's realmlist.
    pub label: Option<String>,
    /// Not on the wire. After deserializing, call [`Target::refresh_rules`].
    #[serde(skip)]
    pub rules: ClientRules,
    /// [`crate::toc::judge_digest`] of `rules`, set with them: remembered verdicts are
    /// believed only under it. Empty until [`Target::refresh_rules`] runs, which no
    /// remembered verdict matches.
    #[serde(skip)]
    pub judge: String,
}

impl Target {
    /// `<client dir>/Interface/AddOns`, spelled as on disk
    /// ([`crate::state::paths::ClientPaths::addons`]). Produced here, never created here.
    pub fn addons_dir(&self) -> PathBuf {
        crate::state::paths::ClientPaths::new(&self.client_dir).addons()
    }

    /// Re-derives `rules` from `table` for this target's family, version and install
    /// kind. Deserialized targets carry the conservative default row until this runs.
    pub fn refresh_rules(&mut self, table: &RulesTable) {
        self.rules = table
            .rules_for_family(self.family, &self.version, self.install_kind)
            .clone();
        self.judge = crate::toc::judge_digest(&self.rules, self.family, table);
    }
}

/// One managed addon on one target (vision §5.1). The per-file index lives in
/// `targets/<key>.files.json`, not here. Every install carries `pinned`, `channel`,
/// `reason` and `required_by` over from the previous record by one set of rules
/// (`engine::plan::carry_over`), again under the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AddonRecord {
    pub r#ref: Ref,
    /// The record's identity: an installed addon keeps its name on every update.
    pub name: String,
    pub version: String,
    pub version_confidence: VersionConfidence,
    pub release_id: Option<String>,
    /// The channel its releases come from: kept unless a request names another.
    pub channel: Channel,
    /// Set by an install with a version and kept by every install without one. A
    /// pinned addon stays at its release: installing or updating it without a version
    /// changes nothing (`addon_pinned`).
    pub pinned: bool,
    /// `explicit` when a user asked for it, `dependency` when only the resolver did.
    /// A user's install promotes a dependency; an update keeps the reason.
    pub reason: AddonReason,
    /// Names of the addons that require this one, sorted and de-duplicated.
    pub required_by: Vec<String>,
    pub folders: Vec<FolderName>,
    pub state: AddonState,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub installed_at: Timestamp,
    pub tx_id: TxId,
    /// The blob-cache archive this release was installed from, which retention keeps;
    /// absent for local archives and directories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_sha256: Option<String>,
    /// The source revision of the installed release, when the source has one (WowAce:
    /// the tag's last-changed revision). The same release id at another revision is an
    /// update (a re-tag), and `sync --locked` fetches this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
    /// The installed release's folders this client does not load, left out of the
    /// install, with why, as its `folders.skipped` showed them: what explains a missing
    /// module after the run's output is gone. Every install or update replaces it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left_out: Vec<LeftOutFolder>,
    /// `install --any-flavor`: on this client, builds its publisher did not make for it
    /// install anyway (evidence against is ignored, and off-range interfaces install as
    /// out of date), until `--no-any-flavor`. Kept by every install, update and sync,
    /// like the channel; never by a dependency of it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub any_flavor: bool,
}

/// A folder of an addon's release that an install left out, as `folders.skipped`
/// showed it: `reason` is its wire name (`game_type_excluded`), and `details` holds the
/// reason's other fields (`interfaces`, `family`, `floor`, `folder`). Kept as written,
/// so a reason a newer omaddons wrote still reads; this build writes, and publishes,
/// only the reasons `folders.skipped` has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LeftOutFolder {
    pub name: FolderName,
    #[schemars(schema_with = "crate::toc::reason_names_schema")]
    pub reason: String,
    #[serde(flatten)]
    pub details: serde_json::Map<String, serde_json::Value>,
}

impl LeftOutFolder {
    /// Why it was left out, in words: `DBM-Midnight (gated to other game types)`.
    pub fn describe(&self) -> String {
        let numbers = |key: &str| -> Vec<String> {
            self.details
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|values| values.iter().map(ToString::to_string).collect())
                .unwrap_or_default()
        };
        let text = |key: &str| {
            self.details
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
                .to_owned()
        };
        let why = match self.reason.as_str() {
            "game_type_excluded" => "gated to other game types".to_owned(),
            "no_loadable_toc" => "no TOC this client loads".to_owned(),
            "no_interface" => "no Interface this client reads".to_owned(),
            "target_interface_unknown" => "the client's interface is unknown".to_owned(),
            "dependency_skipped" => format!("requires {}", text("folder")),
            "interface_not_in_family" => {
                format!("lists only interface {}", numbers("interfaces").join(", "))
            }
            "below_client_floor" => format!(
                "interface {}, below the client's floor {}",
                numbers("interfaces").join(", "),
                self.details
                    .get("floor")
                    .map_or_else(|| "?".to_owned(), ToString::to_string)
            ),
            other => other.to_owned(),
        };
        format!("{} ({why})", self.name.as_str())
    }
}

impl AddonRecord {
    /// Installed as a dependency, and nothing requires it any more. An explicitly
    /// installed addon is never an orphan.
    pub fn is_orphan(&self) -> bool {
        self.reason == AddonReason::Dependency && self.required_by.is_empty()
    }
}

// ---------------------------------------------------------------------------------------
// Release identity (regression guard, design 7.5.1, RG-D24).
//
// Skips, first sight, undo's "did it move", "still installed" and the rollback all ask
// whether two releases are the same one, and they must all compare the same thing: a
// [`ReleaseKey`]. What proves the bytes is an [`ArtifactWitness`], kept apart from the
// key, so a tag republished with other bytes is still the same release (a skip holds)
// while the witness says the content changed.

/// Which release, exactly.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct ReleaseKey {
    /// The source, in its one canonical spelling (`Ref::identity()`): a GitHub owner
    /// and repo in one case, and so on.
    pub source: String,
    /// The part of the release id after its last `@`, exactly as the source spells it:
    /// a tag, or the source's own release number.
    pub tag: String,
    /// The source's revision of that tag, when it numbers them (WowAce: the tag's
    /// last-changed revision, which a re-tag moves). Omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
}

impl ReleaseKey {
    /// From a release id (`<ref>@<tag>`) and a revision. The ref is split off at the
    /// last `@` and canonicalized here, once, with the identity
    /// `resolve::updates::same_release` compares, so two ids that `same_release` calls
    /// one release give equal keys at one revision. `None` for an id with no `@`, an
    /// empty tag, or a ref that does not parse.
    pub fn parse(release_id: &str, revision: Option<u32>) -> Option<Self> {
        let (r, tag) = release_id.rsplit_once('@')?;
        if tag.is_empty() {
            return None;
        }
        let r: Ref = r.parse().ok()?;
        Some(Self {
            source: r.identity(),
            tag: tag.to_owned(),
            revision,
        })
    }

    /// The installed release's key. `None` for a record whose release id names no
    /// source release: none at all (an addon adopted at its TOC's version), a local
    /// file or folder (`file:`, whose id is a hash of its content), or a URL (`url:`,
    /// whose id is the URL itself).
    pub fn of_record(record: &AddonRecord) -> Option<Self> {
        if matches!(record.r#ref, Ref::File { .. } | Ref::Url { .. }) {
            return None;
        }
        Self::parse(record.release_id.as_deref()?, record.revision)
    }

    /// A fetched release's key, for the resolver's side.
    pub fn of_release(release: &Release) -> Option<Self> {
        Self::parse(&release.release_id, release.revision)
    }

    /// A candidate's key: the key of the release it is an asset of.
    pub fn of_candidate(candidate: &Candidate) -> Option<Self> {
        Self::parse(&candidate.release_id, candidate.revision)
    }
}

/// What can show that an archive is the one that was installed. Separate from the
/// [`ReleaseKey`]: a tag alone never proves the bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactWitness {
    /// The SHA-256 of the archive it was installed from (`AddonRecord::blob_sha256`).
    /// Omitted when unknown, as for a local archive or folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_sha256: Option<String>,
    /// An immutable source commit, in lowercase hex, for a `git:` record whose release
    /// names a full commit id (40 hex digits). Omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

impl ArtifactWitness {
    /// Whether the two can be the same bytes: every proof both sides hold agrees. A
    /// proof only one side holds decides nothing.
    fn agrees(&self, other: &Self) -> bool {
        fn agree(a: Option<&str>, b: Option<&str>) -> bool {
            a.zip(b).is_none_or(|(a, b)| a == b)
        }
        agree(self.blob_sha256.as_deref(), other.blob_sha256.as_deref())
            && agree(self.commit.as_deref(), other.commit.as_deref())
    }
}

/// One installed release as a fact: what people call it, which release it is, and what
/// proves its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstalledRelease {
    /// The version label, as the record shows it: presentation, never the identity.
    pub version: String,
    /// Which release it is; `null` for a record whose release id names no source release
    /// (see [`ReleaseKey::of_record`]).
    pub release: Option<ReleaseKey>,
    pub witness: ArtifactWitness,
}

impl InstalledRelease {
    /// The release `record` holds: its version, its key and its witness.
    pub fn of(record: &AddonRecord) -> Self {
        let release = ReleaseKey::of_record(record);
        let commit = match (&record.r#ref, &release) {
            (Ref::Git { .. }, Some(key))
                if key.tag.len() == 40 && key.tag.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                Some(key.tag.to_ascii_lowercase())
            }
            _ => None,
        };
        Self {
            version: record.version.clone(),
            release,
            witness: ArtifactWitness {
                blob_sha256: record.blob_sha256.clone(),
                commit,
            },
        }
    }

    /// The same release and, as far as both sides can show, the same bytes: equal keys
    /// (for two releases without a key, equal versions; a key never equals no key), and
    /// equal archive hashes and commits where both have one. A known revision never
    /// equals an unknown one.
    pub fn same_artifact(&self, other: &InstalledRelease) -> bool {
        let same_release = match (&self.release, &other.release) {
            (None, None) => self.version == other.version,
            (a, b) => a == b,
        };
        same_release && self.witness.agrees(&other.witness)
    }
}

/// What one transaction did to one addon's installed release (design 7.2.1). Types only
/// for now: history lines record them (RG-02), and the regression guard's verdicts
/// hold them (RG-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseTransition {
    /// The addon's name, as its record names it.
    pub addon: String,
    pub change: TransitionChange,
    /// What was installed before; `null` when the addon was not.
    pub from: Option<InstalledRelease>,
    /// What is installed after; `null` when the addon is not.
    pub to: Option<InstalledRelease>,
}

/// How a transaction moved one addon's installed release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransitionChange {
    /// The addon was not installed before.
    Installed,
    /// Another release, or the same release with other bytes, replaced the installed one.
    Updated,
    /// The addon is not installed after.
    Removed,
    /// An undo put back what was installed before the transaction it reverts.
    Reverted,
}

// ---------------------------------------------------------------------------------------
// Refs.

/// The kind of a [`Ref`], named by its canonical prefix.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Gh,
    Wowi,
    Tukui,
    Cf,
    Wago,
    #[serde(rename = "wowace")]
    WowAce,
    Git,
    Url,
    File,
}

impl RefKind {
    pub const ALL: [Self; 9] = [
        Self::Gh,
        Self::Wowi,
        Self::Tukui,
        Self::Cf,
        Self::Wago,
        Self::WowAce,
        Self::Git,
        Self::Url,
        Self::File,
    ];

    /// The canonical prefix without the colon, e.g. `gh`.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Gh => "gh",
            Self::Wowi => "wowi",
            Self::Tukui => "tukui",
            Self::Cf => "cf",
            Self::Wago => "wago",
            Self::WowAce => "wowace",
            Self::Git => "git",
            Self::Url => "url",
            Self::File => "file",
        }
    }
}

impl fmt::Display for RefKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.prefix())
    }
}

/// A canonical addon source reference, `source:id`. Serialized as that string.
///
/// `Display` keeps the given spelling. Equality, hashing and ordering treat GitHub owner
/// and repo case-insensitively, as GitHub does, so one repository is fetched once.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub enum Ref {
    /// `gh:Owner/Repo`
    GitHub { owner: String, repo: String },
    /// `wowi:5108`
    Wowi { id: u64 },
    /// `tukui:elvui`
    Tukui { slug: String },
    /// `cf:61284`
    CurseForge { id: u64 },
    /// `wago:<id>`
    Wago { id: String },
    /// `wowace:<slug>[#<tag>|#r<rev>|#r<rev>:<path>]`: a WowAce project's newest stable
    /// tag, a pinned tag, a trunk revision, or a revision of another of its directories.
    WowAce {
        slug: String,
        rev: Option<WowAceRev>,
    },
    /// `git:<https-url>[#<rev>]`
    Git { url: String, rev: Option<String> },
    /// `url:<https-url>`
    Url { url: String },
    /// `file:<path>`
    File { path: PathBuf },
}

impl Ref {
    pub fn kind(&self) -> RefKind {
        match self {
            Self::GitHub { .. } => RefKind::Gh,
            Self::Wowi { .. } => RefKind::Wowi,
            Self::Tukui { .. } => RefKind::Tukui,
            Self::CurseForge { .. } => RefKind::Cf,
            Self::Wago { .. } => RefKind::Wago,
            Self::WowAce { .. } => RefKind::WowAce,
            Self::Git { .. } => RefKind::Git,
            Self::Url { .. } => RefKind::Url,
            Self::File { .. } => RefKind::File,
        }
    }

    /// The canonical string with GitHub owner and repo, and Tukui slugs, lowercased, and a
    /// local path without `.` components (a record written as `file:/x/./Bar` is the
    /// same addon as `file:/x/Bar`): the identity behind `Eq`, `Hash` and `Ord`, and the
    /// release memo's key.
    pub(crate) fn identity(&self) -> String {
        match self {
            Self::GitHub { owner, repo } => format!(
                "gh:{}/{}",
                owner.to_ascii_lowercase(),
                repo.to_ascii_lowercase()
            ),
            Self::Tukui { slug } => format!("tukui:{}", slug.to_ascii_lowercase()),
            Self::File { path } => {
                let lexical: std::path::PathBuf = path.components().collect();
                format!("file:{}", lexical.display())
            }
            _ => self.to_string(),
        }
    }
}

impl PartialEq for Ref {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for Ref {}

impl Hash for Ref {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity().hash(state);
    }
}

impl PartialOrd for Ref {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ref {
    fn cmp(&self, other: &Self) -> Ordering {
        self.identity().cmp(&other.identity())
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitHub { owner, repo } => write!(f, "gh:{owner}/{repo}"),
            Self::Wowi { id } => write!(f, "wowi:{id}"),
            Self::Tukui { slug } => write!(f, "tukui:{slug}"),
            Self::CurseForge { id } => write!(f, "cf:{id}"),
            Self::Wago { id } => write!(f, "wago:{id}"),
            Self::WowAce { slug, rev: None } => write!(f, "wowace:{slug}"),
            Self::WowAce {
                slug,
                rev: Some(rev),
            } => write!(f, "wowace:{slug}#{rev}"),
            Self::Git { url, rev: None } => write!(f, "git:{url}"),
            Self::Git {
                url,
                rev: Some(rev),
            } => write!(f, "git:{url}#{rev}"),
            Self::Url { url } => write!(f, "url:{url}"),
            Self::File { path } => write!(f, "file:{}", path.display()),
        }
    }
}

impl FromStr for Ref {
    type Err = RefParseError;

    /// Parses canonical forms only. Pasted URLs, catalog names and installed names are
    /// the refs layer's job.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((prefix, rest)) = s.split_once(':') else {
            return Err(RefParseError::MissingPrefix {
                input: s.to_owned(),
            });
        };
        let kind = RefKind::ALL
            .into_iter()
            .find(|k| k.prefix() == prefix)
            .ok_or_else(|| RefParseError::UnknownPrefix {
                input: s.to_owned(),
                prefix: prefix.to_owned(),
            })?;
        let invalid = |reason: &'static str| RefParseError::Invalid {
            input: s.to_owned(),
            kind,
            reason,
        };
        match kind {
            RefKind::Gh => {
                let (owner, repo) = rest
                    .split_once('/')
                    .ok_or_else(|| invalid("expected gh:<owner>/<repo>"))?;
                if !is_safe_segment(owner) || !is_safe_segment(repo) {
                    return Err(invalid(
                        "owner and repo must be letters, digits, '_', '.' or '-'",
                    ));
                }
                Ok(Self::GitHub {
                    owner: owner.to_owned(),
                    repo: repo.to_owned(),
                })
            }
            RefKind::Wowi => Ok(Self::Wowi {
                id: parse_decimal(rest).ok_or_else(|| invalid("expected a decimal id"))?,
            }),
            RefKind::Cf => Ok(Self::CurseForge {
                id: parse_decimal(rest).ok_or_else(|| invalid("expected a decimal id"))?,
            }),
            RefKind::Tukui => {
                if !is_safe_segment(rest) {
                    return Err(invalid("slug must be letters, digits, '_', '.' or '-'"));
                }
                Ok(Self::Tukui {
                    slug: rest.to_owned(),
                })
            }
            RefKind::Wago => {
                if !is_safe_segment(rest) {
                    return Err(invalid("id must be letters, digits, '_', '.' or '-'"));
                }
                Ok(Self::Wago {
                    id: rest.to_owned(),
                })
            }
            RefKind::WowAce => {
                let (slug, rev) = match rest.split_once('#') {
                    Some((slug, rev)) => (slug, Some(rev)),
                    None => (rest, None),
                };
                if !is_wowace_slug(slug) {
                    return Err(invalid(
                        "slug must be 1 to 64 lowercase letters, digits or '-', starting with a letter or digit",
                    ));
                }
                let rev = rev
                    .map(|rev| {
                        WowAceRev::parse(rev).ok_or_else(|| {
                            invalid(
                                "after '#' comes a tag (1 to 64 letters, digits, '.', '_', '+' or '-'), r<revision>, or r<revision>:<directory> (not under trunk or tags)",
                            )
                        })
                    })
                    .transpose()?;
                Ok(Self::WowAce {
                    slug: slug.to_owned(),
                    rev,
                })
            }
            RefKind::Git => {
                let (url, rev) = match rest.split_once('#') {
                    Some((url, rev)) => (url, Some(rev)),
                    None => (rest, None),
                };
                check_https_url(url).map_err(invalid)?;
                if let Some(rev) = rev {
                    // A leading '-' would reach git's argv as an option.
                    if rev.is_empty()
                        || rev.starts_with('-')
                        || rev.chars().any(|c| c.is_whitespace() || c.is_control())
                    {
                        return Err(invalid(
                            "rev must be non-empty, not start with '-', and have no spaces",
                        ));
                    }
                }
                Ok(Self::Git {
                    url: url.to_owned(),
                    rev: rev.map(str::to_owned),
                })
            }
            RefKind::Url => {
                check_https_url(rest).map_err(invalid)?;
                Ok(Self::Url {
                    url: rest.to_owned(),
                })
            }
            RefKind::File => {
                if rest.is_empty() {
                    return Err(invalid("expected a path"));
                }
                Ok(Self::File {
                    path: PathBuf::from(rest),
                })
            }
        }
    }
}

impl TryFrom<String> for Ref {
    type Error = RefParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Ref> for String {
    fn from(r: Ref) -> Self {
        r.to_string()
    }
}

/// Why a string is not a canonical ref. The refs layer maps it to `ref_invalid`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefParseError {
    #[error(
        "ref {input:?} has no source prefix; expected gh:, wowi:, tukui:, cf:, wago:, wowace:, git:, url: or file:"
    )]
    MissingPrefix { input: String },
    #[error(
        "ref {input:?} has unknown source {prefix:?}; expected gh, wowi, tukui, cf, wago, wowace, git, url or file"
    )]
    UnknownPrefix { input: String, prefix: String },
    #[error("invalid {kind} ref {input:?}: {reason}")]
    Invalid {
        input: String,
        kind: RefKind,
        reason: &'static str,
    },
}

/// What follows `#` in a `wowace:` ref: a tag, a trunk revision, or a revision of
/// another directory of the project.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WowAceRev {
    /// A tag directory, e.g. `v12.1.0`.
    Tag(String),
    /// A trunk peg revision, written `r152`.
    Revision(u32),
    /// A directory outside trunk and tags at a peg revision, written
    /// `r43:tmp/trunk/!BugGrabber`: the folder the addon is, for a project whose old
    /// builds lie there (BugGrabber's 1.12 era).
    Path { rev: u32, path: String },
}

impl WowAceRev {
    /// `r<digits>` (a `u32`) is a revision, always, and `r<digits>:<path>` that revision
    /// of `path` ([`is_wowace_tree_path`]); anything else is a tag of 1 to 64 letters,
    /// digits, `.`, `_`, `+` or `-`. WowAce tags are version names (`v12.1.0`), so no
    /// real tag reads as a revision.
    pub fn parse(s: &str) -> Option<Self> {
        let digits = |d: &str| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit());
        let revision = |d: &str| digits(d).then(|| d.parse::<u32>().ok()).flatten();
        if let Some(rest) = s.strip_prefix('r') {
            // All digits: a revision, or nothing when it does not fit.
            if digits(rest) {
                return revision(rest).map(Self::Revision);
            }
            if let Some((digits, path)) = rest.split_once(':') {
                return revision(digits)
                    .filter(|_| is_wowace_tree_path(path))
                    .map(|rev| Self::Path {
                        rev,
                        path: path.to_owned(),
                    });
            }
        }
        let tag = (1..=64).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
        tag.then(|| Self::Tag(s.to_owned()))
    }
}

impl fmt::Display for WowAceRev {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tag(tag) => f.write_str(tag),
            Self::Revision(rev) => write!(f, "r{rev}"),
            Self::Path { rev, path } => write!(f, "r{rev}:{path}"),
        }
    }
}

/// A directory of a WowAce project for `#r<rev>:<path>`: 1 to 8 segments joined by `/`,
/// each 1 to 64 letters, digits, `.`, `_`, `+`, `-` or `!` and neither `.` nor `..`,
/// whose first is not `trunk` or `tags` (`#r<rev>` and `#<tag>` name those).
pub fn is_wowace_tree_path(path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').collect();
    (1..=8).contains(&segments.len())
        && !matches!(segments[0], "trunk" | "tags")
        && segments.iter().all(|s| {
            (1..=64).contains(&s.len())
                && *s != "."
                && *s != ".."
                && s.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-' | b'!')
                })
        })
}

/// A WowAce project slug: `^[a-z0-9][a-z0-9-]{0,63}$`.
pub fn is_wowace_slug(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && s.len() <= 64
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A non-empty path segment of letters, digits, `_`, `.` and `-` that is not `.`/`..`.
fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn parse_decimal(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// An `https://` URL with a host and no whitespace or control characters.
fn check_https_url(url: &str) -> Result<(), &'static str> {
    let rest = url
        .get(..8)
        .filter(|scheme| scheme.eq_ignore_ascii_case("https://"))
        .and_then(|_| url.get(8..))
        .ok_or("the URL must start with https://")?;
    if rest.split(['/', '?', '#']).next().is_none_or(str::is_empty) {
        return Err("the URL has no host");
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the URL contains whitespace or control characters");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Releases and candidates.

/// Everything one fetch learned about one ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseSet {
    pub r#ref: Ref,
    pub releases: Vec<Release>,
}

/// One release of a ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Release {
    pub release_id: String,
    pub version: String,
    pub channel: Channel,
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub published_at: Option<Timestamp>,
    pub assets: Vec<Asset>,
    pub changelog_url: Option<String>,
    /// Release notes as plain text, at most `CHANGELOG_MAX_CHARS` (and the cut's `…`),
    /// when the source sends them with the release (WoWInterface's changelog).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The addon's name as the source knows it (WoWInterface's `UIName`), which names
    /// the primary folder when no catalog name applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hint: Option<String>,
    /// The source's revision of this release, when it has one (WowAce: the tag's
    /// last-changed revision, which only a re-tag moves).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
}

/// One downloadable or local artifact of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Asset {
    pub filename: String,
    pub location: AssetLocation,
    pub size: Option<u64>,
    pub expected_hash: Option<ExpectedHash>,
    /// `release.json` metadata.
    pub flavors: Vec<AssetFlavor>,
    /// Flavor guesses from the file name, for repos without `release.json`.
    pub name_flavors: Vec<FlavorHint>,
    pub nolib: bool,
}

/// Where an asset's bytes are, which decides how the engine reads them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetLocation {
    /// Downloaded into the blob cache.
    Url(String),
    /// A user's zip, hashed and opened in place; never copied into the cache.
    LocalZip(PathBuf),
    /// Read through a directory folder view.
    LocalDir(PathBuf),
    /// Checked out by the git-hosts source into a temp dir, then read like `LocalDir`.
    GitCheckout {
        url: String,
        rev: String,
        sha: String,
    },
    /// Downloaded into the blob cache from a URL `source` resolves for `key` right
    /// before each attempt (`Source::resolve_asset`): a short-lived token URL that is
    /// never stored (Tukui, M2-D13).
    Deferred {
        source: crate::sources::SourceId,
        key: String,
    },
    /// A WowAce tree, fetched file by file under `base` (a `…/!svn/bc/<revision>/…`
    /// path, the tree as of that revision and never a live one), then read like
    /// `LocalDir`.
    #[serde(rename = "wowace_tree")]
    WowAceTree { base: String, revision: u32 },
}

/// One client metadata entry: `release.json`'s, or a source list's (`source`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AssetFlavor {
    pub flavor: String,
    pub interface: Option<u32>,
    /// Who claims it; omitted for `release.json` (and absent in memos written before).
    #[serde(default, skip_serializing_if = "FlavorSource::is_release_json")]
    pub source: FlavorSource,
}

/// Who claims an asset's flavor. Only the publisher's `release.json`, naming the client
/// for this very file, declares it (and so may skip folders the client does not load);
/// a source's project list (Tukui's `patch[]`) only ranks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FlavorSource {
    #[default]
    ReleaseJson,
    SourceList,
}

impl FlavorSource {
    pub fn is_release_json(&self) -> bool {
        matches!(self, Self::ReleaseJson)
    }
}

/// A flavor guessed from an asset file name. Names follow the packager's flavors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FlavorHint {
    Mainline,
    Classic,
    Bcc,
    Wrath,
    Cata,
    Mists,
    Titan,
    Forever,
    /// "classic" without saying which classic family.
    ClassicAny,
    /// A legacy client version in the name (`1.12`).
    VanillaLegacy,
    /// `2.4.3`.
    TbcLegacy,
    /// `3.3.5`, `335`.
    WrathLegacy,
    /// `4.3.4`.
    CataLegacy,
    /// `5.4.8`.
    MistsLegacy,
}

/// A hash a source promises for an asset, checked as the download streams. SHA-1
/// (CurseForge) arrives with that source.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExpectedHash {
    /// Lowercase hex.
    Sha256(String),
    /// Lowercase hex (WoWInterface's `UIMD5`, Tukui's `ETag`). It catches corruption
    /// and a changed upstream file, nothing more: MD5 is not a security check, and
    /// SHA-256 stays the blob's identity.
    Md5(String),
}

impl ExpectedHash {
    /// A SHA-256 hash from 64 hex digits in either case, stored lowercase.
    pub fn sha256(hex: &str) -> Option<Self> {
        (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| Self::Sha256(hex.to_ascii_lowercase()))
    }

    /// An MD5 hash from 32 hex digits in either case, stored lowercase.
    pub fn md5(hex: &str) -> Option<Self> {
        (hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| Self::Md5(hex.to_ascii_lowercase()))
    }

    /// The algorithm's name, as messages say it.
    pub const fn algorithm(&self) -> &'static str {
        match self {
            Self::Sha256(_) => "SHA-256",
            Self::Md5(_) => "MD5",
        }
    }

    /// The hex digits, lowercase.
    pub fn hex(&self) -> &str {
        match self {
            Self::Sha256(hex) | Self::Md5(hex) => hex,
        }
    }
}

/// A hash a source does not declare but its download response carries in a header.
/// Like [`ExpectedHash::Md5`], a corruption check, never a security one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HashFromHeader {
    /// A quoted, strong, 32-hex `ETag` on the 200 response is the body's MD5 (Tukui).
    EtagMd5,
}

/// One concrete asset proposed for one target, in selection order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Candidate {
    pub release_id: String,
    pub version: String,
    pub channel: Channel,
    pub asset: Asset,
    pub rank: RankReason,
    /// The release's source revision ([`Release::revision`]), which the installed
    /// record keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
}

/// Which selection tier proposed a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RankReason {
    MetadataInterface,
    MetadataFlavor,
    NameHint,
    Unannotated,
    NameHintMismatch,
    MetadataMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::{BTreeMap, HashSet};

    fn folder(name: &str) -> FolderName {
        FolderName::new(name).unwrap()
    }

    fn r(s: &str) -> Ref {
        s.parse().unwrap()
    }

    // ---- FolderName ----------------------------------------------------------------

    #[test]
    fn folder_name_rejects_unsafe_names() {
        let long = "a".repeat(256);
        let cases: [(&str, FolderNameError); 11] = [
            ("", FolderNameError::Empty),
            (".", FolderNameError::LeadingDot(".".into())),
            ("..", FolderNameError::LeadingDot("..".into())),
            (".hidden", FolderNameError::LeadingDot(".hidden".into())),
            ("a/b", FolderNameError::Separator("a/b".into())),
            ("a\\b", FolderNameError::Separator("a\\b".into())),
            ("../x", FolderNameError::LeadingDot("../x".into())),
            ("x/..", FolderNameError::Separator("x/..".into())),
            ("a\0b", FolderNameError::ControlCharacter("a\0b".into())),
            ("a\tb", FolderNameError::ControlCharacter("a\tb".into())),
            (
                "a\u{7f}",
                FolderNameError::ControlCharacter("a\u{7f}".into()),
            ),
        ];
        for (name, expected) in cases {
            assert_eq!(FolderName::new(name).unwrap_err(), expected, "{name:?}");
        }
        assert_eq!(
            FolderName::new(&long).unwrap_err(),
            FolderNameError::TooLong { len: 256, max: 255 }
        );
        assert!(FolderName::new(&long[..255]).is_ok());
    }

    #[test]
    fn folder_name_accepts_real_addon_names() {
        for name in [
            "WeakAuras",
            "!BugGrabber",
            "Blizzard_AuctionUI",
            "DBM-Core",
            "Foo.Bar",
        ] {
            assert_eq!(folder(name).as_str(), name);
        }
    }

    #[test]
    fn folder_keys_protection_and_mutability() {
        let auction = folder("Blizzard_AuctionUI");
        assert_eq!(auction.key(), "blizzard_auctionui");
        assert!(auction.is_protected());
        assert_eq!(auction.clone().into_mutable().unwrap_err(), auction);
        assert!(folder("BLIZZARD_x").into_mutable().is_err());
        assert!(!folder("BlizzMove").is_protected());

        let weak = folder("WeakAuras").into_mutable().unwrap();
        assert_eq!(weak.as_str(), "WeakAuras");
        assert_eq!(weak.key(), "weakauras");
        assert_eq!(weak.into_folder(), folder("weakauras"));
    }

    #[test]
    fn folder_case_variants_are_one_folder() {
        let a = folder("Blizzard_AuctionUI");
        let b = folder("blizzard_auctionui");
        assert_eq!(a, b);
        assert_eq!(a.cmp(&b), Ordering::Equal);
        assert_eq!(HashSet::from([a.clone(), b.clone()]).len(), 1);
        let map = BTreeMap::from([(a.clone(), 1), (b, 2)]);
        assert_eq!(map.len(), 1);
        assert_eq!(a.as_str(), "Blizzard_AuctionUI");
        assert_eq!(a.to_string(), "Blizzard_AuctionUI");
    }

    #[test]
    fn folder_names_serialize_as_strings_and_validate_on_load() {
        assert_eq!(
            serde_json::to_string(&folder("WeakAuras")).unwrap(),
            "\"WeakAuras\""
        );
        assert!(serde_json::from_str::<FolderName>("\"../x\"").is_err());
        assert!(serde_json::from_str::<FolderName>("\"\"").is_err());
        let mutable: MutableFolderName = serde_json::from_str("\"Plater\"").unwrap();
        assert_eq!(mutable.as_str(), "Plater");
        let err = serde_json::from_str::<MutableFolderName>("\"Blizzard_UIPanels\"").unwrap_err();
        assert!(err.to_string().contains("never modified"), "{err}");
        let schema = serde_json::to_value(schemars::schema_for!(FolderName)).unwrap();
        assert_eq!(schema["type"], "string", "{schema}");
    }

    proptest! {
        #[test]
        fn folder_name_never_escapes_addons(name in "\\PC{0,12}|[./\\\\a-zA-Z\\x00]{0,8}") {
            if let Ok(f) = FolderName::new(&name) {
                let s = f.as_str();
                prop_assert!(!s.contains(['/', '\\', '\0']));
                prop_assert!(!s.starts_with('.'));
                prop_assert!(s.split(['/', '\\']).all(|part| part != ".."));
            }
        }

        #[test]
        fn folder_key_is_idempotent_and_case_insensitive(name in "[!A-Za-z0-9_.-]{1,20}") {
            if let (Ok(lower), Ok(upper)) = (FolderName::new(&name), FolderName::new(&name.to_ascii_uppercase())) {
                prop_assert_eq!(lower.key(), upper.key());
                let refolded = FolderName::new(lower.key()).unwrap();
                prop_assert_eq!(refolded.key(), lower.key());
                prop_assert_eq!(&lower, &upper);
            }
        }
    }

    // ---- Ids -----------------------------------------------------------------------

    #[test]
    fn target_refs_order_keys_first_and_as_keys_do() {
        let key = |n: u64| TargetKey::from_ulid(Ulid::from_parts(n, u128::from(n)));
        let mut refs = vec![
            TargetRef::Provisional("retail".into()),
            TargetRef::Key(key(30)),
            TargetRef::Provisional("era".into()),
            TargetRef::Key(key(10)),
            TargetRef::Provisional("classic".into()),
            TargetRef::Key(key(20)),
        ];
        refs.sort();
        let mut keys = vec![key(30), key(10), key(20)];
        keys.sort();
        let expected: Vec<TargetRef> = keys
            .into_iter()
            .map(TargetRef::Key)
            .chain(["classic", "era", "retail"].map(|id| TargetRef::Provisional(id.into())))
            .collect();
        assert_eq!(refs, expected);
    }

    #[test]
    fn a_target_ref_is_its_plain_string() {
        let key = TargetKey::new();
        for (case, value, text) in [
            ("key", TargetRef::of(Some(key), "retail"), key.to_string()),
            (
                "provisional",
                TargetRef::of(None, "retail"),
                "retail".to_owned(),
            ),
            (
                "a lowercase ULID-like id",
                TargetRef::Provisional(key.to_string().to_ascii_lowercase()),
                key.to_string().to_ascii_lowercase(),
            ),
        ] {
            assert_eq!(value.to_string(), text, "{case}: display");
            let json = serde_json::to_string(&value).unwrap();
            assert_eq!(json, format!("\"{text}\""), "{case}: serialize");
            let back: TargetRef = serde_json::from_str(&json).unwrap();
            assert_eq!(back, value, "{case}: round trip");
        }
    }

    #[test]
    fn ids_serialize_as_26_character_ulids() {
        let key = TargetKey::new();
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json.len(), 28, "{json}");
        assert_eq!(serde_json::from_str::<TargetKey>(&json).unwrap(), key);
        assert_eq!(key.to_string().parse::<TargetKey>().unwrap(), key);

        let tx: TxId = "01JB0000000000000000000000".parse().unwrap();
        assert_eq!(tx.to_string(), "01JB0000000000000000000000");
        let err = "not-a-ulid".parse::<PlanId>().unwrap_err();
        assert_eq!(err.kind, "plan id");
        assert!(serde_json::from_str::<TxId>("\"01JB\"").is_err());
        assert_ne!(TxId::new(), TxId::new());

        let schema = serde_json::to_value(schemars::schema_for!(TxId)).unwrap();
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["pattern"], "^[0-9A-HJKMNP-TV-Z]{26}$");
    }

    // ---- Small enums ---------------------------------------------------------------

    #[test]
    fn actor_words_are_the_parser_list() {
        assert_eq!(Actor::ALL.map(Actor::as_str), Actor::WORDS);
        for (given, want) in [
            ("human", Ok(Actor::Human)),
            ("AGENT", Ok(Actor::Agent)),
            ("Widget", Ok(Actor::Widget)),
            ("robot", Err(())),
            ("", Err(())),
        ] {
            assert_eq!(given.parse::<Actor>(), want, "{given:?}");
        }
        assert_eq!(
            ACTORS,
            crate::contract::values::ValueSet::Words(&Actor::WORDS)
        );
    }

    #[test]
    fn actor_resolution_precedence() {
        use Actor::{Agent, Human, Widget};
        let cases = [
            (Some("widget"), Some("human"), true, Widget),
            (Some("Agent"), None, true, Agent),
            (None, Some("widget"), false, Widget),
            (None, Some(""), true, Human),
            (None, None, true, Human),
            (None, None, false, Agent),
        ];
        for (flag, env, tty, expected) in cases {
            assert_eq!(
                Actor::resolve(flag, env, tty),
                Ok(expected),
                "{flag:?} {env:?} {tty}"
            );
        }
        assert_eq!(
            Actor::resolve(Some("robot"), Some("agent"), false),
            Err(ActorError {
                value: "robot".into(),
                origin: "--actor"
            })
        );
        assert_eq!(
            Actor::resolve(None, Some("bot"), false),
            Err(ActorError {
                value: "bot".into(),
                origin: "OMADDONS_ACTOR"
            })
        );
        assert_eq!(serde_json::to_value(Widget).unwrap(), "widget");
    }

    #[test]
    fn channel_inclusion_matrix() {
        use Channel::{Alpha, Beta, Stable};
        let cases = [
            (Stable, Stable, true),
            (Stable, Beta, false),
            (Stable, Alpha, false),
            (Beta, Stable, true),
            (Beta, Beta, true),
            (Beta, Alpha, false),
            (Alpha, Stable, true),
            (Alpha, Beta, true),
            (Alpha, Alpha, true),
        ];
        for (addon, release, expected) in cases {
            assert_eq!(addon.includes(release), expected, "{addon:?} {release:?}");
        }
        assert_eq!(Channel::default(), Stable);
    }

    #[test]
    fn reload_requirements_merge_by_max() {
        use ReloadRequired::{None as NoReload, Reload, Restart};
        assert_eq!([NoReload, Reload, NoReload].into_iter().max(), Some(Reload));
        assert_eq!([Reload, Restart, NoReload].into_iter().max(), Some(Restart));
        assert_eq!(ReloadRequired::default(), NoReload);
        assert_eq!(serde_json::to_value(NoReload).unwrap(), "none");
    }

    #[test]
    fn only_backups_are_not_undoable() {
        for kind in [
            TxKind::Install,
            TxKind::Update,
            TxKind::Remove,
            TxKind::Undo,
            TxKind::Adopt,
            TxKind::Sync,
            TxKind::Restore,
            TxKind::Repair,
            TxKind::Pin,
        ] {
            assert!(kind.is_undoable(), "{kind:?}");
        }
        assert!(!TxKind::Backup.is_undoable());
        assert_eq!(serde_json::to_value(TxKind::Pin).unwrap(), "pin");
    }

    #[test]
    fn skill_is_a_setting_never_undone() {
        let kind = TxKind::Skill;
        assert!(kind.is_setting(), "{kind:?} is a settings change");
        assert!(!kind.is_undoable(), "{kind:?} is never undone");
        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            "skill",
            "{kind:?}'s wire name"
        );
        assert_eq!(
            serde_json::from_value::<TxKind>(serde_json::json!("skill")).unwrap(),
            kind,
            "\"skill\" reads back as {kind:?}"
        );
    }

    // ---- Refs ----------------------------------------------------------------------

    #[test]
    fn a_recorded_file_ref_with_a_dot_component_is_the_same_addon() {
        let old: Ref = "file:/home/u/Downloads/./Bar-2.1.0.zip".parse().unwrap();
        let new: Ref = "file:/home/u/Downloads/Bar-2.1.0.zip".parse().unwrap();
        assert_eq!(old, new);
        assert_eq!(old.identity(), new.identity());
        // As recorded: the stored spelling is kept.
        assert_eq!(old.to_string(), "file:/home/u/Downloads/./Bar-2.1.0.zip");
        let other: Ref = "file:/home/u/Bar-2.1.0.zip".parse().unwrap();
        assert_ne!(old, other);
    }

    #[test]
    fn canonical_refs_round_trip() {
        let cases = [
            (
                "gh:WeakAuras/WeakAuras2",
                Ref::GitHub {
                    owner: "WeakAuras".into(),
                    repo: "WeakAuras2".into(),
                },
            ),
            ("wowi:5108", Ref::Wowi { id: 5108 }),
            (
                "tukui:elvui",
                Ref::Tukui {
                    slug: "elvui".into(),
                },
            ),
            ("cf:61284", Ref::CurseForge { id: 61284 }),
            (
                "wago:aNDmy96o",
                Ref::Wago {
                    id: "aNDmy96o".into(),
                },
            ),
            (
                "git:https://gitlab.com/a/b.git",
                Ref::Git {
                    url: "https://gitlab.com/a/b.git".into(),
                    rev: None,
                },
            ),
            (
                "git:https://github.com/a/b#v1.2.3",
                Ref::Git {
                    url: "https://github.com/a/b".into(),
                    rev: Some("v1.2.3".into()),
                },
            ),
            (
                "git:https://example.org/r#feature/x",
                Ref::Git {
                    url: "https://example.org/r".into(),
                    rev: Some("feature/x".into()),
                },
            ),
            (
                "url:https://example.org/Foo.zip?x=1",
                Ref::Url {
                    url: "https://example.org/Foo.zip?x=1".into(),
                },
            ),
            (
                "file:/tmp/My Addon.zip",
                Ref::File {
                    path: "/tmp/My Addon.zip".into(),
                },
            ),
            (
                "file:relative/dir",
                Ref::File {
                    path: "relative/dir".into(),
                },
            ),
        ];
        for (text, expected) in cases {
            let parsed = r(text);
            assert_eq!(parsed, expected, "{text}");
            assert_eq!(parsed.to_string(), text);
            assert_eq!(parsed.kind().prefix(), text.split(':').next().unwrap());
            let json = serde_json::to_string(&parsed).unwrap();
            assert_eq!(json, serde_json::to_string(text).unwrap());
            assert_eq!(serde_json::from_str::<Ref>(&json).unwrap(), expected);
        }
    }

    #[test]
    fn github_refs_compare_case_insensitively() {
        let a = r("gh:WeakAuras/WeakAuras2");
        let b = r("gh:weakauras/weakauras2");
        assert_eq!(a, b);
        assert_eq!(a.cmp(&b), Ordering::Equal);
        assert_eq!(HashSet::from([a.clone(), b]).len(), 1);
        assert_eq!(a.to_string(), "gh:WeakAuras/WeakAuras2");
        // Tukui slugs too: one addon, one memo key.
        let (elv, elv_upper) = (r("tukui:elvui"), r("tukui:ElvUI"));
        assert_eq!(elv, elv_upper);
        assert_eq!(HashSet::from([elv.clone(), elv_upper.clone()]).len(), 1);
        assert_eq!(elv.identity(), elv_upper.identity());
        assert_ne!(r("wowi:1"), r("cf:1"));
    }

    #[test]
    fn non_canonical_refs_are_rejected() {
        let cases = [
            "WeakAuras",
            "",
            "github:a/b",
            "gh:a",
            "gh:a/b/c",
            "gh:./b",
            "gh:a/..",
            "gh:a b/c",
            "wowi:",
            "wowi:12a",
            "wowi:+5",
            "cf:-1",
            "tukui:",
            "tukui:elv/ui",
            "wago:",
            "git:http://github.com/a/b",
            "git:https://",
            "git:https:///path",
            "git:https://github.com/a/b#",
            "git:https://github.com/a/b#--upload-pack=x",
            "git:https://github.com/a/b#a b",
            "url:http://example.org/a.zip",
            "url:ftp://example.org/a.zip",
            "url:https://example.org/a b.zip",
            "file:",
        ];
        for text in cases {
            assert!(text.parse::<Ref>().is_err(), "{text:?} parsed");
        }
        assert_eq!(
            "github:a/b".parse::<Ref>().unwrap_err(),
            RefParseError::UnknownPrefix {
                input: "github:a/b".into(),
                prefix: "github".into()
            }
        );
        assert!(matches!(
            "git:http://x/y".parse::<Ref>().unwrap_err(),
            RefParseError::Invalid {
                kind: RefKind::Git,
                ..
            }
        ));
    }

    proptest! {
        #[test]
        fn generated_refs_round_trip(
            owner in "[A-Za-z0-9_-][A-Za-z0-9_.-]{0,15}",
            repo in "[A-Za-z0-9_-][A-Za-z0-9_.-]{0,15}",
            id in any::<u64>(),
            slug in "[a-z0-9_-]{1,12}",
            host in "[a-z0-9-]{1,12}(\\.[a-z]{2,4})?",
            path in "(/[A-Za-z0-9._-]{1,8}){0,3}",
            rev in proptest::option::of("[A-Za-z0-9_][A-Za-z0-9_./-]{0,12}"),
        ) {
            let refs = [
                Ref::GitHub { owner, repo },
                Ref::Wowi { id },
                Ref::CurseForge { id },
                Ref::Tukui { slug: slug.clone() },
                Ref::Wago { id: slug },
                Ref::Git { url: format!("https://{host}{path}"), rev },
                Ref::Url { url: format!("https://{host}{path}") },
            ];
            for original in refs {
                let text = original.to_string();
                prop_assert_eq!(text.parse::<Ref>(), Ok(original), "{}", text);
            }
        }
    }

    // ---- Targets, records, schemas ---------------------------------------------------

    fn sample_target() -> Target {
        let table = RulesTable::builtin();
        let version: ClientVersion = "12.1.5.70391".parse().unwrap();
        let mut target = Target {
            key: Some("01JB8Q2V9W0000000000000000".parse().unwrap()),
            id: "retail".into(),
            install_root: "/games/battlenet/drive_c/Program Files (x86)/World of Warcraft".into(),
            client_dir: "/games/battlenet/drive_c/Program Files (x86)/World of Warcraft/_retail_"
                .into(),
            flavor_dir: Some("_retail_".into()),
            product: Some("wow".into()),
            install_kind: InstallKind::Battlenet,
            family: Family::Mainline,
            kind: TargetKind::Live,
            version,
            interface: 120_105,
            exe_name: Some("WoW.exe".into()),
            label: None,
            rules: ClientRules::default(),
            judge: String::new(),
        };
        target.refresh_rules(&table);
        target
    }

    fn sample_record() -> AddonRecord {
        AddonRecord {
            r#ref: r("gh:WeakAuras/WeakAuras2"),
            name: "WeakAuras".into(),
            version: "5.19.10".into(),
            version_confidence: VersionConfidence::Release,
            release_id: Some("5.19.10".into()),
            channel: Channel::Stable,
            pinned: false,
            reason: AddonReason::Explicit,
            required_by: vec![],
            folders: vec![folder("WeakAuras"), folder("WeakAurasOptions")],
            state: AddonState::Ok,
            installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
            tx_id: "01JB8Q2VA00000000000000000".parse().unwrap(),
            blob_sha256: None,
            revision: None,
            left_out: Vec::new(),
            any_flavor: false,
        }
    }

    /// A record written before `left_out` existed reads as leaving nothing out, and an
    /// empty one is not written; a left-out folder keeps the wire form of
    /// `folders.skipped`, also for a reason this build does not know.
    #[test]
    fn left_out_folders_read_old_records_and_unknown_reasons() {
        let record = sample_record();
        let mut json = serde_json::to_value(&record).unwrap();
        assert!(json.get("left_out").is_none(), "{json}");
        let back: AddonRecord = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, record);
        json["left_out"] = serde_json::json!([
            {"name": "FooRetail", "reason": "interface_not_in_family", "interfaces": [120105], "family": "vanilla"},
            {"name": "FooNew", "reason": "a_reason_from_a_newer_build", "extra": 1},
        ]);
        let read: AddonRecord = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            read.left_out
                .iter()
                .map(LeftOutFolder::describe)
                .collect::<Vec<_>>(),
            [
                "FooRetail (lists only interface 120105)",
                "FooNew (a_reason_from_a_newer_build)"
            ]
        );
        assert_eq!(serde_json::to_value(&read).unwrap(), json, "round-trips");
    }

    #[test]
    fn target_paths_and_rules() {
        let target = sample_target();
        assert!(target.addons_dir().ends_with("_retail_/Interface/AddOns"));
        assert_eq!(target.rules.name, "mainline");

        let json = serde_json::to_value(&target).unwrap();
        assert!(json.get("rules").is_none(), "{json}");
        let mut back: Target = serde_json::from_value(json).unwrap();
        assert_eq!(back.rules, ClientRules::default());
        back.refresh_rules(&RulesTable::builtin());
        assert_eq!(back, target);
    }

    #[test]
    fn target_snapshot() {
        insta::assert_json_snapshot!("target", sample_target());
    }

    #[test]
    fn addon_record_snapshot_and_validation() {
        let record = sample_record();
        insta::assert_json_snapshot!("addon_record", record);
        let mut json = serde_json::to_value(&record).unwrap();
        assert_eq!(
            serde_json::from_value::<AddonRecord>(json.clone()).unwrap(),
            record
        );
        json["folders"][0] = "../x".into();
        assert!(serde_json::from_value::<AddonRecord>(json).is_err());
    }

    #[test]
    fn md5_hashes_normalize_and_tag_on_the_wire() {
        let upper = "AB".repeat(16);
        assert_eq!(
            ExpectedHash::md5(&upper),
            Some(ExpectedHash::Md5("ab".repeat(16)))
        );
        assert_eq!(
            ExpectedHash::md5(&"ab".repeat(32)),
            None,
            "a SHA-256 length"
        );
        assert_eq!(ExpectedHash::md5("abc"), None);
        assert_eq!(ExpectedHash::md5(&"zz".repeat(16)), None);
        for (hash, wire) in [
            (
                ExpectedHash::Md5("0f".repeat(16)),
                serde_json::json!({"md5": "0f".repeat(16)}),
            ),
            (
                ExpectedHash::Sha256("0f".repeat(32)),
                serde_json::json!({"sha256": "0f".repeat(32)}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&hash).unwrap(), wire);
            assert_eq!(serde_json::from_value::<ExpectedHash>(wire).unwrap(), hash);
        }
        assert_eq!(
            serde_json::to_value(HashFromHeader::EtagMd5).unwrap(),
            "etag_md5"
        );
    }

    #[test]
    fn a_deferred_location_round_trips() {
        let location = AssetLocation::Deferred {
            source: crate::sources::SourceId::Tukui,
            key: "elvui".into(),
        };
        let wire = serde_json::json!({"deferred": {"source": "tukui", "key": "elvui"}});
        assert_eq!(serde_json::to_value(&location).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<AssetLocation>(wire).unwrap(),
            location
        );
    }

    #[test]
    fn expected_hash_normalizes_hex() {
        let upper = "AB".repeat(32);
        assert_eq!(
            ExpectedHash::sha256(&upper),
            Some(ExpectedHash::Sha256("ab".repeat(32)))
        );
        assert_eq!(ExpectedHash::sha256("abc"), None);
        assert_eq!(ExpectedHash::sha256(&"zz".repeat(32)), None);
        assert_eq!(
            serde_json::to_value(ExpectedHash::Sha256("00".repeat(32))).unwrap(),
            serde_json::json!({ "sha256": "00".repeat(32) })
        );
    }

    #[test]
    fn release_types_serialize_with_snake_case_tags() {
        let asset = Asset {
            filename: "WeakAuras-5.19.10.zip".into(),
            location: AssetLocation::GitCheckout {
                url: "https://x.org/r".into(),
                rev: "main".into(),
                sha: "abc".into(),
            },
            size: Some(1),
            expected_hash: None,
            flavors: vec![AssetFlavor {
                flavor: "mainline".into(),
                interface: Some(120_105),
                source: FlavorSource::ReleaseJson,
            }],
            name_flavors: vec![FlavorHint::ClassicAny],
            nolib: false,
        };
        let json = serde_json::to_value(&asset).unwrap();
        assert_eq!(json["location"]["git_checkout"]["rev"], "main");
        assert_eq!(json["name_flavors"][0], "classic_any");
        let url = serde_json::to_value(AssetLocation::LocalZip("/x.zip".into())).unwrap();
        assert_eq!(url, serde_json::json!({ "local_zip": "/x.zip" }));
        assert_eq!(
            serde_json::to_value(RankReason::NameHintMismatch).unwrap(),
            "name_hint_mismatch"
        );
        let candidate = Candidate {
            release_id: "1".into(),
            version: "1".into(),
            channel: Channel::Beta,
            asset,
            rank: RankReason::MetadataInterface,
            revision: Some(401),
        };
        let back: Candidate =
            serde_json::from_value(serde_json::to_value(&candidate).unwrap()).unwrap();
        assert_eq!(back, candidate);
    }

    #[test]
    fn schemas_generate_for_the_main_types() {
        for schema in [
            schemars::schema_for!(Target),
            schemars::schema_for!(AddonRecord),
            schemars::schema_for!(ReleaseSet),
            schemars::schema_for!(Candidate),
        ] {
            let json = serde_json::to_value(&schema).unwrap();
            assert!(
                json.get("type").is_some() || json.get("$ref").is_some(),
                "{json}"
            );
        }
        let target = serde_json::to_value(schemars::schema_for!(Target)).unwrap();
        assert!(target["properties"].get("rules").is_none(), "{target}");
        assert_eq!(
            target["properties"]["version"]["$ref"],
            "#/$defs/ClientVersion"
        );
        assert_eq!(target["$defs"]["ClientVersion"]["type"], "string");
        assert_eq!(target["$defs"]["TargetKey"]["type"], "string");
    }

    // ---- Release identity ------------------------------------------------------------

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const COMMIT: &str = "0123456789ABCDEF0123456789abcdef01234567";

    fn key(release_id: &str, revision: Option<u32>) -> ReleaseKey {
        ReleaseKey::parse(release_id, revision)
            .unwrap_or_else(|| panic!("{release_id} should parse"))
    }

    /// A record of `ref_` at `release_id`, with an archive hash and a revision.
    fn record_at(
        ref_: &str,
        release_id: Option<&str>,
        blob: Option<&str>,
        revision: Option<u32>,
    ) -> AddonRecord {
        AddonRecord {
            r#ref: r(ref_),
            release_id: release_id.map(str::to_owned),
            blob_sha256: blob.map(str::to_owned),
            revision,
            ..sample_record()
        }
    }

    #[test]
    fn release_key_parse_compares_one_release() {
        // (case, a, a's revision, b, b's revision, whether the keys are equal)
        let cases = [
            (
                "a GitHub id in two spellings of owner and repo",
                "gh:WeakAuras/WeakAuras2@5.19.2",
                None,
                "gh:weakauras/WEAKAURAS2@5.19.2",
                None,
                true,
            ),
            (
                "a Tukui slug in two cases",
                "tukui:ElvUI@13.92",
                None,
                "tukui:elvui@13.92",
                None,
                true,
            ),
            (
                "a local path with and without a `.` component",
                "file:/x/./Bar.zip@sha256:1",
                None,
                "file:/x/Bar.zip@sha256:1",
                None,
                true,
            ),
            ("two tags", "gh:o/foo@v1", None, "gh:o/foo@v2", None, false),
            (
                "a tag keeps its spelling",
                "gh:o/foo@V1",
                None,
                "gh:o/foo@v1",
                None,
                false,
            ),
            (
                "one tag at two revisions (a WowAce re-tag)",
                "wowace:bug-grabber@v12.1.1",
                Some(400),
                "wowace:bug-grabber@v12.1.1",
                Some(403),
                false,
            ),
            (
                "a known revision is not an unknown one",
                "wowace:bug-grabber@v12.1.1",
                Some(400),
                "wowace:bug-grabber@v12.1.1",
                None,
                false,
            ),
            (
                "one tag at one revision",
                "wowace:bug-grabber@v12.1.1",
                Some(400),
                "wowace:bug-grabber@v12.1.1",
                Some(400),
                true,
            ),
            (
                "two sources with one tag",
                "gh:o/foo@v1",
                None,
                "gh:o/bar@v1",
                None,
                false,
            ),
        ];
        for (case, a, a_rev, b, b_rev, equal) in cases {
            let (x, y) = (key(a, a_rev), key(b, b_rev));
            assert_eq!(x == y, equal, "{case}: {x:?} vs {y:?}");
        }
    }

    #[test]
    fn release_key_parse_refuses_what_names_no_release() {
        let cases = [
            ("an id without `@`", "5.19.10"),
            (
                "a URL release id, which is the URL",
                "url:https://github.com/o/r/a.zip",
            ),
            ("an empty tag", "gh:o/foo@"),
            ("a ref that does not parse", "gh:o@v1"),
            ("no ref at all", "@v1"),
            ("nothing", ""),
        ];
        for (case, id) in cases {
            assert_eq!(ReleaseKey::parse(id, None), None, "{case}: {id}");
        }
    }

    #[test]
    fn release_key_parse_canonicalizes_the_source_and_splits_at_the_last_at() {
        let gh = key("gh:WeakAuras/WeakAuras2@5.19.2", Some(7));
        assert_eq!(gh.source, "gh:weakauras/weakauras2");
        assert_eq!(gh.tag, "5.19.2");
        assert_eq!(gh.revision, Some(7));
        let wowace = key("wowace:bug-grabber@r43:tmp/trunk/!BugGrabber", Some(43));
        assert_eq!(wowace.source, "wowace:bug-grabber");
        assert_eq!(wowace.tag, "r43:tmp/trunk/!BugGrabber");
    }

    #[test]
    fn release_key_of_release_and_candidate_carry_the_revision() {
        let release = Release {
            release_id: "wowace:bug-grabber@v12.1.1".into(),
            version: "v12.1.1".into(),
            channel: Channel::Stable,
            published_at: None,
            assets: Vec::new(),
            changelog_url: None,
            notes: None,
            name_hint: None,
            revision: Some(403),
        };
        let expected = key("wowace:bug-grabber@v12.1.1", Some(403));
        assert_eq!(ReleaseKey::of_release(&release), Some(expected.clone()));
        let candidate = Candidate {
            release_id: release.release_id.clone(),
            version: release.version.clone(),
            channel: Channel::Stable,
            asset: Asset {
                filename: "bug-grabber.zip".into(),
                location: AssetLocation::Url("https://repos.wowace.com/x".into()),
                size: None,
                expected_hash: None,
                flavors: Vec::new(),
                name_flavors: Vec::new(),
                nolib: false,
            },
            rank: RankReason::Unannotated,
            revision: Some(403),
        };
        assert_eq!(ReleaseKey::of_candidate(&candidate), Some(expected));
    }

    /// A release id from a small pool, so that pairs often name one release in two
    /// spellings, and sometimes do not parse at all.
    fn release_id() -> impl Strategy<Value = String> {
        let source = prop_oneof![
            ("[aA][bB]?", "[cC][dD]?").prop_map(|(o, r)| format!("gh:{o}/{r}")),
            "[eE][lL]?".prop_map(|s| format!("tukui:{s}")),
            (1u64..3).prop_map(|id| format!("wowi:{id}")),
            "[ab]{1,2}".prop_map(|s| format!("wowace:{s}")),
            Just("git:https://github.com/o/r#main".to_owned()),
            Just("file:/x/./a.zip".to_owned()),
            Just("file:/x/a.zip".to_owned()),
            Just("url:https://github.com/o/r/a.zip".to_owned()),
            Just("gh:o".to_owned()),
            "\\PC{0,8}",
        ];
        let tag = prop_oneof![
            Just("v1".to_owned()),
            Just("V1".to_owned()),
            Just("v2".to_owned()),
            Just("r43:tmp/trunk/!BugGrabber".to_owned()),
            Just(String::new()),
            "\\PC{0,4}",
        ];
        (source, proptest::option::of(tag)).prop_map(|(source, tag)| match tag {
            Some(tag) => format!("{source}@{tag}"),
            None => source,
        })
    }

    proptest! {
        /// `same_release` and the key agree on every pair of ids that both parse, so
        /// the key and the resolver's comparison cannot disagree.
        #[test]
        fn release_key_agrees_with_same_release(a in release_id(), b in release_id()) {
            if let (Some(x), Some(y)) = (ReleaseKey::parse(&a, None), ReleaseKey::parse(&b, None)) {
                prop_assert_eq!(
                    crate::resolve::updates::same_release(&a, &b),
                    x == y,
                    "{:?} vs {:?}",
                    a,
                    b
                );
            }
        }
    }

    #[test]
    fn installed_release_of_reads_the_record() {
        let gh = InstalledRelease::of(&record_at(
            "gh:WeakAuras/WeakAuras2",
            Some("gh:WeakAuras/WeakAuras2@5.19.10"),
            Some(SHA_A),
            None,
        ));
        assert_eq!(gh.version, "5.19.10");
        assert_eq!(
            gh.release,
            Some(key("gh:weakauras/weakauras2@5.19.10", None))
        );
        assert_eq!(gh.witness.blob_sha256.as_deref(), Some(SHA_A));
        assert_eq!(gh.witness.commit, None);

        // (case, ref, release id): records whose release names no source release.
        let keyless = [
            (
                "a local file",
                "file:/x/Bar.zip",
                Some(format!("file:/x/Bar.zip@sha256:{SHA_A}")),
            ),
            (
                "a local folder",
                "file:/x/Bar",
                Some("file:/x/Bar@listing:1234".to_owned()),
            ),
            (
                "a URL with an @ in its path",
                "url:https://github.com/o/r/a@b/c.zip",
                Some("url:https://github.com/o/r/a@b/c.zip".to_owned()),
            ),
            ("an adoption at its TOC's version", "gh:o/foo", None),
        ];
        for (case, ref_, release_id) in keyless {
            let installed =
                InstalledRelease::of(&record_at(ref_, release_id.as_deref(), None, None));
            assert_eq!(installed.release, None, "{case}");
            assert_eq!(installed.version, "5.19.10", "{case}");
        }

        let wowace = InstalledRelease::of(&record_at(
            "wowace:bug-grabber",
            Some("wowace:bug-grabber@v12.1.1"),
            Some(SHA_B),
            Some(403),
        ));
        assert_eq!(
            wowace.release,
            Some(key("wowace:bug-grabber@v12.1.1", Some(403)))
        );
        assert_eq!(wowace.witness.commit, None);
    }

    #[test]
    fn installed_release_of_a_git_record_witnesses_a_full_commit() {
        // (case, release id, the commit witnessed)
        let cases = [
            (
                "a full commit id, lowercased",
                format!("git:https://github.com/o/r#main@{COMMIT}"),
                Some(COMMIT.to_ascii_lowercase()),
            ),
            (
                "a tag",
                "git:https://codeberg.org/o/r#v1@v1".to_owned(),
                None,
            ),
            (
                "a short commit id",
                "git:https://github.com/o/r#main@0123456".to_owned(),
                None,
            ),
        ];
        for (case, release_id, commit) in cases {
            let installed = InstalledRelease::of(&record_at(
                "git:https://github.com/o/r#main",
                Some(&release_id),
                None,
                None,
            ));
            assert!(installed.release.is_some(), "{case}");
            assert_eq!(installed.witness.commit, commit, "{case}");
        }
        // A 40-hex tag of another source is no commit.
        let gh = InstalledRelease::of(&record_at(
            "gh:o/foo",
            Some(&format!("gh:o/foo@{COMMIT}")),
            None,
            None,
        ));
        assert_eq!(gh.witness.commit, None);
    }

    #[test]
    fn installed_release_same_artifact() {
        let at = |release_id: Option<&str>, blob: Option<&str>, revision: Option<u32>| {
            let ref_ = release_id.map_or("file:/x/Foo.zip", |id| {
                id.rsplit_once('@').map_or(id, |(r, _)| r)
            });
            InstalledRelease::of(&record_at(ref_, release_id, blob, revision))
        };
        let with_version = |version: &str| InstalledRelease {
            version: version.to_owned(),
            ..at(None, None, None)
        };
        let v1 = Some("wowace:foo@v1");
        // (case, a, b, the same artifact)
        let cases = [
            (
                "equal keys and equal hashes",
                at(v1, Some(SHA_A), Some(5)),
                at(v1, Some(SHA_A), Some(5)),
                true,
            ),
            (
                "equal keys and one hash unknown",
                at(v1, Some(SHA_A), Some(5)),
                at(v1, None, Some(5)),
                true,
            ),
            (
                "equal keys and different hashes",
                at(v1, Some(SHA_A), Some(5)),
                at(v1, Some(SHA_B), Some(5)),
                false,
            ),
            (
                "different revisions of one tag",
                at(v1, Some(SHA_A), Some(5)),
                at(v1, Some(SHA_A), Some(6)),
                false,
            ),
            (
                "a known revision and an unknown one",
                at(v1, None, Some(5)),
                at(v1, None, None),
                false,
            ),
            (
                "a key and no key",
                at(v1, None, None),
                InstalledRelease {
                    version: "v1".into(),
                    ..at(None, None, None)
                },
                false,
            ),
            (
                "two releases without a key and equal versions",
                with_version("1.0"),
                with_version("1.0"),
                true,
            ),
            (
                "two releases without a key and other versions",
                with_version("1.0"),
                with_version("1.1"),
                false,
            ),
        ];
        for (case, a, b, same) in cases {
            assert_eq!(a.same_artifact(&b), same, "{case}: {a:?} vs {b:?}");
            assert_eq!(b.same_artifact(&a), same, "{case}, reversed");
        }
    }

    #[test]
    fn release_identity_serializes_without_what_is_unknown() {
        let bare = key("gh:o/foo@v1", None);
        let json = serde_json::to_value(&bare).unwrap();
        assert_eq!(json, serde_json::json!({"source": "gh:o/foo", "tag": "v1"}));
        let revised = key("wowace:foo@v1", Some(9));
        assert_eq!(serde_json::to_value(&revised).unwrap()["revision"], 9);
        assert_eq!(
            serde_json::to_value(ArtifactWitness::default()).unwrap(),
            serde_json::json!({})
        );

        let transition = ReleaseTransition {
            addon: "Foo".into(),
            change: TransitionChange::Reverted,
            from: Some(InstalledRelease {
                version: "v1".into(),
                release: Some(revised),
                witness: ArtifactWitness {
                    blob_sha256: Some(SHA_A.into()),
                    commit: None,
                },
            }),
            to: None,
        };
        let json = serde_json::to_value(&transition).unwrap();
        assert_eq!(json["change"], "reverted");
        assert_eq!(json["to"], serde_json::Value::Null);
        assert_eq!(
            json["from"]["witness"],
            serde_json::json!({"blob_sha256": SHA_A})
        );
        let back: ReleaseTransition = serde_json::from_value(json).unwrap();
        assert_eq!(back, transition);
        for (change, wire) in [
            (TransitionChange::Installed, "installed"),
            (TransitionChange::Updated, "updated"),
            (TransitionChange::Removed, "removed"),
            (TransitionChange::Reverted, "reverted"),
        ] {
            assert_eq!(serde_json::to_value(change).unwrap(), wire);
        }
        let schema = serde_json::to_value(schemars::schema_for!(ReleaseTransition)).unwrap();
        assert!(schema.get("type").is_some(), "{schema}");
    }
}
