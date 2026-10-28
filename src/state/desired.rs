//! `addons.toml`: the addon sets wanted on each client (M2 goal G-M2-5), which `sync`
//! reconciles against and `export` writes. The default path is
//! [`Paths::addons_toml`](super::paths::Paths::addons_toml).
//!
//! ```toml
//! schema_version = "1.0"
//!
//! [family.mainline]                  # every live mainline client
//! addons = ["WeakAuras", "DBM", "LittleWigs"]
//!
//! [[family.mainline.addon]]          # the table form, when an entry needs options
//! name = "BigWigs"
//! ref = "gh:BigWigsMods/BigWigs"
//! dependency = true                  # a version lock for a dependency, not a request
//! version = "v428"
//!
//! [target."wotlk"]                   # one client by id: the only way to reach a test client
//! addons = ["git:https://github.com/fondlez/GroupBulletinBoard#main"]
//! ```
//!
//! A string is a name or a ref, read the way `install` reads its arguments. A table has
//! `name` (unless `ref` is given), `ref`, `channel` (absent is `stable`, whatever
//! `default_channel` says), `version`, `pinned`, `dependency`, `sha256`, `revision` and
//! `any_flavor`.
//!
//! [`parse`] is pure: no I/O, registry or catalog. Unknown keys warn, bad values are
//! `desired_state_invalid` with the line, and names and refs are resolved per target later
//! (`app::resolve_args::resolve_entry`), so one typo fails one entry instead of the file.
//! [`validate`] checks target ids against the known targets, [`desired_for`] merges a
//! target's layers, and [`render`] writes the canonical file. User files are inputs:
//! never migrated or rewritten in place.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use schemars::JsonSchema;
use serde_json::json;
use toml::Spanned;
use toml::de::{DeString, DeTable, DeValue};

use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::model::{Channel, Ref, Target};
use crate::rules::{Family, TargetKind};

/// The format this build writes; it reads every 1.x.
pub const SCHEMA_VERSION: &str = "1.0";

/// The file keys `parse` reads; every other key warns as `desired_unknown_key`.
pub const FILE_KEYS: &[&str] = &["schema_version", "family", "target"];
/// The keys of one family or target section.
pub const SECTION_KEYS: &[&str] = &["addons", "addon"];
/// The keys of one table-form addon entry.
pub const ENTRY_KEYS: &[&str] = &[
    "name",
    "ref",
    "version",
    "channel",
    "pinned",
    "dependency",
    "sha256",
    "revision",
    "any_flavor",
];

/// `addons.toml` represented as JSON; documentation only, since [`parse`] reads it.
/// Every key may be omitted.
#[derive(JsonSchema)]
pub struct DesiredStateSchema {
    /// The format version, normally `1.0`; omitted reads as `1.0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    /// Addon sets for each live client family; omitted when no family set is given.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<BTreeMap<DesiredFamilyKey, DesiredSectionSchema>>")]
    pub family: Option<BTreeMap<Family, DesiredSectionSchema>>,
    /// Addon sets by client id; omitted when no individual client set is given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<BTreeMap<String, DesiredSectionSchema>>,
}

/// One section; either key may be omitted.
#[derive(JsonSchema)]
pub struct DesiredSectionSchema {
    /// Names or refs to install; omitted when no string-form entries are given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addons: Option<Vec<String>>,
    /// Entries with individual options; omitted when no table-form entries are given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addon: Option<Vec<DesiredEntrySchema>>,
}

/// One table-form addon; every key may be omitted, but `name` or `ref` is needed.
#[derive(JsonSchema)]
pub struct DesiredEntrySchema {
    /// An addon name; may be omitted when `ref` is given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A canonical source ref; may be omitted when `name` is given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    /// A release version; omitted selects the newest compatible release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A release channel; omitted means `stable`, independent of default_channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
    /// Whether to pin the addon; omitted means false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    /// A dependency version lock rather than a request; omitted means false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency: Option<bool>,
    /// The archive's 64 lowercase hexadecimal SHA-256 digits; omitted leaves it unlocked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The release's source revision; omitted leaves the revision unlocked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
    /// Whether to accept a build the publisher made for another client; omitted means false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub any_flavor: Option<bool>,
}

/// A schema-only map key: `unknown` is a discovery fallback, never a desired family.
struct DesiredFamilyKey;

impl JsonSchema for DesiredFamilyKey {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DesiredFamily".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let names: Vec<_> = Family::ALL
            .into_iter()
            .filter(|family| *family != Family::Unknown)
            .map(Family::as_str)
            .collect();
        schemars::json_schema!({ "type": "string", "enum": names })
    }
}

/// The first line [`render`] writes.
const HEADER: &str =
    "# Desired addon sets for omaddons. `omaddons sync` makes each client match this file.";

/// A parsed `addons.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredState {
    pub schema_version: String,
    /// `[family.<family>]`: every live target of that family.
    pub families: BTreeMap<Family, Section>,
    /// `[target.<id>]`: one target, by id (as written; ids compare ignoring ASCII case).
    pub targets: BTreeMap<String, Section>,
}

impl Default for DesiredState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_owned(),
            families: BTreeMap::new(),
            targets: BTreeMap::new(),
        }
    }
}

/// One section's entries, in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Section {
    pub entries: Vec<Entry>,
    /// Its line in the file (1-based); 0 when it was not read from one.
    pub line: usize,
}

/// One wanted addon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// A name or a ref as `install` reads it. A bare string of an `addons` list is one,
    /// unless it is a canonical ref (`gh:o/r`, `wowi:5995`), which is kept as `ref`.
    pub name: Option<String>,
    pub r#ref: Option<String>,
    /// Absent is `stable`, never `default_channel`, so a config change never changes
    /// what a file means (M2-D20).
    pub channel: Channel,
    pub version: Option<String>,
    pub pinned: bool,
    /// A version lock for a dependency: installed only when something requires it.
    pub dependency: bool,
    /// The archive's SHA-256 (64 lowercase hex digits), for offline reproduction.
    pub sha256: Option<String>,
    /// The release's source revision (WowAce: the tag's revision), installed with its
    /// `version`, so a locked install fetches exactly that tree.
    pub revision: Option<u32>,
    /// `install --any-flavor` on this client: builds the publisher did not make for it
    /// install anyway.
    pub any_flavor: bool,
    /// Its line in the file (1-based); 0 when it was not read from one.
    pub line: usize,
}

