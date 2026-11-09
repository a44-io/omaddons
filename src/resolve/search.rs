//! Search, the pure part (M2 §5.4): hits from the catalog, the WoWInterface index, the
//! Tukui list and GitHub, ranked into one list with a compatibility hint per client. No
//! I/O and no clock: the command gathers the hits and passes `now`.
//!
//! - **Tiers:** 0, the whole query is a catalog name or alias, or a WoWInterface or Tukui
//!   project's name; 1, a name starts with it; 2, every word is in the name or a folder;
//!   3, every word is somewhere in the name, folders, author or description. A hit that
//!   reaches no tier is dropped.
//! - **Within a tier:** the catalog first, then popularity (per source, log-scaled to the
//!   source's best in this result set), then the newest, then name and ref.
//! - **Duplicates:** a hit whose ref is a catalog entry's, or whose name is the entry's
//!   name or one of its aliases (ignoring case, spaces and punctuation) and which shares
//!   at least one of the entry's folders, merges into the catalog hit, which keeps its
//!   name, ref and compatibility and takes the hit's popularity and date.

use std::collections::BTreeMap;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::limits::{DEPENDENCY_SOURCE_MAX_AGE, SEARCH_SUMMARY_MAX_CHARS};
use crate::model::Ref;
use crate::rules::Family;

/// Where a result comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HitSource {
    Catalog,
    Wowi,
    Tukui,
    Github,
}

/// How popular a result is: WoWInterface downloads or GitHub stars. `info`'s `about`
/// carries it too, and the about memo keeps it (`state::about_memo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Popularity {
    Downloads(u64),
    Stars(u64),
}

impl Popularity {
    fn count(self) -> u64 {
        match self {
            Self::Downloads(n) | Self::Stars(n) => n,
        }
    }
}

/// A compatibility hint for one client; `install` still verifies every TOC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Compat {
    /// The catalog says the addon ships for the client's family.
    Yes,
    /// The project says the addon supports the family (Tukui patches, WoWInterface's
    /// declared versions).
    Likely,
    /// Nothing says either way.
    Unknown,
    /// Tukui's patches name other families only.
    Unlikely,
    /// The catalog says it does not ship for the family.
    No,
    /// A WoWInterface project not updated in `DEPENDENCY_SOURCE_MAX_AGE`.
    Abandoned,
}

impl Compat {
    /// Whether `search` may suggest installing it on a client: only on evidence (`yes`,
    /// `likely`). An `unknown` everywhere gets `info` instead, which judges each client;
    /// no client is guessed from no evidence (bmcx.6).
    pub fn suggestible(self) -> bool {
        matches!(self, Self::Yes | Self::Likely)
    }
}

/// What a hit's compatibility is judged from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompatBasis {
    /// The catalog entry's families; none means unknown.
    Catalog(Vec<Family>),
    /// The families of the Tukui project's patches; none means unknown.
    Tukui(Vec<Family>),
    /// The families of the WoWInterface project's declared versions, and its date.
    Wowi(Vec<Family>, Option<Timestamp>),
    Github,
}

/// One hit, as a source found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub name: String,
    pub r#ref: Ref,
    pub source: HitSource,
    pub author: Option<String>,
    /// The catalog's notes or GitHub's description.
    pub summary: Option<String>,
    /// Other names it answers to (a catalog entry's aliases).
    pub aliases: Vec<String>,
    pub folders: Vec<String>,
    pub popularity: Option<Popularity>,
    pub updated_at: Option<Timestamp>,
    pub basis: CompatBasis,
}

/// A hit with its tier, after merging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub hit: Hit,
    pub tier: u8,
}

