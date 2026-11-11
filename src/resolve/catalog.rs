//! The built-in addon catalog (vision §5.4): `omaddons install weakauras` works with no
//! account and no repository in mind. `catalog/catalog.toml` is embedded in the binary;
//! `<config>/omaddons/catalog.d/*.toml` overrides it in lexical file order (an entry with
//! an existing identity replaces it, `remove = true` drops it, new ones are added). A bad
//! override file is skipped whole with `catalog_override_invalid`: a user's typo must
//! never break install or status. Loading reads files only.
//!
//! Entries are `{name, variant, aliases[], summary, ref, families[], folders[], notes}`.
//! `summary` is what a player reads in `search` (one sentence: what the addon does, from
//! its publisher's own description); `notes` are the maintainer's, never shown as a
//! description. `families`
//! says where the publisher declares an entry works: `info` shows it, `search` reports
//! compatibility from it, and it orders the folder lookups of dependencies and adoption
//! (entries declaring the client's family first, other entries of its generation only
//! after Tukui and WoWInterface, never one of the other generation). For a name with one
//! entry it never refuses an install: TOC verification decides.
//!
//! **Variants** (M3-D16): one name may have several entries, one build each for a set of
//! client families (BugGrabber's modern tags, and r152 for 3.3.5a). Each then has a
//! `variant`, its identity is `name#variant`, and the variants' families are pairwise
//! disjoint, so [`Catalog::resolve`] picks exactly one for a client's family, or none: a
//! client no variant covers is never offered another family's build. An override naming
//! a variant replaces or removes that one; one naming only the name replaces or removes
//! them all. `folders` feeds the folder index used by adoption and dependencies.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Suggestion, Warning, WarningCode};
use crate::limits::SEARCH_SUMMARY_MAX_CHARS;
use crate::model::{FolderName, Ref};
use crate::rules::Family;
use crate::state::paths::Paths;
use crate::text::{Entities, source_line};

/// The embedded catalog.
pub const BUILTIN: &str = include_str!("../../catalog/catalog.toml");

include!(concat!(env!("OUT_DIR"), "/catalog_folders.rs"));

/// The refs of the built-in entries naming `folder`, from the index `build.rs` makes of
/// the embedded catalog: what `Catalog::builtin().by_folder` answers, without parsing the
/// catalog (omad-7mw6). A unit test holds the two equal.
pub fn builtin_refs_naming(folder: &FolderName) -> Vec<Ref> {
    BUILTIN_FOLDERS
        .binary_search_by(|(key, _)| (*key).cmp(folder.key()))
        .ok()
        .and_then(|at| BUILTIN_FOLDERS.get(at))
        .map(|(_, refs)| refs.iter().filter_map(|r| r.parse().ok()).collect())
        .unwrap_or_default()
}

/// Which catalog entries name a folder, for counting adoptable folders (omad-7mw6): with
/// no `catalog.d` file, the built-in index, so no TOML is parsed; else the catalog with
/// its overrides, as `adopt` would find them.
#[derive(Debug)]
pub enum FolderRefs {
    Builtin,
    Loaded(Catalog),
}

impl FolderRefs {
    /// The built-in index when `catalog.d` holds no file [`Catalog::load`] would apply,
    /// else the loaded catalog with its warnings.
    pub fn load(paths: &Paths) -> (Self, Vec<Warning>) {
        if Catalog::has_overrides(paths) {
            let (catalog, warnings) = Catalog::load(paths);
            (Self::Loaded(catalog), warnings)
        } else {
            (Self::Builtin, Vec::new())
        }
    }

    /// The refs of the entries naming `folder`.
    pub fn naming(&self, folder: &FolderName) -> Vec<Ref> {
        match self {
            Self::Builtin => builtin_refs_naming(folder),
            Self::Loaded(catalog) => catalog
                .by_folder(folder)
                .into_iter()
                .map(|e| e.r#ref.clone())
                .collect(),
        }
    }
}

/// One addon the catalog knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Entry {
    pub name: String,
    /// Which build of `name` this is when the catalog has several (`modern`, `r152`);
    /// they select by the client's family.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    pub aliases: Vec<String>,
    /// What the addon does, in one sentence a player reads (`search`'s summary), made
    /// safe to show; `None` when the entry has none.
    pub summary: Option<String>,
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub families: Vec<Family>,
    pub folders: Vec<FolderName>,
    /// The maintainer's notes on how the entry was verified: never a description.
    pub notes: Option<String>,
}