impl Entry {
    /// `name` with every option at its default.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            r#ref: None,
            channel: Channel::Stable,
            version: None,
            pinned: false,
            dependency: false,
            sha256: None,
            revision: None,
            any_flavor: false,
            line: 0,
        }
    }

    /// What resolves: `ref` when given, else `name`.
    pub fn request(&self) -> &str {
        self.r#ref
            .as_deref()
            .or(self.name.as_deref())
            .unwrap_or_default()
    }

    /// The strings another entry of its section must not repeat: its name and its ref,
    /// ignoring ASCII case.
    fn keys(&self) -> Vec<String> {
        self.name
            .iter()
            .chain(&self.r#ref)
            .map(|s| s.to_ascii_lowercase())
            .collect()
    }
}

/// Why a file is refused (`details.reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// A wrong type or value, a table without `name` or `ref`, or broken TOML.
    BadValue,
    UnknownFamily,
    NewerSchema,
    Duplicate,
}

impl Reason {
    const fn wire(self) -> &'static str {
        match self {
            Self::BadValue => "bad_value",
            Self::UnknownFamily => "unknown_family",
            Self::NewerSchema => "newer_schema",
            Self::Duplicate => "duplicate",
        }
    }
}

/// Reads `text`, the contents of `path`, as desired state: the state and its
/// `desired_unknown_key` warnings, or `desired_state_invalid` with
/// `details: {path, line, key, reason}`. A missing `schema_version` is 1.0.
pub fn parse(text: &str, path: &Path) -> Result<(DesiredState, Vec<Warning>)> {
    let mut p = Parser {
        text,
        path,
        warnings: Vec::new(),
    };
    let doc = DeTable::parse(text).map_err(|e| {
        let line = e.span().map_or(1, |s| p.line(s.start));
        let why = e.message().trim().to_owned();
        p.invalid_at(
            line,
            None,
            "",
            Reason::BadValue,
            format!("it is not TOML: {why}"),
        )
    })?;
    let doc = doc.get_ref();
    let mut state = DesiredState::default();
    // The version first: a newer major's other keys mean nothing to this build.
    if let Some((_, value)) = doc.iter().find(|(k, _)| key_str(k) == "schema_version") {
        state.schema_version = p.schema_version(value)?;
    }
    for (key, value) in doc.iter() {
        match key_str(key) {
            "schema_version" => {}
            "family" => p.families(value, &mut state.families)?,
            "target" => p.targets(value, &mut state.targets)?,
            _ => p.unknown_key(key, "the file"),
        }
    }
    Ok((state, p.warnings))
}

fn key_str<'k>(key: &'k Spanned<DeString<'_>>) -> &'k str {
    key.get_ref().as_ref()
}

/// One parse: the text for line numbers, the path for messages, warnings so far.
struct Parser<'t> {
    text: &'t str,
    path: &'t Path,
    warnings: Vec<Warning>,
}

