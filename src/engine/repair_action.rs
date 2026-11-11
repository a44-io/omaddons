//! The repair action model (E1, M4-D31): the data a doctor repair is made of, and the pure
//! checks of it. Nothing here touches the filesystem.
//!
//! - A **fix** ([`Fix`]) is what a fixer returns for one finding, at the level a person
//!   reads it: a write, a chmod, a quarantine, a flatten, a suspension or a delegation.
//! - A **primitive** ([`Primitive`]) is exactly one filesystem operation: `write_file`,
//!   `chmod` or `rename_noreplace`. E5 lowers every fix of a run into one immutable,
//!   ordered list of them before anything commits, so a flatten becomes two or three
//!   renames and no step hides more than one.
//! - A **step** ([`Step`]) is one primitive as the repair journal records it before it
//!   runs: its sequence number, the fix and failure mode it serves, its endpoint predicates
//!   (what must be at each of its paths before and after it, absence included) and, for a
//!   set-aside, the guards of its suspension (M4-D38).
//!
//! E5 lowers, journals, executes and recovers; E7 inverts for undo. Every path is
//! absolute. The wire form is the repair journal's `prepared.fixes[]` ([`FixEntry`]) and
//! `primitives[]` ([`Step`]), and history's `repairs[]`, whose
//! [`RepairEntry`](crate::state::history::RepairEntry) writes the same JSON. Field order
//! is deterministic but not contract (M4-D56), and no key is refused on read, so a newer
//! minor's additive keys keep reading.

use std::borrow::Cow;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use schemars::JsonSchema;
#[cfg(feature = "schemas")]
use schemars::{Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::fsx::sha256_from_hex;
use crate::model::{TargetKey, TxId};

// ---------------------------------------------------------------------------------------
// Scalars.

/// Why a wire scalar did not read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {what} {input:?}: expected {expected}")]
pub struct ParseError {
    pub what: &'static str,
    pub expected: &'static str,
    pub input: String,
}

/// Written as its `Display`, read through its `FromStr`, published as a string matching
/// `$pattern`.
macro_rules! string_wire {
    ($name:ident, $pattern:literal) => {
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(de::Error::custom)
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
                    "pattern": $pattern
                })
            }
        }
    };
}

/// A permission mode: the low twelve bits, written as exactly four octal digits
/// (`"0600"`, `"4755"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mode(u32);

impl Mode {
    /// `None` above `0o7777`.
    pub const fn new(bits: u32) -> Option<Self> {
        if bits <= 0o7777 {
            Some(Self(bits))
        } else {
            None
        }
    }

    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04o}", self.0)
    }
}

impl FromStr for Mode {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let octal = s.len() == 4 && s.bytes().all(|b| matches!(b, b'0'..=b'7'));
        octal
            .then(|| u32::from_str_radix(s, 8).ok())
            .flatten()
            .and_then(Self::new)
            .ok_or_else(|| ParseError {
                what: "mode",
                expected: "four octal digits",
                input: s.to_owned(),
            })
    }
}

string_wire!(Mode, "^[0-7]{4}$");

/// A path's identity without its device, which a rename on one filesystem keeps: its inode
/// number and, where the filesystem records one, its birth time in nanoseconds. The device
/// is left out because it does not survive a mount (see [`crate::fsx::FileId`]).
///
/// Written as decimal strings, `{"ino":"4242","born_ns":"1767225600000000000"}`, because a
/// nanosecond birth time exceeds the integers `jq` and JavaScript hold exactly; `born_ns`
/// is omitted when the filesystem records no birth time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "IdentityWire", try_from = "IdentityWire")]
pub struct Identity {
    pub ino: u64,
    pub born_ns: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct IdentityWire {
    ino: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    born_ns: Option<String>,
}

impl From<Identity> for IdentityWire {
    fn from(identity: Identity) -> Self {
        Self {
            ino: identity.ino.to_string(),
            born_ns: identity.born_ns.map(|ns| ns.to_string()),
        }
    }
}

impl TryFrom<IdentityWire> for Identity {
    type Error = ParseError;

    fn try_from(wire: IdentityWire) -> Result<Self, Self::Error> {
        let ino = decimal(&wire.ino, false).ok_or_else(|| ParseError {
            what: "ino",
            expected: "a decimal string",
            input: wire.ino.clone(),
        })?;
        let born_ns = match wire.born_ns {
            None => None,
            Some(text) => Some(decimal(&text, true).ok_or(ParseError {
                what: "born_ns",
                expected: "a decimal string",
                input: text,
            })?),
        };
        Ok(Self { ino, born_ns })
    }
}

/// Digits only (after one `-` when `signed`), so `+5`, `" 5"` and `5.0` never read.
fn decimal<T: FromStr>(text: &str, signed: bool) -> Option<T> {
    let digits = if signed {
        text.strip_prefix('-').unwrap_or(text)
    } else {
        text
    };
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

#[cfg(feature = "schemas")]
impl JsonSchema for Identity {
    fn schema_name() -> Cow<'static, str> {
        "Identity".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "properties": {
                "ino": { "type": "string", "pattern": "^[0-9]+$" },
                "born_ns": {
                    "description": "The birth time in nanoseconds; omitted when the filesystem records none.",
                    "type": "string",
                    "pattern": "^-?[0-9]+$"
                }
            },
            "required": ["ino"]
        })
    }
}

/// A file's content digest, written `sha256:<64 lowercase hex>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256(pub [u8; 32]);

/// A directory's stamp digest (M3-D25: a stat digest of every entry below it, no
/// content), written `stamp:sha256:<64 lowercase hex>`. That is the text of
/// [`StampDigest`](crate::fsx::StampDigest)'s `Display`, so one converts with
/// `Stamp::from_str(&digest.to_string())`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Stamp(pub [u8; 32]);

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", hex(&self.0))
    }
}

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stamp:sha256:{}", hex(&self.0))
    }
}

impl FromStr for Sha256 {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        digest(s, "sha256:").map(Self).ok_or_else(|| ParseError {
            what: "content digest",
            expected: "sha256: and 64 lowercase hex digits",
            input: s.to_owned(),
        })
    }
}

impl FromStr for Stamp {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        digest(s, "stamp:sha256:")
            .map(Self)
            .ok_or_else(|| ParseError {
                what: "stamp digest",
                expected: "stamp:sha256: and 64 lowercase hex digits",
                input: s.to_owned(),
            })
    }
}

string_wire!(Sha256, "^sha256:[0-9a-f]{64}$");
string_wire!(Stamp, "^stamp:sha256:[0-9a-f]{64}$");

/// The 32 bytes after `prefix`, from exactly 64 lowercase hex digits.
fn digest(text: &str, prefix: &str) -> Option<[u8; 32]> {
    let hex = text.strip_prefix(prefix)?;
    hex.bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        .then(|| sha256_from_hex(hex))
        .flatten()
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|b| [b >> 4, b & 0x0f])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

// ---------------------------------------------------------------------------------------
// Predicates.

/// One path's predicate: what must be there, or that nothing is. A part that is absent is
/// not checked. One variant per kind, so a content digest on a directory or a stamp on a
/// file cannot be expressed.
///
/// Which part a step records where (§6.14.1): the kind for every present endpoint;
/// `identity` for a rename's source, its destination afterwards, and a chmod's file before
/// and after; `sha256` for a write's `pre` when it overwrites and its `post`; `mode` for a
/// chmod's `pre` and `post` and a write's `post`; `stamp` for a directory that is renamed;
/// `link_target` for a symlink that is renamed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Endpoint {
    Absent,
    File {
        /// Omitted when the step does not check it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<Identity>,
        /// Omitted when the step does not check it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<Sha256>,
        /// Omitted when the step does not check it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<Mode>,
    },
    Dir {
        /// Omitted when the step does not check it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<Identity>,
        /// Omitted when the step does not check it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stamp: Option<Stamp>,
    },
    Symlink {
        /// Omitted when the step does not check it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<Identity>,
        /// As the link holds it, so it may be relative. Omitted when the step does not
        /// check it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link_target: Option<PathBuf>,
    },
}

