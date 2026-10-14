//! Client capability data: the product table, version-to-family tables, interface
//! numbers and `ClientRules` rows. No client version is special-cased in code.
//!
//! Detection and capabilities are data (vision D15, D29): a new flavor folder, a new
//! product code or a private-server client generation is a table row, and every row can
//! be overridden from config through [`RulesOverrides`]. The product code never decides
//! the game family; the client version does ([`RulesTable::family_for`]). This module
//! does no I/O and depends on nothing else in the crate (DEC-13).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Every TOC suffix a client or packager is known to use, without the `-`/`_` separator.
/// Archive inspection uses it to recognize addon roots. Compare case-insensitively.
///
/// Game-specific: `Standard` (retail since 12.1.5), `Forever`/`Camelot`, `Mists`, `Cata`,
/// `Wrath`, `TBC` (also Anniversary), `Vanilla`, `Plunderstorm`/`WoWLabs`, `WoWHack`.
/// Family: `Mainline`, `Classic`. Legacy: `BCC`, `WOTLKC`, which current clients no longer
/// load but which appear in the wild.
pub const KNOWN_TOC_SUFFIXES: &[&str] = &[
    "Standard",
    "Forever",
    "Camelot",
    "Mists",
    "Cata",
    "Wrath",
    "TBC",
    "Vanilla",
    "Plunderstorm",
    "WoWLabs",
    "WoWHack",
    "Mainline",
    "Classic",
    "BCC",
    "WOTLKC",
];

/// A client version such as `12.1.5.70391` or `3.3.5a`.
///
/// Serialized as `major.minor.patch[.build]`. Ordering compares the numeric tuple, with a
/// missing build before any build.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct ClientVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub build: Option<u32>,
}

impl ClientVersion {
    /// A version without a build number.
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            build: None,
        }
    }

    /// The same version with the build number dropped. Version ranges compare these, so
    /// `12.1.4.70000` is inside a range that ends at `12.1.4`.
    pub const fn without_build(self) -> Self {
        Self::new(self.major, self.minor, self.patch)
    }
}

impl FromStr for ClientVersion {
    type Err = VersionParseError;

    /// Accepts 2–4 dot-separated numbers. Trailing ASCII letters on the last number are
    /// ignored (`3.3.5a` → 3.3.5); a missing patch is 0 (`2.5` → 2.5.0).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || VersionParseError {
            input: s.to_owned(),
        };
        let parts: Vec<&str> = s.trim().split('.').collect();
        let last = parts.len().saturating_sub(1);
        let mut numbers = Vec::with_capacity(parts.len());
        for (i, part) in parts.iter().enumerate() {
            let digits = if i == last {
                part.trim_end_matches(|c: char| c.is_ascii_alphabetic())
            } else {
                part
            };
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            numbers.push(digits.parse::<u32>().map_err(|_| invalid())?);
        }
        match *numbers.as_slice() {
            [major, minor] => Ok(Self::new(major, minor, 0)),
            [major, minor, patch] => Ok(Self::new(major, minor, patch)),
            [major, minor, patch, build] => Ok(Self {
                build: Some(build),
                ..Self::new(major, minor, patch)
            }),
            _ => Err(invalid()),
        }
    }
}

impl fmt::Display for ClientVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(build) = self.build {
            write!(f, ".{build}")?;
        }
        Ok(())
    }
}

impl TryFrom<String> for ClientVersion {
    type Error = VersionParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ClientVersion> for String {
    fn from(version: ClientVersion) -> Self {
        version.to_string()
    }
}

/// A string that is not a client version.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid client version {input:?}: expected 2 to 4 dot-separated numbers, e.g. 12.1.5.70391"
)]
pub struct VersionParseError {
    pub input: String,
}

/// A game family: which addon builds a client can load. Derived from the client version,
/// never from the product code.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    Vanilla,
    Forever,
    Tbc,
    Wrath,
    Titan,
    Cata,
    Mists,
    Mainline,
    VanillaLegacy,
    TbcLegacy,
    WrathLegacy,
    CataLegacy,
    MistsLegacy,
    Unknown,
}

/// Packager aliases accepted by [`Family::from_release_flavor`], mapped to the canonical
/// `release.json` flavor name.
const RELEASE_FLAVOR_ALIASES: &[(&str, &str)] = &[
    ("tbc", "bcc"),
    ("vanilla", "classic"),
    ("wotlkc", "wrath"),
    ("camelot", "forever"),
];

impl Family {
    pub const ALL: [Self; 14] = [
        Self::Vanilla,
        Self::Forever,
        Self::Tbc,
        Self::Wrath,
        Self::Titan,
        Self::Cata,
        Self::Mists,
        Self::Mainline,
        Self::VanillaLegacy,
        Self::TbcLegacy,
        Self::WrathLegacy,
        Self::CataLegacy,
        Self::MistsLegacy,
        Self::Unknown,
    ];

    /// The wire name, e.g. `wrath-legacy`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Vanilla => "vanilla",
            Self::Forever => "forever",
            Self::Tbc => "tbc",
            Self::Wrath => "wrath",
            Self::Titan => "titan",
            Self::Cata => "cata",
            Self::Mists => "mists",
            Self::Mainline => "mainline",
            Self::VanillaLegacy => "vanilla-legacy",
            Self::TbcLegacy => "tbc-legacy",
            Self::WrathLegacy => "wrath-legacy",
            Self::CataLegacy => "cata-legacy",
            Self::MistsLegacy => "mists-legacy",
            Self::Unknown => "unknown",
        }
    }

    /// Looks a family up by its wire name, ignoring ASCII case.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|f| f.as_str().eq_ignore_ascii_case(name))
    }

    /// The private-server generations below the multi-TOC era.
    pub const fn is_legacy(self) -> bool {
        matches!(
            self,
            Self::VanillaLegacy
                | Self::TbcLegacy
                | Self::WrathLegacy
                | Self::CataLegacy
                | Self::MistsLegacy
        )
    }

    /// The BigWigs packager `release.json` flavor for this family. Legacy families and
    /// `unknown` have none: no packager metadata covers them.
    pub const fn release_flavor(self) -> Option<&'static str> {
        match self {
            Self::Mainline => Some("mainline"),
            Self::Vanilla => Some("classic"),
            Self::Tbc => Some("bcc"),
            Self::Wrath => Some("wrath"),
            Self::Titan => Some("titan"),
            Self::Cata => Some("cata"),
            Self::Mists => Some("mists"),
            Self::Forever => Some("forever"),
            Self::VanillaLegacy
            | Self::TbcLegacy
            | Self::WrathLegacy
            | Self::CataLegacy
            | Self::MistsLegacy
            | Self::Unknown => None,
        }
    }

    /// The family a `release.json` flavor name belongs to, accepting the packager aliases
    /// `tbc`, `vanilla`, `wotlkc` and `camelot`. Unknown names return `None`.
    pub fn from_release_flavor(name: &str) -> Option<Self> {
        let canonical = RELEASE_FLAVOR_ALIASES
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(name))
            .map_or(name, |&(_, canonical)| canonical);
        Self::ALL.into_iter().find(|f| {
            f.release_flavor()
                .is_some_and(|flavor| flavor.eq_ignore_ascii_case(canonical))
        })
    }
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a client was installed: a Battle.net root with `.build.info`, or a custom client
/// root with `Wow*.exe` and no `.build.info`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum InstallKind {
    Battlenet,
    Custom,
}

/// `test` for PTR, XPTR and beta products, `live` otherwise.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    Live,
    Test,
}

/// An inclusive range of numbers; `max: None` is unbounded.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct NumRange {
    pub min: u32,
    pub max: Option<u32>,
}

impl NumRange {
    pub const fn exactly(n: u32) -> Self {
        Self {
            min: n,
            max: Some(n),
        }
    }

    pub const fn between(min: u32, max: u32) -> Self {
        Self {
            min,
            max: Some(max),
        }
    }

    pub const fn at_least(min: u32) -> Self {
        Self { min, max: None }
    }

    pub fn contains(self, n: u32) -> bool {
        n >= self.min && self.max.is_none_or(|max| n <= max)
    }