/// `s` lowercased, with its whitespace collapsed to single spaces.
pub fn normalize(s: &str) -> String {
    s.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The best tier `hit` reaches for `query` (normalized), or `None`.
pub fn tier(query: &str, hit: &Hit) -> Option<u8> {
    if query.is_empty() {
        return None;
    }
    let name = normalize(&hit.name);
    let aliases: Vec<String> = hit.aliases.iter().map(|a| normalize(a)).collect();
    let exact_source = matches!(
        hit.source,
        HitSource::Catalog | HitSource::Wowi | HitSource::Tukui
    );
    if exact_source && (name == query || aliases.iter().any(|a| a == query)) {
        return Some(0);
    }
    if name.starts_with(query) || aliases.iter().any(|a| a.starts_with(query)) {
        return Some(1);
    }
    let words: Vec<&str> = query.split(' ').collect();
    let mut near: Vec<String> = vec![name];
    near.extend(aliases);
    near.extend(hit.folders.iter().map(|f| normalize(f)));
    let near = near.join(" ");
    if words.iter().all(|w| near.contains(w)) {
        return Some(2);
    }
    let far = [
        hit.author.as_deref().unwrap_or_default(),
        hit.summary.as_deref().unwrap_or_default(),
    ]
    .map(normalize)
    .join(" ");
    let all = format!("{near} {far}");
    words.iter().all(|w| all.contains(w)).then_some(3)
}

/// Whether `hit` is `entry`'s duplicate: the same ref; or the entry's name or one of its
/// aliases, compared [`compact`]ly ("Deadly Boss Mods" is `deadlybossmods`), with at
/// least one of the entry's folders (a namesake with none stays apart).
fn duplicates(entry: &Hit, hit: &Hit) -> bool {
    if hit.r#ref.identity() == entry.r#ref.identity() {
        return true;
    }
    let name = compact(&hit.name);
    let named = !name.is_empty()
        && std::iter::once(&entry.name)
            .chain(&entry.aliases)
            .any(|n| compact(n) == name);
    let folders: Vec<String> = entry.folders.iter().map(|f| f.to_lowercase()).collect();
    named
        && hit
            .folders
            .iter()
            .any(|f| folders.contains(&f.to_lowercase()))
}

/// Lowercase ASCII letters and digits only: spacing, case and punctuation aside.
fn compact(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// `hits` ranked for `query`: merged into the catalog entries they duplicate, dropped
/// when they reach no tier, then sorted (see the module docs).
pub fn rank(query: &str, hits: Vec<Hit>) -> Vec<Ranked> {
    let query = normalize(query);
    let (mut catalog, others): (Vec<Hit>, Vec<Hit>) = hits
        .into_iter()
        .partition(|h| h.source == HitSource::Catalog);
    let mut kept: Vec<(Hit, Option<u8>)> = Vec::new();
    let mut merged_tiers: Vec<Option<u8>> = vec![None; catalog.len()];
    for hit in others {
        match catalog.iter().position(|entry| duplicates(entry, &hit)) {
            Some(i) => {
                let entry = &mut catalog[i];
                // The best popularity and the newest date of its duplicates.
                if hit.popularity.map(Popularity::count) > entry.popularity.map(Popularity::count) {
                    entry.popularity = hit.popularity;
                }
                if hit.updated_at > entry.updated_at {
                    entry.updated_at = hit.updated_at;
                }
                let t = tier(&query, &hit);
                merged_tiers[i] = best(merged_tiers[i], t);
            }
            None => {
                let t = tier(&query, &hit);
                kept.push((hit, t));
            }
        }
    }
    let mut ranked: Vec<Ranked> = catalog
        .into_iter()
        .zip(merged_tiers)
        .map(|(hit, merged)| {
            let t = best(tier(&query, &hit), merged);
            (hit, t)
        })
        .chain(kept)
        .filter_map(|(hit, tier)| Some(Ranked { hit, tier: tier? }))
        .collect();
    // Per source and unit: a catalog hit may carry WoWInterface downloads or GitHub
    // stars, which never measure against each other.
    let stars = |p: Popularity| matches!(p, Popularity::Stars(_));
    let max: BTreeMap<(HitSource, bool), u64> =
        ranked.iter().fold(BTreeMap::new(), |mut max, r| {
            if let Some(p) = r.hit.popularity {
                let m = max.entry((r.hit.source, stars(p))).or_insert(0);
                *m = (*m).max(p.count());
            }
            max
        });
    // Per-source popularity in [0, 1], in thousandths so the sort is total.
    let score = |r: &Ranked| -> u64 {
        let Some(p) = r.hit.popularity else {
            return 0;
        };
        let Some(&m) = max.get(&(r.hit.source, stars(p))) else {
            return 0;
        };
        if m == 0 {
            return 0;
        }
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a ratio in [0, 1] scaled to thousandths"
        )]
        let scaled = ((p.count() as f64).ln_1p() / (m as f64).ln_1p() * 1000.0).round() as u64;
        scaled
    };
    ranked.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| {
                (b.hit.source == HitSource::Catalog).cmp(&(a.hit.source == HitSource::Catalog))
            })
            .then_with(|| score(b).cmp(&score(a)))
            .then_with(|| b.hit.updated_at.cmp(&a.hit.updated_at))
            .then_with(|| normalize(&a.hit.name).cmp(&normalize(&b.hit.name)))
            .then_with(|| a.hit.r#ref.to_string().cmp(&b.hit.r#ref.to_string()))
    });
    ranked
}