impl Endpoint {
    /// Its wire `kind`.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::File { .. } => "file",
            Self::Dir { .. } => "dir",
            Self::Symlink { .. } => "symlink",
        }
    }

    /// The identity it records, if any.
    pub const fn identity(&self) -> Option<Identity> {
        match self {
            Self::Absent => None,
            Self::File { identity, .. }
            | Self::Dir { identity, .. }
            | Self::Symlink { identity, .. } => *identity,
        }
    }
}

/// A step's `pre` or `post`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Endpoints {
    /// A write's or a chmod's: the predicate of its one path.
    One(Endpoint),
    /// A rename's: its source (`path`) and its destination (`to`).
    Pair { path: Endpoint, to: Endpoint },
}

// ---------------------------------------------------------------------------------------
// Primitives and steps.

/// Exactly one filesystem operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op")]
pub enum Primitive {
    /// Writes `path` whole with this mode. The content's digest is the step's `post`.
    #[serde(rename = "write_file")]
    WriteFile {
        path: PathBuf,
        mode: Mode,
        /// Where the file it overwrites is kept: reserved before the write, omitted when
        /// the write creates the file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backup: Option<PathBuf>,
    },
    #[serde(rename = "chmod")]
    Chmod { path: PathBuf, mode: Mode },
    /// `renameat2(RENAME_NOREPLACE)` from `path` to `to`.
    #[serde(rename = "rename_noreplace")]
    RenameNoReplace {
        #[serde(rename = "path")]
        from: PathBuf,
        to: PathBuf,
        /// The rename that reactivates a set-aside journal, which an undo runs last
        /// (M4-D38). Omitted when false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        activation: bool,
    },
}

impl Primitive {
    /// Its wire `op`.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::WriteFile { .. } => "write_file",
            Self::Chmod { .. } => "chmod",
            Self::RenameNoReplace { .. } => "rename_noreplace",
        }
    }

    /// The path it acts on (a rename's source).
    pub fn path(&self) -> &Path {
        match self {
            Self::WriteFile { path, .. } | Self::Chmod { path, .. } => path,
            Self::RenameNoReplace { from, .. } => from,
        }
    }
}

/// One primitive as the repair journal's `prepared.primitives[]` records it, written ahead
/// of it as its own `step` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    /// 1-based and contiguous over the run.
    pub seq: u32,
    /// The fix it lowers, 1-based in the journal's `fixes[]`.
    pub fix: u32,
    /// That fix's failure mode. A string a newer build may have named, so recovery still
    /// reads its journal; fixers borrow their row's id.
    pub fm: Cow<'static, str>,
    #[serde(flatten)]
    pub op: Primitive,
    pub pre: Endpoints,
    pub post: Endpoints,
    /// A set-aside's suspension manifest (M4-D38), on a `rename_noreplace` only. Omitted
    /// when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guards: Vec<Guard>,
}

impl Step {
    /// A write's content digest, which lives in its `post` and nowhere else.
    pub fn write_digest(&self) -> Option<&Sha256> {
        match (&self.op, &self.post) {
            (
                Primitive::WriteFile { .. },
                Endpoints::One(Endpoint::File {
                    sha256: Some(sha256),
                    ..
                }),
            ) => Some(sha256),
            _ => None,
        }
    }

    /// The paths it changes: its path, and a rename's destination.
    pub fn touched(&self) -> Vec<&Path> {
        match &self.op {
            Primitive::RenameNoReplace { from, to, .. } => vec![from, to],
            op => vec![op.path()],
        }
    }

    /// Every path it acts on or reserves: its path, a rename's destination, a write's
    /// backup and an `epoch` guard's folder.
    fn reserved(&self) -> impl Iterator<Item = &Path> {
        let backup = match &self.op {
            Primitive::WriteFile {
                backup: Some(backup),
                ..
            } => Some(backup.as_path()),
            _ => None,
        };
        let epochs = self.guards.iter().filter_map(|guard| match guard {
            Guard::Epoch { path, .. } => Some(path.as_path()),
            _ => None,
        });
        self.touched().into_iter().chain(backup).chain(epochs)
    }

    /// Whether it is well formed on its own: its paths, its endpoints for its op, and
    /// guards only on a rename.
    pub fn validate(&self) -> Result<(), StepError> {
        let seq = self.seq;
        for path in self.reserved() {
            check_path(seq, path)?;
        }
        if !self.guards.is_empty() && !matches!(self.op, Primitive::RenameNoReplace { .. }) {
            return Err(StepError::GuardsOffARename {
                seq,
                op: self.op.name(),
            });
        }
        match &self.op {
            Primitive::WriteFile { mode, backup, .. } => {
                self.validate_write(*mode, backup.is_some())
            }
            Primitive::Chmod { mode, .. } => self.validate_chmod(*mode),
            Primitive::RenameNoReplace { .. } => self.validate_rename(),
        }
    }

    fn validate_write(&self, mode: Mode, backup: bool) -> Result<(), StepError> {
        let seq = self.seq;
        let (Endpoints::One(pre), Endpoints::One(post)) = (&self.pre, &self.post) else {
            return Err(StepError::PairOffARename {
                seq,
                op: "write_file",
            });
        };
        match post {
            Endpoint::File { sha256: None, .. } => {
                return Err(StepError::WritePostWithoutSha256 { seq });
            }
            Endpoint::File { mode: post, .. } if *post != Some(mode) => {
                return Err(StepError::PostModeDiffers {
                    seq,
                    op: "write_file",
                    mode,
                    post: *post,
                });
            }
            Endpoint::File { .. } => {}
            _ => return Err(StepError::WritePostNotAFile { seq }),
        }
        match (pre, backup) {
            (Endpoint::Absent, false)
            | (
                Endpoint::File {
                    sha256: Some(_), ..
                },
                true,
            ) => Ok(()),
            (Endpoint::Absent, true) => Err(StepError::CreateWithBackup { seq }),
            (
                Endpoint::File {
                    sha256: Some(_), ..
                },
                false,
            ) => Err(StepError::OverwriteWithoutBackup { seq }),
            _ => Err(StepError::WritePre { seq }),
        }
    }

    fn validate_chmod(&self, mode: Mode) -> Result<(), StepError> {
        let seq = self.seq;
        let (Endpoints::One(pre), Endpoints::One(post)) = (&self.pre, &self.post) else {
            return Err(StepError::PairOffARename { seq, op: "chmod" });
        };
        let (
            Endpoint::File {
                identity: pre_identity,
                mode: pre_mode,
                ..
            },
            Endpoint::File {
                identity: post_identity,
                mode: post_mode,
                ..
            },
        ) = (pre, post)
        else {
            return Err(StepError::ChmodNotAFile { seq });
        };
        if pre_identity.is_none() || post_identity != pre_identity {
            return Err(StepError::ChmodIdentityChanged { seq });
        }
        if *post_mode != Some(mode) {
            return Err(StepError::PostModeDiffers {
                seq,
                op: "chmod",
                mode,
                post: *post_mode,
            });
        }
        match pre_mode {
            None => Err(StepError::ChmodPreWithoutMode { seq }),
            Some(pre_mode) if *pre_mode == mode => Err(StepError::ChmodToTheSameMode { seq }),
            Some(_) => Ok(()),
        }
    }