impl Entry {
    /// Whether `name` is this entry's name or an alias, ignoring ASCII case.
    pub fn answers_to(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
    }

    /// `name`, or `name#variant`: what an override names.
    pub fn identity(&self) -> String {
        match &self.variant {
            Some(variant) => format!("{}#{variant}", self.name),
            None => self.name.clone(),
        }
    }

    /// Whether this entry is `name`'s, ignoring ASCII case.
    fn named(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    /// Whether this entry is the variant `variant` (`None`: any), ignoring ASCII case.
    fn is_variant(&self, variant: Option<&str>) -> bool {
        variant.is_none_or(|v| {
            self.variant
                .as_deref()
                .is_some_and(|w| w.eq_ignore_ascii_case(v))
        })
    }

    /// Whether the publisher declares `family` (an entry declaring none claims all).
    pub fn declares(&self, family: Family) -> bool {
        self.families.is_empty() || self.families.contains(&family)
    }

    /// Whether every family it declares is of one generation with a client that is
    /// `legacy` or not: a family of that generation it does not declare may still load
    /// its TOCs, one of the other never does.
    pub fn of_generation(&self, legacy: bool) -> bool {
        self.families.iter().all(|f| f.is_legacy() == legacy)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: String,
    #[serde(default)]
    addon: Vec<RawEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    name: String,
    variant: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    summary: Option<String>,
    #[serde(rename = "ref")]
    r#ref: Option<String>,
    #[serde(default)]
    families: Vec<String>,
    #[serde(default)]
    folders: Vec<String>,
    notes: Option<String>,
    #[serde(default)]
    remove: bool,
}

/// One change a file makes.
enum Change {
    Upsert(Entry),
    /// Every variant of a name, or one of them.
    Remove {
        name: String,
        variant: Option<String>,
    },
}

/// The merged catalog.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Catalog {
    entries: Vec<Entry>,
}

impl Catalog {
    /// The embedded catalog alone.
    pub fn builtin() -> Result<Self, String> {
        let mut catalog = Self::default();
        catalog.apply(parse(BUILTIN, false)?)?;
        Ok(catalog)
    }

    /// The `catalog.d` files [`Catalog::load`] applies, in its order: every `*.toml`
    /// regular file, by name.
    fn override_files(paths: &Paths) -> Vec<std::path::PathBuf> {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(paths.catalog_d())
            .map(|listing| {
                listing
                    .filter_map(std::result::Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "toml") && p.is_file())
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    /// Whether `catalog.d` holds a file [`Catalog::load`] would apply (or warn about).
    pub fn has_overrides(paths: &Paths) -> bool {
        !Self::override_files(paths).is_empty()
    }

    /// The embedded catalog with `catalog.d` overrides. Never fails: a broken file is
    /// a warning.
    pub fn load(paths: &Paths) -> (Self, Vec<Warning>) {
        let mut warnings = Vec::new();
        let mut catalog = match Self::builtin() {
            Ok(catalog) => catalog,
            Err(why) => {
                warnings.push(Warning::new(
                    WarningCode::CatalogOverrideInvalid,
                    format!("the built-in catalog does not load: {why}"),
                ));
                Self::default()
            }
        };
        for file in Self::override_files(paths) {
            let applied = std::fs::read_to_string(&file)
                .map_err(|e| e.to_string())
                .and_then(|text| parse(&text, true))
                .and_then(|changes| {
                    let mut next = catalog.clone();
                    next.apply(changes)?;
                    Ok(next)
                });
            match applied {
                Ok(next) => catalog = next,
                Err(why) => warnings.push(
                    Warning::new(
                        WarningCode::CatalogOverrideInvalid,
                        format!("{} is ignored: {why}", file.display()),
                    )
                    .with_fix(Suggestion::text(format!(
                        "fix or remove {}",
                        file.display()
                    ))),
                ),
            }
        }
        (catalog, warnings)
    }

    /// Every entry, in file order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Every entry `name` (a name or an alias, ignoring ASCII case) answers to, in file
    /// order: one name's variants, since an alias belongs to one name.
    pub fn variants(&self, name: &str) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.answers_to(name)).collect()
    }

