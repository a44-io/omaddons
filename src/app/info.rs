//! `omaddons info <ref|addon> [-t T]`: what an addon is and what installing it would
//! get. The installed records across targets, the catalog entry, and the newest stable
//! release with how likely it is to load on each target (what plans proved, its
//! metadata, the catalog's families: nothing is downloaded or verified; `install`
//! verifies). `--changelog` adds the release notes of
//! every release newer than the oldest installed one (`sources::changelog`).
//!
//! Release metadata comes through the release memo: a check from the last 15 minutes is
//! reused (`--refresh` fetches anyway). `--offline` answers from the memo whatever its
//! age, with `stale_check`, or with `latest: null` when nothing is remembered; it never
//! makes a request. A failed fetch for an addon known locally is a partial success; for
//! one known nowhere, the command's error.

use std::io::Write;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;

use super::drift::DriftSummary;
use super::history::TargetRef;
use super::resolve_args::{aimed_at, for_family, parse_ref};
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::drift::{DriftMode, DriftState};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::limits::{ABOUT_MEMO_MAX_ENTRIES, INFO_RELEASES_MAX, TOC_TEXT_MAX_CHARS};
use crate::model::{
    AddonReason, AddonState, Channel, FolderName, LeftOutFolder, Ref, Release, ReleaseSet, Target,
    TxId, VersionConfidence,
};
use crate::net::{Http, NetConfig, NetLog, stack};
use crate::output::{Outcome, Style};
use crate::politeness::{ABOUT_RECHECK_INTERVAL, SOURCE_RECHECK_MIN_INTERVAL};
use crate::resolve::cache::{Freshness, ReleaseCache};
use crate::resolve::search::Popularity;
use crate::resolve::select::{newest, select};
use crate::resolve::updates::{Confidence, rank_confidence};
use crate::rules::Family;
use crate::sources::about::{self, Project};
use crate::sources::changelog::{
    Changelog, ChangelogUnavailable, Installed, changelog_for, human_lines, oldest_since,
};
use crate::sources::git_hosts::GitRunner;
use crate::sources::{SourceId, SourceRegistry, Wanted};
use crate::state::about_memo::{self, AboutEntry};
use crate::state::memo::FactIndex;
use crate::text::{Entities, source_line};

/// `info`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct InfoArgs {
    /// The addon: a catalog name, an installed addon's name, or a ref or link
    #[arg(value_name = "REF")]
    pub query: String,
    /// Fetch the newest release and the about again, even if checked recently
    #[arg(long)]
    pub refresh: bool,
    /// Add the notes of every release newer than the oldest installed
    #[arg(long)]
    pub changelog: bool,
    /// Add the recent releases, with how likely each loads on each client
    #[arg(long)]
    pub releases: bool,
    /// How many releases --releases lists (default 10, at most 20)
    #[arg(long, value_name = "N", requires = "releases")]
    pub limit: Option<usize>,
}

/// The releases `--releases` lists when `--limit` is not given.
const DEFAULT_RELEASES: usize = 10;

/// `info`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InfoData {
    /// The argument exactly as given.
    pub query: String,
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub name: Option<String>,
    pub catalog: Option<CatalogInfo>,
    pub installed: Vec<InstalledInfo>,
    pub latest: Option<Latest>,
    /// What the addon is, as its source describes it, with its project page; `null` when
    /// the ref names no project on GitHub, WoWInterface, Tukui or WowAce.
    pub about: Option<About>,
    /// Why `about` is `null`, or holds only what the ref gives (`homepage`); `null` when
    /// the source answered (now, or within `ABOUT_RECHECK_INTERVAL`).
    pub about_unavailable: Option<AboutUnavailable>,
    pub offline: bool,
    /// `--changelog` only: the notes of the releases newer than the oldest installed one
    /// (the latest's alone when none records a release), or `null`. Omitted without
    /// `--changelog`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<Option<Changelog>>,
    /// Why `changelog` is `null`; omitted when `changelog` is absent or holds notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_unavailable: Option<ChangelogUnavailable>,
    /// `--releases` only: the source's recent releases, newest first, at most `--limit`,
    /// from one listing request; `null` when it lists none (`releases_unavailable` says
    /// why). Omitted without `--releases`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub releases: Option<Option<Vec<ListedRelease>>>,
    /// Why `releases` is `null`; omitted when `releases` is absent or lists releases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub releases_unavailable: Option<ReleasesUnavailable>,
}

/// One release `--releases` lists: what `install --version <version>` installs.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ListedRelease {
    pub version: String,
    /// When its source published it; `null` when the listing gives no date (WowAce).
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub published_at: Option<jiff::Timestamp>,
    /// `beta` for a GitHub prerelease; WowAce lists its stable tags only.
    pub channel: Channel,
    /// The release id a plan of `install --version <version>` names.
    pub release_id: String,
    /// Its archives by file name, as the listing names them (WowAce: the tree omaddons
    /// packs).
    pub assets: Vec<String>,
    /// For each selected client, how likely it is to load: `latest.compat`'s judgment
    /// (the newest stable release's own, when it is listed), from what plans proved,
    /// the archives' names and the catalog's families. No release metadata is read, so
    /// an older release is never `declared`. A hint only: `install` verifies every TOC.
    pub compat: std::collections::BTreeMap<String, LoadHint>,
}