    fn validate_rename(&self) -> Result<(), StepError> {
        let seq = self.seq;
        let (
            Endpoints::Pair {
                path: source,
                to: destination,
            },
            Endpoints::Pair {
                path: source_after,
                to: destination_after,
            },
        ) = (&self.pre, &self.post)
        else {
            return Err(StepError::RenameWithOneEndpoint { seq });
        };
        if *source == Endpoint::Absent {
            return Err(StepError::RenameSourceAbsentBefore { seq });
        }
        let Some(identity) = source.identity() else {
            return Err(StepError::RenameSourceWithoutIdentity { seq });
        };
        if *destination != Endpoint::Absent {
            return Err(StepError::RenameDestinationNotAbsentBefore { seq });
        }
        if *source_after != Endpoint::Absent {
            return Err(StepError::RenameSourceNotAbsentAfter { seq });
        }
        if destination_after.kind() != source.kind() {
            return Err(StepError::RenameKindChanged {
                seq,
                before: source.kind(),
                after: destination_after.kind(),
            });
        }
        if destination_after.identity() != Some(identity) {
            return Err(StepError::RenameIdentityChanged { seq });
        }
        match source {
            Endpoint::Dir { stamp: None, .. } => Err(StepError::DirRenameWithoutStamp { seq }),
            Endpoint::Symlink {
                link_target: None, ..
            } => Err(StepError::SymlinkRenameWithoutLinkTarget { seq }),
            _ => Ok(()),
        }
    }
}

/// Absolute, valid UTF-8 and free of `..`: what every path a step names must be. Lowering
/// asks it of a fix's own paths before it resolves them, which would hide a `..`.
pub(crate) fn check_path(seq: u32, path: &Path) -> Result<(), StepError> {
    let path_buf = || path.to_path_buf();
    if path.to_str().is_none() {
        return Err(StepError::NotUtf8 {
            seq,
            path: path_buf(),
        });
    }
    if !path.is_absolute() {
        return Err(StepError::RelativePath {
            seq,
            path: path_buf(),
        });
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(StepError::ParentComponent {
            seq,
            path: path_buf(),
        });
    }
    Ok(())
}

/// Whether a run's steps are well formed together: `seq` runs 1, 2, 3 … without a gap,
/// each `fix` names one of `fixes` and carries its failure mode, and each step validates.
pub fn validate_steps(fixes: &[FixEntry], steps: &[Step]) -> Result<(), StepError> {
    for (expected, step) in (1..).zip(steps) {
        let seq = step.seq;
        if seq != expected {
            return Err(StepError::SeqOutOfOrder { seq, expected });
        }
        let entry = (step.fix as usize)
            .checked_sub(1)
            .and_then(|index| fixes.get(index))
            .ok_or(StepError::FixOutOfRange {
                seq,
                fix: step.fix,
                fixes: fixes.len(),
            })?;
        // ubs:ignore — failure-mode ids, not secrets
        if entry.fm != step.fm {
            return Err(StepError::FmDiffers {
                seq,
                fm: step.fm.to_string(),
                fix_fm: entry.fm.to_string(),
            });
        }
        step.validate()?;
    }
    Ok(())
}

/// Why a step, or a run's list of steps, is not well formed. Every variant names the step.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StepError {
    #[error("step {seq}: {} is not absolute", .path.display())]
    RelativePath { seq: u32, path: PathBuf },
    #[error("step {seq}: {} has a `..` component", .path.display())]
    ParentComponent { seq: u32, path: PathBuf },
    #[error("step {seq}: {} is not valid UTF-8", .path.display())]
    NotUtf8 { seq: u32, path: PathBuf },
    #[error("step {seq}: a {op} records one endpoint, not a source and a destination")]
    PairOffARename { seq: u32, op: &'static str },
    #[error("step {seq}: a rename records its source and its destination, not one endpoint")]
    RenameWithOneEndpoint { seq: u32 },
    #[error("step {seq}: a write's post must be a file")]
    WritePostNotAFile { seq: u32 },
    #[error("step {seq}: a write's post must carry the content's sha256")]
    WritePostWithoutSha256 { seq: u32 },
    #[error("step {seq}: a {op}'s post must carry the mode it sets ({mode}), not {post:?}")]
    PostModeDiffers {
        seq: u32,
        op: &'static str,
        mode: Mode,
        post: Option<Mode>,
    },
    #[error("step {seq}: a write's pre must be absent, or a file with its sha256")]
    WritePre { seq: u32 },
    #[error("step {seq}: a write that overwrites a file must reserve its backup")]
    OverwriteWithoutBackup { seq: u32 },
    #[error("step {seq}: a write that creates its file has nothing to back up")]
    CreateWithBackup { seq: u32 },
    #[error("step {seq}: a chmod's pre and post must be files")]
    ChmodNotAFile { seq: u32 },
    #[error("step {seq}: a chmod's pre and post must carry the same identity")]
    ChmodIdentityChanged { seq: u32 },
    #[error("step {seq}: a chmod's pre must carry the mode it replaces")]
    ChmodPreWithoutMode { seq: u32 },
    #[error("step {seq}: a chmod must change the mode")]
    ChmodToTheSameMode { seq: u32 },
    #[error("step {seq}: a rename's source must be present before it")]
    RenameSourceAbsentBefore { seq: u32 },
    #[error("step {seq}: a rename's source must carry its identity")]
    RenameSourceWithoutIdentity { seq: u32 },
    #[error("step {seq}: a rename's destination must be absent before it")]
    RenameDestinationNotAbsentBefore { seq: u32 },
    #[error("step {seq}: a rename's source must be absent after it")]
    RenameSourceNotAbsentAfter { seq: u32 },
    #[error("step {seq}: a rename keeps its kind ({before}), not {after}")]
    RenameKindChanged {
        seq: u32,
        before: &'static str,
        after: &'static str,
    },
    #[error("step {seq}: a rename's destination after it must carry the source's identity")]
    RenameIdentityChanged { seq: u32 },
    #[error("step {seq}: a renamed directory must carry its stamp")]
    DirRenameWithoutStamp { seq: u32 },
    #[error("step {seq}: a renamed symlink must carry its link target")]
    SymlinkRenameWithoutLinkTarget { seq: u32 },
    #[error("step {seq}: guards belong on a rename, not a {op}")]
    GuardsOffARename { seq: u32, op: &'static str },
    #[error("step {seq}: expected step {expected}; steps run 1, 2, 3 … without a gap")]
    SeqOutOfOrder { seq: u32, expected: u32 },
    #[error("step {seq}: fix {fix} is not one of the run's {fixes} fixes")]
    FixOutOfRange { seq: u32, fix: u32, fixes: usize },
    #[error("step {seq}: failure mode {fm} is not its fix's ({fix_fm})")]
    FmDiffers {
        seq: u32,
        fm: String,
        fix_fm: String,
    },
}

// ---------------------------------------------------------------------------------------
// Guards, fixes and their companions.

/// One condition of a set-aside's suspension manifest (M4-D38), recorded when the journal
/// was set aside and checked before its undo reactivates it: a change since refuses the
/// undo (`undo_conflict`, `changed_since_suspension`). `engine::repair::guard` observes
/// and evaluates them.
///
/// What it recorded may be that nothing was there: a marker or a folder that was absent
/// omits its digest, and its later appearance is a change like any other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Guard {
    /// The target's record generation.
    Generation { target: TargetKey, generation: u64 },
    /// The digest of the target's `<meta>/target.json`.
    Marker {
        target: TargetKey,
        /// Omitted when the client had no marker.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<Sha256>,
    },
    /// The live stamp of a folder the suspended journal would touch on recovery.
    Epoch {
        path: PathBuf,
        /// Omitted when the folder was absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stamp: Option<Stamp>,
    },
    /// One stamp over a client's whole `WTF/`, never a path below it.
    WtfEpoch {
        target: TargetKey,
        /// Omitted when the client had no `WTF/`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stamp: Option<Stamp>,
    },
}

/// What kind of fix it is: `prepared.fixes[].kind`, and capabilities' `ops`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FixKind {
    Write,
    Chmod,
    Quarantine,
    Flatten,
    Suspend,
    Delegate,
}

/// One fix as the repair journal's `prepared.fixes[]` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FixEntry {
    /// 1-based in the run.
    pub fix: u32,
    /// The failure mode it repairs.
    pub fm: Cow<'static, str>,
    pub kind: FixKind,
}