/// The lower (better) of two tiers.
fn best(a: Option<u8>, b: Option<u8>) -> Option<u8> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The hint for a client of `family`, at `now`.
pub fn compat(basis: &CompatBasis, family: Family, now: Timestamp) -> Compat {
    match basis {
        CompatBasis::Catalog(families) if families.is_empty() => Compat::Unknown,
        CompatBasis::Catalog(families) if families.contains(&family) => Compat::Yes,
        CompatBasis::Catalog(_) => Compat::No,
        CompatBasis::Tukui(families) if families.is_empty() => Compat::Unknown,
        CompatBasis::Tukui(families) if families.contains(&family) => Compat::Likely,
        CompatBasis::Tukui(_) => Compat::Unlikely,
        CompatBasis::Wowi(families, date) => {
            let max_age = jiff::SignedDuration::try_from(DEPENDENCY_SOURCE_MAX_AGE)
                .unwrap_or(jiff::SignedDuration::MAX);
            if date.is_some_and(|d| d < now - max_age) {
                Compat::Abandoned
            } else if families.contains(&family) {
                Compat::Likely
            } else {
                // WoWInterface's declared versions lag behind: never `no`.
                Compat::Unknown
            }
        }
        CompatBasis::Github => Compat::Unknown,
    }
}

/// `s` cut to `SEARCH_SUMMARY_MAX_CHARS` characters, with `…` when cut.
pub fn cap_summary(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= SEARCH_SUMMARY_MAX_CHARS {
        return s.to_owned();
    }
    let kept: String = s.chars().take(SEARCH_SUMMARY_MAX_CHARS - 1).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, r: &str, source: HitSource) -> Hit {
        Hit {
            name: name.to_owned(),
            r#ref: r.parse().unwrap(),
            source,
            author: None,
            summary: None,
            aliases: Vec::new(),
            folders: Vec::new(),
            popularity: None,
            updated_at: None,
            basis: CompatBasis::Github,
        }
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn names(ranked: &[Ranked]) -> Vec<(&str, u8)> {
        ranked
            .iter()
            .map(|r| (r.hit.name.as_str(), r.tier))
            .collect()
    }

    #[test]
    fn tiers_go_from_exact_to_prefix_to_words_to_author_and_description() {
        let q = |s: &str| normalize(s);
        let mut details = hit("Details! Damage Meter", "wowi:23056", HitSource::Wowi);
        details.folders = vec!["Details".into()];
        assert_eq!(tier(&q("details! damage meter"), &details), Some(0));
        assert_eq!(tier(&q("Details!"), &details), Some(1));
        assert_eq!(tier(&q("damage  METER"), &details), Some(2));
        let mut skada = hit("Skada", "gh:o/skada", HitSource::Github);
        skada.summary = Some("A modular damage meter".into());
        assert_eq!(tier(&q("damage meter"), &skada), Some(3));
        assert_eq!(tier(&q("healing"), &skada), None);
        // GitHub never reaches tier 0; a catalog alias does.
        assert_eq!(tier(&q("skada"), &skada), Some(1));
        let mut wa = hit("WeakAuras", "gh:WeakAuras/WeakAuras2", HitSource::Catalog);
        wa.aliases = vec!["wa".into()];
        assert_eq!(tier(&q("WA"), &wa), Some(0));
    }

    #[test]
    fn the_catalog_comes_first_then_popularity_per_source_then_recency_then_name() {
        let mut big_wowi = hit("Meter A", "wowi:1", HitSource::Wowi);
        big_wowi.popularity = Some(Popularity::Downloads(2_000_000));
        let mut small_wowi = hit("Meter B", "wowi:2", HitSource::Wowi);
        small_wowi.popularity = Some(Popularity::Downloads(10));
        // The best GitHub repo scores as high as the best WoWI project.
        let mut gh = hit("Meter C", "gh:o/c", HitSource::Github);
        gh.popularity = Some(Popularity::Stars(400));
        gh.updated_at = Some(at("2026-10-01T00:00:00Z"));
        big_wowi.updated_at = Some(at("2020-01-01T00:00:00Z"));
        let catalog = hit("Meter Z", "gh:o/z", HitSource::Catalog);
        let ranked = rank("meter", vec![small_wowi, gh, big_wowi, catalog]);
        assert_eq!(
            names(&ranked),
            [
                ("Meter Z", 1),
                ("Meter C", 1),
                ("Meter A", 1),
                ("Meter B", 1)
            ],
            "catalog, then 1.0 popularity by recency (C newer than A), then B"
        );
        // Stars and downloads never measure against each other, even in one source:
        // each catalog entry tops its unit, so recency decides.
        let mut downloads = hit("Meter X", "wowi:3", HitSource::Catalog);
        downloads.popularity = Some(Popularity::Downloads(2_000_000));
        downloads.updated_at = Some(at("2020-01-01T00:00:00Z"));
        let mut stars = hit("Meter Y", "gh:o/y", HitSource::Catalog);
        stars.popularity = Some(Popularity::Stars(3_000));
        stars.updated_at = Some(at("2026-10-01T00:00:00Z"));
        let ranked = rank("meter", vec![downloads, stars]);
        assert_eq!(names(&ranked), [("Meter Y", 1), ("Meter X", 1)]);
        // Ties fall back to name, then ref.
        let ranked = rank(
            "x",
            vec![
                hit("Xb", "gh:o/b", HitSource::Github),
                hit("Xa", "gh:o/z", HitSource::Github),
                hit("Xa", "gh:o/a", HitSource::Github),
            ],
        );
        let refs: Vec<String> = ranked.iter().map(|r| r.hit.r#ref.to_string()).collect();
        assert_eq!(refs, ["gh:o/a", "gh:o/z", "gh:o/b"]);
    }

    #[test]
    fn duplicates_merge_into_the_catalog_entry() {
        let mut entry = hit("WeakAuras", "gh:WeakAuras/WeakAuras2", HitSource::Catalog);
        entry.folders = vec!["WeakAuras".into(), "WeakAurasOptions".into()];
        entry.basis = CompatBasis::Catalog(vec![Family::Mainline]);
        // The same addon on WoWInterface: same name, every catalog folder.
        let mut wowi = hit("WeakAuras", "wowi:24910", HitSource::Wowi);
        wowi.folders = vec![
            "WeakAuras".into(),
            "weakaurasoptions".into(),
            "WeakAurasArchive".into(),
        ];
        wowi.popularity = Some(Popularity::Downloads(9_000_000));
        wowi.updated_at = Some(at("2026-09-01T00:00:00Z"));
        // The same ref on GitHub.
        let mut gh = hit("WeakAuras2", "gh:weakauras/weakauras2", HitSource::Github);
        gh.popularity = Some(Popularity::Stars(1300));
        // A namesake with other folders stays apart.
        let mut other = hit("WeakAuras", "wowi:999", HitSource::Wowi);
        other.folders = vec!["SomethingElse".into()];
        let ranked = rank("weakauras", vec![wowi, gh, other, entry]);
        assert_eq!(names(&ranked), [("WeakAuras", 0), ("WeakAuras", 0)]);
        let first = &ranked[0].hit;
        assert_eq!(
            (
                first.source,
                first.r#ref.to_string(),
                first.popularity,
                first.updated_at
            ),
            (
                HitSource::Catalog,
                "gh:WeakAuras/WeakAuras2".to_owned(),
                Some(Popularity::Downloads(9_000_000)),
                Some(at("2026-09-01T00:00:00Z"))
            )
        );
        assert_eq!(first.basis, CompatBasis::Catalog(vec![Family::Mainline]));
        assert_eq!(ranked[1].hit.r#ref.to_string(), "wowi:999");
    }

    /// A project named like a catalog entry's alias, spacing and punctuation aside,
    /// merges when it shares one of the entry's folders, and only then.
    #[test]
    fn a_hit_named_like_an_alias_with_a_shared_folder_merges() {
        let dbm = || {
            let mut entry = hit(
                "DBM",
                "gh:DeadlyBossMods/DeadlyBossMods",
                HitSource::Catalog,
            );
            entry.aliases = vec!["deadlybossmods".into(), "dbm-core".into()];
            entry.folders = vec!["DBM-Core".into(), "DBM-GUI".into(), "DBM-Raids".into()];
            entry
        };
        let project = |name: &str, folders: &[&str]| {
            let mut wowi = hit(name, "wowi:8814", HitSource::Wowi);
            wowi.folders = folders.iter().map(|f| (*f).to_owned()).collect();
            wowi.popularity = Some(Popularity::Downloads(3_000_000));
            wowi
        };
        for name in ["Deadly Boss Mods", "Deadly-Boss Mods", "DEADLYBOSSMODS"] {
            let ranked = rank(
                "dbm",
                vec![dbm(), project(name, &["dbm-core", "DBM-StatusBarTimers"])],
            );
            assert_eq!(ranked.len(), 1, "{name}: merged");
            assert_eq!(
                ranked[0].hit.popularity,
                Some(Popularity::Downloads(3_000_000)),
                "{name}"
            );
        }
        // The same name and no shared folder: another addon, kept apart.
        let ranked = rank("dbm", vec![dbm(), project("DBM", &["DBMVoice"])]);
        assert_eq!(ranked.len(), 2);
        // A name that is no alias stays apart even with a shared folder.
        let ranked = rank("dbm", vec![dbm(), project("DBM Voice Pack", &["DBM-Core"])]);
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn compatibility_follows_each_sources_rule() {
        let now = at("2026-10-16T00:00:00Z");
        let (m, v) = (Family::Mainline, Family::Vanilla);
        assert_eq!(compat(&CompatBasis::Catalog(vec![m]), m, now), Compat::Yes);
        assert_eq!(compat(&CompatBasis::Catalog(vec![m]), v, now), Compat::No);
        assert_eq!(
            compat(&CompatBasis::Catalog(vec![]), v, now),
            Compat::Unknown
        );
        assert_eq!(compat(&CompatBasis::Tukui(vec![m]), m, now), Compat::Likely);
        assert_eq!(
            compat(&CompatBasis::Tukui(vec![m]), v, now),
            Compat::Unlikely
        );
        assert_eq!(compat(&CompatBasis::Tukui(vec![]), v, now), Compat::Unknown);
        let recent = Some(at("2026-01-01T00:00:00Z"));
        assert_eq!(
            compat(&CompatBasis::Wowi(vec![m], recent), m, now),
            Compat::Likely
        );
        assert_eq!(
            compat(&CompatBasis::Wowi(vec![m], recent), v, now),
            Compat::Unknown
        );
        let old = Some(at("2020-06-01T00:00:00Z"));
        assert_eq!(
            compat(&CompatBasis::Wowi(vec![m], old), m, now),
            Compat::Abandoned
        );
        assert_eq!(compat(&CompatBasis::Github, v, now), Compat::Unknown);
        assert!(!Compat::Unknown.suggestible() && !Compat::Abandoned.suggestible());
        assert!(Compat::Yes.suggestible() && Compat::Likely.suggestible());
        assert!(!Compat::No.suggestible() && !Compat::Unlikely.suggestible());
    }

    #[test]
    fn summaries_are_capped() {
        assert_eq!(cap_summary("  short  "), "short");
        let long = "x".repeat(500);
        let capped = cap_summary(&long);
        assert_eq!(capped.chars().count(), SEARCH_SUMMARY_MAX_CHARS);
        assert!(capped.ends_with('…'));
    }
}