    fn is_inverted(self) -> bool {
        self.max.is_some_and(|max| max < self.min)
    }
}

impl fmt::Display for NumRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.max {
            Some(max) => write!(f, "{}..={max}", self.min),
            None => write!(f, "{}..", self.min),
        }
    }
}

/// One Battle.net product (vision §4.1). Folder names are not derivable from the code
/// (`wowf` → `_forever_`), and exe names vary in case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Product {
    /// Product code, e.g. `wow_classic_era`.
    pub code: String,
    /// Flavor folder under the Battle.net root, e.g. `_classic_era_`.
    pub folder: String,
    /// Executable name inside the flavor folder; match it case-insensitively.
    pub exe: String,
    pub kind: TargetKind,
    /// Unqualified human target id, e.g. `era`.
    pub default_id: String,
}

/// One row of the version → family table. Rows are tried in order; the first match wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VersionFamilyRule {
    /// `None` matches every install kind.
    pub install_kind: Option<InstallKind>,
    pub major: NumRange,
    pub minor: NumRange,
    pub family: Family,
}

impl VersionFamilyRule {
    pub fn matches(&self, version: &ClientVersion, kind: InstallKind) -> bool {
        self.install_kind.is_none_or(|k| k == kind)
            && self.major.contains(version.major)
            && self.minor.contains(version.minor)
    }
}

/// Per-family data: the interface range the TOC verdict uses as floor and ceiling, and
/// the default human id of a custom client in this family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FamilyRow {
    pub family: Family,
    /// `None` only for `unknown`.
    pub interface_range: Option<NumRange>,
    /// Battle.net targets use [`Product::default_id`] instead. `None` for `unknown`, whose
    /// id comes from the install.
    pub default_id: Option<String>,
}

/// What one client generation can load (vision §5.3). `loaded_toc` and the compatibility
/// verdict read only these fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientRules {
    pub name: String,
    pub family: Family,
    /// `None` applies to every install kind.
    pub install_kind: Option<InstallKind>,
    pub version_min: ClientVersion,
    /// Inclusive; `None` is unbounded. Bounds ignore build numbers.
    pub version_max: Option<ClientVersion>,
    /// The client's internal game type, e.g. `standard` or `camelot`.
    pub game: String,
    /// Every name `AllowLoadGameType`/`ExcludeLoadGameType` may use for this client.
    pub game_types: Vec<String>,
    pub multi_toc: bool,
    pub comma_interface: bool,
    /// Game-level TOC suffixes in precedence order, without the separator.
    pub game_suffixes: Vec<String>,
    pub family_suffixes: Vec<String>,
    /// Retired suffixes (`BCC`, `WOTLKC`) the client still loads at game level. Empty in
    /// every builtin row; the field exists for overrides.
    pub legacy_suffixes: Vec<String>,
    pub directive_conditionals: bool,
    pub path_variables: bool,
    /// Whether the interface number includes the patch digit (12.1.5 → 120105) or not
    /// (legacy 3.3.5 → 30300).
    pub interface_includes_patch: bool,
    pub legacy: bool,
}

/// The conservative `fallback-custom` row. A deserialized `Target` carries it until
/// `Target::refresh_rules` re-derives the row from the live table.
impl Default for ClientRules {
    fn default() -> Self {
        fallback_custom()
    }
}

impl ClientRules {
    /// Whether this row applies to `kind`.
    pub fn applies_to(&self, kind: InstallKind) -> bool {
        self.install_kind.is_none_or(|k| k == kind)
    }

    /// Whether `version` is inside `version_min..=version_max`, ignoring build numbers.
    pub fn covers(&self, version: &ClientVersion) -> bool {
        let version = version.without_build();
        version >= self.version_min.without_build()
            && self
                .version_max
                .is_none_or(|max| version <= max.without_build())
    }

    fn suffixes(&self) -> impl Iterator<Item = &str> {
        self.game_suffixes
            .iter()
            .chain(&self.family_suffixes)
            .chain(&self.legacy_suffixes)
            .map(String::as_str)
    }
}

/// The client's interface number: `major*10000 + minor*100`, plus the patch where the
/// row says the client includes it.
pub fn interface_for(version: &ClientVersion, rules: &ClientRules) -> u32 {
    let base = version
        .major
        .saturating_mul(10_000)
        .saturating_add(version.minor.saturating_mul(100));
    if rules.interface_includes_patch {
        base.saturating_add(version.patch)
    } else {
        base
    }
}

/// Every table the rest of the program reads. Built from [`RulesTable::builtin`] and
/// optionally [`RulesTable::with_overrides`], both of which validate it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RulesTable {
    products: Vec<Product>,
    version_families: Vec<VersionFamilyRule>,
    families: Vec<FamilyRow>,
    client_rules: Vec<ClientRules>,
    /// Used for Battle.net clients whose version maps to no row. Battle.net clients are
    /// all multi-TOC, so it is a permissive modern row.
    fallback_battlenet: ClientRules,
    /// Used for custom clients whose version maps to no row, e.g. pre-9.1 retail private
    /// servers. Conservative: a single plain TOC, no patch digit.
    fallback_custom: ClientRules,
}

impl RulesTable {
    /// The builtin tables (vision §4.1–§4.3, verified 2026-10-14).
    pub fn builtin() -> Self {
        Self {
            products: builtin_products(),
            version_families: builtin_version_families(),
            families: builtin_families(),
            client_rules: builtin_client_rules(),
            fallback_battlenet: fallback_battlenet(),
            fallback_custom: fallback_custom(),
        }
    }

    /// Applies config overrides to a copy of this table and validates the result.
    ///
    /// Products are added or replaced by code. Version → family rows are prepended, so
    /// they win. Family ranges are replaced. Client rules rows are patched by name
    /// (`fallback-battlenet` and `fallback-custom` included); a new name adds a row
    /// before the existing ones.
    pub fn with_overrides(&self, overrides: &RulesOverrides) -> Result<Self, RulesError> {
        let mut table = self.clone();
        for product in &overrides.products {
            match table
                .products
                .iter_mut()
                .find(|p| p.code.eq_ignore_ascii_case(&product.code))
            {
                Some(existing) => *existing = product.clone(),
                None => table.products.push(product.clone()),
            }
        }
        table
            .version_families
            .splice(0..0, overrides.version_families.iter().cloned());
        for (&family, &range) in &overrides.family_ranges {
            if family == Family::Unknown {
                return Err(RulesError::UnknownFamilyRange);
            }
            if let Some(row) = table.families.iter_mut().find(|r| r.family == family) {
                row.interface_range = Some(range);
            }
        }
        let mut added = Vec::new();
        for (name, patch) in &overrides.client_rules {
            match table.row_mut(name) {
                Some(row) => patch.apply(row),
                None => added.push(patch.new_row(name)?),
            }
        }
        table.client_rules.splice(0..0, added);
        table.validate()?;
        Ok(table)
    }

    pub fn products(&self) -> &[Product] {
        &self.products
    }

    /// The versioned rows, without the two fallbacks.
    pub fn client_rules(&self) -> &[ClientRules] {
        &self.client_rules
    }

    pub fn product_by_code(&self, code: &str) -> Option<&Product> {
        self.products
            .iter()
            .find(|p| p.code.eq_ignore_ascii_case(code))
    }

    /// The product whose flavor folder is `folder`, ignoring ASCII case.
    pub fn product_by_folder(&self, folder: &str) -> Option<&Product> {
        self.products
            .iter()
            .find(|p| p.folder.eq_ignore_ascii_case(folder))
    }

    /// Every product that ships `exe`, ignoring ASCII case. Several products share one
    /// exe name (`WoWClassic.exe`, `WoWT.exe`), so this is not a single lookup.
    pub fn products_by_exe<'a>(&'a self, exe: &'a str) -> impl Iterator<Item = &'a Product> {
        self.products
            .iter()
            .filter(move |p| p.exe.eq_ignore_ascii_case(exe))
    }

    /// The family of a client: the first matching version → family row, else `unknown`.
    pub fn family_for(&self, version: &ClientVersion, kind: InstallKind) -> Family {
        self.version_families
            .iter()
            .find(|rule| rule.matches(version, kind))
            .map_or(Family::Unknown, |rule| rule.family)
    }

