//! `omaddons search <query…> [-t T] [--source …]… [--limit N] [--refresh]` (alias
//! `find`): addons by name across the catalog, the WoWInterface index, the Tukui list and
//! GitHub, ranked (`resolve::search`), each with a compatibility hint per selected client
//! and an install command for the best few. Only GitHub receives the query (M2-D21): the
//! catalog is built in, and the WoWInterface index and Tukui list are whole lists,
//! fetched and searched here. Compatibility is a hint; `install` verifies every TOC.

use std::collections::BTreeMap;
use std::io::Write;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::engine::Engine;
use super::world::World;
use crate::cli::Context;
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::Ref;
use crate::output::{Outcome, Phase, Style};
use crate::politeness::SOURCE_RECHECK_MIN_INTERVAL;
use crate::resolve::cache::Freshness;
use crate::resolve::catalog::Catalog;
use crate::resolve::search::{
    Compat, CompatBasis, Hit, HitSource, Popularity, cap_summary, compat, rank,
};
use crate::rules::{ClientVersion, Family, InstallKind, RulesTable};
use crate::sources::github_search::GithubSearch;
use crate::sources::wowi::index::{IndexPolicy, WowiIndex};
use crate::sources::{SourceId, SourceSkipReason, SourceSkipped};
use crate::state::memo::TukuiList;

/// The results listed when `--limit` is not given.
const DEFAULT_LIMIT: usize = 20;
/// The install commands suggested at most.
const INSTALL_HINTS: usize = 3;

/// Which source to consult (`--source`, repeatable; all by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum SourceFilter {
    Catalog,
    Wowi,
    Tukui,
    Github,
}

/// `search`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SearchArgs {
    /// What to look for: words of the addon's name, folders, author or description
    #[arg(required = true, value_name = "QUERY")]
    pub query: Vec<String>,
    /// Consult only this source (repeatable): catalog, wowi, tukui or github
    #[arg(long = "source", value_enum, value_name = "SOURCE")]
    pub sources: Vec<SourceFilter>,
    /// Most results to list (default 20, at most 50)
    #[arg(long)]
    pub limit: Option<usize>,
    /// Fetch the WoWInterface index and the Tukui list now, even inside their windows
    #[arg(long)]
    pub refresh: bool,
}

/// `search`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchData {
    pub query: String,
    /// Best first.
    pub results: Vec<SearchResult>,
    /// Sources not consulted, and why (never those `--source` left out).
    pub sources_skipped: Vec<SourceSkipped>,
}

/// One addon found.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchResult {
    pub name: String,
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub source: HitSource,
    pub author: Option<String>,
    /// The catalog's notes or GitHub's description; `null` for WoWInterface and Tukui.
    pub summary: Option<String>,
    /// `{downloads}` (WoWInterface) or `{stars}` (GitHub); `null` otherwise.
    pub popularity: Option<Popularity>,
    #[schemars(with = "Option<String>")]
    pub updated_at: Option<Timestamp>,
    /// A hint per selected client, by target id; `install` verifies.
    pub compat: BTreeMap<String, Compat>,
}

/// `search`'s outcome.
#[derive(Debug)]
pub struct SearchOutcome {
    data: SearchData,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    partial: bool,
    /// For `-v` log lines.
    pub notes: Vec<String>,
}

impl Outcome for SearchOutcome {
    type Data = SearchData;

    fn command(&self) -> &'static str {
        "search"
    }

    fn data(&self) -> &SearchData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn partial(&self) -> bool {
        self.partial
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        if d.results.is_empty() {
            writeln!(w, "No addon matches \"{}\".", d.query)?;
        }
        let width = |f: fn(&SearchResult) -> String| {
            d.results
                .iter()
                .map(|r| f(r).chars().count())
                .max()
                .unwrap_or(0)
        };
        let (name_w, ref_w) = (width(|r| r.name.clone()), width(|r| r.r#ref.to_string()));
        for r in &d.results {
            let mut facts = vec![source_label(r.source).to_owned()];
            facts.extend(r.popularity.map(popularity_text));
            facts.extend(
                r.updated_at
                    .map(|t| format!("updated {}", t.strftime("%Y"))),
            );
            let compat: Vec<String> = r
                .compat
                .iter()
                .map(|(id, c)| format!("{id}: {}", compat_text(*c)))
                .collect();
            if !compat.is_empty() {
                facts.push(compat.join(", "));
            }
            writeln!(
                w,
                "  {:<name_w$}  {:<ref_w$}  {}",
                r.name,
                r.r#ref.to_string(),
                facts.join(" · ")
            )?;
        }
        for s in &d.sources_skipped {
            let why = match s.reason {
                SourceSkipReason::Offline => "offline".to_owned(),
                SourceSkipReason::Disabled => "disabled".to_owned(),
                SourceSkipReason::NoIndex => "no index yet".to_owned(),
                SourceSkipReason::RateLimited => s.reset_at.map_or_else(
                    || "rate limited".to_owned(),
                    |t| format!("rate limited until {t}"),
                ),
            };
            writeln!(
                w,
                "{}",
                style.dim(&format!("{}: not searched ({why})", source_name(s.source)))
            )?;
        }
        match self.commands.first() {
            Some(hint) => writeln!(
                w,
                "Install: {}        (a hint: install verifies each addon's TOC)",
                hint.command()
            )?,
            None if !d.results.is_empty() => writeln!(
                w,
                "No install suggested: no result is known to load on a selected client."
            )?,
            None => {}
        }
        Ok(())
    }
}

fn source_label(source: HitSource) -> &'static str {
    match source {
        HitSource::Catalog => "catalog",
        HitSource::Wowi => "WoWInterface",
        HitSource::Tukui => "Tukui",
        HitSource::Github => "GitHub",
    }
}