/// What a fixer returns for one finding, before E5 lowers it into primitives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fix {
    /// Writes `path` whole: one `write_file`.
    Write {
        path: PathBuf,
        content: Vec<u8>,
        mode: Mode,
    },
    /// One `chmod`.
    Chmod { path: PathBuf, mode: Mode },
    /// Moves `path` into the quarantine of `root`'s filesystem: one `rename_noreplace`.
    Quarantine { path: PathBuf, root: QuarantineRoot },
    /// Lifts a nested addon folder out of its wrapper: `inner` (`AddOns/X/Y`) to
    /// `AddOns/Y`, then `outer` (`AddOns/X`) into the client's quarantine. When the inner
    /// name equals the wrapper's, E5 lowers three renames through
    /// `AddOns/.omaddons-flatten-<tx>`.
    Flatten { inner: PathBuf, outer: PathBuf },
    /// Sets a stuck journal's artifacts aside, guarded (M4-D38): one `rename_noreplace`
    /// per artifact.
    Suspend { journal: JournalSet },
    /// Has the engine's own recovery settle a journal (M4-D37): no primitive.
    Delegate { what: Delegation },
    /// Has the engine's own snapshot retention prune what it would prune now on one
    /// client, a delegation as `Delegate` is: no primitive.
    Retain { what: RetentionPrune },
}

/// Snapshots the doctor has engine retention prune (`doctor fix`), as its preview lists
/// them (`prunes_planned`) and its token selects them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPrune {
    pub target: TargetKey,
    pub client_dir: PathBuf,
    /// The transactions whose snapshots go, in history order.
    pub txs: Vec<TxId>,
    /// The policy that judged them (config's limits), which judges them again under the
    /// lock.
    pub limits: crate::engine::retention::RetentionLimits,
}

impl Fix {
    pub const fn kind(&self) -> FixKind {
        match self {
            Self::Write { .. } => FixKind::Write,
            Self::Chmod { .. } => FixKind::Chmod,
            Self::Quarantine { .. } => FixKind::Quarantine,
            Self::Flatten { .. } => FixKind::Flatten,
            Self::Suspend { .. } => FixKind::Suspend,
            Self::Delegate { .. } | Self::Retain { .. } => FixKind::Delegate,
        }
    }

    /// How many primitives E5 lowers it to (`remediation.estimated_actions`): one write,
    /// chmod or quarantine; a flatten's two renames, or three when the inner folder has
    /// its wrapper's name (in any case); a suspension's one per artifact; a delegation's
    /// none. A chmod to the mode the file already has lowers to none, so this counts it
    /// as an estimate does.
    pub fn primitive_count(&self) -> u32 {
        match self {
            Self::Write { .. } | Self::Chmod { .. } | Self::Quarantine { .. } => 1,
            Self::Flatten { inner, outer } => {
                let own_name = match (inner.file_name(), outer.file_name()) {
                    (Some(name), Some(wrapper)) => name.eq_ignore_ascii_case(wrapper),
                    _ => false,
                };
                if own_name { 3 } else { 2 }
            }
            Self::Suspend { journal } => u32::try_from(journal.artifacts.len()).unwrap_or(u32::MAX),
            Self::Delegate { .. } | Self::Retain { .. } => 0,
        }
    }
}

/// Where a quarantined path goes. Quarantine never crosses a filesystem (M4-D35): each
/// root is on the filesystem of what it receives, and E5 resolves it to an absolute
/// destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineRoot {
    /// A victim under `<state>`: `<run>/quarantine/state/<path relative to <state>>`.
    State,
    /// A victim under `<config>`: `<run>/quarantine/config/<path relative to <config>>`.
    Config,
    /// A victim under `<cache>`: `<cache>/quarantine/<run id>/<path relative to <cache>>`.
    Cache,
    /// A victim in a client (`<meta>` or `Interface/AddOns`):
    /// `<meta>/snapshots/<repair tx>.repair/quarantine/<path relative to client_dir>`.
    Client { client_dir: PathBuf },
    /// A victim in an agent skill directory, which is under the home directory but none
    /// of omaddons' own: `<run>/quarantine/skill/<path relative to the home directory>`.
    Skill,
}

/// A stuck journal and every artifact of it, to set aside as one suspension (M4-D38).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalSet {
    pub tx: TxId,
    pub family: JournalFamily,
    /// Its target, when its `prepared` line names one that reads.
    pub target: Option<TargetKey>,
    pub artifacts: Vec<SuspendedArtifact>,
    /// The suspension manifest, recorded on its set-aside renames.
    pub guards: Vec<Guard>,
    pub attribution: Attribution,
    /// Paths of it found that no root can receive, left where they are.
    pub unattributed: Vec<PathBuf>,
}

/// One artifact of a suspended journal (its journal file, in-flight lock, stage or
/// snapshot half) and the quarantine that receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendedArtifact {
    pub path: PathBuf,
    pub root: QuarantineRoot,
}

/// Whether every artifact of a journal was attributed to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Attribution {
    Complete,
    Partial,
}

/// A recovery the doctor has the engine run for it (M4-D37), as its preview discloses it
/// (`delegated_planned`) and its token selects it (M4-D44).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Delegation {
    pub tx: TxId,
    pub family: JournalFamily,
    pub phase: JournalPhase,
    pub direction: Direction,
}

/// Which kind of journal it is. `journal::diagnose` reads the same type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JournalFamily {
    /// A folder or settings transaction (`journal/<tx>.jsonl`, `<tx>.wtf.jsonl`).
    Folder,
    /// A doctor repair (M4-D31), whose journal E5 adds.
    Repair,
}

/// The phase a pending journal is in, across both families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JournalPhase {
    /// No complete line yet: recovery only removes it.
    Unprepared,
    Prepared,
    /// A repair with at least one `step` line.
    Step,
    Swapped,
    /// A restore whose displaced trees are being given a durable home.
    Preserve,
    Preserved,
    /// A repair whose every step ran.
    Done,
}

