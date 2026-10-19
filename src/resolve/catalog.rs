//! The built-in addon catalog (vision §5.4): `omaddons install weakauras` works with no
//! account and no repository in mind. `catalog/catalog.toml` is embedded in the binary;
//! `<config>/omaddons/catalog.d/*.toml` overrides it in lexical file order (an entry with
//! an existing name replaces it, `remove = true` drops it, new names are added). A bad
//! override file is skipped whole with `catalog_override_invalid`: a user's typo must
//! never break install or status. Loading reads files only.
//!
//! Entries are `{name, aliases[], ref, families[], folders[], notes}`. `families` says
//! where the publisher declares an entry works: `info` shows it, `search` reports
//! compatibility from it, and it orders the folder lookups of dependencies and adoption
//! (entries declaring the client's family first, other entries of its generation only
//! after Tukui and WoWInterface, never one of the other generation). It never refuses
//! an install: TOC verification decides. `folders` feeds the folder index used by
//! adoption and dependencies.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Suggestion, Warning, WarningCode};
use crate::model::{FolderName, Ref};
use crate::rules::Family;
use crate::state::paths::Paths;

/// The embedded catalog.
pub const BUILTIN: &str = include_str!("../../catalog/catalog.toml");

/// One addon the catalog knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Entry {
    pub name: String,
    pub aliases: Vec<String>,
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub families: Vec<Family>,
    pub folders: Vec<FolderName>,
    pub notes: Option<String>,
}

impl Entry {
    /// Whether `name` is this entry's name or an alias, ignoring ASCII case.
    pub fn answers_to(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
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
    #[serde(default)]
    aliases: Vec<String>,
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
    Remove(String),
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
        let dir = paths.catalog_d();
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .map(|listing| {
                listing
                    .filter_map(std::result::Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "toml") && p.is_file())
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        for file in files {
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

    /// The entry named or aliased `name`, ignoring ASCII case.
    pub fn by_name(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.answers_to(name))
    }

    /// The entries that ship `folder`, in file order.
    pub fn by_folder(&self, folder: &FolderName) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.folders.contains(folder))
            .collect()
    }

    /// Applies one file's changes, then checks that names and aliases stay unique.
    fn apply(&mut self, changes: Vec<Change>) -> Result<(), String> {
        for change in changes {
            match change {
                Change::Upsert(entry) => match self
                    .entries
                    .iter()
                    .position(|e| e.name.eq_ignore_ascii_case(&entry.name))
                {
                    Some(at) => {
                        if let Some(slot) = self.entries.get_mut(at) {
                            *slot = entry;
                        }
                    }
                    None => self.entries.push(entry),
                },
                Change::Remove(name) => {
                    self.entries.retain(|e| !e.name.eq_ignore_ascii_case(&name))
                }
            }
        }
        let mut seen: Vec<(String, &str)> = Vec::new();
        for entry in &self.entries {
            for label in std::iter::once(&entry.name).chain(&entry.aliases) {
                let key = label.to_ascii_lowercase();
                if let Some((_, owner)) = seen.iter().find(|(k, _)| *k == key) {
                    return Err(format!("{label:?} names both {owner} and {}", entry.name));
                }
                seen.push((key, &entry.name));
            }
        }
        Ok(())
    }
}

/// Parses and validates one catalog file.
fn parse(text: &str, overrides: bool) -> Result<Vec<Change>, String> {
    let doc: Document = toml::from_str(text).map_err(|e| e.to_string())?;
    if !doc.schema_version.starts_with("1.") {
        return Err(format!("schema_version {} is not 1.x", doc.schema_version));
    }
    doc.addon
        .into_iter()
        .map(|raw| {
            let name = raw.name.trim().to_owned();
            if name.is_empty() {
                return Err("an entry has an empty name".to_owned());
            }
            if raw.remove {
                if !overrides {
                    return Err(format!("{name}: remove = true only belongs in catalog.d"));
                }
                return Ok(Change::Remove(name));
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
                aliases: raw
                    .aliases
                    .iter()
                    .map(|a| a.trim().to_owned())
                    .filter(|a| !a.is_empty())
                    .collect(),
                r#ref,
                families,
                folders,
                notes: raw.notes,
            }))
        })
        .collect()
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

    fn paths(dir: &std::path::Path) -> Paths {
        Paths::with_roots(dir, dir.join("c"), dir.join("s"), dir.join("k"))
    }

    fn write(paths: &Paths, name: &str, text: &str) {
        std::fs::create_dir_all(paths.catalog_d()).unwrap();
        std::fs::write(paths.catalog_d().join(name), text).unwrap();
    }

    const HEADER: &str = "schema_version = \"1.0\"\n";

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
            catalog.by_name(&first).unwrap().r#ref.to_string(),
            "gh:me/fork"
        );
        assert_eq!(catalog.by_name("MY").unwrap().name, "Mine");
        assert!(catalog.by_name(&second).is_none());
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
        assert!(Catalog::load(&paths).0.by_name("Ok").is_none());
    }

    #[test]
    fn lookups() {
        let catalog = Catalog::builtin().unwrap();
        let first = &catalog.entries()[0];
        assert_eq!(catalog.by_name(&first.name.to_uppercase()), Some(first));
        for alias in &first.aliases {
            assert_eq!(catalog.by_name(alias), Some(first), "{alias}");
        }
        let folder = &first.folders[0];
        assert!(catalog.by_folder(folder).contains(&first));
        assert!(catalog.by_name("no-such-addon").is_none());

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