    /// Whether `name` has variants, which select by the client's family.
    pub fn has_variants(&self, name: &str) -> bool {
        self.variants(name).iter().any(|e| e.variant.is_some())
    }

    /// The entry `name` (a name or an alias, ignoring ASCII case) means on a client of
    /// `family` (M3-D16). A name without variants is its one entry, whatever the family
    /// (TOC verification decides). A name with variants is the variant declaring
    /// `family`, else `None`: the catalog has no build for that client. Without a family
    /// (`info`, `search`): the variant declaring `mainline`, else the one whose first
    /// family comes first in [`Family::ALL`].
    pub fn resolve(&self, name: &str, family: Option<Family>) -> Option<&Entry> {
        let all = self.variants(name);
        if let [only] = all.as_slice()
            && only.variant.is_none()
        {
            return Some(only);
        }
        match family {
            Some(family) => all.into_iter().find(|e| e.families.contains(&family)),
            None => {
                let order = |e: &&Entry| {
                    e.families
                        .iter()
                        .filter_map(|f| Family::ALL.iter().position(|a| a == f))
                        .min()
                        .unwrap_or(usize::MAX)
                };
                let modern = all
                    .iter()
                    .copied()
                    .find(|e| e.families.contains(&Family::Mainline));
                modern.or_else(|| all.iter().copied().min_by_key(order))
            }
        }
    }