/// Why `--releases` lists nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReleasesUnavailable {
    pub reason: ReleasesUnavailableReason,
}

/// What kept a source from listing its releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReleasesUnavailableReason {
    /// The source serves only its latest release (WoWInterface, Tukui, `url:`, `git:`,
    /// a local archive or folder), or the ref names one fixed build (a WowAce revision).
    SourceListsLatestOnly,
    /// The source is turned off in config.toml, or not in this build (CurseForge, Wago).
    SourceDisabled,
    /// `--offline`: no listing is ever kept, so nothing is asked.
    Offline,
    /// The source knows no such project.
    NotFound,
    /// GitHub's REST budget is spent.
    RateLimited,
    /// The request failed.
    FetchFailed,
}

impl ReleasesUnavailableReason {
    fn of(error: &Error) -> Self {
        match error.code {
            ErrorCode::Offline => Self::Offline,
            ErrorCode::RateLimited => Self::RateLimited,
            ErrorCode::NotFound => Self::NotFound,
            ErrorCode::SourceDisabled => Self::SourceDisabled,
            _ => Self::FetchFailed,
        }
    }

    /// The human view's words.
    fn words(self) -> &'static str {
        match self {
            Self::SourceListsLatestOnly => "its source serves only its latest release",
            Self::SourceDisabled => "not listed (its source is disabled)",
            Self::Offline => "not listed (offline)",
            Self::NotFound => "its source knows no such project",
            Self::RateLimited => "not listed (GitHub's API budget is spent)",
            Self::FetchFailed => "its source did not answer",
        }
    }
}

/// What an addon is, as its source describes it (`sources::about`), made safe to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct About {
    /// One line about it: GitHub's description, Tukui's `small_desc`. WoWInterface's
    /// description is a page, not a line, so it gives none.
    pub summary: Option<String>,
    pub author: Option<String>,
    /// The project's page, built from the ref alone (never from a TOC's `X-Website`), on
    /// `github.com`, `www.wowinterface.com`, `tukui.org` or `www.wowace.com`.
    pub homepage: Option<String>,
    /// When it last changed, as its source says: GitHub's last push, WoWInterface's
    /// file date, Tukui's last update.
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub updated_at: Option<jiff::Timestamp>,
    /// WoWInterface's downloads or GitHub's stars.
    pub popularity: Option<Popularity>,
}

/// Why `about` is missing, or holds only its `homepage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AboutUnavailable {
    pub reason: AboutUnavailableReason,
}

/// What kept a source's description from `about`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AboutUnavailableReason {
    /// The ref names no project page: `url:`, `file:`, `git:` off GitHub, CurseForge or
    /// Wago (`about` is `null`).
    SourceHasNoMetadata,
    /// The source describes no project (WowAce): the page alone.
    NotSupported,
    /// The source is turned off in config.toml.
    SourceDisabled,
    /// `--offline`, and nothing is remembered.
    Offline,
    /// The source knows no such project (a deleted or private repository).
    NotFound,
    /// GitHub's REST budget is spent.
    RateLimited,
    /// The request failed, and nothing is remembered.
    FetchFailed,
}

impl AboutUnavailableReason {
    fn of(error: &Error) -> Self {
        match error.code {
            ErrorCode::Offline => Self::Offline,
            ErrorCode::RateLimited => Self::RateLimited,
            ErrorCode::SourceDisabled => Self::SourceDisabled,
            _ => Self::FetchFailed,
        }
    }

    /// The human view's words; `None` when `about`'s absence says it already.
    fn words(self) -> Option<&'static str> {
        match self {
            Self::SourceHasNoMetadata | Self::NotSupported => None,
            Self::SourceDisabled => Some("not checked (its source is disabled)"),
            Self::Offline => Some("not checked (offline)"),
            Self::NotFound => Some("its source knows no such project"),
            Self::RateLimited => Some("not checked (GitHub's API budget is spent)"),
            Self::FetchFailed => Some("its source did not answer"),
        }
    }
}

/// The TOC an installed addon's own folder loads on its client, as the client reads it
/// (conditional lines evaluated), made safe to show: UI escapes, control characters and
/// characters that hide text removed, each string cut at `TOC_TEXT_MAX_CHARS` (then `…`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct InstalledToc {
    /// The folder read: the one named like the addon, else its first.
    pub folder: FolderName,
    /// `## Title`, as this client shows it, plain.
    pub title_plain: Option<String>,
    /// `## Notes`, plain.
    pub notes: Option<String>,
    pub author: Option<String>,
    pub version: Option<String>,
    pub interfaces: Vec<u32>,
    /// `X-Website` as the author wrote it: text to show, never a link omaddons
    /// follows or offers (`about.homepage` is the project's page).
    pub website: Option<String>,
    /// The folders it requires on this client.
    pub dependencies: Vec<String>,
    /// `## OptionalDeps` on this client.
    pub optional_deps: Vec<String>,
}