fn source_name(source: SourceId) -> String {
    match source {
        SourceId::Github => "GitHub".to_owned(),
        SourceId::Wowi => "WoWInterface".to_owned(),
        SourceId::Tukui => "Tukui".to_owned(),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn popularity_text(p: Popularity) -> String {
    let short = |n: u64| -> String {
        match n {
            0..1_000 => n.to_string(),
            1_000..1_000_000 => format!("{}k", n / 1_000),
            _ => format!("{:.1}M", n as f64 / 1_000_000.0),
        }
    };
    match p {
        Popularity::Downloads(n) => format!("{} downloads", short(n)),
        Popularity::Stars(n) => format!("★{}", short(n)),
    }
}

fn compat_text(c: Compat) -> &'static str {
    match c {
        Compat::Yes => "yes",
        Compat::Likely => "likely",
        Compat::Unknown => "unknown",
        Compat::Unlikely => "unlikely",
        Compat::No => "no",
        Compat::Abandoned => "abandoned",
    }
}

/// The family a declared client version (`11.2.0`, `1.15.7`) belongs to.
fn family_of(table: &RulesTable, version: &str) -> Option<Family> {
    let version: ClientVersion = version.trim().parse().ok()?;
    Some(table.family_for(&version, InstallKind::Battlenet)).filter(|f| *f != Family::Unknown)
}

/// `usage` for a `--limit` outside 1..=`SEARCH_MAX_RESULTS`; the fix uses the bound.
fn check_limit(args: &SearchArgs) -> Result<usize> {
    use crate::limits::SEARCH_MAX_RESULTS;
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    if (1..=SEARCH_MAX_RESULTS).contains(&limit) {
        return Ok(limit);
    }
    let bound = if limit == 0 { 1 } else { SEARCH_MAX_RESULTS };
    let mut argv = vec!["omaddons".to_owned(), "search".to_owned()];
    argv.extend(args.query.iter().cloned());
    argv.extend(["--limit".to_owned(), bound.to_string()]);
    Err(Error::new(
        ErrorCode::Usage,
        format!("--limit takes 1 to {SEARCH_MAX_RESULTS}, not {limit}"),
        Suggestion::command(argv),
    ))
}

/// Why a source could not be consulted, as `sources_skipped` says it.
fn skip_reason(error: &Error) -> SourceSkipReason {
    match error.code {
        ErrorCode::Offline => SourceSkipReason::Offline,
        ErrorCode::RateLimited => SourceSkipReason::RateLimited,
        _ => SourceSkipReason::NoIndex,
    }
}

/// When a rate limit lifts, from its error's details.
fn reset_at(error: &Error) -> Option<Timestamp> {
    error.details["reset_at"]
        .as_str()
        .and_then(|t| t.parse().ok())
}

/// Runs `search`.
pub fn run(ctx: &Context, args: &SearchArgs) -> Result<SearchOutcome> {
    let limit = check_limit(args)?;
    let query = args.query.join(" ");
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    let mut notes: Vec<String> = Vec::new();
    let mut partial = false;
    let mut targets = world.select_many(ctx)?;
    targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    let engine = Engine::new(ctx, &world, "search")?;
    let enabled = engine.enabled_sources();
    let wants = |s: SourceFilter| args.sources.is_empty() || args.sources.contains(&s);
    let freshness = if ctx.offline {
        Freshness::OfflineOnly
    } else if args.refresh {
        Freshness::Refresh
    } else {
        Freshness::Window(SOURCE_RECHECK_MIN_INTERVAL)
    };
    let cache = engine.cache(freshness);
    let mut sources_skipped: Vec<SourceSkipped> = Vec::new();
    let mut skip = |source, reason, reset_at| {
        sources_skipped.push(SourceSkipped {
            source,
            reason,
            reset_at,
        });
    };
    let consulted: Vec<(SourceFilter, SourceId)> = [
        (SourceFilter::Wowi, SourceId::Wowi),
        (SourceFilter::Tukui, SourceId::Tukui),
        (SourceFilter::Github, SourceId::Github),
    ]
    .into_iter()
    .filter(|(f, _)| wants(*f))
    .collect();
    for (_, id) in consulted.iter().filter(|(_, id)| !enabled.contains(id)) {
        skip(*id, SourceSkipReason::Disabled, None);
    }
    let fetched = consulted
        .iter()
        .filter(|(_, id)| enabled.contains(id))
        .count() as u64;
    let progress = |what: &str| ctx.progress.event(Phase::Fetch, fetched, what);
    let mut hits: Vec<Hit> = Vec::new();

    // The catalog: offline, built in (with catalog.d).
    if wants(SourceFilter::Catalog) {
        let (catalog, loaded) = Catalog::load(&world.paths);
        warnings.extend(loaded);
        hits.extend(catalog.entries().iter().map(|e| Hit {
            name: e.name.clone(),
            r#ref: e.r#ref.clone(),
            source: HitSource::Catalog,
            author: None,
            summary: e.notes.as_deref().map(cap_summary),
            aliases: e.aliases.clone(),
            folders: e.folders.iter().map(|f| f.as_str().to_owned()).collect(),
            popularity: None,
            updated_at: None,
            basis: CompatBasis::Catalog(e.families.clone()),
        }));
    }

    // The WoWInterface index: the whole file list, searched here.
    if wants(SourceFilter::Wowi) && enabled.contains(&SourceId::Wowi) {
        match WowiIndex::load_or_refresh(&world.paths, engine.http(), IndexPolicy::from(freshness))
        {
            Ok(load) => {
                warnings.extend(load.warnings.iter().cloned());
                notes.extend(load.diagnostics.iter().cloned());
                if let Some(s) = load.skipped() {
                    skip(s.source, s.reason, s.reset_at);
                }
                if let Some(index) = &load.index {
                    hits.extend(index.entries().iter().map(|e| {
                        let families = e
                            .compat
                            .iter()
                            .filter_map(|c| family_of(&world.table, &c.version))
                            .collect();
                        let date = i64::try_from(e.date_ms)
                            .ok()
                            .and_then(|ms| Timestamp::from_millisecond(ms).ok());
                        Hit {
                            name: e.name.clone(),
                            r#ref: Ref::Wowi { id: e.uid },
                            source: HitSource::Wowi,
                            author: Some(e.author.clone()).filter(|a| !a.is_empty()),
                            summary: None,
                            aliases: Vec::new(),
                            folders: e.dirs.iter().map(|f| f.as_str().to_owned()).collect(),
                            popularity: Some(Popularity::Downloads(e.downloads)),
                            updated_at: date,
                            basis: CompatBasis::Wowi(families, date),
                        }
                    }));
                }
            }
            Err(error) => {
                notes.push(format!("the WoWInterface index: {}", error.message));
                skip(SourceId::Wowi, skip_reason(&error), reset_at(&error));
            }
        }
        progress("the WoWInterface index");
    }

    // The Tukui list: from the memo inside its window.
    if wants(SourceFilter::Tukui) && enabled.contains(&SourceId::Tukui) {
        match cache.tukui_list() {
            Ok(list) => hits.extend(tukui_hits(&list, &world.table)),
            Err(error) => {
                let reason = skip_reason(&error);
                if matches!(reason, SourceSkipReason::NoIndex) {
                    // A failure, not a state: said, and the run is partial.
                    partial = true;
                    let mut warning = Warning::item(&error);
                    warning.message = format!("the Tukui list: {}", error.message);
                    warnings.push(warning);
                    skip(SourceId::Tukui, SourceSkipReason::NoIndex, None);
                } else {
                    skip(SourceId::Tukui, reason, reset_at(&error));
                }
            }
        }
        progress("the Tukui list");
    }

    // GitHub: the only request that carries the query.
    if wants(SourceFilter::Github) && enabled.contains(&SourceId::Github) {
        if ctx.offline {
            skip(SourceId::Github, SourceSkipReason::Offline, None);
        } else {
            let search =
                GithubSearch::with_github_token(std::sync::Arc::clone(&world.github_token))
                    .offline(ctx.offline);
            match search.search(&query, engine.http()) {
                Ok(found) => {
                    if found.dropped > 0 {
                        notes.push(format!(
                            "GitHub: {} repositories without a WoW marker dropped",
                            found.dropped
                        ));
                    }
                    hits.extend(found.repos.into_iter().map(|r| Hit {
                        name: r.name,
                        r#ref: r.r#ref,
                        source: HitSource::Github,
                        author: Some(r.owner),
                        summary: r.description.as_deref().map(cap_summary),
                        aliases: Vec::new(),
                        folders: Vec::new(),
                        popularity: Some(Popularity::Stars(r.stars)),
                        updated_at: r.pushed_at,
                        basis: CompatBasis::Github,
                    }));
                }
                Err(error) => match error.code {
                    // Not a failure: said, with when it lifts.
                    ErrorCode::RateLimited | ErrorCode::Offline => {
                        skip(SourceId::Github, skip_reason(&error), reset_at(&error));
                    }
                    _ => {
                        partial = true;
                        let mut warning = Warning::item(&error);
                        warning.message = format!("GitHub's search: {}", error.message);
                        warnings.push(warning);
                        skip(SourceId::Github, SourceSkipReason::NoIndex, None);
                    }
                },
            }
        }
        progress("GitHub's search");
    }

    let now = Timestamp::now();
    let results: Vec<SearchResult> = rank(&query, hits)
        .into_iter()
        .take(limit)
        .map(|r| {
            let hit = r.hit;
            let compat = targets
                .iter()
                .map(|d| {
                    (
                        d.target.id.clone(),
                        compat(&hit.basis, d.target.family, now),
                    )
                })
                .collect();
            SearchResult {
                name: hit.name,
                r#ref: hit.r#ref,
                source: hit.source,
                author: hit.author,
                summary: hit.summary,
                popularity: hit.popularity,
                updated_at: hit.updated_at,
                compat,
            }
        })
        .collect();
    let commands = install_hints(&results);
    sources_skipped.sort_by_key(|s| s.source);
    // One memo write per command: the Tukui list it fetched, for the next run.
    let remembered = cache.remember(&[]);
    warnings.extend(remembered.warnings);
    notes.extend(remembered.notes);
    Ok(SearchOutcome {
        data: SearchData {
            query,
            results,
            sources_skipped,
        },
        warnings,
        commands,
        partial,
        notes,
    })
}

/// The Tukui projects as hits.
fn tukui_hits<'a>(list: &'a TukuiList, table: &'a RulesTable) -> impl Iterator<Item = Hit> + 'a {
    list.entries.iter().filter_map(move |e| {
        let r#ref: Ref = format!("tukui:{}", e.slug).parse().ok()?;
        let families = e.patch.iter().filter_map(|p| family_of(table, p)).collect();
        // A memo from before this build kept the text as sent: made safe here too.
        let safe = crate::sources::tukui::safe;
        Some(Hit {
            name: safe(&e.name),
            r#ref,
            source: HitSource::Tukui,
            author: Some(safe(&e.author)).filter(|a| !a.is_empty()),
            summary: None,
            aliases: Vec::new(),
            folders: e.directories.clone(),
            popularity: None,
            updated_at: parse_time(&e.last_update),
            basis: CompatBasis::Tukui(families),
        })
    })
}

/// Tukui's `last_update`: RFC 3339, or a civil date and time taken as UTC.
fn parse_time(s: &str) -> Option<Timestamp> {
    s.parse::<Timestamp>().ok().or_else(|| {
        s.parse::<jiff::civil::DateTime>()
            .ok()
            .or_else(|| {
                s.parse::<jiff::civil::Date>()
                    .ok()
                    .map(|d| d.to_datetime(jiff::civil::Time::midnight()))
            })
            .and_then(|dt| dt.to_zoned(jiff::tz::TimeZone::UTC).ok())
            .map(|z| z.timestamp())
    })
}

/// `install <ref> -t <target>` for up to three results, best first, each on the first
/// client (by id) with its best suggestible hint (`yes`, then `likely`, then `unknown`).
fn install_hints(results: &[SearchResult]) -> Vec<CommandHint> {
    results
        .iter()
        .filter_map(|r| {
            let (target, _) = r
                .compat
                .iter()
                .filter(|(_, c)| c.suggestible())
                .min_by_key(|(id, c)| (**c, (*id).clone()))?;
            Some(CommandHint::new(
                "install",
                [
                    "omaddons".to_owned(),
                    "install".to_owned(),
                    r.r#ref.to_string(),
                    "-t".to_owned(),
                    target.clone(),
                ],
                false,
            ))
        })
        .take(INSTALL_HINTS)
        .collect()
}