    /// The entries that ship `folder`, in file order.
    pub fn by_folder(&self, folder: &FolderName) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.folders.contains(folder))
            .collect()
    }

    /// Applies one file's changes by identity (a variant replaces or removes that one, a
    /// bare name all of the name's), then validates the result.
    fn apply(&mut self, changes: Vec<Change>) -> Result<(), String> {
        for change in changes {
            match change {
                Change::Upsert(entry) => {
                    let variant = entry.variant.clone();
                    let at = match &variant {
                        Some(v) => self
                            .entries
                            .iter()
                            .position(|e| e.named(&entry.name) && e.is_variant(Some(v))),
                        None => self.entries.iter().position(|e| e.named(&entry.name)),
                    };
                    match (at, variant) {
                        (Some(at), Some(_)) => {
                            if let Some(slot) = self.entries.get_mut(at) {
                                *slot = entry;
                            }
                        }
                        // Every variant of the name, replaced where the first one was.
                        (Some(at), None) => {
                            self.entries.retain(|e| !e.named(&entry.name));
                            self.entries.insert(at, entry);
                        }
                        (None, _) => self.entries.push(entry),
                    }
                }
                Change::Remove { name, variant } => self
                    .entries
                    .retain(|e| !(e.named(&name) && e.is_variant(variant.as_deref()))),
            }
        }
        self.validate()
    }

    /// Names and aliases each belong to one name (a name's variants share theirs); a name
    /// with several entries gives each a distinct `variant`, and their families are
    /// pairwise disjoint and never empty (an empty list would claim every family).
    fn validate(&self) -> Result<(), String> {
        let mut seen: Vec<(String, &str)> = Vec::new();
        for entry in &self.entries {
            for label in std::iter::once(&entry.name).chain(&entry.aliases) {
                let key = label.to_ascii_lowercase();
                match seen.iter().find(|(k, _)| *k == key) {
                    Some((_, owner)) if !entry.named(owner) => {
                        return Err(format!("{label:?} names both {owner} and {}", entry.name));
                    }
                    Some(_) => {}
                    None => seen.push((key, &entry.name)),
                }
            }
        }
        let mut names: Vec<&str> = Vec::new();
        for entry in &self.entries {
            if !names.iter().any(|n| entry.named(n)) {
                names.push(&entry.name);
            }
        }
        for name in names {
            let group: Vec<&Entry> = self.entries.iter().filter(|e| e.named(name)).collect();
            if let Some(e) = group
                .iter()
                .find(|e| e.variant.is_some() && e.families.is_empty())
            {
                return Err(format!(
                    "{} declares no family, so it would claim every client",
                    e.identity()
                ));
            }
            if group.len() < 2 {
                continue;
            }
            if group.iter().any(|e| e.variant.is_none()) {
                return Err(format!(
                    "{name} has {} entries, so each needs a variant",
                    group.len()
                ));
            }
            for (i, a) in group.iter().enumerate() {
                for b in &group[i + 1..] {
                    if b.is_variant(a.variant.as_deref()) {
                        return Err(format!("{} is defined twice", a.identity()));
                    }
                    if let Some(f) = a.families.iter().find(|f| b.families.contains(f)) {
                        return Err(format!(
                            "{} and {} both declare {}: a name's variants declare disjoint families",
                            a.identity(),
                            b.identity(),
                            f.as_str()
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// A variant key: letters, digits, `.`, `_` and `-`.
fn is_variant_key(v: &str) -> bool {
    !v.is_empty()
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Parses and validates one catalog file. A name appears in it once, or once per
/// variant: two entries of one identity would silently become one.
fn parse(text: &str, overrides: bool) -> Result<Vec<Change>, String> {
    let doc: Document = toml::from_str(text).map_err(|e| e.to_string())?;
    if !doc.schema_version.starts_with("1.") {
        return Err(format!("schema_version {} is not 1.x", doc.schema_version));
    }
    let changes = doc
        .addon
        .into_iter()
        .map(|raw| {
            let name = raw.name.trim().to_owned();
            if name.is_empty() {
                return Err("an entry has an empty name".to_owned());
            }
            let variant = raw.variant.map(|v| v.trim().to_owned());
            if let Some(v) = variant.as_deref().filter(|v| !is_variant_key(v)) {
                return Err(format!(
                    "{name}: variant {v:?} is not letters, digits, '.', '_' or '-'"
                ));
            }
            if raw.remove {
                if !overrides {
                    return Err(format!("{name}: remove = true only belongs in catalog.d"));
                }
                return Ok(Change::Remove { name, variant });
            }
            let r#ref = raw
                .r#ref
                .ok_or_else(|| format!("{name}: ref is missing"))?
                .parse::<Ref>()
                .map_err(|e| format!("{name}: {e}"))?;
            let families = raw
                .families
                .iter()
                .map(|f| {
                    serde_json::from_value::<Family>(serde_json::Value::String(f.clone()))
                        .map_err(|_| format!("{name}: {f:?} is not a family"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let folders = raw
                .folders
                .iter()
                .map(|f| {
                    let folder = FolderName::new(f).map_err(|e| format!("{name}: {e}"))?;
                    if folder.is_protected() {
                        return Err(format!("{name}: {f} belongs to the game client"));
                    }
                    Ok(folder)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Change::Upsert(Entry {
                name,
                variant,
                aliases: raw
                    .aliases
                    .iter()
                    .map(|a| a.trim().to_owned())
                    .filter(|a| !a.is_empty())
                    .collect(),
                // A catalog.d file is the user's own text, but it is shown on a
                // terminal and in the bar widget: one line, nothing that hides text.
                summary: raw
                    .summary
                    .map(|s| source_line(&s, SEARCH_SUMMARY_MAX_CHARS, Entities::Keep))
                    .filter(|s| !s.is_empty()),
                r#ref,
                families,
                folders,
                notes: raw.notes,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let upserts: Vec<&Entry> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Upsert(e) => Some(e),
            Change::Remove { .. } => None,
        })
        .collect();
    for (i, a) in upserts.iter().enumerate() {
        let twice = upserts[i + 1..].iter().find(|b| {
            b.named(&a.name)
                && (a.variant.is_none()
                    || b.variant.is_none()
                    || b.is_variant(a.variant.as_deref()))
        });
        if let Some(b) = twice {
            return Err(format!(
                "{} and {} are one entry twice: give each a distinct variant",
                a.identity(),
                b.identity()
            ));
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_catalog_is_valid() {
        let catalog = Catalog::builtin().unwrap();
        assert!(!catalog.entries().is_empty());
        for entry in catalog.entries() {
            assert_eq!(
                entry.r#ref.to_string().parse::<Ref>().unwrap(),
                entry.r#ref,
                "{}",
                entry.name
            );
            assert!(!entry.folders.is_empty(), "{}: folders", entry.name);
            assert!(!entry.families.is_empty(), "{}: families", entry.name);
        }
    }

    /// What `search` shows a player as a catalog hit's description (omad-5c7j): every
    /// built-in entry has one sentence of its own, never its maintainer notes.
    #[test]
    fn every_builtin_entry_has_a_one_sentence_summary_that_is_not_its_notes() {
        let catalog = Catalog::builtin().unwrap();
        for entry in catalog.entries() {
            let id = entry.identity();
            let summary = entry
                .summary
                .as_deref()
                .unwrap_or_else(|| panic!("{id}: no summary"));
            assert!(
                (10..=120).contains(&summary.chars().count()),
                "{id}: {summary:?} is not one short sentence"
            );
            assert!(
                summary.ends_with('.'),
                "{id}: {summary:?} ends without a full stop"
            );
            assert!(
                !summary.trim_end_matches('.').contains(". "),
                "{id}: {summary:?} is more than one sentence"
            );
            assert_ne!(Some(summary), entry.notes.as_deref(), "{id}: its notes");
        }
    }

    #[test]
    fn an_override_summary_is_made_safe_and_replaces_with_its_entry() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let builtin = Catalog::builtin().unwrap();
        let first = builtin.entries()[0].name.clone();
        write(
            &paths,
            "10-mine.toml",
            &format!(
                "{HEADER}[[addon]]\nname = \"{first}\"\nsummary = \"  Mine,\\u001b[31m\\nnow\\u202e red  \"\nref = \"gh:me/fork\"\nfolders = [\"Fork\"]\n\n[[addon]]\nname = \"Bare\"\nref = \"gh:me/bare\"\nfolders = [\"Bare\"]\n"
            ),
        );
        let (catalog, warnings) = Catalog::load(&paths);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            catalog.resolve(&first, None).unwrap().summary.as_deref(),
            Some("Mine,[31m now red"),
            "control and text-hiding characters dropped, whitespace folded, one line"
        );
        assert_eq!(catalog.resolve("Bare", None).unwrap().summary, None);
    }

    fn paths(dir: &std::path::Path) -> Paths {
        Paths::with_roots(dir, dir.join("c"), dir.join("s"), dir.join("k"))
    }

    fn write(paths: &Paths, name: &str, text: &str) {
        std::fs::create_dir_all(paths.catalog_d()).unwrap();
        std::fs::write(paths.catalog_d().join(name), text).unwrap();
    }

    const HEADER: &str = "schema_version = \"1.0\"\n";

    /// omad-7mw6: the index `build.rs` makes answers every folder of every built-in entry
    /// as the parsed catalog does, in any case, and nothing for a folder no entry names.
    #[test]
    fn the_builtin_index_answers_as_the_parsed_catalog() {
        let catalog = Catalog::builtin().unwrap();
        let mut folders: Vec<&FolderName> =
            catalog.entries().iter().flat_map(|e| &e.folders).collect();
        folders.sort();
        folders.dedup();
        assert_eq!(
            folders.len(),
            BUILTIN_FOLDERS.len(),
            "one index row per folder"
        );
        for folder in folders {
            let parsed: Vec<Ref> = catalog
                .by_folder(folder)
                .into_iter()
                .map(|e| e.r#ref.clone())
                .collect();
            assert_eq!(builtin_refs_naming(folder), parsed, "{folder}");
            let shouted = FolderName::try_from(folder.as_str().to_ascii_uppercase()).unwrap();
            assert_eq!(builtin_refs_naming(&shouted), parsed, "{shouted}");
        }
        let nobody = FolderName::try_from("NoCatalogEntryNamesThis".to_owned()).unwrap();
        assert_eq!(builtin_refs_naming(&nobody), Vec::<Ref>::new());
    }

    /// omad-7mw6: with no `catalog.d` file the lookup is the built-in index (nothing
    /// parsed); with one, even a broken one, it is the loaded catalog, overrides and
    /// warnings included.
    #[test]
    fn folder_refs_parse_only_with_an_override() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let (refs, warnings) = FolderRefs::load(&paths);
        assert!(matches!(refs, FolderRefs::Builtin), "{refs:?}");
        assert!(warnings.is_empty());
        // A folder that is no `*.toml` is no override.
        write(&paths, "notes.txt", "not a catalog");
        assert!(matches!(FolderRefs::load(&paths).0, FolderRefs::Builtin));

        let mine = FolderName::try_from("MyBars".to_owned()).unwrap();
        assert_eq!(builtin_refs_naming(&mine), Vec::<Ref>::new());
        write(
            &paths,
            "50-mine.toml",
            &format!(
                "{HEADER}[[addon]]\nname = \"MyBars\"\nref = \"gh:example/MyBars\"\nfolders = [\"MyBars\"]\n"
            ),
        );
        let (refs, warnings) = FolderRefs::load(&paths);
        assert!(matches!(refs, FolderRefs::Loaded(_)), "{refs:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            refs.naming(&mine),
            vec!["gh:example/MyBars".parse::<Ref>().unwrap()]
        );
        write(&paths, "60-broken.toml", "not = [toml");
        let (refs, warnings) = FolderRefs::load(&paths);
        assert!(matches!(refs, FolderRefs::Loaded(_)));
        assert_eq!(
            warnings.iter().map(|w| w.code).collect::<Vec<_>>(),
            [WarningCode::CatalogOverrideInvalid]
        );
    }

    #[test]
    fn overrides_apply_in_lexical_order() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let builtin = Catalog::builtin().unwrap();
        let first = builtin.entries()[0].name.clone();
        let second = builtin.entries()[1].name.clone();
        write(
            &paths,
            "10-replace.toml",
            &format!(
                "{HEADER}[[addon]]\nname = \"{first}\"\nref = \"gh:me/fork\"\nfamilies = [\"wrath-legacy\"]\nfolders = [\"Fork\"]\n"
            ),
        );
        write(
            &paths,
            "20-add.toml",
            &format!(
                "{HEADER}[[addon]]\nname = \"Mine\"\naliases = [\"my\"]\nref = \"git:https://codeberg.org/me/mine.git\"\nfamilies = [\"wrath-legacy\"]\nfolders = [\"Mine\", \"MineOptions\"]\n"
            ),
        );
        write(
            &paths,
            "30-remove.toml",
            &format!("{HEADER}[[addon]]\nname = \"{second}\"\nremove = true\n"),
        );
        write(&paths, "40-readme.txt", "not toml");
        let (catalog, warnings) = Catalog::load(&paths);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            catalog.resolve(&first, None).unwrap().r#ref.to_string(),
            "gh:me/fork"
        );
        assert_eq!(catalog.resolve("MY", None).unwrap().name, "Mine");
        assert!(catalog.resolve(&second, None).is_none());
        assert_eq!(catalog.entries().len(), builtin.entries().len());
    }

    #[test]
    fn a_bad_override_is_skipped_whole() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let builtin = Catalog::builtin().unwrap();
        let first = builtin.entries()[0].name.clone();
        let bad: &[(&str, String)] = &[
            ("a.toml", "this is = = not toml".to_owned()),
            (
                "b.toml",
                format!("{HEADER}[[addon]]\nname = \"X\"\nref = \"nonsense\"\n"),
            ),
            (
                "c.toml",
                format!(
                    "{HEADER}[[addon]]\nname = \"Y\"\nref = \"gh:a/b\"\nfolders = [\"Blizzard_X\"]\n"
                ),
            ),
            (
                "d.toml",
                format!(
                    "{HEADER}[[addon]]\nname = \"Z\"\nref = \"gh:a/b\"\nfamilies = [\"classic\"]\n"
                ),
            ),
            (
                "e.toml",
                format!(
                    "{HEADER}[[addon]]\nname = \"Dup\"\naliases = [\"{first}\"]\nref = \"gh:a/b\"\n"
                ),
            ),
            (
                "f.toml",
                format!(
                    "{HEADER}[[addon]]\nname = \"Ok\"\nref = \"gh:a/b\"\n[[addon]]\nname = \"Bad\"\nref = \"gh:a\"\n"
                ),
            ),
            ("g.toml", "schema_version = \"2.0\"\n".to_owned()),
            (
                "h.toml",
                format!("{HEADER}[[addon]]\nname = \"W\"\nref = \"gh:a/b\"\nsurprise = 1\n"),
            ),
        ];
        for (name, text) in bad {
            write(&paths, name, text);
        }
        let (catalog, warnings) = Catalog::load(&paths);
        assert_eq!(warnings.len(), bad.len(), "{warnings:?}");
        assert!(
            warnings
                .iter()
                .all(|w| w.code == WarningCode::CatalogOverrideInvalid)
        );
        assert!(
            warnings[0].message.contains("a.toml"),
            "{}",
            warnings[0].message
        );
        assert_eq!(
            catalog, builtin,
            "built-ins intact, and nothing from a half-valid file"
        );
        assert!(Catalog::load(&paths).0.resolve("Ok", None).is_none());
    }

    /// `text`'s entries alone, loaded as an override file is.
    fn only(text: &str) -> Result<Catalog, String> {
        let mut catalog = Catalog::default();
        catalog.apply(parse(&format!("{HEADER}{text}"), true)?)?;
        Ok(catalog)
    }

    /// One `Grab` variant declaring `families`.
    fn grab(variant: &str, families: &str) -> String {
        format!(
            "[[addon]]\nname = \"Grab\"\nvariant = \"{variant}\"\naliases = [\"g\"]\n\
             ref = \"wowace:bug-grabber#r{}\"\nfamilies = [{families}]\nfolders = [\"!Grab\"]\n",
            100 + variant.len()
        )
    }

    #[test]
    fn variants_select_by_family_and_never_cross() {
        let text = [
            grab("modern", "\"mainline\", \"vanilla\""),
            grab("tbc", "\"tbc-legacy\""),
            grab("wrath", "\"wrath-legacy\""),
            grab("cata", "\"cata-legacy\""),
            grab("mists", "\"mists-legacy\""),
        ]
        .concat();
        let catalog = only(&text).unwrap();
        let variant = |family| {
            catalog
                .resolve("G", family)
                .and_then(|e| e.variant.as_deref())
        };
        assert_eq!(variant(Some(Family::Mainline)), Some("modern"));
        assert_eq!(variant(Some(Family::Vanilla)), Some("modern"));
        assert_eq!(variant(Some(Family::WrathLegacy)), Some("wrath"));
        assert_eq!(variant(Some(Family::MistsLegacy)), Some("mists"));
        assert_eq!(
            variant(Some(Family::VanillaLegacy)),
            None,
            "no build, none borrowed"
        );
        assert_eq!(variant(Some(Family::Tbc)), None);
        assert_eq!(variant(None), Some("modern"), "the mainline one by default");
        assert!(catalog.has_variants("grab"));
        assert_eq!(catalog.variants("g").len(), 5);
        assert_eq!(
            catalog.resolve("g", None).unwrap().identity(),
            "Grab#modern"
        );

        let legacy = only(
            &[
                grab("wrath", "\"wrath-legacy\""),
                grab("tbc", "\"tbc-legacy\""),
            ]
            .concat(),
        )
        .unwrap();
        assert_eq!(
            legacy.resolve("grab", None).unwrap().identity(),
            "Grab#tbc",
            "without mainline: the first family in order"
        );
        // A lone variant still selects by family.
        let lone = only(&grab("wrath", "\"wrath-legacy\"")).unwrap();
        assert!(lone.resolve("grab", Some(Family::Mainline)).is_none());
        assert!(
            !only("[[addon]]\nname = \"X\"\nref = \"gh:a/b\"\n")
                .unwrap()
                .has_variants("x")
        );
    }

    #[test]
    fn the_validator_refuses_what_could_not_be_resolved() {
        let two = |a: &str, b: &str| format!("{a}{b}");
        let cases: &[(String, &str)] = &[
            (
                two(
                    &grab("a", "\"wrath-legacy\""),
                    &grab("b", "\"wrath-legacy\", \"tbc-legacy\""),
                ),
                "Grab#a and Grab#b both declare wrath-legacy",
            ),
            (
                two(&grab("a", "\"wrath-legacy\""), &grab("A", "\"tbc-legacy\"")),
                "one entry twice",
            ),
            (
                two(
                    &grab("a", "\"wrath-legacy\""),
                    "[[addon]]\nname = \"grab\"\nref = \"gh:a/b\"\nfamilies = [\"tbc\"]\n",
                ),
                "one entry twice",
            ),
            (grab("a", ""), "Grab#a declares no family"),
            (
                two(
                    &grab("a", "\"wrath-legacy\""),
                    "[[addon]]\nname = \"Other\"\naliases = [\"g\"]\nref = \"gh:a/b\"\n",
                ),
                "\"g\" names both Grab and Other",
            ),
            (grab("a b", "\"wrath-legacy\""), "is not letters"),
        ];
        for (text, why) in cases {
            let err = only(text).unwrap_err();
            assert!(err.contains(why), "{err} (wanted {why:?})");
        }
        // Across files: a bare entry beside a variant has no variant of its own.
        let mut catalog = only(&grab("a", "\"wrath-legacy\"")).unwrap();
        let bare = parse(
            &format!("{HEADER}[[addon]]\nname = \"Other\"\nref = \"gh:a/b\"\n"),
            true,
        )
        .unwrap();
        catalog.apply(bare).unwrap();
        let mut other = catalog.clone();
        let renamed = parse(
            &format!(
                "{HEADER}[[addon]]\nname = \"Grab2\"\naliases = [\"x\"]\nref = \"gh:a/b\"\n{}",
                grab("b", "\"tbc-legacy\"")
            ),
            true,
        )
        .unwrap();
        other.apply(renamed).unwrap();
        assert_eq!(
            other.variants("grab").len(),
            2,
            "aliases shared within one name"
        );
    }

    #[test]
    fn overrides_merge_by_identity() {
        let builtin = Catalog::builtin().unwrap();
        let identities = |c: &Catalog| -> Vec<String> {
            c.variants("BugGrabber")
                .iter()
                .map(|e| e.identity())
                .collect()
        };
        assert_eq!(
            identities(&builtin),
            [
                "BugGrabber#modern",
                "BugGrabber#r152",
                "BugGrabber#r130",
                "BugGrabber#r43"
            ],
            "the shipped variants survive the embedded apply"
        );
        let load = |text: &str| {
            let dir = tempfile::tempdir().unwrap();
            let paths = paths(dir.path());
            write(&paths, "10.toml", &format!("{HEADER}{text}"));
            Catalog::load(&paths)
        };

        let (one, warnings) = load(
            "[[addon]]\nname = \"BugGrabber\"\nvariant = \"r152\"\naliases = [\"!buggrabber\"]\nref = \"wowace:bug-grabber#r150\"\nfamilies = [\"wrath-legacy\"]\nfolders = [\"!BugGrabber\"]\n",
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(identities(&one), identities(&builtin), "in place");
        assert_eq!(
            one.resolve("buggrabber", Some(Family::WrathLegacy))
                .unwrap()
                .r#ref
                .to_string(),
            "wowace:bug-grabber#r150"
        );
        assert_eq!(
            one.resolve("buggrabber", Some(Family::Mainline)),
            builtin.resolve("buggrabber", Some(Family::Mainline)),
            "the modern variant intact"
        );

        let (bare, warnings) = load(
            "[[addon]]\nname = \"BugGrabber\"\nref = \"gh:me/grabber\"\nfolders = [\"!BugGrabber\"]\n",
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            identities(&bare),
            ["BugGrabber"],
            "a bare name replaces them all"
        );
        assert!(
            !bare.has_variants("bug-grabber"),
            "and its aliases with them"
        );

        let (gone, warnings) = load("[[addon]]\nname = \"BugGrabber\"\nremove = true\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(identities(&gone).is_empty());

        let (fewer, warnings) =
            load("[[addon]]\nname = \"BugGrabber\"\nvariant = \"r130\"\nremove = true\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            identities(&fewer),
            ["BugGrabber#modern", "BugGrabber#r152", "BugGrabber#r43"]
        );

        let (kept, warnings) = load(
            "[[addon]]\nname = \"BugGrabber\"\nvariant = \"new\"\nref = \"wowace:bug-grabber#r150\"\nfamilies = [\"wrath-legacy\"]\nfolders = [\"!BugGrabber\"]\n",
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].code, WarningCode::CatalogOverrideInvalid);
        assert!(
            warnings[0].message.contains("both declare wrath-legacy"),
            "{}",
            warnings[0].message
        );
        assert_eq!(kept, builtin, "an overlapping file is skipped whole");
    }

    #[test]
    fn lookups() {
        let catalog = Catalog::builtin().unwrap();
        let first = &catalog.entries()[0];
        // A name without variants is its entry, whatever the family.
        for family in [None, Some(Family::Mainline), Some(Family::VanillaLegacy)] {
            assert_eq!(
                catalog.resolve(&first.name.to_uppercase(), family),
                Some(first)
            );
        }
        for alias in &first.aliases {
            assert_eq!(catalog.resolve(alias, None), Some(first), "{alias}");
        }
        let folder = &first.folders[0];
        assert!(catalog.by_folder(folder).contains(&first));
        assert!(catalog.resolve("no-such-addon", None).is_none());

        let mut shared = catalog.clone();
        let mut twin = first.clone();
        twin.name = "Twin".into();
        twin.aliases.clear();
        shared.apply(vec![Change::Upsert(twin)]).unwrap();
        let owners: Vec<&str> = shared
            .by_folder(folder)
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(owners, [first.name.as_str(), "Twin"], "both, in file order");
    }
}
