//! The finding model (E1; M4-D30, M4-D41, M4-D42): what one doctor finding is, how it
//! names what is broken, how sure it is, what it shows and how to address it.
//!
//! A detector reports [`Observation`]s, one per broken instance; the registry row that ran
//! it turns each into a [`Finding`] with [`Finding::build`], adding the row's id,
//! subsystem, severity and remediation, so no detector can misspell or mis-rank its own
//! findings. A finding's stable [`InstanceKey`] and its human `label` come from one
//! [`Subject`] through one function, so no detector can spell them differently. Pure: no
//! I/O and no clock.

use std::borrow::Cow;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Serialize, Serializer};

use super::budget::Budget;
use crate::model::{FolderName, TargetKey, TxId};

/// How sure a finding is (M4-D41): `high` when the state was observed directly (a hash, a
/// mode, a missing file), `medium` when inferred from consistent evidence, `low` for a
/// heuristic. The same enum as `errors`' attribution confidence.
pub use crate::errors::attribute::Confidence;

// ---------------------------------------------------------------------------------------
// Subsystems and severity.

/// The doctor contract's thirteen subsystems, the middle part of every failure-mode id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Subsystem {
    StateFiles,
    Configs,
    Schemas,
    Caches,
    Sockets,
    Hooks,
    Plugins,
    Secrets,
    Permissions,
    ExternalArtifacts,
    ConcurrencyPrimitives,
    Network,
    UserlandState,
}

impl Subsystem {
    /// Every subsystem, in the doctor contract's order.
    pub const ALL: [Self; 13] = [
        Self::StateFiles,
        Self::Configs,
        Self::Schemas,
        Self::Caches,
        Self::Sockets,
        Self::Hooks,
        Self::Plugins,
        Self::Secrets,
        Self::Permissions,
        Self::ExternalArtifacts,
        Self::ConcurrencyPrimitives,
        Self::Network,
        Self::UserlandState,
    ];

    /// Its wire name, as in a failure-mode id.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StateFiles => "state_files",
            Self::Configs => "configs",
            Self::Schemas => "schemas",
            Self::Caches => "caches",
            Self::Sockets => "sockets",
            Self::Hooks => "hooks",
            Self::Plugins => "plugins",
            Self::Secrets => "secrets",
            Self::Permissions => "permissions",
            Self::ExternalArtifacts => "external_artifacts",
            Self::ConcurrencyPrimitives => "concurrency_primitives",
            Self::Network => "network",
            Self::UserlandState => "userland_state",
        }
    }

    /// Whether omaddons has failure modes in it: it has no sockets and runs no hooks.
    pub const fn applicable(self) -> bool {
        !matches!(self, Self::Sockets | Self::Hooks)
    }
}

/// A name that is no subsystem's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown subsystem {0:?}")]
pub struct UnknownSubsystem(pub String);

impl FromStr for Subsystem {
    type Err = UnknownSubsystem;

    /// Exact wire names only.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|subsystem| subsystem.as_str() == s)
            .ok_or_else(|| UnknownSubsystem(s.to_owned()))
    }
}

/// How urgent an issue is (vision D20): the bar turns urgent on P0 and P1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub enum Severity {
    P0,
    P1,
    P2,
    P3,
}

// ---------------------------------------------------------------------------------------
// Evidence and remediation.

/// What a finding shows (M4-D42): typed paths, digests, modes, counts and positions, never
/// file content and never a path below `WTF/`. Every key is omitted when unset, and catalog
/// leaves add keys as further optional fields, never as a map.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Evidence {
    /// The one path concerned. Omitted when the finding names no single path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Several paths concerned, in a stable order. Omitted when the finding names none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<PathBuf>>,
    /// A digest in fsx's text form (`sha256:…`, `stamp:sha256:…`, `tree:sha256:…`).
    /// Omitted when no digest is concerned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// What was found (a version, a mode, a digest). Omitted when there is nothing to set
    /// against an expectation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<String>,
    /// What was expected instead. Omitted when there is nothing to set against what was
    /// found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// A permission mode, as four octal digits (`"0600"`). Omitted when no mode is
    /// concerned.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "octal_mode")]
    #[schemars(with = "Option<String>", pattern(r"^[0-7]{4}$"))]
    pub mode: Option<u32>,
    /// A process id. Omitted when no process is concerned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// An age in seconds. Omitted when no age is concerned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_s: Option<u64>,
    /// A 1-based line in a file. Omitted when no position is concerned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// A 1-based column in that line. Omitted when no position is concerned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// A transaction. Omitted when no transaction is concerned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx: Option<TxId>,
    /// A count. Omitted when nothing is counted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    /// A snake_case code saying why, never formatted text. Omitted when the failure mode
    /// says it all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    /// A configuration key's name (`limits.tx_retention_count`), never its value. Omitted
    /// when no key is concerned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// A size in bytes. Omitted when no size is concerned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// The name of the limit a size or count is over (`DISCOVERY_FILE_MAX_BYTES`), as
    /// `capabilities` lists it. Omitted when no limit is concerned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<&'static str>,
}