impl Parser<'_> {
    /// The 1-based line holding byte `offset`.
    fn line(&self, offset: usize) -> usize {
        let end = offset.min(self.text.len());
        self.text.as_bytes()[..end]
            .iter()
            .filter(|b| **b == b'\n')
            .count()
            + 1
    }

    fn invalid(&self, span: Range<usize>, key: &str, reason: Reason, why: String) -> Error {
        self.invalid_at(self.line(span.start), None, key, reason, why)
    }

    /// `desired_state_invalid` at `line` (and, for a duplicate, the `first` line too).
    fn invalid_at(
        &self,
        line: usize,
        first: Option<usize>,
        key: &str,
        reason: Reason,
        why: String,
    ) -> Error {
        let path = self.path.display();
        let fix = match reason {
            Reason::NewerSchema => Suggestion::text("upgrade omaddons"),
            Reason::UnknownFamily => {
                let names: Vec<&str> = Family::ALL
                    .iter()
                    .filter(|f| **f != Family::Unknown)
                    .map(|f| f.as_str())
                    .collect();
                Suggestion::text(format!(
                    "use a family name ({}) on line {line} of {path}",
                    names.join(", ")
                ))
            }
            Reason::Duplicate => Suggestion::text(format!(
                "keep one of the entries on lines {} and {line} of {path}",
                first.unwrap_or(line)
            )),
            Reason::BadValue => Suggestion::text(format!("correct line {line} of {path}")),
        };
        let mut details = json!({
            "path": path.to_string(),
            "line": line,
            "key": key,
            "reason": reason.wire(),
        });
        if let (Some(first), Some(map)) = (first, details.as_object_mut()) {
            map.insert("first_line".into(), json!(first));
        }
        Error::new(
            ErrorCode::DesiredStateInvalid,
            format!("{path} line {line}: {why}"),
            fix,
        )
        .details(details)
    }

    fn duplicate(&self, key: &str, a: usize, b: usize, what: &str) -> Error {
        let (first, later) = (a.min(b), a.max(b));
        self.invalid_at(
            later,
            Some(first),
            key,
            Reason::Duplicate,
            format!("{what} is listed twice (lines {first} and {later})"),
        )
    }

    fn unknown_key(&mut self, key: &Spanned<DeString<'_>>, within: &str) {
        let line = self.line(key.span().start);
        let name = key_str(key);
        self.warnings.push(
            Warning::new(
                WarningCode::DesiredUnknownKey,
                format!(
                    "{} line {line}: unknown key {name:?} in {within} is ignored",
                    self.path.display()
                ),
            )
            .with_fix(Suggestion::text(format!(
                "remove it, or correct its spelling (line {line})"
            ))),
        );
    }

    fn schema_version(&self, value: &Spanned<DeValue<'_>>) -> Result<String> {
        let key = "schema_version";
        let DeValue::String(version) = value.get_ref() else {
            let why = "schema_version is a string such as \"1.0\"".to_owned();
            return Err(self.invalid(value.span(), key, Reason::BadValue, why));
        };
        let major = version
            .split('.')
            .next()
            .and_then(|m| m.parse::<u32>().ok());
        match major {
            Some(1) => Ok(version.to_string()),
            Some(newer) if newer > 1 => Err(self.invalid(
                value.span(),
                key,
                Reason::NewerSchema,
                format!("it is schema {version}, newer than this omaddons reads (1.x)"),
            )),
            _ => Err(self.invalid(
                value.span(),
                key,
                Reason::BadValue,
                format!("schema_version {version:?} is not a version such as \"1.0\""),
            )),
        }
    }

    fn table<'v, 'i>(&self, value: &'v Spanned<DeValue<'i>>, key: &str) -> Result<&'v DeTable<'i>> {
        match value.get_ref() {
            DeValue::Table(table) => Ok(table),
            _ => Err(self.invalid(
                value.span(),
                key,
                Reason::BadValue,
                format!("{key} is a table"),
            )),
        }
    }

    fn families(
        &mut self,
        value: &Spanned<DeValue<'_>>,
        out: &mut BTreeMap<Family, Section>,
    ) -> Result<()> {
        for (key, section) in self.table(value, "family")? {
            let name = key_str(key);
            let label = format!("family.{name}");
            let family = Family::from_name(name)
                .filter(|f| *f != Family::Unknown)
                .ok_or_else(|| {
                    let why = format!("{name:?} is not a family");
                    self.invalid(key.span(), &label, Reason::UnknownFamily, why)
                })?;
            let parsed = self.section(section, &label)?;
            if let Some(first) = out.get(&family) {
                let what = format!("[family.{}]", family.as_str());
                return Err(self.duplicate(&label, first.line, parsed.line, &what));
            }
            out.insert(family, parsed);
        }
        Ok(())
    }

    fn targets(
        &mut self,
        value: &Spanned<DeValue<'_>>,
        out: &mut BTreeMap<String, Section>,
    ) -> Result<()> {
        for (key, section) in self.table(value, "target")? {
            let id = key_str(key);
            let label = format!("target.{id}");
            if !is_target_id(id) {
                let why = format!(
                    "{id:?} is not a target id: letters, digits and @._-, starting with a letter or digit"
                );
                return Err(self.invalid(key.span(), &label, Reason::BadValue, why));
            }
            let parsed = self.section(section, &label)?;
            if let Some((_, first)) = out.iter().find(|(k, _)| k.eq_ignore_ascii_case(id)) {
                let what = format!("[target.{id:?}]");
                return Err(self.duplicate(&label, first.line, parsed.line, &what));
            }
            out.insert(id.to_owned(), parsed);
        }
        Ok(())
    }

    fn section(&mut self, value: &Spanned<DeValue<'_>>, label: &str) -> Result<Section> {
        let mut section = Section {
            entries: Vec::new(),
            line: self.line(value.span().start),
        };
        for (key, v) in self.table(value, label)? {
            match key_str(key) {
                "addons" => self.strings(v, &format!("{label}.addons"), &mut section)?,
                "addon" => self.tables(v, &format!("{label}.addon"), &mut section)?,
                _ => self.unknown_key(key, &format!("[{label}]")),
            }
        }
        // Lists and tables, in file order.
        section.entries.sort_by_key(|e| e.line);
        for (i, later) in section.entries.iter().enumerate() {
            let keys = later.keys();
            if let Some(first) = section.entries[..i]
                .iter()
                .find(|e| e.keys().iter().any(|k| keys.contains(k)))
            {
                let what = format!("{:?}", later.request());
                return Err(self.duplicate(label, first.line, later.line, &what));
            }
        }
        Ok(section)
    }

    /// `addons = ["name", "gh:owner/repo", …]`.
    fn strings(
        &self,
        value: &Spanned<DeValue<'_>>,
        key: &str,
        section: &mut Section,
    ) -> Result<()> {
        let DeValue::Array(items) = value.get_ref() else {
            let why = format!("{key} is a list of names and refs");
            return Err(self.invalid(value.span(), key, Reason::BadValue, why));
        };
        for item in items.iter() {
            let text = self.text(item, key)?;
            // A canonical ref is kept as one; anything else is resolved later.
            let canonical = text.parse::<Ref>().is_ok();
            section.entries.push(Entry {
                name: (!canonical).then(|| text.clone()),
                r#ref: canonical.then_some(text),
                line: self.line(item.span().start),
                ..Entry::named("")
            });
        }
        Ok(())
    }

    /// `[[….addon]]` tables.
    fn tables(
        &mut self,
        value: &Spanned<DeValue<'_>>,
        key: &str,
        section: &mut Section,
    ) -> Result<()> {
        let DeValue::Array(items) = value.get_ref() else {
            let why = format!("{key} is a list of tables ([[{key}]])");
            return Err(self.invalid(value.span(), key, Reason::BadValue, why));
        };
        for item in items.iter() {
            let DeValue::Table(table) = item.get_ref() else {
                let why = format!("each {key} is a table");
                return Err(self.invalid(item.span(), key, Reason::BadValue, why));
            };
            let entry = self.entry(table, item.span(), key)?;
            section.entries.push(entry);
        }
        Ok(())
    }

    fn entry(&mut self, table: &DeTable<'_>, span: Range<usize>, label: &str) -> Result<Entry> {
        let mut entry = Entry {
            name: None,
            line: self.line(span.start),
            ..Entry::named("")
        };
        for (key, v) in table {
            let k = key_str(key);
            let at = format!("{label}.{k}");
            match k {
                "name" => entry.name = Some(self.text(v, &at)?),
                "ref" => entry.r#ref = Some(self.text(v, &at)?),
                "version" => entry.version = Some(self.text(v, &at)?),
                "channel" => {
                    entry.channel = match self.text(v, &at)?.as_str() {
                        "stable" => Channel::Stable,
                        "beta" => Channel::Beta,
                        "alpha" => Channel::Alpha,
                        other => {
                            let why = format!("channel {other:?} is not stable, beta or alpha");
                            return Err(self.invalid(v.span(), &at, Reason::BadValue, why));
                        }
                    }
                }
                "pinned" => entry.pinned = self.boolean(v, &at)?,
                "dependency" => entry.dependency = self.boolean(v, &at)?,
                "sha256" => {
                    let hash = self.text(v, &at)?;
                    if !is_sha256(&hash) {
                        let why = "sha256 is 64 lowercase hex digits".to_owned();
                        return Err(self.invalid(v.span(), &at, Reason::BadValue, why));
                    }
                    entry.sha256 = Some(hash);
                }
                "revision" => entry.revision = Some(self.revision(v, &at)?),
                "any_flavor" => entry.any_flavor = self.boolean(v, &at)?,
                _ => self.unknown_key(key, &format!("an [[{label}]] table")),
            }
        }
        if entry.name.is_none() && entry.r#ref.is_none() {
            let why = format!("an [[{label}]] table needs a name or a ref");
            return Err(self.invalid(span, label, Reason::BadValue, why));
        }
        Ok(entry)
    }

    /// A string with something in it.
    fn text(&self, value: &Spanned<DeValue<'_>>, key: &str) -> Result<String> {
        match value.get_ref() {
            DeValue::String(s) if !s.trim().is_empty() => Ok(s.to_string()),
            DeValue::String(_) => Err(self.invalid(
                value.span(),
                key,
                Reason::BadValue,
                format!("{key} is empty"),
            )),
            _ => Err(self.invalid(
                value.span(),
                key,
                Reason::BadValue,
                format!("{key} is a string"),
            )),
        }
    }

    /// A revision: a whole number from 1 up.
    fn revision(&self, value: &Spanned<DeValue<'_>>, key: &str) -> Result<u32> {
        let parsed = match value.get_ref() {
            DeValue::Integer(i) => u32::from_str_radix(&i.as_str().replace('_', ""), i.radix())
                .ok()
                .filter(|r| *r > 0),
            _ => None,
        };
        parsed.ok_or_else(|| {
            self.invalid(
                value.span(),
                key,
                Reason::BadValue,
                format!("{key} is a revision number, 1 or more"),
            )
        })
    }

    fn boolean(&self, value: &Spanned<DeValue<'_>>, key: &str) -> Result<bool> {
        match value.get_ref() {
            DeValue::Boolean(b) => Ok(*b),
            _ => Err(self.invalid(
                value.span(),
                key,
                Reason::BadValue,
                format!("{key} is true or false"),
            )),
        }
    }
}