    /// The rules of a client: the first row of the version's family whose install kind
    /// and version range match, else the install kind's fallback row.
    pub fn rules_for(&self, version: &ClientVersion, kind: InstallKind) -> &ClientRules {
        self.rules_for_family(self.family_for(version, kind), version, kind)
    }

    /// Like [`RulesTable::rules_for`] with the family already decided, e.g. by a config
    /// target override.
    pub fn rules_for_family(
        &self,
        family: Family,
        version: &ClientVersion,
        kind: InstallKind,
    ) -> &ClientRules {
        self.client_rules
            .iter()
            .find(|row| row.family == family && row.applies_to(kind) && row.covers(version))
            .unwrap_or_else(|| self.fallback(kind))
    }

    /// The rules for a client with no usable version.
    pub fn fallback(&self, kind: InstallKind) -> &ClientRules {
        match kind {
            InstallKind::Battlenet => &self.fallback_battlenet,
            InstallKind::Custom => &self.fallback_custom,
        }
    }

    /// The interfaces a family's addons may declare, inclusive. `None` for `unknown`.
    pub fn family_range(&self, family: Family) -> Option<NumRange> {
        self.family_row(family).and_then(|row| row.interface_range)
    }

    /// The default human id of a custom client in `family`, e.g. `wotlk` for
    /// `wrath-legacy`. `None` for `unknown`.
    pub fn default_id_for_family(&self, family: Family) -> Option<&str> {
        self.family_row(family)
            .and_then(|row| row.default_id.as_deref())
    }

    /// Every TOC suffix worth recognizing when looking for addon roots:
    /// [`KNOWN_TOC_SUFFIXES`], then any other suffix a row lists (override rows
    /// included). Each appears once, compared ignoring ASCII case.
    pub fn toc_suffixes(&self) -> Vec<&str> {
        let mut suffixes: Vec<&str> = KNOWN_TOC_SUFFIXES.to_vec();
        for suffix in self.all_rows().flat_map(ClientRules::suffixes) {
            if !suffixes.iter().any(|s| s.eq_ignore_ascii_case(suffix)) {
                suffixes.push(suffix);
            }
        }
        suffixes
    }

    /// The versioned rows followed by the two fallbacks.
    fn all_rows(&self) -> impl Iterator<Item = &ClientRules> {
        self.client_rules
            .iter()
            .chain([&self.fallback_battlenet, &self.fallback_custom])
    }

    fn family_row(&self, family: Family) -> Option<&FamilyRow> {
        self.families.iter().find(|row| row.family == family)
    }

    fn row_mut(&mut self, name: &str) -> Option<&mut ClientRules> {
        self.client_rules
            .iter_mut()
            .chain([&mut self.fallback_battlenet, &mut self.fallback_custom])
            .find(|row| row.name == name)
    }

    fn validate(&self) -> Result<(), RulesError> {
        for (i, product) in self.products.iter().enumerate() {
            for (field, value) in [
                ("code", &product.code),
                ("folder", &product.folder),
                ("exe", &product.exe),
                ("default_id", &product.default_id),
            ] {
                if value.trim().is_empty() {
                    return Err(RulesError::EmptyProductField {
                        code: product.code.clone(),
                        field,
                    });
                }
            }
            if let Some(first) = self
                .products
                .iter()
                .take(i)
                .find(|p| p.folder.eq_ignore_ascii_case(&product.folder))
            {
                return Err(RulesError::DuplicateProductFolder {
                    folder: product.folder.clone(),
                    first: first.code.clone(),
                    second: product.code.clone(),
                });
            }
        }
        for (i, rule) in self.version_families.iter().enumerate() {
            for (part, range) in [("major", rule.major), ("minor", rule.minor)] {
                if range.is_inverted() {
                    return Err(RulesError::InvertedRange {
                        what: format!("version family row {} ({part})", i + 1),
                        range: range.to_string(),
                    });
                }
            }
        }
        for row in &self.families {
            if let Some(range) = row.interface_range {
                if row.family == Family::Unknown {
                    return Err(RulesError::UnknownFamilyRange);
                }
                if range.is_inverted() {
                    return Err(RulesError::InvertedRange {
                        what: format!("interface range of family {}", row.family),
                        range: range.to_string(),
                    });
                }
            }
        }
        for row in self.all_rows() {
            if let Some(suffix) = row.suffixes().find(|s| !is_valid_suffix(s)) {
                return Err(RulesError::InvalidSuffix {
                    row: row.name.clone(),
                    suffix: suffix.to_owned(),
                });
            }
            if let Some(max) = row.version_max
                && max.without_build() < row.version_min.without_build()
            {
                return Err(RulesError::InvertedRange {
                    what: format!("versions of rules row {}", row.name),
                    range: format!("{}..={max}", row.version_min),
                });
            }
        }
        Ok(())
    }
}

fn is_valid_suffix(suffix: &str) -> bool {
    !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The config `[rules]` section. Every part is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RulesOverrides {
    /// Added, or replacing the product with the same code.
    pub products: Vec<Product>,
    /// Prepended to the version → family table, in order.
    pub version_families: Vec<VersionFamilyRule>,
    /// Replacement interface ranges. `unknown` cannot have one.
    pub family_ranges: BTreeMap<Family, NumRange>,
    /// Client rules rows by name: a patch for an existing row, or a new row.
    pub client_rules: BTreeMap<String, ClientRulesOverride>,
}

/// A field subset of [`ClientRules`]. Patching an existing row replaces only the fields
/// given. A new row needs `family`, `version_min`, `game`, `multi_toc`,
/// `comma_interface`, `interface_includes_patch` and `legacy`; the rest default to
/// empty, false or `None`. A patch cannot reset `install_kind` or `version_max` to `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ClientRulesOverride {
    pub family: Option<Family>,
    pub install_kind: Option<InstallKind>,
    pub version_min: Option<ClientVersion>,
    pub version_max: Option<ClientVersion>,
    pub game: Option<String>,
    pub game_types: Option<Vec<String>>,
    pub multi_toc: Option<bool>,
    pub comma_interface: Option<bool>,
    pub game_suffixes: Option<Vec<String>>,
    pub family_suffixes: Option<Vec<String>>,
    pub legacy_suffixes: Option<Vec<String>>,
    pub directive_conditionals: Option<bool>,
    pub path_variables: Option<bool>,
    pub interface_includes_patch: Option<bool>,
    pub legacy: Option<bool>,
}

impl ClientRulesOverride {
    fn apply(&self, row: &mut ClientRules) {
        fn set<T: Clone>(field: &mut T, value: &Option<T>) {
            if let Some(value) = value {
                *field = value.clone();
            }
        }
        set(&mut row.family, &self.family);
        if self.install_kind.is_some() {
            row.install_kind = self.install_kind;
        }
        set(&mut row.version_min, &self.version_min);
        if self.version_max.is_some() {
            row.version_max = self.version_max;
        }
        set(&mut row.game, &self.game);
        set(&mut row.game_types, &self.game_types);
        set(&mut row.multi_toc, &self.multi_toc);
        set(&mut row.comma_interface, &self.comma_interface);
        set(&mut row.game_suffixes, &self.game_suffixes);
        set(&mut row.family_suffixes, &self.family_suffixes);
        set(&mut row.legacy_suffixes, &self.legacy_suffixes);
        set(
            &mut row.directive_conditionals,
            &self.directive_conditionals,
        );
        set(&mut row.path_variables, &self.path_variables);
        set(
            &mut row.interface_includes_patch,
            &self.interface_includes_patch,
        );
        set(&mut row.legacy, &self.legacy);
    }

