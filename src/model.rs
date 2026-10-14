//! Domain types: targets, folder names, addon records, refs, release sets, candidates,
//! plans, transactions and findings.
//!
//! Several safety invariants live in these types rather than in scattered checks
//! (vision §3, D14, D27). A [`FolderName`] cannot escape `Interface/AddOns/`, folder
//! identity is ASCII case-insensitive like Wine's lookup, and only a
//! [`MutableFolderName`] (never a `blizzard_*` key) reaches an API that changes a folder.
//! `Family`, `InstallKind`, `TargetKind`, `ClientVersion` and `ClientRules` live in
//! [`crate::rules`] (DEC-13).

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::str::FromStr;

use jiff::Timestamp;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
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

/// A folder name that is not protected. Every API that creates, replaces, renames or
/// removes an AddOns folder takes this type, so a `blizzard_*` folder cannot reach one
/// (D14). Only [`FolderName::into_mutable`] constructs it.
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

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "pattern": "^[0-9A-HJKMNP-TV-Z]{26}$"
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

impl Actor {
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
        [Self::Human, Self::Agent, Self::Widget]
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
}

impl TxKind {
    pub fn is_undoable(self) -> bool {
        self != Self::Backup
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
}

impl Target {
    /// `<client dir>/Interface/AddOns`. Produced here, never created here.
    pub fn addons_dir(&self) -> PathBuf {
        self.client_dir.join("Interface").join("AddOns")
    }

    /// Re-derives `rules` from `table` for this target's family, version and install
    /// kind. Deserialized targets carry the conservative default row until this runs.
    pub fn refresh_rules(&mut self, table: &RulesTable) {
        self.rules = table
            .rules_for_family(self.family, &self.version, self.install_kind)
            .clone();
    }
}

/// One managed addon on one target (vision §5.1). The per-file index lives in
/// `targets/<key>.files.json`, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AddonRecord {
    pub r#ref: Ref,
    pub name: String,
    pub version: String,
    pub version_confidence: VersionConfidence,
    pub release_id: Option<String>,
    pub channel: Channel,
    pub pinned: bool,
    pub reason: AddonReason,
    /// Names of the addons that require this one.
    pub required_by: Vec<String>,
    pub folders: Vec<FolderName>,
    pub state: AddonState,
    #[schemars(with = "String")]
    pub installed_at: Timestamp,
    pub tx_id: TxId,
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
    Git,
    Url,
    File,
}

impl RefKind {
    pub const ALL: [Self; 8] = [
        Self::Gh,
        Self::Wowi,
        Self::Tukui,
        Self::Cf,
        Self::Wago,
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
            Self::Git { .. } => RefKind::Git,
            Self::Url { .. } => RefKind::Url,
            Self::File { .. } => RefKind::File,
        }
    }

    /// The canonical string with GitHub owner and repo lowercased: the identity behind
    /// `Eq`, `Hash` and `Ord`.
    fn identity(&self) -> String {
        match self {
            Self::GitHub { owner, repo } => format!(
                "gh:{}/{}",
                owner.to_ascii_lowercase(),
                repo.to_ascii_lowercase()
            ),
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
        "ref {input:?} has no source prefix; expected gh:, wowi:, tukui:, cf:, wago:, git:, url: or file:"
    )]
    MissingPrefix { input: String },
    #[error(
        "ref {input:?} has unknown source {prefix:?}; expected gh, wowi, tukui, cf, wago, git, url or file"
    )]
    UnknownPrefix { input: String, prefix: String },
    #[error("invalid {kind} ref {input:?}: {reason}")]
    Invalid {
        input: String,
        kind: RefKind,
        reason: &'static str,
    },
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
    #[schemars(with = "Option<String>")]
    pub published_at: Option<Timestamp>,
    pub assets: Vec<Asset>,
    pub changelog_url: Option<String>,
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
}

/// One `release.json` metadata entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AssetFlavor {
    pub flavor: String,
    pub interface: Option<u32>,
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
}

/// A hash a source promises for an asset. SHA-256 only in M1; MD5 (WoWInterface) and
/// SHA-1 (CurseForge) arrive with those sources.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExpectedHash {
    /// Lowercase hex.
    Sha256(String),
}

impl ExpectedHash {
    /// A SHA-256 hash from 64 hex digits in either case, stored lowercase.
    pub fn sha256(hex: &str) -> Option<Self> {
        (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| Self::Sha256(hex.to_ascii_lowercase()))
    }
}

/// One concrete asset proposed for one target, in selection order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Candidate {
    pub release_id: String,
    pub version: String,
    pub channel: Channel,
    pub asset: Asset,
    pub rank: RankReason,
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
        ] {
            assert!(kind.is_undoable(), "{kind:?}");
        }
        assert!(!TxKind::Backup.is_undoable());
    }

    // ---- Refs ----------------------------------------------------------------------

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
        assert_ne!(r("tukui:elvui"), r("tukui:ElvUI"));
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
        }
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
}