/// The target id grammar (`^[a-z0-9][a-z0-9@._-]*$`), ignoring ASCII case.
fn is_target_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "@._-".contains(c))
}

fn is_sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `desired_unknown_target` for each `[target.<id>]` section naming none of `known`
/// (compared ignoring ASCII case): pass `Resolved::known_ids`, so discovered but
/// unregistered targets and registered but missing ones count as known. A dotfile may
/// list clients this machine lacks, so it is a warning.
pub fn validate(state: &DesiredState, known: &[&str]) -> Vec<Warning> {
    state
        .targets
        .iter()
        .filter(|(id, _)| !known.iter().any(|k| k.eq_ignore_ascii_case(id)))
        .map(|(id, section)| {
            Warning::new(
                WarningCode::DesiredUnknownTarget,
                format!(
                    "[target.{id:?}] (line {}) names no known client; its entries are skipped",
                    section.line
                ),
            )
            .with_fix(Suggestion::command(["omaddons", "targets"]))
        })
        .collect()
}

/// Which section an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Family,
    Target,
}

/// One entry a target wants, and what it resolved to there.
#[derive(Debug)]
pub struct Desired<'s> {
    pub entry: &'s Entry,
    pub layer: Layer,
    pub resolved: Result<Ref>,
}

impl Desired<'_> {
    /// Whether `self` and `other` name one addon: the same resolved ref, else (when
    /// either did not resolve) the same request, ignoring ASCII case.
    pub fn same_addon(&self, other: &Desired<'_>) -> bool {
        match (&self.resolved, &other.resolved) {
            (Ok(a), Ok(b)) => a.identity() == b.identity(),
            _ => self
                .entry
                .request()
                .eq_ignore_ascii_case(other.entry.request()),
        }
    }
}

/// What `target` wants: the section of its family when it is a live target (never for
/// a test target), then its own `[target.<id>]` section, each entry resolved by
/// `resolve`. An entry of the target's own section replaces every family entry naming
/// the same addon. Duplicates left within one layer are `sync`'s to report.
pub fn desired_for<'s>(
    state: &'s DesiredState,
    target: &Target,
    mut resolve: impl FnMut(&Entry) -> Result<Ref>,
) -> Vec<Desired<'s>> {
    let family = state
        .families
        .get(&target.family)
        .filter(|_| target.kind == TargetKind::Live);
    let mut out: Vec<Desired<'s>> = family
        .into_iter()
        .flat_map(|s| &s.entries)
        .map(|entry| Desired {
            entry,
            layer: Layer::Family,
            resolved: resolve(entry),
        })
        .collect();
    let own: Vec<Desired<'s>> = state
        .targets
        .iter()
        .filter(|(id, _)| id.eq_ignore_ascii_case(&target.id))
        .flat_map(|(_, s)| &s.entries)
        .map(|entry| Desired {
            entry,
            layer: Layer::Target,
            resolved: resolve(entry),
        })
        .collect();
    out.retain(|f| !own.iter().any(|t| t.same_addon(f)));
    out.extend(own);
    out
}

/// The canonical file: the header, `schema_version`, family sections in family order,
/// then target sections by id (each id quoted), entries sorted by name in table form,
/// with `channel`, `pinned` and `dependency` only when not their defaults.
pub fn render(state: &DesiredState) -> String {
    let mut out = format!("{HEADER}\nschema_version = {}\n", quoted(SCHEMA_VERSION));
    for (family, section) in &state.families {
        render_section(&mut out, &format!("family.{}", family.as_str()), section);
    }
    for (id, section) in &state.targets {
        render_section(&mut out, &format!("target.{}", quoted(id)), section);
    }
    out
}

fn render_section(out: &mut String, header: &str, section: &Section) {
    out.push('\n');
    if section.entries.is_empty() {
        out.push_str(&format!("[{header}]\n"));
        return;
    }
    let mut entries: Vec<&Entry> = section.entries.iter().collect();
    // By name, else ref, ignoring case; ties by the exact strings.
    entries.sort_by_cached_key(|e| {
        let name = e.name.as_deref().or(e.r#ref.as_deref()).unwrap_or_default();
        (name.to_ascii_lowercase(), e.name.clone(), e.r#ref.clone())
    });
    for (i, e) in entries.into_iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("[[{header}.addon]]\n"));
        let mut line = |key: &str, value: String| out.push_str(&format!("{key} = {value}\n"));
        if let Some(name) = &e.name {
            line("name", quoted(name));
        }
        if let Some(r) = &e.r#ref {
            line("ref", quoted(r));
        }
        if e.channel != Channel::Stable {
            line("channel", quoted(channel_name(e.channel)));
        }
        if let Some(version) = &e.version {
            line("version", quoted(version));
        }
        if e.pinned {
            line("pinned", toml::Value::Boolean(true).to_string());
        }
        if e.dependency {
            line("dependency", toml::Value::Boolean(true).to_string());
        }
        if let Some(hash) = &e.sha256 {
            line("sha256", quoted(hash));
        }
        if let Some(revision) = e.revision {
            line("revision", revision.to_string());
        }
        if e.any_flavor {
            line("any_flavor", toml::Value::Boolean(true).to_string());
        }
    }
}