/// `mode` as exactly four octal digits.
fn octal_mode<S: Serializer>(mode: &Option<u32>, serializer: S) -> Result<S::Ok, S::Error> {
    match mode {
        Some(mode) => serializer.collect_str(&format_args!("{mode:04o}")),
        None => serializer.serialize_none(),
    }
}

impl Evidence {
    /// Whether it keeps clear of `WTF/`, whose paths name the account, realm and
    /// character: no path has a component `WTF` (in any case) with anything below it, and
    /// neither `observed` nor `expected` holds `/wtf/`. `<client dir>/WTF` itself is
    /// allowed.
    pub fn is_private_safe(&self) -> bool {
        let below_wtf = |path: &Path| {
            let components: Vec<Component<'_>> = path.components().collect();
            components
                .iter()
                .rev()
                .skip(1)
                .any(|c| c.as_os_str().eq_ignore_ascii_case("wtf"))
        };
        let names_wtf = |text: &str| text.to_ascii_lowercase().contains("/wtf/");
        !(self.path.as_deref().is_some_and(below_wtf)
            || self.paths.iter().flatten().any(|path| below_wtf(path))
            || self.observed.as_deref().is_some_and(names_wtf)
            || self.expected.as_deref().is_some_and(names_wtf))
    }
}

/// How to address a finding (A.4): the command, or prose when no command would help, the
/// command that explains it, and what a fix would take.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(rename = "DoctorRemediation")]
pub struct Remediation {
    /// The command for display, or the prose when there is no command.
    pub command: String,
    /// The command as argv; `null` for prose.
    pub argv: Option<Vec<String>>,
    pub explain_command: String,
    pub explain_argv: Vec<String>,
    /// `doctor fix` repairs it without `--force`.
    pub auto_fixable: bool,
    /// Its fixer runs only with `--force`.
    pub needs_force: bool,
    /// How many filesystem operations the fix would take; 0 until `doctor fix` plans it.
    pub estimated_actions: u32,
    pub destructive: bool,
}

// ---------------------------------------------------------------------------------------
// Subjects and instance keys.

/// A finding's target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FindingTarget {
    pub id: String,
    pub key: TargetKey,
}

/// Where a finding is: one target, by its key and its current id, or the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceScope {
    Target { id: String, key: TargetKey },
    Machine,
}

/// The broken resource within a scope, when the failure mode can break more than one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discriminator {
    Tx(TxId),
    /// Lexically normalized and absolute.
    Path(PathBuf),
    Folder(FolderName),
    Addon(String),
    Blob([u8; 32]),
}

/// What a finding is about: a scope and at most one resource in it. The one way a
/// detector names an instance (M4-D30).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    scope: InstanceScope,
    discriminator: Option<Discriminator>,
}

impl Subject {
    pub fn target(id: &str, key: TargetKey) -> Self {
        Self {
            scope: InstanceScope::Target {
                id: id.to_owned(),
                key,
            },
            discriminator: None,
        }
    }

    pub const fn machine() -> Self {
        Self {
            scope: InstanceScope::Machine,
            discriminator: None,
        }
    }

    pub fn tx(self, tx: TxId) -> Self {
        self.about(Discriminator::Tx(tx))
    }

    /// Stored lexically normalized: `.`, repeated and trailing separators dropped, and
    /// `..` taking off the component before it.
    pub fn path(self, path: &Path) -> Self {
        self.about(Discriminator::Path(normalize(path)))
    }

    pub fn folder(self, folder: &FolderName) -> Self {
        self.about(Discriminator::Folder(folder.clone()))
    }

    pub fn addon(self, name: &str) -> Self {
        self.about(Discriminator::Addon(name.to_owned()))
    }

    pub fn blob(self, sha256: [u8; 32]) -> Self {
        self.about(Discriminator::Blob(sha256))
    }

    /// A second discriminator replaces the first; no detector should set two.
    fn about(mut self, discriminator: Discriminator) -> Self {
        debug_assert!(
            self.discriminator.is_none(),
            "{self:?} already names a resource; not also {discriminator:?}"
        );
        self.discriminator = Some(discriminator);
        self
    }
}

