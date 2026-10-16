//! Adoption's evidence (M2-D40): for one unmanaged folder, every claim about where it
//! came from, each with its precedence. Pure: the lookups are in-memory traits the
//! caller fills (the catalog, the Tukui list, the WoWInterface index); nothing here
//! reads the disk, the network, the clock or the environment. Grouping the claims into
//! addons with tiers is the next step's.
//!
//! Precedence, strongest first: the catalog (what `install` would resolve the folder
//! to), then the TOC's Tukui id, `X-Website`, WoWInterface id, then a WoWInterface
//! `UIDir` match corroborated by the title or author, then name-only matches, then
//! CurseForge and Wago ids alone (M5). A weaker claim that disagrees with a stronger one
//! is still returned: grouping lists it as an alternative and never chooses it.

use schemars::JsonSchema;
use serde::Serialize;

use crate::inventory::{FolderClass, FolderEntry, TocSummary};
use crate::model::{FolderName, Ref};
use crate::resolve::catalog::{Catalog, Entry};
use crate::rules::Family;
use crate::sources::refs::{is_github_owner, is_github_repo, is_slug};
use crate::toc::plain_title;

/// The shortest prefix [`names_match`] accepts as a match of a longer name.
pub const NAME_PREFIX_MIN_CHARS: usize = 5;

/// Tukui's two projects by TOC id, for when the list is unavailable: `(id, slug, name)`.
pub const TUKUI_BUILTIN: [(i64, &str, &str); 2] = [(-1, "tukui", "Tukui"), (-2, "elvui", "ElvUI")];

/// An `AddOns` entry adoption may claim: unmanaged, a valid name, not one of a case
/// pair. Its loaded TOC, if one loads, is what it is identified by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFolder {
    pub folder: FolderName,
    pub toc: Option<TocSummary>,
}

impl CandidateFolder {
    /// The candidate an inventory entry is, if it is one: symlinks, protected, invalid
    /// and managed entries never are.
    pub fn from_entry(entry: &FolderEntry) -> Option<Self> {
        if entry.class != FolderClass::Unmanaged || entry.case_duplicate {
            return None;
        }
        Some(Self {
            folder: FolderName::new(&entry.name).ok()?,
            toc: entry.toc.clone(),
        })
    }
}

/// What evidence is looked up in. A missing source's lookup is `None`: its rows are
/// skipped, never fetched.
pub struct AdoptLookups<'a> {
    /// The target's family, for the catalog filter.
    pub family: Family,
    pub catalog: &'a dyn AdoptCatalog,
    /// `None`: the Tukui list is unavailable (the built-in ids still work).
    pub tukui: Option<&'a dyn AdoptTukui>,
    /// `None`: no index, or WoWInterface is disabled.
    pub wowi: Option<&'a dyn AdoptWowi>,
}

/// The catalog as adoption reads it.
pub trait AdoptCatalog {
    /// Every entry that ships `folder`, whatever its families.
    fn by_folder(&self, folder: &FolderName) -> Vec<&Entry>;
    /// The entry `name` is the name or an alias of, ignoring ASCII case.
    fn by_name(&self, name: &str) -> Option<&Entry>;
}

impl AdoptCatalog for Catalog {
    fn by_folder(&self, folder: &FolderName) -> Vec<&Entry> {
        Catalog::by_folder(self, folder)
    }

    fn by_name(&self, name: &str) -> Option<&Entry> {
        Catalog::by_name(self, name)
    }
}

/// The Tukui list as adoption reads it.
pub trait AdoptTukui {
    fn by_id(&self, id: i64) -> Option<TukuiProject>;
    fn by_slug(&self, slug: &str) -> Option<TukuiProject>;
}

/// One Tukui project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TukuiProject {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub directories: Vec<FolderName>,
}

/// The WoWInterface index as adoption reads it.
pub trait AdoptWowi {
    /// The projects whose sanitised `UIDir` lists `folder`, ignoring case.
    fn by_folder(&self, folder: &FolderName) -> Vec<u64>;
    fn by_uid(&self, uid: u64) -> Option<WowiProject>;
    /// The projects whose [`normalise`]d `UIName` equals `normalised`.
    fn by_name(&self, normalised: &str) -> Vec<u64>;
}

/// One WoWInterface project; `dirs` sanitised as the index stores them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WowiProject {
    pub uid: u64,
    pub name: String,
    pub author: String,
    pub dirs: Vec<FolderName>,
    pub downloads: u64,
    pub date_ms: i64,
}

/// What a claim rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// The catalog ships the folder for the target's family.
    CatalogFolder,
    /// `## X-Tukui-ProjectID`.
    TocTukuiId,
    /// `## X-Website` naming a GitHub repository, WoWInterface project or Tukui addon.
    TocWebsite,
    /// `## X-WoWI-ID`.
    TocWowiId,
    /// A WoWInterface project lists the folder, and its name or author matches the
    /// TOC's.
    WowiDirCorroborated,
    /// A WoWInterface project lists the folder, nothing more.
    WowiDir,
    /// The folder is named like a catalog entry or a WoWInterface project.
    NameMatch,
    /// `## X-Curse-Project-ID` alone (CurseForge arrives in M5).
    TocCurseId,
    /// `## X-Wago-ID` alone (Wago arrives in M5).
    TocWagoId,
}