/// `s` as a TOML string, formatted by the `toml` crate.
fn quoted(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

const fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "stable",
        Channel::Beta => "beta",
        Channel::Alpha => "alpha",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use proptest::prelude::*;

    use super::*;

    fn schema_keys(value: &serde_json::Value) -> std::collections::BTreeSet<&str> {
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn desired_state_schema_names_the_parser_keys() {
        let schema = crate::contract::schema::generator()
            .root_schema_for::<DesiredStateSchema>()
            .to_value();
        let section = &schema["properties"]["target"]["additionalProperties"];
        let section = schema
            .pointer(&section["$ref"].as_str().unwrap()[1..])
            .unwrap();
        let entry = &section["properties"]["addon"]["items"];
        let entry = schema
            .pointer(&entry["$ref"].as_str().unwrap()[1..])
            .unwrap();
        for (case, properties, listed) in [
            ("file", &schema["properties"], FILE_KEYS),
            ("section", &section["properties"], SECTION_KEYS),
            ("entry", &entry["properties"], ENTRY_KEYS),
        ] {
            assert_eq!(
                schema_keys(properties),
                listed.iter().copied().collect(),
                "{case}: {schema}"
            );
        }
        for (case, value) in [("file", &schema), ("section", section), ("entry", entry)] {
            assert!(
                value.get("required").is_none(),
                "{case}: every key may be omitted: {schema}"
            );
        }
    }

    #[test]
    fn every_listed_key_parses_without_a_warning() {
        let mut cases = Vec::new();
        for &key in FILE_KEYS {
            let template = match key {
                "schema_version" => "KEY = \"1.0\"\n",
                "family" => "[KEY.mainline]\naddons = [\"Foo\"]\n",
                "target" => "[KEY.retail]\naddons = [\"Foo\"]\n",
                _ => "",
            };
            assert!(!template.is_empty(), "file key {key} needs a fixture");
            cases.push((format!("file_{key}"), key, template.to_owned()));
        }
        for &key in SECTION_KEYS {
            let template = match key {
                "addons" => "[target.retail]\nKEY = [\"Foo\"]\n",
                "addon" => "[[target.retail.KEY]]\nname = \"Foo\"\n",
                _ => "",
            };
            assert!(!template.is_empty(), "section key {key} needs a fixture");
            cases.push((format!("section_{key}"), key, template.to_owned()));
        }
        for &key in ENTRY_KEYS {
            let value = match key {
                "name" => "\"Foo\"".into(),
                "ref" => "\"gh:o/foo\"".into(),
                "version" => "\"v1\"".into(),
                "channel" => "\"beta\"".into(),
                "pinned" | "dependency" | "any_flavor" => "true".into(),
                "sha256" => format!("\"{}\"", "a".repeat(64)),
                "revision" => "1".into(),
                _ => String::new(),
            };
            assert!(!value.is_empty(), "entry key {key} needs a fixture");
            let identity = if key == "name" {
                "ref = \"gh:o/foo\""
            } else {
                "name = \"Foo\""
            };
            let template = format!("[[target.retail.addon]]\n{identity}\nKEY = {value}\n");
            cases.push((format!("entry_{key}"), key, template));
        }
        for (case, key, template) in cases {
            for (variant, spelling, warning_count) in [
                ("known", key.to_owned(), 0),
                ("unknown", format!("{key}x"), 1),
            ] {
                let text = template.replace("KEY", &spelling);
                let result = parse(&text, Path::new("addons.toml"));
                assert!(result.is_ok(), "{case}/{variant}: {text}\n{result:?}");
                let (_, warnings) = result.unwrap();
                assert_eq!(
                    warnings.len(),
                    warning_count,
                    "{case}/{variant}: {text}\n{warnings:?}"
                );
                for warning in warnings {
                    assert_eq!(
                        warning.code,
                        WarningCode::DesiredUnknownKey,
                        "{case}/{variant}: {warning:?}"
                    );
                    assert!(
                        warning.message.contains(&spelling),
                        "{case}/{variant}: {warning:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_parser_match_arms_are_the_listed_keys() {
        use crate::source_scan::{Literals, production_code};
        let code = production_code(include_str!("desired.rs"), Literals::Keep).unwrap();
        for (case, start, end, indent, listed) in [
            ("file", "pub fn parse(", "fn key_str", 12, FILE_KEYS),
            (
                "section",
                "    fn section(",
                "    fn strings(",
                16,
                SECTION_KEYS,
            ),
            ("entry", "    fn entry(", "    fn text(", 16, ENTRY_KEYS),
        ] {
            let body = code.split(start).nth(1).unwrap().split(end).next().unwrap();
            let prefix = format!("{}\"", " ".repeat(indent));
            let actual: std::collections::BTreeSet<_> = body
                .lines()
                .filter_map(|line| {
                    let rest = line.strip_prefix(&prefix)?;
                    let (literal, tail) = rest.split_once('"')?;
                    tail.trim_start().starts_with("=>").then_some(literal)
                })
                .collect();
            let expected: std::collections::BTreeSet<_> = listed.iter().copied().collect();
            assert_eq!(
                expected.len(),
                listed.len(),
                "{case}: duplicate key declaration"
            );
            assert_eq!(actual, expected, "{case}: parser match arms\n{body}");
        }
    }

    #[test]
    fn desired_state_family_keys_exclude_unknown() {
        let schema = crate::contract::schema::generator()
            .root_schema_for::<DesiredStateSchema>()
            .to_value();
        let expected: std::collections::BTreeSet<_> = Family::ALL
            .into_iter()
            .filter(|family| *family != Family::Unknown)
            .map(Family::as_str)
            .collect();
        assert_eq!(
            schema_keys(&schema["properties"]["family"]["properties"]),
            expected,
            "{schema}"
        );
        assert!(parse("[family.unknown]\n", Path::new("addons.toml")).is_err());
    }

    /// The documented example.
    const EXAMPLE: &str = r#"# Desired addon sets for omaddons. `omaddons sync` makes each client match this file.
schema_version = "1.0"

[family.mainline]                       # every live mainline client
addons = ["WeakAuras", "DBM", "wowi:5995", "LittleWigs"]

[[family.mainline.addon]]               # table form when options are needed
name = "BigWigs"
ref = "gh:BigWigsMods/BigWigs"
dependency = true                       # a lock for LittleWigs' dependency, not a request
version = "v428"

[[family.mainline.addon]]
name = "Plater"
version = "Plater-v661"
pinned = true

[family.vanilla]
addons = ["Questie", "DBM"]

[target."wotlk"]                        # one client by id; the only way to address test clients
addons = ["git:https://github.com/fondlez/GroupBulletinBoard#main"]
"#;

    fn path() -> PathBuf {
        PathBuf::from("/home/u/.config/omaddons/addons.toml")
    }

    fn ok(text: &str) -> (DesiredState, Vec<Warning>) {
        parse(text, &path()).unwrap()
    }

    /// The error's `(line, key, reason)`.
    fn refused(text: &str) -> (u64, String, String, Error) {
        let e = parse(text, &path()).unwrap_err();
        assert_eq!(e.code, ErrorCode::DesiredStateInvalid, "{e:?}");
        assert_eq!(e.details["path"], path().display().to_string());
        let line = e.details["line"].as_u64().unwrap();
        let key = e.details["key"].as_str().unwrap().to_owned();
        let reason = e.details["reason"].as_str().unwrap().to_owned();
        (line, key, reason, e)
    }

    fn names(section: &Section) -> Vec<&str> {
        section.entries.iter().map(Entry::request).collect()
    }

    #[test]
    fn the_documented_example_parses_without_warnings() {
        let (state, warnings) = ok(EXAMPLE);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(state.schema_version, "1.0");
        let mainline = &state.families[&Family::Mainline];
        assert_eq!(
            names(mainline),
            [
                "WeakAuras",
                "DBM",
                "wowi:5995",
                "LittleWigs",
                "gh:BigWigsMods/BigWigs",
                "Plater"
            ]
        );
        let bigwigs = &mainline.entries[4];
        assert_eq!(
            (
                bigwigs.name.as_deref(),
                bigwigs.dependency,
                bigwigs.version.as_deref(),
                bigwigs.line
            ),
            (Some("BigWigs"), true, Some("v428"), 7)
        );
        let plater = &mainline.entries[5];
        assert_eq!(
            (plater.pinned, plater.channel, plater.line),
            (true, Channel::Stable, 13)
        );
        assert_eq!(mainline.entries[0].line, 5);
        assert_eq!(names(&state.families[&Family::Vanilla]), ["Questie", "DBM"]);
        assert_eq!(state.targets["wotlk"].line, 21);
        assert_eq!(state.targets["wotlk"].entries[0].line, 22);
    }

    #[test]
    fn defaults_hold_whatever_the_config_says() {
        let (state, _) = ok("[[family.mainline.addon]]\nname = \"DBM\"\n");
        assert_eq!(
            state.schema_version, "1.0",
            "a missing schema_version is 1.0"
        );
        let e = &state.families[&Family::Mainline].entries[0];
        assert_eq!(
            (e.channel, e.pinned, e.dependency, e.version.as_deref()),
            (Channel::Stable, false, false, None)
        );
    }

    #[test]
    fn unknown_families_are_refused_with_the_line() {
        for text in [
            "schema_version = \"1.0\"\n\n[family.mainlien]\naddons = []\n",
            "schema_version = \"1.0\"\n\n[family.unknown]\naddons = []\n",
        ] {
            let (line, key, reason, e) = refused(text);
            assert_eq!((line, reason.as_str()), (3, "unknown_family"), "{e:?}");
            assert!(key.starts_with("family."), "{key}");
            assert!(e.fix.display().starts_with("use a family name"));
        }
        // Family names ignore case, like everywhere else.
        let (state, _) = ok("[family.Mainline]\naddons = [\"DBM\"]\n");
        assert!(state.families.contains_key(&Family::Mainline));
    }

    #[test]
    fn unknown_keys_warn_with_the_line_and_parsing_goes_on() {
        let text = "schema_version = \"1.0\"\ncolour = 1\n\n[family.mainline]\naddons = [\"DBM\"]\nadons = []\n\n[[family.mainline.addon]]\nname = \"X\"\npined = true\n";
        let (state, warnings) = ok(text);
        let lines: Vec<(WarningCode, bool)> = warnings
            .iter()
            .map(|w| (w.code, w.message.contains("line ")))
            .collect();
        assert_eq!(lines.len(), 3, "{warnings:?}");
        assert!(
            lines
                .iter()
                .all(|(c, l)| *c == WarningCode::DesiredUnknownKey && *l)
        );
        let messages: Vec<&str> = warnings.iter().map(|w| w.message.as_str()).collect();
        for (key, line) in [("colour", 2), ("adons", 6), ("pined", 10)] {
            assert!(
                messages
                    .iter()
                    .any(|m| m.contains(&format!("line {line}: unknown key {key:?}"))),
                "{key}: {messages:?}"
            );
        }
        assert_eq!(names(&state.families[&Family::Mainline]), ["DBM", "X"]);
    }

    #[test]
    fn a_name_twice_in_one_section_is_refused_citing_both_lines() {
        let cases = [
            // Two strings, ignoring case.
            ("[family.mainline]\naddons = [\"DBM\",\n  \"dbm\"]\n", 2, 3),
            // A string and a table.
            (
                "[family.mainline]\naddons = [\"Plater\"]\n\n[[family.mainline.addon]]\nname = \"Plater\"\npinned = true\n",
                2,
                4,
            ),
            // A bare string and another entry's ref.
            (
                "[[family.mainline.addon]]\nname = \"BigWigs\"\nref = \"gh:o/bw\"\n\n[family.mainline]\naddons = [\"gh:o/bw\"]\n",
                1,
                6,
            ),
        ];
        for (text, first, later) in cases {
            let (line, _, reason, e) = refused(text);
            assert_eq!((line, reason.as_str()), (later, "duplicate"), "{text}");
            assert_eq!(e.details["first_line"], first, "{text}");
            assert!(e.fix.display().starts_with("keep one of the entries"));
            assert!(
                e.message.contains(&format!("lines {first} and {later}")),
                "{}",
                e.message
            );
        }
        // The same name in two sections is fine.
        ok("[family.mainline]\naddons = [\"DBM\"]\n\n[family.vanilla]\naddons = [\"DBM\"]\n");
        // Sections twice: a family by case, a target id by case.
        let (_, _, reason, _) = refused("[family.mainline]\n\n[family.MAINLINE]\n");
        assert_eq!(reason, "duplicate");
        let (_, _, reason, _) = refused("[target.wotlk]\n\n[target.WotLK]\n");
        assert_eq!(reason, "duplicate");
    }

    #[test]
    fn a_newer_schema_is_refused_with_upgrade_as_the_fix() {
        let (line, key, reason, e) =
            refused("\nschema_version = \"2.0\"\n\n[family.bogus]\naddons = 3\n");
        // The version is judged first, whatever else the file holds.
        assert_eq!(
            (line, key.as_str(), reason.as_str()),
            (2, "schema_version", "newer_schema")
        );
        assert_eq!(e.fix.display(), "upgrade omaddons");
        assert_eq!(ok("schema_version = \"1.7\"\n").0.schema_version, "1.7");
        let (_, _, reason, _) = refused("schema_version = 1\n");
        assert_eq!(reason, "bad_value");
    }

    #[test]
    fn bad_values_are_refused_with_their_key_and_line() {
        let cases: &[(&str, u64, &str)] = &[
            (
                "[family.mainline]\naddons = \"DBM\"\n",
                2,
                "family.mainline.addons",
            ),
            (
                "[family.mainline]\naddons = [\"DBM\", 3]\n",
                2,
                "family.mainline.addons",
            ),
            (
                "[family.mainline]\naddons = [\"  \"]\n",
                2,
                "family.mainline.addons",
            ),
            (
                "[[family.mainline.addon]]\nversion = \"1\"\n",
                1,
                "family.mainline.addon",
            ),
            (
                "[[family.mainline.addon]]\nname = \"X\"\nchannel = \"nightly\"\n",
                3,
                "family.mainline.addon.channel",
            ),
            (
                "[[family.mainline.addon]]\nname = \"X\"\npinned = \"yes\"\n",
                3,
                "family.mainline.addon.pinned",
            ),
            (
                "[[family.mainline.addon]]\nname = \"X\"\nsha256 = \"ABC\"\n",
                3,
                "family.mainline.addon.sha256",
            ),
            (
                "[[family.mainline.addon]]\nname = \"X\"\nrevision = 0\n",
                3,
                "family.mainline.addon.revision",
            ),
            (
                "[[family.mainline.addon]]\nname = \"X\"\nrevision = \"401\"\n",
                3,
                "family.mainline.addon.revision",
            ),
            (
                "[[family.mainline.addon]]\nname = \"X\"\nrevision = 4294967296\n",
                3,
                "family.mainline.addon.revision",
            ),
            ("family = 3\n", 1, "family"),
            ("[target.\"my client\"]\n", 1, "target.my client"),
            // Broken TOML: where the parser says.
            ("[family.mainline]\naddons = [\n", 2, ""),
        ];
        for (text, want_line, want_key) in cases {
            let (line, key, reason, e) = refused(text);
            assert_eq!(
                (line, key.as_str(), reason.as_str()),
                (*want_line, *want_key, "bad_value"),
                "{text}\n{e:?}"
            );
            assert!(
                e.fix.display().starts_with("correct line"),
                "{}",
                e.fix.display()
            );
            assert_eq!(e.code.exit().code(), 2);
            assert!(!e.retryable);
        }
        let hash = "0123456789abcdef".repeat(4);
        let text = format!("[[family.mainline.addon]]\nname = \"X\"\nsha256 = \"{hash}\"\n");
        assert_eq!(
            ok(&text).0.families[&Family::Mainline].entries[0].sha256,
            Some(hash)
        );
        let text = "[[family.mainline.addon]]\nname = \"X\"\nrevision = 0x191\n";
        assert_eq!(
            ok(text).0.families[&Family::Mainline].entries[0].revision,
            Some(401)
        );
    }

    #[test]
    fn unknown_targets_are_warnings_known_ones_are_not() {
        let (state, _) =
            ok("[target.wotlk]\naddons = [\"x\"]\n\n[target.\"PSERVER@1\"]\n\n[target.gone]\n");
        let warnings = validate(&state, &["retail", "wotlk", "pserver@1"]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].code, WarningCode::DesiredUnknownTarget);
        assert!(warnings[0].message.contains("\"gone\"") && warnings[0].message.contains("line 6"));
        assert_eq!(
            warnings[0].fix.as_ref().map(Suggestion::display),
            Some("omaddons targets")
        );
    }

    fn client(id: &str, family: Family, kind: TargetKind) -> Target {
        let mut t = crate::engine::testing::target(Path::new("/c/_retail_"));
        t.id = id.to_owned();
        t.family = family;
        t.kind = kind;
        t
    }

    /// Resolves `gh:`/`git:` strings as refs and names through a tiny catalog.
    fn resolver(entry: &Entry) -> Result<Ref> {
        let request = entry.request();
        let canonical = match request.to_ascii_lowercase().as_str() {
            "bigwigs" => "gh:BigWigsMods/BigWigs".to_owned(),
            "dbm" => "gh:DeadlyBossMods/DeadlyBossMods".to_owned(),
            _ => request.to_owned(),
        };
        canonical.parse::<Ref>().map_err(|_| {
            Error::new(
                ErrorCode::NotFound,
                format!("{request}?"),
                Suggestion::text("x"),
            )
        })
    }

    #[test]
    fn family_sections_apply_only_to_live_targets_and_target_sections_override() {
        let text = r#"
[family.mainline]
addons = ["DBM", "BigWigs", "Typo"]

[family.wrath]
addons = ["Questie"]

[target.retail]
addons = ["gh:BigWigsMods/BigWigs"]

[[target.retail.addon]]
name = "typo"
pinned = true

[target.ptr]
addons = ["DBM"]
"#;
        let (state, _) = ok(text);
        let retail = client("retail", Family::Mainline, TargetKind::Live);
        let want = desired_for(&state, &retail, resolver);
        let got: Vec<(&str, Layer)> = want.iter().map(|d| (d.entry.request(), d.layer)).collect();
        assert_eq!(
            got,
            [
                ("DBM", Layer::Family),
                // BigWigs by name and by ref resolve alike: the target's wins.
                ("gh:BigWigsMods/BigWigs", Layer::Target),
                // An unresolved name is matched by name, ignoring case.
                ("typo", Layer::Target),
            ]
        );
        assert!(want[2].entry.pinned && want[2].resolved.is_err());

        // A test client of the family takes only its own section.
        let ptr = client("ptr", Family::Mainline, TargetKind::Test);
        let want = desired_for(&state, &ptr, resolver);
        let got: Vec<(&str, Layer)> = want.iter().map(|d| (d.entry.request(), d.layer)).collect();
        assert_eq!(got, [("DBM", Layer::Target)]);

        // Another family's live client takes its family; ids ignore case.
        let wrath = client("WotLK", Family::Wrath, TargetKind::Live);
        let want = desired_for(&state, &wrath, resolver);
        assert_eq!(want.len(), 1);
        assert_eq!(want[0].entry.request(), "Questie");
    }

    #[test]
    fn render_is_canonical() {
        let (state, _) = ok(EXAMPLE);
        let rendered = render(&state);
        insta::assert_snapshot!("desired_render_example", rendered);
        assert_eq!(render(&ok(&rendered).0), rendered);
        // Defaults are left out; a beta channel is written.
        let mut beta = Entry::named("WeakAuras");
        beta.channel = Channel::Beta;
        let state = DesiredState {
            targets: BTreeMap::from([(
                "pserver@1".to_owned(),
                Section {
                    entries: vec![Entry::named("pfQuest"), beta],
                    line: 0,
                },
            )]),
            ..DesiredState::default()
        };
        let rendered = render(&state);
        assert!(rendered.contains("[[target.\"pserver@1\".addon]]\nname = \"pfQuest\"\n\n[[target.\"pserver@1\".addon]]\nname = \"WeakAuras\"\nchannel = \"beta\"\n"), "{rendered}");
        assert!(!rendered.contains("pinned") && !rendered.contains("dependency"));
    }

    /// `any_flavor = true` reads on a table entry and renders back unchanged; absent, it
    /// is false and not written.
    #[test]
    fn any_flavor_round_trips_on_a_table_entry() {
        let text = format!(
            "{HEADER}\nschema_version = \"1.0\"\n\n[[family.mists.addon]]\nname = \"Foo\"\nany_flavor = true\n"
        );
        let (state, warnings) = ok(&text);
        assert!(warnings.is_empty(), "{warnings:?}");
        let entry = &state.families[&Family::Mists].entries[0];
        assert!(entry.any_flavor);
        assert_eq!(render(&state), text);
        assert!(
            !ok(EXAMPLE)
                .0
                .families
                .values()
                .flat_map(|s| &s.entries)
                .any(|e| e.any_flavor)
        );
    }

    fn entry_strategy() -> impl Strategy<Value = Entry> {
        (
            proptest::option::of("\\PC{1,12}"),
            proptest::option::of("[a-z]{2,4}:[^\\s]{1,16}"),
            prop_oneof![
                Just(Channel::Stable),
                Just(Channel::Beta),
                Just(Channel::Alpha)
            ],
            proptest::option::of("\\PC{1,10}"),
            any::<bool>(),
            any::<bool>(),
            proptest::option::of("[0-9a-f]{64}"),
            proptest::option::of(1u32..),
            any::<bool>(),
        )
            .prop_filter("a name or a ref", |(n, r, ..)| {
                n.as_ref().is_some_and(|n| !n.trim().is_empty())
                    || r.as_ref().is_some_and(|r| !r.trim().is_empty())
            })
            .prop_map(
                |(name, r, channel, version, pinned, dependency, sha256, revision, any_flavor)| {
                    Entry {
                        name: name.filter(|n| !n.trim().is_empty()),
                        r#ref: r,
                        channel,
                        version: version.filter(|v| !v.trim().is_empty()),
                        pinned,
                        dependency,
                        sha256,
                        revision,
                        any_flavor,
                        line: 0,
                    }
                },
            )
    }

    /// Entries whose names and refs are distinct, ignoring case.
    fn section_strategy() -> impl Strategy<Value = Section> {
        proptest::collection::vec(entry_strategy(), 0..5).prop_map(|entries| {
            let mut seen: Vec<String> = Vec::new();
            let entries = entries
                .into_iter()
                .filter(|e| {
                    let keys = e.keys();
                    let fresh = !keys.iter().any(|k| seen.contains(k));
                    if fresh {
                        seen.extend(keys);
                    }
                    fresh
                })
                .collect();
            Section { entries, line: 0 }
        })
    }

    fn state_strategy() -> impl Strategy<Value = DesiredState> {
        let families = proptest::collection::btree_map(
            proptest::sample::select(
                Family::ALL
                    .into_iter()
                    .filter(|f| *f != Family::Unknown)
                    .collect::<Vec<_>>(),
            ),
            section_strategy(),
            0..4,
        );
        let targets =
            proptest::collection::btree_map("[a-z0-9][a-z0-9@._-]{0,10}", section_strategy(), 0..3);
        (families, targets).prop_map(|(families, targets)| DesiredState {
            schema_version: SCHEMA_VERSION.to_owned(),
            families,
            targets,
        })
    }

    proptest! {
        #[test]
        fn render_parse_render_is_a_fixed_point(state in state_strategy()) {
            let once = render(&state);
            let (parsed, warnings) = parse(&once, &path()).map_err(|e| {
                TestCaseError::fail(format!("{e:?}\n{once}"))
            })?;
            prop_assert!(warnings.is_empty(), "{warnings:?}");
            prop_assert_eq!(render(&parsed), once);
        }

        #[test]
        fn arbitrary_text_parses_or_is_refused(text in "\\PC{0,200}") {
            let _ = parse(&text, &path());
        }
    }
}