/// `path` without `.`, repeated or trailing separators, and with each `..` taking off the
/// component before it, by its components alone.
fn normalize(path: &Path) -> PathBuf {
    debug_assert!(path.is_absolute(), "{} is not absolute", path.display());
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A finding's stable identity (M4-D30): `<fm-id>@<target key or machine>`, then
/// `:<kind>=<value>` naming the broken resource. Never the renameable target id, so a
/// rename keeps it; built only by [`key_and_label`], beside the label it goes with.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstanceKey(String);

impl InstanceKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// An instance key's parts, for `doctor explain <instance>` and `--since`. Only the
    /// canonical form reads: a target key in its uppercase form, a lowercase folder, addon
    /// or blob.
    pub fn parse(s: &str) -> Result<ParsedInstance, InstanceParseError> {
        let (fm, rest) = s
            .split_once('@')
            .ok_or_else(|| InstanceParseError::MissingScope(s.to_owned()))?;
        if !is_fm_id(fm) {
            return Err(InstanceParseError::BadFmId(fm.to_owned()));
        }
        let (scope, discriminator) = match rest.split_once(':') {
            Some((scope, discriminator)) => (scope, Some(discriminator)),
            None => (rest, None),
        };
        let scope = if scope == "machine" {
            ParsedScope::Machine
        } else {
            scope
                .parse::<TargetKey>()
                .ok()
                .filter(|key| key.to_string() == scope)
                .map(ParsedScope::Target)
                .ok_or_else(|| InstanceParseError::BadScope(scope.to_owned()))?
        };
        let discriminator = discriminator
            .map(|text| {
                let (kind, value) = text.split_once('=').unwrap_or((text, ""));
                let kind = DiscriminatorKind::ALL
                    .into_iter()
                    .find(|k| k.as_str() == kind)
                    .ok_or_else(|| InstanceParseError::UnknownDiscriminator(kind.to_owned()))?;
                if kind.accepts(value) {
                    Ok((kind, value.to_owned()))
                } else {
                    Err(InstanceParseError::BadValue {
                        kind,
                        value: value.to_owned(),
                    })
                }
            })
            .transpose()?;
        Ok(ParsedInstance {
            fm: fm.to_owned(),
            scope,
            discriminator,
        })
    }
}

impl fmt::Display for InstanceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for InstanceKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl JsonSchema for InstanceKey {
    fn schema_name() -> Cow<'static, str> {
        "InstanceKey".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "`<fm-id>@<target key or machine>`, then `:<kind>=<value>` naming the broken resource"
        })
    }
}

/// A finding's instance key and its label, built together so they always name the same
/// thing. The label is display only (`retail · tx 01M7…`, `retail/DBM`): it follows a
/// rename and is never compared.
fn key_and_label(fm_id: &'static str, subject: &Subject) -> (InstanceKey, String) {
    let (scope, scope_label) = match &subject.scope {
        InstanceScope::Target { id, key } => (key.to_string(), id.as_str()),
        InstanceScope::Machine => ("machine".to_owned(), "machine"),
    };
    let (key_part, label_part) = match &subject.discriminator {
        None => (String::new(), String::new()),
        Some(Discriminator::Tx(tx)) => (format!(":tx={tx}"), format!(" · tx {tx}")),
        Some(Discriminator::Path(path)) => {
            let path = path.to_string_lossy();
            (format!(":path={path}"), format!(" · {path}"))
        }
        Some(Discriminator::Folder(folder)) => (
            format!(":folder={}", folder.key()),
            format!("/{}", folder.as_str()),
        ),
        Some(Discriminator::Addon(name)) => (
            format!(":addon={}", name.to_ascii_lowercase()),
            format!("/{name}"),
        ),
        Some(Discriminator::Blob(sha256)) => {
            let hex = hex(sha256);
            let short = hex.get(..12).unwrap_or(&hex);
            (format!(":blob={hex}"), format!(" · blob {short}"))
        }
    };
    (
        InstanceKey(format!("{fm_id}@{scope}{key_part}")),
        format!("{scope_label}{label_part}"),
    )
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|b| [b >> 4, b & 0x0f])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

/// An instance key read back into its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedInstance {
    pub fm: String,
    pub scope: ParsedScope,
    pub discriminator: Option<(DiscriminatorKind, String)>,
}

impl fmt::Display for ParsedInstance {
    /// The instance key it was read from.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@", self.fm)?;
        match &self.scope {
            ParsedScope::Target(key) => write!(f, "{key}")?,
            ParsedScope::Machine => f.write_str("machine")?,
        }
        match &self.discriminator {
            Some((kind, value)) => write!(f, ":{}={value}", kind.as_str()),
            None => Ok(()),
        }
    }
}

/// An instance key's scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedScope {
    Target(TargetKey),
    Machine,
}

/// The kinds of resource an instance key can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscriminatorKind {
    Tx,
    Path,
    Folder,
    Addon,
    Blob,
}