    fn new_row(&self, name: &str) -> Result<ClientRules, RulesError> {
        fn required<T: Clone>(
            value: &Option<T>,
            row: &str,
            field: &'static str,
        ) -> Result<T, RulesError> {
            value.clone().ok_or_else(|| RulesError::MissingField {
                row: row.to_owned(),
                field,
            })
        }
        Ok(ClientRules {
            name: name.to_owned(),
            family: required(&self.family, name, "family")?,
            install_kind: self.install_kind,
            version_min: required(&self.version_min, name, "version_min")?,
            version_max: self.version_max,
            game: required(&self.game, name, "game")?,
            game_types: self.game_types.clone().unwrap_or_default(),
            multi_toc: required(&self.multi_toc, name, "multi_toc")?,
            comma_interface: required(&self.comma_interface, name, "comma_interface")?,
            game_suffixes: self.game_suffixes.clone().unwrap_or_default(),
            family_suffixes: self.family_suffixes.clone().unwrap_or_default(),
            legacy_suffixes: self.legacy_suffixes.clone().unwrap_or_default(),
            directive_conditionals: self.directive_conditionals.unwrap_or(false),
            path_variables: self.path_variables.unwrap_or(false),
            interface_includes_patch: required(
                &self.interface_includes_patch,
                name,
                "interface_includes_patch",
            )?,
            legacy: required(&self.legacy, name, "legacy")?,
        })
    }
}

/// An override that would make the rules table invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RulesError {
    #[error(
        "rules row {row}: invalid TOC suffix {suffix:?} (expected ASCII letters and digits, no separator)"
    )]
    InvalidSuffix { row: String, suffix: String },
    #[error("products {first} and {second} both use the flavor folder {folder}")]
    DuplicateProductFolder {
        folder: String,
        first: String,
        second: String,
    },
    #[error("product {code:?}: {field} is empty")]
    EmptyProductField { code: String, field: &'static str },
    #[error("{what}: range {range} is inverted")]
    InvertedRange { what: String, range: String },
    #[error("family unknown cannot have an interface range")]
    UnknownFamilyRange,
    #[error("new rules row {row} is missing {field}")]
    MissingField { row: String, field: &'static str },
}

// ---------------------------------------------------------------------------------------
// Builtin data.

/// code, folder, exe, kind, default id (vision §4.1).
#[rustfmt::skip]
const PRODUCTS: &[(&str, &str, &str, TargetKind, &str)] = &[
    ("wow",                 "_retail_",          "WoW.exe",         TargetKind::Live, "retail"),
    ("wowt",                "_ptr_",             "WoWT.exe",        TargetKind::Test, "ptr"),
    ("wowxptr",             "_xptr_",            "WoWT.exe",        TargetKind::Test, "xptr"),
    ("wow_beta",            "_beta_",            "WoWB.exe",        TargetKind::Test, "beta"),
    ("wow_classic",         "_classic_",         "WoWClassic.exe",  TargetKind::Live, "classic"),
    ("wow_classic_ptr",     "_classic_ptr_",     "WoWClassicT.exe", TargetKind::Test, "classic-ptr"),
    ("wow_classic_beta",    "_classic_beta_",    "WowB.exe",        TargetKind::Test, "classic-beta"),
    ("wow_classic_era",     "_classic_era_",     "WoWClassic.exe",  TargetKind::Live, "era"),
    ("wow_classic_era_ptr", "_classic_era_ptr_", "WowClassicT.exe", TargetKind::Test, "era-ptr"),
    ("wow_anniversary",     "_anniversary_",     "WoWClassic.exe",  TargetKind::Live, "anniversary"),
    ("wow_classic_titan",   "_classic_titan_",   "WoWClassic.exe",  TargetKind::Live, "titan"),
    ("wowf",                "_forever_",         "Wow.exe",         TargetKind::Live, "forever"),
];

fn builtin_products() -> Vec<Product> {
    PRODUCTS
        .iter()
        .map(|&(code, folder, exe, kind, default_id)| Product {
            code: code.to_owned(),
            folder: folder.to_owned(),
            exe: exe.to_owned(),
            kind,
            default_id: default_id.to_owned(),
        })
        .collect()
}

/// install kind, major, minor, family; first match wins (vision §4.1). Custom clients
/// outside the legacy generations fall through to the general rows (custom 3.4.x is
/// `wrath`).
#[rustfmt::skip]
const VERSION_FAMILIES: &[(Option<InstallKind>, NumRange, NumRange, Family)] = &[
    (Some(InstallKind::Custom), NumRange::exactly(1),  NumRange::exactly(12),    Family::VanillaLegacy),
    (Some(InstallKind::Custom), NumRange::exactly(2),  NumRange::exactly(4),     Family::TbcLegacy),
    (Some(InstallKind::Custom), NumRange::exactly(3),  NumRange::exactly(3),     Family::WrathLegacy),
    (Some(InstallKind::Custom), NumRange::exactly(4),  NumRange::exactly(3),     Family::CataLegacy),
    (Some(InstallKind::Custom), NumRange::exactly(5),  NumRange::exactly(4),     Family::MistsLegacy),
    (None,                      NumRange::exactly(1),  NumRange::between(0, 49), Family::Vanilla),
    (None,                      NumRange::exactly(1),  NumRange::at_least(50),   Family::Forever),
    (None,                      NumRange::exactly(2),  NumRange::exactly(5),     Family::Tbc),
    (None,                      NumRange::exactly(3),  NumRange::exactly(4),     Family::Wrath),
    (None,                      NumRange::exactly(3),  NumRange::exactly(80),    Family::Titan),
    (None,                      NumRange::exactly(4),  NumRange::exactly(4),     Family::Cata),
    (None,                      NumRange::exactly(5),  NumRange::exactly(5),     Family::Mists),
    (None,                      NumRange::at_least(6), NumRange::at_least(0),    Family::Mainline),
];

fn builtin_version_families() -> Vec<VersionFamilyRule> {
    VERSION_FAMILIES
        .iter()
        .map(|&(install_kind, major, minor, family)| VersionFamilyRule {
            install_kind,
            major,
            minor,
            family,
        })
        .collect()
}

/// family, interface range, custom-client default id.
///
/// The ranges separate families, not expansions: an older-expansion retail TOC is out of
/// date, not wrong flavor. The vanilla floor is 11300 because Battle.net Classic Era
/// starts at 1.13; 11200–11299 is the 1.12 private-server generation (`vanilla-legacy`).
/// Each legacy family is one minor generation wide.
#[rustfmt::skip]
const FAMILIES: &[(Family, Option<NumRange>, Option<&str>)] = &[
    (Family::Vanilla,       Some(NumRange::between(11_300, 14_999)), Some("vanilla")),
    (Family::Forever,       Some(NumRange::between(15_000, 19_999)), Some("forever")),
    (Family::Tbc,           Some(NumRange::between(20_500, 20_599)), Some("tbc")),
    (Family::Wrath,         Some(NumRange::between(30_400, 30_499)), Some("wrath")),
    (Family::Titan,         Some(NumRange::between(38_000, 38_099)), Some("titan")),
    (Family::Cata,          Some(NumRange::between(40_400, 40_499)), Some("cata")),
    (Family::Mists,         Some(NumRange::between(50_500, 50_599)), Some("mists")),
    (Family::Mainline,      Some(NumRange::at_least(60_000)),        Some("mainline")),
    (Family::VanillaLegacy, Some(NumRange::between(11_200, 11_299)), Some("vanilla")),
    (Family::TbcLegacy,     Some(NumRange::between(20_400, 20_499)), Some("tbc")),
    (Family::WrathLegacy,   Some(NumRange::between(30_300, 30_399)), Some("wotlk")),
    (Family::CataLegacy,    Some(NumRange::between(40_300, 40_399)), Some("cata")),
    (Family::MistsLegacy,   Some(NumRange::between(50_400, 50_499)), Some("mists")),
    (Family::Unknown,       None,                                    None),
];

fn builtin_families() -> Vec<FamilyRow> {
    FAMILIES
        .iter()
        .map(|&(family, interface_range, default_id)| FamilyRow {
            family,
            interface_range,
            default_id: default_id.map(str::to_owned),
        })
        .collect()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|&s| s.to_owned()).collect()
}