/// How far a claim can be trusted, on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Strength {
    Catalog,
    TocId,
    Corroborated,
    NameOnly,
    CfOnly,
}

impl Evidence {
    /// The precedence, 1 strongest. The TOC ids share a strength but not a rank:
    /// Tukui's id before `X-Website` before `X-WoWI-ID` (M2-D40).
    pub const fn rank(self) -> u8 {
        match self {
            Self::CatalogFolder => 1,
            Self::TocTukuiId => 2,
            Self::TocWebsite => 3,
            Self::TocWowiId => 4,
            Self::WowiDirCorroborated => 5,
            Self::WowiDir | Self::NameMatch => 6,
            Self::TocCurseId | Self::TocWagoId => 7,
        }
    }

    pub const fn strength(self) -> Strength {
        match self {
            Self::CatalogFolder => Strength::Catalog,
            Self::TocTukuiId | Self::TocWebsite | Self::TocWowiId => Strength::TocId,
            Self::WowiDirCorroborated => Strength::Corroborated,
            Self::WowiDir | Self::NameMatch => Strength::NameOnly,
            Self::TocCurseId | Self::TocWagoId => Strength::CfOnly,
        }
    }
}

/// One claim about a candidate folder's source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Claim {
    pub evidence: Evidence,
    /// `None` only for the CurseForge and Wago ids.
    #[serde(rename = "ref")]
    pub r#ref: Option<Ref>,
    /// The folders the source ships together, always including the candidate, sorted
    /// by key.
    pub folders: Vec<FolderName>,
    /// The source's name for the addon: the catalog name, the Tukui name or the
    /// WoWInterface `UIName`.
    pub name: Option<String>,
}