impl DiscriminatorKind {
    const ALL: [Self; 5] = [Self::Tx, Self::Path, Self::Folder, Self::Addon, Self::Blob];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tx => "tx",
            Self::Path => "path",
            Self::Folder => "folder",
            Self::Addon => "addon",
            Self::Blob => "blob",
        }
    }

    /// Whether `value` is this kind's canonical form.
    fn accepts(self, value: &str) -> bool {
        match self {
            Self::Tx => value
                .parse::<TxId>()
                .is_ok_and(|tx| tx.to_string() == value),
            Self::Path => value.starts_with('/'),
            Self::Folder | Self::Addon => !value.is_empty() && value == value.to_ascii_lowercase(),
            Self::Blob => {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            }
        }
    }
}

/// Why a string is not an instance key, naming the part.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstanceParseError {
    #[error("{0:?} has no scope: an instance key is <fm-id>@<target key or machine>")]
    MissingScope(String),
    #[error("{0:?} is not a failure-mode id (fm-<subsystem>-<slug>)")]
    BadFmId(String),
    #[error("scope {0:?} is neither `machine` nor a target key")]
    BadScope(String),
    #[error("{0:?} is not a resource kind: expected tx, path, folder, addon or blob")]
    UnknownDiscriminator(String),
    #[error("{value:?} is not a {} value", .kind.as_str())]
    BadValue {
        kind: DiscriminatorKind,
        value: String,
    },
}

/// Whether `s` is a failure-mode id, `^fm-[a-z0-9_]+-[a-z0-9-]+$` (M4-D30): `fm-`, the
/// subsystem in snake_case, `-`, and the slug in kebab-case.
pub fn is_fm_id(s: &str) -> bool {
    let Some((subsystem, slug)) = s.strip_prefix("fm-").and_then(|rest| rest.split_once('-'))
    else {
        return false;
    };
    !subsystem.is_empty()
        && subsystem
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'))
        && !slug.is_empty()
        && slug
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// The subsystem part of a failure-mode id: what lies between `fm-` and the next `-`.
pub fn subsystem_part(id: &str) -> Option<&str> {
    id.strip_prefix("fm-")?
        .split_once('-')
        .map(|(subsystem, _)| subsystem)
}

// ---------------------------------------------------------------------------------------
// Findings and observations.

/// One broken instance of one failure mode, as a doctor report carries it (A.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Finding {
    /// The failure mode's id.
    pub id: &'static str,
    pub instance: InstanceKey,
    /// Display only: it follows a rename and is never compared.
    pub label: String,
    pub severity: Severity,
    pub subsystem: Subsystem,
    /// This instance's title.
    pub title: String,
    pub confidence: Confidence,
    /// `null` for a finding about the machine.
    pub target: Option<FindingTarget>,
    /// `null` when it concerns no addon.
    pub addon: Option<String>,
    /// `null` when it concerns no AddOns folder.
    pub folder: Option<String>,
    pub evidence: Evidence,
    pub remediation: Remediation,
}

impl Finding {
    /// A registry row's finding for one observation: the instance key and label from its
    /// subject, the target from its scope, then the remediation, from the finished
    /// finding. The registry is its caller.
    pub(crate) fn build(
        id: &'static str,
        subsystem: Subsystem,
        severity: Severity,
        obs: Observation,
        remediation: impl FnOnce(&Self) -> Remediation,
    ) -> Self {
        debug_assert!(
            obs.evidence.is_private_safe(),
            "{id}: evidence below WTF/: {:?}",
            obs.evidence
        );
        let (instance, label) = key_and_label(id, &obs.subject);
        let target = match obs.subject.scope {
            InstanceScope::Target { id, key } => Some(FindingTarget { id, key }),
            InstanceScope::Machine => None,
        };
        let mut finding = Self {
            id,
            instance,
            label,
            severity,
            subsystem,
            title: obs.title,
            confidence: obs.confidence,
            target,
            addon: obs.addon,
            folder: obs.folder,
            evidence: obs.evidence,
            remediation: Remediation::default(),
        };
        finding.remediation = remediation(&finding);
        finding
    }
}

/// What a detector reports, one per broken instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub subject: Subject,
    pub title: String,
    pub confidence: Confidence,
    pub addon: Option<String>,
    pub folder: Option<String>,
    pub evidence: Evidence,
}

/// A detector's return: what it found, and the limit that stopped it short, if any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detected {
    pub found: Vec<Observation>,
    pub incomplete: Option<Incomplete>,
}

impl Detected {
    /// A detector that ran to its end.
    pub fn complete(found: Vec<Observation>) -> Self {
        Self {
            found,
            incomplete: None,
        }
    }