impl InstalledToc {
    /// `toc`, read from `folder`, made safe.
    fn of(folder: FolderName, toc: crate::inventory::TocSummary) -> Self {
        Self {
            folder,
            title_plain: toc_text(toc.title.as_deref()),
            notes: toc_text(toc.notes.as_deref()),
            author: toc_text(toc.author.as_deref()),
            version: toc_text(toc.version.as_deref()),
            interfaces: toc.interfaces,
            website: toc
                .website
                .as_deref()
                .map(|w| source_line(w, TOC_TEXT_MAX_CHARS, Entities::Keep))
                .filter(|w| !w.is_empty()),
            dependencies: toc.dependencies,
            optional_deps: toc.optional_deps,
        }
    }
}

/// One TOC field, written by the addon's author: its UI escapes (colour codes, textures,
/// atlases, links) removed, then made one safe line cut at `TOC_TEXT_MAX_CHARS` (then
/// `…`); `None` when nothing is left.
fn toc_text(raw: Option<&str>) -> Option<String> {
    raw.map(|r| {
        source_line(
            &crate::text::strip_ui_escapes(r),
            TOC_TEXT_MAX_CHARS,
            Entities::Keep,
        )
    })
    .filter(|s| !s.is_empty())
}

/// The TOC of `addon`'s own folder on `target`: the folder named like it (ignoring
/// case), else its first, as the engine picks it for an adopted version.
fn installed_toc(target: &Target, addon: &str, folders: &[FolderName]) -> Option<InstalledToc> {
    let folder = folders
        .iter()
        .find(|f| f.as_str().eq_ignore_ascii_case(addon))
        .or_else(|| folders.first())?;
    crate::inventory::folder_toc(target, folder).map(|toc| InstalledToc::of(folder.clone(), toc))
}

/// The catalog entry.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CatalogInfo {
    pub name: String,
    /// The variant shown, when the name has several (M3-D16). Omitted for an entry
    /// without a variant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    pub aliases: Vec<String>,
    pub families: Vec<Family>,
    pub folders: Vec<FolderName>,
    pub notes: Option<String>,
    /// The name's other variants, and the families each is for; omitted when none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<CatalogVariant>,
}

/// Another variant of a catalog name.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CatalogVariant {
    pub variant: String,
    pub families: Vec<Family>,
}

/// One installed record.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InstalledInfo {
    pub target: TargetRef,
    pub version: String,
    pub version_confidence: VersionConfidence,
    pub channel: Channel,
    pub pinned: bool,
    pub state: AddonState,
    /// `explicit` when a user asked for it, `dependency` when only the resolver did.
    pub reason: AddonReason,
    /// The addons that require it here, sorted.
    pub required_by: Vec<String>,
    /// A dependency nothing requires any more.
    pub orphan: bool,
    /// Installed under `--any-flavor` on this client.
    pub any_flavor: bool,
    pub folders: Vec<FolderName>,
    /// The release's folders this client does not load, left out of the install, with
    /// why (as `folders.skipped` showed them).
    pub left_out: Vec<LeftOutFolder>,
    /// Whether its files changed on disk (stat-first, local), with paths under `-v`.
    pub drift: DriftSummary,
    /// The TOC its own folder loads here; `null` when that folder is missing, is a
    /// symlink or cannot be read, or holds no TOC this client loads.
    pub toc: Option<InstalledToc>,
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub installed_at: jiff::Timestamp,
    pub tx: TxId,
    /// `-v` only: the record file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_path: Option<PathBuf>,
}

/// The newest stable release.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Latest {
    pub version: String,
    pub release_id: String,
    pub channel: Channel,
    /// release.json flavor names, when the release has them.
    pub flavors: Vec<String>,
    /// The selected targets it is `verified`, `declared` or `likely` to load on, sorted.
    pub targets: Vec<String>,
    /// For each selected target, how likely it is to load ([`judge`]). A hint only:
    /// `install` verifies every TOC.
    pub compat: std::collections::BTreeMap<String, LoadHint>,
    /// `-v` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_urls: Option<Vec<String>>,
}

/// How likely the newest release is to load on one client: `outdated`'s words, plus
/// `blocked` for what a plan proved does not load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoadHint {
    /// A plan verified a candidate loads here.
    Verified,
    /// Its release metadata names this client.
    Declared,
    /// Nothing says otherwise.
    Likely,
    /// The metadata names other clients only, or the catalog none of its families.
    Unlikely,
    /// A plan verified that no candidate loads here.
    Blocked,
}

impl LoadHint {
    fn likely(self) -> bool {
        matches!(self, Self::Verified | Self::Declared | Self::Likely)
    }
}

/// `set`'s newest stable release on `target`, by the first rule that says something:
/// what plans proved (`facts`); else what its best candidate's metadata says (it
/// describes this very release); else the catalog's `families`, when it lists any (a
/// `git:` or `url:` release carries no metadata); else likely.
pub(crate) fn judge(
    set: &ReleaseSet,
    target: &Target,
    facts: &FactIndex,
    families: &[Family],
) -> LoadHint {
    let candidates = select(set, target, Channel::Stable, None).candidates;
    let Some(best) = candidates.first() else {
        return LoadHint::Unlikely;
    };
    let verdicts: Vec<_> = candidates
        .iter()
        .map(|c| facts.verdict(&c.release_id, &c.asset.filename, target))
        .collect();
    if verdicts.iter().flatten().any(|v| v.loads) {
        return LoadHint::Verified;
    }
    if verdicts.iter().all(|v| v.is_some_and(|v| !v.loads)) {
        return LoadHint::Blocked;
    }
    match rank_confidence(best.rank) {
        Confidence::Declared => LoadHint::Declared,
        Confidence::Unlikely => LoadHint::Unlikely,
        Confidence::Verified | Confidence::Likely if families.is_empty() => LoadHint::Likely,
        Confidence::Verified | Confidence::Likely if families.contains(&target.family) => {
            LoadHint::Likely
        }
        Confidence::Verified | Confidence::Likely => LoadHint::Unlikely,
    }
}