/// Every claim about `candidate`, strongest first: by rank, then ref, then evidence;
/// each `(evidence, ref)` once.
pub fn evidence(candidate: &CandidateFolder, lookups: &AdoptLookups<'_>) -> Vec<Claim> {
    let f = &candidate.folder;
    let toc = candidate.toc.as_ref();
    let x_id = |key: &str| {
        toc.and_then(|t| {
            t.x_ids
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.trim().to_owned())
        })
    };
    let wowi_project = |uid: u64| lookups.wowi.and_then(|w| w.by_uid(uid));
    let mut claims: Vec<Claim> = Vec::new();
    // Rows 1–6, which carry a ref.
    {
        let mut push = |evidence, r#ref, folders, name| {
            claims.push(Claim {
                evidence,
                r#ref,
                folders,
                name,
            });
        };

        // 1. The catalog, for the target's family.
        for entry in lookups.catalog.by_folder(f) {
            if entry.families.is_empty() || entry.families.contains(&lookups.family) {
                push(
                    Evidence::CatalogFolder,
                    Some(entry.r#ref.clone()),
                    with(f, &entry.folders),
                    Some(entry.name.clone()),
                );
            }
        }

        // 2. The Tukui id: the list, else the built-in map.
        if let Some(id) = x_id("X-Tukui-ProjectID").and_then(|v| v.parse::<i64>().ok())
            && let Some(project) = lookups
                .tukui
                .and_then(|t| t.by_id(id))
                .or_else(|| builtin(id))
        {
            let listed: Vec<FolderName> = toc
                .map(|t| &t.tukui_project_folders)
                .into_iter()
                .flatten()
                .filter_map(|name| FolderName::new(name).ok())
                .collect();
            let folders = if listed.is_empty() {
                with(f, &project.directories)
            } else {
                with(f, &listed)
            };
            push(
                Evidence::TocTukuiId,
                Some(Ref::Tukui { slug: project.slug }),
                folders,
                Some(project.name),
            );
        }

        // 3. `X-Website`, when it names a source this build installs from.
        if let Some(r) = toc.and_then(|t| t.website.as_deref()).and_then(website_ref) {
            let (folders, name) = match &r {
                Ref::Wowi { id } => wowi_project(*id).map_or_else(
                    || (vec![f.clone()], None),
                    |p| (with(f, &p.dirs), Some(p.name)),
                ),
                Ref::Tukui { slug } => lookups.tukui.and_then(|t| t.by_slug(slug)).map_or_else(
                    || (vec![f.clone()], None),
                    |p| (with(f, &p.directories), Some(p.name)),
                ),
                _ => (vec![f.clone()], None),
            };
            push(Evidence::TocWebsite, Some(r), folders, name);
        }

        // 4. The WoWInterface id: no index needed.
        if let Some(id) = x_id("X-WoWI-ID")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|id| *id > 0)
        {
            let (folders, name) = wowi_project(id).map_or_else(
                || (vec![f.clone()], None),
                |p| (with(f, &p.dirs), Some(p.name)),
            );
            push(Evidence::TocWowiId, Some(Ref::Wowi { id }), folders, name);
        }

        // 5–6. WoWInterface `UIDir`: one project (or the only single-folder one) is
        // corroborated by the TOC's title or author; several are name-only each.
        if let Some(wowi) = lookups.wowi {
            let projects: Vec<WowiProject> = wowi
                .by_folder(f)
                .into_iter()
                .filter_map(|uid| wowi.by_uid(uid))
                .collect();
            let single: Vec<&WowiProject> = projects
                .iter()
                .filter(|p| p.dirs.len() == 1 && p.dirs.contains(f))
                .collect();
            let unique: Option<&WowiProject> = match (projects.as_slice(), single.as_slice()) {
                ([only], _) => Some(only),
                (_, [only]) => Some(*only),
                _ => None,
            };
            match unique {
                Some(p) => {
                    let corroborated = toc.is_some_and(|t| {
                        t.title_plain
                            .as_deref()
                            .is_some_and(|title| names_match(title, &p.name))
                            || t.author
                                .as_deref()
                                .is_some_and(|author| names_match(author, &p.author))
                    });
                    let evidence = if corroborated {
                        Evidence::WowiDirCorroborated
                    } else {
                        Evidence::WowiDir
                    };
                    push(
                        evidence,
                        Some(Ref::Wowi { id: p.uid }),
                        with(f, &p.dirs),
                        Some(p.name.clone()),
                    );
                }
                None => {
                    for p in &projects {
                        push(
                            Evidence::WowiDir,
                            Some(Ref::Wowi { id: p.uid }),
                            with(f, &p.dirs),
                            Some(p.name.clone()),
                        );
                    }
                }
            }
        }

        // 6. The folder named like a catalog entry (any family: only ever a guess) or a
        // WoWInterface project.
        if let Some(entry) = lookups.catalog.by_name(f.as_str()) {
            push(
                Evidence::NameMatch,
                Some(entry.r#ref.clone()),
                with(f, &entry.folders),
                Some(entry.name.clone()),
            );
        }
        if let Some(wowi) = lookups.wowi {
            for uid in wowi.by_name(&normalise(f.as_str())) {
                let (folders, name) = wowi_project(uid).map_or_else(
                    || (vec![f.clone()], None),
                    |p| (with(f, &p.dirs), Some(p.name)),
                );
                push(
                    Evidence::NameMatch,
                    Some(Ref::Wowi { id: uid }),
                    folders,
                    name,
                );
            }
        }
    }

    // 7. CurseForge and Wago ids, only when nothing else claims the folder.
    if claims.is_empty() {
        let alone = |evidence| Claim {
            evidence,
            r#ref: None,
            folders: vec![f.clone()],
            name: None,
        };
        if x_id("X-Curse-Project-ID")
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        {
            claims.push(alone(Evidence::TocCurseId));
        }
        if x_id("X-Wago-ID")
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric()))
        {
            claims.push(alone(Evidence::TocWagoId));
        }
    }

    claims.sort_by(|a, b| {
        a.evidence
            .rank()
            .cmp(&b.evidence.rank())
            .then_with(|| a.r#ref.cmp(&b.r#ref))
            .then_with(|| a.evidence.cmp(&b.evidence))
    });
    // ubs:ignore — comparing evidence kinds and refs, not secrets
    claims.dedup_by(|b, a| a.evidence == b.evidence && a.r#ref == b.r#ref);
    claims
}

/// A Tukui project from [`TUKUI_BUILTIN`].
fn builtin(id: i64) -> Option<TukuiProject> {
    TUKUI_BUILTIN
        .iter()
        .find(|(known, ..)| *known == id)
        .map(|(id, slug, name)| TukuiProject {
            id: *id,
            slug: (*slug).to_owned(),
            name: (*name).to_owned(),
            directories: Vec::new(),
        })
}

/// `folders` and `f`, each once (by key), sorted by key.
fn with(f: &FolderName, folders: &[FolderName]) -> Vec<FolderName> {
    let mut all: Vec<FolderName> = Vec::with_capacity(folders.len() + 1);
    for folder in folders.iter().chain(std::iter::once(f)) {
        if !all.contains(folder) {
            all.push(folder.clone());
        }
    }
    all.sort_by(|a, b| a.key().cmp(b.key()));
    all
}

/// The ref an `X-Website` link names, if it names one adoption can install from: a
/// GitHub repository (always `gh:`, whatever page of it), a WoWInterface project page
/// (`…/info<id>-<name>.html`) or a Tukui addon (`/addon/<slug>`, `/addons/<slug>`,
/// `?ui=<slug>`). `http://` is accepted: nothing is fetched, the scheme carries no
/// identity. Any other host, a GitHub account or non-repository page, and a bare Tukui
/// page give `None`, never a `url:` or `git:` ref.
pub fn website_ref(raw: &str) -> Option<Ref> {
    let raw = raw.trim();
    let lower = raw.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        raw.get("https://".len()..)?
    } else if lower.starts_with("http://") {
        raw.get("http://".len()..)?
    } else {
        return None;
    };
    let rest = rest.split('#').next().unwrap_or_default();
    let (authority, path_query) = rest.split_once('/').unwrap_or((rest, ""));
    let (path, query) = path_query.split_once('?').unwrap_or((path_query, ""));
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match host {
        "github.com" => {
            let [owner, repo, ..] = segments.as_slice() else {
                return None;
            };
            let not_accounts = ["sponsors", "orgs", "users", "topics"];
            if not_accounts.iter().any(|n| owner.eq_ignore_ascii_case(n)) {
                return None;
            }
            let repo = repo.strip_suffix(".git").unwrap_or(repo);
            (is_github_owner(owner) && is_github_repo(repo)).then(|| Ref::GitHub {
                owner: (*owner).to_owned(),
                repo: repo.to_owned(),
            })
        }
        "wowinterface.com" => {
            let digits = segments.last()?.strip_prefix("info")?;
            let id = digits.split(['-', '.']).next()?.parse::<u64>().ok()?;
            (id > 0).then_some(Ref::Wowi { id })
        }
        "tukui.org" => {
            let slug = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("ui="))
                .or(match segments.as_slice() {
                    ["addon" | "addons", slug] => Some(*slug),
                    _ => None,
                })?
                .to_ascii_lowercase();
            is_slug(&slug).then_some(Ref::Tukui { slug })
        }
        _ => None,
    }
}