    /// A detector that stopped when `budget` ran out: what it found so far, and the limit
    /// that stopped it.
    pub fn cut_short(found: Vec<Observation>, budget: &Budget) -> Self {
        Self {
            found,
            incomplete: budget.incomplete(),
        }
    }
}

/// A detector stopped by a limit (M4-D43): what it found stands, and the run reports a
/// `fm-state_files-check-incomplete` finding for it. It holds only what the limit fixes,
/// never an elapsed time or a count, so two runs over one state record the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Incomplete {
    pub limit: IncompleteLimit,
    /// The deadline that passed, for `deadline`.
    pub deadline_ms: Option<u64>,
    /// The entry cap that was reached, for `entries`.
    pub max_entries: Option<u64>,
}

/// The limit that stopped a detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IncompleteLimit {
    Deadline,
    Entries,
    OnlineRequests,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const K: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";
    const T: &str = "01M7A1B2C3D4E5F6G7H8J9K0MN";
    const FM: &str = "fm-state_files-example-mode";
    const PENDING: &str = "fm-concurrency_primitives-journal-pending";

    /// The seven failure modes `status` publishes (README: stable across releases).
    const PUBLISHED: [&str; 7] = [
        "fm-concurrency_primitives-journal-pending",
        "fm-schemas-state-version-newer",
        "fm-state_files-record-folder-missing",
        "fm-external_artifacts-managed-files-modified",
        "fm-external_artifacts-ownership-transferred",
        "fm-userland_state-target-vanished",
        "fm-plugins-widget-version-drift",
    ];

    fn key() -> TargetKey {
        K.parse().unwrap()
    }

    fn tx() -> TxId {
        T.parse().unwrap()
    }

    fn retail() -> Subject {
        Subject::target("retail", key())
    }

    fn key_of(subject: &Subject) -> String {
        key_and_label(FM, subject).0.to_string()
    }

    #[test]
    fn subsystems_are_the_doctor_contracts_thirteen() {
        let names: Vec<&str> = Subsystem::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            [
                "state_files",
                "configs",
                "schemas",
                "caches",
                "sockets",
                "hooks",
                "plugins",
                "secrets",
                "permissions",
                "external_artifacts",
                "concurrency_primitives",
                "network",
                "userland_state",
            ]
        );
        for subsystem in Subsystem::ALL {
            let name = subsystem.as_str();
            assert_eq!(
                serde_json::to_value(subsystem).unwrap(),
                json!(name),
                "{name}"
            );
            assert_eq!(name.parse::<Subsystem>(), Ok(subsystem), "{name}");
            assert_eq!(
                subsystem.applicable(),
                !matches!(name, "sockets" | "hooks"),
                "{name}"
            );
        }
        for name in ["state-files", "StateFiles", ""] {
            let read = name.parse::<Subsystem>();
            assert_eq!(read, Err(UnknownSubsystem(name.to_owned())), "{name:?}");
        }
    }

    #[test]
    fn severity_keeps_its_wire_form_and_order() {
        let all = [Severity::P0, Severity::P1, Severity::P2, Severity::P3];
        assert_eq!(
            serde_json::to_value(all).unwrap(),
            json!(["P0", "P1", "P2", "P3"])
        );
        assert!(all.windows(2).all(|pair| pair[0] < pair[1]), "{all:?}");
        let urgent = |severity: crate::app::issues::Severity| severity <= Severity::P1;
        assert!(!urgent(Severity::P2));
    }

    #[test]
    fn confidence_is_the_errors_enum() {
        let all: [crate::errors::attribute::Confidence; 3] =
            [Confidence::High, Confidence::Medium, Confidence::Low];
        assert_eq!(
            serde_json::to_value(all).unwrap(),
            json!(["high", "medium", "low"])
        );
    }

    #[test]
    fn instance_keys_and_labels_by_case() {
        for (case, subject, instance, label) in key_cases() {
            let (got_instance, got_label) = key_and_label(FM, &subject);
            assert_eq!(
                (got_instance.as_str(), got_label.as_str()),
                (instance.as_str(), label.as_str()),
                "{case}: {subject:?}"
            );
        }
    }

    /// `(case, subject, instance, label)`.
    fn key_cases() -> Vec<(&'static str, Subject, String, String)> {
        let blob = "ab".repeat(32);
        vec![
            (
                "target_only",
                retail(),
                format!("{FM}@{K}"),
                "retail".to_owned(),
            ),
            (
                "target_tx",
                retail().tx(tx()),
                format!("{FM}@{K}:tx={T}"),
                format!("retail · tx {T}"),
            ),
            (
                "machine_only",
                Subject::machine(),
                format!("{FM}@machine"),
                "machine".to_owned(),
            ),
            (
                "machine_tx",
                Subject::machine().tx(tx()),
                format!("{FM}@machine:tx={T}"),
                format!("machine · tx {T}"),
            ),
            (
                "path_normalized",
                retail().path(Path::new("/fixture/a/./b//c/")),
                format!("{FM}@{K}:path=/fixture/a/b/c"),
                "retail · /fixture/a/b/c".to_owned(),
            ),
            (
                "path_parent_resolved",
                retail().path(Path::new("/fixture/a/b/../c")),
                format!("{FM}@{K}:path=/fixture/a/c"),
                "retail · /fixture/a/c".to_owned(),
            ),
            (
                "folder_key",
                retail().folder(&FolderName::new("DBM-Core").unwrap()),
                format!("{FM}@{K}:folder=dbm-core"),
                "retail/DBM-Core".to_owned(),
            ),
            (
                "addon_lowercased",
                retail().addon("DBM"),
                format!("{FM}@{K}:addon=dbm"),
                "retail/DBM".to_owned(),
            ),
            (
                "blob_hex",
                Subject::machine().blob([0xab; 32]),
                format!("{FM}@machine:blob={blob}"),
                "machine · blob abababababab".to_owned(),
            ),
        ]
    }

    #[test]
    fn resources_on_one_target_are_distinct_instances() {
        let other: TxId = "01M7A1B2C3D4E5F6G7H8J9K0MP".parse().unwrap();
        let cases = [
            ("two_txs", retail().tx(tx()), retail().tx(other)),
            (
                "two_paths",
                retail().path(Path::new("/fixture/a")),
                retail().path(Path::new("/fixture/b")),
            ),
            (
                "two_addons",
                retail().addon("DBM"),
                retail().addon("Plater"),
            ),
        ];
        for (case, a, b) in cases {
            assert_ne!(key_of(&a), key_of(&b), "{case}: {a:?} and {b:?}");
            assert_eq!(key_of(&a), key_of(&a.clone()), "{case}: {a:?} twice");
        }
    }

    #[test]
    fn a_rename_changes_the_label_not_the_instance() {
        let before = key_and_label(FM, &retail().tx(tx()));
        let after = key_and_label(FM, &Subject::target("main", key()).tx(tx()));
        assert_eq!(before.0, after.0);
        assert_eq!(before.1, format!("retail · tx {T}"));
        assert_eq!(after.1, format!("main · tx {T}"));
    }

    #[test]
    fn instance_keys_parse_back() {
        for (case, subject, instance, _) in key_cases() {
            let parsed = InstanceKey::parse(&instance);
            assert!(parsed.is_ok(), "{case}: {instance} -> {parsed:?}");
            let parsed = parsed.unwrap();
            assert_eq!(parsed.fm, FM, "{case}: {instance}");
            let scope = match subject.scope {
                InstanceScope::Target { key, .. } => ParsedScope::Target(key),
                InstanceScope::Machine => ParsedScope::Machine,
            };
            assert_eq!(parsed.scope, scope, "{case}: {instance}");
            let kind = parsed.discriminator.as_ref().map(|(kind, _)| *kind);
            let expected = subject.discriminator.as_ref().map(|d| match d {
                Discriminator::Tx(_) => DiscriminatorKind::Tx,
                Discriminator::Path(_) => DiscriminatorKind::Path,
                Discriminator::Folder(_) => DiscriminatorKind::Folder,
                Discriminator::Addon(_) => DiscriminatorKind::Addon,
                Discriminator::Blob(_) => DiscriminatorKind::Blob,
            });
            assert_eq!(kind, expected, "{case}: {instance}");
            assert_eq!(parsed.to_string(), instance, "{case}");
        }
    }

    #[test]
    fn malformed_instance_keys_are_refused_by_part() {
        let bad = |kind, value: &str| InstanceParseError::BadValue {
            kind,
            value: value.to_owned(),
        };
        let cases = [
            (
                "no_scope",
                "fm-a-b",
                InstanceParseError::MissingScope("fm-a-b".to_owned()),
            ),
            (
                "uppercase_fm",
                "fm-A-b@machine",
                InstanceParseError::BadFmId("fm-A-b".to_owned()),
            ),
            (
                "an_id_is_not_a_key",
                "fm-a-b@retail",
                InstanceParseError::BadScope("retail".to_owned()),
            ),
            (
                "a_lowercase_key",
                &format!("fm-a-b@{}", K.to_ascii_lowercase()),
                InstanceParseError::BadScope(K.to_ascii_lowercase()),
            ),
            (
                "unknown_kind",
                "fm-a-b@machine:color=red",
                InstanceParseError::UnknownDiscriminator("color".to_owned()),
            ),
            (
                "bad_tx",
                "fm-a-b@machine:tx=nope",
                bad(DiscriminatorKind::Tx, "nope"),
            ),
            (
                "relative_path",
                "fm-a-b@machine:path=relative",
                bad(DiscriminatorKind::Path, "relative"),
            ),
            (
                "uppercase_addon",
                "fm-a-b@machine:addon=DBM",
                bad(DiscriminatorKind::Addon, "DBM"),
            ),
            (
                "empty_folder",
                "fm-a-b@machine:folder=",
                bad(DiscriminatorKind::Folder, ""),
            ),
            (
                "short_blob",
                "fm-a-b@machine:blob=abc",
                bad(DiscriminatorKind::Blob, "abc"),
            ),
        ];
        for (case, text, expected) in cases {
            let parsed = InstanceKey::parse(text);
            assert_eq!(parsed, Err(expected), "{case}: {text}");
        }
    }

    #[test]
    fn fm_ids_by_case() {
        let accepted = PUBLISHED
            .iter()
            .copied()
            .chain(["fm-state_files-check-incomplete", "fm-a-b-"]);
        for id in accepted {
            assert!(is_fm_id(id), "{id} refused");
        }
        for (case, id) in [
            ("no_slug", "fm-a"),
            ("empty_subsystem", "fm--b"),
            ("empty_slug", "fm-a-"),
            ("underscore_in_slug", "fm-a-b_c"),
            ("uppercase", "fm-State-b"),
            ("wrong_prefix", "xm-a-b"),
            ("a_space", "fm-a-b c"),
            ("empty", ""),
        ] {
            assert!(!is_fm_id(id), "{case}: {id:?} accepted");
        }
        assert_eq!(
            subsystem_part("fm-state_files-record-folder-missing"),
            Some("state_files")
        );
        for id in PUBLISHED {
            let part = subsystem_part(id).unwrap_or_default();
            assert!(part.parse::<Subsystem>().is_ok(), "{id}: {part}");
        }
    }

    #[test]
    fn evidence_serializes_only_what_it_holds() {
        let text = |evidence: &Evidence| serde_json::to_string(evidence).unwrap();
        assert_eq!(text(&Evidence::default()), "{}");
        let journal = Evidence {
            tx: Some(tx()),
            reason: Some("journal_too_new"),
            ..Evidence::default()
        };
        assert_eq!(
            text(&journal),
            format!(r#"{{"tx":"{T}","reason":"journal_too_new"}}"#)
        );
        for (bits, wire) in [
            (0o600, r#"{"mode":"0600"}"#),
            (0o4755, r#"{"mode":"4755"}"#),
        ] {
            let evidence = Evidence {
                mode: Some(bits),
                ..Evidence::default()
            };
            assert_eq!(text(&evidence), wire, "{bits:o}");
        }
        let two = Evidence {
            paths: Some(vec!["/fixture/b".into(), "/fixture/a".into()]),
            count: Some(2),
            path: Some("/fixture/c".into()),
            ..Evidence::default()
        };
        assert_eq!(
            text(&two),
            r#"{"path":"/fixture/c","paths":["/fixture/b","/fixture/a"],"count":2}"#
        );
    }

    #[test]
    fn evidence_below_wtf_is_not_private_safe() {
        let at = |path: &str| Evidence {
            path: Some(path.into()),
            ..Evidence::default()
        };
        let safe = [
            (
                "a_state_file",
                at(&format!("/fixture/state/omaddons/targets/{K}.json")),
            ),
            ("wtf_itself", at("/fixture/client/WTF")),
            (
                "a_folder_named_like_it",
                at("/fixture/client/Interface/AddOns/WTFHelper"),
            ),
            ("nothing", Evidence::default()),
        ];
        for (case, evidence) in safe {
            assert!(evidence.is_private_safe(), "{case}: {evidence:?}");
        }
        let unsafe_cases = [
            (
                "a_savedvariables_file",
                at("/fixture/client/WTF/Account/A/SavedVariables/x.lua"),
            ),
            ("lowercase_wtf", at("/fixture/client/wtf/Config.wtf")),
            (
                "one_bad_path_among_good",
                Evidence {
                    paths: Some(vec![
                        "/fixture/client/Interface".into(),
                        "/fixture/client/WTF/Account".into(),
                    ]),
                    ..Evidence::default()
                },
            ),
            (
                "observed_text",
                Evidence {
                    observed: Some("/fixture/client/WTF/Account".to_owned()),
                    ..Evidence::default()
                },
            ),
            (
                "expected_text",
                Evidence {
                    expected: Some("C:/Games/wtf/account".to_owned()),
                    ..Evidence::default()
                },
            ),
        ];
        for (case, evidence) in unsafe_cases {
            assert!(!evidence.is_private_safe(), "{case}: {evidence:?}");
        }
    }

    /// `omaddons doctor explain <fm> [-t <target id>]`, as the registry will build it.
    fn explain(finding: &Finding) -> (String, Vec<String>) {
        let mut argv: Vec<String> = ["omaddons", "doctor", "explain", finding.id]
            .map(str::to_owned)
            .to_vec();
        if let Some(target) = &finding.target {
            argv.extend(["-t".to_owned(), target.id.clone()]);
        }
        (argv.join(" "), argv)
    }

    /// The journal-pending finding of the parent bead's example, and a machine finding.
    fn report_findings() -> [Finding; 2] {
        let journal = Observation {
            subject: retail().tx(tx()),
            title: "An interrupted change needs manual repair".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence {
                tx: Some(tx()),
                reason: Some("journal_unreadable"),
                ..Evidence::default()
            },
        };
        let target_finding = Finding::build(
            PENDING,
            Subsystem::ConcurrencyPrimitives,
            Severity::P0,
            journal,
            |finding| {
                let (explain_command, explain_argv) = explain(finding);
                Remediation {
                    command: "see \"journal_pending that does not clear\" under Troubleshooting in the README"
                        .to_owned(),
                    argv: None,
                    explain_command,
                    explain_argv,
                    ..Remediation::default()
                }
            },
        );
        let widget = Observation {
            subject: Subject::machine(),
            title: "The bar widget is from another omaddons build".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence: Evidence {
                observed: Some("0.1.0+aaaa".to_owned()),
                expected: Some("0.1.0+bbbb".to_owned()),
                ..Evidence::default()
            },
        };
        let machine_finding = Finding::build(
            "fm-plugins-widget-version-drift",
            Subsystem::Plugins,
            Severity::P3,
            widget,
            |finding| {
                let (explain_command, explain_argv) = explain(finding);
                Remediation {
                    command: "omaddons omarchy install".to_owned(),
                    argv: Some(
                        ["omaddons", "omarchy", "install"]
                            .map(str::to_owned)
                            .to_vec(),
                    ),
                    explain_command,
                    explain_argv,
                    ..Remediation::default()
                }
            },
        );
        [target_finding, machine_finding]
    }

    #[test]
    fn a_finding_serializes_in_report_order() {
        let findings = report_findings();
        insta::assert_json_snapshot!("finding_report_shape", findings);
        let text = serde_json::to_string(&findings[1]).unwrap();
        let order = [
            "\"id\"",
            "\"instance\"",
            "\"label\"",
            "\"severity\"",
            "\"subsystem\"",
            "\"title\"",
            "\"confidence\"",
            "\"target\":null",
            "\"addon\":null",
            "\"folder\":null",
            "\"evidence\"",
            "\"remediation\":{\"command\"",
            "\"argv\"",
            "\"explain_command\"",
            "\"explain_argv\"",
            "\"auto_fixable\"",
            "\"needs_force\"",
            "\"estimated_actions\"",
            "\"destructive\"",
        ];
        let positions: Vec<Option<usize>> = order.iter().map(|key| text.find(key)).collect();
        assert!(
            positions.iter().all(Option::is_some) && positions.windows(2).all(|p| p[0] < p[1]),
            "{order:?} in {text}: {positions:?}"
        );
    }

    #[test]
    fn the_doctor_remediation_schema_has_its_own_name() {
        assert_eq!(
            <Remediation as JsonSchema>::schema_name(),
            "DoctorRemediation"
        );
        assert_eq!(
            <crate::app::issues::Remediation as JsonSchema>::schema_name(),
            "Remediation"
        );
    }

    #[test]
    fn build_runs_the_remediation_on_the_finished_finding() {
        let mut seen = None;
        let finding = Finding::build(
            PENDING,
            Subsystem::ConcurrencyPrimitives,
            Severity::P0,
            Observation {
                subject: retail().tx(tx()),
                title: "t".to_owned(),
                confidence: Confidence::Medium,
                addon: Some("DBM".to_owned()),
                folder: None,
                evidence: Evidence::default(),
            },
            |finding| {
                seen = Some((finding.instance.clone(), finding.target.clone()));
                Remediation::default()
            },
        );
        assert_eq!(
            seen,
            Some((
                finding.instance.clone(),
                Some(FindingTarget {
                    id: "retail".to_owned(),
                    key: key(),
                })
            ))
        );
        assert_eq!(finding.instance.as_str(), format!("{PENDING}@{K}:tx={T}"));
        assert_eq!(finding.addon.as_deref(), Some("DBM"));
    }
}