/// A multi-TOC Battle.net-era row: comma Interface lists, path variables and the patch
/// digit, no directive conditionals.
fn modern_row(
    name: &str,
    family: Family,
    version_min: ClientVersion,
    game: &str,
    game_types: &[&str],
    game_suffixes: &[&str],
    family_suffix: &str,
) -> ClientRules {
    ClientRules {
        name: name.to_owned(),
        family,
        install_kind: None,
        version_min,
        version_max: None,
        game: game.to_owned(),
        game_types: strings(game_types),
        multi_toc: true,
        comma_interface: true,
        game_suffixes: strings(game_suffixes),
        family_suffixes: strings(&[family_suffix]),
        legacy_suffixes: Vec::new(),
        directive_conditionals: false,
        path_variables: true,
        interface_includes_patch: true,
        legacy: false,
    }
}

/// A private-server generation below the multi-TOC era: one plain TOC with a single
/// Interface value and no patch digit.
fn legacy_row(name: &str, family: Family, version_min: ClientVersion, game: &str) -> ClientRules {
    ClientRules {
        name: name.to_owned(),
        family,
        install_kind: Some(InstallKind::Custom),
        version_min,
        version_max: None,
        game: game.to_owned(),
        game_types: Vec::new(),
        multi_toc: false,
        comma_interface: false,
        game_suffixes: Vec::new(),
        family_suffixes: Vec::new(),
        legacy_suffixes: Vec::new(),
        directive_conditionals: false,
        path_variables: false,
        interface_includes_patch: false,
        legacy: true,
    }
}

/// Rows of one family are tried in order. Mainline is split where clients gained
/// `_Standard` (12.1.5), directive conditionals (12.0.7), comma lists (10.2.7) and
/// multi-TOC (9.1.0). Within the classic families each row covers the whole family,
/// so it has no `version_max`.
///
/// UNVERIFIED, so table values only: Titan's game type and suffix, the first classic
/// builds with multi-TOC and comma lists (these rows describe current clients), and the
/// build that introduced path variables.
fn builtin_client_rules() -> Vec<ClientRules> {
    let standard = ["standard"];
    vec![
        ClientRules {
            directive_conditionals: true,
            ..modern_row(
                "mainline",
                Family::Mainline,
                ClientVersion::new(12, 1, 5),
                "standard",
                &standard,
                &["Standard"],
                "Mainline",
            )
        },
        ClientRules {
            version_max: Some(ClientVersion::new(12, 1, 4)),
            directive_conditionals: true,
            ..modern_row(
                "mainline-12.0.7",
                Family::Mainline,
                ClientVersion::new(12, 0, 7),
                "standard",
                &standard,
                &[],
                "Mainline",
            )
        },
        ClientRules {
            version_max: Some(ClientVersion::new(12, 0, 6)),
            ..modern_row(
                "mainline-10.2.7",
                Family::Mainline,
                ClientVersion::new(10, 2, 7),
                "standard",
                &standard,
                &[],
                "Mainline",
            )
        },
        ClientRules {
            version_max: Some(ClientVersion::new(10, 2, 6)),
            comma_interface: false,
            ..modern_row(
                "mainline-9.1",
                Family::Mainline,
                ClientVersion::new(9, 1, 0),
                "standard",
                &standard,
                &[],
                "Mainline",
            )
        },
        modern_row(
            "forever",
            Family::Forever,
            ClientVersion::new(1, 50, 0),
            "camelot",
            &["camelot", "forever"],
            &["Forever", "Camelot"],
            "Mainline",
        ),
        modern_row(
            "vanilla",
            Family::Vanilla,
            ClientVersion::new(1, 13, 0),
            "vanilla",
            &["vanilla"],
            &["Vanilla"],
            "Classic",
        ),
        modern_row(
            "tbc",
            Family::Tbc,
            ClientVersion::new(2, 5, 0),
            "tbc",
            &["tbc"],
            &["TBC"],
            "Classic",
        ),
        modern_row(
            "wrath",
            Family::Wrath,
            ClientVersion::new(3, 4, 0),
            "wrath",
            &["wrath"],
            &["Wrath"],
            "Classic",
        ),
        modern_row(
            "titan",
            Family::Titan,
            ClientVersion::new(3, 80, 0),
            "titan",
            &[],
            &[],
            "Classic",
        ),
        modern_row(
            "cata",
            Family::Cata,
            ClientVersion::new(4, 4, 0),
            "cata",
            &["cata"],
            &["Cata"],
            "Classic",
        ),
        modern_row(
            "mists",
            Family::Mists,
            ClientVersion::new(5, 5, 0),
            "mists",
            &["mists"],
            &["Mists"],
            "Classic",
        ),
        legacy_row(
            "vanilla-legacy",
            Family::VanillaLegacy,
            ClientVersion::new(1, 12, 0),
            "vanilla",
        ),
        legacy_row(
            "tbc-legacy",
            Family::TbcLegacy,
            ClientVersion::new(2, 4, 0),
            "tbc",
        ),
        legacy_row(
            "wrath-legacy",
            Family::WrathLegacy,
            ClientVersion::new(3, 3, 0),
            "wrath",
        ),
        legacy_row(
            "cata-legacy",
            Family::CataLegacy,
            ClientVersion::new(4, 3, 0),
            "cata",
        ),
        legacy_row(
            "mists-legacy",
            Family::MistsLegacy,
            ClientVersion::new(5, 4, 0),
            "mists",
        ),
    ]
}

/// Every Battle.net client is multi-TOC, but an unmapped one could belong to either
/// family, so both family suffixes are candidates. The verdict's exact-interface rule
/// for `unknown` is what guards installs.
fn fallback_battlenet() -> ClientRules {
    ClientRules {
        install_kind: Some(InstallKind::Battlenet),
        family_suffixes: strings(&["Mainline", "Classic"]),
        ..modern_row(
            "fallback-battlenet",
            Family::Unknown,
            ClientVersion::new(0, 0, 0),
            "unknown",
            &[],
            &[],
            "Mainline",
        )
    }
}