/// A name as names are compared: UI escapes stripped ([`plain_title`]), lowercased,
/// every character that is neither alphanumeric nor whitespace dropped, whitespace
/// runs collapsed, trimmed.
pub fn normalise(name: &str) -> String {
    let kept: String = plain_title(name)
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether two names are the same name: both non-empty once [`normalise`]d, and equal,
/// or the shorter a prefix of the longer of at least [`NAME_PREFIX_MIN_CHARS`]
/// characters.
pub fn names_match(a: &str, b: &str) -> bool {
    let (a, b) = (normalise(a), normalise(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (short, long) = if a.chars().count() <= b.chars().count() {
        (a, b)
    } else {
        (b, a)
    };
    short == long || (short.chars().count() >= NAME_PREFIX_MIN_CHARS && long.starts_with(&short))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use proptest::prelude::*;

    use super::*;

    fn f(name: &str) -> FolderName {
        FolderName::new(name).unwrap()
    }

    fn r(s: &str) -> Ref {
        s.parse().unwrap()
    }

    fn entry(
        name: &str,
        aliases: &[&str],
        r#ref: &str,
        families: &[Family],
        folders: &[&str],
    ) -> Entry {
        Entry {
            name: name.into(),
            aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
            r#ref: r(r#ref),
            families: families.to_vec(),
            folders: folders.iter().map(|x| f(x)).collect(),
            notes: None,
        }
    }

    #[derive(Default)]
    struct FakeCatalog(Vec<Entry>);

    impl AdoptCatalog for FakeCatalog {
        fn by_folder(&self, folder: &FolderName) -> Vec<&Entry> {
            self.0
                .iter()
                .filter(|e| e.folders.contains(folder))
                .collect()
        }

        fn by_name(&self, name: &str) -> Option<&Entry> {
            self.0.iter().find(|e| e.answers_to(name))
        }
    }

    #[derive(Default)]
    struct FakeTukui(BTreeMap<i64, TukuiProject>);

    impl AdoptTukui for FakeTukui {
        fn by_id(&self, id: i64) -> Option<TukuiProject> {
            self.0.get(&id).cloned()
        }

        fn by_slug(&self, slug: &str) -> Option<TukuiProject> {
            self.0.values().find(|p| p.slug == slug).cloned()
        }
    }

    /// Projects by uid; `dirs` given raw and sanitised as the index stores them.
    #[derive(Default)]
    struct FakeWowi(BTreeMap<u64, WowiProject>);

    impl FakeWowi {
        fn with(mut self, uid: u64, name: &str, author: &str, dirs: &[&str]) -> Self {
            let raw: Vec<String> = dirs.iter().map(|d| (*d).to_owned()).collect();
            self.0.insert(
                uid,
                WowiProject {
                    uid,
                    name: name.into(),
                    author: author.into(),
                    dirs: crate::sources::wowi::index::sanitize_dirs(&raw),
                    downloads: 1,
                    date_ms: 0,
                },
            );
            self
        }
    }

    impl AdoptWowi for FakeWowi {
        fn by_folder(&self, folder: &FolderName) -> Vec<u64> {
            self.0
                .values()
                .filter(|p| p.dirs.contains(folder))
                .map(|p| p.uid)
                .collect()
        }

        fn by_uid(&self, uid: u64) -> Option<WowiProject> {
            self.0.get(&uid).cloned()
        }

        fn by_name(&self, normalised: &str) -> Vec<u64> {
            self.0
                .values()
                .filter(|p| normalise(&p.name) == normalised)
                .map(|p| p.uid)
                .collect()
        }
    }

    /// A loaded TOC: `title` raw (its plain form derived), `x` the `X-…` fields.
    fn toc(title: Option<&str>, author: Option<&str>, x: &[(&str, &str)]) -> TocSummary {
        let x_fields: BTreeMap<String, String> = x
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        TocSummary {
            file: "X.toc".into(),
            title: title.map(str::to_owned),
            title_plain: title.map(plain_title),
            version: None,
            author: author.map(str::to_owned),
            interfaces: vec![120_105],
            x_ids: x_fields
                .iter()
                .filter(|(k, _)| k.to_ascii_lowercase().ends_with("id"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            website: x_fields
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("X-Website"))
                .map(|(_, v)| v.clone()),
            dependencies: Vec::new(),
            optional_deps: Vec::new(),
            tukui_project_folders: x_fields
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("X-Tukui-ProjectFolders"))
                .map(|(_, v)| v.split(',').map(|s| s.trim().to_owned()).collect())
                .unwrap_or_default(),
        }
    }

    fn candidate(folder: &str, toc: Option<TocSummary>) -> CandidateFolder {
        CandidateFolder {
            folder: f(folder),
            toc,
        }
    }

    /// `(evidence, ref, folders, name)` of each claim, for compact tables.
    type Shown = (Evidence, Option<String>, Vec<String>, Option<String>);

    fn shown(claims: &[Claim]) -> Vec<Shown> {
        claims
            .iter()
            .map(|c| {
                (
                    c.evidence,
                    c.r#ref.as_ref().map(ToString::to_string),
                    c.folders.iter().map(|x| x.as_str().to_owned()).collect(),
                    c.name.clone(),
                )
            })
            .collect()
    }

    fn claim(
        evidence: Evidence,
        r#ref: Option<&str>,
        folders: &[&str],
        name: Option<&str>,
    ) -> Shown {
        (
            evidence,
            r#ref.map(str::to_owned),
            folders.iter().map(|x| (*x).to_owned()).collect(),
            name.map(str::to_owned),
        )
    }

    fn lookups<'a>(
        catalog: &'a FakeCatalog,
        tukui: Option<&'a FakeTukui>,
        wowi: Option<&'a FakeWowi>,
    ) -> AdoptLookups<'a> {
        AdoptLookups {
            family: Family::Mainline,
            catalog,
            tukui: tukui.map(|t| t as &dyn AdoptTukui),
            wowi: wowi.map(|w| w as &dyn AdoptWowi),
        }
    }

    #[test]
    fn wire_names_ranks_and_strengths() {
        let all = [
            (Evidence::CatalogFolder, "catalog_folder", 1, "catalog"),
            (Evidence::TocTukuiId, "toc_tukui_id", 2, "toc_id"),
            (Evidence::TocWebsite, "toc_website", 3, "toc_id"),
            (Evidence::TocWowiId, "toc_wowi_id", 4, "toc_id"),
            (
                Evidence::WowiDirCorroborated,
                "wowi_dir_corroborated",
                5,
                "corroborated",
            ),
            (Evidence::WowiDir, "wowi_dir", 6, "name_only"),
            (Evidence::NameMatch, "name_match", 6, "name_only"),
            (Evidence::TocCurseId, "toc_curse_id", 7, "cf_only"),
            (Evidence::TocWagoId, "toc_wago_id", 7, "cf_only"),
        ];
        for (evidence, wire, rank, strength) in all {
            assert_eq!(serde_json::to_value(evidence).unwrap(), wire);
            assert_eq!(evidence.rank(), rank, "{wire}");
            assert_eq!(serde_json::to_value(evidence.strength()).unwrap(), strength);
        }
    }

    #[test]
    fn only_unmanaged_unique_entries_are_candidates() {
        let entry = |class, case_duplicate, name: &str| FolderEntry {
            name: name.into(),
            key: name.to_ascii_lowercase(),
            class,
            owners: Vec::new(),
            case_duplicate,
            ownership_conflict: false,
            unreadable: false,
            toc: None,
            verdict: None,
            link_target: None,
        };
        assert!(
            CandidateFolder::from_entry(&entry(FolderClass::Unmanaged, false, "Foo")).is_some()
        );
        for (class, dup) in [
            (FolderClass::Unmanaged, true),
            (FolderClass::Managed, false),
            (FolderClass::Linked, false),
            (FolderClass::Protected, false),
            (FolderClass::Invalid, false),
        ] {
            assert!(
                CandidateFolder::from_entry(&entry(class, dup, "Foo")).is_none(),
                "{class:?} {dup}"
            );
        }
    }

    #[test]
    fn the_catalog_claims_for_the_targets_family() {
        let catalog = FakeCatalog(vec![
            entry(
                "BigWigs",
                &["bw"],
                "gh:BigWigsMods/BigWigs",
                &[],
                &["BigWigs", "BigWigs_Core"],
            ),
            entry(
                "Questie",
                &[],
                "gh:Questie/Questie",
                &[Family::Vanilla],
                &["Questie"],
            ),
            entry(
                "DBM",
                &[],
                "gh:DeadlyBossMods/DeadlyBossMods",
                &[Family::Mainline],
                &["DBM-Core"],
            ),
            entry(
                "DBM-Classic",
                &[],
                "gh:DeadlyBossMods/DBM-Classic",
                &[Family::Mainline],
                &["DBM-Core"],
            ),
        ]);
        let l = lookups(&catalog, None, None);
        let bw = toc(Some("BigWigs"), None, &[("X-WoWI-ID", "5086")]);
        assert_eq!(
            shown(&evidence(&candidate("BigWigs", Some(bw)), &l)),
            [
                claim(
                    Evidence::CatalogFolder,
                    Some("gh:BigWigsMods/BigWigs"),
                    &["BigWigs", "BigWigs_Core"],
                    Some("BigWigs")
                ),
                claim(Evidence::TocWowiId, Some("wowi:5086"), &["BigWigs"], None),
                claim(
                    Evidence::NameMatch,
                    Some("gh:BigWigsMods/BigWigs"),
                    &["BigWigs", "BigWigs_Core"],
                    Some("BigWigs")
                ),
            ],
            "the catalog wins; the TOC's WoWI id and the name match are still listed"
        );
        assert!(
            evidence(&candidate("Questie", None), &l)
                .iter()
                .all(|c| c.evidence != Evidence::CatalogFolder),
            "a vanilla-only entry does not claim on retail"
        );
        let dbm = evidence(&candidate("DBM-Core", None), &l);
        assert_eq!(
            dbm.iter()
                .filter(|c| c.evidence == Evidence::CatalogFolder)
                .count(),
            2,
            "two entries: two rank-1 claims"
        );
    }

    #[test]
    fn tukui_ids_resolve_through_the_list_or_the_builtin_map() {
        let catalog = FakeCatalog::default();
        let elv = toc(
            Some("|cff1784d1ElvUI|r"),
            Some("Elv"),
            &[
                ("X-Tukui-ProjectID", "-2"),
                (
                    "X-Tukui-ProjectFolders",
                    "ElvUI, ElvUI_Options, bad/name, ElvUI_Libraries",
                ),
            ],
        );
        let expected = [claim(
            Evidence::TocTukuiId,
            Some("tukui:elvui"),
            &["ElvUI", "ElvUI_Libraries", "ElvUI_Options"],
            Some("ElvUI"),
        )];
        assert_eq!(
            shown(&evidence(
                &candidate("ElvUI", Some(elv.clone())),
                &lookups(&catalog, None, None)
            )),
            expected,
            "no list: the built-in map; invalid names dropped"
        );
        let list = FakeTukui(BTreeMap::from([(
            -2,
            TukuiProject {
                id: -2,
                slug: "elvui".into(),
                name: "ElvUI".into(),
                directories: vec![f("ElvUI"), f("ElvUI_Libraries"), f("ElvUI_Options")],
            },
        )]));
        assert_eq!(
            shown(&evidence(
                &candidate("ElvUI", Some(elv)),
                &lookups(&catalog, Some(&list), None)
            )),
            expected
        );
        for id in ["-9", "x", ""] {
            let t = toc(None, None, &[("X-Tukui-ProjectID", id)]);
            assert!(
                evidence(&candidate("ElvUI", Some(t)), &lookups(&catalog, None, None)).is_empty(),
                "{id:?}"
            );
        }
    }

    #[test]
    fn websites_name_only_sources_adoption_installs_from() {
        let cases = [
            ("https://github.com/o/r", Some("gh:o/r")),
            ("http://github.com/o/r/releases", Some("gh:o/r")),
            ("https://www.github.com/o/r.git", Some("gh:o/r")),
            ("https://github.com/o/r/tree/main", Some("gh:o/r")),
            ("HTTPS://GitHub.com:443/O/R#readme", Some("gh:O/R")),
            ("https://github.com/o", None),
            ("https://github.com/sponsors/o", None),
            ("https://github.com/orgs/o/people", None),
            ("https://github.com/o/r@x", None),
            ("https://deadlybossmods.com", None),
            (
                "https://www.wowinterface.com/downloads/info5995-BugSack.html",
                Some("wowi:5995"),
            ),
            (
                "https://wowinterface.com/downloads/info5995",
                Some("wowi:5995"),
            ),
            ("https://www.wowinterface.com/forums/", None),
            (
                "https://www.wowinterface.com/downloads/info0-Zero.html",
                None,
            ),
            ("https://www.tukui.org/addon/elvui", Some("tukui:elvui")),
            ("https://tukui.org/addons/ElvUI", Some("tukui:elvui")),
            (
                "https://tukui.org/download.php?ui=tukui",
                Some("tukui:tukui"),
            ),
            ("https://tukui.org/news", None),
            ("ftp://github.com/o/r", None),
            ("github.com/o/r", None),
            ("", None),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                website_ref(raw).map(|r| r.to_string()).as_deref(),
                expected,
                "{raw}"
            );
        }
        let catalog = FakeCatalog::default();
        let wowi = FakeWowi::default().with(5995, "BugSack", "funkydude", &["BugSack"]);
        let t = toc(
            None,
            None,
            &[(
                "X-Website",
                "https://www.wowinterface.com/downloads/info5995-BugSack.html",
            )],
        );
        assert_eq!(
            shown(&evidence(
                &candidate("BugSack", Some(t)),
                &lookups(&catalog, None, Some(&wowi))
            )),
            [
                claim(
                    Evidence::TocWebsite,
                    Some("wowi:5995"),
                    &["BugSack"],
                    Some("BugSack")
                ),
                claim(
                    Evidence::WowiDir,
                    Some("wowi:5995"),
                    &["BugSack"],
                    Some("BugSack")
                ),
                claim(
                    Evidence::NameMatch,
                    Some("wowi:5995"),
                    &["BugSack"],
                    Some("BugSack")
                ),
            ]
        );
    }

    #[test]
    fn a_wowi_id_needs_no_index() {
        let catalog = FakeCatalog::default();
        let t = toc(None, None, &[("X-WoWI-ID", "5086")]);
        let without = evidence(
            &candidate("BigWigs", Some(t.clone())),
            &lookups(&catalog, None, None),
        );
        assert_eq!(
            shown(&without),
            [claim(
                Evidence::TocWowiId,
                Some("wowi:5086"),
                &["BigWigs"],
                None
            )]
        );
        let wowi = FakeWowi::default().with(
            5086,
            "BigWigs Bossmods",
            "Funkeh",
            &["BigWigs", "BigWigs_Core"],
        );
        let with_index = evidence(
            &candidate("BigWigs", Some(t)),
            &lookups(&catalog, None, Some(&wowi)),
        );
        assert_eq!(
            shown(&with_index)[0],
            claim(
                Evidence::TocWowiId,
                Some("wowi:5086"),
                &["BigWigs", "BigWigs_Core"],
                Some("BigWigs Bossmods")
            )
        );
    }

    #[test]
    fn a_uidir_match_needs_a_title_or_author_to_corroborate() {
        let catalog = FakeCatalog::default();
        let wowi = FakeWowi::default()
            .with(
                1,
                "Details! Damage Meter",
                "Terciob",
                &["Details", "Details_Streamer", "__MACOSX"],
            )
            .with(2, "Bagnon", "Jaliborc", &["Bagnon", "Bagnon_Config"])
            .with(3, "Bagnon Fork", "Someone", &["Bagnon", "Bagnon_Extra"])
            .with(4, "Lone", "Solo", &["Lone"])
            .with(5, "Lone Pack", "Other", &["Lone", "LonePack"]);
        let l = lookups(&catalog, None, Some(&wowi));
        let first = |c: CandidateFolder| shown(&evidence(&c, &l)).remove(0);

        let title = toc(Some("|cffffaa00Details!|r"), None, &[]);
        assert_eq!(
            first(candidate("Details", Some(title))).0,
            Evidence::WowiDirCorroborated,
            "a title that is a 5+ char prefix of UIName"
        );
        let author = toc(Some("Something Else"), Some("Terciob"), &[]);
        assert_eq!(
            first(candidate("Details", Some(author))).0,
            Evidence::WowiDirCorroborated
        );
        let neither = toc(Some("Other"), Some("Nobody"), &[]);
        assert_eq!(
            first(candidate("Details", Some(neither))),
            claim(
                Evidence::WowiDir,
                Some("wowi:1"),
                &["Details", "Details_Streamer"],
                Some("Details! Damage Meter")
            ),
            "junk dirs are not folders"
        );
        assert_eq!(first(candidate("Details", None)).0, Evidence::WowiDir);

        // Two multi-folder projects: one wowi_dir each (and a name match).
        let bagnon = shown(&evidence(
            &candidate("Bagnon", Some(toc(Some("Bagnon"), None, &[]))),
            &l,
        ));
        assert_eq!(
            bagnon,
            [
                claim(
                    Evidence::WowiDir,
                    Some("wowi:2"),
                    &["Bagnon", "Bagnon_Config"],
                    Some("Bagnon")
                ),
                claim(
                    Evidence::NameMatch,
                    Some("wowi:2"),
                    &["Bagnon", "Bagnon_Config"],
                    Some("Bagnon")
                ),
                claim(
                    Evidence::WowiDir,
                    Some("wowi:3"),
                    &["Bagnon", "Bagnon_Extra"],
                    Some("Bagnon Fork")
                ),
            ]
        );
        // The sole single-folder project among several is unique.
        let lone = shown(&evidence(
            &candidate("Lone", Some(toc(Some("Lone"), None, &[]))),
            &l,
        ));
        assert_eq!(
            lone[0],
            claim(
                Evidence::WowiDirCorroborated,
                Some("wowi:4"),
                &["Lone"],
                Some("Lone")
            )
        );
        assert!(lone.iter().all(|c| c.1.as_deref() != Some("wowi:5")));
    }

    #[test]
    fn a_catalog_name_or_alias_matches_whatever_its_families() {
        let catalog = FakeCatalog(vec![entry(
            "WeakAuras",
            &["wa"],
            "gh:WeakAuras/WeakAuras2",
            &[Family::Vanilla],
            &["WeakAuras", "WeakAurasOptions"],
        )]);
        let l = lookups(&catalog, None, None);
        assert_eq!(
            shown(&evidence(&candidate("WA", None), &l)),
            [claim(
                Evidence::NameMatch,
                Some("gh:WeakAuras/WeakAuras2"),
                &["WA", "WeakAuras", "WeakAurasOptions"],
                Some("WeakAuras")
            )]
        );
        assert_eq!(
            evidence(&candidate("WeakAuras", None), &l)
                .iter()
                .map(|c| c.evidence)
                .collect::<Vec<_>>(),
            [Evidence::NameMatch],
            "no catalog_folder outside the entry's families"
        );
    }

    #[test]
    fn curse_and_wago_ids_alone_claim_without_a_ref() {
        let catalog = FakeCatalog::default();
        let l = lookups(&catalog, None, None);
        let curse = toc(None, None, &[("X-Curse-Project-ID", "6273")]);
        assert_eq!(
            shown(&evidence(&candidate("BugSack", Some(curse)), &l)),
            [claim(Evidence::TocCurseId, None, &["BugSack"], None)]
        );
        let wago = toc(None, None, &[("X-Wago-ID", "rkGrrgGy")]);
        assert_eq!(
            shown(&evidence(&candidate("BugSack", Some(wago)), &l)),
            [claim(Evidence::TocWagoId, None, &["BugSack"], None)]
        );
        let both = toc(
            None,
            None,
            &[
                ("X-Curse-Project-ID", "6273"),
                ("X-Wago-ID", "rkGrrgGy"),
                ("X-WoWI-ID", "5995"),
            ],
        );
        assert_eq!(
            shown(&evidence(&candidate("BugSack", Some(both)), &l)),
            [claim(
                Evidence::TocWowiId,
                Some("wowi:5995"),
                &["BugSack"],
                None
            )],
            "never alongside ref evidence"
        );
        let junk = toc(
            None,
            None,
            &[("X-Curse-Project-ID", "abc"), ("X-Wago-ID", "a b")],
        );
        assert!(evidence(&candidate("BugSack", Some(junk)), &l).is_empty());
    }

    #[test]
    fn claims_are_deterministic_and_carry_only_installable_refs() {
        let catalog = FakeCatalog(vec![entry(
            "BugSack",
            &[],
            "gh:funkydude/BugSack",
            &[],
            &["BugSack"],
        )]);
        let wowi = FakeWowi::default().with(5995, "BugSack", "funkydude", &["BugSack"]);
        let t = toc(
            Some("BugSack"),
            Some("funkydude"),
            &[
                ("X-WoWI-ID", "5995"),
                ("X-Curse-Project-ID", "6273"),
                ("X-Website", "https://github.com/funkydude/BugSack"),
            ],
        );
        let c = candidate("BugSack", Some(t));
        let l = lookups(&catalog, None, Some(&wowi));
        let claims = evidence(&c, &l);
        assert_eq!(claims, evidence(&c, &l));
        assert_eq!(
            claims.iter().map(|c| c.evidence).collect::<Vec<_>>(),
            [
                Evidence::CatalogFolder,
                Evidence::TocWebsite,
                Evidence::TocWowiId,
                Evidence::WowiDirCorroborated,
                Evidence::NameMatch,
                Evidence::NameMatch,
            ]
        );
        for claim in &claims {
            assert!(
                claim.r#ref.as_ref().is_some_and(|r| matches!(
                    r,
                    Ref::GitHub { .. } | Ref::Wowi { .. } | Ref::Tukui { .. }
                )),
                "{claim:?}"
            );
        }
    }

    #[test]
    fn names_normalise_and_match_by_prefix() {
        assert_eq!(
            normalise("|cff33ff99Details!|r Damage Meter"),
            "details damage meter"
        );
        assert_eq!(
            normalise("|TInterface\\x:16|t  Bagnon  –  Bags"),
            "bagnon bags"
        );
        assert_eq!(normalise("|A:atlas:12:12|aÉlvUI"), "élvui");
        assert_eq!(normalise("!!!"), "");
        assert_eq!(normalise(""), "");
        assert!(names_match("Details!", "Details! Damage Meter"));
        assert!(names_match("BigWigs", "bigwigs"));
        assert!(names_match("Bagno", "Bagnon"), "a 5-char prefix");
        assert!(!names_match("Bagn", "Bagnon"), "a 4-char prefix");
        assert!(!names_match("", "Bagnon"));
        assert!(!names_match("!!!", "!!!"), "empty once normalised");
        assert!(!names_match("Details", "Bagnon"));
    }

    /// A ref adoption can install from.
    fn installable(r: &Ref) -> bool {
        matches!(r, Ref::GitHub { .. } | Ref::Wowi { .. } | Ref::Tukui { .. })
    }

    proptest! {
        #[test]
        fn websites_never_panic_and_only_name_installable_refs(raw in "\\PC{0,80}") {
            if let Some(r) = website_ref(&raw) {
                prop_assert!(installable(&r), "{}", r);
            }
        }

        #[test]
        fn shaped_websites_never_panic(
            scheme in prop_oneof![Just("https://"), Just("http://"), Just("HTTP://"), Just("")],
            host in prop_oneof![Just("github.com"), Just("www.wowinterface.com"), Just("tukui.org"), Just("u@x:1")],
            path in "[a-zA-Z0-9/._?=&#@:-]{0,40}",
        ) {
            if let Some(r) = website_ref(&format!("{scheme}{host}/{path}")) {
                prop_assert!(installable(&r), "{}", r);
            }
        }
    }
}