/// `info`'s outcome.
#[derive(Debug)]
pub struct InfoOutcome {
    data: InfoData,
    warnings: Vec<Warning>,
    /// The fetch failed for an addon known locally.
    partial: bool,
    /// For `-v` log lines: the release memo ignored or not written.
    pub notes: Vec<String>,
}

impl Outcome for InfoOutcome {
    type Data = InfoData;

    fn command(&self) -> &'static str {
        "info"
    }

    fn data(&self) -> &InfoData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn partial(&self) -> bool {
        self.partial
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let name = d.name.as_deref().unwrap_or(&d.query);
        writeln!(w, "{name}  {}", d.r#ref)?;
        if let Some(about) = &d.about {
            if let Some(summary) = &about.summary {
                writeln!(w, "  {summary}")?;
            }
            let mut facts: Vec<String> = Vec::new();
            facts.extend(about.author.as_ref().map(|a| format!("by {a}")));
            facts.extend(about.popularity.map(super::search::popularity_text));
            facts.extend(about.updated_at.map(|t| {
                let day = t
                    .to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%Y-%m-%d");
                format!("updated {day}")
            }));
            facts.extend(about.homepage.clone());
            if !facts.is_empty() {
                writeln!(w, "  {}", facts.join(" · "))?;
            }
        }
        if let Some(words) = d.about_unavailable.and_then(|u| u.reason.words()) {
            writeln!(w, "  about: {words}")?;
        }
        if let Some(c) = &d.catalog {
            let folders: Vec<&str> = c.folders.iter().map(FolderName::as_str).collect();
            let families = |list: &[Family]| -> String {
                list.iter()
                    .map(|f| f.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let variant = c
                .variant
                .as_ref()
                .map(|v| format!(" ({v})"))
                .unwrap_or_default();
            writeln!(
                w,
                "  catalog{variant}: folders {} · families {}",
                folders.join(", "),
                families(&c.families)
            )?;
            for other in &c.variants {
                writeln!(
                    w,
                    "  other build ({}): families {}",
                    other.variant,
                    families(&other.families)
                )?;
            }
        }
        for i in &d.installed {
            let confidence = serde_json::to_value(i.version_confidence)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let channel = serde_json::to_value(i.channel)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let drift = match i.drift.state {
                DriftState::Modified => {
                    let files = if i.drift.files == 1 { "file" } else { "files" };
                    format!(" · {} {files} changed on disk", i.drift.files)
                }
                DriftState::Missing => " · a folder is missing".to_owned(),
                DriftState::Unindexed => " · not indexed".to_owned(),
                DriftState::Clean => String::new(),
            };
            let dependency = if i.orphan {
                " · a dependency nothing requires".to_owned()
            } else if i.reason == AddonReason::Dependency {
                format!(" · dependency of {}", i.required_by.join(", "))
            } else {
                String::new()
            };
            let any_flavor = if i.any_flavor {
                " · any flavor (--any-flavor)"
            } else {
                ""
            };
            writeln!(
                w,
                "  {}: {} ({confidence}, {channel}) installed {}{drift}{dependency}{any_flavor}",
                i.target.id,
                i.version,
                super::history::local_time(i.installed_at)
            )?;
            if !i.left_out.is_empty() {
                let left: Vec<String> = i.left_out.iter().map(LeftOutFolder::describe).collect();
                writeln!(w, "    left out: {}", left.join(", "))?;
            }
        }
        match (&d.latest, d.offline) {
            (Some(l), _) => {
                let not: Vec<&str> = l
                    .compat
                    .iter()
                    .filter(|(_, hint)| !hint.likely())
                    .map(|(id, _)| id.as_str())
                    .collect();
                let likely = l.targets.join(", ");
                let line = match (l.targets.is_empty(), not.is_empty()) {
                    (true, _) => format!(
                        "latest: {} · not likely to load on any client here",
                        l.version
                    ),
                    (false, true) => format!("latest: {} · likely to load on {likely}", l.version),
                    (false, false) => format!(
                        "latest: {} · likely to load on {likely} · not on {}",
                        l.version,
                        not.join(", ")
                    ),
                };
                writeln!(w, "  {line}")?;
                if style.detail() {
                    for (id, hint) in &l.compat {
                        let hint = serde_json::to_value(hint).unwrap_or_default();
                        writeln!(w, "    {id}: {}", hint.as_str().unwrap_or_default())?;
                    }
                }
            }
            (None, true) => writeln!(w, "  latest: not checked (offline)")?,
            (None, false) => writeln!(w, "  latest: unavailable")?,
        }
        match (&d.releases, d.releases_unavailable) {
            (Some(Some(list)), _) if list.is_empty() => writeln!(w, "  releases: none")?,
            (Some(Some(list)), _) => {
                writeln!(w, "  releases:")?;
                let rows: Vec<Vec<String>> = list.iter().map(release_row).collect();
                for line in crate::output::table(&rows).lines() {
                    writeln!(w, "    {line}")?;
                }
            }
            (Some(None), unavailable) => writeln!(
                w,
                "  releases: {}",
                unavailable.map_or("unavailable", |u| u.reason.words())
            )?,
            (None, _) => {}
        }
        if let Some(changelog) = &d.changelog {
            writeln!(w, "  changelog:")?;
            for line in human_lines(changelog.as_ref(), d.changelog_unavailable) {
                writeln!(w, "    {line}")?;
            }
        }
        Ok(())
    }
}

/// Runs `info`.
pub fn run(ctx: &Context, args: &InfoArgs) -> Result<InfoOutcome> {
    let limit = check_limit(args)?;
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.detail();
    let mut warnings = world.warnings.clone();
    let (catalog, catalog_warnings) = world.catalog(ctx);
    warnings.extend(catalog_warnings);
    let records = world.records()?;
    let cwd = std::env::current_dir().unwrap_or_default();
    let parsed = parse_ref(
        &world.config.git_hosts,
        &records,
        &catalog,
        &cwd,
        &args.query,
    )
    .map_err(|e| {
        let mut e = aimed_at(e, &ctx.argv, &args.query);
        // A name nothing knows, said as `info` reads it (an installed name counts too),
        // with the parser's fix (`search`); a path that does not exist says so itself.
        let unknown_name = e
            .fix
            .argv()
            .is_some_and(|a| a.get(1).is_some_and(|c| c == "search"));
        if e.code == ErrorCode::NotFound && unknown_name {
            e.message = format!(
                "{} is not installed, not in the catalog, and not a ref",
                args.query
            );
        }
        e
    })?;
    let targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    // A catalog name with variants means the selected client's (one `-t`, else the
    // default target), else its default variant.
    let family = match targets.as_slice() {
        [one] => Some(one.target.family),
        _ => world
            .default_target(&ctx.argv)
            .and_then(Result::ok)
            .map(|d| d.target.family),
    };
    // The one client asked about without a build is `not_found`, as `install` says; the
    // default target's family without one shows the default build.
    let one_client = matches!(targets.as_slice(), [_]);
    let parsed = match family {
        Some(family) => match for_family(&parsed, &catalog, family, &args.query) {
            Ok(own) => own,
            Err(_) if !one_client => parsed,
            Err(error) => return Err(error),
        },
        None => parsed,
    };
    let r = parsed.r#ref.clone();
    // Every installed build of a name with variants is this addon, on every target.
    let refs: Vec<Ref> = match &parsed.catalog_variants {
        Some(name) => catalog
            .variants(name)
            .iter()
            .map(|e| e.r#ref.clone())
            .collect(),
        None => vec![r.clone()],
    };

    let mut installed = Vec::new();
    // Each installed release, for the changelog's oldest.
    let mut releases: Vec<Installed<'_>> = Vec::new();
    for d in &targets {
        let Some(record) = records.get(&d.target.id) else {
            continue;
        };
        let Some(key) = d.target.key else { continue };
        let mut report = None;
        for a in record.addons.values().filter(|a| refs.contains(&a.r#ref)) {
            // The changelog is the shown build's: another build's release is no `since`.
            if let Some(release_id) = a.release_id.as_ref().filter(|_| a.r#ref == r) {
                releases.push(Installed {
                    release_id,
                    version: &a.version,
                    installed_at: a.installed_at,
                });
            }
            let report = report.get_or_insert_with(|| {
                super::drift::report(&world.paths, &d.target, record, DriftMode::Stat)
            });
            let drift = report
                .addons
                .iter()
                .find(|r| r.addon.eq_ignore_ascii_case(&a.name))
                .map_or(
                    DriftSummary {
                        state: DriftState::Clean,
                        files: 0,
                        folders: None,
                    },
                    |r| DriftSummary::of(r, verbose),
                );
            installed.push(InstalledInfo {
                target: TargetRef {
                    id: d.target.id.clone(),
                    key,
                },
                version: a.version.clone(),
                version_confidence: a.version_confidence,
                channel: a.channel,
                pinned: a.pinned,
                state: a.state,
                reason: a.reason,
                required_by: a.required_by.clone(),
                orphan: a.is_orphan(),
                any_flavor: a.any_flavor,
                folders: a.folders.clone(),
                left_out: a.left_out.clone(),
                drift,
                toc: installed_toc(&d.target, &a.name, &a.folders),
                installed_at: a.installed_at,
                tx: a.tx_id,
                record_path: verbose.then(|| world.paths.target_record(&key)),
            });
        }
    }
    let entry = catalog
        .entries()
        .iter()
        .find(|e| e.r#ref == r)
        .or_else(|| catalog.resolve(&args.query, family));
    let catalog_info = entry.map(|e| CatalogInfo {
        name: e.name.clone(),
        variant: e.variant.clone(),
        aliases: e.aliases.clone(),
        families: e.families.clone(),
        folders: e.folders.clone(),
        notes: e.notes.clone(),
        variants: catalog
            .variants(&e.name)
            .into_iter()
            .filter(|other| other.identity() != e.identity())
            .filter_map(|other| {
                Some(CatalogVariant {
                    variant: other.variant.clone()?,
                    families: other.families.clone(),
                })
            })
            .collect(),
    });
    let name = installed
        .first()
        .and_then(|i| {
            records
                .get(&i.target.id)?
                .addons
                .values()
                .find(|a| refs.contains(&a.r#ref))
                .map(|a| a.name.clone())
        })
        .or_else(|| catalog_info.as_ref().map(|c| c.name.clone()));

    let known_locally = !installed.is_empty() || catalog_info.is_some();
    let net = NetConfig {
        offline: ctx.offline,
        git_hosts: world.config.git_hosts.clone(),
        ..NetConfig::default()
    };
    let log = NetLog::new(&world.paths)?;
    let http = stack(&net, log.clone(), &world.paths);
    let sources = world.sources().with_offline(ctx.offline);
    let registry =
        SourceRegistry::builtin(&sources).with_git(GitRunner::new(&net, log, &world.paths));
    // Read-only: an answer from the last 15 minutes is good enough.
    let freshness = if ctx.offline {
        Freshness::OfflineOnly
    } else if args.refresh {
        Freshness::Refresh
    } else {
        Freshness::Window(SOURCE_RECHECK_MIN_INTERVAL)
    };
    let cache = ReleaseCache::new(
        &world.paths,
        http.as_ref(),
        &registry,
        freshness,
        ctx.command,
        ctx.actor,
    );
    let wanted = Wanted::stable(r.clone());
    let fetched = cache
        .get(std::slice::from_ref(&wanted))
        .remove(&wanted)
        .map_or_else(
            || Err(Error::internal("the fetch returned nothing for the ref")),
            |f| f.set,
        );
    // What plans learned, for judging each client (taken before the cache is spent).
    let facts = cache.facts().clone();
    // Before any error return: a remembered failure is worth keeping too.
    let remembered = cache.remember(&[]);
    warnings.extend(remembered.warnings);
    let families = catalog_info
        .as_ref()
        .map_or(&[][..], |c| c.families.as_slice());
    let (latest, partial, set) = match fetched {
        Ok(set) => {
            let found = latest(&set, &targets, verbose, &facts, families);
            (Some(found), false, Some(set))
        }
        // Offline with nothing remembered: the local part only, as in M1.
        Err(_) if ctx.offline => (None, false, None),
        Err(error) if known_locally => {
            warnings.push(Warning::item(&error));
            (None, true, None)
        }
        Err(error) => return Err(error),
    };
    let latest = latest.flatten();
    let mut notes = remembered.notes;
    // `--releases`: one listing request, judged client by client as `latest` is.
    let (listed, listed_unavailable) = if args.releases {
        let judged = Judged {
            r: &r,
            targets: &targets,
            facts: &facts,
            families,
            latest: latest.as_ref(),
        };
        match listed_releases(&registry, &r, http.as_ref()) {
            Ok(list) => (
                Some(Some(
                    list.into_iter()
                        .take(limit)
                        .map(|release| judged.listed(release))
                        .collect(),
                )),
                None,
            ),
            Err((reason, error)) => {
                notes.extend(error.map(|e| format!("releases: {}", e.message)));
                (Some(None), Some(ReleasesUnavailable { reason }))
            }
        }
    } else {
        (None, None)
    };
    // `--changelog`: the releases newer than the oldest installed one.
    let (changelog, changelog_unavailable) = if args.changelog {
        let newest_stable = set
            .as_ref()
            .and_then(|s| newest(s, Channel::Stable).map(|r| (s, r)));
        match newest_stable {
            Some((set, release)) => {
                let since = oldest_since(&releases, set);
                let found = changelog_for(&registry, set, release, since.as_deref(), http.as_ref());
                notes.extend(found.diagnostic);
                (Some(found.changelog), found.unavailable)
            }
            None if ctx.offline => (Some(None), Some(ChangelogUnavailable::Offline)),
            None if partial => (Some(None), Some(ChangelogUnavailable::FetchFailed)),
            None => (Some(None), Some(ChangelogUnavailable::NoReleases)),
        }
    } else {
        (None, None)
    };
    let asked = AboutAsk {
        offline: ctx.offline,
        refresh: args.refresh,
    };
    let (about, about_unavailable) = about_of(
        &r,
        &registry,
        http.as_ref(),
        &world.paths,
        asked,
        &mut notes,
    );
    Ok(InfoOutcome {
        data: InfoData {
            query: args.query.clone(),
            r#ref: r,
            name,
            catalog: catalog_info,
            installed,
            latest,
            about,
            about_unavailable,
            offline: ctx.offline,
            changelog,
            changelog_unavailable,
            releases: listed,
            releases_unavailable: listed_unavailable,
        },
        warnings,
        partial,
        notes,
    })
}

/// One listed release in the human view: its version, day, channel and where it is
/// likely to load (`v2  2026-10-28  stable  likely on retail · not on classic`).
fn release_row(l: &ListedRelease) -> Vec<String> {
    let day = l.published_at.map_or_else(
        || "-".to_owned(),
        |t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d")
                .to_string()
        },
    );
    let channel = serde_json::to_value(l.channel)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let (likely, not): (Vec<_>, Vec<_>) = l.compat.iter().partition(|(_, hint)| hint.likely());
    let ids = |list: Vec<(&String, &LoadHint)>| -> String {
        list.into_iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let fits = match (likely.is_empty(), not.is_empty()) {
        (true, true) => String::new(),
        (true, false) => "not likely to load on any client here".to_owned(),
        (false, true) => format!("likely on {}", ids(likely)),
        (false, false) => format!("likely on {} · not on {}", ids(likely), ids(not)),
    };
    vec![l.version.clone(), day, channel, fits]
}

/// `--limit` within 1..=`INFO_RELEASES_MAX` (`usage` otherwise, before anything is read,
/// the fix using the bound), or the default.
fn check_limit(args: &InfoArgs) -> Result<usize> {
    let limit = args.limit.unwrap_or(DEFAULT_RELEASES);
    if (1..=INFO_RELEASES_MAX).contains(&limit) {
        return Ok(limit);
    }
    let bound = if limit == 0 { 1 } else { INFO_RELEASES_MAX };
    Err(Error::new(
        ErrorCode::Usage,
        format!("--limit takes 1 to {INFO_RELEASES_MAX}, not {limit}"),
        Suggestion::command([
            "omaddons".to_owned(),
            "info".to_owned(),
            args.query.clone(),
            "--releases".to_owned(),
            "--limit".to_owned(),
            bound.to_string(),
        ]),
    ))
}

/// `r`'s recent releases from its source, newest first: the reason they are not listed
/// otherwise, with the error behind it when there was one.
fn listed_releases(
    registry: &SourceRegistry,
    r: &Ref,
    http: &dyn Http,
) -> std::result::Result<Vec<Release>, (ReleasesUnavailableReason, Option<Error>)> {
    let Some(source) = registry.get(SourceId::of(r)) else {
        return Err((ReleasesUnavailableReason::SourceDisabled, None));
    };
    match source.releases(r, http) {
        None => Err((ReleasesUnavailableReason::SourceListsLatestOnly, None)),
        Some(Ok(list)) => Ok(list),
        Some(Err(error)) => Err((ReleasesUnavailableReason::of(&error), Some(error))),
    }
}

/// What judges each listed release on each selected client.
struct Judged<'a> {
    r: &'a Ref,
    targets: &'a [&'a DiscoveredTarget],
    facts: &'a FactIndex,
    families: &'a [Family],
    /// The newest stable release, judged from its own metadata.
    latest: Option<&'a Latest>,
}

impl Judged<'_> {
    /// `release` as `--releases` lists it. The newest stable release keeps `latest`'s
    /// judgment, which read its metadata; any other is judged alone, as a stable release,
    /// since how likely it is to load does not depend on its channel.
    fn listed(&self, release: Release) -> ListedRelease {
        let newest = self.latest.filter(|l| l.release_id == release.release_id);
        let alone = ReleaseSet {
            r#ref: self.r.clone(),
            releases: vec![Release {
                channel: Channel::Stable,
                ..release.clone()
            }],
        };
        let compat = self
            .targets
            .iter()
            .map(|d| {
                let hint = newest
                    .and_then(|l| l.compat.get(&d.target.id).copied())
                    .unwrap_or_else(|| judge(&alone, &d.target, self.facts, self.families));
                (d.target.id.clone(), hint)
            })
            .collect();
        ListedRelease {
            assets: release.assets.iter().map(|a| a.filename.clone()).collect(),
            version: release.version,
            published_at: release.published_at,
            channel: release.channel,
            release_id: release.release_id,
            compat,
        }
    }
}

/// How `info` may ask a source about a project.
#[derive(Debug, Clone, Copy)]
struct AboutAsk {
    /// `--offline`: the memo's answer, whatever its age, or nothing.
    offline: bool,
    /// `--refresh`: ask even inside `ABOUT_RECHECK_INTERVAL`.
    refresh: bool,
}

/// `about` and `about_unavailable` for `r`. The page comes from the ref; the rest from
/// the about memo inside `ABOUT_RECHECK_INTERVAL` (any age offline), else from one
/// request to the project's source, which the memo then keeps (a project the source
/// does not know too). A failed request falls back to the memo's older answer when it
/// has one. Never a warning and never partial: what is missing says why, and `-v` notes
/// the failure.
fn about_of(
    r: &Ref,
    registry: &SourceRegistry,
    http: &dyn crate::net::Http,
    paths: &crate::state::paths::Paths,
    ask: AboutAsk,
    notes: &mut Vec<String>,
) -> (Option<About>, Option<AboutUnavailable>) {
    let unavailable = |reason| Some(AboutUnavailable { reason });
    let Some(project) = Project::of(r) else {
        return (
            None,
            unavailable(AboutUnavailableReason::SourceHasNoMetadata),
        );
    };
    let page_only = About {
        summary: None,
        author: None,
        homepage: project.homepage(),
        updated_at: None,
        popularity: None,
    };
    if !project.described() {
        return (
            Some(page_only),
            unavailable(AboutUnavailableReason::NotSupported),
        );
    }
    if !registry.enabled().contains(&project.source()) {
        return (
            Some(page_only),
            unavailable(AboutUnavailableReason::SourceDisabled),
        );
    }
    let (memo, note) = about_memo::load(paths);
    notes.extend(note);
    let key = project.key();
    let remembered = memo.entries.get(&key);
    let now = jiff::Timestamp::now();
    let fresh = remembered
        .filter(|e| ask.offline || (!ask.refresh && e.fresh(now, ABOUT_RECHECK_INTERVAL)));
    let entry = match (fresh, ask.offline) {
        (Some(entry), _) => entry.clone(),
        (None, true) => {
            return (
                Some(page_only),
                unavailable(AboutUnavailableReason::Offline),
            );
        }
        (None, false) => match about::fetch(project, registry, http) {
            Ok(found) => {
                let entry = AboutEntry {
                    fetched_at: now,
                    found,
                };
                notes.extend(about_memo::record(
                    paths,
                    &key,
                    entry.clone(),
                    ABOUT_MEMO_MAX_ENTRIES,
                ));
                entry
            }
            Err(error) => {
                notes.push(format!("about {key}: {}", error.message));
                match remembered {
                    Some(older) => older.clone(),
                    None => {
                        return (
                            Some(page_only),
                            unavailable(AboutUnavailableReason::of(&error)),
                        );
                    }
                }
            }
        },
    };
    match entry.found {
        Some(facts) => (
            Some(About {
                summary: facts.summary,
                author: facts.author,
                updated_at: facts.updated_at,
                popularity: facts.popularity,
                ..page_only
            }),
            None,
        ),
        None => (
            Some(page_only),
            unavailable(AboutUnavailableReason::NotFound),
        ),
    }
}

/// The newest stable release and the targets with a candidate for it; `None` when the
/// source lists no stable release.
fn latest(
    set: &ReleaseSet,
    targets: &[&DiscoveredTarget],
    verbose: bool,
    facts: &FactIndex,
    families: &[Family],
) -> Option<Latest> {
    let release = newest(set, Channel::Stable)?;
    let mut flavors: Vec<String> = release
        .assets
        .iter()
        .flat_map(|a| a.flavors.iter().map(|f| f.flavor.clone()))
        .collect();
    flavors.sort();
    flavors.dedup();
    let compat: std::collections::BTreeMap<String, LoadHint> = targets
        .iter()
        .map(|d| (d.target.id.clone(), judge(set, &d.target, facts, families)))
        .collect();
    let likely: Vec<String> = compat
        .iter()
        .filter(|(_, hint)| hint.likely())
        .map(|(id, _)| id.clone())
        .collect();
    let urls = verbose.then(|| {
        release
            .assets
            .iter()
            .filter_map(|a| match &a.location {
                crate::model::AssetLocation::Url(url) => Some(url.clone()),
                // A token URL exists only at download time: show where it comes from.
                crate::model::AssetLocation::Deferred {
                    source: crate::sources::SourceId::Tukui,
                    key,
                } => Some(crate::sources::tukui::canonical_url(key)),
                _ => None,
            })
            .collect()
    });
    Some(Latest {
        version: release.version.clone(),
        release_id: release.release_id.clone(),
        channel: release.channel,
        flavors,
        targets: likely,
        compat,
        asset_urls: urls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TOC field is the author's text: one safe line, its UI escapes, controls and
    /// hidden characters gone, cut at the cap, and nothing when nothing is left.
    #[test]
    fn a_toc_field_is_one_safe_line() {
        let long = "x".repeat(TOC_TEXT_MAX_CHARS + 50);
        let cut = format!("{}…", "x".repeat(TOC_TEXT_MAX_CHARS));
        let cases: &[(&str, Option<&str>)] = &[
            (
                "|cff33ff99Details!|r Damage Meter",
                Some("Details! Damage Meter"),
            ),
            ("Says\u{202e} hi\u{7}\nthere", Some("Says hi there")),
            ("|TInterface\\Icons\\x:16|t", None),
            ("   ", None),
            ("", None),
            ("a || b", Some("a | b")),
            (&long, Some(&cut)),
        ];
        for (raw, want) in cases {
            assert_eq!(toc_text(Some(raw)).as_deref(), *want, "{raw:?}");
        }
        assert_eq!(toc_text(None), None);
    }

    /// `--limit` is 1 to `INFO_RELEASES_MAX`, 10 when absent; out of range it is
    /// `usage`, its fix the same command at the bound.
    #[test]
    fn the_release_limit_is_bounded() {
        let args = |limit: Option<usize>| InfoArgs {
            query: "dbm".into(),
            releases: true,
            limit,
            ..InfoArgs::default()
        };
        for (limit, want) in [
            (None, Ok(DEFAULT_RELEASES)),
            (Some(1), Ok(1)),
            (Some(INFO_RELEASES_MAX), Ok(INFO_RELEASES_MAX)),
            (Some(0), Err("1")),
            (Some(INFO_RELEASES_MAX + 1), Err("20")),
        ] {
            let got = check_limit(&args(limit)).map_err(|e| {
                assert_eq!(e.code, ErrorCode::Usage, "{limit:?}");
                let argv = e.fix.argv().unwrap_or_default().to_vec();
                assert_eq!(
                    argv[..5],
                    ["omaddons", "info", "dbm", "--releases", "--limit"]
                );
                argv[5].clone()
            });
            assert_eq!(got, want.map_err(str::to_owned), "{limit:?}");
        }
    }
}