fn fallback_custom() -> ClientRules {
    legacy_row(
        "fallback-custom",
        Family::Unknown,
        ClientVersion::new(0, 0, 0),
        "unknown",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn v(s: &str) -> ClientVersion {
        s.parse().unwrap()
    }

    fn table() -> RulesTable {
        RulesTable::builtin()
    }

    // ---- ClientVersion -------------------------------------------------------------

    #[test]
    fn version_parses_two_to_four_parts() {
        let cases = [
            ("12.1.5.70391", (12, 1, 5, Some(70391))),
            ("3.3.5a", (3, 3, 5, None)),
            ("1.60.1", (1, 60, 1, None)),
            ("2.5", (2, 5, 0, None)),
            (" 1.15.9 ", (1, 15, 9, None)),
            ("3.3.5.12340", (3, 3, 5, Some(12340))),
            ("1.12.1b", (1, 12, 1, None)),
        ];
        for (input, (major, minor, patch, build)) in cases {
            let parsed = v(input);
            assert_eq!(
                parsed,
                ClientVersion {
                    major,
                    minor,
                    patch,
                    build
                },
                "input {input:?}"
            );
        }
    }

    #[test]
    fn version_rejects_malformed_input() {
        for input in [
            "",
            "12",
            "1.2.3.4.5",
            "a.b",
            "3.3a.5",
            "1..2",
            "+1.2",
            "1.-2",
            "1.2.a",
            "99999999999.1",
            "1.2.3 4",
        ] {
            let err = input.parse::<ClientVersion>().unwrap_err();
            assert_eq!(err.input, input, "input {input:?}");
        }
    }

    #[test]
    fn version_orders_by_numeric_tuple() {
        assert!(v("12.1.5") < v("12.1.5.70391"));
        assert!(v("12.1.5.70391") < v("12.1.6"));
        assert!(v("1.60.1") > v("1.15.9"));
        assert!(v("3.80.2") > v("3.4.3"));
        assert!(v("10.2.7") > v("9.1.0"));
        assert_eq!(v("2.5"), v("2.5.0"));
    }

    #[test]
    fn version_displays_and_serializes_as_dotted_string() {
        assert_eq!(v("12.1.5.70391").to_string(), "12.1.5.70391");
        assert_eq!(v("3.3.5a").to_string(), "3.3.5");
        assert_eq!(v("2.5").to_string(), "2.5.0");
        assert_eq!(
            serde_json::to_string(&v("12.1.5.70391")).unwrap(),
            "\"12.1.5.70391\""
        );
        let back: ClientVersion = serde_json::from_str("\"1.60.1\"").unwrap();
        assert_eq!(back, v("1.60.1"));
        assert!(serde_json::from_str::<ClientVersion>("\"retail\"").is_err());
        let schema = serde_json::to_value(schemars::schema_for!(ClientVersion)).unwrap();
        assert_eq!(schema["type"], "string", "{schema}");
    }

    proptest! {
        #[test]
        fn version_display_round_trips(
            major in any::<u32>(),
            minor in any::<u32>(),
            patch in any::<u32>(),
            build in proptest::option::of(any::<u32>()),
        ) {
            let version = ClientVersion { major, minor, patch, build };
            prop_assert_eq!(version.to_string().parse::<ClientVersion>(), Ok(version));
        }

        #[test]
        fn version_parse_never_panics(input in ".{0,40}") {
            let _ = input.parse::<ClientVersion>();
        }
    }

    // ---- Family and kinds ----------------------------------------------------------

    #[test]
    fn family_wire_names_match_as_str() {
        for family in Family::ALL {
            let wire = serde_json::to_value(family).unwrap();
            assert_eq!(wire, family.as_str(), "{family:?}");
            assert_eq!(Family::from_name(family.as_str()), Some(family));
        }
        assert_eq!(Family::from_name("Wrath-Legacy"), Some(Family::WrathLegacy));
        assert_eq!(Family::from_name("retail"), None);
        assert_eq!(
            serde_json::to_value(InstallKind::Battlenet).unwrap(),
            "battlenet"
        );
        assert_eq!(serde_json::to_value(InstallKind::Custom).unwrap(), "custom");
        assert_eq!(serde_json::to_value(TargetKind::Live).unwrap(), "live");
        assert_eq!(serde_json::to_value(TargetKind::Test).unwrap(), "test");
    }

    #[test]
    fn legacy_families_are_exactly_the_dash_legacy_ones() {
        for family in Family::ALL {
            assert_eq!(
                family.is_legacy(),
                family.as_str().ends_with("-legacy"),
                "{family}"
            );
        }
    }

    #[test]
    fn release_flavors_and_packager_aliases() {
        let expected = [
            (Family::Mainline, Some("mainline")),
            (Family::Vanilla, Some("classic")),
            (Family::Tbc, Some("bcc")),
            (Family::Wrath, Some("wrath")),
            (Family::Titan, Some("titan")),
            (Family::Cata, Some("cata")),
            (Family::Mists, Some("mists")),
            (Family::Forever, Some("forever")),
            (Family::VanillaLegacy, None),
            (Family::TbcLegacy, None),
            (Family::WrathLegacy, None),
            (Family::CataLegacy, None),
            (Family::MistsLegacy, None),
            (Family::Unknown, None),
        ];
        for (family, flavor) in expected {
            assert_eq!(family.release_flavor(), flavor, "{family}");
            if let Some(flavor) = flavor {
                assert_eq!(Family::from_release_flavor(flavor), Some(family));
            }
        }
        assert_eq!(Family::from_release_flavor("tbc"), Some(Family::Tbc));
        assert_eq!(
            Family::from_release_flavor("vanilla"),
            Some(Family::Vanilla)
        );
        assert_eq!(Family::from_release_flavor("wotlkc"), Some(Family::Wrath));
        assert_eq!(
            Family::from_release_flavor("camelot"),
            Some(Family::Forever)
        );
        assert_eq!(
            Family::from_release_flavor("Mainline"),
            Some(Family::Mainline)
        );
        assert_eq!(Family::from_release_flavor("retail"), None);
        assert_eq!(Family::from_release_flavor(""), None);
    }

    // ---- Version → family, rules and interface vectors -----------------------------

    #[test]
    fn every_product_resolves_family_interface_and_rules() {
        // folder, .build.info version, family, interface, kind, rules row (vision §4.1).
        #[rustfmt::skip]
        let cases = [
            ("_retail_",          "12.1.5.70391", Family::Mainline, 120_105, TargetKind::Live, "mainline"),
            ("_ptr_",             "12.1.0",       Family::Mainline, 120_100, TargetKind::Test, "mainline-12.0.7"),
            ("_xptr_",            "12.1.5",       Family::Mainline, 120_105, TargetKind::Test, "mainline"),
            ("_beta_",            "12.0.1",       Family::Mainline, 120_001, TargetKind::Test, "mainline-10.2.7"),
            ("_classic_",         "5.5.4",        Family::Mists,    50_504,  TargetKind::Live, "mists"),
            ("_classic_ptr_",     "5.5.4",        Family::Mists,    50_504,  TargetKind::Test, "mists"),
            ("_classic_beta_",    "1.60.1.70374", Family::Forever,  16_001,  TargetKind::Test, "forever"),
            ("_classic_era_",     "1.15.9",       Family::Vanilla,  11_509,  TargetKind::Live, "vanilla"),
            ("_classic_era_ptr_", "2.5.6",        Family::Tbc,      20_506,  TargetKind::Test, "tbc"),
            ("_anniversary_",     "2.5.6",        Family::Tbc,      20_506,  TargetKind::Live, "tbc"),
            ("_classic_titan_",   "3.80.2",       Family::Titan,    38_002,  TargetKind::Live, "titan"),
            ("_forever_",         "1.60.1.70245", Family::Forever,  16_001,  TargetKind::Live, "forever"),
        ];
        let table = table();
        for (folder, version, family, interface, kind, row) in cases {
            let product = table.product_by_folder(folder).unwrap();
            assert_eq!(product.kind, kind, "{folder}");
            let version = v(version);
            assert_eq!(
                table.family_for(&version, InstallKind::Battlenet),
                family,
                "{folder}"
            );
            let rules = table.rules_for(&version, InstallKind::Battlenet);
            assert_eq!(rules.name, row, "{folder}");
            assert_eq!(interface_for(&version, rules), interface, "{folder}");
            assert!(
                table.family_range(family).unwrap().contains(interface),
                "{folder}: {interface} outside the {family} range"
            );
        }
        assert_eq!(table.products().len(), cases.len());
    }

    #[test]
    fn every_legacy_row_resolves_family_interface_and_rules() {
        let cases = [
            ("1.12.1", Family::VanillaLegacy, 11_200, "vanilla"),
            ("2.4.3", Family::TbcLegacy, 20_400, "tbc"),
            ("3.3.5a", Family::WrathLegacy, 30_300, "wotlk"),
            ("4.3.4", Family::CataLegacy, 40_300, "cata"),
            ("5.4.8", Family::MistsLegacy, 50_400, "mists"),
        ];
        let table = table();
        for (version, family, interface, id) in cases {
            let version = v(version);
            assert_eq!(
                table.family_for(&version, InstallKind::Custom),
                family,
                "{version}"
            );
            let rules = table.rules_for(&version, InstallKind::Custom);
            assert_eq!(rules.name, family.as_str(), "{version}");
            assert!(rules.legacy && !rules.multi_toc && !rules.comma_interface);
            assert_eq!(interface_for(&version, rules), interface, "{version}");
            assert!(table.family_range(family).unwrap().contains(interface));
            assert_eq!(table.default_id_for_family(family), Some(id));
        }
    }

    #[test]
    fn family_for_acceptance_vectors() {
        let table = table();
        let bn = InstallKind::Battlenet;
        let custom = InstallKind::Custom;
        assert_eq!(table.family_for(&v("1.15.9"), bn), Family::Vanilla);
        assert_eq!(table.family_for(&v("1.60.1"), bn), Family::Forever);
        assert_eq!(table.family_for(&v("2.5.6"), bn), Family::Tbc);
        assert_eq!(table.family_for(&v("3.80.2"), bn), Family::Titan);
        assert_eq!(table.family_for(&v("3.3.5"), custom), Family::WrathLegacy);
        assert_eq!(table.family_for(&v("3.4.3"), custom), Family::Wrath);
        assert_eq!(table.family_for(&v("2.6.0"), bn), Family::Unknown);
        assert_eq!(table.family_for(&v("13.0.0"), bn), Family::Mainline);
        // A Battle.net 3.3.5 is not a legacy client.
        assert_eq!(table.family_for(&v("3.3.5"), bn), Family::Unknown);
        // The product code plays no part: era PTR runs TBC and the classic beta runs Forever.
        assert_eq!(
            table.product_by_code("wow_classic_era_ptr").unwrap().folder,
            "_classic_era_ptr_"
        );
        assert_eq!(
            table.product_by_code("wow_classic_beta").unwrap().folder,
            "_classic_beta_"
        );
    }

    #[test]
    fn mainline_rows_follow_client_capabilities() {
        let table = table();
        let bn = InstallKind::Battlenet;
        let live = table.rules_for(&v("12.1.5.70391"), bn);
        assert_eq!(live.game_suffixes, ["Standard"]);
        assert!(live.directive_conditionals);
        let ptr = table.rules_for(&v("12.1.0"), bn);
        assert!(ptr.game_suffixes.is_empty());
        assert!(ptr.directive_conditionals);
        let beta = table.rules_for(&v("12.0.1"), bn);
        assert!(!beta.directive_conditionals);
        assert!(beta.comma_interface);
        let shadowlands = table.rules_for(&v("9.2.7"), bn);
        assert_eq!(shadowlands.name, "mainline-9.1");
        assert!(shadowlands.multi_toc && !shadowlands.comma_interface);
        // The upper bounds ignore build numbers.
        assert_eq!(
            table.rules_for(&v("12.1.4.99999"), bn).name,
            "mainline-12.0.7"
        );
    }

    #[test]
    fn forever_rows_load_forever_camelot_and_mainline() {
        let rules = table()
            .rules_for(&v("1.60.1"), InstallKind::Battlenet)
            .clone();
        assert_eq!(rules.game, "camelot");
        assert_eq!(rules.game_types, ["camelot", "forever"]);
        assert_eq!(rules.game_suffixes, ["Forever", "Camelot"]);
        assert_eq!(rules.family_suffixes, ["Mainline"]);
        // The Forever-loads-_Mainline guard is the family range, not the suffix.
        let range = table().family_range(Family::Forever).unwrap();
        assert!(!range.contains(120_105));
        assert!(range.contains(16_001));
    }

    #[test]
    fn unmapped_versions_use_the_install_kind_fallback() {
        let table = table();
        let bn = table.rules_for(&v("2.6.0"), InstallKind::Battlenet);
        assert_eq!(bn.name, "fallback-battlenet");
        assert!(bn.multi_toc && !bn.legacy);
        assert_eq!(bn.family_suffixes, ["Mainline", "Classic"]);
        assert_eq!(table.fallback(InstallKind::Battlenet), bn);

        // A pre-9.1 retail private server is mainline by family but gets the conservative row.
        let legion = v("7.3.5");
        assert_eq!(
            table.family_for(&legion, InstallKind::Custom),
            Family::Mainline
        );
        let custom = table.rules_for(&legion, InstallKind::Custom);
        assert_eq!(custom.name, "fallback-custom");
        assert!(custom.legacy && !custom.multi_toc);
        assert_eq!(interface_for(&legion, custom), 70_300);
        // A custom 1.11 client is vanilla by family, below every vanilla row.
        assert_eq!(
            table.rules_for(&v("1.11.2"), InstallKind::Custom).name,
            "fallback-custom"
        );
        assert_eq!(table.family_range(Family::Unknown), None);
        assert_eq!(table.default_id_for_family(Family::Unknown), None);
        assert_eq!(&ClientRules::default(), table.fallback(InstallKind::Custom));
    }

    #[test]
    fn rules_for_family_honors_a_decided_family() {
        let table = table();
        let bn = InstallKind::Battlenet;
        // A declared family picks that family's row: classic rows have no upper bound, so
        // a 1.60.1 client declared vanilla gets the vanilla row. A version below every row
        // of the declared family falls back.
        assert_eq!(
            table
                .rules_for_family(Family::Vanilla, &v("1.60.1"), bn)
                .name,
            "vanilla"
        );
        assert_eq!(
            table
                .rules_for_family(Family::Mainline, &v("7.3.5"), bn)
                .name,
            "fallback-battlenet"
        );
        assert_eq!(
            table
                .rules_for_family(Family::Mainline, &v("12.1.5"), bn)
                .name,
            "mainline"
        );
    }

    // ---- Products ------------------------------------------------------------------

    #[test]
    fn product_lookups_ignore_case() {
        let table = table();
        assert_eq!(table.product_by_folder("_RETAIL_").unwrap().code, "wow");
        assert_eq!(
            table.product_by_folder("_Forever_").unwrap().default_id,
            "forever"
        );
        assert!(table.product_by_folder("_xyz_").is_none());
        assert_eq!(table.product_by_code("WOWF").unwrap().folder, "_forever_");
        assert!(table.product_by_code("wowz").is_none());

        let classic: Vec<&str> = table
            .products_by_exe("wowclassic.exe")
            .map(|p| p.code.as_str())
            .collect();
        assert_eq!(
            classic,
            [
                "wow_classic",
                "wow_classic_era",
                "wow_anniversary",
                "wow_classic_titan"
            ]
        );
        let wow: Vec<&str> = table
            .products_by_exe("WOW.EXE")
            .map(|p| p.code.as_str())
            .collect();
        assert_eq!(wow, ["wow", "wowf"]);
        assert_eq!(table.products_by_exe("WowError.exe").count(), 0);
    }

    // ---- Table invariants ----------------------------------------------------------

    #[test]
    fn builtin_table_is_valid() {
        assert_eq!(table().validate(), Ok(()));
    }

    #[test]
    fn product_codes_folders_and_ids_are_unique() {
        let table = table();
        let products = table.products();
        for (i, a) in products.iter().enumerate() {
            for b in products.iter().skip(i + 1) {
                assert_ne!(a.code, b.code);
                assert!(!a.folder.eq_ignore_ascii_case(&b.folder), "{}", a.folder);
                assert_ne!(a.default_id, b.default_id);
            }
            assert!(
                a.folder.starts_with('_') && a.folder.ends_with('_'),
                "{}",
                a.folder
            );
        }
    }

    #[test]
    fn every_family_has_one_row_and_ranges_do_not_overlap() {
        let table = table();
        for family in Family::ALL {
            let rows = table.families.iter().filter(|r| r.family == family).count();
            assert_eq!(rows, 1, "{family}");
            assert_eq!(
                table.family_range(family).is_some(),
                family != Family::Unknown,
                "{family}"
            );
        }
        let ranges: Vec<(Family, NumRange)> = table
            .families
            .iter()
            .filter_map(|r| r.interface_range.map(|range| (r.family, range)))
            .collect();
        for (i, &(fa, a)) in ranges.iter().enumerate() {
            for &(fb, b) in ranges.iter().skip(i + 1) {
                let a_below_b = a.max.is_some_and(|max| max < b.min);
                let b_below_a = b.max.is_some_and(|max| max < a.min);
                assert!(a_below_b || b_below_a, "{fa} {a} overlaps {fb} {b}");
            }
        }
    }

    #[test]
    fn every_rules_row_matches_its_family_and_resolves_to_itself() {
        let table = table();
        let mut names = Vec::new();
        for row in table.client_rules() {
            let kinds = match row.install_kind {
                Some(kind) => vec![kind],
                None => vec![InstallKind::Battlenet, InstallKind::Custom],
            };
            for kind in kinds {
                let mut bounds = vec![row.version_min];
                bounds.extend(row.version_max);
                for version in bounds {
                    assert_eq!(
                        table.family_for(&version, kind),
                        row.family,
                        "{} {version}",
                        row.name
                    );
                    assert_eq!(
                        table.rules_for(&version, kind).name,
                        row.name,
                        "{} {version}",
                        row.name
                    );
                    let interface = interface_for(&version, row);
                    assert!(
                        table.family_range(row.family).unwrap().contains(interface),
                        "{}: {interface}",
                        row.name
                    );
                }
            }
            assert_eq!(row.legacy, row.family.is_legacy(), "{}", row.name);
            assert_eq!(row.path_variables, row.multi_toc, "{}", row.name);
            assert!(row.legacy_suffixes.is_empty(), "{}", row.name);
            names.push(row.name.as_str());
        }
        names.push(&table.fallback_battlenet.name);
        names.push(&table.fallback_custom.name);
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "{names:?}");
    }

    #[test]
    fn every_builtin_suffix_is_known_and_valid() {
        for suffix in KNOWN_TOC_SUFFIXES {
            assert!(is_valid_suffix(suffix), "{suffix}");
        }
        let table = table();
        for row in table.all_rows() {
            for suffix in row.suffixes() {
                assert!(
                    KNOWN_TOC_SUFFIXES.contains(&suffix),
                    "{}: {suffix}",
                    row.name
                );
            }
        }
        assert_eq!(table.toc_suffixes(), KNOWN_TOC_SUFFIXES);
    }

    #[test]
    fn toc_suffixes_include_override_suffixes_once() {
        let overrides = RulesOverrides {
            client_rules: BTreeMap::from([(
                "titan".to_owned(),
                ClientRulesOverride {
                    game_suffixes: Some(strings(&["Titan", "tbc"])),
                    ..ClientRulesOverride::default()
                },
            )]),
            ..RulesOverrides::default()
        };
        let table = table().with_overrides(&overrides).unwrap();
        let suffixes = table.toc_suffixes();
        assert_eq!(suffixes.len(), KNOWN_TOC_SUFFIXES.len() + 1, "{suffixes:?}");
        assert_eq!(suffixes.last(), Some(&"Titan"));
    }

    // ---- Overrides -----------------------------------------------------------------

    #[test]
    fn overrides_parse_from_toml_and_apply() {
        let overrides: RulesOverrides = toml::from_str(
            r#"
            [[products]]
            code = "wow_newflavor"
            folder = "_newflavor_"
            exe = "WowNew.exe"
            kind = "live"
            default_id = "newflavor"

            [[version_families]]
            install_kind = "custom"
            major = { min = 7, max = 7 }
            minor = { min = 3, max = 3 }
            family = "mainline"

            [family_ranges]
            wrath-legacy = { min = 30200, max = 30399 }

            [client_rules.titan]
            game_suffixes = ["Titan"]

            [client_rules.legion]
            family = "mainline"
            install_kind = "custom"
            version_min = "7.3.0"
            version_max = "7.3.5"
            game = "standard"
            multi_toc = false
            comma_interface = false
            interface_includes_patch = false
            legacy = true
            "#,
        )
        .unwrap();
        let table = table().with_overrides(&overrides).unwrap();

        let product = table.product_by_folder("_NEWFLAVOR_").unwrap();
        assert_eq!(product.code, "wow_newflavor");
        assert_eq!(table.products().len(), 13);

        let titan = table.rules_for(&v("3.80.2"), InstallKind::Battlenet);
        assert_eq!(titan.game_suffixes, ["Titan"]);
        assert_eq!(titan.family_suffixes, ["Classic"], "unpatched fields stay");

        assert_eq!(
            table.family_range(Family::WrathLegacy),
            Some(NumRange::between(30_200, 30_399))
        );

        let legion = v("7.3.5");
        assert_eq!(
            table.family_for(&legion, InstallKind::Custom),
            Family::Mainline
        );
        let rules = table.rules_for(&legion, InstallKind::Custom);
        assert_eq!(rules.name, "legion");
        assert!(rules.game_suffixes.is_empty() && !rules.directive_conditionals);
        assert_eq!(interface_for(&legion, rules), 70_300);
        // The new row is tried before the builtin mainline rows.
        assert_eq!(table.client_rules()[0].name, "legion");
    }

    #[test]
    fn overrides_replace_products_by_code_and_patch_fallbacks() {
        let overrides = RulesOverrides {
            products: vec![Product {
                code: "WOWF".to_owned(),
                folder: "_forever_".to_owned(),
                exe: "WowForever.exe".to_owned(),
                kind: TargetKind::Live,
                default_id: "forever".to_owned(),
            }],
            client_rules: BTreeMap::from([(
                "fallback-custom".to_owned(),
                ClientRulesOverride {
                    interface_includes_patch: Some(true),
                    ..ClientRulesOverride::default()
                },
            )]),
            ..RulesOverrides::default()
        };
        let table = table().with_overrides(&overrides).unwrap();
        assert_eq!(table.products().len(), 12);
        assert_eq!(
            table.product_by_folder("_forever_").unwrap().exe,
            "WowForever.exe"
        );
        assert!(table.fallback(InstallKind::Custom).interface_includes_patch);
        assert_eq!(
            RulesTable::builtin().with_overrides(&RulesOverrides::default()),
            Ok(RulesTable::builtin())
        );
    }

    #[test]
    fn invalid_overrides_are_rejected() {
        let patch = |name: &str, o: ClientRulesOverride| RulesOverrides {
            client_rules: BTreeMap::from([(name.to_owned(), o)]),
            ..RulesOverrides::default()
        };
        let suffixes = |s: &[&str]| ClientRulesOverride {
            game_suffixes: Some(strings(s)),
            ..ClientRulesOverride::default()
        };
        let product = |code: &str, folder: &str| Product {
            code: code.to_owned(),
            folder: folder.to_owned(),
            exe: "Wow.exe".to_owned(),
            kind: TargetKind::Live,
            default_id: code.to_owned(),
        };
        let cases: Vec<(RulesOverrides, RulesError)> = vec![
            (
                patch("titan", suffixes(&["Ti-tan"])),
                RulesError::InvalidSuffix {
                    row: "titan".into(),
                    suffix: "Ti-tan".into(),
                },
            ),
            (
                patch("titan", suffixes(&[""])),
                RulesError::InvalidSuffix {
                    row: "titan".into(),
                    suffix: String::new(),
                },
            ),
            (
                RulesOverrides {
                    products: vec![product("wowz", "_RETAIL_")],
                    ..RulesOverrides::default()
                },
                RulesError::DuplicateProductFolder {
                    folder: "_RETAIL_".into(),
                    first: "wow".into(),
                    second: "wowz".into(),
                },
            ),
            (
                RulesOverrides {
                    products: vec![product("wowz", " ")],
                    ..RulesOverrides::default()
                },
                RulesError::EmptyProductField {
                    code: "wowz".into(),
                    field: "folder",
                },
            ),
            (
                RulesOverrides {
                    family_ranges: BTreeMap::from([(
                        Family::Tbc,
                        NumRange::between(20_599, 20_500),
                    )]),
                    ..RulesOverrides::default()
                },
                RulesError::InvertedRange {
                    what: "interface range of family tbc".into(),
                    range: "20599..=20500".into(),
                },
            ),
            (
                RulesOverrides {
                    family_ranges: BTreeMap::from([(Family::Unknown, NumRange::at_least(0))]),
                    ..RulesOverrides::default()
                },
                RulesError::UnknownFamilyRange,
            ),
            (
                RulesOverrides {
                    version_families: vec![VersionFamilyRule {
                        install_kind: None,
                        major: NumRange::between(7, 6),
                        minor: NumRange::at_least(0),
                        family: Family::Mainline,
                    }],
                    ..RulesOverrides::default()
                },
                RulesError::InvertedRange {
                    what: "version family row 1 (major)".into(),
                    range: "7..=6".into(),
                },
            ),
            (
                patch(
                    "mainline",
                    ClientRulesOverride {
                        version_max: Some(ClientVersion::new(12, 0, 0)),
                        ..ClientRulesOverride::default()
                    },
                ),
                RulesError::InvertedRange {
                    what: "versions of rules row mainline".into(),
                    range: "12.1.5..=12.0.0".into(),
                },
            ),
            (
                patch(
                    "legion",
                    ClientRulesOverride {
                        family: Some(Family::Mainline),
                        ..ClientRulesOverride::default()
                    },
                ),
                RulesError::MissingField {
                    row: "legion".into(),
                    field: "version_min",
                },
            ),
        ];
        for (overrides, expected) in cases {
            assert_eq!(table().with_overrides(&overrides), Err(expected));
        }
    }

    // ---- Snapshot ------------------------------------------------------------------

    #[test]
    fn builtin_table_snapshot() {
        insta::assert_json_snapshot!("builtin_rules_table", RulesTable::builtin());
    }
}