/// Which way a delegated recovery settles its journal. This is the delegation's wire
/// form; `journal::diagnose`'s own prediction ([`super::journal::Direction`]) also says
/// when recovery only deletes a journal that never ran, which a delegation discloses as a
/// rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    RollBack,
    RollForward,
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    use serde_json::json;

    use super::*;
    use crate::fsx::StampDigest;

    const SKILL: &str = "fm-plugins-skill-version-drift";
    const FILE_MODE: &str = "fm-secrets-credential-file-mode";
    const NESTED: &str = "fm-external_artifacts-nested-addon-folder";
    const CORRUPT: &str = "fm-schemas-state-corrupt";
    const TEMP: &str = "fm-state_files-orphan-temp-file";
    const TOO_NEW: &str = "fm-concurrency_primitives-journal-too-new";
    const PENDING: &str = "fm-concurrency_primitives-journal-pending";

    const BORN: i64 = 1_767_225_600_000_000_000;
    const RUN: &str = "/fixture/state/omaddons/doctor/runs/R";
    const JOURNAL: &str = "/fixture/state/omaddons/journal/01JB8Q2VA00000000000000003.jsonl";
    const SET_ASIDE: &str = "/fixture/state/omaddons/doctor/runs/R/quarantine/state/journal/01JB8Q2VA00000000000000003.jsonl";

    fn target() -> TargetKey {
        "01JB8Q2VA00000000000000002".parse().unwrap()
    }

    fn tx() -> TxId {
        "01JB8Q2VA00000000000000003".parse().unwrap()
    }

    fn mode(bits: u32) -> Mode {
        Mode::new(bits).unwrap()
    }

    fn id(ino: u64) -> Identity {
        Identity {
            ino,
            born_ns: Some(BORN),
        }
    }

    fn file(identity: Option<Identity>, sha256: Option<Sha256>, mode: Option<Mode>) -> Endpoint {
        Endpoint::File {
            identity,
            sha256,
            mode,
        }
    }

    fn step(seq: u32, fm: &'static str, op: Primitive, pre: Endpoints, post: Endpoints) -> Step {
        Step {
            seq,
            fix: seq,
            fm: Cow::Borrowed(fm),
            op,
            pre,
            post,
            guards: Vec::new(),
        }
    }

    fn rename(from: &str, to: &str) -> Primitive {
        Primitive::RenameNoReplace {
            from: from.into(),
            to: to.into(),
            activation: false,
        }
    }

    /// A rename's endpoints: `source` before, nothing at the destination; then the
    /// reverse, the destination holding `after`.
    fn moved(source: Endpoint, after: Endpoint) -> (Endpoints, Endpoints) {
        (
            Endpoints::Pair {
                path: source,
                to: Endpoint::Absent,
            },
            Endpoints::Pair {
                path: Endpoint::Absent,
                to: after,
            },
        )
    }

    /// The eight valid steps, named, in snapshot order.
    fn cases() -> Vec<(&'static str, Step)> {
        let write_over = step(
            1,
            SKILL,
            Primitive::WriteFile {
                path: "/fixture/skills/omaddons/SKILL.md".into(),
                mode: mode(0o644),
                backup: Some(format!("{RUN}/backups/1").into()),
            },
            Endpoints::One(file(None, Some(Sha256([0x11; 32])), None)),
            Endpoints::One(file(None, Some(Sha256([0x22; 32])), Some(mode(0o644)))),
        );
        let write_create = step(
            2,
            SKILL,
            Primitive::WriteFile {
                path: "/fixture/agents/skills/omaddons/SKILL.md".into(),
                mode: mode(0o644),
                backup: None,
            },
            Endpoints::One(Endpoint::Absent),
            Endpoints::One(file(None, Some(Sha256([0x22; 32])), Some(mode(0o644)))),
        );
        let chmod = step(
            3,
            FILE_MODE,
            Primitive::Chmod {
                path: "/fixture/config/omaddons/credentials/github".into(),
                mode: mode(0o600),
            },
            Endpoints::One(file(Some(id(4242)), None, Some(mode(0o644)))),
            Endpoints::One(file(Some(id(4242)), None, Some(mode(0o600)))),
        );
        let dir = Endpoint::Dir {
            identity: Some(id(77)),
            stamp: Some(Stamp([0x33; 32])),
        };
        let (pre, post) = moved(
            dir,
            Endpoint::Dir {
                identity: Some(id(77)),
                stamp: None,
            },
        );
        let quarantine_dir = step(
            4,
            NESTED,
            rename(
                "/fixture/client/Interface/AddOns/X",
                "/fixture/client/Interface/.omaddons/snapshots/01JB8Q2VA00000000000000003.repair/quarantine/Interface/AddOns/X",
            ),
            pre,
            post,
        );
        let (pre, post) = moved(
            file(Some(id(4343)), None, None),
            file(Some(id(4343)), None, None),
        );
        let quarantine_file = step(
            5,
            CORRUPT,
            rename(
                "/fixture/state/omaddons/targets/01JB8Q2VA00000000000000002.json",
                &format!("{RUN}/quarantine/state/targets/01JB8Q2VA00000000000000002.json"),
            ),
            pre,
            post,
        );
        let link = |link_target: Option<PathBuf>| Endpoint::Symlink {
            identity: Some(id(88)),
            link_target,
        };
        let (pre, post) = moved(link(Some("../blobs/ab".into())), link(None));
        let temp_symlink = step(
            6,
            TEMP,
            rename(
                "/fixture/cache/omaddons/tmp/download.part",
                "/fixture/cache/omaddons/quarantine/R/tmp/download.part",
            ),
            pre,
            post,
        );
        let (pre, post) = moved(
            file(Some(id(99)), None, None),
            file(Some(id(99)), None, None),
        );
        let activation = step(
            7,
            TOO_NEW,
            Primitive::RenameNoReplace {
                from: SET_ASIDE.into(),
                to: JOURNAL.into(),
                activation: true,
            },
            pre,
            post,
        );
        let unborn = Identity {
            ino: 99,
            born_ns: None,
        };
        let (pre, post) = moved(
            file(Some(unborn), None, None),
            file(Some(unborn), None, None),
        );
        let mut set_aside = step(8, TOO_NEW, rename(JOURNAL, SET_ASIDE), pre, post);
        set_aside.guards = vec![
            Guard::Generation {
                target: target(),
                generation: 7,
            },
            Guard::Marker {
                target: target(),
                sha256: Some(Sha256([0x44; 32])),
            },
            Guard::Epoch {
                path: "/fixture/client/Interface/AddOns/X".into(),
                stamp: Some(Stamp([0x55; 32])),
            },
            Guard::WtfEpoch {
                target: target(),
                stamp: Some(Stamp([0x66; 32])),
            },
        ];
        vec![
            ("write_over_a_file", write_over),
            ("write_creating_a_file", write_create),
            ("chmod_a_credential", chmod),
            ("rename_a_dir_into_quarantine", quarantine_dir),
            ("rename_a_state_file_into_quarantine", quarantine_file),
            ("rename_a_temp_symlink", temp_symlink),
            ("activation_rename", activation),
            ("set_aside_rename_with_guards", set_aside),
        ]
    }

    fn named(name: &str) -> Step {
        cases()
            .into_iter()
            .find(|(case, _)| *case == name)
            .map(|(_, step)| step)
            .unwrap()
    }

    /// One fix entry per kind.
    fn fixes() -> Vec<FixEntry> {
        [
            (SKILL, FixKind::Write),
            (FILE_MODE, FixKind::Chmod),
            (CORRUPT, FixKind::Quarantine),
            (NESTED, FixKind::Flatten),
            (TOO_NEW, FixKind::Suspend),
            (PENDING, FixKind::Delegate),
        ]
        .into_iter()
        .zip(1..)
        .map(|((fm, kind), fix)| FixEntry {
            fix,
            fm: Cow::Borrowed(fm),
            kind,
        })
        .collect()
    }

    #[test]
    fn primitives_serialize_to_the_journal_wire_form() {
        let steps: Vec<Step> = cases().into_iter().map(|(_, step)| step).collect();
        insta::assert_json_snapshot!("repair_primitives_wire_form", steps);
        insta::assert_json_snapshot!("repair_fixes_wire_form", fixes());
    }

    #[test]
    fn the_wire_form_reads_back_equal() {
        for (case, step) in cases() {
            let text = serde_json::to_string(&step).unwrap();
            let back = serde_json::from_str::<Step>(&text);
            assert_eq!(
                back.as_ref().ok(),
                Some(&step),
                "{case}: {text} -> {back:?}"
            );
        }
        let text = serde_json::to_string(&fixes()).unwrap();
        let back: Vec<FixEntry> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, fixes(), "fixes: {text}");
    }

    #[test]
    fn ops_kinds_and_guards_use_the_published_names() {
        let ops: Vec<String> = cases()
            .iter()
            .map(|(_, step)| serde_json::to_value(step).unwrap()["op"].to_string())
            .collect();
        assert_eq!(
            ops,
            [
                "\"write_file\"",
                "\"write_file\"",
                "\"chmod\"",
                "\"rename_noreplace\"",
                "\"rename_noreplace\"",
                "\"rename_noreplace\"",
                "\"rename_noreplace\"",
                "\"rename_noreplace\"",
            ]
        );
        for (case, step) in cases() {
            assert_eq!(
                serde_json::to_value(&step).unwrap()["op"],
                json!(step.op.name()),
                "{case}"
            );
        }
        let kinds = [
            Endpoint::Absent,
            file(None, None, None),
            Endpoint::Dir {
                identity: None,
                stamp: None,
            },
            Endpoint::Symlink {
                identity: None,
                link_target: None,
            },
        ];
        for endpoint in kinds {
            assert_eq!(
                serde_json::to_value(&endpoint).unwrap(),
                json!({"kind": endpoint.kind()}),
                "{endpoint:?}"
            );
        }
        assert_eq!(
            fixes()
                .iter()
                .map(|f| serde_json::to_value(f.kind).unwrap())
                .collect::<Vec<_>>(),
            [
                "write",
                "chmod",
                "quarantine",
                "flatten",
                "suspend",
                "delegate"
            ]
        );
        let guards: Vec<serde_json::Value> = named("set_aside_rename_with_guards")
            .guards
            .iter()
            .map(|g| serde_json::to_value(g).unwrap()["kind"].clone())
            .collect();
        assert_eq!(guards, ["generation", "marker", "epoch", "wtf_epoch"]);
        assert_eq!(
            serde_json::to_value([Attribution::Complete, Attribution::Partial]).unwrap(),
            json!(["complete", "partial"])
        );
    }

    #[test]
    fn modes_are_four_octal_digits() {
        for (bits, text) in [
            (0o600, "0600"),
            (0o644, "0644"),
            (0o4755, "4755"),
            (0o7777, "7777"),
            (0, "0000"),
        ] {
            let m = mode(bits);
            assert_eq!(m.to_string(), text, "{bits:o}");
            assert_eq!(serde_json::to_value(m).unwrap(), json!(text), "{bits:o}");
            assert_eq!(text.parse::<Mode>(), Ok(m), "{text}");
            assert_eq!(m.bits(), bits, "{text}");
        }
        assert_eq!(Mode::new(0o10000), None);
        for text in ["600", "0800", "07777", "06o0", "", "+600", " 600"] {
            assert!(text.parse::<Mode>().is_err(), "{text:?} read as a mode");
            assert!(
                serde_json::from_value::<Mode>(json!(text)).is_err(),
                "{text:?} deserialized"
            );
        }
        assert!(
            serde_json::from_value::<Mode>(json!(384)).is_err(),
            "a number"
        );
    }

    #[test]
    fn identities_are_decimal_strings() {
        let cases = [
            (
                "with_birth_time",
                Identity {
                    ino: 12345,
                    born_ns: Some(1_767_225_600_123_456_789),
                },
                r#"{"ino":"12345","born_ns":"1767225600123456789"}"#,
            ),
            (
                "before_the_epoch",
                Identity {
                    ino: 1,
                    born_ns: Some(-5),
                },
                r#"{"ino":"1","born_ns":"-5"}"#,
            ),
            (
                "without_birth_time",
                Identity {
                    ino: u64::MAX,
                    born_ns: None,
                },
                r#"{"ino":"18446744073709551615"}"#,
            ),
        ];
        for (case, identity, wire) in cases {
            let text = serde_json::to_string(&identity).unwrap();
            assert_eq!(text, wire, "{case}: {identity:?} -> {text}");
            let back: Identity = serde_json::from_str(&text).unwrap();
            assert_eq!(back, identity, "{case}: {text}");
        }
        for (case, wire) in [
            ("not_digits", json!({"ino": "x"})),
            ("a_number", json!({"ino": 12})),
            ("a_signed_inode", json!({"ino": "-1"})),
            ("a_plus_sign", json!({"ino": "+1"})),
            ("an_empty_inode", json!({"ino": ""})),
            ("past_u64", json!({"ino": "18446744073709551616"})),
            ("a_numeric_birth_time", json!({"ino": "1", "born_ns": 5})),
            ("a_bare_minus", json!({"ino": "1", "born_ns": "-"})),
            ("no_inode", json!({"born_ns": "5"})),
        ] {
            let read = serde_json::from_value::<Identity>(wire.clone());
            assert!(read.is_err(), "{case}: {wire} -> {read:?}");
        }
    }

    #[test]
    fn digests_keep_their_prefixes() {
        let sha = Sha256([0xab; 32]);
        let stamp = Stamp([0xab; 32]);
        let hex64 = "ab".repeat(32);
        assert_eq!(sha.to_string(), format!("sha256:{hex64}"));
        assert_eq!(stamp.to_string(), format!("stamp:sha256:{hex64}"));
        assert_eq!(sha.to_string().parse::<Sha256>(), Ok(sha));
        assert_eq!(stamp.to_string().parse::<Stamp>(), Ok(stamp));
        assert_eq!(
            serde_json::from_value::<Sha256>(json!(sha.to_string())).unwrap(),
            sha
        );
        for (case, text) in [
            ("a_stamp", format!("stamp:sha256:{hex64}")),
            ("uppercase", format!("sha256:{}", "AB".repeat(32))),
            ("63_digits", format!("sha256:{}", &hex64[1..])),
            ("65_digits", format!("sha256:{hex64}a")),
            ("no_prefix", hex64.clone()),
        ] {
            let read = text.parse::<Sha256>();
            assert!(read.is_err(), "{case}: {text} -> {read:?}");
        }
        for (case, text) in [
            ("a_content_digest", format!("sha256:{hex64}")),
            ("uppercase", format!("stamp:sha256:{}", "AB".repeat(32))),
        ] {
            let read = text.parse::<Stamp>();
            assert!(read.is_err(), "{case}: {text} -> {read:?}");
        }
    }

    #[test]
    fn an_fsx_stamp_reads_as_a_stamp() {
        let digest = StampDigest::from_entries([("a", "file", 1, 0o644, 1)]);
        let text = digest.to_string();
        let stamp: Stamp = text.parse().unwrap();
        assert_eq!(stamp.to_string(), text);
    }

    /// Mutations of a valid case that break exactly one rule.
    fn invalid_cases() -> Vec<(&'static str, Step, StepError)> {
        let mut cases = Vec::new();
        let mut push = |case, base: &str, edit: &dyn Fn(&mut Step), err| {
            let mut step = named(base);
            edit(&mut step);
            cases.push((case, step, err));
        };
        push(
            "relative_path",
            "write_over_a_file",
            &|s| {
                s.op = Primitive::WriteFile {
                    path: "skills/SKILL.md".into(),
                    mode: mode(0o644),
                    backup: Some(format!("{RUN}/backups/1").into()),
                };
            },
            StepError::RelativePath {
                seq: 1,
                path: "skills/SKILL.md".into(),
            },
        );
        push(
            "parent_component",
            "rename_a_dir_into_quarantine",
            &|s| {
                s.op = rename(
                    "/fixture/client/Interface/AddOns/X",
                    "/fixture/client/Interface/AddOns/../X2",
                );
            },
            StepError::ParentComponent {
                seq: 4,
                path: "/fixture/client/Interface/AddOns/../X2".into(),
            },
        );
        push(
            "write_with_pair_endpoints",
            "write_over_a_file",
            &|s| {
                s.pre = Endpoints::Pair {
                    path: file(None, Some(Sha256([0x11; 32])), None),
                    to: Endpoint::Absent,
                };
            },
            StepError::PairOffARename {
                seq: 1,
                op: "write_file",
            },
        );
        push(
            "rename_with_one_endpoint",
            "rename_a_state_file_into_quarantine",
            &|s| s.pre = Endpoints::One(file(Some(id(4343)), None, None)),
            StepError::RenameWithOneEndpoint { seq: 5 },
        );
        push(
            "write_post_without_sha256",
            "write_over_a_file",
            &|s| s.post = Endpoints::One(file(None, None, Some(mode(0o644)))),
            StepError::WritePostWithoutSha256 { seq: 1 },
        );
        push(
            "write_post_mode_differs",
            "write_creating_a_file",
            &|s| {
                s.post = Endpoints::One(file(None, Some(Sha256([0x22; 32])), Some(mode(0o600))));
            },
            StepError::PostModeDiffers {
                seq: 2,
                op: "write_file",
                mode: mode(0o644),
                post: Some(mode(0o600)),
            },
        );
        push(
            "overwrite_without_backup",
            "write_over_a_file",
            &|s| {
                s.op = Primitive::WriteFile {
                    path: "/fixture/skills/omaddons/SKILL.md".into(),
                    mode: mode(0o644),
                    backup: None,
                };
            },
            StepError::OverwriteWithoutBackup { seq: 1 },
        );
        push(
            "create_with_backup",
            "write_creating_a_file",
            &|s| {
                s.op = Primitive::WriteFile {
                    path: "/fixture/agents/skills/omaddons/SKILL.md".into(),
                    mode: mode(0o644),
                    backup: Some(format!("{RUN}/backups/2").into()),
                };
            },
            StepError::CreateWithBackup { seq: 2 },
        );
        push(
            "chmod_identity_changed",
            "chmod_a_credential",
            &|s| s.post = Endpoints::One(file(Some(id(4243)), None, Some(mode(0o600)))),
            StepError::ChmodIdentityChanged { seq: 3 },
        );
        push(
            "chmod_to_the_same_mode",
            "chmod_a_credential",
            &|s| s.pre = Endpoints::One(file(Some(id(4242)), None, Some(mode(0o600)))),
            StepError::ChmodToTheSameMode { seq: 3 },
        );
        push(
            "chmod_pre_without_mode",
            "chmod_a_credential",
            &|s| s.pre = Endpoints::One(file(Some(id(4242)), None, None)),
            StepError::ChmodPreWithoutMode { seq: 3 },
        );
        push(
            "rename_destination_not_absent_before",
            "rename_a_state_file_into_quarantine",
            &|s| {
                s.pre = Endpoints::Pair {
                    path: file(Some(id(4343)), None, None),
                    to: file(None, None, None),
                };
            },
            StepError::RenameDestinationNotAbsentBefore { seq: 5 },
        );
        push(
            "rename_source_not_absent_after",
            "rename_a_state_file_into_quarantine",
            &|s| {
                s.post = Endpoints::Pair {
                    path: file(None, None, None),
                    to: file(Some(id(4343)), None, None),
                };
            },
            StepError::RenameSourceNotAbsentAfter { seq: 5 },
        );
        push(
            "rename_identity_changed",
            "rename_a_state_file_into_quarantine",
            &|s| {
                s.post = Endpoints::Pair {
                    path: Endpoint::Absent,
                    to: file(Some(id(4344)), None, None),
                };
            },
            StepError::RenameIdentityChanged { seq: 5 },
        );
        push(
            "rename_kind_changed",
            "rename_a_dir_into_quarantine",
            &|s| {
                s.post = Endpoints::Pair {
                    path: Endpoint::Absent,
                    to: file(Some(id(77)), None, None),
                };
            },
            StepError::RenameKindChanged {
                seq: 4,
                before: "dir",
                after: "file",
            },
        );
        push(
            "dir_rename_without_stamp",
            "rename_a_dir_into_quarantine",
            &|s| {
                s.pre = Endpoints::Pair {
                    path: Endpoint::Dir {
                        identity: Some(id(77)),
                        stamp: None,
                    },
                    to: Endpoint::Absent,
                };
            },
            StepError::DirRenameWithoutStamp { seq: 4 },
        );
        push(
            "symlink_rename_without_link_target",
            "rename_a_temp_symlink",
            &|s| {
                s.pre = Endpoints::Pair {
                    path: Endpoint::Symlink {
                        identity: Some(id(88)),
                        link_target: None,
                    },
                    to: Endpoint::Absent,
                };
            },
            StepError::SymlinkRenameWithoutLinkTarget { seq: 6 },
        );
        push(
            "guards_on_a_chmod",
            "chmod_a_credential",
            &|s| s.guards = named("set_aside_rename_with_guards").guards,
            StepError::GuardsOffARename {
                seq: 3,
                op: "chmod",
            },
        );
        push(
            "a_relative_epoch_guard",
            "set_aside_rename_with_guards",
            &|s| {
                s.guards.push(Guard::Epoch {
                    path: "Interface/AddOns/X".into(),
                    stamp: Some(Stamp([0x55; 32])),
                });
            },
            StepError::RelativePath {
                seq: 8,
                path: "Interface/AddOns/X".into(),
            },
        );
        push(
            "a_relative_backup",
            "write_over_a_file",
            &|s| {
                s.op = Primitive::WriteFile {
                    path: "/fixture/skills/omaddons/SKILL.md".into(),
                    mode: mode(0o644),
                    backup: Some("backups/1".into()),
                };
            },
            StepError::RelativePath {
                seq: 1,
                path: "backups/1".into(),
            },
        );
        push(
            "rename_source_absent_before",
            "rename_a_state_file_into_quarantine",
            &|s| {
                s.pre = Endpoints::Pair {
                    path: Endpoint::Absent,
                    to: Endpoint::Absent,
                };
            },
            StepError::RenameSourceAbsentBefore { seq: 5 },
        );
        push(
            "rename_source_without_identity",
            "rename_a_state_file_into_quarantine",
            &|s| {
                s.pre = Endpoints::Pair {
                    path: file(None, None, None),
                    to: Endpoint::Absent,
                };
            },
            StepError::RenameSourceWithoutIdentity { seq: 5 },
        );
        push(
            "write_pre_a_dir",
            "write_over_a_file",
            &|s| {
                s.pre = Endpoints::One(Endpoint::Dir {
                    identity: None,
                    stamp: None,
                });
            },
            StepError::WritePre { seq: 1 },
        );
        push(
            "write_post_absent",
            "write_over_a_file",
            &|s| s.post = Endpoints::One(Endpoint::Absent),
            StepError::WritePostNotAFile { seq: 1 },
        );
        push(
            "chmod_post_mode_differs",
            "chmod_a_credential",
            &|s| s.post = Endpoints::One(file(Some(id(4242)), None, Some(mode(0o640)))),
            StepError::PostModeDiffers {
                seq: 3,
                op: "chmod",
                mode: mode(0o600),
                post: Some(mode(0o640)),
            },
        );
        push(
            "chmod_of_a_dir",
            "chmod_a_credential",
            &|s| {
                s.pre = Endpoints::One(Endpoint::Dir {
                    identity: Some(id(4242)),
                    stamp: None,
                });
            },
            StepError::ChmodNotAFile { seq: 3 },
        );
        cases
    }

    #[test]
    fn steps_validate_by_case() {
        for (case, step) in cases() {
            assert_eq!(step.validate(), Ok(()), "{case}: {step:?}");
        }
        let mut seen = Vec::new();
        for (case, step, expected) in invalid_cases() {
            let got = step.validate();
            assert_eq!(got.as_ref(), Err(&expected), "{case}: {step:?}");
            let message = expected.to_string();
            assert!(
                message.starts_with(&format!("step {}: ", step.seq)),
                "{case}: {message}"
            );
            assert!(
                !seen.contains(&expected),
                "{case} repeats an error: {expected}"
            );
            seen.push(expected);
        }
    }

    #[test]
    fn step_lists_validate_by_case() {
        let fixes = fixes();
        let three = || {
            let mut steps: Vec<Step> = ["write_over_a_file", "chmod_a_credential"]
                .into_iter()
                .map(named)
                .collect();
            steps.push(named("rename_a_state_file_into_quarantine"));
            for ((seq, fix), step) in [(1, 1), (2, 2), (3, 3)].into_iter().zip(&mut steps) {
                step.seq = seq;
                step.fix = fix;
            }
            steps
        };
        assert_eq!(validate_steps(&fixes, &three()), Ok(()));
        assert_eq!(validate_steps(&fixes, &[]), Ok(()));
        let edit = |f: &dyn Fn(&mut Vec<Step>)| {
            let mut steps = three();
            f(&mut steps);
            steps
        };
        let cases: [(&str, Vec<Step>, StepError); 6] = [
            (
                "seq_starts_at_two",
                edit(&|s| {
                    for step in s.iter_mut() {
                        step.seq += 1;
                    }
                }),
                StepError::SeqOutOfOrder {
                    seq: 2,
                    expected: 1,
                },
            ),
            (
                "seq_gap",
                edit(&|s| s[2].seq = 4),
                StepError::SeqOutOfOrder {
                    seq: 4,
                    expected: 3,
                },
            ),
            (
                "fix_zero",
                edit(&|s| s[1].fix = 0),
                StepError::FixOutOfRange {
                    seq: 2,
                    fix: 0,
                    fixes: 6,
                },
            ),
            (
                "fix_past_the_end",
                edit(&|s| s[2].fix = 7),
                StepError::FixOutOfRange {
                    seq: 3,
                    fix: 7,
                    fixes: 6,
                },
            ),
            (
                "fm_differs_from_its_fix",
                edit(&|s| s[1].fm = Cow::Borrowed(SKILL)),
                StepError::FmDiffers {
                    seq: 2,
                    fm: SKILL.to_owned(),
                    fix_fm: FILE_MODE.to_owned(),
                },
            ),
            (
                "an_invalid_step",
                edit(&|s| s[0].post = Endpoints::One(Endpoint::Absent)),
                StepError::WritePostNotAFile { seq: 1 },
            ),
        ];
        for (case, steps, expected) in cases {
            assert_eq!(
                validate_steps(&fixes, &steps),
                Err(expected),
                "{case}: {steps:?}"
            );
        }
    }

    #[test]
    fn a_non_utf8_path_never_serializes() {
        let path = PathBuf::from(OsStr::from_bytes(b"/fixture/\xff"));
        let mut step = named("chmod_a_credential");
        step.op = Primitive::Chmod {
            path: path.clone(),
            mode: mode(0o600),
        };
        assert_eq!(step.validate(), Err(StepError::NotUtf8 { seq: 3, path }));
        let written = serde_json::to_string(&step);
        assert!(written.is_err(), "{written:?}");
    }

    #[test]
    fn newer_minor_keys_are_ignored_and_unknown_ops_refused() {
        for (case, step) in cases() {
            let mut value = serde_json::to_value(&step).unwrap();
            value["note"] = json!("x");
            if let Some(pre) = value["pre"].get_mut("path") {
                pre["note"] = json!("y");
            } else {
                value["pre"]["note"] = json!("y");
            }
            let back = serde_json::from_value::<Step>(value.clone());
            assert_eq!(
                back.as_ref().ok(),
                Some(&step),
                "{case}: {value} -> {back:?}"
            );
        }
        let mut unknown_op = serde_json::to_value(named("chmod_a_credential")).unwrap();
        unknown_op["op"] = json!("symlink_atomic");
        let mut unknown_kind = serde_json::to_value(named("chmod_a_credential")).unwrap();
        unknown_kind["pre"]["kind"] = json!("socket");
        let mut unknown_guard =
            serde_json::to_value(named("set_aside_rename_with_guards")).unwrap();
        unknown_guard["guards"][0]["kind"] = json!("lease");
        for (case, value) in [
            ("unknown_op", unknown_op),
            ("unknown_kind", unknown_kind),
            ("unknown_guard", unknown_guard),
        ] {
            let read = serde_json::from_value::<Step>(value.clone());
            assert!(read.is_err(), "{case}: {value} -> {read:?}");
        }
    }

    #[test]
    fn fix_kinds_match_fixes() {
        let delegation = Delegation {
            tx: tx(),
            family: JournalFamily::Folder,
            phase: JournalPhase::Prepared,
            direction: Direction::RollBack,
        };
        let suspension = JournalSet {
            tx: tx(),
            family: JournalFamily::Folder,
            target: Some(target()),
            artifacts: vec![SuspendedArtifact {
                path: JOURNAL.into(),
                root: QuarantineRoot::State,
            }],
            guards: Vec::new(),
            attribution: Attribution::Complete,
            unattributed: Vec::new(),
        };
        let cases = [
            (
                Fix::Write {
                    path: "/fixture/skills/omaddons/SKILL.md".into(),
                    content: b"guide".to_vec(),
                    mode: mode(0o644),
                },
                FixKind::Write,
            ),
            (
                Fix::Chmod {
                    path: "/fixture/config/omaddons/credentials/github".into(),
                    mode: mode(0o600),
                },
                FixKind::Chmod,
            ),
            (
                Fix::Quarantine {
                    path: "/fixture/client/Interface/AddOns/X".into(),
                    root: QuarantineRoot::Client {
                        client_dir: "/fixture/client".into(),
                    },
                },
                FixKind::Quarantine,
            ),
            (
                Fix::Flatten {
                    inner: "/fixture/client/Interface/AddOns/X/Y".into(),
                    outer: "/fixture/client/Interface/AddOns/X".into(),
                },
                FixKind::Flatten,
            ),
            (
                Fix::Suspend {
                    journal: suspension,
                },
                FixKind::Suspend,
            ),
            (Fix::Delegate { what: delegation }, FixKind::Delegate),
        ];
        for (fix, kind) in cases {
            assert_eq!(fix.kind(), kind, "{fix:?}");
        }
    }

    #[test]
    fn delegations_serialize_as_previewed() {
        let cases = [
            (
                Delegation {
                    tx: tx(),
                    family: JournalFamily::Folder,
                    phase: JournalPhase::Step,
                    direction: Direction::RollBack,
                },
                json!({"tx": tx().to_string(), "family": "folder", "phase": "step", "direction": "roll_back"}),
            ),
            (
                Delegation {
                    tx: tx(),
                    family: JournalFamily::Repair,
                    phase: JournalPhase::Done,
                    direction: Direction::RollForward,
                },
                json!({"tx": tx().to_string(), "family": "repair", "phase": "done", "direction": "roll_forward"}),
            ),
        ];
        for (delegation, wire) in cases {
            assert_eq!(serde_json::to_value(delegation).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<Delegation>(wire.clone()).unwrap(),
                delegation,
                "{wire}"
            );
        }
        let phases = [
            JournalPhase::Prepared,
            JournalPhase::Step,
            JournalPhase::Swapped,
            JournalPhase::Preserve,
            JournalPhase::Preserved,
            JournalPhase::Done,
        ];
        assert_eq!(
            serde_json::to_value(phases).unwrap(),
            json!([
                "prepared",
                "step",
                "swapped",
                "preserve",
                "preserved",
                "done"
            ])
        );
    }

    #[test]
    fn write_digest_reads_the_post() {
        assert_eq!(
            named("write_over_a_file").write_digest(),
            Some(&Sha256([0x22; 32]))
        );
        assert_eq!(
            named("write_creating_a_file").write_digest(),
            Some(&Sha256([0x22; 32]))
        );
        assert_eq!(named("chmod_a_credential").write_digest(), None);
    }

    #[test]
    fn touched_names_a_renames_both_ends() {
        let paths = |name: &str| -> Vec<PathBuf> {
            named(name)
                .touched()
                .into_iter()
                .map(Path::to_path_buf)
                .collect()
        };
        assert_eq!(
            paths("write_over_a_file"),
            [PathBuf::from("/fixture/skills/omaddons/SKILL.md")]
        );
        assert_eq!(
            paths("set_aside_rename_with_guards"),
            [PathBuf::from(JOURNAL), PathBuf::from(SET_ASIDE)]
        );
    }

    #[test]
    fn the_step_schema_is_pinned() {
        let schema = crate::contract::schema::generator().into_root_schema_for::<Step>();
        insta::assert_json_snapshot!("repair_step_schema", schema);
        let value = schema.as_value();
        let variants = value["oneOf"].as_array().unwrap();
        // An op with no variant requires nothing, and fails the assertions below.
        let required = |op: &str| -> Vec<String> {
            variants
                .iter()
                .find(|v| v["properties"]["op"]["const"] == json!(op))
                .and_then(|variant| variant["required"].as_array())
                .into_iter()
                .flatten()
                .filter_map(|key| key.as_str().map(str::to_owned))
                .collect()
        };
        for (op, keys, optional) in [
            ("write_file", ["path", "mode"], "backup"),
            ("chmod", ["path", "mode"], "backup"),
            ("rename_noreplace", ["path", "to"], "activation"),
        ] {
            let required = required(op);
            for key in keys {
                assert!(
                    required.contains(&key.to_owned()),
                    "{op} requires {key}: {required:?}"
                );
            }
            assert!(
                !required.contains(&optional.to_owned()),
                "{op}: {required:?}"
            );
        }
        let top: Vec<&str> = value["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|key| key.as_str().unwrap())
            .collect();
        for key in ["seq", "fix", "fm", "pre", "post"] {
            assert!(top.contains(&key), "{key}: {top:?}");
        }
        assert!(!top.contains(&"guards"), "{top:?}");
    }
}
